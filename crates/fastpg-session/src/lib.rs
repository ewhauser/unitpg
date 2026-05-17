#![forbid(unsafe_code)]

pub use fastpg_exec::{QueryDescription, QueryExecution, QueryResult};
pub use fastpg_types::{Column, PgType, Value};

use fastpg_exec::QueryExecutor;

#[derive(Clone, Debug)]
pub struct SessionState {
    executor: QueryExecutor,
}

impl SessionState {
    pub fn new(server_version: impl Into<String>) -> Self {
        Self {
            executor: QueryExecutor::new(server_version),
        }
    }

    pub fn describe(&self, sql: &str) -> Option<QueryDescription> {
        self.executor.describe(sql)
    }

    pub fn execute(&self, sql: &str, parameters: &[Value]) -> QueryExecution {
        self.executor.execute(sql, parameters)
    }
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new(fastpg_compat::DEFAULT_SERVER_VERSION)
    }
}
