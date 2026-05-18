#![forbid(unsafe_code)]

pub use fastpg_exec::{CopyTarget, QueryDescription, QueryExecution, QueryResult};
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
