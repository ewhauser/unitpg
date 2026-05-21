#![forbid(unsafe_code)]

use std::borrow::Cow;
use std::sync::Arc;

#[cfg(feature = "postgres-execution")]
use fastpg_catalog::relation_by_name;
#[cfg(feature = "postgres-execution")]
use fastpg_pgcore::{
    ExecutionResult as PgCoreExecutionResult, INT2_OID, INT4_OID, INT8_OID, PgCoreNotice,
    PgCoreParam, PgCoreSession, PgCoreTransactionCommand, PgCoreValue, PreparedStatement, TEXT_OID,
    VARCHAR_OID,
};
use fastpg_types::{Column, PgType, Value};

#[cfg(feature = "postgres-execution")]
const BPCHAR_OID: u32 = 1042;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryDescription {
    pub parameter_types: Vec<PgType>,
    pub parameter_type_oids: Vec<u32>,
    pub fields: Vec<Column>,
}

impl QueryDescription {
    pub fn new(parameter_types: Vec<PgType>, fields: Vec<Column>) -> Self {
        let parameter_type_oids = parameter_types
            .iter()
            .copied()
            .map(PgType::default_type_oid)
            .collect();
        Self {
            parameter_types,
            parameter_type_oids,
            fields,
        }
    }

    pub fn with_type_oids(
        parameter_types: Vec<PgType>,
        parameter_type_oids: Vec<u32>,
        fields: Vec<Column>,
    ) -> Self {
        Self {
            parameter_types,
            parameter_type_oids,
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
pub struct QueryNotice {
    pub severity: String,
    pub sqlstate: String,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    pub context: Option<String>,
    pub cursorpos: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CopyTarget {
    pub table: String,
    pub columns: usize,
    pub column_names: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueryExecution {
    WithNotices {
        notices: Vec<QueryNotice>,
        execution: Box<QueryExecution>,
    },
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

impl QueryExecution {
    pub fn into_notices_and_execution(self) -> (Vec<QueryNotice>, QueryExecution) {
        match self {
            QueryExecution::WithNotices { notices, execution } => (notices, *execution),
            execution => (Vec::new(), execution),
        }
    }
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
    #[cfg(feature = "postgres-execution")]
    storage2_session: fastpg_storage2::SessionStorageHandle,
}

impl QueryExecutor {
    pub fn new(server_version: impl Into<String>) -> Self {
        Self::with_shared(Arc::new(QueryExecutorShared::new(server_version)))
    }

    pub fn with_shared(shared: Arc<QueryExecutorShared>) -> Self {
        Self::with_shared_for_database(shared, "postgres")
    }

    pub fn with_shared_for_database(
        shared: Arc<QueryExecutorShared>,
        database: impl Into<String>,
    ) -> Self {
        #[cfg(feature = "postgres-execution")]
        {
            let storage_session = fastpg_storage::new_session_storage();
            let storage2_session = fastpg_storage2::new_session_storage();
            Self {
                shared,
                pgcore_session: PgCoreSession::with_storage_sessions_and_database(
                    storage_session.clone(),
                    storage2_session.clone(),
                    database,
                ),
                storage_session,
                storage2_session,
            }
        }
        #[cfg(not(feature = "postgres-execution"))]
        {
            let _ = database.into();
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
        self.copy_target_text_line(
            &CopyTarget {
                table: table.to_owned(),
                columns: 0,
                column_names: Vec::new(),
            },
            line,
        )
    }

    pub fn copy_target_text_line(&self, target: &CopyTarget, line: &str) -> Result<bool, String> {
        #[cfg(feature = "postgres-execution")]
        {
            let _guard = fastpg_storage::enter_session_storage(self.storage_session.clone());
            let _storage2_guard =
                fastpg_storage2::enter_session_storage(self.storage2_session.clone());
            if storage2_enabled() {
                self.copy_text_line_storage2(target, line)
            } else {
                self.copy_text_line_storage1(target, line)
            }
        }
        #[cfg(not(feature = "postgres-execution"))]
        {
            let _ = (target, line);
            Err("fastpg-exec was built without PostgreSQL execution".to_owned())
        }
    }

    #[cfg(feature = "postgres-execution")]
    fn copy_text_line_storage1(&self, target: &CopyTarget, line: &str) -> Result<bool, String> {
        let line = line.trim_end_matches('\n').trim_end_matches('\r');
        if line == "\\." {
            return Ok(false);
        }

        let relation = relation_by_name(&target.table)
            .ok_or_else(|| format!("relation \"{}\" does not exist", target.table.trim()))?;
        let copy_columns = if target.column_names.is_empty() {
            relation.columns.iter().enumerate().collect::<Vec<_>>()
        } else {
            target
                .column_names
                .iter()
                .map(|name| {
                    let normalized = name.to_ascii_lowercase();
                    relation
                        .columns
                        .iter()
                        .enumerate()
                        .find(|(_, column)| column.name == normalized)
                        .ok_or_else(|| {
                            format!(
                                "column \"{}\" of relation \"{}\" does not exist",
                                name, relation.name
                            )
                        })
                })
                .collect::<Result<Vec<_>, String>>()?
        };
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != copy_columns.len() {
            return Err(format!(
                "COPY row for relation \"{}\" has {} fields but {} columns",
                relation.name,
                fields.len(),
                copy_columns.len()
            ));
        }

        let mut datums = (0..relation.columns.len())
            .map(|_| None)
            .collect::<Vec<Option<fastpg_storage::CopyDatum>>>();
        for (field, (column_index, column)) in fields.iter().zip(copy_columns) {
            let datum = if *field == "\\N" {
                None
            } else {
                let decoded = decode_copy_text_field(field);
                let datum = self
                    .pgcore_session
                    .input_text_datum(column.type_oid.0, column.type_mod, &decoded)
                    .map_err(pgcore_copy_error)?;
                Some(if datum.typbyval {
                    fastpg_storage::CopyDatum::by_value(datum.value)
                } else {
                    fastpg_storage::CopyDatum::by_reference(datum.payload.unwrap_or_default())
                })
            };
            datums[column_index] = datum;
        }

        fastpg_storage::insert_copy_datums(&target.table, datums)
    }

    #[cfg(feature = "postgres-execution")]
    fn copy_text_line_storage2(&self, target: &CopyTarget, line: &str) -> Result<bool, String> {
        let line = line.trim_end_matches('\n').trim_end_matches('\r');
        if line == "\\." {
            return Ok(false);
        }

        let relation = relation_by_name(&target.table)
            .ok_or_else(|| format!("relation \"{}\" does not exist", target.table.trim()))?;
        let copy_columns = if target.column_names.is_empty() {
            relation.columns.iter().enumerate().collect::<Vec<_>>()
        } else {
            target
                .column_names
                .iter()
                .map(|name| {
                    let normalized = name.to_ascii_lowercase();
                    relation
                        .columns
                        .iter()
                        .enumerate()
                        .find(|(_, column)| column.name == normalized)
                        .ok_or_else(|| {
                            format!(
                                "column \"{}\" of relation \"{}\" does not exist",
                                name, relation.name
                            )
                        })
                })
                .collect::<Result<Vec<_>, String>>()?
        };
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != copy_columns.len() {
            return Err(format!(
                "COPY row for relation \"{}\" has {} fields but {} columns",
                relation.name,
                fields.len(),
                copy_columns.len()
            ));
        }

        let mut datums = (0..relation.columns.len())
            .map(|_| None)
            .collect::<Vec<Option<fastpg_storage2::CopyDatum>>>();
        for (field, (column_index, column)) in fields.iter().zip(copy_columns) {
            let datum = if *field == "\\N" {
                None
            } else {
                let decoded = decode_copy_text_field(field);
                let datum = self
                    .pgcore_session
                    .input_text_datum(column.type_oid.0, column.type_mod, &decoded)
                    .map_err(pgcore_copy_error)?;
                Some(if datum.typbyval {
                    fastpg_storage2::CopyDatum::by_value(datum.value)
                } else {
                    fastpg_storage2::CopyDatum::by_reference(datum.payload.unwrap_or_default())
                })
            };
            datums[column_index] = datum;
        }

        fastpg_storage2::insert_copy_datums(&target.table, datums)
    }

    pub fn finish_copy(&self) {
        #[cfg(feature = "postgres-execution")]
        {
            let _guard = fastpg_storage::enter_session_storage(self.storage_session.clone());
            let _storage2_guard =
                fastpg_storage2::enter_session_storage(self.storage2_session.clone());
            fastpg_storage::commit_implicit_transaction();
            fastpg_storage2::fastpg_storage2_xact_commit_if_implicit();
        }
    }

    pub fn abort_copy(&self) {
        #[cfg(feature = "postgres-execution")]
        {
            let _guard = fastpg_storage::enter_session_storage(self.storage_session.clone());
            let _storage2_guard =
                fastpg_storage2::enter_session_storage(self.storage2_session.clone());
            fastpg_storage::abort_implicit_transaction();
            fastpg_storage2::fastpg_storage2_xact_abort_if_implicit();
        }
    }

    pub fn close(&self) {
        #[cfg(feature = "postgres-execution")]
        {
            let _guard = fastpg_storage::enter_session_storage(self.storage_session.clone());
            let _storage2_guard =
                fastpg_storage2::enter_session_storage(self.storage2_session.clone());
            if fastpg_storage::is_explicit_transaction() {
                fastpg_storage::abort_explicit_transaction();
                fastpg_storage2::fastpg_storage2_xact_abort();
            } else {
                fastpg_storage::abort_implicit_transaction();
                fastpg_storage2::fastpg_storage2_xact_abort_if_implicit();
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
        if parameters.is_empty()
            && let Some(command) = fast_transaction_command(sql)
        {
            return pgcore_execution_to_query_execution(
                self.pgcore_session.execute_transaction_command(command),
            );
        }

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

#[cfg(feature = "postgres-execution")]
fn fast_transaction_command(sql: &str) -> Option<PgCoreTransactionCommand> {
    let trimmed = sql.trim();
    let command = trimmed.strip_suffix(';').unwrap_or(trimmed).trim();
    if command.eq_ignore_ascii_case("BEGIN") {
        Some(PgCoreTransactionCommand::Begin)
    } else if command.eq_ignore_ascii_case("COMMIT") || command.eq_ignore_ascii_case("END") {
        Some(PgCoreTransactionCommand::Commit)
    } else if command.eq_ignore_ascii_case("ROLLBACK") {
        Some(PgCoreTransactionCommand::Rollback)
    } else {
        None
    }
}

#[cfg(feature = "postgres-execution")]
fn storage2_enabled() -> bool {
    std::env::var("FASTPG_STORAGE_ENGINE")
        .map(|value| value.eq_ignore_ascii_case("storage2"))
        .unwrap_or(false)
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
    QueryDescription::with_type_oids(
        description
            .parameter_type_oids
            .iter()
            .copied()
            .map(pg_type_for_oid)
            .collect(),
        description.parameter_type_oids,
        description
            .fields
            .into_iter()
            .map(|field| {
                Column::with_type_metadata(
                    field.name,
                    pg_type_for_oid(field.type_oid),
                    field.type_oid,
                    field.type_modifier,
                )
            })
            .collect(),
    )
}

#[cfg(feature = "postgres-execution")]
fn pgcore_execution_to_query_execution(result: PgCoreExecutionResult) -> QueryExecution {
    let notices = result
        .notices
        .into_iter()
        .map(query_notice_from_pgcore)
        .collect::<Vec<_>>();
    let execution = pgcore_statements_to_query_execution(result.statements);

    if notices.is_empty() {
        execution
    } else {
        QueryExecution::WithNotices {
            notices,
            execution: Box::new(execution),
        }
    }
}

#[cfg(feature = "postgres-execution")]
fn query_notice_from_pgcore(notice: PgCoreNotice) -> QueryNotice {
    QueryNotice {
        severity: notice.severity,
        sqlstate: notice.sqlstate,
        message: notice.message,
        detail: notice.detail,
        hint: notice.hint,
        context: notice.context,
        cursorpos: notice.cursorpos,
    }
}

#[cfg(feature = "postgres-execution")]
fn pgcore_statements_to_query_execution(
    statements: Vec<fastpg_pgcore::ExecutionStatement>,
) -> QueryExecution {
    let Some(statement) = statements.into_iter().next() else {
        return QueryExecution::Empty;
    };

    if let Some(copy_in) = statement.copy_in {
        return QueryExecution::CopyIn(CopyTarget {
            table: copy_in.table,
            columns: copy_in.columns,
            column_names: copy_in.column_names,
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
        .map(|field| {
            Column::with_type_metadata(
                field.name,
                pg_type_for_oid(field.type_oid),
                field.type_oid,
                field.type_modifier,
            )
        })
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

#[cfg(feature = "postgres-execution")]
fn decode_copy_text_field(field: &str) -> String {
    let mut decoded = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            decoded.push(ch);
            continue;
        }

        match chars.next() {
            Some('b') => decoded.push('\u{0008}'),
            Some('f') => decoded.push('\u{000c}'),
            Some('n') => decoded.push('\n'),
            Some('r') => decoded.push('\r'),
            Some('t') => decoded.push('\t'),
            Some('\\') => decoded.push('\\'),
            Some(other) => decoded.push(other),
            None => decoded.push('\\'),
        }
    }
    decoded
}

#[cfg(feature = "postgres-execution")]
fn pgcore_copy_error(error: fastpg_pgcore::PgCoreError) -> String {
    if let Some(detail) = error.detail {
        format!("{}: {}", error.message, detail)
    } else {
        error.message
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
    fn executes_simple_transaction_commands_without_pgcore_parse() {
        let executor = QueryExecutor::new("17.0-fastpg");

        assert_eq!(
            executor.execute("BEGIN;", &[]),
            QueryExecution::Command {
                tag: "BEGIN".into(),
                rows: 0,
            }
        );
        assert_eq!(
            executor.execute("ROLLBACK;", &[]),
            QueryExecution::Command {
                tag: "ROLLBACK".into(),
                rows: 0,
            }
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
            QueryDescription::with_type_oids(
                vec![PgType::Varchar],
                vec![2205],
                vec![Column::with_type_oid("relkind", PgType::Varchar, 18)]
            )
        );
        assert_eq!(
            executor.execute(sql, &[Value::Text(table.clone())]),
            QueryExecution::Rows(QueryResult::new(
                vec![Column::with_type_oid("relkind", PgType::Varchar, 18)],
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
                column_names: Vec::new(),
            })
        );
        assert!(executor.copy_text_line(&table, "1\t").unwrap());
        assert!(!executor.copy_text_line(&table, "\\.").unwrap());

        executor.execute(&format!("drop table if exists {table}"), &[]);
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn copy_from_stdin_honors_column_list() {
        let executor = QueryExecutor::new("17.0-fastpg");
        let table = format!("fastpg_exec_copy_columns_{}", std::process::id());
        executor.execute(&format!("create table {table}(id int, filler text)"), &[]);

        let copy = executor.execute(&format!("copy {table} (id) from stdin"), &[]);
        assert_eq!(
            copy,
            QueryExecution::CopyIn(CopyTarget {
                table: table.clone(),
                columns: 1,
                column_names: vec!["id".to_owned()],
            })
        );
        let QueryExecution::CopyIn(target) = copy else {
            unreachable!("assertion above verified COPY target");
        };
        assert!(executor.copy_target_text_line(&target, "42").unwrap());
        assert!(!executor.copy_target_text_line(&target, "\\.").unwrap());

        assert_eq!(
            executor.execute(&format!("select id, filler is null from {table}"), &[]),
            QueryExecution::Rows(QueryResult::new(
                vec![
                    Column::with_type_oid("id", PgType::Int4, 23),
                    Column::with_type_oid("?column?", PgType::Varchar, 16),
                ],
                vec![vec![Value::Int4(42), Value::Text("t".to_owned())]]
            ))
        );

        executor.execute(&format!("drop table if exists {table}"), &[]);
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn copy_from_stdin_uses_postgres_type_input() {
        let executor = QueryExecutor::new("17.0-fastpg");
        let table = format!("fastpg_exec_copy_varbit_{}", std::process::id());
        executor.execute(&format!("create table {table}(bits bit varying(8))"), &[]);

        let copy = executor.execute(&format!("copy {table} from stdin"), &[]);
        let QueryExecution::CopyIn(target) = copy else {
            panic!("expected COPY target, got {copy:?}");
        };
        assert!(executor.copy_target_text_line(&target, "101").unwrap());
        assert!(!executor.copy_target_text_line(&target, "\\.").unwrap());

        assert_eq!(
            executor.execute(&format!("select bits from {table}"), &[]),
            QueryExecution::Rows(QueryResult::new(
                vec![Column::with_type_metadata("bits", PgType::Varchar, 1562, 8)],
                vec![vec![Value::Text("101".to_owned())]]
            ))
        );

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
            vec![Column::with_type_oid("server_version", PgType::Varchar, 25)]
        );
        assert_eq!(result.rows.len(), 1);
        let Value::Text(server_version) = &result.rows[0][0] else {
            panic!("expected text server_version, got {:?}", result.rows[0][0]);
        };
        assert!(!server_version.is_empty());
        assert_ne!(server_version, "17.0-fastpg");
    }
}
