#![forbid(unsafe_code)]

use std::borrow::Cow;
#[cfg(feature = "postgres-execution")]
use std::ffi::CStr;
#[cfg(feature = "postgres-execution")]
use std::fs;
#[cfg(feature = "postgres-execution")]
use std::io::Write;
#[cfg(feature = "postgres-execution")]
use std::path::{Path, PathBuf};
#[cfg(feature = "postgres-execution")]
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(feature = "postgres-execution")]
use fastpg_catalog::relation_by_name;
#[cfg(feature = "postgres-execution")]
use fastpg_pgcore::{
    ExecutionResult as PgCoreExecutionResult, INT2_OID, INT4_OID, INT8_OID, PgCoreNotice,
    PgCoreParam, PgCoreSession, PgCoreTransactionCommand, PgCoreValue, PreparedStatement,
    SimpleExecutionResult as PgCoreSimpleExecutionResult, TEXT_OID, VARCHAR_OID,
};
use fastpg_types::{Column, PgType, Value};

#[cfg(feature = "postgres-execution")]
const BPCHAR_OID: u32 = 1042;
pub const COPY_HEADER_MATCH: i32 = -1;
pub const COPY_HEADER_FALSE: i32 = 0;
pub const COPY_FORMAT_TEXT: i32 = 0;
pub const COPY_FORMAT_CSV: i32 = 2;
pub const COPY_ON_ERROR_STOP: i32 = 0;
pub const COPY_ON_ERROR_IGNORE: i32 = 1;
pub const COPY_ERROR_FIELDS_PREFIX: &str = "\x1ffastpg-copy-error-fields\n";
#[cfg(feature = "postgres-execution")]
static COPY_FILE_COUNTER: AtomicU64 = AtomicU64::new(1);

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
    pub command_tag: Option<Cow<'static, str>>,
    pub command_rows: Option<usize>,
}

impl QueryResult {
    pub fn new(fields: Vec<Column>, rows: Vec<Vec<Value>>) -> Self {
        Self {
            fields,
            rows,
            command_tag: None,
            command_rows: None,
        }
    }

