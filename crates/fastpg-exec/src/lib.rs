#![forbid(unsafe_code)]

#[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
use std::collections::BTreeMap;
use std::sync::Arc;
#[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
use std::sync::Mutex;

#[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
use fastpg_bind::{BoundExpression, BoundStatement, bind};
#[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
use fastpg_parser::{ParseError, parse};
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
        tag: String,
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
pub struct QueryExecutorShared {
    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    server_version: String,
    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    database: Arc<Mutex<DatabaseState>>,
}

impl QueryExecutorShared {
    pub fn new(server_version: impl Into<String>) -> Self {
        #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
        {
            Self {
                server_version: server_version.into(),
                database: Arc::new(Mutex::new(DatabaseState::default())),
            }
        }
        #[cfg(not(all(feature = "mini-sql-testkit", not(feature = "postgres-execution"))))]
        {
            let _ = server_version.into();
            Self {}
        }
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
    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    transaction: Mutex<SessionTransaction>,
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
        #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
        {
            Self {
                shared,
                transaction: Mutex::new(SessionTransaction::default()),
            }
        }
        #[cfg(not(any(feature = "postgres-execution", feature = "mini-sql-testkit")))]
        {
            Self { shared }
        }
    }

    pub fn describe(&self, sql: &str) -> Option<QueryDescription> {
        #[cfg(feature = "postgres-execution")]
        {
            self.describe_pgcore(sql)
        }
        #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
        {
            let statement = bind_sql(sql).ok()?;
            Some(QueryDescription::new(
                parameter_types(&statement),
                self.result_fields(&statement),
            ))
        }
        #[cfg(not(any(feature = "postgres-execution", feature = "mini-sql-testkit")))]
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
        #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
        {
            let statement = match bind_sql(sql) {
                Ok(statement) => statement,
                Err(error) => {
                    return parse_failure_execution(error);
                }
            };

            self.execute_bound(statement, parameters)
        }
        #[cfg(not(any(feature = "postgres-execution", feature = "mini-sql-testkit")))]
        {
            let _ = (sql, parameters);
            execution_error(
                "0A000",
                "fastpg-exec was built without PostgreSQL execution; the Rust mini SQL executor is only available with the mini-sql-testkit feature",
            )
        }
    }

