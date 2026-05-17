#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use fastpg_bind::{BoundExpression, BoundStatement, bind};
use fastpg_parser::parse;
use fastpg_types::{Column, PgType, Value};

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
    Rows(QueryResult),
    Command { tag: String, rows: usize },
    CopyIn(CopyTarget),
    Unsupported { query: String },
    InvalidParameters { message: String },
    Error { sqlstate: String, message: String },
}

#[derive(Clone, Debug)]
pub struct QueryExecutor {
    server_version: String,
    database: Arc<Mutex<DatabaseState>>,
}

impl QueryExecutor {
    pub fn new(server_version: impl Into<String>) -> Self {
        Self {
            server_version: server_version.into(),
            database: Arc::new(Mutex::new(DatabaseState::default())),
        }
    }

    pub fn describe(&self, sql: &str) -> Option<QueryDescription> {
        let statement = bind_sql(sql).ok()?;
        Some(QueryDescription::new(
            parameter_types(&statement),
            self.result_fields(&statement),
        ))
    }

    pub fn execute(&self, sql: &str, parameters: &[Value]) -> QueryExecution {
        let Ok(statement) = bind_sql(sql) else {
            return QueryExecution::Unsupported {
                query: sql.to_owned(),
            };
        };

        self.execute_bound(statement, parameters)
    }

    pub fn copy_text_line(&self, table: &str, line: &str) -> Result<bool, String> {
        let line = line.trim_end_matches('\r');
        if line == "\\." {
            return Ok(false);
        }

        let mut database = self
            .database
            .lock()
            .expect("fastpg database mutex poisoned");
        let table = database
            .tables
            .get_mut(table)
            .ok_or_else(|| format!("relation \"{table}\" does not exist"))?;
        table.copy_text_row(line)?;
        Ok(true)
    }

