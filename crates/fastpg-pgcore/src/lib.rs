#![deny(unsafe_op_in_unsafe_fn)]

use std::borrow::Cow;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawParseSummary {
    pub statement_count: usize,
}

pub const INT8_OID: u32 = 20;
pub const INT2_OID: u32 = 21;
pub const INT4_OID: u32 = 23;
pub const TEXT_OID: u32 = 25;
pub const VARCHAR_OID: u32 = 1043;
pub const REGCLASS_OID: u32 = 2205;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PgCoreError {
    pub sqlstate: String,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    pub cursorpos: i32,
}

impl PgCoreError {
    pub fn new(sqlstate: impl Into<String>, message: impl Into<String>, cursorpos: i32) -> Self {
        Self {
            sqlstate: sqlstate.into(),
            message: message.into(),
            detail: None,
            hint: None,
            cursorpos,
        }
    }

    pub fn with_fields(
        sqlstate: impl Into<String>,
        message: impl Into<String>,
        detail: Option<String>,
        hint: Option<String>,
        cursorpos: i32,
    ) -> Self {
        Self {
            sqlstate: sqlstate.into(),
            message: message.into(),
            detail,
            hint,
            cursorpos,
        }
    }
}

pub fn raw_parse(sql: &str) -> Result<RawParseSummary, PgCoreError> {
    inner::raw_parse(sql)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PgCoreLaneMetrics {
    pub operations: u64,
    pub active: u64,
    pub max_active: u64,
    pub wait_nanos: u64,
    pub execution_nanos: u64,
}

pub fn pgcore_lane_metrics() -> PgCoreLaneMetrics {
    inner::pgcore_lane_metrics()
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
    Datum(usize),
    Null,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PgCoreCopyIn {
    pub table: String,
    pub columns: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionStatement {
    pub command_tag: Cow<'static, str>,
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
    storage_session: fastpg_storage::SessionStorageHandle,
}

impl PgCoreSession {
    pub fn new() -> Self {
        Self::with_storage_session(fastpg_storage::new_session_storage())
    }

    pub fn with_storage_session(storage_session: fastpg_storage::SessionStorageHandle) -> Self {
        Self {
            inner: inner::PgCoreSession::new(),
            storage_session,
        }
    }

    pub fn prepare(&self, sql: &str) -> Result<PreparedStatement, PgCoreError> {
        let _guard = fastpg_storage::enter_session_storage(self.storage_session.clone());
        self.inner.prepare(sql).map(|inner| PreparedStatement {
            inner,
            storage_session: self.storage_session.clone(),
        })
    }

    pub fn execute_with_params(
        &self,
        sql: &str,
        params: &[PgCoreParam],
    ) -> Result<ExecutionResult, PgCoreError> {
        let _guard = fastpg_storage::enter_session_storage(self.storage_session.clone());
        let statement = self.inner.prepare(sql)?;
        statement.execute_with_params(params)
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
    storage_session: fastpg_storage::SessionStorageHandle,
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
        let _guard = fastpg_storage::enter_session_storage(self.storage_session.clone());
        self.inner.execute()
    }

    pub fn execute_with_params(
        &self,
        params: &[PgCoreParam],
    ) -> Result<ExecutionResult, PgCoreError> {
        let _guard = fastpg_storage::enter_session_storage(self.storage_session.clone());
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
    use std::borrow::Cow;
    use std::ffi::{CStr, CString, c_char};
    use std::ptr;
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, MutexGuard};
    use std::time::Instant;

    use super::{
        ExecutionResult, ExecutionStatement, PgCoreCopyIn, PgCoreError, PgCoreField,
        PgCoreLaneMetrics, PgCoreParam, PgCoreValue, RawParseSummary, StatementDescription,
    };

    static PGCORE_LOCK: Mutex<()> = Mutex::new(());
    static PGCORE_OPERATIONS: AtomicU64 = AtomicU64::new(0);
    static PGCORE_ACTIVE: AtomicU64 = AtomicU64::new(0);
    static PGCORE_MAX_ACTIVE: AtomicU64 = AtomicU64::new(0);
    static PGCORE_WAIT_NANOS: AtomicU64 = AtomicU64::new(0);
    static PGCORE_EXECUTION_NANOS: AtomicU64 = AtomicU64::new(0);
    static PGCORE_CATALOG_GENERATION: AtomicU64 = AtomicU64::new(0);
    const STACK_SQL_BUFFER_LEN: usize = 1024;

    pub fn pgcore_lane_metrics() -> PgCoreLaneMetrics {
        PgCoreLaneMetrics {
            operations: PGCORE_OPERATIONS.load(Ordering::Relaxed),
            active: PGCORE_ACTIVE.load(Ordering::Relaxed),
            max_active: PGCORE_MAX_ACTIVE.load(Ordering::Relaxed),
            wait_nanos: PGCORE_WAIT_NANOS.load(Ordering::Relaxed),
            execution_nanos: PGCORE_EXECUTION_NANOS.load(Ordering::Relaxed),
        }
    }

    struct PgCoreLaneGuard {
        _guard: MutexGuard<'static, ()>,
        started_at: Instant,
    }

    fn enter_pgcore_lane(operation: &'static str) -> PgCoreLaneGuard {
        let waiting_since = Instant::now();
        let guard = PGCORE_LOCK
            .lock()
            .unwrap_or_else(|_| panic!("fastpg pgcore {operation} mutex poisoned"));
        let started_at = Instant::now();
        add_duration(
            &PGCORE_WAIT_NANOS,
            started_at.duration_since(waiting_since).as_nanos(),
        );
        let active = PGCORE_ACTIVE.fetch_add(1, Ordering::SeqCst) + 1;
        PGCORE_OPERATIONS.fetch_add(1, Ordering::Relaxed);
        update_max_active(active);
        refresh_pgcore_caches_if_catalog_changed();
        PgCoreLaneGuard {
            _guard: guard,
            started_at,
        }
    }

    impl Drop for PgCoreLaneGuard {
        fn drop(&mut self) {
            add_duration(
                &PGCORE_EXECUTION_NANOS,
                self.started_at.elapsed().as_nanos(),
            );
            PGCORE_ACTIVE.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn add_duration(counter: &AtomicU64, nanos: u128) {
        let addition = u64::try_from(nanos).unwrap_or(u64::MAX);
        let mut current = counter.load(Ordering::Relaxed);
        loop {
            let next = current.saturating_add(addition);
            match counter.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    fn update_max_active(active: u64) {
        let mut current = PGCORE_MAX_ACTIVE.load(Ordering::Relaxed);
        while active > current {
            match PGCORE_MAX_ACTIVE.compare_exchange_weak(
                current,
                active,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    fn refresh_pgcore_caches_if_catalog_changed() {
        let current_generation = fastpg_catalog::current_generation();
        if PGCORE_CATALOG_GENERATION.load(Ordering::Relaxed) == current_generation {
            return;
        }
        unsafe {
            fastpg_pgcore_invalidate_system_caches();
        }
        PGCORE_CATALOG_GENERATION.store(current_generation, Ordering::Relaxed);
    }

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
        fn fastpg_pgcore_invalidate_system_caches();
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
        fn fastpg_pgcore_parse_result_detail(
            result: *const FastPgPgCoreParseResult,
        ) -> *const c_char;
        fn fastpg_pgcore_parse_result_hint(result: *const FastPgPgCoreParseResult)
        -> *const c_char;
        fn fastpg_pgcore_parse_result_cursorpos(result: *const FastPgPgCoreParseResult) -> i32;
        fn fastpg_pgcore_prepare(sql: *const c_char) -> *mut FastPgPgCorePrepared;
        fn fastpg_pgcore_prepared_free(prepared: *mut FastPgPgCorePrepared);
        fn fastpg_pgcore_prepared_ok(prepared: *const FastPgPgCorePrepared) -> bool;
        fn fastpg_pgcore_prepared_sqlstate(prepared: *const FastPgPgCorePrepared) -> *const c_char;
        fn fastpg_pgcore_prepared_message(prepared: *const FastPgPgCorePrepared) -> *const c_char;
        fn fastpg_pgcore_prepared_detail(prepared: *const FastPgPgCorePrepared) -> *const c_char;
        fn fastpg_pgcore_prepared_hint(prepared: *const FastPgPgCorePrepared) -> *const c_char;
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
            parameter_datums: *const usize,
            parameter_is_datum: *const bool,
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
        fn fastpg_pgcore_execute_result_detail(
            result: *const FastPgPgCoreExecuteResult,
        ) -> *const c_char;
        fn fastpg_pgcore_execute_result_hint(
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

    fn check_sql(sql: &str) -> Result<(), PgCoreError> {
        if sql.as_bytes().contains(&0) {
            return Err(PgCoreError::new(
                "22023",
                "query contains an embedded NUL byte",
                0,
            ));
        }
        Ok(())
    }

    fn with_c_sql<R>(sql: &str, f: impl FnOnce(*const c_char) -> R) -> R {
        let bytes = sql.as_bytes();
        if bytes.len() < STACK_SQL_BUFFER_LEN {
            let mut buffer = [0u8; STACK_SQL_BUFFER_LEN];
            buffer[..bytes.len()].copy_from_slice(bytes);
            return f(buffer.as_ptr().cast());
        }
        let c_sql = CString::new(sql).expect("checked SQL string does not contain embedded NULs");
        f(c_sql.as_ptr())
    }

    pub fn raw_parse(sql: &str) -> Result<RawParseSummary, PgCoreError> {
        check_sql(sql)?;
        let _guard = enter_pgcore_lane("raw_parse");

        let result = with_c_sql(sql, |c_sql| unsafe { fastpg_pgcore_raw_parse(c_sql) });
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
            Err(PgCoreError::with_fields(
                unsafe { c_string(fastpg_pgcore_parse_result_sqlstate(result.as_ptr())) },
                unsafe { c_string(fastpg_pgcore_parse_result_message(result.as_ptr())) },
                unsafe { optional_c_string(fastpg_pgcore_parse_result_detail(result.as_ptr())) },
                unsafe { optional_c_string(fastpg_pgcore_parse_result_hint(result.as_ptr())) },
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

    unsafe fn optional_c_string(value: *const c_char) -> Option<String> {
        if value.is_null() {
            return None;
        }
        let value = unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .into_owned();
        if value.is_empty() { None } else { Some(value) }
    }

    unsafe fn command_tag(value: *const c_char) -> Cow<'static, str> {
        if value.is_null() {
            return Cow::Borrowed("");
        }

        let value = unsafe { CStr::from_ptr(value) };
        match value.to_bytes() {
            b"" => Cow::Borrowed(""),
            b"BEGIN" => Cow::Borrowed("BEGIN"),
            b"COMMIT" => Cow::Borrowed("COMMIT"),
            b"COPY" => Cow::Borrowed("COPY"),
            b"CREATE TABLE" => Cow::Borrowed("CREATE TABLE"),
            b"DELETE" => Cow::Borrowed("DELETE"),
            b"DROP TABLE" => Cow::Borrowed("DROP TABLE"),
            b"INSERT" => Cow::Borrowed("INSERT"),
            b"MERGE" => Cow::Borrowed("MERGE"),
            b"RELEASE" => Cow::Borrowed("RELEASE"),
            b"ROLLBACK" => Cow::Borrowed("ROLLBACK"),
            b"SAVEPOINT" => Cow::Borrowed("SAVEPOINT"),
            b"SELECT" => Cow::Borrowed("SELECT"),
            b"TRUNCATE TABLE" => Cow::Borrowed("TRUNCATE TABLE"),
            b"UPDATE" => Cow::Borrowed("UPDATE"),
            _ => Cow::Owned(value.to_string_lossy().into_owned()),
        }
    }

    #[derive(Clone, Debug)]
    pub struct PgCoreSession;

    impl PgCoreSession {
        pub fn new() -> Self {
            let _ = fastpg_storage::fastpg_rust_relation_row_count(0);
            Self
        }

        pub fn prepare(&self, sql: &str) -> Result<PreparedStatement, PgCoreError> {
            check_sql(sql)?;
            let _guard = enter_pgcore_lane("prepare");
            let prepared = with_c_sql(sql, |c_sql| unsafe { fastpg_pgcore_prepare(c_sql) });
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
                let error = PgCoreError::with_fields(
                    unsafe { c_string(fastpg_pgcore_prepared_sqlstate(prepared.as_ptr())) },
                    unsafe { c_string(fastpg_pgcore_prepared_message(prepared.as_ptr())) },
                    unsafe { optional_c_string(fastpg_pgcore_prepared_detail(prepared.as_ptr())) },
                    unsafe { optional_c_string(fastpg_pgcore_prepared_hint(prepared.as_ptr())) },
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
            let _guard = enter_pgcore_lane("describe");
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
            if params.is_empty() {
                let _guard = enter_pgcore_lane("execute");
                let result = unsafe { fastpg_pgcore_execute(self.as_ptr()) };
                return execution_result_from_ptr(result);
            }

            let param_count = i32::try_from(params.len()).map_err(|_| {
                PgCoreError::new("54000", "too many parameters for PostgreSQL execution", 0)
            })?;
            let mut encoded_text_params = Vec::with_capacity(params.len());
            let mut parameter_values = Vec::with_capacity(params.len());
            let mut parameter_is_null = Vec::with_capacity(params.len());
            let mut parameter_datums = Vec::with_capacity(params.len());
            let mut parameter_is_datum = Vec::with_capacity(params.len());
            for param in params {
                match param {
                    PgCoreParam::Text(value) => {
                        let value = CString::new(value.as_str()).map_err(|_| {
                            PgCoreError::new(
                                "22023",
                                "query parameter contains an embedded NUL byte",
                                0,
                            )
                        })?;
                        parameter_values.push(value.as_ptr());
                        parameter_is_null.push(false);
                        parameter_datums.push(0);
                        parameter_is_datum.push(false);
                        encoded_text_params.push(value);
                    }
                    PgCoreParam::Datum(value) => {
                        parameter_values.push(ptr::null());
                        parameter_is_null.push(false);
                        parameter_datums.push(*value);
                        parameter_is_datum.push(true);
                    }
                    PgCoreParam::Null => {
                        parameter_values.push(ptr::null());
                        parameter_is_null.push(true);
                        parameter_datums.push(0);
                        parameter_is_datum.push(false);
                    }
                }
            }

            let _guard = enter_pgcore_lane("execute");
            let result = unsafe {
                fastpg_pgcore_execute_params(
                    self.as_ptr(),
                    parameter_values.as_ptr(),
                    parameter_is_null.as_ptr(),
                    parameter_datums.as_ptr(),
                    parameter_is_datum.as_ptr(),
                    param_count,
                )
            };
            execution_result_from_ptr(result)
        }
    }

    fn execution_result_from_ptr(
        result: *mut FastPgPgCoreExecuteResult,
    ) -> Result<ExecutionResult, PgCoreError> {
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
            Err(PgCoreError::with_fields(
                unsafe { c_string(fastpg_pgcore_execute_result_sqlstate(result.as_ptr())) },
                unsafe { c_string(fastpg_pgcore_execute_result_message(result.as_ptr())) },
                unsafe { optional_c_string(fastpg_pgcore_execute_result_detail(result.as_ptr())) },
                unsafe { optional_c_string(fastpg_pgcore_execute_result_hint(result.as_ptr())) },
                unsafe { fastpg_pgcore_execute_result_cursorpos(result.as_ptr()) },
            ))
        }
    }

    impl Drop for PreparedStatement {
        fn drop(&mut self) {
            let _guard = enter_pgcore_lane("prepared_free");
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
                            command_tag(fastpg_pgcore_execute_statement_command_tag(
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
    use super::{
        ExecutionResult, PgCoreError, PgCoreLaneMetrics, RawParseSummary, StatementDescription,
    };

    pub fn pgcore_lane_metrics() -> PgCoreLaneMetrics {
        PgCoreLaneMetrics::default()
    }

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
    fn execute_comment_only_query_is_empty() {
        let session = PgCoreSession::new();
        let statement = session
            .prepare("/* comment-only query should be an empty query */")
            .unwrap();
        let result = statement.execute().unwrap();
        assert!(result.statements.is_empty());
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
    fn execute_create_table_uses_generated_catalog_types() {
        let session = PgCoreSession::new();
        let table = format!("fastpg_pgcore_generated_types_{}", std::process::id());

        let create = session
            .prepare(&format!(
                "create table {table}(value float8, location point, route path)"
            ))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(create.statements[0].command_tag, "CREATE TABLE");

        session
            .prepare(&format!("drop table if exists {table}"))
            .unwrap()
            .execute()
            .unwrap();
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn execute_uuid_serial_generated_column_table() {
        let session = PgCoreSession::new();
        let table = format!("fastpg_pgcore_uuid_generated_{}", std::process::id());

        let create = session
            .prepare(&format!(
                "create table {table}(
                    id serial,
                    guid_field uuid,
                    guid_encoded text generated always as (encode(guid_field::bytea, 'base32hex')) stored
                )"
            ))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(create.statements[0].command_tag, "CREATE TABLE");

        session
            .prepare(&format!(
                "insert into {table}(guid_field)
                 values ('00000000-0000-0000-0000-000000000000'::uuid)"
            ))
            .unwrap()
            .execute()
            .unwrap();

        let select = session
            .prepare(&format!("select id, guid_encoded from {table}"))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(
            select.statements[0].rows[0][0],
            PgCoreValue::Text("1".to_owned())
        );
        assert_eq!(
            select.statements[0].rows[0][1],
            PgCoreValue::Text("00000000000000000000000000======".to_owned())
        );

        session
            .prepare(&format!("drop table if exists {table} cascade"))
            .unwrap()
            .execute()
            .unwrap();
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn repeated_executor_errors_keep_pgcore_session_alive() {
        let session = PgCoreSession::new();

        let first = session
            .prepare("select format('Hello %s %s', 'World')")
            .unwrap()
            .execute()
            .unwrap_err();
        assert_eq!(first.sqlstate, "22023");

        let second = session
            .prepare("select format('Hello %s')")
            .unwrap()
            .execute()
            .unwrap_err();
        assert_eq!(second.sqlstate, "22023");

        let ok = session.prepare("select 1").unwrap().execute().unwrap();
        assert_eq!(
            ok.statements[0].rows[0][0],
            PgCoreValue::Text("1".to_owned())
        );
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn execute_catalog_visible_type_function_and_opclass_ddl() {
        let session = PgCoreSession::new();
        let suffix = std::process::id();
        let enum_type = format!("fastpg_pgcore_enum_{suffix}");
        let range_type = format!("fastpg_pgcore_range_{suffix}");
        let composite_type = format!("fastpg_pgcore_composite_{suffix}");
        let function = format!("fastpg_pgcore_hash_{suffix}");
        let opclass = format!("fastpg_pgcore_int4_ops_{suffix}");

        let create_enum = session
            .prepare(&format!("create type {enum_type} as enum ('red', 'green')"))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(create_enum.statements[0].command_tag, "CREATE TYPE");

        let create_range = session
            .prepare(&format!(
                "create type {range_type} as range (subtype = float8, subtype_diff = float8mi)"
            ))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(create_range.statements[0].command_tag, "CREATE TYPE");

        let create_composite = session
            .prepare(&format!(
                "create type {composite_type} as (id int, label text)"
            ))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(create_composite.statements[0].command_tag, "CREATE TYPE");

        let create_function = session
            .prepare(&format!(
                "create function {function}(value int4, seed int8) returns int8 \
                 as $$ select value + seed $$ language sql strict immutable parallel safe"
            ))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(create_function.statements[0].command_tag, "CREATE FUNCTION");

        let create_opclass = session
            .prepare(&format!(
                "create operator class {opclass} for type int4 using hash as \
                 operator 1 =, function 2 {function}(int4, int8)"
            ))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(
            create_opclass.statements[0].command_tag,
            "CREATE OPERATOR CLASS"
        );

        let type_lookup = session
            .prepare(&format!(
                "select typname from pg_type where typname = '{enum_type}'"
            ))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(
            type_lookup.statements[0].rows,
            vec![vec![PgCoreValue::Text(enum_type)]]
        );

        let proc_lookup = session
            .prepare(&format!(
                "select proname from pg_proc where proname = '{function}'"
            ))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(
            proc_lookup.statements[0].rows,
            vec![vec![PgCoreValue::Text(function)]]
        );

        let opclass_lookup = session
            .prepare(&format!(
                "select opcname from pg_opclass where opcname = '{opclass}'"
            ))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(
            opclass_lookup.statements[0].rows,
            vec![vec![PgCoreValue::Text(opclass)]]
        );
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

        let unique = session
            .prepare(&format!("alter table {table} add unique (id)"))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(unique.statements[0].command_tag, "ALTER TABLE");

        session
            .prepare(&format!("drop table if exists {table}"))
            .unwrap()
            .execute()
            .unwrap();
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn execute_regress_compatibility_noop_utilities() {
        let session = PgCoreSession::new();
        let table = format!("fastpg_pgcore_noop_{}", std::process::id());

        let set = session
            .prepare("set synchronous_commit = on")
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(set.statements[0].command_tag, "SET");

        let grant = session
            .prepare("grant all on schema public to public")
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(grant.statements[0].command_tag, "GRANT");

        let tablespace = session
            .prepare("create tablespace fastpg_regress_tblspace location ''")
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(tablespace.statements[0].command_tag, "CREATE TABLESPACE");

        session
            .prepare(&format!("create table {table}(id int not null)"))
            .unwrap()
            .execute()
            .unwrap();
        let comment = session
            .prepare(&format!("comment on table {table} is 'regress shim'"))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(comment.statements[0].command_tag, "COMMENT");

        session
            .prepare(&format!("drop table if exists {table}"))
            .unwrap()
            .execute()
            .unwrap();
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn execute_system_catalog_write_is_noop() {
        let session = PgCoreSession::new();
        let table = format!("fastpg_pgcore_catalog_noop_{}", std::process::id());

        session
            .prepare(&format!("create table {table}(id int not null)"))
            .unwrap()
            .execute()
            .unwrap();

        let update = session
            .prepare(&format!(
                "update pg_class set reloptions = '{{fillfactor=13}}' where oid = '{table}'::regclass"
            ))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(update.statements[0].command_tag, "UPDATE");
        assert!(update.statements[0].rows.is_empty());

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
    fn autocommit_rows_survive_explicit_rollback() {
        let session = PgCoreSession::new();
        let table = format!("fastpg_pgcore_rollback_{}", std::process::id());
        session
            .prepare(&format!(
                "create table {table}(id int not null, amount int)"
            ))
            .unwrap()
            .execute()
            .unwrap();
        session
            .prepare(&format!("insert into {table} values (1, 10)"))
            .unwrap()
            .execute()
            .unwrap();
        session.prepare("begin").unwrap().execute().unwrap();
        session
            .prepare(&format!("update {table} set amount = 20 where id = 1"))
            .unwrap()
            .execute()
            .unwrap();
        session
            .prepare(&format!("insert into {table} values (2, 30)"))
            .unwrap()
            .execute()
            .unwrap();
        session.prepare("rollback").unwrap().execute().unwrap();

        let result = session
            .prepare(&format!("select id, amount from {table}"))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(
            result.statements[0].rows,
            vec![vec![
                PgCoreValue::Text("1".to_owned()),
                PgCoreValue::Text("10".to_owned())
            ]]
        );

        session
            .prepare(&format!("drop table if exists {table}"))
            .unwrap()
            .execute()
            .unwrap();
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