    pub fn copy_text_line(&self, table: &str, line: &str) -> Result<bool, String> {
        #[cfg(feature = "postgres-execution")]
        {
            let _guard = fastpg_storage::enter_session_storage(self.storage_session.clone());
            fastpg_storage::copy_text_line(table, line)
        }
        #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
        {
            let line = line.trim_end_matches('\r');
            if line == "\\." {
                return Ok(false);
            }

            self.with_write_tables(|tables| {
                let table = tables
                    .get_mut(table)
                    .ok_or_else(|| format!("relation \"{table}\" does not exist"))?;
                table.copy_text_row(line)?;
                Ok(true)
            })
        }
        #[cfg(not(any(feature = "postgres-execution", feature = "mini-sql-testkit")))]
        {
            let _ = (table, line);
            Err("fastpg-exec was built without PostgreSQL execution; COPY is only available through pgcore or the mini-sql-testkit feature".to_owned())
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
        #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
        {
            self.rollback_session_transaction();
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

        let _guard = fastpg_storage::enter_session_storage(self.storage_session.clone());
        let statement = match self.pgcore_session.prepare(sql) {
            Ok(statement) => statement,
            Err(error) => return pgcore_error_execution(error),
        };
        let execution_result = statement.execute_with_params(&parameters);

        match execution_result {
            Ok(result) => pgcore_execution_to_query_execution(result),
            Err(error) => pgcore_error_execution(error),
        }
    }

    #[cfg(feature = "postgres-execution")]
    fn prepare_pgcore(&self, sql: &str) -> Result<PreparedStatement, fastpg_pgcore::PgCoreError> {
        let _guard = fastpg_storage::enter_session_storage(self.storage_session.clone());
        self.pgcore_session.prepare(sql)
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    fn with_read_tables<R>(&self, f: impl FnOnce(&BTreeMap<String, Table>) -> R) -> R {
        let transaction = self
            .transaction
            .lock()
            .expect("fastpg session transaction mutex poisoned");
        if let Some(tables) = transaction.tables.as_ref() {
            return f(tables);
        }
        drop(transaction);

        let database = self
            .shared
            .database
            .lock()
            .expect("fastpg database mutex poisoned");
        f(&database.tables)
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    fn with_write_tables<R>(&self, f: impl FnOnce(&mut BTreeMap<String, Table>) -> R) -> R {
        let mut transaction = self
            .transaction
            .lock()
            .expect("fastpg session transaction mutex poisoned");
        if transaction.tables.is_some() {
            let tables = transaction
                .tables
                .as_mut()
                .expect("checked transaction table presence");
            return f(tables);
        }
        drop(transaction);

        let mut database = self
            .shared
            .database
            .lock()
            .expect("fastpg database mutex poisoned");
        f(&mut database.tables)
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    fn begin_session_transaction(&self) {
        let mut transaction = self
            .transaction
            .lock()
            .expect("fastpg session transaction mutex poisoned");
        if transaction.tables.is_none() {
            let database = self
                .shared
                .database
                .lock()
                .expect("fastpg database mutex poisoned");
            transaction.tables = Some(database.tables.clone());
        }
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    fn commit_session_transaction(&self) {
        let tables = self
            .transaction
            .lock()
            .expect("fastpg session transaction mutex poisoned")
            .tables
            .take();
        if let Some(tables) = tables {
            let mut database = self
                .shared
                .database
                .lock()
                .expect("fastpg database mutex poisoned");
            database.tables = tables;
        }
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    fn rollback_session_transaction(&self) {
        self.transaction
            .lock()
            .expect("fastpg session transaction mutex poisoned")
            .tables
            .take();
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    fn execute_bound(&self, statement: BoundStatement, parameters: &[Value]) -> QueryExecution {
        match statement {
            BoundStatement::SelectOne => QueryExecution::Rows(QueryResult::new(
                result_fields_for_statement(&BoundStatement::SelectOne),
                vec![vec![Value::Int4(1)]],
            )),
            BoundStatement::ShowServerVersion => QueryExecution::Rows(QueryResult::new(
                result_fields_for_statement(&BoundStatement::ShowServerVersion),
                vec![vec![Value::Text(self.shared.server_version.clone())]],
            )),
            BoundStatement::SelectInt4Parameter => match parameters.first() {
                Some(Value::Int4(value)) => QueryExecution::Rows(QueryResult::new(
                    result_fields_for_statement(&BoundStatement::SelectInt4Parameter),
                    vec![vec![Value::Int4(*value)]],
                )),
                Some(Value::Null) => QueryExecution::Rows(QueryResult::new(
                    result_fields_for_statement(&BoundStatement::SelectInt4Parameter),
                    vec![vec![Value::Null]],
                )),
                Some(other) => QueryExecution::InvalidParameters {
                    message: format!("expected int4 parameter, got {other:?}"),
                },
                None => QueryExecution::InvalidParameters {
                    message: "missing int4 parameter".to_owned(),
                },
            },
            BoundStatement::SelectRelkindByRegclassParameter => {
                self.execute_relkind_lookup(parameters)
            }
            BoundStatement::SelectPgbenchPartitionInfo => self.execute_pgbench_partition_info(),
            BoundStatement::SelectCount { table } => self.execute_count(&table),
            BoundStatement::SelectColumnWhereInt {
                table,
                column,
                key_column,
                key_value,
            } => self.execute_select_column_where_int(&table, &column, &key_column, key_value),
            BoundStatement::DropTables { if_exists, names } => {
                self.execute_drop_tables(if_exists, &names)
            }
            BoundStatement::CreateTable { name, columns } => {
                self.execute_create_table(name, columns)
            }
            BoundStatement::TruncateTables { names } => self.execute_truncate_tables(&names),
            BoundStatement::Begin => self.execute_begin(),
            BoundStatement::Commit => self.execute_commit(),
            BoundStatement::Rollback => self.execute_rollback(),
            BoundStatement::CopyFromStdin { table } => self.execute_copy_from_stdin(&table),
            BoundStatement::UpdateAddInt {
                table,
                column,
                addend,
                key_column,
                key_value,
            } => self.execute_update_add_int(&table, &column, addend, &key_column, key_value),
            BoundStatement::Insert {
                table,
                columns,
                values,
            } => self.execute_insert(&table, &columns, &values),
        }
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    fn result_fields(&self, statement: &BoundStatement) -> Vec<Column> {
        if let BoundStatement::SelectColumnWhereInt { table, column, .. } = statement {
            if let Some(data_type) =
                self.with_read_tables(|tables| tables.get(table)?.column_type(column))
            {
                return vec![Column::new(column, data_type)];
            }
        }

        result_fields_for_statement(statement)
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    fn execute_relkind_lookup(&self, parameters: &[Value]) -> QueryExecution {
        let Some(Value::Text(table)) = parameters.first() else {
            return QueryExecution::InvalidParameters {
                message: "missing regclass text parameter".to_owned(),
            };
        };

        let table = normalize_identifier(table);
        if !self.with_read_tables(|tables| tables.contains_key(&table)) {
            return undefined_table(&table);
        }

        QueryExecution::Rows(QueryResult::new(
            result_fields_for_statement(&BoundStatement::SelectRelkindByRegclassParameter),
            vec![vec![Value::Text("r".to_owned())]],
        ))
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    fn execute_pgbench_partition_info(&self) -> QueryExecution {
        let rows = self
            .with_read_tables(|tables| tables.contains_key("pgbench_accounts"))
            .then(|| vec![Value::Int4(1), Value::Null, Value::Int8(0)])
            .into_iter()
            .collect::<Vec<_>>();

        QueryExecution::Rows(QueryResult::new(
            result_fields_for_statement(&BoundStatement::SelectPgbenchPartitionInfo),
            rows,
        ))
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    fn execute_count(&self, table: &str) -> QueryExecution {
        let Some(row_count) =
            self.with_read_tables(|tables| tables.get(table).map(|t| t.rows.len()))
        else {
            return undefined_table(table);
        };

        QueryExecution::Rows(QueryResult::new(
            result_fields_for_statement(&BoundStatement::SelectCount {
                table: String::new(),
            }),
            vec![vec![Value::Int8(row_count as i64)]],
        ))
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    fn execute_select_column_where_int(
        &self,
        table: &str,
        column: &str,
        key_column: &str,
        key_value: i64,
    ) -> QueryExecution {
        let result = self.with_read_tables(|tables| {
            tables
                .get(table)
                .ok_or_else(|| format!("relation \"{table}\" does not exist"))?
                .select_column_where_int(column, key_column, key_value)
        });

        match result {
            Ok((field, rows)) => QueryExecution::Rows(QueryResult::new(vec![field], rows)),
            Err(message) if message.starts_with("relation ") => execution_error("42P01", message),
            Err(message) => execution_error("42703", message),
        }
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    fn execute_drop_tables(&self, if_exists: bool, names: &[String]) -> QueryExecution {
        if let Some(missing) = self.with_write_tables(|tables| {
            for name in names {
                if tables.remove(name).is_none() && !if_exists {
                    return Some(name.clone());
                }
            }
            None
        }) {
            return undefined_table(&missing);
        }
        QueryExecution::Command {
            tag: "DROP TABLE".to_owned(),
            rows: 0,
        }
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    fn execute_create_table(&self, name: String, columns: Vec<Column>) -> QueryExecution {
        if self.with_write_tables(|tables| {
            if tables.contains_key(&name) {
                return false;
            }
            tables.insert(name.clone(), Table::new(columns));
            true
        }) {
            QueryExecution::Command {
                tag: "CREATE TABLE".to_owned(),
                rows: 0,
            }
        } else {
            return execution_error("42P07", format!("relation \"{name}\" already exists"));
        }
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    fn execute_truncate_tables(&self, names: &[String]) -> QueryExecution {
        if let Some(missing) = self.with_write_tables(|tables| {
            for name in names {
                let Some(table) = tables.get_mut(name) else {
                    return Some(name.clone());
                };
                table.rows.clear();
            }
            None
        }) {
            return undefined_table(&missing);
        }

        QueryExecution::Command {
            tag: "TRUNCATE TABLE".to_owned(),
            rows: 0,
        }
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    fn execute_begin(&self) -> QueryExecution {
        self.begin_session_transaction();
        QueryExecution::Command {
            tag: "BEGIN".to_owned(),
            rows: 0,
        }
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    fn execute_commit(&self) -> QueryExecution {
        self.commit_session_transaction();
        QueryExecution::Command {
            tag: "COMMIT".to_owned(),
            rows: 0,
        }
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    fn execute_rollback(&self) -> QueryExecution {
        self.rollback_session_transaction();
        QueryExecution::Command {
            tag: "ROLLBACK".to_owned(),
            rows: 0,
        }
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    fn execute_copy_from_stdin(&self, table: &str) -> QueryExecution {
        let Some(columns) =
            self.with_read_tables(|tables| tables.get(table).map(|t| t.columns.len()))
        else {
            return undefined_table(table);
        };

        QueryExecution::CopyIn(CopyTarget {
            table: table.to_owned(),
            columns,
        })
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    fn execute_update_add_int(
        &self,
        table: &str,
        column: &str,
        addend: i64,
        key_column: &str,
        key_value: i64,
    ) -> QueryExecution {
        let result = self.with_write_tables(|tables| {
            tables
                .get_mut(table)
                .ok_or_else(|| format!("relation \"{table}\" does not exist"))?
                .update_add_int(column, addend, key_column, key_value)
        });

        match result {
            Ok(rows) => QueryExecution::Command {
                tag: "UPDATE".to_owned(),
                rows,
            },
            Err(message) if message.starts_with("relation ") => execution_error("42P01", message),
            Err(message) => execution_error("42703", message),
        }
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    fn execute_insert(
        &self,
        table: &str,
        columns: &[String],
        values: &[BoundExpression],
    ) -> QueryExecution {
        let result = self.with_write_tables(|tables| {
            tables
                .get_mut(table)
                .ok_or_else(|| format!("relation \"{table}\" does not exist"))?
                .insert_values(columns, values)
        });

        match result {
            Ok(()) => QueryExecution::Command {
                tag: "INSERT".to_owned(),
                rows: 1,
            },
            Err(message) if message.starts_with("relation ") => execution_error("42P01", message),
            Err(message) => execution_error("42601", message),
        }
    }
}

#[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
fn bind_sql(sql: &str) -> Result<BoundStatement, ParseError> {
    parse(sql).map(bind)
}

#[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
fn parse_failure_execution(error: ParseError) -> QueryExecution {
    match (error.sqlstate, error.message) {
        (Some(sqlstate), Some(message)) => QueryExecution::Error {
            sqlstate,
            message,
            detail: None,
            hint: None,
            cursorpos: 0,
        },
        _ => QueryExecution::Unsupported { query: error.query },
    }
}

#[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
fn parameter_types(statement: &BoundStatement) -> Vec<PgType> {
    match statement {
        BoundStatement::SelectInt4Parameter => vec![PgType::Int4],
        BoundStatement::SelectRelkindByRegclassParameter => vec![PgType::Varchar],
        _ => vec![],
    }
}

#[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
fn result_fields_for_statement(statement: &BoundStatement) -> Vec<Column> {
    match statement {
        BoundStatement::SelectOne | BoundStatement::SelectInt4Parameter => {
            vec![Column::new("?column?", PgType::Int4)]
        }
        BoundStatement::ShowServerVersion => vec![Column::new("server_version", PgType::Varchar)],
        BoundStatement::SelectRelkindByRegclassParameter => {
            vec![Column::new("relkind", PgType::Varchar)]
        }
        BoundStatement::SelectPgbenchPartitionInfo => vec![
            Column::new("n", PgType::Int4),
            Column::new("partstrat", PgType::Varchar),
            Column::new("count", PgType::Int8),
        ],
        BoundStatement::SelectCount { .. } => vec![Column::new("count", PgType::Int8)],
        _ => vec![],
    }
}

#[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
fn undefined_table(table: &str) -> QueryExecution {
    execution_error("42P01", format!("relation \"{table}\" does not exist"))
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

#[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
fn normalize_identifier(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

#[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct DatabaseState {
    tables: BTreeMap<String, Table>,
}

#[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SessionTransaction {
    tables: Option<BTreeMap<String, Table>>,
}

#[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
#[derive(Clone, Debug, Eq, PartialEq)]
struct Table {
    columns: Vec<Column>,
    rows: Vec<Vec<Value>>,
}

#[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
impl Table {
    fn new(columns: Vec<Column>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
        }
    }

    fn column_index(&self, column: &str) -> Option<usize> {
        self.columns.iter().position(|field| field.name == column)
    }

    fn column_type(&self, column: &str) -> Option<PgType> {
        self.columns
            .iter()
            .find(|field| field.name == column)
            .map(|field| field.data_type)
    }

    fn copy_text_row(&mut self, line: &str) -> Result<(), String> {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != self.columns.len() {
            return Err(format!(
                "COPY row for relation has {} fields but {} columns",
                fields.len(),
                self.columns.len()
            ));
        }

        let row = fields
            .iter()
            .zip(&self.columns)
            .map(|(field, column)| copy_field_to_value(field, column.data_type))
            .collect::<Result<Vec<_>, _>>()?;
        self.rows.push(row);
        Ok(())
    }

    fn insert_values(
        &mut self,
        insert_columns: &[String],
        expressions: &[BoundExpression],
    ) -> Result<(), String> {
        let mut row = vec![Value::Null; self.columns.len()];
        if insert_columns.is_empty() {
            if expressions.len() != self.columns.len() {
                return Err(format!(
                    "INSERT has {} expressions but relation has {} columns",
                    expressions.len(),
                    self.columns.len()
                ));
            }
            for (idx, expression) in expressions.iter().enumerate() {
                row[idx] = expression_to_value(expression, self.columns[idx].data_type)?;
            }
        } else {
            if expressions.len() != insert_columns.len() {
                return Err(format!(
                    "INSERT has {} expressions but {} target columns",
                    expressions.len(),
                    insert_columns.len()
                ));
            }
            for (column, expression) in insert_columns.iter().zip(expressions) {
                let Some(idx) = self.column_index(column) else {
                    return Err(format!("column \"{column}\" does not exist"));
                };
                row[idx] = expression_to_value(expression, self.columns[idx].data_type)?;
            }
        }

        self.rows.push(row);
        Ok(())
    }

    fn update_add_int(
        &mut self,
        column: &str,
        addend: i64,
        key_column: &str,
        key_value: i64,
    ) -> Result<usize, String> {
        let Some(column_idx) = self.column_index(column) else {
            return Err(format!("column \"{column}\" does not exist"));
        };
        let Some(key_idx) = self.column_index(key_column) else {
            return Err(format!("column \"{key_column}\" does not exist"));
        };

        let mut updated = 0usize;
        for row in &mut self.rows {
            if !value_equals_i64(&row[key_idx], key_value) {
                continue;
            }
            updated += 1;
            match &mut row[column_idx] {
                Value::Int2(value) => {
                    *value = checked_i64_to_i16(*value as i64 + addend)?;
                }
                Value::Int4(value) => {
                    *value = checked_i64_to_i32(*value as i64 + addend)?;
                }
                Value::Int8(value) => {
                    *value = value
                        .checked_add(addend)
                        .ok_or_else(|| "bigint out of range".to_owned())?;
                }
                Value::Null => {}
                actual => {
                    return Err(format!("cannot add integer to {actual:?}"));
                }
            }
        }

        Ok(updated)
    }

    fn select_column_where_int(
        &self,
        column: &str,
        key_column: &str,
        key_value: i64,
    ) -> Result<(Column, Vec<Vec<Value>>), String> {
        let Some(column_idx) = self.column_index(column) else {
            return Err(format!("column \"{column}\" does not exist"));
        };
        let Some(key_idx) = self.column_index(key_column) else {
            return Err(format!("column \"{key_column}\" does not exist"));
        };

        let field = self.columns[column_idx].clone();
        let rows = self
            .rows
            .iter()
            .filter(|row| value_equals_i64(&row[key_idx], key_value))
            .map(|row| vec![row[column_idx].clone()])
            .collect();
        Ok((field, rows))
    }
}

#[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
fn copy_field_to_value(field: &str, data_type: PgType) -> Result<Value, String> {
    if field == "\\N" {
        return Ok(Value::Null);
    }

    match data_type {
        PgType::Int2 => field
            .parse::<i64>()
            .map_err(|_| format!("invalid int2 literal: {field}"))
            .and_then(checked_i64_to_i16)
            .map(Value::Int2),
        PgType::Int4 => field
            .parse::<i64>()
            .map_err(|_| format!("invalid int4 literal: {field}"))
            .and_then(checked_i64_to_i32)
            .map(Value::Int4),
        PgType::Int8 => field
            .parse::<i64>()
            .map(Value::Int8)
            .map_err(|_| format!("invalid int8 literal: {field}")),
        PgType::Varchar => Ok(Value::Text(decode_copy_text_field(field))),
    }
}

#[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
fn expression_to_value(expression: &BoundExpression, data_type: PgType) -> Result<Value, String> {
    match (expression, data_type) {
        (BoundExpression::Null, _) => Ok(Value::Null),
        (BoundExpression::Int(value), PgType::Int2) => checked_i64_to_i16(*value).map(Value::Int2),
        (BoundExpression::Int(value), PgType::Int4) => checked_i64_to_i32(*value).map(Value::Int4),
        (BoundExpression::Int(value), PgType::Int8) => Ok(Value::Int8(*value)),
        (BoundExpression::Int(value), PgType::Varchar) => Ok(Value::Text(value.to_string())),
        (BoundExpression::Text(value), PgType::Varchar) => Ok(Value::Text(value.clone())),
        (BoundExpression::Text(value), PgType::Int2) => value
            .parse::<i64>()
            .map_err(|_| format!("invalid int2 literal: {value}"))
            .and_then(checked_i64_to_i16)
            .map(Value::Int2),
        (BoundExpression::Text(value), PgType::Int4) => value
            .parse::<i64>()
            .map_err(|_| format!("invalid int4 literal: {value}"))
            .and_then(checked_i64_to_i32)
            .map(Value::Int4),
        (BoundExpression::Text(value), PgType::Int8) => value
            .parse::<i64>()
            .map(Value::Int8)
            .map_err(|_| format!("invalid int8 literal: {value}")),
        (BoundExpression::CurrentTimestamp, PgType::Varchar) => {
            Ok(Value::Text("CURRENT_TIMESTAMP".to_owned()))
        }
        (BoundExpression::CurrentTimestamp, _) => Ok(Value::Null),
    }
}

#[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
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

#[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
fn checked_i64_to_i32(value: i64) -> Result<i32, String> {
    i32::try_from(value).map_err(|_| "int4 out of range".to_owned())
}

#[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
fn checked_i64_to_i16(value: i64) -> Result<i16, String> {
    i16::try_from(value).map_err(|_| "int2 out of range".to_owned())
}

#[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
fn value_equals_i64(value: &Value, expected: i64) -> bool {
    match value {
        Value::Int2(value) => *value as i64 == expected,
        Value::Int4(value) => *value as i64 == expected,
        Value::Int8(value) => *value == expected,
        Value::Text(value) => value.parse::<i64>().is_ok_and(|value| value == expected),
        Value::Null => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(any(feature = "postgres-execution", feature = "mini-sql-testkit")))]
    #[test]
    fn default_build_has_no_rust_sql_executor() {
        let executor = QueryExecutor::new("17.0-fastpg");

        assert_eq!(executor.describe("SELECT 1"), None);
        let QueryExecution::Error {
            sqlstate, message, ..
        } = executor.execute("SELECT 1", &[])
        else {
            panic!("expected disabled executor error");
        };
        assert_eq!(sqlstate, "0A000");
        assert!(message.contains("mini SQL executor is only available"));
        assert!(executor.copy_text_line("smoke", "1").is_err());
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    #[test]
    fn describes_parameterized_int4_query() {
        let executor = QueryExecutor::new("17.0-fastpg");

        let description = executor.describe("SELECT $1::int4").unwrap();

        assert_eq!(description.parameter_types, vec![PgType::Int4]);
        assert_eq!(
            description.fields,
            vec![Column::new("?column?", PgType::Int4)]
        );
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    #[test]
    fn executes_server_version_query() {
        let executor = QueryExecutor::new("17.0-fastpg");

        let result = executor.execute("SHOW server_version", &[]);

        assert_eq!(
            result,
            QueryExecution::Rows(QueryResult::new(
                vec![Column::new("server_version", PgType::Varchar)],
                vec![vec![Value::Text("17.0-fastpg".to_owned())]]
            ))
        );
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    #[test]
    fn executes_pgbench_ddl_and_count() {
        let executor = QueryExecutor::new("17.0-fastpg");

        assert_eq!(
            executor.execute(
                "create table pgbench_branches(bid int not null,bbalance int,filler char(88))",
                &[]
            ),
            QueryExecution::Command {
                tag: "CREATE TABLE".to_owned(),
                rows: 0,
            }
        );
        assert_eq!(
            executor.execute("select count(*) from pgbench_branches", &[]),
            QueryExecution::Rows(QueryResult::new(
                vec![Column::new("count", PgType::Int8)],
                vec![vec![Value::Int8(0)]]
            ))
        );
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    #[test]
    fn copies_rows_and_updates_them() {
        let executor = QueryExecutor::new("17.0-fastpg");

        executor.execute(
            "create table pgbench_accounts(aid int not null,bid int,abalance int,filler char(84))",
            &[],
        );
        assert!(
            executor
                .copy_text_line("pgbench_accounts", "1\t1\t0\t")
                .unwrap()
        );
        assert_eq!(
            executor.execute(
                "UPDATE pgbench_accounts SET abalance = abalance + 5 WHERE aid = 1",
                &[]
            ),
            QueryExecution::Command {
                tag: "UPDATE".to_owned(),
                rows: 1,
            }
        );
        assert_eq!(
            executor.execute("SELECT abalance FROM pgbench_accounts WHERE aid = 1", &[]),
            QueryExecution::Rows(QueryResult::new(
                vec![Column::new("abalance", PgType::Int4)],
                vec![vec![Value::Int4(5)]]
            ))
        );
    }

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    #[test]
    fn explicit_transactions_are_per_executor() {
        let shared = Arc::new(QueryExecutorShared::new("17.0-fastpg"));
        let session_a = QueryExecutor::with_shared(shared.clone());
        let session_b = QueryExecutor::with_shared(shared);

        assert_eq!(
            session_a.execute("create table session_rows(id int)", &[]),
            QueryExecution::Command {
                tag: "CREATE TABLE".to_owned(),
                rows: 0,
            }
        );

        assert_eq!(
            session_a.execute("begin", &[]),
            QueryExecution::Command {
                tag: "BEGIN".to_owned(),
                rows: 0,
            }
        );
        assert_eq!(
            session_a.execute("insert into session_rows values (1)", &[]),
            QueryExecution::Command {
                tag: "INSERT".to_owned(),
                rows: 1,
            }
        );

        assert_eq!(count_rows(&session_a, "session_rows"), 1);
        assert_eq!(count_rows(&session_b, "session_rows"), 0);

        assert_eq!(
            session_a.execute("rollback", &[]),
            QueryExecution::Command {
                tag: "ROLLBACK".to_owned(),
                rows: 0,
            }
        );
        assert_eq!(count_rows(&session_a, "session_rows"), 0);

        session_a.execute("begin", &[]);
        session_a.execute("insert into session_rows values (2)", &[]);
        session_a.execute("commit", &[]);
        assert_eq!(count_rows(&session_b, "session_rows"), 1);
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
                tag: "CREATE TABLE".to_owned(),
                rows: 0,
            }
        );
        assert_eq!(
            executor.execute(&format!("drop table if exists {table}"), &[]),
            QueryExecution::Command {
                tag: "DROP TABLE".to_owned(),
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

    #[cfg(all(feature = "mini-sql-testkit", not(feature = "postgres-execution")))]
    fn count_rows(executor: &QueryExecutor, table: &str) -> i64 {
        let QueryExecution::Rows(result) =
            executor.execute(&format!("select count(*) from {table}"), &[])
        else {
            panic!("expected count rows");
        };
        let Value::Int8(count) = &result.rows[0][0] else {
            panic!("expected int8 count");
        };
        *count
    }
}
