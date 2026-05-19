#![forbid(unsafe_code)]

pub use fastpg_exec::{CopyTarget, QueryDescription, QueryExecution, QueryResult};
pub use fastpg_types::{Column, PgType, Value};

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use fastpg_exec::{QueryExecutor, QueryExecutorShared};

pub type SessionId = u64;

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
    next_session_id: AtomicU64,
}

impl ServerState {
    pub fn new(server_version: impl Into<String>) -> Self {
        Self {
            executor: Arc::new(QueryExecutorShared::new(server_version)),
            next_session_id: AtomicU64::new(1),
        }
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

#[derive(Debug)]
pub struct SessionState {
    id: SessionId,
    server: Arc<ServerState>,
    executor: QueryExecutor,
    startup: StartupParameters,
    copy: Mutex<Option<SessionCopyState>>,
}

impl SessionState {
    pub fn new(server: Arc<ServerState>, startup: StartupParameters) -> Self {
        let id = server.allocate_session_id();
        let executor = QueryExecutor::with_shared(server.executor.clone());
        Self {
            id,
            server,
            executor,
            startup,
            copy: Mutex::new(None),
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

    pub fn copy_text_line(&self, table: &str, line: &str) -> Result<bool, String> {
        self.executor.copy_text_line(table, line)
    }

    pub fn begin_copy(&self, target: CopyTarget) {
        let mut copy = self
            .copy
            .lock()
            .expect("fastpg session COPY mutex poisoned");
        *copy = Some(SessionCopyState::new(target));
    }

    pub fn push_copy_data(&self, data: &[u8]) -> Result<(), String> {
        let mut copy = self
            .copy
            .lock()
            .expect("fastpg session COPY mutex poisoned");
        let copy = copy
            .as_mut()
            .ok_or_else(|| "COPY data received without an active COPY target".to_owned())?;
        copy.push_data(&self.executor, data)
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
                self.executor.finish_copy();
                Ok(rows)
            }
            Err(error) => {
                self.executor.abort_copy();
                Err(error)
            }
        }
    }

    pub fn abort_active_copy(&self) {
        let mut copy = self
            .copy
            .lock()
            .expect("fastpg session COPY mutex poisoned");
        copy.take();
        self.executor.abort_copy();
    }

    pub fn finish_copy(&self) {
        self.executor.finish_copy();
    }

    pub fn abort_copy(&self) {
        self.executor.abort_copy();
    }
}

impl Drop for SessionState {
    fn drop(&mut self) {
        self.abort_active_copy();
        self.executor.close();
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
    rows: usize,
}

impl SessionCopyState {
    fn new(target: CopyTarget) -> Self {
        Self {
            target,
            pending: String::new(),
            rows: 0,
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
        Ok(self.rows)
    }

    fn process_line(&mut self, executor: &QueryExecutor, line: &str) -> Result<(), String> {
        if executor.copy_text_line(&self.target.table, line)? {
            self.rows += 1;
        }
        Ok(())
    }
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