    pub fn with_command_complete(
        mut self,
        command_tag: Cow<'static, str>,
        command_rows: Option<usize>,
    ) -> Self {
        self.command_tag = Some(command_tag);
        self.command_rows = command_rows;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CopyTarget {
    pub source_sql: String,
    pub table: String,
    pub table_oid: u32,
    pub relation_columns: usize,
    pub columns: usize,
    pub format: i32,
    pub header_line: i32,
    pub on_error: i32,
    pub freeze: bool,
    pub foreign_table: bool,
    pub partitioned_table: bool,
    pub has_insert_triggers: bool,
    pub has_generated_columns: bool,
    pub delimiter: String,
    pub null_print: String,
    pub default_print: Option<String>,
    pub column_names: Vec<String>,
    pub column_metadata: Vec<CopyColumn>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CopyColumn {
    pub name: String,
    pub attnum: i16,
    pub type_oid: u32,
    pub type_modifier: i32,
}

impl CopyTarget {
    pub fn header_lines_to_skip(&self) -> usize {
        match self.header_line {
            COPY_HEADER_FALSE => 0,
            COPY_HEADER_MATCH => 1,
            value if value > 0 => value as usize,
            _ => 0,
        }
    }

    pub fn validate_header_line(&self, line: &str) -> Result<(), String> {
        let fields = self.parse_input_fields(line)?;
        if fields.len() != self.column_metadata.len() {
            return Err(format!(
                "wrong number of fields in header line: got {}, expected {}",
                fields.len(),
                self.column_metadata.len()
            ));
        }
        for (index, (field, column)) in fields.iter().zip(&self.column_metadata).enumerate() {
            match field {
                Some(value) if value == &column.name => {}
                Some(value) => {
                    return Err(format!(
                        "column name mismatch in header line field {}: got \"{}\", expected \"{}\"",
                        index + 1,
                        value,
                        column.name
                    ));
                }
                None => {
                    return Err(format!(
                        "column name mismatch in header line field {}: got null value (\"{}\"), expected \"{}\"",
                        index + 1,
                        self.null_print,
                        column.name
                    ));
                }
            }
        }
        Ok(())
    }

    fn parse_input_fields(&self, line: &str) -> Result<Vec<Option<String>>, String> {
        let raw_fields = match self.format {
            COPY_FORMAT_CSV => parse_copy_csv_fields(line, delimiter_char(&self.delimiter, ','))?,
            COPY_FORMAT_TEXT => split_copy_text_fields(line, delimiter_char(&self.delimiter, '\t')),
            other => return Err(format!("COPY FROM format {} is not supported", other)),
        };
        Ok(raw_fields
            .into_iter()
            .map(|field| {
                if field == self.null_print {
                    None
                } else if self.format == COPY_FORMAT_TEXT {
                    Some(decode_copy_text_field(&field))
                } else {
                    Some(field)
                }
            })
            .collect())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CopyOutput {
    pub format: i8,
    pub columns: usize,
    pub chunks: Vec<Vec<u8>>,
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
pub enum QueryExecution {
    WithNotices {
        notices: Vec<QueryNotice>,
        execution: Box<QueryExecution>,
    },
    Empty,
    Batch(Vec<QueryExecution>),
    Rows(QueryResult),
    Command {
        tag: Cow<'static, str>,
        rows: Option<usize>,
    },
    CopyIn(CopyTarget),
    CopyOut(CopyOutput),
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
        context: Option<String>,
        cursorpos: i32,
        internal_query: Option<String>,
        internalpos: i32,
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
    #[cfg(feature = "postgres-execution")]
    pgcore_started: AtomicBool,
    #[cfg(feature = "postgres-execution")]
    pgcore_start_lock: Mutex<()>,
    #[cfg(feature = "postgres-execution")]
    notice_count: AtomicUsize,
    notices: Mutex<Vec<QueryNotice>>,
}

#[cfg(feature = "postgres-execution")]
#[derive(Debug)]
pub struct QueryExecutorCloseWork {
    pgcore_session: PgCoreSession,
    storage_session: fastpg_storage::SessionStorageHandle,
    storage2_session: fastpg_storage2::SessionStorageHandle,
    active_copy_owned_transaction: Option<bool>,
}

#[cfg(feature = "postgres-execution")]
impl QueryExecutorCloseWork {
    pub fn run(self) {
        let _guard = fastpg_storage::enter_session_storage(self.storage_session);
        let _storage2_guard = fastpg_storage2::enter_session_storage(self.storage2_session);

        if let Some(owned_transaction) = self.active_copy_owned_transaction {
            fastpg_storage::fastpg_rust_subxact_abort();
            fastpg_storage2::fastpg_storage2_subxact_abort();
            if owned_transaction {
                fastpg_storage::abort_implicit_transaction();
                fastpg_storage2::fastpg_storage2_xact_abort_if_implicit();
            } else if fastpg_storage::is_explicit_transaction() {
                fastpg_storage::abort_explicit_transaction();
                fastpg_storage2::fastpg_storage2_xact_abort();
            }
        }

        if fastpg_storage::is_explicit_transaction() {
            fastpg_storage::abort_explicit_transaction();
            fastpg_storage2::fastpg_storage2_xact_abort();
        } else {
            fastpg_storage::abort_implicit_transaction();
            fastpg_storage2::fastpg_storage2_xact_abort_if_implicit();
        }
        self.pgcore_session.reset_session_state();
        self.pgcore_session.end_client_session();
    }
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
            let pgcore_session = PgCoreSession::with_storage_sessions_and_database(
                storage_session.clone(),
                storage2_session.clone(),
                database,
            );
            Self {
                shared,
                pgcore_session,
                storage_session,
                storage2_session,
                pgcore_started: AtomicBool::new(false),
                pgcore_start_lock: Mutex::new(()),
                notice_count: AtomicUsize::new(0),
                notices: Mutex::new(Vec::new()),
            }
        }
        #[cfg(not(feature = "postgres-execution"))]
        {
            let _ = database.into();
            Self {
                shared,
                notices: Mutex::new(Vec::new()),
            }
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
            self.execute_pgcore(sql, parameters, PgCoreRowConversion::Typed)
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

    pub fn execute_simple_text(&self, sql: &str) -> QueryExecution {
        #[cfg(feature = "postgres-execution")]
        {
            self.execute_pgcore(sql, &[], PgCoreRowConversion::PreserveText)
        }
        #[cfg(not(feature = "postgres-execution"))]
        {
            let _ = sql;
            execution_error(
                "0A000",
                "fastpg-exec was built without PostgreSQL execution",
            )
        }
    }

    #[cfg(feature = "postgres-execution")]
    pub fn execute_simple_cstr(&self, sql: &CStr) -> QueryExecution {
        self.ensure_pgcore_session_started();
        if let Ok(sql_text) = sql.to_str()
            && let Some(command) = fast_transaction_command(sql_text)
        {
            return match self.pgcore_session.execute_transaction_command(command) {
                Ok(result) => {
                    pgcore_execution_to_query_execution(result, PgCoreRowConversion::PreserveText)
                }
                Err(error) => pgcore_error_execution(error),
            };
        }
        match self.pgcore_session.execute_simple_cstr_fast(sql) {
            Ok(PgCoreSimpleExecutionResult::Command { notices, tag, rows }) => {
                if !notices.is_empty() {
                    self.replace_notices(
                        notices
                            .into_iter()
                            .map(pgcore_notice_to_query_notice)
                            .collect(),
                    );
                }
                QueryExecution::Command { tag, rows }
            }
            Ok(PgCoreSimpleExecutionResult::Full(result)) => {
                let PgCoreExecutionResult {
                    notices,
                    statements,
                } = result;
                if !notices.is_empty() {
                    self.replace_notices(
                        notices
                            .into_iter()
                            .map(pgcore_notice_to_query_notice)
                            .collect(),
                    );
                }
                pgcore_statements_to_query_execution(statements, PgCoreRowConversion::PreserveText)
            }
            Err(error) => {
                if !error.notices.is_empty() {
                    self.replace_notices(
                        error
                            .notices
                            .iter()
                            .cloned()
                            .map(pgcore_notice_to_query_notice)
                            .collect(),
                    );
                }
                pgcore_error_execution(error)
            }
        }
    }

    pub fn take_notices(&self) -> Vec<QueryNotice> {
        #[cfg(feature = "postgres-execution")]
        if self.notice_count.load(Ordering::Relaxed) == 0 {
            return Vec::new();
        }

        let notices = std::mem::take(
            &mut *self
                .notices
                .lock()
                .expect("fastpg executor notice mutex poisoned"),
        );
        #[cfg(feature = "postgres-execution")]
        self.notice_count.store(0, Ordering::Relaxed);
        notices
    }

    #[cfg(feature = "postgres-execution")]
    fn replace_notices(&self, notices: Vec<QueryNotice>) {
        if notices.is_empty() {
            return;
        }

        let mut stored_notices = self
            .notices
            .lock()
            .expect("fastpg executor notice mutex poisoned");
        if stored_notices.is_empty() {
            *stored_notices = notices;
        } else {
            stored_notices.extend(notices);
        }
        self.notice_count
            .store(stored_notices.len(), Ordering::Relaxed);
    }

    #[cfg(feature = "postgres-execution")]
    fn ensure_pgcore_session_started(&self) {
        if self.pgcore_started.load(Ordering::Acquire) {
            return;
        }

        let _guard = self
            .pgcore_start_lock
            .lock()
            .expect("fastpg executor pgcore-start mutex poisoned");
        if !self.pgcore_started.load(Ordering::Relaxed) {
            let notices = self
                .pgcore_session
                .start_client_session()
                .into_iter()
                .map(pgcore_notice_to_query_notice)
                .collect();
            self.replace_notices(notices);
            self.pgcore_started.store(true, Ordering::Release);
        }
    }

    pub fn copy_text_line(&self, table: &str, line: &str) -> Result<bool, String> {
        self.copy_target_text_line(
            &CopyTarget {
                source_sql: String::new(),
                table: table.to_owned(),
                table_oid: 0,
                relation_columns: 0,
                columns: 0,
                format: COPY_FORMAT_TEXT,
                header_line: COPY_HEADER_FALSE,
                on_error: COPY_ON_ERROR_STOP,
                freeze: false,
                foreign_table: false,
                partitioned_table: false,
                has_insert_triggers: false,
                has_generated_columns: false,
                delimiter: "\t".to_owned(),
                null_print: "\\N".to_owned(),
                default_print: None,
                column_names: Vec::new(),
                column_metadata: Vec::new(),
            },
            line,
        )
    }

    pub fn copy_target_text_line(&self, target: &CopyTarget, line: &str) -> Result<bool, String> {
        #[cfg(feature = "postgres-execution")]
        {
            self.ensure_pgcore_session_started();
            let _guard = fastpg_storage::enter_session_storage(self.storage_session.clone());
            let _storage2_guard =
                fastpg_storage2::enter_session_storage(self.storage2_session.clone());
            if postgres_catalog_enabled() {
                return self.copy_text_line_postgres_catalog(target, line);
            }
            if storage2_enabled() {
                self.copy_text_line_storage2(target, line)
            } else {
                fastpg_storage::copy_text_line(&target.table, line)
            }
        }
        #[cfg(not(feature = "postgres-execution"))]
        {
            let _ = (target, line);
            Err("fastpg-exec was built without PostgreSQL execution".to_owned())
        }
    }

    pub fn copy_target_buffered_lines(
        &self,
        target: &CopyTarget,
        lines: &[&str],
    ) -> Result<Option<usize>, String> {
        #[cfg(feature = "postgres-execution")]
        {
            self.ensure_pgcore_session_started();
            let _guard = fastpg_storage::enter_session_storage(self.storage_session.clone());
            let _storage2_guard =
                fastpg_storage2::enter_session_storage(self.storage2_session.clone());
            if postgres_catalog_enabled() {
                return self.copy_buffered_postgres_catalog(target, lines).map(Some);
            }
        }
        #[cfg(not(feature = "postgres-execution"))]
        let _ = (target, lines);
        Ok(None)
    }

    pub fn begin_copy(&self) -> bool {
        #[cfg(feature = "postgres-execution")]
        {
            let _guard = fastpg_storage::enter_session_storage(self.storage_session.clone());
            let _storage2_guard =
                fastpg_storage2::enter_session_storage(self.storage2_session.clone());
            let owned_transaction = !fastpg_storage::is_explicit_transaction();
            fastpg_storage::fastpg_rust_subxact_begin();
            fastpg_storage2::fastpg_storage2_subxact_begin();
            owned_transaction
        }
        #[cfg(not(feature = "postgres-execution"))]
        {
            false
        }
    }

    #[cfg(feature = "postgres-execution")]
    fn copy_text_line_postgres_catalog(
        &self,
        target: &CopyTarget,
        line: &str,
    ) -> Result<bool, String> {
        let line = line.trim_end_matches('\n').trim_end_matches('\r');
        if line == "\\." {
            return Ok(false);
        }
        if line.contains("\\.") {
            return Err("end-of-copy marker is not alone on its line".to_owned());
        }
        if target.table_oid == 0 || target.relation_columns == 0 {
            return Err(format!(
                "COPY target \"{}\" is missing PostgreSQL catalog metadata",
                target.table
            ));
        }

        let fields = match target.parse_input_fields(line) {
            Ok(fields) => fields,
            Err(_) if target.on_error == COPY_ON_ERROR_IGNORE => return Ok(false),
            Err(error) => return Err(error),
        };
        if fields.len() != target.column_metadata.len() {
            return Err(format!(
                "COPY row for relation \"{}\" has {} fields but {} columns",
                target.table,
                fields.len(),
                target.column_metadata.len()
            ));
        }

        let mut datums = (0..target.relation_columns)
            .map(|_| None)
            .collect::<Vec<Option<fastpg_storage::CopyDatum>>>();
        for (field, column) in fields.iter().zip(&target.column_metadata) {
            if column.attnum <= 0 || column.attnum as usize > target.relation_columns {
                return Err(format!(
                    "COPY column \"{}\" of relation \"{}\" has invalid attribute number {}",
                    column.name, target.table, column.attnum
                ));
            }
            let Some(decoded) = field else {
                datums[column.attnum as usize - 1] = None;
                continue;
            };
            let datum = match self.pgcore_session.input_text_datum(
                column.type_oid,
                column.type_modifier,
                decoded,
            ) {
                Ok(datum) => datum,
                Err(_) if target.on_error == COPY_ON_ERROR_IGNORE => return Ok(false),
                Err(error) => return Err(pgcore_copy_error(error)),
            };
            let datum = Some(if datum.typbyval {
                fastpg_storage::CopyDatum::by_value(datum.value)
            } else {
                fastpg_storage::CopyDatum::by_reference(datum.payload.unwrap_or_default())
            });
            datums[column.attnum as usize - 1] = datum;
        }

        fastpg_storage::insert_copy_datums_by_oid(
            target.table_oid,
            &target.table,
            target.relation_columns,
            datums,
        )
    }

    #[cfg(feature = "postgres-execution")]
    fn copy_buffered_postgres_catalog(
        &self,
        target: &CopyTarget,
        lines: &[&str],
    ) -> Result<usize, String> {
        if !target.source_sql.is_empty() {
            let data = copy_lines_to_bytes(lines);
            let result = match self
                .pgcore_session
                .execute_copy_from_stdin(&target.source_sql, &data)
            {
                Ok(result) => result,
                Err(error) => {
                    self.replace_notices(
                        error
                            .notices
                            .iter()
                            .cloned()
                            .map(pgcore_notice_to_query_notice)
                            .collect(),
                    );
                    return Err(pgcore_copy_error(error));
                }
            };
            self.replace_notices(
                result
                    .notices
                    .iter()
                    .cloned()
                    .map(pgcore_notice_to_query_notice)
                    .collect(),
            );
            let Some(statement) = result.statements.into_iter().next() else {
                return Ok(lines
                    .len()
                    .saturating_sub(target.header_lines_to_skip().min(lines.len())));
            };
            return Ok(statement.command_rows.unwrap_or_else(|| {
                lines
                    .len()
                    .saturating_sub(target.header_lines_to_skip().min(lines.len()))
            }));
        }

        let path = write_copy_temp_file(lines)?;
        let sql = copy_from_file_sql(target, &path);
        let execution = self.execute_pgcore(&sql, &[], PgCoreRowConversion::Typed);
        let _ = fs::remove_file(&path);
        match execution {
            QueryExecution::Error {
                message, context, ..
            } => {
                if let Some(context) = context {
                    Err(format!("{message}\n{context}"))
                } else {
                    Err(message)
                }
            }
            QueryExecution::Command { .. } => Ok(lines
                .len()
                .saturating_sub(target.header_lines_to_skip().min(lines.len()))),
            other => Err(format!("unexpected COPY execution result: {other:?}")),
        }
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

    pub fn finish_copy(&self, owned_transaction: bool) {
        #[cfg(feature = "postgres-execution")]
        {
            let _guard = fastpg_storage::enter_session_storage(self.storage_session.clone());
            let _storage2_guard =
                fastpg_storage2::enter_session_storage(self.storage2_session.clone());
            fastpg_storage::fastpg_rust_subxact_commit();
            fastpg_storage2::fastpg_storage2_subxact_commit();
            if owned_transaction {
                fastpg_storage::commit_implicit_transaction();
                fastpg_storage2::fastpg_storage2_xact_commit_if_implicit();
            }
        }
        #[cfg(not(feature = "postgres-execution"))]
        let _ = owned_transaction;
    }

    pub fn abort_copy(&self, owned_transaction: bool) {
        #[cfg(feature = "postgres-execution")]
        {
            let _guard = fastpg_storage::enter_session_storage(self.storage_session.clone());
            let _storage2_guard =
                fastpg_storage2::enter_session_storage(self.storage2_session.clone());
            fastpg_storage::fastpg_rust_subxact_abort();
            fastpg_storage2::fastpg_storage2_subxact_abort();
            if owned_transaction {
                fastpg_storage::abort_implicit_transaction();
                fastpg_storage2::fastpg_storage2_xact_abort_if_implicit();
            } else if fastpg_storage::is_explicit_transaction() {
                fastpg_storage::abort_explicit_transaction();
                fastpg_storage2::fastpg_storage2_xact_abort();
            }
        }
        #[cfg(not(feature = "postgres-execution"))]
        let _ = owned_transaction;
    }

    pub fn close(&self) {
        #[cfg(feature = "postgres-execution")]
        {
            if let Some(work) = self.close_work(None) {
                work.run();
            }
        }
    }

    #[cfg(feature = "postgres-execution")]
    pub fn close_work(
        &self,
        active_copy_owned_transaction: Option<bool>,
    ) -> Option<QueryExecutorCloseWork> {
        if !self.pgcore_started.load(Ordering::Acquire) {
            return None;
        }

        Some(QueryExecutorCloseWork {
            pgcore_session: self.pgcore_session.clone(),
            storage_session: self.storage_session.clone(),
            storage2_session: self.storage2_session.clone(),
            active_copy_owned_transaction,
        })
    }

    #[cfg(feature = "postgres-execution")]
    fn describe_pgcore(&self, sql: &str) -> Option<QueryDescription> {
        self.ensure_pgcore_session_started();
        self.prepare_pgcore(sql)
            .ok()
            .map(|statement| query_description_from_pgcore(&statement))
    }

    #[cfg(feature = "postgres-execution")]
    fn execute_pgcore(
        &self,
        sql: &str,
        parameters: &[Value],
        row_conversion: PgCoreRowConversion,
    ) -> QueryExecution {
        self.ensure_pgcore_session_started();
        if parameters.is_empty()
            && let Some(command) = fast_transaction_command(sql)
        {
            return match self.pgcore_session.execute_transaction_command(command) {
                Ok(result) => pgcore_execution_to_query_execution(result, row_conversion),
                Err(error) => pgcore_error_execution(error),
            };
        }

        let execution_result = if parameters.is_empty() {
            self.pgcore_session.execute_simple(sql)
        } else {
            let parameters = parameters
                .iter()
                .map(pgcore_param_value)
                .collect::<Vec<_>>();
            self.pgcore_session
                .prepare(sql)
                .and_then(|statement| statement.execute_with_params(&parameters))
        };

        match execution_result {
            Ok(result) => {
                let PgCoreExecutionResult {
                    notices,
                    statements,
                } = result;
                if !notices.is_empty() {
                    self.replace_notices(
                        notices
                            .into_iter()
                            .map(pgcore_notice_to_query_notice)
                            .collect(),
                    );
                }
                pgcore_statements_to_query_execution(statements, row_conversion)
            }
            Err(error) => {
                if !error.notices.is_empty() {
                    self.replace_notices(
                        error
                            .notices
                            .iter()
                            .cloned()
                            .map(pgcore_notice_to_query_notice)
                            .collect(),
                    );
                }
                pgcore_error_execution(error)
            }
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
    static STORAGE2_ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

    *STORAGE2_ENABLED.get_or_init(|| {
        std::env::var("FASTPG_STORAGE_ENGINE")
            .map(|value| value.eq_ignore_ascii_case("storage2"))
            .unwrap_or(true)
    })
}

#[cfg(feature = "postgres-execution")]
fn postgres_catalog_enabled() -> bool {
    !cfg!(feature = "rust-catalog")
}

fn execution_error(sqlstate: impl Into<String>, message: impl Into<String>) -> QueryExecution {
    QueryExecution::Error {
        sqlstate: sqlstate.into(),
        message: message.into(),
        detail: None,
        hint: None,
        context: None,
        cursorpos: 0,
        internal_query: None,
        internalpos: 0,
    }
}

#[cfg(feature = "postgres-execution")]
fn pgcore_error_execution(error: fastpg_pgcore::PgCoreError) -> QueryExecution {
    let error = error.into_fields();
    let error_execution = QueryExecution::Error {
        sqlstate: error.sqlstate,
        message: error.message,
        detail: error.detail,
        hint: error.hint,
        context: error.context,
        cursorpos: error.cursorpos,
        internal_query: error.internal_query,
        internalpos: error.internalpos,
    };
    if error.partial.is_empty() {
        error_execution
    } else {
        let mut executions = error
            .partial
            .into_iter()
            .map(|statement| {
                pgcore_statement_to_query_execution(statement, PgCoreRowConversion::Typed)
            })
            .collect::<Vec<_>>();
        executions.push(error_execution);
        QueryExecution::Batch(executions)
    }
}

#[cfg(feature = "postgres-execution")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PgCoreRowConversion {
    Typed,
    PreserveText,
}

#[cfg(feature = "postgres-execution")]
fn pgcore_notice_to_query_notice(notice: PgCoreNotice) -> QueryNotice {
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
fn pgcore_execution_to_query_execution(
    result: PgCoreExecutionResult,
    row_conversion: PgCoreRowConversion,
) -> QueryExecution {
    pgcore_statements_to_query_execution(result.statements, row_conversion)
}

#[cfg(feature = "postgres-execution")]
fn pgcore_statements_to_query_execution(
    statements: Vec<fastpg_pgcore::ExecutionStatement>,
    row_conversion: PgCoreRowConversion,
) -> QueryExecution {
    match statements.len() {
        0 => QueryExecution::Empty,
        1 => {
            let statement = statements
                .into_iter()
                .next()
                .expect("single-statement result contains one statement");
            pgcore_statement_to_query_execution(statement, row_conversion)
        }
        _ => QueryExecution::Batch(
            statements
                .into_iter()
                .map(|statement| pgcore_statement_to_query_execution(statement, row_conversion))
                .collect(),
        ),
    }
}

#[cfg(feature = "postgres-execution")]
fn pgcore_statement_to_query_execution(
    statement: fastpg_pgcore::ExecutionStatement,
    row_conversion: PgCoreRowConversion,
) -> QueryExecution {
    if let Some(copy_in) = statement.copy_in {
        return QueryExecution::CopyIn(CopyTarget {
            source_sql: copy_in.source_sql,
            table: copy_in.table,
            table_oid: copy_in.table_oid,
            relation_columns: copy_in.relation_columns,
            columns: copy_in.columns,
            format: copy_in.format,
            header_line: copy_in.header_line,
            on_error: copy_in.on_error,
            freeze: copy_in.freeze,
            foreign_table: copy_in.foreign_table,
            partitioned_table: copy_in.partitioned_table,
            has_insert_triggers: copy_in.has_insert_triggers,
            has_generated_columns: copy_in.has_generated_columns,
            delimiter: copy_in.delimiter,
            null_print: copy_in.null_print,
            default_print: copy_in.default_print,
            column_names: copy_in.column_names,
            column_metadata: copy_in
                .column_metadata
                .into_iter()
                .map(|column| CopyColumn {
                    name: column.name,
                    attnum: column.attnum,
                    type_oid: column.type_oid,
                    type_modifier: column.type_modifier,
                })
                .collect(),
        });
    }

    if let Some(copy_out) = statement.copy_out {
        return QueryExecution::CopyOut(CopyOutput {
            format: copy_out.format,
            columns: copy_out.columns,
            chunks: copy_out.chunks,
        });
    }

    if statement.fields.is_empty()
        && statement.is_select
        && statement.command_tag.as_ref() == "SELECT"
    {
        return QueryExecution::Rows(QueryResult::new(
            Vec::new(),
            statement.rows.into_iter().map(|_| Vec::new()).collect(),
        ));
    }

    if statement.fields.is_empty() {
        return QueryExecution::Command {
            tag: statement.command_tag,
            rows: statement.command_rows,
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
                .map(|(value, field)| pgcore_value_to_value(value, field.data_type, row_conversion))
                .collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<Vec<_>, _>>();

    match rows {
        Ok(rows) => {
            let result = QueryResult::new(fields, rows);
            if statement.is_select
                && statement.command_tag.as_ref() == "SELECT"
                && statement.command_rows.is_none()
            {
                QueryExecution::Rows(result)
            } else {
                QueryExecution::Rows(
                    result.with_command_complete(statement.command_tag, statement.command_rows),
                )
            }
        }
        Err(message) => execution_error("22P02", message),
    }
}

#[cfg(feature = "postgres-execution")]
fn pgcore_param_value(value: &Value) -> PgCoreParam {
    match value {
        Value::Int2(value) => PgCoreParam::Datum(*value as usize),
        Value::Int4(value) => PgCoreParam::Datum(*value as usize),
        Value::Int8(value) => PgCoreParam::Datum(*value as usize),
        Value::Text(value) | Value::RawText(value) => PgCoreParam::Text(value.clone()),
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
fn pgcore_value_to_value(
    value: PgCoreValue,
    data_type: PgType,
    row_conversion: PgCoreRowConversion,
) -> Result<Value, String> {
    match (value, data_type, row_conversion) {
        (PgCoreValue::Text(value), _, PgCoreRowConversion::PreserveText) => {
            Ok(Value::RawText(value))
        }
        (PgCoreValue::Null, _, PgCoreRowConversion::PreserveText) => Ok(Value::Null),
        (PgCoreValue::Null, _, PgCoreRowConversion::Typed) => Ok(Value::Null),
        (PgCoreValue::Text(value), PgType::Int2, PgCoreRowConversion::Typed) => value
            .parse::<i16>()
            .map(Value::Int2)
            .map_err(|error| format!("cannot decode PostgreSQL int2 value {value:?}: {error}")),
        (PgCoreValue::Text(value), PgType::Int4, PgCoreRowConversion::Typed) => value
            .parse::<i32>()
            .map(Value::Int4)
            .map_err(|error| format!("cannot decode PostgreSQL int4 value {value:?}: {error}")),
        (PgCoreValue::Text(value), PgType::Int8, PgCoreRowConversion::Typed) => value
            .parse::<i64>()
            .map(Value::Int8)
            .map_err(|error| format!("cannot decode PostgreSQL int8 value {value:?}: {error}")),
        (PgCoreValue::Text(value), PgType::Varchar, PgCoreRowConversion::Typed) => {
            Ok(Value::Text(value))
        }
    }
}

fn delimiter_char(delimiter: &str, default: char) -> char {
    delimiter.chars().next().unwrap_or(default)
}

fn split_copy_text_fields(line: &str, delimiter: char) -> Vec<String> {
    line.split(delimiter).map(str::to_owned).collect()
}

fn parse_copy_csv_fields(line: &str, delimiter: char) -> Result<Vec<String>, String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = line.chars().peekable();
    let mut in_quotes = false;

    while let Some(ch) = chars.next() {
        if in_quotes {
            if ch == '"' {
                if matches!(chars.peek(), Some('"')) {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(ch);
            }
        } else if ch == delimiter {
            fields.push(std::mem::take(&mut field));
        } else if ch == '"' && field.is_empty() {
            in_quotes = true;
        } else {
            field.push(ch);
        }
    }

    if in_quotes {
        return Err("unterminated CSV quoted field".to_owned());
    }
    fields.push(field);
    Ok(fields)
}

#[cfg(feature = "postgres-execution")]
fn copy_lines_to_bytes(lines: &[&str]) -> Vec<u8> {
    let mut data = Vec::new();
    for line in lines {
        data.extend_from_slice(line.as_bytes());
        data.push(b'\n');
    }
    data
}

#[cfg(feature = "postgres-execution")]
fn write_copy_temp_file(lines: &[&str]) -> Result<PathBuf, String> {
    let mut path = std::env::temp_dir();
    let counter = COPY_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    path.push(format!("fastpg-copy-{}-{counter}.dat", std::process::id()));
    let mut file = fs::File::create(&path).map_err(|error| {
        format!(
            "failed to create COPY temp file {}: {error}",
            path.display()
        )
    })?;
    for line in lines {
        file.write_all(line.as_bytes()).map_err(|error| {
            format!("failed to write COPY temp file {}: {error}", path.display())
        })?;
        file.write_all(b"\n").map_err(|error| {
            format!("failed to write COPY temp file {}: {error}", path.display())
        })?;
    }
    Ok(path)
}

#[cfg(feature = "postgres-execution")]
fn copy_from_file_sql(target: &CopyTarget, path: &Path) -> String {
    let columns = if target.column_names.is_empty() {
        String::new()
    } else {
        format!(
            " ({})",
            target
                .column_names
                .iter()
                .map(|name| quote_sql_ident(name))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let mut options = Vec::new();
    options.push(format!(
        "FORMAT {}",
        match target.format {
            COPY_FORMAT_CSV => "csv",
            _ => "text",
        }
    ));
    match target.header_line {
        COPY_HEADER_MATCH => options.push("HEADER MATCH".to_owned()),
        value if value > 0 => options.push(format!("HEADER {value}")),
        _ => {}
    }
    options.push(format!(
        "DELIMITER {}",
        quote_sql_literal(&target.delimiter)
    ));
    options.push(format!("NULL {}", quote_sql_literal(&target.null_print)));
    if let Some(default_print) = &target.default_print {
        options.push(format!("DEFAULT {}", quote_sql_literal(default_print)));
    }
    if target.on_error == COPY_ON_ERROR_IGNORE {
        options.push("ON_ERROR ignore".to_owned());
    }
    if target.freeze {
        options.push("FREEZE true".to_owned());
    }
    format!(
        "COPY {}{} FROM {} WITH ({})",
        quote_sql_ident(&target.table),
        columns,
        quote_sql_literal(&path.to_string_lossy()),
        options.join(", ")
    )
}

#[cfg(feature = "postgres-execution")]
fn quote_sql_ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

#[cfg(feature = "postgres-execution")]
fn quote_sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
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

#[cfg(feature = "postgres-execution")]
fn pgcore_copy_error(error: fastpg_pgcore::PgCoreError) -> String {
    let error = error.into_fields();
    format!(
        "{}{}\n{}\n{}\n{}",
        COPY_ERROR_FIELDS_PREFIX,
        error.message,
        error.detail.unwrap_or_default(),
        error.hint.unwrap_or_default(),
        error.context.unwrap_or_default()
    )
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
                rows: None,
            }
        );
        assert_eq!(
            executor.execute("ROLLBACK;", &[]),
            QueryExecution::Command {
                tag: "ROLLBACK".into(),
                rows: None,
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
                rows: None,
            }
        );
        assert_eq!(
            executor.execute(&format!("drop table if exists {table}"), &[]),
            QueryExecution::Command {
                tag: "DROP TABLE".into(),
                rows: None,
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

        match executor.execute(&format!("copy {table} from stdin with (freeze on)"), &[]) {
            QueryExecution::CopyIn(target) => {
                assert_eq!(target.table, table);
                assert_eq!(target.columns, 2);
            }
            other => panic!("expected COPY, got {other:?}"),
        }
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
