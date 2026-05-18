#![deny(unsafe_op_in_unsafe_fn)]

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawParseSummary {
    pub statement_count: usize,
}

pub const INT8_OID: u32 = 20;
pub const INT4_OID: u32 = 23;
pub const TEXT_OID: u32 = 25;
pub const VARCHAR_OID: u32 = 1043;
pub const REGCLASS_OID: u32 = 2205;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PgCoreError {
    pub sqlstate: String,
    pub message: String,
    pub cursorpos: i32,
}

impl PgCoreError {
    pub fn new(sqlstate: impl Into<String>, message: impl Into<String>, cursorpos: i32) -> Self {
        Self {
            sqlstate: sqlstate.into(),
            message: message.into(),
            cursorpos,
        }
    }
}

pub fn raw_parse(sql: &str) -> Result<RawParseSummary, PgCoreError> {
    inner::raw_parse(sql)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PgCoreField {
    pub name: String,
    pub type_oid: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatementDescription {
    pub parameter_type_oids: Vec<u32>,
    pub fields: Vec<PgCoreField>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PgCoreValue {
    Text(String),
    Null,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PgCoreParam {
    Text(String),
    Null,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PgCoreCopyIn {
    pub table: String,
    pub columns: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionStatement {
    pub command_tag: String,
    pub fields: Vec<PgCoreField>,
    pub rows: Vec<Vec<PgCoreValue>>,
    pub copy_in: Option<PgCoreCopyIn>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionResult {
    pub statements: Vec<ExecutionStatement>,
}

#[derive(Clone, Debug)]
pub struct PgCoreSession {
    inner: inner::PgCoreSession,
}

impl PgCoreSession {
    pub fn new() -> Self {
        Self {
            inner: inner::PgCoreSession::new(),
        }
    }

    pub fn prepare(&self, sql: &str) -> Result<PreparedStatement, PgCoreError> {
        self.inner
            .prepare(sql)
            .map(|inner| PreparedStatement { inner })
    }
}

impl Default for PgCoreSession {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct PreparedStatement {
    inner: inner::PreparedStatement,
}

// pgcore is backed by PostgreSQL backend globals, so every public operation is
// serialized by the pgcore lane. That lets session-level Rust caches hold
// prepared handles behind Arcs without allowing concurrent C backend access.
unsafe impl Send for PreparedStatement {}
unsafe impl Sync for PreparedStatement {}

impl PreparedStatement {
    pub fn describe(&self) -> StatementDescription {
        self.inner.describe()
    }

    pub fn execute(&self) -> Result<ExecutionResult, PgCoreError> {
        self.inner.execute()
    }

    pub fn execute_with_params(
        &self,
        params: &[PgCoreParam],
    ) -> Result<ExecutionResult, PgCoreError> {
        self.inner.execute_with_params(params)
    }
}

#[derive(Debug)]
pub struct Portal {
    statement: PreparedStatement,
}

impl Portal {
    pub fn new(statement: PreparedStatement) -> Self {
        Self { statement }
    }

    pub fn execute(&self) -> Result<ExecutionResult, PgCoreError> {
        self.statement.execute()
    }

    pub fn execute_with_params(
        &self,
        params: &[PgCoreParam],
    ) -> Result<ExecutionResult, PgCoreError> {
        self.statement.execute_with_params(params)
    }
}

#[cfg(feature = "postgres-linked")]
mod inner {
    use std::ffi::{CStr, CString, c_char};
    use std::ptr;
    use std::ptr::NonNull;
    use std::sync::Mutex;

    use super::{
        ExecutionResult, ExecutionStatement, PgCoreCopyIn, PgCoreError, PgCoreField, PgCoreParam,
        PgCoreValue, RawParseSummary, StatementDescription,
    };

    static PGCORE_LOCK: Mutex<()> = Mutex::new(());

    #[repr(C)]
    struct FastPgPgCoreParseResult {
        _private: [u8; 0],
    }

    #[repr(C)]
    struct FastPgPgCorePrepared {
        _private: [u8; 0],
    }

    #[repr(C)]
    struct FastPgPgCoreExecuteResult {
        _private: [u8; 0],
    }

    unsafe extern "C" {
        fn fastpg_pgcore_raw_parse(sql: *const c_char) -> *mut FastPgPgCoreParseResult;
        fn fastpg_pgcore_parse_result_free(result: *mut FastPgPgCoreParseResult);
        fn fastpg_pgcore_parse_result_ok(result: *const FastPgPgCoreParseResult) -> bool;
        fn fastpg_pgcore_parse_result_statement_count(
            result: *const FastPgPgCoreParseResult,
        ) -> i32;
        fn fastpg_pgcore_parse_result_sqlstate(
            result: *const FastPgPgCoreParseResult,
        ) -> *const c_char;
        fn fastpg_pgcore_parse_result_message(
            result: *const FastPgPgCoreParseResult,
        ) -> *const c_char;
        fn fastpg_pgcore_parse_result_cursorpos(result: *const FastPgPgCoreParseResult) -> i32;
        fn fastpg_pgcore_prepare(sql: *const c_char) -> *mut FastPgPgCorePrepared;
        fn fastpg_pgcore_prepared_free(prepared: *mut FastPgPgCorePrepared);
        fn fastpg_pgcore_prepared_ok(prepared: *const FastPgPgCorePrepared) -> bool;
        fn fastpg_pgcore_prepared_sqlstate(prepared: *const FastPgPgCorePrepared) -> *const c_char;
        fn fastpg_pgcore_prepared_message(prepared: *const FastPgPgCorePrepared) -> *const c_char;
        fn fastpg_pgcore_prepared_cursorpos(prepared: *const FastPgPgCorePrepared) -> i32;
        fn fastpg_pgcore_prepared_parameter_count(prepared: *const FastPgPgCorePrepared) -> i32;
        fn fastpg_pgcore_prepared_parameter_type_oid(
            prepared: *const FastPgPgCorePrepared,
            index: i32,
        ) -> u32;
        fn fastpg_pgcore_prepared_field_count(prepared: *const FastPgPgCorePrepared) -> i32;
        fn fastpg_pgcore_prepared_field_name(
            prepared: *const FastPgPgCorePrepared,
            index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_prepared_field_type_oid(
            prepared: *const FastPgPgCorePrepared,
            index: i32,
        ) -> u32;
        fn fastpg_pgcore_execute(
            prepared: *const FastPgPgCorePrepared,
        ) -> *mut FastPgPgCoreExecuteResult;
        fn fastpg_pgcore_execute_params(
            prepared: *const FastPgPgCorePrepared,
            parameter_values: *const *const c_char,
            parameter_is_null: *const bool,
            parameter_count: i32,
        ) -> *mut FastPgPgCoreExecuteResult;
        fn fastpg_pgcore_execute_result_free(result: *mut FastPgPgCoreExecuteResult);
        fn fastpg_pgcore_execute_result_ok(result: *const FastPgPgCoreExecuteResult) -> bool;
        fn fastpg_pgcore_execute_result_sqlstate(
            result: *const FastPgPgCoreExecuteResult,
        ) -> *const c_char;
        fn fastpg_pgcore_execute_result_message(
            result: *const FastPgPgCoreExecuteResult,
        ) -> *const c_char;
        fn fastpg_pgcore_execute_result_cursorpos(result: *const FastPgPgCoreExecuteResult) -> i32;
        fn fastpg_pgcore_execute_statement_count(result: *const FastPgPgCoreExecuteResult) -> i32;
        fn fastpg_pgcore_execute_statement_command_tag(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_execute_statement_column_count(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> i32;
        fn fastpg_pgcore_execute_statement_row_count(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> i32;
        fn fastpg_pgcore_execute_statement_is_copy_in(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> bool;
        fn fastpg_pgcore_execute_statement_copy_table(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_execute_statement_copy_column_count(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> i32;
        fn fastpg_pgcore_execute_column_name(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
            column_index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_execute_column_type_oid(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
            column_index: i32,
        ) -> u32;
        fn fastpg_pgcore_execute_value_is_null(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
            row_index: i32,
            column_index: i32,
        ) -> bool;
        fn fastpg_pgcore_execute_value_text(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
            row_index: i32,
            column_index: i32,
        ) -> *const c_char;
    }

    pub fn raw_parse(sql: &str) -> Result<RawParseSummary, PgCoreError> {
        let c_sql = CString::new(sql)
            .map_err(|_| PgCoreError::new("22023", "query contains an embedded NUL byte", 0))?;
        let _guard = PGCORE_LOCK
            .lock()
            .expect("fastpg pgcore raw parser mutex poisoned");

        let result = unsafe { fastpg_pgcore_raw_parse(c_sql.as_ptr()) };
        let Some(result) = NonNull::new(result) else {
            return Err(PgCoreError::new(
                "XX000",
                "PostgreSQL raw parser returned a null result",
                0,
            ));
        };
        let result = ParseResult(result);

        if unsafe { fastpg_pgcore_parse_result_ok(result.as_ptr()) } {
            let statement_count =
                unsafe { fastpg_pgcore_parse_result_statement_count(result.as_ptr()) };
            Ok(RawParseSummary {
                statement_count: statement_count.max(0) as usize,
            })
        } else {
            Err(PgCoreError::new(
                unsafe { c_string(fastpg_pgcore_parse_result_sqlstate(result.as_ptr())) },
                unsafe { c_string(fastpg_pgcore_parse_result_message(result.as_ptr())) },
                unsafe { fastpg_pgcore_parse_result_cursorpos(result.as_ptr()) },
            ))
        }
    }

    struct ParseResult(NonNull<FastPgPgCoreParseResult>);

    impl ParseResult {
        fn as_ptr(&self) -> *const FastPgPgCoreParseResult {
            self.0.as_ptr()
        }
    }

    impl Drop for ParseResult {
        fn drop(&mut self) {
            unsafe {
                fastpg_pgcore_parse_result_free(self.0.as_ptr());
            }
        }
    }

    unsafe fn c_string(value: *const c_char) -> String {
        if value.is_null() {
            return String::new();
        }
        unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .into_owned()
    }

    #[derive(Clone, Debug)]
    pub struct PgCoreSession;

    impl PgCoreSession {
        pub fn new() -> Self {
            let _ = fastpg_storage::fastpg_rust_relation_row_count(0);
            Self
        }

        pub fn prepare(&self, sql: &str) -> Result<PreparedStatement, PgCoreError> {
            let c_sql = CString::new(sql)
                .map_err(|_| PgCoreError::new("22023", "query contains an embedded NUL byte", 0))?;
            let _guard = PGCORE_LOCK
                .lock()
                .expect("fastpg pgcore prepare mutex poisoned");
            let prepared = unsafe { fastpg_pgcore_prepare(c_sql.as_ptr()) };
            let Some(prepared) = NonNull::new(prepared) else {
                return Err(PgCoreError::new(
                    "XX000",
                    "PostgreSQL prepare returned a null result",
                    0,
                ));
            };
            let prepared = PreparedStatement(prepared);
            if unsafe { fastpg_pgcore_prepared_ok(prepared.as_ptr()) } {
                drop(_guard);
                Ok(prepared)
            } else {
                let error = PgCoreError::new(
                    unsafe { c_string(fastpg_pgcore_prepared_sqlstate(prepared.as_ptr())) },
                    unsafe { c_string(fastpg_pgcore_prepared_message(prepared.as_ptr())) },
                    unsafe { fastpg_pgcore_prepared_cursorpos(prepared.as_ptr()) },
                );
                drop(_guard);
                Err(error)
            }
        }
    }

    #[derive(Debug)]
    pub struct PreparedStatement(NonNull<FastPgPgCorePrepared>);

    impl PreparedStatement {
        fn as_ptr(&self) -> *const FastPgPgCorePrepared {
            self.0.as_ptr()
        }

        pub fn describe(&self) -> StatementDescription {
            let _guard = PGCORE_LOCK
                .lock()
                .expect("fastpg pgcore describe mutex poisoned");
            let parameter_count =
                unsafe { fastpg_pgcore_prepared_parameter_count(self.as_ptr()) }.max(0);
            let field_count = unsafe { fastpg_pgcore_prepared_field_count(self.as_ptr()) }.max(0);
            StatementDescription {
                parameter_type_oids: (0..parameter_count)
                    .map(|index| unsafe {
                        fastpg_pgcore_prepared_parameter_type_oid(self.as_ptr(), index)
                    })
                    .collect(),
                fields: (0..field_count)
                    .map(|index| PgCoreField {
                        name: unsafe {
                            c_string(fastpg_pgcore_prepared_field_name(self.as_ptr(), index))
                        },
                        type_oid: unsafe {
                            fastpg_pgcore_prepared_field_type_oid(self.as_ptr(), index)
                        },
                    })
                    .collect(),
            }
        }

        pub fn execute(&self) -> Result<ExecutionResult, PgCoreError> {
            self.execute_with_params(&[])
        }

        pub fn execute_with_params(
            &self,
            params: &[PgCoreParam],
        ) -> Result<ExecutionResult, PgCoreError> {
            let param_count = i32::try_from(params.len()).map_err(|_| {
                PgCoreError::new("54000", "too many parameters for PostgreSQL execution", 0)
            })?;
            let encoded_params = params
                .iter()
                .map(|param| match param {
                    PgCoreParam::Text(value) => {
                        CString::new(value.as_str()).map(Some).map_err(|_| {
                            PgCoreError::new(
                                "22023",
                                "query parameter contains an embedded NUL byte",
                                0,
                            )
                        })
                    }
                    PgCoreParam::Null => Ok(None),
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut parameter_values = Vec::with_capacity(encoded_params.len());
            let mut parameter_is_null = Vec::with_capacity(encoded_params.len());
            for param in &encoded_params {
                match param {
                    Some(value) => {
                        parameter_values.push(value.as_ptr());
                        parameter_is_null.push(false);
                    }
                    None => {
                        parameter_values.push(ptr::null());
                        parameter_is_null.push(true);
                    }
                }
            }

            let _guard = PGCORE_LOCK
                .lock()
                .expect("fastpg pgcore execute mutex poisoned");
            let result = if params.is_empty() {
                unsafe { fastpg_pgcore_execute(self.as_ptr()) }
            } else {
                unsafe {
                    fastpg_pgcore_execute_params(
                        self.as_ptr(),
                        parameter_values.as_ptr(),
                        parameter_is_null.as_ptr(),
                        param_count,
                    )
                }
            };
            let Some(result) = NonNull::new(result) else {
                return Err(PgCoreError::new(
                    "XX000",
                    "PostgreSQL execute returned a null result",
                    0,
                ));
            };
            let result = ExecuteResult(result);
            if unsafe { fastpg_pgcore_execute_result_ok(result.as_ptr()) } {
                Ok(result.to_execution_result())
            } else {
                Err(PgCoreError::new(
                    unsafe { c_string(fastpg_pgcore_execute_result_sqlstate(result.as_ptr())) },
                    unsafe { c_string(fastpg_pgcore_execute_result_message(result.as_ptr())) },
                    unsafe { fastpg_pgcore_execute_result_cursorpos(result.as_ptr()) },
                ))
            }
        }
    }

    impl Drop for PreparedStatement {
        fn drop(&mut self) {
            let _guard = PGCORE_LOCK
                .lock()
                .expect("fastpg pgcore prepared free mutex poisoned");
            unsafe {
                fastpg_pgcore_prepared_free(self.0.as_ptr());
            }
        }
    }

    struct ExecuteResult(NonNull<FastPgPgCoreExecuteResult>);

    impl ExecuteResult {
        fn as_ptr(&self) -> *const FastPgPgCoreExecuteResult {
            self.0.as_ptr()
        }

        fn to_execution_result(&self) -> ExecutionResult {
            let statement_count =
                unsafe { fastpg_pgcore_execute_statement_count(self.as_ptr()) }.max(0);
            let statements = (0..statement_count)
                .map(|statement_index| {
                    let field_count = unsafe {
                        fastpg_pgcore_execute_statement_column_count(self.as_ptr(), statement_index)
                    }
                    .max(0);
                    let row_count = unsafe {
                        fastpg_pgcore_execute_statement_row_count(self.as_ptr(), statement_index)
                    }
                    .max(0);
                    let copy_in = unsafe {
                        fastpg_pgcore_execute_statement_is_copy_in(self.as_ptr(), statement_index)
                    }
                    .then(|| PgCoreCopyIn {
                        table: unsafe {
                            c_string(fastpg_pgcore_execute_statement_copy_table(
                                self.as_ptr(),
                                statement_index,
                            ))
                        },
                        columns: unsafe {
                            fastpg_pgcore_execute_statement_copy_column_count(
                                self.as_ptr(),
                                statement_index,
                            )
                        }
                        .max(0) as usize,
                    });
                    let fields = (0..field_count)
                        .map(|column_index| PgCoreField {
                            name: unsafe {
                                c_string(fastpg_pgcore_execute_column_name(
                                    self.as_ptr(),
                                    statement_index,
                                    column_index,
                                ))
                            },
                            type_oid: unsafe {
                                fastpg_pgcore_execute_column_type_oid(
                                    self.as_ptr(),
                                    statement_index,
                                    column_index,
                                )
                            },
                        })
                        .collect::<Vec<_>>();
                    let rows = (0..row_count)
                        .map(|row_index| {
                            (0..field_count)
                                .map(|column_index| {
                                    if unsafe {
                                        fastpg_pgcore_execute_value_is_null(
                                            self.as_ptr(),
                                            statement_index,
                                            row_index,
                                            column_index,
                                        )
                                    } {
                                        PgCoreValue::Null
                                    } else {
                                        PgCoreValue::Text(unsafe {
                                            c_string(fastpg_pgcore_execute_value_text(
                                                self.as_ptr(),
                                                statement_index,
                                                row_index,
                                                column_index,
                                            ))
                                        })
                                    }
                                })
                                .collect::<Vec<_>>()
                        })
                        .collect::<Vec<_>>();
                    ExecutionStatement {
                        command_tag: unsafe {
                            c_string(fastpg_pgcore_execute_statement_command_tag(
                                self.as_ptr(),
                                statement_index,
                            ))
                        },
                        fields,
                        rows,
                        copy_in,
                    }
                })
                .collect();
            ExecutionResult { statements }
        }
    }

    impl Drop for ExecuteResult {
        fn drop(&mut self) {
            unsafe {
                fastpg_pgcore_execute_result_free(self.0.as_ptr());
            }
        }
    }
}

#[cfg(not(feature = "postgres-linked"))]
mod inner {
    use super::{ExecutionResult, PgCoreError, RawParseSummary, StatementDescription};

    pub fn raw_parse(_sql: &str) -> Result<RawParseSummary, PgCoreError> {
        Ok(RawParseSummary { statement_count: 0 })
    }

    #[derive(Clone, Debug)]
    pub struct PgCoreSession;

    impl PgCoreSession {
        pub fn new() -> Self {
            Self
        }

        pub fn prepare(&self, _sql: &str) -> Result<PreparedStatement, PgCoreError> {
            Err(PgCoreError::new(
                "0A000",
                "fastpg-pgcore was built without postgres-linked support",
                0,
            ))
        }
    }

    #[derive(Debug)]
    pub struct PreparedStatement;

    impl PreparedStatement {
        pub fn describe(&self) -> StatementDescription {
            StatementDescription {
                parameter_type_oids: Vec::new(),
                fields: Vec::new(),
            }
        }

        pub fn execute(&self) -> Result<ExecutionResult, PgCoreError> {
            Err(PgCoreError::new(
                "0A000",
                "fastpg-pgcore was built without postgres-linked support",
                0,
            ))
        }

        pub fn execute_with_params(
            &self,
            _params: &[super::PgCoreParam],
        ) -> Result<ExecutionResult, PgCoreError> {
            Err(PgCoreError::new(
                "0A000",
                "fastpg-pgcore was built without postgres-linked support",
                0,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_parse_smoke() {
        let summary = raw_parse("select 1").unwrap();
        #[cfg(feature = "postgres-linked")]
        assert_eq!(summary.statement_count, 1);
        #[cfg(not(feature = "postgres-linked"))]
        assert_eq!(summary.statement_count, 0);
    }

    #[cfg(feature = "postgres-linked")]
    #[test]
    fn raw_parse_reports_postgres_syntax_errors() {
        let error = raw_parse("select from").unwrap_err();
        assert_eq!(error.sqlstate, "42601");
        assert!(error.message.contains("syntax error"));
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn prepare_describes_select_one() {
        let session = PgCoreSession::new();
        let statement = session.prepare("select 1").unwrap();
        let description = statement.describe();
        assert_eq!(description.parameter_type_oids, Vec::<u32>::new());
        assert_eq!(description.fields.len(), 1);
        assert_eq!(description.fields[0].type_oid, INT4_OID);
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn execute_select_one() {
        let session = PgCoreSession::new();
        let statement = session.prepare("select 1").unwrap();
        let result = statement.execute().unwrap();
        assert_eq!(result.statements.len(), 1);
        assert_eq!(
            result.statements[0].rows,
            vec![vec![PgCoreValue::Text("1".to_owned())]]
        );
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn execute_parameterized_select_through_executor_params() {
        let session = PgCoreSession::new();
        let statement = session.prepare("select $1::int4").unwrap();
        let description = statement.describe();
        assert_eq!(description.parameter_type_oids, vec![INT4_OID]);

        let result = statement
            .execute_with_params(&[PgCoreParam::Text("41".to_owned())])
            .unwrap();
        assert_eq!(
            result.statements[0].rows,
            vec![vec![PgCoreValue::Text("41".to_owned())]]
        );

        let result = statement.execute_with_params(&[PgCoreParam::Null]).unwrap();
        assert_eq!(result.statements[0].rows, vec![vec![PgCoreValue::Null]]);
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn execute_parameter_count_mismatch_is_protocol_error() {
        let session = PgCoreSession::new();
        let statement = session.prepare("select $1::int4").unwrap();

        let error = statement.execute().unwrap_err();
        assert_eq!(error.sqlstate, "08P01");
        assert!(error.message.contains("expected 1 parameters but got 0"));
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn execute_pg_class_relkind_by_regclass_param() {
        let session = PgCoreSession::new();
        let table = format!("fastpg_pgcore_relkind_{}", std::process::id());
        session
            .prepare(&format!("create table {table}(id int not null)"))
            .unwrap()
            .execute()
            .unwrap();

        let statement = session
            .prepare("select relkind from pg_catalog.pg_class where oid=$1::pg_catalog.regclass")
            .unwrap();
        let description = statement.describe();
        assert_eq!(description.parameter_type_oids, vec![REGCLASS_OID]);

        let result = statement
            .execute_with_params(&[PgCoreParam::Text(table.clone())])
            .unwrap();
        assert_eq!(
            result.statements[0].rows,
            vec![vec![PgCoreValue::Text("r".to_owned())]]
        );

        session
            .prepare(&format!("drop table if exists {table}"))
            .unwrap()
            .execute()
            .unwrap();
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn execute_insert_and_count_user_relation() {
        let session = PgCoreSession::new();
        let table = format!("fastpg_pgcore_count_{}", std::process::id());
        session
            .prepare(&format!("create table {table}(id int not null)"))
            .unwrap()
            .execute()
            .unwrap();

        let insert = session
            .prepare(&format!("insert into {table} values (1), (2)"))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(insert.statements[0].command_tag, "INSERT");

        let count = session
            .prepare(&format!("select count(*) from {table}"))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(
            count.statements[0].rows,
            vec![vec![PgCoreValue::Text("2".to_owned())]]
        );

        session
            .prepare(&format!("drop table if exists {table}"))
            .unwrap()
            .execute()
            .unwrap();
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn execute_current_timestamp_assignment_to_timestamp_column() {
        let session = PgCoreSession::new();
        let table = format!("fastpg_pgcore_mtime_{}", std::process::id());
        session
            .prepare(&format!(
                "create table {table}(id int not null, mtime timestamp)"
            ))
            .unwrap()
            .execute()
            .unwrap();

        session
            .prepare(&format!(
                "insert into {table} values (1, current_timestamp)"
            ))
            .unwrap()
            .execute()
            .unwrap();

        let update = session
            .prepare(&format!(
                "update {table} set mtime = current_timestamp where id = 1"
            ))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(update.statements[0].command_tag, "UPDATE");

        session
            .prepare(&format!("drop table if exists {table}"))
            .unwrap()
            .execute()
            .unwrap();
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn execute_create_truncate_drop_table_utilities() {
        let session = PgCoreSession::new();
        let suffix = std::process::id();
        let table = format!("fastpg_pgcore_util_{suffix}");

        let create = session
            .prepare(&format!(
                "create table {table}(id int not null, filler char(8), mtime timestamp)"
            ))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(create.statements[0].command_tag, "CREATE TABLE");

        let truncate = session
            .prepare(&format!("truncate {table}"))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(truncate.statements[0].command_tag, "TRUNCATE TABLE");

        let drop = session
            .prepare(&format!("drop table if exists {table}"))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(drop.statements[0].command_tag, "DROP TABLE");
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn execute_alter_table_add_primary_key() {
        let session = PgCoreSession::new();
        let table = format!("fastpg_pgcore_pk_{}", std::process::id());
        session
            .prepare(&format!("create table {table}(id int not null)"))
            .unwrap()
            .execute()
            .unwrap();

        let result = session
            .prepare(&format!("alter table {table} add primary key (id)"))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(result.statements[0].command_tag, "ALTER TABLE");

        session
            .prepare(&format!("drop table if exists {table}"))
            .unwrap()
            .execute()
            .unwrap();
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn execute_transaction_utilities() {
        let session = PgCoreSession::new();

        let begin = session.prepare("begin").unwrap().execute().unwrap();
        assert_eq!(begin.statements[0].command_tag, "BEGIN");

        let commit = session.prepare("commit").unwrap().execute().unwrap();
        assert_eq!(commit.statements[0].command_tag, "COMMIT");
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn execute_copy_from_stdin_utility() {
        let session = PgCoreSession::new();
        let table = format!("fastpg_pgcore_copy_{}", std::process::id());
        session
            .prepare(&format!(
                "create table {table}(id int not null, filler char(8))"
            ))
            .unwrap()
            .execute()
            .unwrap();

        let copy = session
            .prepare(&format!("copy {table} from stdin with (freeze on)"))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(copy.statements[0].command_tag, "COPY");
        assert_eq!(
            copy.statements[0].copy_in,
            Some(PgCoreCopyIn {
                table: table.clone(),
                columns: 2
            })
        );

        session
            .prepare(&format!("drop table if exists {table}"))
            .unwrap()
            .execute()
            .unwrap();
    }
}
