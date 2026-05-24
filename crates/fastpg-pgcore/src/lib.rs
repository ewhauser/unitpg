#![deny(unsafe_op_in_unsafe_fn)]

use std::borrow::Cow;
#[cfg(feature = "postgres-execution")]
use std::ffi::CStr;
use std::ops::{Deref, DerefMut};
use std::sync::OnceLock;

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
pub struct PgCoreNotice {
    pub severity: String,
    pub sqlstate: String,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    pub context: Option<String>,
    pub cursorpos: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PgCoreError {
    inner: Box<PgCoreErrorInfo>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PgCoreErrorInfo {
    pub sqlstate: String,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    pub context: Option<String>,
    pub cursorpos: i32,
    pub internal_query: Option<String>,
    pub internalpos: i32,
    pub notices: Vec<PgCoreNotice>,
    pub partial: Vec<ExecutionStatement>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PgCoreErrorFields {
    pub detail: Option<String>,
    pub hint: Option<String>,
    pub context: Option<String>,
    pub cursorpos: i32,
    pub internal_query: Option<String>,
    pub internalpos: i32,
    pub notices: Vec<PgCoreNotice>,
    pub partial: Vec<ExecutionStatement>,
}

impl Deref for PgCoreError {
    type Target = PgCoreErrorInfo;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for PgCoreError {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl PgCoreError {
    pub fn new(sqlstate: impl Into<String>, message: impl Into<String>, cursorpos: i32) -> Self {
        Self::with_fields(
            sqlstate,
            message,
            PgCoreErrorFields {
                cursorpos,
                ..Default::default()
            },
        )
    }

    pub fn with_fields(
        sqlstate: impl Into<String>,
        message: impl Into<String>,
        fields: PgCoreErrorFields,
    ) -> Self {
        let PgCoreErrorFields {
            detail,
            hint,
            context,
            cursorpos,
            internal_query,
            internalpos,
            notices,
            partial,
        } = fields;

        Self {
            inner: Box::new(PgCoreErrorInfo {
                sqlstate: sqlstate.into(),
                message: message.into(),
                detail,
                hint,
                context,
                cursorpos,
                internal_query,
                internalpos,
                notices,
                partial,
            }),
        }
    }

    pub fn into_fields(self) -> PgCoreErrorInfo {
        *self.inner
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
    pub type_modifier: i32,
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
pub struct PgCoreInputDatum {
    pub value: usize,
    pub typbyval: bool,
    pub typlen: i16,
    pub payload: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PgCoreCopyIn {
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
    pub column_metadata: Vec<PgCoreCopyColumn>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PgCoreCopyColumn {
    pub name: String,
    pub attnum: i16,
    pub type_oid: u32,
    pub type_modifier: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PgCoreCopyOut {
    pub format: i8,
    pub columns: usize,
    pub chunks: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionStatement {
    pub command_tag: Cow<'static, str>,
    pub command_rows: Option<usize>,
    pub is_select: bool,
    pub fields: Vec<PgCoreField>,
    pub rows: Vec<Vec<PgCoreValue>>,
    pub copy_in: Option<PgCoreCopyIn>,
    pub copy_out: Option<PgCoreCopyOut>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionResult {
    pub notices: Vec<PgCoreNotice>,
    pub statements: Vec<ExecutionStatement>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SimpleExecutionResult {
    Command {
        notices: Vec<PgCoreNotice>,
        tag: Cow<'static, str>,
        rows: Option<usize>,
    },
    Full(ExecutionResult),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PgCoreTransactionCommand {
    Begin,
    Commit,
    Rollback,
}

impl PgCoreTransactionCommand {
    #[cfg_attr(not(feature = "postgres-execution"), allow(dead_code))]
    fn command_tag(self) -> &'static str {
        match self {
            Self::Begin => "BEGIN",
            Self::Commit => "COMMIT",
            Self::Rollback => "ROLLBACK",
        }
    }

    #[cfg(feature = "postgres-execution")]
    fn pgcore_code(self) -> i32 {
        match self {
            Self::Begin => 0,
            Self::Commit => 1,
            Self::Rollback => 2,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PgCoreSession {
    inner: inner::PgCoreSession,
    storage_session: fastpg_storage::SessionStorageHandle,
    storage2_session: fastpg_storage2::SessionStorageHandle,
}

impl PgCoreSession {
    pub fn new() -> Self {
        Self::with_storage_session(fastpg_storage::new_session_storage())
    }

    pub fn with_database(database: impl Into<String>) -> Self {
        Self::with_storage_sessions_and_database(
            fastpg_storage::new_session_storage(),
            fastpg_storage2::new_session_storage(),
            database,
        )
    }

    pub fn with_storage_session(storage_session: fastpg_storage::SessionStorageHandle) -> Self {
        Self::with_storage_sessions(storage_session, fastpg_storage2::new_session_storage())
    }

    pub fn with_storage_sessions(
        storage_session: fastpg_storage::SessionStorageHandle,
        storage2_session: fastpg_storage2::SessionStorageHandle,
    ) -> Self {
        Self::with_storage_sessions_and_database(storage_session, storage2_session, "postgres")
    }

    pub fn with_storage_sessions_and_database(
        storage_session: fastpg_storage::SessionStorageHandle,
        storage2_session: fastpg_storage2::SessionStorageHandle,
        database: impl Into<String>,
    ) -> Self {
        let database = database.into();
        let database_oid = if inner::postgres_catalog_enabled() {
            if !database.eq_ignore_ascii_case("postgres") {
                panic!(
                    "Postgres catalog mode only supports database \"postgres\" in this experiment"
                );
            }
            5
        } else {
            fastpg_catalog::ensure_database(&database).0
        };
        Self {
            inner: inner::PgCoreSession::new(database_oid),
            storage_session,
            storage2_session,
        }
    }

    fn enter_storage(&self) -> PgCoreStorageGuards<'_> {
        PgCoreStorageGuards {
            _storage1: if storage2_enabled() {
                None
            } else {
                Some(fastpg_storage::enter_session_storage(
                    self.storage_session.clone(),
                ))
            },
            _storage2: fastpg_storage2::enter_locked_session_storage(&self.storage2_session),
        }
    }

    pub fn prepare(&self, sql: &str) -> Result<PreparedStatement, PgCoreError> {
        let _guard = self.enter_storage();
        self.inner.prepare(sql).map(|inner| PreparedStatement {
            inner,
            storage_session: self.storage_session.clone(),
            storage2_session: self.storage2_session.clone(),
        })
    }

    pub fn execute_simple(&self, sql: &str) -> Result<ExecutionResult, PgCoreError> {
        let _guard = self.enter_storage();
        self.inner.execute_simple(sql)
    }

    #[cfg(feature = "postgres-execution")]
    pub fn execute_simple_cstr(&self, sql: &CStr) -> Result<ExecutionResult, PgCoreError> {
        let _guard = self.enter_storage();
        self.inner.execute_simple_cstr(sql)
    }

    #[cfg(feature = "postgres-execution")]
    pub fn execute_simple_cstr_fast(
        &self,
        sql: &CStr,
    ) -> Result<SimpleExecutionResult, PgCoreError> {
        let _guard = self.enter_storage();
        self.inner.execute_simple_cstr_fast(sql)
    }

    #[cfg(feature = "postgres-execution")]
    pub fn execute_transaction_command(
        &self,
        command: PgCoreTransactionCommand,
    ) -> Result<ExecutionResult, PgCoreError> {
        let _guard = self.enter_storage();
        self.inner.execute_transaction_command(command)
    }

    pub fn execute_copy_from_stdin(
        &self,
        sql: &str,
        data: &[u8],
    ) -> Result<ExecutionResult, PgCoreError> {
        let _guard = self.enter_storage();
        self.inner.execute_copy_from_stdin(sql, data)
    }

    pub fn input_text_datum(
        &self,
        type_oid: u32,
        typmod: i32,
        value: &str,
    ) -> Result<PgCoreInputDatum, PgCoreError> {
        let _guard = self.enter_storage();
        self.inner.input_text_datum(type_oid, typmod, value)
    }

    pub fn reset_session_state(&self) {
        let _guard = self.enter_storage();
        self.inner.reset_session_state();
    }

    pub fn start_client_session(&self) -> Vec<PgCoreNotice> {
        let _guard = self.enter_storage();
        self.inner.start_client_session()
    }

    pub fn end_client_session(&self) {
        let _guard = self.enter_storage();
        self.inner.end_client_session();
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
    storage2_session: fastpg_storage2::SessionStorageHandle,
}

// pgcore-backed execution is intentionally allowed to overlap across client
// tasks so the Rust server's concurrency tests exercise the real concurrent
// path instead of a single global lane.
unsafe impl Send for PreparedStatement {}
unsafe impl Sync for PreparedStatement {}

impl PreparedStatement {
    pub fn describe(&self) -> StatementDescription {
        self.inner.describe()
    }

    fn enter_storage(&self) -> PgCoreStorageGuards<'_> {
        PgCoreStorageGuards {
            _storage1: if storage2_enabled() {
                None
            } else {
                Some(fastpg_storage::enter_session_storage(
                    self.storage_session.clone(),
                ))
            },
            _storage2: fastpg_storage2::enter_locked_session_storage(&self.storage2_session),
        }
    }

    pub fn execute(&self) -> Result<ExecutionResult, PgCoreError> {
        let _guard = self.enter_storage();
        self.inner.execute()
    }

    pub fn execute_with_params(
        &self,
        params: &[PgCoreParam],
    ) -> Result<ExecutionResult, PgCoreError> {
        let _guard = self.enter_storage();
        self.inner.execute_with_params(params)
    }
}

struct PgCoreStorageGuards<'a> {
    _storage1: Option<fastpg_storage::SessionStorageGuard>,
    _storage2: fastpg_storage2::LockedSessionStorageGuard<'a>,
}

fn storage2_enabled() -> bool {
    static STORAGE2_ENABLED: OnceLock<bool> = OnceLock::new();

    *STORAGE2_ENABLED.get_or_init(|| {
        std::env::var("FASTPG_STORAGE_ENGINE")
            .map(|value| value.eq_ignore_ascii_case("storage2"))
            .unwrap_or(false)
    })
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

#[cfg(feature = "postgres-execution")]
mod inner {
    use std::borrow::Cow;
    use std::cell::Cell;
    use std::ffi::{CStr, CString, c_char};
    use std::mem::MaybeUninit;
    use std::ptr;
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        ExecutionResult, ExecutionStatement, PgCoreCopyColumn, PgCoreCopyIn, PgCoreCopyOut,
        PgCoreError, PgCoreErrorFields, PgCoreField, PgCoreInputDatum, PgCoreLaneMetrics,
        PgCoreNotice, PgCoreParam, PgCoreTransactionCommand, PgCoreValue, RawParseSummary,
        SimpleExecutionResult, StatementDescription,
    };

    static PGCORE_OPERATIONS: AtomicU64 = AtomicU64::new(0);
    static PGCORE_ACTIVE: AtomicU64 = AtomicU64::new(0);
    static PGCORE_MAX_ACTIVE: AtomicU64 = AtomicU64::new(0);
    static PGCORE_EXECUTION_NANOS: AtomicU64 = AtomicU64::new(0);
    static PGCORE_CATALOG_GENERATION: AtomicU64 = AtomicU64::new(0);
    const STACK_SQL_BUFFER_LEN: usize = 1024;

    thread_local! {
        static CURRENT_DATABASE_OID: Cell<u32> = const { Cell::new(0) };
    }

    pub fn pgcore_lane_metrics() -> PgCoreLaneMetrics {
        PgCoreLaneMetrics {
            operations: PGCORE_OPERATIONS.load(Ordering::Relaxed),
            active: PGCORE_ACTIVE.load(Ordering::Relaxed),
            max_active: PGCORE_MAX_ACTIVE.load(Ordering::Relaxed),
            wait_nanos: 0,
            execution_nanos: PGCORE_EXECUTION_NANOS.load(Ordering::Relaxed),
        }
    }

    struct PgCoreLaneGuard;

    fn enter_pgcore_lane(_operation: &'static str) -> PgCoreLaneGuard {
        let active = PGCORE_ACTIVE.fetch_add(1, Ordering::Relaxed) + 1;
        PGCORE_OPERATIONS.fetch_add(1, Ordering::Relaxed);
        update_max_active(active);
        refresh_pgcore_caches_if_catalog_changed();
        PgCoreLaneGuard
    }

    impl Drop for PgCoreLaneGuard {
        fn drop(&mut self) {
            PGCORE_ACTIVE.fetch_sub(1, Ordering::Relaxed);
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
        if postgres_catalog_enabled() {
            return;
        }
        let current_generation = fastpg_catalog::current_generation();
        if PGCORE_CATALOG_GENERATION.load(Ordering::Relaxed) == current_generation {
            return;
        }
        unsafe {
            fastpg_pgcore_invalidate_system_caches();
        }
        PGCORE_CATALOG_GENERATION.store(current_generation, Ordering::Relaxed);
    }

    fn set_pgcore_database(database_oid: u32) {
        if database_oid != 0 {
            let current = CURRENT_DATABASE_OID.with(Cell::get);
            if current == database_oid {
                return;
            }
        }

        unsafe {
            fastpg_pgcore_set_database(database_oid);
        }
        CURRENT_DATABASE_OID.with(|current| current.set(database_oid));
    }

    pub(super) fn postgres_catalog_enabled() -> bool {
        !cfg!(feature = "rust-catalog")
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

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct FastPgPgCoreExecuteStatementSummary {
        command_tag: *const c_char,
        is_select: bool,
        column_count: i32,
        row_count: i32,
        has_processed_count: bool,
        processed_count: u64,
        copy_in: bool,
        copy_out: bool,
    }

    impl Default for FastPgPgCoreExecuteStatementSummary {
        fn default() -> Self {
            Self {
                command_tag: std::ptr::null(),
                is_select: false,
                column_count: 0,
                row_count: 0,
                has_processed_count: false,
                processed_count: 0,
                copy_in: false,
                copy_out: false,
            }
        }
    }

    #[repr(C)]
    struct FastPgPgCoreInputDatumResult {
        _private: [u8; 0],
    }

    unsafe extern "C" {
        fn fastpg_pgcore_raw_parse(sql: *const c_char) -> *mut FastPgPgCoreParseResult;
        fn fastpg_xid_begin();
        fn fastpg_xid_commit();
        fn fastpg_xid_rollback();
        fn fastpg_pgcore_invalidate_system_caches();
        fn fastpg_pgcore_set_database(database_oid: u32);
        fn fastpg_pgcore_notice_capture_begin();
        fn fastpg_pgcore_notice_capture_end();
        fn fastpg_pgcore_notice_capture_clear();
        fn fastpg_pgcore_notice_capture_count() -> i32;
        fn fastpg_pgcore_notice_capture_severity(index: i32) -> *const c_char;
        fn fastpg_pgcore_notice_capture_sqlstate(index: i32) -> *const c_char;
        fn fastpg_pgcore_notice_capture_message(index: i32) -> *const c_char;
        fn fastpg_pgcore_notice_capture_detail(index: i32) -> *const c_char;
        fn fastpg_pgcore_notice_capture_hint(index: i32) -> *const c_char;
        fn fastpg_pgcore_notice_capture_context(index: i32) -> *const c_char;
        fn fastpg_pgcore_notice_capture_cursorpos(index: i32) -> i32;
        fn fastpg_pgcore_reset_session_state();
        fn fastpg_pgcore_start_client_session();
        fn fastpg_pgcore_end_client_session();
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
        fn fastpg_pgcore_parse_result_context(
            result: *const FastPgPgCoreParseResult,
        ) -> *const c_char;
        fn fastpg_pgcore_parse_result_cursorpos(result: *const FastPgPgCoreParseResult) -> i32;
        fn fastpg_pgcore_prepare(sql: *const c_char) -> *mut FastPgPgCorePrepared;
        fn fastpg_pgcore_prepared_free(prepared: *mut FastPgPgCorePrepared);
        fn fastpg_pgcore_prepared_ok(prepared: *const FastPgPgCorePrepared) -> bool;
        fn fastpg_pgcore_prepared_sqlstate(prepared: *const FastPgPgCorePrepared) -> *const c_char;
        fn fastpg_pgcore_prepared_message(prepared: *const FastPgPgCorePrepared) -> *const c_char;
        fn fastpg_pgcore_prepared_detail(prepared: *const FastPgPgCorePrepared) -> *const c_char;
        fn fastpg_pgcore_prepared_hint(prepared: *const FastPgPgCorePrepared) -> *const c_char;
        fn fastpg_pgcore_prepared_context(prepared: *const FastPgPgCorePrepared) -> *const c_char;
        fn fastpg_pgcore_prepared_cursorpos(prepared: *const FastPgPgCorePrepared) -> i32;
        fn fastpg_pgcore_prepared_internal_query(
            prepared: *const FastPgPgCorePrepared,
        ) -> *const c_char;
        fn fastpg_pgcore_prepared_internalpos(prepared: *const FastPgPgCorePrepared) -> i32;
        fn fastpg_pgcore_prepared_notice_count(prepared: *const FastPgPgCorePrepared) -> i32;
        fn fastpg_pgcore_prepared_notice_severity(
            prepared: *const FastPgPgCorePrepared,
            notice_index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_prepared_notice_sqlstate(
            prepared: *const FastPgPgCorePrepared,
            notice_index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_prepared_notice_message(
            prepared: *const FastPgPgCorePrepared,
            notice_index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_prepared_notice_detail(
            prepared: *const FastPgPgCorePrepared,
            notice_index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_prepared_notice_hint(
            prepared: *const FastPgPgCorePrepared,
            notice_index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_prepared_notice_context(
            prepared: *const FastPgPgCorePrepared,
            notice_index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_prepared_notice_cursorpos(
            prepared: *const FastPgPgCorePrepared,
            notice_index: i32,
        ) -> i32;
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
        fn fastpg_pgcore_prepared_field_type_modifier(
            prepared: *const FastPgPgCorePrepared,
            index: i32,
        ) -> i32;
        fn fastpg_pgcore_execute(
            prepared: *const FastPgPgCorePrepared,
        ) -> *mut FastPgPgCoreExecuteResult;
        fn fastpg_pgcore_execute_simple(sql: *const c_char) -> *mut FastPgPgCoreExecuteResult;
        fn fastpg_pgcore_execute_transaction_command(
            command: i32,
        ) -> *mut FastPgPgCoreExecuteResult;
        fn fastpg_pgcore_execute_params(
            prepared: *const FastPgPgCorePrepared,
            parameter_values: *const *const c_char,
            parameter_is_null: *const bool,
            parameter_datums: *const usize,
            parameter_is_datum: *const bool,
            parameter_count: i32,
        ) -> *mut FastPgPgCoreExecuteResult;
        fn fastpg_pgcore_execute_copy_from_stdin(
            sql: *const c_char,
            data: *const c_char,
            data_len: usize,
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
        fn fastpg_pgcore_execute_result_context(
            result: *const FastPgPgCoreExecuteResult,
        ) -> *const c_char;
        fn fastpg_pgcore_execute_result_cursorpos(result: *const FastPgPgCoreExecuteResult) -> i32;
        fn fastpg_pgcore_execute_result_internal_query(
            result: *const FastPgPgCoreExecuteResult,
        ) -> *const c_char;
        fn fastpg_pgcore_execute_result_internalpos(
            result: *const FastPgPgCoreExecuteResult,
        ) -> i32;
        fn fastpg_pgcore_execute_notice_count(result: *const FastPgPgCoreExecuteResult) -> i32;
        fn fastpg_pgcore_execute_notice_severity(
            result: *const FastPgPgCoreExecuteResult,
            notice_index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_execute_notice_sqlstate(
            result: *const FastPgPgCoreExecuteResult,
            notice_index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_execute_notice_message(
            result: *const FastPgPgCoreExecuteResult,
            notice_index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_execute_notice_detail(
            result: *const FastPgPgCoreExecuteResult,
            notice_index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_execute_notice_hint(
            result: *const FastPgPgCoreExecuteResult,
            notice_index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_execute_notice_context(
            result: *const FastPgPgCoreExecuteResult,
            notice_index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_execute_notice_cursorpos(
            result: *const FastPgPgCoreExecuteResult,
            notice_index: i32,
        ) -> i32;
        fn fastpg_pgcore_execute_statement_count(result: *const FastPgPgCoreExecuteResult) -> i32;
        fn fastpg_pgcore_execute_statement_summaries(
            result: *const FastPgPgCoreExecuteResult,
            summaries: *mut FastPgPgCoreExecuteStatementSummary,
            summary_capacity: i32,
        ) -> i32;
        fn fastpg_pgcore_execute_statement_command_tag(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_execute_statement_is_select(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> bool;
        fn fastpg_pgcore_execute_statement_column_count(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> i32;
        fn fastpg_pgcore_execute_statement_row_count(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> i32;
        fn fastpg_pgcore_execute_statement_has_processed_count(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> bool;
        fn fastpg_pgcore_execute_statement_processed_count(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> u64;
        fn fastpg_pgcore_execute_statement_is_copy_in(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> bool;
        fn fastpg_pgcore_execute_statement_is_copy_out(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> bool;
        fn fastpg_pgcore_execute_statement_copy_out_format(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> i32;
        fn fastpg_pgcore_execute_statement_copy_out_columns(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> i32;
        fn fastpg_pgcore_execute_statement_copy_out_chunk_count(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> i32;
        fn fastpg_pgcore_execute_statement_copy_out_chunk_data(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
            chunk_index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_execute_statement_copy_out_chunk_len(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
            chunk_index: i32,
        ) -> i32;
        fn fastpg_pgcore_execute_statement_copy_table(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_execute_statement_copy_table_oid(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> u32;
        fn fastpg_pgcore_execute_statement_copy_relation_column_count(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> i32;
        fn fastpg_pgcore_execute_statement_copy_column_count(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> i32;
        fn fastpg_pgcore_execute_statement_copy_format(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> i32;
        fn fastpg_pgcore_execute_statement_copy_header_line(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> i32;
        fn fastpg_pgcore_execute_statement_copy_on_error(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> i32;
        fn fastpg_pgcore_execute_statement_copy_freeze(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> bool;
        fn fastpg_pgcore_execute_statement_copy_foreign_table(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> bool;
        fn fastpg_pgcore_execute_statement_copy_partitioned_table(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> bool;
        fn fastpg_pgcore_execute_statement_copy_has_insert_triggers(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> bool;
        fn fastpg_pgcore_execute_statement_copy_has_generated_columns(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> bool;
        fn fastpg_pgcore_execute_statement_copy_source_text(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_execute_statement_copy_delimiter(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_execute_statement_copy_null_print(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_execute_statement_copy_default_print(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_execute_statement_copy_column_name(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
            column_index: i32,
        ) -> *const c_char;
        fn fastpg_pgcore_execute_statement_copy_column_attnum(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
            column_index: i32,
        ) -> i32;
        fn fastpg_pgcore_execute_statement_copy_column_type_oid(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
            column_index: i32,
        ) -> u32;
        fn fastpg_pgcore_execute_statement_copy_column_type_modifier(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
            column_index: i32,
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
        fn fastpg_pgcore_execute_column_type_modifier(
            result: *const FastPgPgCoreExecuteResult,
            statement_index: i32,
            column_index: i32,
        ) -> i32;
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
        fn fastpg_pgcore_input_text_datum(
            type_oid: u32,
            typmod: i32,
            value_text: *const c_char,
        ) -> *mut FastPgPgCoreInputDatumResult;
        fn fastpg_pgcore_input_datum_result_free(result: *mut FastPgPgCoreInputDatumResult);
        fn fastpg_pgcore_input_datum_result_ok(result: *const FastPgPgCoreInputDatumResult)
        -> bool;
        fn fastpg_pgcore_input_datum_result_sqlstate(
            result: *const FastPgPgCoreInputDatumResult,
        ) -> *const c_char;
        fn fastpg_pgcore_input_datum_result_message(
            result: *const FastPgPgCoreInputDatumResult,
        ) -> *const c_char;
        fn fastpg_pgcore_input_datum_result_detail(
            result: *const FastPgPgCoreInputDatumResult,
        ) -> *const c_char;
        fn fastpg_pgcore_input_datum_result_hint(
            result: *const FastPgPgCoreInputDatumResult,
        ) -> *const c_char;
        fn fastpg_pgcore_input_datum_result_context(
            result: *const FastPgPgCoreInputDatumResult,
        ) -> *const c_char;
        fn fastpg_pgcore_input_datum_result_cursorpos(
            result: *const FastPgPgCoreInputDatumResult,
        ) -> i32;
        fn fastpg_pgcore_input_datum_result_value(
            result: *const FastPgPgCoreInputDatumResult,
        ) -> usize;
        fn fastpg_pgcore_input_datum_result_typbyval(
            result: *const FastPgPgCoreInputDatumResult,
        ) -> bool;
        fn fastpg_pgcore_input_datum_result_typlen(
            result: *const FastPgPgCoreInputDatumResult,
        ) -> i16;
        fn fastpg_pgcore_input_datum_result_value_len(
            result: *const FastPgPgCoreInputDatumResult,
        ) -> usize;
        fn fastpg_pgcore_input_datum_result_payload(
            result: *const FastPgPgCoreInputDatumResult,
        ) -> *const u8;
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
            let mut buffer = MaybeUninit::<[u8; STACK_SQL_BUFFER_LEN]>::uninit();
            let buffer_ptr = buffer.as_mut_ptr().cast::<u8>();
            unsafe {
                ptr::copy_nonoverlapping(bytes.as_ptr(), buffer_ptr, bytes.len());
                *buffer_ptr.add(bytes.len()) = 0;
            }
            return f(buffer_ptr.cast());
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
                PgCoreErrorFields {
                    detail: unsafe {
                        optional_c_string(fastpg_pgcore_parse_result_detail(result.as_ptr()))
                    },
                    hint: unsafe {
                        optional_c_string(fastpg_pgcore_parse_result_hint(result.as_ptr()))
                    },
                    context: unsafe {
                        optional_c_string(fastpg_pgcore_parse_result_context(result.as_ptr()))
                    },
                    cursorpos: unsafe { fastpg_pgcore_parse_result_cursorpos(result.as_ptr()) },
                    ..Default::default()
                },
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

    struct NoticeCaptureGuard {
        active: bool,
    }

    impl NoticeCaptureGuard {
        fn begin() -> Self {
            unsafe {
                fastpg_pgcore_notice_capture_begin();
            }
            Self { active: true }
        }

        fn finish(mut self) -> Vec<PgCoreNotice> {
            let notices = self.finish_inner();
            self.active = false;
            notices
        }

        fn finish_inner(&self) -> Vec<PgCoreNotice> {
            unsafe {
                fastpg_pgcore_notice_capture_end();
            }
            let notice_count = unsafe { fastpg_pgcore_notice_capture_count() }.max(0);
            let notices = (0..notice_count)
                .map(|index| PgCoreNotice {
                    severity: unsafe { c_string(fastpg_pgcore_notice_capture_severity(index)) },
                    sqlstate: unsafe { c_string(fastpg_pgcore_notice_capture_sqlstate(index)) },
                    message: unsafe { c_string(fastpg_pgcore_notice_capture_message(index)) },
                    detail: unsafe {
                        optional_c_string(fastpg_pgcore_notice_capture_detail(index))
                    },
                    hint: unsafe { optional_c_string(fastpg_pgcore_notice_capture_hint(index)) },
                    context: unsafe {
                        optional_c_string(fastpg_pgcore_notice_capture_context(index))
                    },
                    cursorpos: unsafe { fastpg_pgcore_notice_capture_cursorpos(index) },
                })
                .collect();
            unsafe {
                fastpg_pgcore_notice_capture_clear();
            }
            notices
        }
    }

    impl Drop for NoticeCaptureGuard {
        fn drop(&mut self) {
            if self.active {
                let _ = self.finish_inner();
            }
        }
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
    pub struct PgCoreSession {
        database_oid: u32,
    }

    impl PgCoreSession {
        pub fn new(database_oid: u32) -> Self {
            let _ = fastpg_storage::fastpg_rust_relation_row_count(0);
            Self { database_oid }
        }

        fn set_database(&self) {
            set_pgcore_database(self.database_oid);
        }

        pub fn reset_session_state(&self) {
            let _guard = enter_pgcore_lane("reset_session_state");
            unsafe {
                fastpg_pgcore_reset_session_state();
            }
        }

        pub fn start_client_session(&self) -> Vec<PgCoreNotice> {
            let _guard = enter_pgcore_lane("start_client_session");
            self.set_database();
            let notice_capture = NoticeCaptureGuard::begin();
            unsafe {
                fastpg_pgcore_start_client_session();
            }
            notice_capture.finish()
        }

        pub fn end_client_session(&self) {
            let _guard = enter_pgcore_lane("end_client_session");
            unsafe {
                fastpg_pgcore_end_client_session();
            }
        }

        pub fn prepare(&self, sql: &str) -> Result<PreparedStatement, PgCoreError> {
            check_sql(sql)?;
            let _guard = enter_pgcore_lane("prepare");
            self.set_database();
            let prepared = with_c_sql(sql, |c_sql| unsafe { fastpg_pgcore_prepare(c_sql) });
            let Some(prepared) = NonNull::new(prepared) else {
                return Err(PgCoreError::new(
                    "XX000",
                    "PostgreSQL prepare returned a null result",
                    0,
                ));
            };
            let prepared = PreparedStatement {
                prepared,
                database_oid: self.database_oid,
            };
            if unsafe { fastpg_pgcore_prepared_ok(prepared.as_ptr()) } {
                drop(_guard);
                Ok(prepared)
            } else {
                let error = PgCoreError::with_fields(
                    unsafe { c_string(fastpg_pgcore_prepared_sqlstate(prepared.as_ptr())) },
                    unsafe { c_string(fastpg_pgcore_prepared_message(prepared.as_ptr())) },
                    PgCoreErrorFields {
                        detail: unsafe {
                            optional_c_string(fastpg_pgcore_prepared_detail(prepared.as_ptr()))
                        },
                        hint: unsafe {
                            optional_c_string(fastpg_pgcore_prepared_hint(prepared.as_ptr()))
                        },
                        context: unsafe {
                            optional_c_string(fastpg_pgcore_prepared_context(prepared.as_ptr()))
                        },
                        cursorpos: unsafe { fastpg_pgcore_prepared_cursorpos(prepared.as_ptr()) },
                        internal_query: unsafe {
                            optional_c_string(fastpg_pgcore_prepared_internal_query(
                                prepared.as_ptr(),
                            ))
                        },
                        internalpos: unsafe {
                            fastpg_pgcore_prepared_internalpos(prepared.as_ptr())
                        },
                        notices: prepared_notices_from_ptr(prepared.as_ptr()),
                        ..Default::default()
                    },
                );
                drop(_guard);
                Err(error)
            }
        }

        pub fn execute_simple(&self, sql: &str) -> Result<ExecutionResult, PgCoreError> {
            check_sql(sql)?;
            let _guard = enter_pgcore_lane("execute_simple");
            self.set_database();
            let result = with_c_sql(sql, |c_sql| unsafe { fastpg_pgcore_execute_simple(c_sql) });
            execution_result_from_ptr(result)
        }

        pub fn execute_simple_cstr(&self, sql: &CStr) -> Result<ExecutionResult, PgCoreError> {
            let _guard = enter_pgcore_lane("execute_simple");
            self.set_database();
            let result = unsafe { fastpg_pgcore_execute_simple(sql.as_ptr()) };
            execution_result_from_ptr(result)
        }

        pub fn execute_simple_cstr_fast(
            &self,
            sql: &CStr,
        ) -> Result<SimpleExecutionResult, PgCoreError> {
            let _guard = enter_pgcore_lane("execute_simple");
            self.set_database();
            let result = unsafe { fastpg_pgcore_execute_simple(sql.as_ptr()) };
            simple_execution_result_from_ptr(result)
        }

        pub fn execute_transaction_command(
            &self,
            command: PgCoreTransactionCommand,
        ) -> Result<ExecutionResult, PgCoreError> {
            let _guard = enter_pgcore_lane("transaction_command");
            self.set_database();
            if postgres_catalog_enabled() {
                let result =
                    unsafe { fastpg_pgcore_execute_transaction_command(command.pgcore_code()) };
                return execution_result_from_ptr(result);
            }
            match command {
                PgCoreTransactionCommand::Begin => {
                    unsafe {
                        fastpg_xid_begin();
                    }
                    fastpg_storage::begin_explicit_transaction();
                    fastpg_storage2::fastpg_storage2_xact_begin();
                }
                PgCoreTransactionCommand::Commit => {
                    unsafe {
                        fastpg_xid_commit();
                    }
                    fastpg_storage::commit_explicit_transaction();
                    fastpg_storage2::fastpg_storage2_xact_commit();
                }
                PgCoreTransactionCommand::Rollback => {
                    unsafe {
                        fastpg_xid_rollback();
                    }
                    fastpg_storage::abort_explicit_transaction();
                    fastpg_storage2::fastpg_storage2_xact_abort();
                }
            }
            Ok(ExecutionResult {
                statements: vec![ExecutionStatement {
                    command_tag: command.command_tag().into(),
                    command_rows: None,
                    is_select: false,
                    fields: Vec::new(),
                    rows: Vec::new(),
                    copy_in: None,
                    copy_out: None,
                }],
                notices: Vec::new(),
            })
        }

        pub fn execute_copy_from_stdin(
            &self,
            sql: &str,
            data: &[u8],
        ) -> Result<ExecutionResult, PgCoreError> {
            check_sql(sql)?;
            let _guard = enter_pgcore_lane("copy_from_stdin");
            self.set_database();
            let result = with_c_sql(sql, |c_sql| unsafe {
                fastpg_pgcore_execute_copy_from_stdin(
                    c_sql,
                    data.as_ptr().cast::<c_char>(),
                    data.len(),
                )
            });
            execution_result_from_ptr(result)
        }

        pub fn input_text_datum(
            &self,
            type_oid: u32,
            typmod: i32,
            value: &str,
        ) -> Result<PgCoreInputDatum, PgCoreError> {
            let value = CString::new(value).map_err(|_| {
                PgCoreError::new("22023", "COPY text value contains an embedded NUL byte", 0)
            })?;
            let _guard = enter_pgcore_lane("input_text_datum");
            self.set_database();
            let result =
                unsafe { fastpg_pgcore_input_text_datum(type_oid, typmod, value.as_ptr()) };
            let Some(result) = NonNull::new(result) else {
                return Err(PgCoreError::new(
                    "XX000",
                    "PostgreSQL type input returned a null result",
                    0,
                ));
            };
            let result = InputDatumResult(result);
            if unsafe { fastpg_pgcore_input_datum_result_ok(result.as_ptr()) } {
                let typbyval =
                    unsafe { fastpg_pgcore_input_datum_result_typbyval(result.as_ptr()) };
                let value = unsafe { fastpg_pgcore_input_datum_result_value(result.as_ptr()) };
                let typlen = unsafe { fastpg_pgcore_input_datum_result_typlen(result.as_ptr()) };
                let payload = if typbyval {
                    None
                } else {
                    let len =
                        unsafe { fastpg_pgcore_input_datum_result_value_len(result.as_ptr()) };
                    let ptr = unsafe { fastpg_pgcore_input_datum_result_payload(result.as_ptr()) };
                    if len == 0 {
                        Some(Vec::new())
                    } else if ptr.is_null() {
                        return Err(PgCoreError::new(
                            "XX000",
                            "PostgreSQL type input returned a null by-reference payload",
                            0,
                        ));
                    } else {
                        Some(unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec())
                    }
                };
                Ok(PgCoreInputDatum {
                    value,
                    typbyval,
                    typlen,
                    payload,
                })
            } else {
                Err(PgCoreError::with_fields(
                    unsafe { c_string(fastpg_pgcore_input_datum_result_sqlstate(result.as_ptr())) },
                    unsafe { c_string(fastpg_pgcore_input_datum_result_message(result.as_ptr())) },
                    PgCoreErrorFields {
                        detail: unsafe {
                            optional_c_string(fastpg_pgcore_input_datum_result_detail(
                                result.as_ptr(),
                            ))
                        },
                        hint: unsafe {
                            optional_c_string(fastpg_pgcore_input_datum_result_hint(
                                result.as_ptr(),
                            ))
                        },
                        context: unsafe {
                            optional_c_string(fastpg_pgcore_input_datum_result_context(
                                result.as_ptr(),
                            ))
                        },
                        cursorpos: unsafe {
                            fastpg_pgcore_input_datum_result_cursorpos(result.as_ptr())
                        },
                        ..Default::default()
                    },
                ))
            }
        }
    }

    struct InputDatumResult(NonNull<FastPgPgCoreInputDatumResult>);

    impl InputDatumResult {
        fn as_ptr(&self) -> *const FastPgPgCoreInputDatumResult {
            self.0.as_ptr()
        }
    }

    impl Drop for InputDatumResult {
        fn drop(&mut self) {
            unsafe {
                fastpg_pgcore_input_datum_result_free(self.0.as_ptr());
            }
        }
    }

    #[derive(Debug)]
    pub struct PreparedStatement {
        prepared: NonNull<FastPgPgCorePrepared>,
        database_oid: u32,
    }

    impl PreparedStatement {
        fn as_ptr(&self) -> *const FastPgPgCorePrepared {
            self.prepared.as_ptr()
        }

        fn set_database(&self) {
            set_pgcore_database(self.database_oid);
        }

        pub fn describe(&self) -> StatementDescription {
            let _guard = enter_pgcore_lane("describe");
            self.set_database();
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
                        type_modifier: unsafe {
                            fastpg_pgcore_prepared_field_type_modifier(self.as_ptr(), index)
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
                self.set_database();
                let notice_capture = NoticeCaptureGuard::begin();
                let result = unsafe { fastpg_pgcore_execute(self.as_ptr()) };
                let result = execution_result_from_ptr(result);
                let notices = notice_capture.finish();
                return result.map(|mut result| {
                    result.notices = notices;
                    result
                });
            }

            let encoded_params = EncodedParams::new(params)?;
            let _guard = enter_pgcore_lane("execute");
            self.set_database();
            let notice_capture = NoticeCaptureGuard::begin();
            let result = execute_prepared_ptr_with_params(self.as_ptr(), &encoded_params);
            let notices = notice_capture.finish();
            result.map(|mut result| {
                result.notices = notices;
                result
            })
        }
    }

    struct EncodedParams {
        encoded_text_params: Vec<CString>,
        parameter_values: Vec<*const c_char>,
        parameter_is_null: Vec<bool>,
        parameter_datums: Vec<usize>,
        parameter_is_datum: Vec<bool>,
        param_count: i32,
    }

    impl EncodedParams {
        fn new(params: &[PgCoreParam]) -> Result<Self, PgCoreError> {
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
            Ok(Self {
                encoded_text_params,
                parameter_values,
                parameter_is_null,
                parameter_datums,
                parameter_is_datum,
                param_count,
            })
        }
    }

    fn execute_prepared_ptr_with_params(
        prepared: *const FastPgPgCorePrepared,
        params: &EncodedParams,
    ) -> Result<ExecutionResult, PgCoreError> {
        let _keep_text_params_alive = &params.encoded_text_params;
        let mut prepared_notices = prepared_notices_from_ptr(prepared);
        let result = if params.param_count == 0 {
            unsafe { fastpg_pgcore_execute(prepared) }
        } else {
            unsafe {
                fastpg_pgcore_execute_params(
                    prepared,
                    params.parameter_values.as_ptr(),
                    params.parameter_is_null.as_ptr(),
                    params.parameter_datums.as_ptr(),
                    params.parameter_is_datum.as_ptr(),
                    params.param_count,
                )
            }
        };
        match execution_result_from_ptr(result) {
            Ok(mut result) => {
                if !prepared_notices.is_empty() {
                    prepared_notices.extend(result.notices);
                    result.notices = prepared_notices;
                }
                Ok(result)
            }
            Err(mut error) => {
                if !prepared_notices.is_empty() {
                    prepared_notices.extend(std::mem::take(&mut error.notices));
                    error.notices = prepared_notices;
                }
                Err(error)
            }
        }
    }

    fn prepared_notices_from_ptr(prepared: *const FastPgPgCorePrepared) -> Vec<PgCoreNotice> {
        let notice_count = unsafe { fastpg_pgcore_prepared_notice_count(prepared) }.max(0);
        let mut notices = Vec::with_capacity(notice_count as usize);
        for notice_index in 0..notice_count {
            notices.push(PgCoreNotice {
                severity: unsafe {
                    c_string(fastpg_pgcore_prepared_notice_severity(
                        prepared,
                        notice_index,
                    ))
                },
                sqlstate: unsafe {
                    c_string(fastpg_pgcore_prepared_notice_sqlstate(
                        prepared,
                        notice_index,
                    ))
                },
                message: unsafe {
                    c_string(fastpg_pgcore_prepared_notice_message(
                        prepared,
                        notice_index,
                    ))
                },
                detail: unsafe {
                    optional_c_string(fastpg_pgcore_prepared_notice_detail(prepared, notice_index))
                },
                hint: unsafe {
                    optional_c_string(fastpg_pgcore_prepared_notice_hint(prepared, notice_index))
                },
                context: unsafe {
                    optional_c_string(fastpg_pgcore_prepared_notice_context(
                        prepared,
                        notice_index,
                    ))
                },
                cursorpos: unsafe {
                    fastpg_pgcore_prepared_notice_cursorpos(prepared, notice_index)
                },
            });
        }
        notices
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
            let partial = result.to_execution_result().statements;
            let mut error = PgCoreError::with_fields(
                unsafe { c_string(fastpg_pgcore_execute_result_sqlstate(result.as_ptr())) },
                unsafe { c_string(fastpg_pgcore_execute_result_message(result.as_ptr())) },
                PgCoreErrorFields {
                    detail: unsafe {
                        optional_c_string(fastpg_pgcore_execute_result_detail(result.as_ptr()))
                    },
                    hint: unsafe {
                        optional_c_string(fastpg_pgcore_execute_result_hint(result.as_ptr()))
                    },
                    context: unsafe {
                        optional_c_string(fastpg_pgcore_execute_result_context(result.as_ptr()))
                    },
                    cursorpos: unsafe { fastpg_pgcore_execute_result_cursorpos(result.as_ptr()) },
                    internal_query: unsafe {
                        optional_c_string(fastpg_pgcore_execute_result_internal_query(
                            result.as_ptr(),
                        ))
                    },
                    internalpos: unsafe {
                        fastpg_pgcore_execute_result_internalpos(result.as_ptr())
                    },
                    notices: result.to_notices(),
                    ..Default::default()
                },
            );
            error.partial = partial;
            Err(error)
        }
    }

    fn simple_execution_result_from_ptr(
        result: *mut FastPgPgCoreExecuteResult,
    ) -> Result<SimpleExecutionResult, PgCoreError> {
        let Some(result) = NonNull::new(result) else {
            return Err(PgCoreError::new(
                "XX000",
                "PostgreSQL execute returned a null result",
                0,
            ));
        };
        let result = ExecuteResult(result);
        if unsafe { fastpg_pgcore_execute_result_ok(result.as_ptr()) } {
            if let Some((tag, rows)) = result.to_single_command() {
                let notices = result.to_notices();
                Ok(SimpleExecutionResult::Command { notices, tag, rows })
            } else {
                Ok(SimpleExecutionResult::Full(result.to_execution_result()))
            }
        } else {
            let partial = result.to_execution_result().statements;
            let mut error = PgCoreError::with_fields(
                unsafe { c_string(fastpg_pgcore_execute_result_sqlstate(result.as_ptr())) },
                unsafe { c_string(fastpg_pgcore_execute_result_message(result.as_ptr())) },
                PgCoreErrorFields {
                    detail: unsafe {
                        optional_c_string(fastpg_pgcore_execute_result_detail(result.as_ptr()))
                    },
                    hint: unsafe {
                        optional_c_string(fastpg_pgcore_execute_result_hint(result.as_ptr()))
                    },
                    context: unsafe {
                        optional_c_string(fastpg_pgcore_execute_result_context(result.as_ptr()))
                    },
                    cursorpos: unsafe { fastpg_pgcore_execute_result_cursorpos(result.as_ptr()) },
                    internal_query: unsafe {
                        optional_c_string(fastpg_pgcore_execute_result_internal_query(
                            result.as_ptr(),
                        ))
                    },
                    internalpos: unsafe {
                        fastpg_pgcore_execute_result_internalpos(result.as_ptr())
                    },
                    notices: result.to_notices(),
                    ..Default::default()
                },
            );
            error.partial = partial;
            Err(error)
        }
    }

    impl Drop for PreparedStatement {
        fn drop(&mut self) {
            let _guard = enter_pgcore_lane("prepared_free");
            unsafe {
                fastpg_pgcore_prepared_free(self.prepared.as_ptr());
            }
        }
    }

    struct ExecuteResult(NonNull<FastPgPgCoreExecuteResult>);

    impl ExecuteResult {
        const INLINE_STATEMENT_SUMMARIES: usize = 8;

        fn as_ptr(&self) -> *const FastPgPgCoreExecuteResult {
            self.0.as_ptr()
        }

        fn to_notices(&self) -> Vec<PgCoreNotice> {
            let notice_count = unsafe { fastpg_pgcore_execute_notice_count(self.as_ptr()) }.max(0);
            let mut notices = Vec::with_capacity(notice_count as usize);
            for notice_index in 0..notice_count {
                notices.push(PgCoreNotice {
                    severity: unsafe {
                        c_string(fastpg_pgcore_execute_notice_severity(
                            self.as_ptr(),
                            notice_index,
                        ))
                    },
                    sqlstate: unsafe {
                        c_string(fastpg_pgcore_execute_notice_sqlstate(
                            self.as_ptr(),
                            notice_index,
                        ))
                    },
                    message: unsafe {
                        c_string(fastpg_pgcore_execute_notice_message(
                            self.as_ptr(),
                            notice_index,
                        ))
                    },
                    detail: unsafe {
                        optional_c_string(fastpg_pgcore_execute_notice_detail(
                            self.as_ptr(),
                            notice_index,
                        ))
                    },
                    hint: unsafe {
                        optional_c_string(fastpg_pgcore_execute_notice_hint(
                            self.as_ptr(),
                            notice_index,
                        ))
                    },
                    context: unsafe {
                        optional_c_string(fastpg_pgcore_execute_notice_context(
                            self.as_ptr(),
                            notice_index,
                        ))
                    },
                    cursorpos: unsafe {
                        fastpg_pgcore_execute_notice_cursorpos(self.as_ptr(), notice_index)
                    },
                });
            }
            notices
        }

        fn to_single_command(&self) -> Option<(Cow<'static, str>, Option<usize>)> {
            let statement_count =
                unsafe { fastpg_pgcore_execute_statement_count(self.as_ptr()) }.max(0);
            if statement_count != 1 {
                return None;
            }

            let mut summary = FastPgPgCoreExecuteStatementSummary::default();
            let summary_count = unsafe {
                fastpg_pgcore_execute_statement_summaries(self.as_ptr(), &mut summary, 1)
            };
            if summary_count != 1
                || summary.is_select
                || summary.column_count != 0
                || summary.row_count != 0
                || summary.copy_in
                || summary.copy_out
            {
                return None;
            }

            let rows = summary
                .has_processed_count
                .then(|| usize::try_from(summary.processed_count).unwrap_or(usize::MAX));
            Some((unsafe { command_tag(summary.command_tag) }, rows))
        }

        fn to_execution_result(&self) -> ExecutionResult {
            let notices = self.to_notices();
            let statement_count =
                unsafe { fastpg_pgcore_execute_statement_count(self.as_ptr()) }.max(0);
            let statement_len = statement_count as usize;
            let mut inline_summaries =
                [FastPgPgCoreExecuteStatementSummary::default(); Self::INLINE_STATEMENT_SUMMARIES];
            let mut heap_summaries = Vec::new();
            let summaries = if statement_len <= Self::INLINE_STATEMENT_SUMMARIES {
                &mut inline_summaries[..statement_len]
            } else {
                heap_summaries.resize(
                    statement_len,
                    FastPgPgCoreExecuteStatementSummary::default(),
                );
                heap_summaries.as_mut_slice()
            };
            let summary_count = if statement_count == 0 {
                0
            } else {
                unsafe {
                    fastpg_pgcore_execute_statement_summaries(
                        self.as_ptr(),
                        summaries.as_mut_ptr(),
                        statement_count,
                    )
                }
                .max(0)
                .min(statement_count)
            };
            let mut statements = Vec::with_capacity(statement_count as usize);
            for statement_index in 0..statement_count {
                let summary = if statement_index < summary_count {
                    summaries[statement_index as usize]
                } else {
                    FastPgPgCoreExecuteStatementSummary {
                        command_tag: unsafe {
                            fastpg_pgcore_execute_statement_command_tag(
                                self.as_ptr(),
                                statement_index,
                            )
                        },
                        is_select: unsafe {
                            fastpg_pgcore_execute_statement_is_select(
                                self.as_ptr(),
                                statement_index,
                            )
                        },
                        column_count: unsafe {
                            fastpg_pgcore_execute_statement_column_count(
                                self.as_ptr(),
                                statement_index,
                            )
                        },
                        row_count: unsafe {
                            fastpg_pgcore_execute_statement_row_count(
                                self.as_ptr(),
                                statement_index,
                            )
                        },
                        has_processed_count: unsafe {
                            fastpg_pgcore_execute_statement_has_processed_count(
                                self.as_ptr(),
                                statement_index,
                            )
                        },
                        processed_count: unsafe {
                            fastpg_pgcore_execute_statement_processed_count(
                                self.as_ptr(),
                                statement_index,
                            )
                        },
                        copy_in: unsafe {
                            fastpg_pgcore_execute_statement_is_copy_in(
                                self.as_ptr(),
                                statement_index,
                            )
                        },
                        copy_out: unsafe {
                            fastpg_pgcore_execute_statement_is_copy_out(
                                self.as_ptr(),
                                statement_index,
                            )
                        },
                    }
                };
                let field_count = summary.column_count.max(0);
                let row_count = summary.row_count.max(0);
                let command_rows = summary
                    .has_processed_count
                    .then(|| usize::try_from(summary.processed_count).unwrap_or(usize::MAX));
                let copy_in = summary.copy_in.then(|| {
                    let columns = unsafe {
                        fastpg_pgcore_execute_statement_copy_column_count(
                            self.as_ptr(),
                            statement_index,
                        )
                    }
                    .max(0);
                    let column_names = (0..columns)
                        .filter_map(|column_index| {
                            let name = unsafe {
                                optional_c_string(fastpg_pgcore_execute_statement_copy_column_name(
                                    self.as_ptr(),
                                    statement_index,
                                    column_index,
                                ))
                            };
                            name.filter(|name| !name.is_empty())
                        })
                        .collect();
                    let column_metadata = (0..columns)
                        .map(|column_index| {
                            let name = unsafe {
                                c_string(fastpg_pgcore_execute_statement_copy_column_name(
                                    self.as_ptr(),
                                    statement_index,
                                    column_index,
                                ))
                            };
                            PgCoreCopyColumn {
                                name,
                                attnum: unsafe {
                                    fastpg_pgcore_execute_statement_copy_column_attnum(
                                        self.as_ptr(),
                                        statement_index,
                                        column_index,
                                    )
                                } as i16,
                                type_oid: unsafe {
                                    fastpg_pgcore_execute_statement_copy_column_type_oid(
                                        self.as_ptr(),
                                        statement_index,
                                        column_index,
                                    )
                                },
                                type_modifier: unsafe {
                                    fastpg_pgcore_execute_statement_copy_column_type_modifier(
                                        self.as_ptr(),
                                        statement_index,
                                        column_index,
                                    )
                                },
                            }
                        })
                        .collect();

                    PgCoreCopyIn {
                        source_sql: unsafe {
                            c_string(fastpg_pgcore_execute_statement_copy_source_text(
                                self.as_ptr(),
                                statement_index,
                            ))
                        },
                        table: unsafe {
                            c_string(fastpg_pgcore_execute_statement_copy_table(
                                self.as_ptr(),
                                statement_index,
                            ))
                        },
                        table_oid: unsafe {
                            fastpg_pgcore_execute_statement_copy_table_oid(
                                self.as_ptr(),
                                statement_index,
                            )
                        },
                        relation_columns: unsafe {
                            fastpg_pgcore_execute_statement_copy_relation_column_count(
                                self.as_ptr(),
                                statement_index,
                            )
                        }
                        .max(0) as usize,
                        columns: columns as usize,
                        format: unsafe {
                            fastpg_pgcore_execute_statement_copy_format(
                                self.as_ptr(),
                                statement_index,
                            )
                        },
                        header_line: unsafe {
                            fastpg_pgcore_execute_statement_copy_header_line(
                                self.as_ptr(),
                                statement_index,
                            )
                        },
                        on_error: unsafe {
                            fastpg_pgcore_execute_statement_copy_on_error(
                                self.as_ptr(),
                                statement_index,
                            )
                        },
                        freeze: unsafe {
                            fastpg_pgcore_execute_statement_copy_freeze(
                                self.as_ptr(),
                                statement_index,
                            )
                        },
                        foreign_table: unsafe {
                            fastpg_pgcore_execute_statement_copy_foreign_table(
                                self.as_ptr(),
                                statement_index,
                            )
                        },
                        partitioned_table: unsafe {
                            fastpg_pgcore_execute_statement_copy_partitioned_table(
                                self.as_ptr(),
                                statement_index,
                            )
                        },
                        has_insert_triggers: unsafe {
                            fastpg_pgcore_execute_statement_copy_has_insert_triggers(
                                self.as_ptr(),
                                statement_index,
                            )
                        },
                        has_generated_columns: unsafe {
                            fastpg_pgcore_execute_statement_copy_has_generated_columns(
                                self.as_ptr(),
                                statement_index,
                            )
                        },
                        delimiter: unsafe {
                            optional_c_string(fastpg_pgcore_execute_statement_copy_delimiter(
                                self.as_ptr(),
                                statement_index,
                            ))
                        }
                        .unwrap_or_else(|| "\t".to_owned()),
                        null_print: unsafe {
                            optional_c_string(fastpg_pgcore_execute_statement_copy_null_print(
                                self.as_ptr(),
                                statement_index,
                            ))
                        }
                        .unwrap_or_else(|| "\\N".to_owned()),
                        default_print: unsafe {
                            optional_c_string(fastpg_pgcore_execute_statement_copy_default_print(
                                self.as_ptr(),
                                statement_index,
                            ))
                        },
                        column_names,
                        column_metadata,
                    }
                });
                let copy_out = summary.copy_out.then(|| {
                    let chunk_count = unsafe {
                        fastpg_pgcore_execute_statement_copy_out_chunk_count(
                            self.as_ptr(),
                            statement_index,
                        )
                    }
                    .max(0);
                    let mut chunks = Vec::with_capacity(chunk_count as usize);
                    for chunk_index in 0..chunk_count {
                        let len = unsafe {
                            fastpg_pgcore_execute_statement_copy_out_chunk_len(
                                self.as_ptr(),
                                statement_index,
                                chunk_index,
                            )
                        }
                        .max(0) as usize;
                        let ptr = unsafe {
                            fastpg_pgcore_execute_statement_copy_out_chunk_data(
                                self.as_ptr(),
                                statement_index,
                                chunk_index,
                            )
                        };
                        if len == 0 {
                            chunks.push(Vec::new());
                        } else if !ptr.is_null() {
                            chunks.push(
                                unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) }
                                    .to_vec(),
                            );
                        }
                    }

                    PgCoreCopyOut {
                        format: unsafe {
                            fastpg_pgcore_execute_statement_copy_out_format(
                                self.as_ptr(),
                                statement_index,
                            )
                        } as i8,
                        columns: unsafe {
                            fastpg_pgcore_execute_statement_copy_out_columns(
                                self.as_ptr(),
                                statement_index,
                            )
                        }
                        .max(0) as usize,
                        chunks,
                    }
                });
                let mut fields = Vec::with_capacity(field_count as usize);
                for column_index in 0..field_count {
                    fields.push(PgCoreField {
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
                        type_modifier: unsafe {
                            fastpg_pgcore_execute_column_type_modifier(
                                self.as_ptr(),
                                statement_index,
                                column_index,
                            )
                        },
                    });
                }
                let mut rows = Vec::with_capacity(row_count as usize);
                for row_index in 0..row_count {
                    let mut row = Vec::with_capacity(field_count as usize);
                    for column_index in 0..field_count {
                        if unsafe {
                            fastpg_pgcore_execute_value_is_null(
                                self.as_ptr(),
                                statement_index,
                                row_index,
                                column_index,
                            )
                        } {
                            row.push(PgCoreValue::Null);
                        } else {
                            row.push(PgCoreValue::Text(unsafe {
                                c_string(fastpg_pgcore_execute_value_text(
                                    self.as_ptr(),
                                    statement_index,
                                    row_index,
                                    column_index,
                                ))
                            }));
                        }
                    }
                    rows.push(row);
                }
                statements.push(ExecutionStatement {
                    command_tag: unsafe { command_tag(summary.command_tag) },
                    command_rows,
                    is_select: summary.is_select,
                    fields,
                    rows,
                    copy_in,
                    copy_out,
                });
            }
            ExecutionResult {
                notices,
                statements,
            }
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

#[cfg(not(feature = "postgres-execution"))]
mod inner {
    use super::{
        ExecutionResult, PgCoreError, PgCoreInputDatum, PgCoreLaneMetrics, PgCoreNotice,
        RawParseSummary, StatementDescription,
    };

    pub fn pgcore_lane_metrics() -> PgCoreLaneMetrics {
        PgCoreLaneMetrics::default()
    }

    pub(super) fn postgres_catalog_enabled() -> bool {
        false
    }

    pub fn raw_parse(_sql: &str) -> Result<RawParseSummary, PgCoreError> {
        Ok(RawParseSummary { statement_count: 0 })
    }

    #[derive(Clone, Debug)]
    pub struct PgCoreSession;

    impl PgCoreSession {
        pub fn new(_database_oid: u32) -> Self {
            Self
        }

        pub fn prepare(&self, _sql: &str) -> Result<PreparedStatement, PgCoreError> {
            Err(PgCoreError::new(
                "0A000",
                "fastpg-pgcore was built without PostgreSQL execution",
                0,
            ))
        }

        pub fn execute_simple(&self, _sql: &str) -> Result<ExecutionResult, PgCoreError> {
            Err(PgCoreError::new(
                "0A000",
                "fastpg-pgcore was built without PostgreSQL execution",
                0,
            ))
        }

        pub fn execute_copy_from_stdin(
            &self,
            _sql: &str,
            _data: &[u8],
        ) -> Result<ExecutionResult, PgCoreError> {
            Err(PgCoreError::new(
                "0A000",
                "fastpg-pgcore was built without PostgreSQL execution",
                0,
            ))
        }

        pub fn input_text_datum(
            &self,
            _type_oid: u32,
            _typmod: i32,
            _value: &str,
        ) -> Result<PgCoreInputDatum, PgCoreError> {
            Err(PgCoreError::new(
                "0A000",
                "fastpg-pgcore was built without postgres-linked support",
                0,
            ))
        }

        pub fn reset_session_state(&self) {}

        pub fn start_client_session(&self) -> Vec<PgCoreNotice> {
            Vec::new()
        }

        pub fn end_client_session(&self) {}
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
                "fastpg-pgcore was built without PostgreSQL execution",
                0,
            ))
        }

        pub fn execute_with_params(
            &self,
            _params: &[super::PgCoreParam],
        ) -> Result<ExecutionResult, PgCoreError> {
            Err(PgCoreError::new(
                "0A000",
                "fastpg-pgcore was built without PostgreSQL execution",
                0,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "postgres-execution")]
    fn unique_pg_name(prefix: &str) -> String {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

        format!(
            "{prefix}_{}_{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )
    }

    #[test]
    fn raw_parse_smoke() {
        let summary = raw_parse("select 1").unwrap();
        #[cfg(feature = "postgres-execution")]
        assert_eq!(summary.statement_count, 1);
        #[cfg(not(feature = "postgres-execution"))]
        assert_eq!(summary.statement_count, 0);
    }

    #[cfg(feature = "postgres-execution")]
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

    #[cfg(all(feature = "postgres-execution", feature = "rust-catalog"))]
    #[test]
    fn current_database_uses_session_database() {
        let session = PgCoreSession::with_database("regression");
        let result = session
            .execute_simple("select current_database(), current_catalog = current_database()")
            .unwrap();
        assert_eq!(
            result.statements[0].rows,
            vec![vec![
                PgCoreValue::Text("regression".to_owned()),
                PgCoreValue::Text("t".to_owned()),
            ]]
        );

        let result = session
            .execute_simple(
                "select datname from pg_database where oid = (select oid from pg_database where datname = current_database())",
            )
            .unwrap();
        assert_eq!(
            result.statements[0].rows,
            vec![vec![PgCoreValue::Text("regression".to_owned())]]
        );
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn current_schema_uses_default_public_search_path() {
        let session = PgCoreSession::new();
        let result = session.execute_simple("select current_schema").unwrap();
        assert_eq!(
            result.statements[0].rows,
            vec![vec![PgCoreValue::Text("public".to_owned())]]
        );
    }

    #[cfg(all(feature = "postgres-execution", feature = "rust-catalog"))]
    #[test]
    fn prepared_statements_restore_their_session_database() {
        let first = PgCoreSession::with_database("fastpg_db_a");
        let second = PgCoreSession::with_database("fastpg_db_b");
        let first_statement = first.prepare("select current_database()").unwrap();
        let second_statement = second.prepare("select current_database()").unwrap();

        assert_eq!(
            second_statement.execute().unwrap().statements[0].rows,
            vec![vec![PgCoreValue::Text("fastpg_db_b".to_owned())]]
        );
        assert_eq!(
            first_statement.execute().unwrap().statements[0].rows,
            vec![vec![PgCoreValue::Text("fastpg_db_a".to_owned())]]
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
    fn input_text_datum_uses_postgres_type_input() {
        const VARBIT_OID: u32 = 1562;

        let session = PgCoreSession::new();
        let int4 = session.input_text_datum(INT4_OID, -1, "42").unwrap();
        assert!(int4.typbyval);
        assert_eq!(int4.value, 42);
        assert!(int4.payload.is_none());

        let varbit = session.input_text_datum(VARBIT_OID, -1, "101").unwrap();
        assert!(!varbit.typbyval);
        assert_eq!(varbit.typlen, -1);
        assert!(
            varbit
                .payload
                .as_ref()
                .is_some_and(|payload| !payload.is_empty())
        );
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
    fn execute_simple_create_table_is_visible_to_following_query() {
        let session = PgCoreSession::new();
        let table = format!("fastpg_pgcore_simple_create_{}", std::process::id());
        let _ = session.execute_simple(&format!("drop table if exists {table}"));

        let create = session
            .execute_simple(&format!("create table {table}(id int not null)"))
            .unwrap();
        assert_eq!(create.statements[0].command_tag, "CREATE TABLE");

        let insert = session
            .execute_simple(&format!("insert into {table} values (1), (2)"))
            .unwrap();
        assert_eq!(insert.statements[0].command_tag, "INSERT");

        let count = session
            .execute_simple(&format!("select count(*) from {table}"))
            .unwrap();
        assert_eq!(
            count.statements[0].rows,
            vec![vec![PgCoreValue::Text("2".to_owned())]]
        );

        session
            .execute_simple(&format!("drop table if exists {table}"))
            .unwrap();
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn execute_simple_multi_statement_sees_prior_ddl() {
        let session = PgCoreSession::new();
        let table = format!("fastpg_pgcore_simple_multi_{}", std::process::id());
        let _ = session.execute_simple(&format!("drop table if exists {table}"));

        let result = session
            .execute_simple(&format!(
                "create table {table}(id int not null); \
                 insert into {table} values (1), (2); \
                 select count(*) from {table};"
            ))
            .unwrap();
        assert_eq!(result.statements[0].command_tag, "CREATE TABLE");
        assert_eq!(result.statements[1].command_tag, "INSERT");
        assert_eq!(
            result.statements[2].rows,
            vec![vec![PgCoreValue::Text("2".to_owned())]]
        );

        session
            .execute_simple(&format!("drop table if exists {table}"))
            .unwrap();
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn execute_simple_captures_more_than_two_rows() {
        let session = PgCoreSession::new();
        let result = session
            .execute_simple("select * from (values (1), (2), (3)) as rows(id)")
            .unwrap();

        assert_eq!(
            result.statements[0].rows,
            vec![
                vec![PgCoreValue::Text("1".to_owned())],
                vec![PgCoreValue::Text("2".to_owned())],
                vec![PgCoreValue::Text("3".to_owned())],
            ]
        );
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
    fn execute_recreated_serial_sequence_starts_at_beginning() {
        let session = PgCoreSession::new();
        let table = format!("fastpg_pgcore_serial_recreate_{}", std::process::id());
        let sequence = format!("{table}_id_seq");

        session
            .prepare(&format!("drop table if exists {table} cascade"))
            .unwrap()
            .execute()
            .unwrap();
        session
            .prepare(&format!("create table {table}(id serial)"))
            .unwrap()
            .execute()
            .unwrap();
        session
            .prepare(&format!("insert into {table} default values"))
            .unwrap()
            .execute()
            .unwrap();
        session
            .prepare(&format!(
                "select setval('{sequence}'::regclass, 2147483647, true)"
            ))
            .unwrap()
            .execute()
            .unwrap();
        session
            .prepare(&format!("drop table if exists {table} cascade"))
            .unwrap()
            .execute()
            .unwrap();

        session
            .prepare(&format!("create table {table}(id serial)"))
            .unwrap()
            .execute()
            .unwrap();
        session
            .prepare(&format!("insert into {table} default values"))
            .unwrap()
            .execute()
            .unwrap();
        let select = session
            .prepare(&format!("select id from {table}"))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(
            select.statements[0].rows[0][0],
            PgCoreValue::Text("1".to_owned())
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
    fn execute_view_rewrite_uses_dynamic_pg_rewrite_catalog() {
        let session = PgCoreSession::new();
        let table = unique_pg_name("fp_view_base");
        let view = unique_pg_name("fp_view");

        session
            .prepare(&format!(
                "create table {table}(id int not null, label text)"
            ))
            .unwrap()
            .execute()
            .unwrap();
        session
            .prepare(&format!("insert into {table} values (1, 'one')"))
            .unwrap()
            .execute()
            .unwrap();

        let create = session
            .prepare(&format!(
                "create view {view} as select id, label from {table} where id >= 0"
            ))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(create.statements[0].command_tag, "CREATE VIEW");

        let rewrite = session
            .prepare(&format!(
                "select rulename from pg_rewrite where ev_class = '{view}'::regclass"
            ))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(
            rewrite.statements[0].rows,
            vec![vec![PgCoreValue::Text("_RETURN".to_owned())]]
        );

        session
            .prepare(&format!(
                "select c.relchecks, c.relkind, c.relhasindex, c.relhasrules, \
                 c.relhastriggers, c.relrowsecurity, c.relforcerowsecurity, \
                 false as relhasoids, c.relispartition, \
                 pg_catalog.array_to_string(c.reloptions || array(select 'toast.' || x from pg_catalog.unnest(tc.reloptions) x), ', '), \
                 c.reltablespace, \
                 case when c.reloftype = 0 then '' else c.reloftype::pg_catalog.regtype::pg_catalog.text end, \
                 c.relpersistence, c.relreplident, am.amname \
                 from pg_catalog.pg_class c \
                 left join pg_catalog.pg_class tc on (c.reltoastrelid = tc.oid) \
                 left join pg_catalog.pg_am am on (c.relam = am.oid) \
                 where c.oid = '{view}'::regclass::oid"
            ))
            .unwrap()
            .execute()
            .unwrap();

        let view_definition = session
            .prepare(&format!(
                "/* Get view's definition */\nselect pg_catalog.pg_get_viewdef('{view}'::regclass::oid, true)"
            ))
            .unwrap()
            .execute()
            .unwrap();
        assert!(matches!(
            &view_definition.statements[0].rows[0][0],
            PgCoreValue::Text(value) if value.contains(&table)
        ));

        let select = session
            .prepare(&format!("select id, label from {view}"))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(
            select.statements[0].rows,
            vec![vec![
                PgCoreValue::Text("1".to_owned()),
                PgCoreValue::Text("one".to_owned())
            ]]
        );

        session
            .prepare(&format!("drop view if exists {view}"))
            .unwrap()
            .execute()
            .unwrap();
        session
            .prepare(&format!("drop table if exists {table}"))
            .unwrap()
            .execute()
            .unwrap();
    }

    #[cfg(feature = "postgres-execution")]
    #[test]
    fn execute_user_defined_operator_metadata_uses_dynamic_catalogs() {
        let session = PgCoreSession::new();
        let typ = unique_pg_name("fp_myint");
        let input = unique_pg_name("fp_myintin");
        let output = unique_pg_name("fp_myintout");
        let hash = unique_pg_name("fp_myinthash");
        let eq = unique_pg_name("fp_myinteq");
        let opclass = unique_pg_name("fp_myint_ops");
        let table = unique_pg_name("fp_inttest");

        session
            .prepare(&format!("create type {typ}"))
            .unwrap()
            .execute()
            .unwrap();
        session
            .prepare(&format!(
                "create function {input}(cstring) returns {typ} strict immutable \
                 language internal as 'int4in'"
            ))
            .unwrap()
            .execute()
            .unwrap();
        session
            .prepare(&format!(
                "create function {output}({typ}) returns cstring strict immutable \
                 language internal as 'int4out'"
            ))
            .unwrap()
            .execute()
            .unwrap();
        session
            .prepare(&format!(
                "create function {hash}({typ}) returns integer strict immutable \
                 language internal as 'hashint4'"
            ))
            .unwrap()
            .execute()
            .unwrap();
        session
            .prepare(&format!(
                "create type {typ} (input = {input}, output = {output}, like = int4)"
            ))
            .unwrap()
            .execute()
            .unwrap();
        session
            .prepare(&format!("create cast (int4 as {typ}) without function"))
            .unwrap()
            .execute()
            .unwrap();
        session
            .prepare(&format!("create cast ({typ} as int4) without function"))
            .unwrap()
            .execute()
            .unwrap();
        session
            .prepare(&format!(
                "create function {eq}({typ}, {typ}) returns bool strict immutable \
                 language internal as 'int4eq'"
            ))
            .unwrap()
            .execute()
            .unwrap();

        let operator = session
            .prepare(&format!(
                "create operator = (
                    leftarg = {typ},
                    rightarg = {typ},
                    procedure = {eq},
                    restrict = eqsel,
                    join = eqjoinsel,
                    hashes
                )"
            ))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(operator.statements[0].command_tag, "CREATE OPERATOR");

        let opclass_result = session
            .prepare(&format!(
                "create operator class {opclass} default for type {typ} using hash as \
                 operator 1 = ({typ}, {typ}), function 1 {hash}({typ})"
            ))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(
            opclass_result.statements[0].command_tag,
            "CREATE OPERATOR CLASS"
        );

        let type_metadata = session
            .prepare(&format!(
                "select typinput::oid, typoutput::oid, typisdefined::text, typlen::text, \
                 typbyval::text from pg_type where oid = '{typ}'::regtype"
            ))
            .unwrap()
            .execute()
            .unwrap()
            .statements[0]
            .rows
            .clone();
        assert_eq!(type_metadata.len(), 1);
        assert_ne!(type_metadata[0][0], PgCoreValue::Text("0".to_owned()));
        assert_ne!(type_metadata[0][1], PgCoreValue::Text("0".to_owned()));
        assert_eq!(type_metadata[0][2], PgCoreValue::Text("true".to_owned()));
        assert_eq!(type_metadata[0][3], PgCoreValue::Text("4".to_owned()));
        assert_eq!(type_metadata[0][4], PgCoreValue::Text("true".to_owned()));

        let cast_metadata = session
            .prepare(&format!(
                "select castfunc::oid, castcontext::text, castmethod::text from pg_cast \
                 where castsource = 'int4'::regtype and casttarget = '{typ}'::regtype"
            ))
            .unwrap()
            .execute()
            .unwrap()
            .statements[0]
            .rows
            .clone();
        assert_eq!(
            cast_metadata,
            vec![vec![
                PgCoreValue::Text("0".to_owned()),
                PgCoreValue::Text("e".to_owned()),
                PgCoreValue::Text("b".to_owned())
            ]]
        );

        session
            .prepare(&format!("create table {table}(a {typ})"))
            .unwrap()
            .execute()
            .unwrap();
        session
            .prepare(&format!(
                "insert into {table} values (null), (0::{typ}), (1::{typ})"
            ))
            .unwrap()
            .execute()
            .unwrap();

        let select = session
            .prepare(&format!(
                "select a::int4, a in (1::{typ}, 2::{typ}, 3::{typ}, 4::{typ}, \
                 5::{typ}, 6::{typ}, 7::{typ}, 8::{typ}, 9::{typ}) \
                 from {table} order by a::int4 nulls first"
            ))
            .unwrap()
            .execute()
            .unwrap();
        assert_eq!(
            select.statements[0].rows,
            vec![
                vec![PgCoreValue::Null, PgCoreValue::Null],
                vec![
                    PgCoreValue::Text("0".to_owned()),
                    PgCoreValue::Text("f".to_owned())
                ],
                vec![
                    PgCoreValue::Text("1".to_owned()),
                    PgCoreValue::Text("t".to_owned())
                ],
            ]
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
        let copy_in = copy.statements[0].copy_in.as_ref().unwrap();
        assert_eq!(copy_in.table, table);
        assert_eq!(copy_in.columns, 2);

        session
            .prepare(&format!("drop table if exists {table}"))
            .unwrap()
            .execute()
            .unwrap();
    }
}
