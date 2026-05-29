#![forbid(unsafe_code)]

pub use fastpg_exec::{
    CopyOutput, CopyTarget, QueryDescription, QueryExecution, QueryNotice, QueryResult,
};
pub use fastpg_types::{Column, ParameterFormat, PgType, QueryParameter, Value};

use std::any::Any;
use std::collections::{BTreeMap, VecDeque};
#[cfg(feature = "postgres-execution")]
use std::ffi::CStr;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;

use fastpg_exec::{COPY_HEADER_MATCH, QueryExecutor, QueryExecutorShared};

pub type SessionId = u64;
pub const COPY_ERROR_CONTEXT_PREFIX: &str = "\x1ffastpg-copy-context\n";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartupParameters {
    parameters: BTreeMap<String, String>,
    user: String,
    database: String,
}

impl StartupParameters {
    pub fn new(parameters: BTreeMap<String, String>) -> Self {
        let user = parameters
            .get("user")
            .cloned()
            .unwrap_or_else(|| "postgres".to_owned());
        let database = parameters
            .get("database")
            .cloned()
            .unwrap_or_else(|| user.clone());

        Self {
            parameters,
            user,
            database,
        }
    }

    pub fn user(&self) -> &str {
        &self.user
    }

    pub fn database(&self) -> &str {
        &self.database
    }

    pub fn parameters(&self) -> &BTreeMap<String, String> {
        &self.parameters
    }
}

impl Default for StartupParameters {
    fn default() -> Self {
        Self::new(BTreeMap::new())
    }
}

impl From<BTreeMap<String, String>> for StartupParameters {
    fn from(parameters: BTreeMap<String, String>) -> Self {
        Self::new(parameters)
    }
}

#[derive(Debug)]
pub struct ServerState {
    executor: Arc<QueryExecutorShared>,
    server_version: String,
    next_session_id: AtomicU64,
}

impl ServerState {
    pub fn new(server_version: impl Into<String>) -> Self {
        let server_version = server_version.into();
        Self {
            executor: Arc::new(QueryExecutorShared::new(server_version.clone())),
            server_version,
            next_session_id: AtomicU64::new(1),
        }
    }

    pub fn server_version(&self) -> &str {
        &self.server_version
    }

    fn allocate_session_id(&self) -> u64 {
        self.next_session_id.fetch_add(1, Ordering::Relaxed)
    }
}

impl Default for ServerState {
    fn default() -> Self {
        Self::new(fastpg_compat::DEFAULT_SERVER_VERSION)
    }
}

type SessionBackendJob = Box<dyn FnOnce() + Send + 'static>;
const POSTGRES_SAFE_THREAD_STACK_SIZE: usize = 8 * 1024 * 1024;

enum SessionBackendMessage {
    Run(SessionBackendJob),
    Shutdown,
}

struct SessionBackendExecutor {
    sender: mpsc::Sender<SessionBackendMessage>,
    handle: Mutex<Option<thread::JoinHandle<()>>>,
}

impl SessionBackendExecutor {
    fn new(session_id: SessionId) -> Self {
        let (sender, receiver) = mpsc::channel::<SessionBackendMessage>();
        let handle = thread::Builder::new()
            .name(format!("fastpg-session-{session_id}"))
            .stack_size(POSTGRES_SAFE_THREAD_STACK_SIZE)
            .spawn(move || {
                while let Ok(message) = receiver.recv() {
                    match message {
                        SessionBackendMessage::Run(job) => job(),
                        SessionBackendMessage::Shutdown => break,
                    }
                }
            })
            .expect("failed to spawn fastpg session backend thread");

        Self {
            sender,
            handle: Mutex::new(Some(handle)),
        }
    }

