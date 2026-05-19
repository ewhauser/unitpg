#![forbid(unsafe_code)]

use std::borrow::Cow;
use std::sync::Arc;

#[cfg(feature = "postgres-execution")]
use fastpg_pgcore::{
    ExecutionResult as PgCoreExecutionResult, INT2_OID, INT4_OID, INT8_OID, PgCoreParam,
    PgCoreSession, PgCoreValue, PreparedStatement, TEXT_OID, VARCHAR_OID,
};
use fastpg_types::{Column, PgType, Value};

#[cfg(feature = "postgres-execution")]
const BPCHAR_OID: u32 = 1042;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryDescription {
    pub parameter_types: Vec<PgType>,
    pub fields: Vec<Column>,
}

impl QueryDescription {
    pub fn new(parameter_types: Vec<PgType>, fields: Vec<Column>) -> Self {
        Self {
            parameter_types,
            fields,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryResult {
    pub fields: Vec<Column>,
    pub rows: Vec<Vec<Value>>,
}

impl QueryResult {
    pub fn new(fields: Vec<Column>, rows: Vec<Vec<Value>>) -> Self {
        Self { fields, rows }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CopyTarget {
    pub table: String,
    pub columns: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueryExecution {
    Empty,
    Rows(QueryResult),
    Command {
        tag: Cow<'static, str>,
        rows: usize,
    },
    CopyIn(CopyTarget),
    Unsupported {
        query: String,
    },
    InvalidParameters {
        message: String,
    },
    Error {
        sqlstate: String,
        message: String,
        detail: Option<String>,
        hint: Option<String>,
        cursorpos: i32,
    },
}

#[derive(Clone, Debug)]
pub struct QueryExecutorShared;

impl QueryExecutorShared {
    pub fn new(server_version: impl Into<String>) -> Self {
        let _ = server_version.into();
        Self
    }
}

#[derive(Debug)]
pub struct QueryExecutor {
    #[allow(dead_code)]
    shared: Arc<QueryExecutorShared>,
    #[cfg(feature = "postgres-execution")]
    pgcore_session: PgCoreSession,
    #[cfg(feature = "postgres-execution")]
    storage_session: fastpg_storage::SessionStorageHandle,
}

impl QueryExecutor {
    pub fn new(server_version: impl Into<String>) -> Self {
        Self::with_shared(Arc::new(QueryExecutorShared::new(server_version)))
    }

    pub fn with_shared(shared: Arc<QueryExecutorShared>) -> Self {
        #[cfg(feature = "postgres-execution")]
        {
            let storage_session = fastpg_storage::new_session_storage();
            Self {
                shared,
                pgcore_session: PgCoreSession::with_storage_session(storage_session.clone()),
                storage_session,
            }
        }
        #[cfg(not(feature = "postgres-execution"))]
        {
            Self { shared }
        }
    }

    pub fn describe(&self, sql: &str) -> Option<QueryDescription> {
        #[cfg(feature = "postgres-execution")]
        {
            self.describe_pgcore(sql)
        }
        #[cfg(not(feature = "postgres-execution"))]
        {
            let _ = sql;
            None
        }
    }

    pub fn execute(&self, sql: &str, parameters: &[Value]) -> QueryExecution {
        #[cfg(feature = "postgres-execution")]
        {
            self.execute_pgcore(sql, parameters)
        }
        #[cfg(not(feature = "postgres-execution"))]
        {
            let _ = (sql, parameters);
            execution_error(
                "0A000",
                "fastpg-exec was built without PostgreSQL execution",
            )
        }
    }

    pub fn copy_text_line(&self, table: &str, line: &str) -> Result<bool, String> {
        #[cfg(feature = "postgres-execution")]
        {
            let _guard = fastpg_storage::enter_session_storage(self.storage_session.clone());
            fastpg_storage::copy_text_line(table, line)
        }
        #[cfg(not(feature = "postgres-execution"))]
        {
            let _ = (table, line);
            Err("fastpg-exec was built without PostgreSQL execution".to_owned())
        }
    }

    pub fn finish_copy(&self) {
        #[cfg(feature = "postgres-execution")]
        {
            let _guard = fastpg_storage::enter_session_storage(self.storage_session.clone());
            fastpg_storage::commit_implicit_transaction();
        }
    }

    pub fn abort_copy(&self) {
        #[cfg(feature = "postgres-execution")]
        {
            let _guard = fastpg_storage::enter_session_storage(self.storage_session.clone());
            fastpg_storage::abort_implicit_transaction();
        }
    }

    pub fn close(&self) {
        #[cfg(feature = "postgres-execution")]
        {
            let _guard = fastpg_storage::enter_session_storage(self.storage_session.clone());
            if fastpg_storage::is_explicit_transaction() {
                fastpg_storage::abort_explicit_transaction();
            } else {
                fastpg_storage::abort_implicit_transaction();
            }
        }
    }

    #[cfg(feature = "postgres-execution")]
    fn describe_pgcore(&self, sql: &str) -> Option<QueryDescription> {
        self.prepare_pgcore(sql)
            .ok()
            .map(|statement| query_description_from_pgcore(&statement))
    }

    #[cfg(feature = "postgres-execution")]
    fn execute_pgcore(&self, sql: &str, parameters: &[Value]) -> QueryExecution {
        let parameters = parameters
            .iter()
            .map(pgcore_param_value)
            .collect::<Vec<_>>();

        let execution_result = self.pgcore_session.execute_with_params(sql, &parameters);

        match execution_result {
            Ok(result) => pgcore_execution_to_query_execution(result),
            Err(error) => pgcore_error_execution(error),
        }
    }

    #[cfg(feature = "postgres-execution")]
    fn prepare_pgcore(&self, sql: &str) -> Result<PreparedStatement, fastpg_pgcore::PgCoreError> {
        self.pgcore_session.prepare(sql)
    }
}

fn execution_error(sqlstate: impl Into<String>, message: impl Into<String>) -> QueryExecution {
    QueryExecution::Error {
        sqlstate: sqlstate.into(),
        message: message.into(),
        detail: None,
        hint: None,
        cursorpos: 0,
    }
}

#[cfg(feature = "postgres-execution")]
fn pgcore_error_execution(error: fastpg_pgcore::PgCoreError) -> QueryExecution {
    QueryExecution::Error {
        sqlstate: error.sqlstate,
        message: error.message,
        detail: error.detail,
        hint: error.hint,
        cursorpos: error.cursorpos,
    }
}

#[cfg(feature = "postgres-execution")]
fn query_description_from_pgcore(statement: &PreparedStatement) -> QueryDescription {
    let description = statement.describe();
    QueryDescription::new(
        description
            .parameter_type_oids
            .iter()
            .copied()
            .map(pg_type_for_oid)
            .collect(),
        description
            .fields
            .into_iter()
            .map(|field| Column::new(field.name, pg_type_for_oid(field.type_oid)))
            .collect(),
    )
}

#[cfg(feature = "postgres-execution")]
fn pgcore_execution_to_query_execution(result: PgCoreExecutionResult) -> QueryExecution {
    let Some(statement) = result.statements.into_iter().next() else {
        return QueryExecution::Empty;
    };

    if let Some(copy_in) = statement.copy_in {
        return QueryExecution::CopyIn(CopyTarget {
            table: copy_in.table,
            columns: copy_in.columns,
        });
    }

    if statement.fields.is_empty() {
        return QueryExecution::Command {
            tag: statement.command_tag,
            rows: statement.rows.len(),
        };
    }

    let fields = statement
        .fields
        .into_iter()
        .map(|field| Column::new(field.name, pg_type_for_oid(field.type_oid)))
        .collect::<Vec<_>>();
    let rows = statement
        .rows
        .into_iter()
        .map(|row| {
            row.into_iter()
                .zip(fields.iter())
                .map(|(value, field)| pgcore_value_to_value(value, field.data_type))
                .collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<Vec<_>, _>>();

    match rows {
        Ok(rows) => QueryExecution::Rows(QueryResult::new(fields, rows)),
        Err(message) => execution_error("22P02", message),
    }
}

#[cfg(feature = "postgres-execution")]
fn pgcore_param_value(value: &Value) -> PgCoreParam {
    match value {
        Value::Int2(value) => PgCoreParam::Datum(*value as usize),
        Value::Int4(value) => PgCoreParam::Datum(*value as usize),
        Value::Int8(value) => PgCoreParam::Datum(*value as usize),
        Value::Text(value) => PgCoreParam::Text(value.clone()),
        Value::Null => PgCoreParam::Null,
    }
}

#[cfg(feature = "postgres-execution")]
fn pg_type_for_oid(type_oid: u32) -> PgType {
    match type_oid {
        INT2_OID => PgType::Int2,
        INT4_OID => PgType::Int4,
        INT8_OID => PgType::Int8,
        TEXT_OID | VARCHAR_OID | BPCHAR_OID => PgType::Varchar,
        _ => PgType::Varchar,
    }
}

#[cfg(feature = "postgres-execution")]
fn pgcore_value_to_value(value: PgCoreValue, data_type: PgType) -> Result<Value, String> {
    match (value, data_type) {
        (PgCoreValue::Null, _) => Ok(Value::Null),
        (PgCoreValue::Text(value), PgType::Int2) => value
            .parse::<i16>()
            .map(Value::Int2)
            .map_err(|error| format!("cannot decode PostgreSQL int2 value {value:?}: {error}")),
        (PgCoreValue::Text(value), PgType::Int4) => value
            .parse::<i32>()
            .map(Value::Int4)
            .map_err(|error| format!("cannot decode PostgreSQL int4 value {value:?}: {error}")),
        (PgCoreValue::Text(value), PgType::Int8) => value
            .parse::<i64>()
            .map(Value::Int8)
            .map_err(|error| format!("cannot decode PostgreSQL int8 value {value:?}: {error}")),
        (PgCoreValue::Text(value), PgType::Varchar) => Ok(Value::Text(value)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(feature = "postgres-execution"))]
    #[test]
    fn default_build_has_no_postgres_execution() {
        let executor = QueryExecutor::new("17.0-fastpg");

        assert_eq!(executor.describe("SELECT 1"), None);
        let QueryExecution::Error {
            sqlstate, message, ..
        } = executor.execute("SELECT 1", &[])
        else {
            panic!("expected disabled executor error");
        };
        assert_eq!(sqlstate, "0A000");
        assert!(message.contains("without PostgreSQL execution"));
        assert!(executor.copy_text_line("smoke", "1").is_err());
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn executes_select_one_through_pgcore() {
        let executor = QueryExecutor::new("17.0-fastpg");

        assert_eq!(
            executor.describe("SELECT 1").unwrap().fields,
            vec![Column::new("?column?", PgType::Int4)]
        );
        assert_eq!(
            executor.execute("SELECT 1", &[]),
            QueryExecution::Rows(QueryResult::new(
                vec![Column::new("?column?", PgType::Int4)],
                vec![vec![Value::Int4(1)]]
            ))
        );
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn executes_comment_only_query_as_empty_query() {
        let executor = QueryExecutor::new("17.0-fastpg");

        assert_eq!(
            executor.execute("/* comment-only query should be empty */", &[]),
            QueryExecution::Empty
        );
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn executes_create_table_through_pgcore() {
        let executor = QueryExecutor::new("17.0-fastpg");
        let table = format!("fastpg_exec_util_{}", std::process::id());

        assert_eq!(
            executor.execute(
                &format!("create table {table}(id int not null, filler char(8), mtime timestamp)"),
                &[]
            ),
            QueryExecution::Command {
                tag: "CREATE TABLE".into(),
                rows: 0,
            }
        );
        assert_eq!(
            executor.execute(&format!("drop table if exists {table}"), &[]),
            QueryExecution::Command {
                tag: "DROP TABLE".into(),
                rows: 0,
            }
        );
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn executes_parameterized_int4_through_pgcore() {
        let executor = QueryExecutor::new("17.0-fastpg");

        assert_eq!(
            executor.execute("SELECT $1::int4", &[Value::Int4(41)]),
            QueryExecution::Rows(QueryResult::new(
                vec![Column::new("int4", PgType::Int4)],
                vec![vec![Value::Int4(41)]]
            ))
        );
        assert_eq!(
            executor.execute("SELECT $1::int4", &[Value::Null]),
            QueryExecution::Rows(QueryResult::new(
                vec![Column::new("int4", PgType::Int4)],
                vec![vec![Value::Null]]
            ))
        );
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn answers_pgbench_relkind_query_through_pgcore() {
        let executor = QueryExecutor::new("17.0-fastpg");
        let table = format!("fastpg_exec_relkind_{}", std::process::id());
        executor.execute(&format!("create table {table}(id int not null)"), &[]);

        let sql = "SELECT relkind FROM pg_catalog.pg_class WHERE oid=$1::pg_catalog.regclass";
        assert_eq!(
            executor.describe(sql).unwrap(),
            QueryDescription::new(
                vec![PgType::Varchar],
                vec![Column::new("relkind", PgType::Varchar)]
            )
        );
        assert_eq!(
            executor.execute(sql, &[Value::Text(table.clone())]),
            QueryExecution::Rows(QueryResult::new(
                vec![Column::new("relkind", PgType::Varchar)],
                vec![vec![Value::Text("r".to_owned())]]
            ))
        );

        executor.execute(&format!("drop table if exists {table}"), &[]);
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn copy_from_stdin_uses_pgcore_and_rust_storage() {
        let executor = QueryExecutor::new("17.0-fastpg");
        let table = format!("fastpg_exec_copy_{}", std::process::id());
        executor.execute(
            &format!("create table {table}(id int not null, filler char(8))"),
            &[],
        );

        assert_eq!(
            executor.execute(&format!("copy {table} from stdin with (freeze on)"), &[]),
            QueryExecution::CopyIn(CopyTarget {
                table: table.clone(),
                columns: 2,
            })
        );
        assert!(executor.copy_text_line(&table, "1\t").unwrap());
        assert!(!executor.copy_text_line(&table, "\\.").unwrap());

        executor.execute(&format!("drop table if exists {table}"), &[]);
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn show_server_version_goes_through_pgcore() {
        let executor = QueryExecutor::new("17.0-fastpg");

        let result = executor.execute("SHOW server_version", &[]);
        let QueryExecution::Rows(result) = result else {
            panic!("expected pgcore rows, got {result:?}");
        };
        assert_eq!(
            result.fields,
            vec![Column::new("server_version", PgType::Varchar)]
        );
        assert_eq!(result.rows.len(), 1);
        let Value::Text(server_version) = &result.rows[0][0] else {
            panic!("expected text server_version, got {:?}", result.rows[0][0]);
        };
        assert!(!server_version.is_empty());
        assert_ne!(server_version, "17.0-fastpg");
    }
}