    fn execute_bound(&self, statement: BoundStatement, parameters: &[Value]) -> QueryExecution {
        match statement {
            BoundStatement::SelectOne => QueryExecution::Rows(QueryResult::new(
                result_fields_for_statement(&BoundStatement::SelectOne),
                vec![vec![Value::Int4(1)]],
            )),
            BoundStatement::ShowServerVersion => QueryExecution::Rows(QueryResult::new(
                result_fields_for_statement(&BoundStatement::ShowServerVersion),
                vec![vec![Value::Text(self.server_version.clone())]],
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

    fn result_fields(&self, statement: &BoundStatement) -> Vec<Column> {
        if let BoundStatement::SelectColumnWhereInt { table, column, .. } = statement {
            let database = self
                .database
                .lock()
                .expect("fastpg database mutex poisoned");
            if let Some(data_type) = database.column_type(table, column) {
                return vec![Column::new(column, data_type)];
            }
        }

        result_fields_for_statement(statement)
    }

    fn execute_relkind_lookup(&self, parameters: &[Value]) -> QueryExecution {
        let Some(Value::Text(table)) = parameters.first() else {
            return QueryExecution::InvalidParameters {
                message: "missing regclass text parameter".to_owned(),
            };
        };

        let table = normalize_identifier(table);
        let database = self
            .database
            .lock()
            .expect("fastpg database mutex poisoned");
        if !database.tables.contains_key(&table) {
            return undefined_table(&table);
        }

        QueryExecution::Rows(QueryResult::new(
            result_fields_for_statement(&BoundStatement::SelectRelkindByRegclassParameter),
            vec![vec![Value::Text("r".to_owned())]],
        ))
    }

    fn execute_pgbench_partition_info(&self) -> QueryExecution {
        let database = self
            .database
            .lock()
            .expect("fastpg database mutex poisoned");
        let rows = database
            .tables
            .contains_key("pgbench_accounts")
            .then(|| vec![Value::Int4(1), Value::Null, Value::Int8(0)])
            .into_iter()
            .collect::<Vec<_>>();

        QueryExecution::Rows(QueryResult::new(
            result_fields_for_statement(&BoundStatement::SelectPgbenchPartitionInfo),
            rows,
        ))
    }

    fn execute_count(&self, table: &str) -> QueryExecution {
        let database = self
            .database
            .lock()
            .expect("fastpg database mutex poisoned");
        let Some(table) = database.tables.get(table) else {
            return undefined_table(table);
        };

        QueryExecution::Rows(QueryResult::new(
            result_fields_for_statement(&BoundStatement::SelectCount {
                table: String::new(),
            }),
            vec![vec![Value::Int8(table.rows.len() as i64)]],
        ))
    }

    fn execute_select_column_where_int(
        &self,
        table: &str,
        column: &str,
        key_column: &str,
        key_value: i64,
    ) -> QueryExecution {
        let database = self
            .database
            .lock()
            .expect("fastpg database mutex poisoned");
        let Some(table_ref) = database.tables.get(table) else {
            return undefined_table(table);
        };

        match table_ref.select_column_where_int(column, key_column, key_value) {
            Ok((field, rows)) => QueryExecution::Rows(QueryResult::new(vec![field], rows)),
            Err(message) => execution_error("42703", message),
        }
    }

    fn execute_drop_tables(&self, if_exists: bool, names: &[String]) -> QueryExecution {
        let mut database = self
            .database
            .lock()
            .expect("fastpg database mutex poisoned");
        for name in names {
            if database.tables.remove(name).is_none() && !if_exists {
                return undefined_table(name);
            }
        }
        QueryExecution::Command {
            tag: "DROP TABLE".to_owned(),
            rows: 0,
        }
    }

    fn execute_create_table(&self, name: String, columns: Vec<Column>) -> QueryExecution {
        let mut database = self
            .database
            .lock()
            .expect("fastpg database mutex poisoned");
        if database.tables.contains_key(&name) {
            return execution_error("42P07", format!("relation \"{name}\" already exists"));
        }

        database.tables.insert(name, Table::new(columns));
        QueryExecution::Command {
            tag: "CREATE TABLE".to_owned(),
            rows: 0,
        }
    }

    fn execute_truncate_tables(&self, names: &[String]) -> QueryExecution {
        let mut database = self
            .database
            .lock()
            .expect("fastpg database mutex poisoned");
        for name in names {
            let Some(table) = database.tables.get_mut(name) else {
                return undefined_table(name);
            };
            table.rows.clear();
        }

        QueryExecution::Command {
            tag: "TRUNCATE TABLE".to_owned(),
            rows: 0,
        }
    }

    fn execute_begin(&self) -> QueryExecution {
        let mut database = self
            .database
            .lock()
            .expect("fastpg database mutex poisoned");
        let snapshot = database.tables.clone();
        database.snapshots.push(snapshot);
        QueryExecution::Command {
            tag: "BEGIN".to_owned(),
            rows: 0,
        }
    }

    fn execute_commit(&self) -> QueryExecution {
        let mut database = self
            .database
            .lock()
            .expect("fastpg database mutex poisoned");
        database.snapshots.pop();
        QueryExecution::Command {
            tag: "COMMIT".to_owned(),
            rows: 0,
        }
    }

    fn execute_rollback(&self) -> QueryExecution {
        let mut database = self
            .database
            .lock()
            .expect("fastpg database mutex poisoned");
        if let Some(snapshot) = database.snapshots.pop() {
            database.tables = snapshot;
        }
        QueryExecution::Command {
            tag: "ROLLBACK".to_owned(),
            rows: 0,
        }
    }

    fn execute_copy_from_stdin(&self, table: &str) -> QueryExecution {
        let database = self
            .database
            .lock()
            .expect("fastpg database mutex poisoned");
        let Some(table_ref) = database.tables.get(table) else {
            return undefined_table(table);
        };

        QueryExecution::CopyIn(CopyTarget {
            table: table.to_owned(),
            columns: table_ref.columns.len(),
        })
    }

    fn execute_update_add_int(
        &self,
        table: &str,
        column: &str,
        addend: i64,
        key_column: &str,
        key_value: i64,
    ) -> QueryExecution {
        let mut database = self
            .database
            .lock()
            .expect("fastpg database mutex poisoned");
        let Some(table_ref) = database.tables.get_mut(table) else {
            return undefined_table(table);
        };

        match table_ref.update_add_int(column, addend, key_column, key_value) {
            Ok(rows) => QueryExecution::Command {
                tag: "UPDATE".to_owned(),
                rows,
            },
            Err(message) => execution_error("42703", message),
        }
    }

    fn execute_insert(
        &self,
        table: &str,
        columns: &[String],
        values: &[BoundExpression],
    ) -> QueryExecution {
        let mut database = self
            .database
            .lock()
            .expect("fastpg database mutex poisoned");
        let Some(table_ref) = database.tables.get_mut(table) else {
            return undefined_table(table);
        };

        match table_ref.insert_values(columns, values) {
            Ok(()) => QueryExecution::Command {
                tag: "INSERT".to_owned(),
                rows: 1,
            },
            Err(message) => execution_error("42601", message),
        }
    }
}

fn bind_sql(sql: &str) -> Result<BoundStatement, ()> {
    parse(sql).map(bind).map_err(|_| ())
}

fn parameter_types(statement: &BoundStatement) -> Vec<PgType> {
    match statement {
        BoundStatement::SelectInt4Parameter => vec![PgType::Int4],
        BoundStatement::SelectRelkindByRegclassParameter => vec![PgType::Varchar],
        _ => vec![],
    }
}

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

fn undefined_table(table: &str) -> QueryExecution {
    execution_error("42P01", format!("relation \"{table}\" does not exist"))
}

fn execution_error(sqlstate: impl Into<String>, message: impl Into<String>) -> QueryExecution {
    QueryExecution::Error {
        sqlstate: sqlstate.into(),
        message: message.into(),
    }
}

fn normalize_identifier(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct DatabaseState {
    tables: BTreeMap<String, Table>,
    snapshots: Vec<BTreeMap<String, Table>>,
}

impl DatabaseState {
    fn column_type(&self, table: &str, column: &str) -> Option<PgType> {
        self.tables.get(table)?.column_type(column)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Table {
    columns: Vec<Column>,
    rows: Vec<Vec<Value>>,
}

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

fn copy_field_to_value(field: &str, data_type: PgType) -> Result<Value, String> {
    if field == "\\N" {
        return Ok(Value::Null);
    }

    match data_type {
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

fn expression_to_value(expression: &BoundExpression, data_type: PgType) -> Result<Value, String> {
    match (expression, data_type) {
        (BoundExpression::Null, _) => Ok(Value::Null),
        (BoundExpression::Int(value), PgType::Int4) => checked_i64_to_i32(*value).map(Value::Int4),
        (BoundExpression::Int(value), PgType::Int8) => Ok(Value::Int8(*value)),
        (BoundExpression::Int(value), PgType::Varchar) => Ok(Value::Text(value.to_string())),
        (BoundExpression::Text(value), PgType::Varchar) => Ok(Value::Text(value.clone())),
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

fn checked_i64_to_i32(value: i64) -> Result<i32, String> {
    i32::try_from(value).map_err(|_| "int4 out of range".to_owned())
}

fn value_equals_i64(value: &Value, expected: i64) -> bool {
    match value {
        Value::Int4(value) => *value as i64 == expected,
        Value::Int8(value) => *value == expected,
        Value::Text(value) => value.parse::<i64>().is_ok_and(|value| value == expected),
        Value::Null => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