    fn run<R>(&self, operation: impl FnOnce() -> R + Send + 'static) -> R
    where
        R: Send + 'static,
    {
        let (sender, receiver) = mpsc::sync_channel::<Result<R, Box<dyn Any + Send + 'static>>>(1);
        let job = Box::new(move || {
            let result = catch_unwind(AssertUnwindSafe(operation));
            let _ = sender.send(result);
        });
        self.sender
            .send(SessionBackendMessage::Run(job))
            .expect("fastpg session backend thread exited");
        match receiver
            .recv()
            .expect("fastpg session backend dropped operation result")
        {
            Ok(result) => result,
            Err(payload) => resume_unwind(payload),
        }
    }

    fn enqueue(&self, operation: impl FnOnce() + Send + 'static) {
        self.sender
            .send(SessionBackendMessage::Run(Box::new(operation)))
            .expect("fastpg session backend thread exited");
    }
}

impl fmt::Debug for SessionBackendExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionBackendExecutor")
            .finish_non_exhaustive()
    }
}

impl Drop for SessionBackendExecutor {
    fn drop(&mut self) {
        let _ = self.sender.send(SessionBackendMessage::Shutdown);
        if let Some(handle) = self
            .handle
            .lock()
            .expect("fastpg session backend handle mutex poisoned")
            .take()
        {
            let _ = handle.join();
        }
    }
}

#[derive(Debug)]
pub struct SessionState {
    id: SessionId,
    server: Arc<ServerState>,
    executor: QueryExecutor,
    backend: Option<SessionBackendExecutor>,
    inline_execution: bool,
    startup: StartupParameters,
    copy: Mutex<Option<SessionCopyState>>,
    pending_simple: Mutex<VecDeque<String>>,
}

impl SessionState {
    pub fn new(server: Arc<ServerState>, startup: StartupParameters) -> Self {
        Self::with_inline_execution(server, startup, false)
    }

    pub fn new_inline_execution(server: Arc<ServerState>, startup: StartupParameters) -> Self {
        Self::with_inline_execution(server, startup, true)
    }

    fn with_inline_execution(
        server: Arc<ServerState>,
        startup: StartupParameters,
        inline_execution: bool,
    ) -> Self {
        let id = server.allocate_session_id();
        let executor =
            QueryExecutor::with_shared_for_database(server.executor.clone(), startup.database());
        Self {
            id,
            server,
            executor,
            backend: (!inline_execution).then(|| SessionBackendExecutor::new(id)),
            inline_execution,
            startup,
            copy: Mutex::new(None),
            pending_simple: Mutex::new(VecDeque::new()),
        }
    }

    pub fn with_server_version(server_version: impl Into<String>) -> Self {
        Self::new(
            Arc::new(ServerState::new(server_version)),
            StartupParameters::default(),
        )
    }

