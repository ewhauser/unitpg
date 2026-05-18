#![forbid(unsafe_code)]

pub use fastpg_exec::{CopyTarget, QueryDescription, QueryExecution, QueryResult};
pub use fastpg_types::{Column, PgType, Value};

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fastpg_exec::{QueryExecutor, QueryExecutorShared};

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

#[derive(Clone, Debug)]
pub struct SessionState {
    id: u64,
    server: Arc<ServerState>,
    executor: QueryExecutor,
}

impl SessionState {
    pub fn new(server_version: impl Into<String>) -> Self {
        Self::for_server(Arc::new(ServerState::new(server_version)))
    }

    pub fn for_server(server: Arc<ServerState>) -> Self {
        let id = server.allocate_session_id();
        let executor = QueryExecutor::with_shared(server.executor.clone());
        Self {
            id,
            server,
            executor,
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn server(&self) -> &Arc<ServerState> {
        &self.server
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

    pub fn finish_copy(&self) {
        self.executor.finish_copy();
    }

    pub fn abort_copy(&self) {
        self.executor.abort_copy();
    }
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new(fastpg_compat::DEFAULT_SERVER_VERSION)
    }
}