    pub fn for_server(server: Arc<ServerState>) -> Self {
        Self::new(server, StartupParameters::default())
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    pub fn server(&self) -> &Arc<ServerState> {
        &self.server
    }

    pub fn current_user(&self) -> &str {
        self.startup.user()
    }

    pub fn current_database(&self) -> &str {
        self.startup.database()
    }

    pub fn startup_parameters(&self) -> &StartupParameters {
        &self.startup
    }

    pub fn describe(&self, sql: &str) -> Option<QueryDescription> {
        self.executor.describe(sql)
    }

    pub fn execute(&self, sql: &str, parameters: &[Value]) -> QueryExecution {
        self.executor.execute(sql, parameters)
    }

    pub fn execute_with_parameters(
        &self,
        sql: &str,
        parameters: &[QueryParameter],
    ) -> QueryExecution {
        self.executor.execute_with_parameters(sql, parameters)
    }

    pub fn execute_simple_text(&self, sql: &str) -> QueryExecution {
        self.executor.execute_simple_text(sql)
    }

    #[cfg(feature = "postgres-execution")]
    pub fn execute_simple_cstr(&self, sql: &CStr) -> QueryExecution {
        self.executor.execute_simple_cstr(sql)
    }

    pub fn run_on_backend<R>(&self, operation: impl FnOnce() -> R + Send + 'static) -> R
    where
        R: Send + 'static,
    {
        if let Some(backend) = &self.backend {
            backend.run(operation)
        } else {
            operation()
        }
    }

    pub fn enqueue_on_backend(&self, operation: impl FnOnce() + Send + 'static) {
        if let Some(backend) = &self.backend {
            backend.enqueue(operation);
        } else {
            operation();
        }
    }

    pub fn take_notices(&self) -> Vec<QueryNotice> {
        self.executor.take_notices()
    }

    pub fn copy_text_line(&self, table: &str, line: &str) -> Result<bool, String> {
        self.executor.copy_text_line(table, line)
    }

    pub fn begin_copy(&self, target: CopyTarget) {
        let owned_transaction = self.executor.begin_copy();
        let mut copy = self
            .copy
            .lock()
            .expect("fastpg session COPY mutex poisoned");
        *copy = Some(SessionCopyState::new(target, owned_transaction));
    }

    pub fn set_pending_simple_statements(&self, statements: Vec<String>) {
        *self
            .pending_simple
            .lock()
            .expect("fastpg session pending simple mutex poisoned") = VecDeque::from(statements);
    }

    pub fn take_pending_simple_statements(&self) -> VecDeque<String> {
        std::mem::take(
            &mut *self
                .pending_simple
                .lock()
                .expect("fastpg session pending simple mutex poisoned"),
        )
    }

    pub fn push_copy_data(&self, data: &[u8]) -> Result<(), String> {
        let result = {
            let mut copy = self
                .copy
                .lock()
                .expect("fastpg session COPY mutex poisoned");
            let copy = copy
                .as_mut()
                .ok_or_else(|| "COPY data received without an active COPY target".to_owned())?;
            copy.push_data(&self.executor, data)
        };
        if result.is_err() {
            self.abort_active_copy();
        }
        result
    }

    pub fn finish_active_copy(&self) -> Result<usize, String> {
        let mut copy = self
            .copy
            .lock()
            .expect("fastpg session COPY mutex poisoned");
        let Some(mut copy) = copy.take() else {
            return Err("COPY done received without an active COPY target".to_owned());
        };

        match copy.finish(&self.executor) {
            Ok(rows) => {
                self.executor.finish_copy(copy.owned_transaction);
                Ok(rows)
            }
            Err(error) => {
                self.executor.abort_copy(copy.owned_transaction);
                Err(error)
            }
        }
    }

    pub fn abort_active_copy(&self) {
        let mut copy = self
            .copy
            .lock()
            .expect("fastpg session COPY mutex poisoned");
        let owned_transaction = copy
            .take()
            .map(|copy| copy.owned_transaction)
            .unwrap_or(false);
        self.executor.abort_copy(owned_transaction);
    }

    pub fn finish_copy(&self) {
        self.executor.finish_copy(false);
    }

    pub fn abort_copy(&self) {
        self.executor.abort_copy(false);
    }

    #[cfg(feature = "postgres-execution")]
    fn take_active_copy_owned_transaction(&self) -> Option<bool> {
        self.copy
            .lock()
            .expect("fastpg session COPY mutex poisoned")
            .take()
            .map(|copy| copy.owned_transaction)
    }
}

impl Drop for SessionState {
    fn drop(&mut self) {
        #[cfg(feature = "postgres-execution")]
        {
            let active_copy_owned_transaction = self.take_active_copy_owned_transaction();
            if let Some(close_work) = self.executor.close_work(active_copy_owned_transaction) {
                if self.inline_execution {
                    close_work.run();
                } else if let Some(backend) = &self.backend {
                    backend.run(move || close_work.run());
                }
            }
        }

        #[cfg(not(feature = "postgres-execution"))]
        {
            self.abort_active_copy();
            self.executor.close();
        }
    }
}

impl Default for SessionState {
    fn default() -> Self {
        Self::with_server_version(fastpg_compat::DEFAULT_SERVER_VERSION)
    }
}

#[derive(Debug)]
struct SessionCopyState {
    target: CopyTarget,
    pending: String,
    lines: Vec<(usize, String)>,
    rows: usize,
    done: bool,
    line_number: usize,
    owned_transaction: bool,
}

impl SessionCopyState {
    fn new(target: CopyTarget, owned_transaction: bool) -> Self {
        Self {
            target,
            pending: String::new(),
            lines: Vec::new(),
            rows: 0,
            done: false,
            line_number: 0,
            owned_transaction,
        }
    }

    fn push_data(&mut self, executor: &QueryExecutor, data: &[u8]) -> Result<(), String> {
        let chunk = std::str::from_utf8(data).map_err(|error| error.to_string())?;
        self.pending.push_str(chunk);

        while let Some(newline) = self.pending.find('\n') {
            let line = self.pending[..newline].trim_end_matches('\r').to_owned();
            self.pending.drain(..=newline);
            self.process_line(executor, &line)?;
        }

        Ok(())
    }

    fn finish(&mut self, executor: &QueryExecutor) -> Result<usize, String> {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.process_line(executor, line.trim_end_matches('\r'))?;
        }
        let buffered_lines = self
            .lines
            .iter()
            .map(|(_, line)| line.as_str())
            .collect::<Vec<_>>();
        if let Some(rows) = executor.copy_target_buffered_lines(&self.target, &buffered_lines)? {
            self.rows = rows;
            return Ok(rows);
        }
        let data_start = self.target.header_lines_to_skip().min(self.lines.len());
        if self.target.header_line == COPY_HEADER_MATCH && data_start > 0 {
            let (line_number, line) = &self.lines[data_start - 1];
            self.target.validate_header_line(line).map_err(|error| {
                copy_error_with_context(&self.target, *line_number, line, error)
            })?;
        }
        if self.target.freeze && self.target.foreign_table {
            return Err("cannot perform COPY FREEZE on a foreign table".to_owned());
        }
        for (line_number, line) in &self.lines[data_start..] {
            if executor
                .copy_target_text_line(&self.target, line)
                .map_err(|error| copy_error_with_context(&self.target, *line_number, line, error))?
            {
                self.rows += 1;
            }
        }
        Ok(self.rows)
    }

    fn process_line(&mut self, _executor: &QueryExecutor, line: &str) -> Result<(), String> {
        self.line_number += 1;
        if self.done {
            return Err("received copy data after EOF marker".to_owned());
        }
        if line == "\\." {
            self.done = true;
            return Ok(());
        }
        self.lines.push((self.line_number, line.to_owned()));
        Ok(())
    }
}

fn copy_error_with_context(
    target: &CopyTarget,
    line_number: usize,
    line: &str,
    message: String,
) -> String {
    format!(
        "{COPY_ERROR_CONTEXT_PREFIX}{message}\nCOPY {}, line {}: \"{}\"",
        target.table, line_number, line
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_from_one_server_get_distinct_ids() {
        let server = Arc::new(ServerState::new("17.0-fastpg"));

        let first = SessionState::new(server.clone(), StartupParameters::default());
        let second = SessionState::new(server.clone(), StartupParameters::default());

        assert_ne!(first.id(), second.id());
        assert!(Arc::ptr_eq(first.server(), second.server()));
    }

    #[test]
    fn startup_parameters_capture_user_and_database() {
        let mut parameters = BTreeMap::new();
        parameters.insert("user".to_owned(), "alice".to_owned());
        let startup = StartupParameters::new(parameters);

        assert_eq!(startup.user(), "alice");
        assert_eq!(startup.database(), "alice");

        let mut parameters = BTreeMap::new();
        parameters.insert("user".to_owned(), "alice".to_owned());
        parameters.insert("database".to_owned(), "appdb".to_owned());
        let startup = StartupParameters::new(parameters);

        assert_eq!(startup.user(), "alice");
        assert_eq!(startup.database(), "appdb");
    }
}
