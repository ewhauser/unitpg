#![forbid(unsafe_code)]

use std::fmt::Debug;
use std::io;
use std::num::NonZeroUsize;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::{BufMut, BytesMut};
use fastpg_session::{
    Column, PgType, QueryDescription, QueryExecution, QueryResult, ServerState, SessionState,
    StartupParameters, Value,
};
use futures::{Sink, SinkExt, stream};
use pgwire::api::auth::StartupHandler;
use pgwire::api::auth::noop::NoopStartupHandler;
use pgwire::api::copy::CopyHandler;
use pgwire::api::portal::{Format, Portal};
use pgwire::api::query::{ExtendedQueryHandler, SimpleQueryHandler};
use pgwire::api::results::{
    CopyResponse, DataRowEncoder, DescribePortalResponse, DescribeStatementResponse, FieldFormat,
    FieldInfo, QueryResponse, Response, Tag,
};
use pgwire::api::stmt::{NoopQueryParser, StoredStatement};
use pgwire::api::{ClientInfo, ClientPortalStore, PgWireServerHandlers, Type};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use pgwire::messages::copy::{CopyData, CopyDone, CopyFail};
use pgwire::messages::data::DataRow;
use pgwire::messages::{PgWireBackendMessage, PgWireFrontendMessage};
use postgres_types::Kind;
use tokio::sync::Semaphore;

#[derive(Debug)]
pub struct FastPgServerHandlers {
    handler: Arc<FastPgWireHandler>,
}

impl FastPgServerHandlers {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_server_version(server_version: impl Into<String>) -> Self {
        Self {
            handler: Arc::new(FastPgWireHandler::new(server_version)),
        }
    }

    pub fn with_server_state(server: Arc<ServerState>) -> Self {
        Self {
            handler: Arc::new(FastPgWireHandler::with_server_state(server)),
        }
    }

    pub fn with_execution_concurrency(max_concurrency: NonZeroUsize) -> Self {
        Self {
            handler: Arc::new(
                FastPgWireHandler::with_server_version_and_execution_concurrency(
                    fastpg_compat::DEFAULT_SERVER_VERSION,
                    max_concurrency,
                ),
            ),
        }
    }

    pub fn with_server_version_and_execution_concurrency(
        server_version: impl Into<String>,
        max_concurrency: NonZeroUsize,
    ) -> Self {
        Self {
            handler: Arc::new(
                FastPgWireHandler::with_server_version_and_execution_concurrency(
                    server_version,
                    max_concurrency,
                ),
            ),
        }
    }

    pub fn with_server_state_and_execution_concurrency(
        server: Arc<ServerState>,
        max_concurrency: NonZeroUsize,
    ) -> Self {
        Self {
            handler: Arc::new(
                FastPgWireHandler::with_server_state_and_execution_concurrency(
                    server,
                    max_concurrency,
                ),
            ),
        }
    }
}

impl Default for FastPgServerHandlers {
    fn default() -> Self {
        Self {
            handler: Arc::new(FastPgWireHandler::default()),
        }
    }
}

impl PgWireServerHandlers for FastPgServerHandlers {
    fn simple_query_handler(&self) -> Arc<impl SimpleQueryHandler> {
        self.handler.clone()
    }

    fn extended_query_handler(&self) -> Arc<impl ExtendedQueryHandler> {
        self.handler.clone()
    }

    fn startup_handler(&self) -> Arc<impl StartupHandler> {
        self.handler.clone()
    }

    fn copy_handler(&self) -> Arc<impl CopyHandler> {
        self.handler.clone()
    }
}

#[derive(Debug)]
pub struct FastPgWireHandler {
    server: Arc<ServerState>,
    query_parser: Arc<NoopQueryParser>,
    execution: ExecutionDispatcher,
}

impl FastPgWireHandler {
    pub fn new(server_version: impl Into<String>) -> Self {
        Self::with_server_version_and_execution_concurrency(
            server_version,
            default_execution_concurrency(),
        )
    }

    pub fn with_server_state(server: Arc<ServerState>) -> Self {
        Self::with_server_state_and_execution_concurrency(server, default_execution_concurrency())
    }

    pub fn with_server_version_and_execution_concurrency(
        server_version: impl Into<String>,
        max_concurrency: NonZeroUsize,
    ) -> Self {
        Self::with_server_state_and_execution_concurrency(
            Arc::new(ServerState::new(server_version)),
            max_concurrency,
        )
    }

    pub fn with_server_state_and_execution_concurrency(
        server: Arc<ServerState>,
        max_concurrency: NonZeroUsize,
    ) -> Self {
        Self {
            server,
            query_parser: Arc::new(NoopQueryParser::new()),
            execution: ExecutionDispatcher::new(max_concurrency),
        }
    }

    async fn describe_query(
        &self,
        session: Arc<SessionState>,
        query: String,
    ) -> PgWireResult<Option<QueryDescription>> {
        self.execution
            .run_blocking(move || session.describe(&query))
            .await
    }

    async fn execute_query(
        &self,
        session: Arc<SessionState>,
        query: String,
        parameters: Vec<Value>,
    ) -> PgWireResult<QueryExecution> {
        self.execution
            .run_blocking(move || session.execute(&query, &parameters))
            .await
    }

    async fn push_copy_data(&self, session: Arc<SessionState>, data: Vec<u8>) -> PgWireResult<()> {
        self.execution
            .run_blocking(move || session.push_copy_data(&data))
            .await?
            .map_err(copy_data_error)
    }

    async fn finish_copy(&self, session: Arc<SessionState>) -> PgWireResult<usize> {
        self.execution
            .run_blocking(move || session.finish_active_copy())
            .await?
            .map_err(copy_data_error)
    }

    async fn abort_copy(&self, session: Arc<SessionState>) -> PgWireResult<()> {
        self.execution
            .run_blocking(move || session.abort_active_copy())
            .await
    }
}

impl Default for FastPgWireHandler {
    fn default() -> Self {
        Self::new(fastpg_compat::DEFAULT_SERVER_VERSION)
    }
}

#[async_trait]
impl NoopStartupHandler for FastPgWireHandler {
    async fn post_startup<C>(
        &self,
        client: &mut C,
        message: PgWireFrontendMessage,
    ) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        client.session_extensions().insert(SessionState::new(
            self.server.clone(),
            startup_parameters(client, &message),
        ));
        Ok(())
    }
}

#[async_trait]
impl SimpleQueryHandler for FastPgWireHandler {
    async fn do_query<C>(&self, client: &mut C, query: &str) -> PgWireResult<Vec<Response>>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let session = session_for_client(client)?;
        let execution = self
            .execute_query(session.clone(), query.to_owned(), vec![])
            .await?;
        remember_copy_target(&session, &execution);
        Ok(vec![execution_to_response(execution, FieldFormat::Text)?])
    }
}

#[async_trait]
impl ExtendedQueryHandler for FastPgWireHandler {
    type Statement = String;
    type QueryParser = NoopQueryParser;

    fn query_parser(&self) -> Arc<Self::QueryParser> {
        self.query_parser.clone()
    }

    async fn do_describe_statement<C>(
        &self,
        client: &mut C,
        target: &StoredStatement<Self::Statement>,
    ) -> PgWireResult<DescribeStatementResponse>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let session = session_for_client(client)?;
        let Some(description) = self
            .describe_query(session, target.statement.clone())
            .await?
        else {
            return Ok(DescribeStatementResponse::new(vec![], vec![]));
        };

        Ok(DescribeStatementResponse::new(
            parameter_types(&description),
            field_infos(&description.fields, FieldFormat::Text),
        ))
    }

    async fn do_describe_portal<C>(
        &self,
        client: &mut C,
        target: &Portal<Self::Statement>,
    ) -> PgWireResult<DescribePortalResponse>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let session = session_for_client(client)?;
        let Some(description) = self
            .describe_query(session, target.statement.statement.clone())
            .await?
        else {
            return Ok(DescribePortalResponse::new(vec![]));
        };

        Ok(DescribePortalResponse::new(portal_field_infos(
            &description.fields,
            &target.result_column_format,
        )))
    }

    async fn do_query<C>(
        &self,
        client: &mut C,
        portal: &Portal<Self::Statement>,
        _max_rows: usize,
    ) -> PgWireResult<Response>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let session = session_for_client(client)?;
        let query = portal.statement.statement.clone();
        let parameters = match self.describe_query(session.clone(), query.clone()).await? {
            Some(description) => decode_parameters(portal, &description)?,
            None => vec![],
        };
        let execution = self
            .execute_query(session.clone(), query, parameters)
            .await?;
        remember_copy_target(&session, &execution);

        execution_to_response(execution, portal.result_column_format.format_for(0))
    }
}

#[async_trait]
impl CopyHandler for FastPgWireHandler {
    async fn on_copy_data<C>(&self, client: &mut C, copy_data: CopyData) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let session = session_for_client(client)?;
        self.push_copy_data(session, copy_data.data.as_ref().to_vec())
            .await?;
        Ok(())
    }

    async fn on_copy_done<C>(&self, client: &mut C, _done: CopyDone) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let session = session_for_client(client)?;
        let rows = self.finish_copy(session).await?;

        client
            .send(PgWireBackendMessage::CommandComplete(
                Tag::new("COPY").with_rows(rows).into(),
            ))
            .await?;
        Ok(())
    }

    async fn on_copy_fail<C>(&self, client: &mut C, fail: CopyFail) -> PgWireError
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let session = match session_for_client(client) {
            Ok(session) => session,
            Err(error) => return error,
        };
        if let Err(error) = self.abort_copy(session).await {
            return error;
        }
        PgWireError::UserError(Box::new(ErrorInfo::new(
            "ERROR".to_owned(),
            "57014".to_owned(),
            format!("COPY cancelled by client: {}", fail.message),
        )))
    }
}

fn default_execution_concurrency() -> NonZeroUsize {
    std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN)
}

#[derive(Clone, Debug)]
struct ExecutionDispatcher {
    permits: Arc<Semaphore>,
}

impl ExecutionDispatcher {
    fn new(max_concurrency: NonZeroUsize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(max_concurrency.get())),
        }
    }

    async fn run_blocking<R>(
        &self,
        operation: impl FnOnce() -> R + Send + 'static,
    ) -> PgWireResult<R>
    where
        R: Send + 'static,
    {
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(api_io_error)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            operation()
        })
        .await
        .map_err(api_io_error)
    }
}

fn api_io_error(error: impl std::error::Error + Send + Sync + 'static) -> PgWireError {
    PgWireError::ApiError(Box::new(io::Error::other(error)))
}

fn session_for_client<C>(client: &C) -> PgWireResult<Arc<SessionState>>
where
    C: ClientInfo,
{
    client
        .session_extensions()
        .get::<SessionState>()
        .ok_or_else(missing_session_error)
}

fn startup_parameters<C>(client: &C, message: &PgWireFrontendMessage) -> StartupParameters
where
    C: ClientInfo,
{
    match message {
        PgWireFrontendMessage::Startup(startup) => {
            StartupParameters::from(startup.parameters.clone())
        }
        _ => StartupParameters::from(
            client
                .metadata()
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<std::collections::BTreeMap<_, _>>(),
        ),
    }
}

fn missing_session_error() -> PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new(
        "ERROR".to_owned(),
        "08P01".to_owned(),
        "fastpg session state is missing for client".to_owned(),
    )))
}

fn parameter_types(description: &QueryDescription) -> Vec<Type> {
    description
        .parameter_type_oids
        .iter()
        .copied()
        .zip(description.parameter_types.iter().copied())
        .map(|(type_oid, fallback)| pgwire_type_for_oid(type_oid, fallback))
        .collect()
}

fn field_infos(fields: &[Column], format: FieldFormat) -> Vec<FieldInfo> {
    fields
        .iter()
        .map(|field| field_info(field, format))
        .collect()
}

fn portal_field_infos(fields: &[Column], format: &Format) -> Vec<FieldInfo> {
    fields
        .iter()
        .enumerate()
        .map(|(idx, field)| field_info(field, format.format_for(idx)))
        .collect()
}

fn field_info(field: &Column, format: FieldFormat) -> FieldInfo {
    FieldInfo::new(
        field.name.clone(),
        None,
        None,
        pgwire_type_for_oid(field.type_oid, field.data_type),
        format,
    )
}

fn pgwire_type_for_oid(type_oid: u32, fallback: PgType) -> Type {
    Type::from_oid(type_oid).unwrap_or_else(|| {
        if type_oid == 0 {
            pgwire_type_for_pg_type(fallback)
        } else {
            Type::new(
                format!("fastpg_type_{type_oid}"),
                type_oid,
                Kind::Simple,
                "pg_catalog".to_owned(),
            )
        }
    })
}

fn pgwire_type_for_pg_type(data_type: PgType) -> Type {
    match data_type {
        PgType::Int2 => Type::INT2,
        PgType::Int4 => Type::INT4,
        PgType::Int8 => Type::INT8,
        PgType::Varchar => Type::VARCHAR,
    }
}

fn decode_parameters(
    portal: &Portal<String>,
    description: &QueryDescription,
) -> PgWireResult<Vec<Value>> {
    description
        .parameter_types
        .iter()
        .enumerate()
        .map(|(idx, data_type)| match data_type {
            PgType::Int2 => portal
                .parameter::<i16>(idx, &Type::INT2)
                .map(|value| value.map(Value::Int2).unwrap_or(Value::Null)),
            PgType::Int4 => portal
                .parameter::<i32>(idx, &Type::INT4)
                .map(|value| value.map(Value::Int4).unwrap_or(Value::Null)),
            PgType::Int8 => portal
                .parameter::<i64>(idx, &Type::INT8)
                .map(|value| value.map(Value::Int8).unwrap_or(Value::Null)),
            PgType::Varchar => portal
                .parameter::<String>(idx, &Type::VARCHAR)
                .map(|value| value.map(Value::Text).unwrap_or(Value::Null)),
        })
        .collect()
}

fn execution_to_response(execution: QueryExecution, format: FieldFormat) -> PgWireResult<Response> {
    match execution {
        QueryExecution::Empty => Ok(Response::EmptyQuery),
        QueryExecution::Rows(result) => query_result_response(result, format),
        QueryExecution::Command { tag, rows } => Ok(command_response(tag.as_ref(), rows)),
        QueryExecution::CopyIn(target) => Ok(Response::CopyIn(CopyResponse::new(
            0,
            target.columns,
            stream::empty(),
        ))),
        QueryExecution::Unsupported { query } => Ok(unsupported_response(&query)),
        QueryExecution::InvalidParameters { message } => Ok(invalid_parameter_response(&message)),
        QueryExecution::Error {
            sqlstate,
            message,
            detail,
            hint,
            cursorpos,
        } => Ok(error_response(&sqlstate, &message, detail, hint, cursorpos)),
    }
}

fn query_result_response(result: QueryResult, format: FieldFormat) -> PgWireResult<Response> {
    let schema = Arc::new(field_infos(&result.fields, format));
    let fields = result.fields;
    let row_schema = schema.clone();
    let rows = result
        .rows
        .into_iter()
        .map(move |row| encode_row(row_schema.clone(), &fields, &row));

    Ok(Response::Query(QueryResponse::new(
        schema,
        stream::iter(rows),
    )))
}

fn encode_row(
    schema: Arc<Vec<FieldInfo>>,
    fields: &[Column],
    values: &[Value],
) -> PgWireResult<DataRow> {
    if schema
        .iter()
        .all(|field| field.format() == FieldFormat::Text)
    {
        let mut row = BytesMut::with_capacity(128);
        for value in values {
            encode_text_field(&mut row, value);
        }
        return Ok(DataRow::new(row, values.len() as i16));
    }

    let mut encoder = DataRowEncoder::new(schema);
    for (field, value) in fields.iter().zip(values) {
        encode_value(&mut encoder, field.data_type, value)?;
    }
    Ok(encoder.take_row())
}

fn encode_text_field(row: &mut BytesMut, value: &Value) {
    match value {
        Value::Null => row.put_i32(-1),
        Value::Int2(value) => encode_text_bytes(row, value.to_string().as_bytes()),
        Value::Int4(value) => encode_text_bytes(row, value.to_string().as_bytes()),
        Value::Int8(value) => encode_text_bytes(row, value.to_string().as_bytes()),
        Value::Text(value) => encode_text_bytes(row, value.as_bytes()),
    }
}

fn encode_text_bytes(row: &mut BytesMut, bytes: &[u8]) {
    row.put_i32(bytes.len() as i32);
    row.put_slice(bytes);
}

fn encode_value(
    encoder: &mut DataRowEncoder,
    data_type: PgType,
    value: &Value,
) -> PgWireResult<()> {
    match (data_type, value) {
        (PgType::Int2, Value::Int2(value)) => encoder.encode_field(&Some(*value)),
        (PgType::Int4, Value::Int4(value)) => encoder.encode_field(&Some(*value)),
        (PgType::Int8, Value::Int8(value)) => encoder.encode_field(&Some(*value)),
        (PgType::Varchar, Value::Text(value)) => encoder.encode_field(&Some(value.as_str())),
        (PgType::Int2, Value::Null) => encoder.encode_field(&Option::<i16>::None),
        (PgType::Int4, Value::Null) => encoder.encode_field(&Option::<i32>::None),
        (PgType::Int8, Value::Null) => encoder.encode_field(&Option::<i64>::None),
        (PgType::Varchar, Value::Null) => encoder.encode_field(&Option::<&str>::None),
        (expected, actual) => Err(PgWireError::ApiError(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("cannot encode {actual:?} as {expected:?}"),
        )))),
    }
}

fn unsupported_response(query: &str) -> Response {
    Response::Error(Box::new(ErrorInfo::new(
        "ERROR".to_owned(),
        "0A000".to_owned(),
        format!("feature not supported by fastpg test server yet: {query}"),
    )))
}

fn invalid_parameter_response(message: &str) -> Response {
    Response::Error(Box::new(ErrorInfo::new(
        "ERROR".to_owned(),
        "08P01".to_owned(),
        message.to_owned(),
    )))
}

fn error_response(
    sqlstate: &str,
    message: &str,
    detail: Option<String>,
    hint: Option<String>,
    cursorpos: i32,
) -> Response {
    let mut error = ErrorInfo::new("ERROR".to_owned(), sqlstate.to_owned(), message.to_owned());
    error.detail = detail;
    error.hint = hint;
    if cursorpos > 0 {
        error.position = Some(cursorpos.to_string());
    }
    Response::Error(Box::new(error))
}

fn command_response(tag: &str, rows: usize) -> Response {
    match tag.split_whitespace().next() {
        Some(command) if command.eq_ignore_ascii_case("BEGIN") => {
            Response::TransactionStart(Tag::new(tag))
        }
        Some(command)
            if command.eq_ignore_ascii_case("COMMIT")
                || command.eq_ignore_ascii_case("END")
                || command.eq_ignore_ascii_case("ROLLBACK") =>
        {
            Response::TransactionEnd(Tag::new(tag))
        }
        _ => command_complete(tag, rows),
    }
}

pub fn command_complete(tag: &str, rows: usize) -> Response {
    let tag = if tag == "INSERT" {
        Tag::new(tag).with_oid(0).with_rows(rows)
    } else {
        Tag::new(tag).with_rows(rows)
    };
    Response::Execution(tag)
}

fn remember_copy_target(session: &SessionState, execution: &QueryExecution) {
    if let QueryExecution::CopyIn(target) = execution {
        session.begin_copy(target.clone());
    }
}

fn copy_data_error(message: String) -> PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new(
        "ERROR".to_owned(),
        "22P04".to_owned(),
        message,
    )))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use pgwire::messages::response::CommandComplete;

    use super::*;

    #[test]
    fn insert_command_complete_includes_legacy_oid_field() {
        assert_eq!(
            command_complete_tag(command_complete("INSERT", 3)),
            "INSERT 0 3"
        );
    }

    #[test]
    fn non_insert_command_complete_keeps_row_count_shape() {
        assert_eq!(
            command_complete_tag(command_complete("UPDATE", 2)),
            "UPDATE 2"
        );
    }

    #[test]
    fn transaction_commands_drive_pgwire_transaction_status() {
        assert!(matches!(
            command_response("BEGIN", 0),
            Response::TransactionStart(_)
        ));
        assert!(matches!(
            command_response("COMMIT", 0),
            Response::TransactionEnd(_)
        ));
        assert!(matches!(
            command_response("ROLLBACK", 0),
            Response::TransactionEnd(_)
        ));
    }

    fn command_complete_tag(response: Response) -> String {
        let Response::Execution(tag) = response else {
            panic!("expected execution response");
        };
        let command_complete = CommandComplete::from(tag);
        command_complete.tag
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn execution_dispatcher_caps_concurrent_blocking_work() {
        let dispatcher = ExecutionDispatcher::new(NonZeroUsize::new(2).unwrap());
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();

        for _ in 0..8 {
            let dispatcher = dispatcher.clone();
            let active = active.clone();
            let max_active = max_active.clone();
            tasks.push(tokio::spawn(async move {
                dispatcher
                    .run_blocking(move || {
                        let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                        max_active.fetch_max(current, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(20));
                        active.fetch_sub(1, Ordering::SeqCst);
                    })
                    .await
                    .unwrap();
            }));
        }

        for task in tasks {
            task.await.unwrap();
        }

        assert_eq!(max_active.load(Ordering::SeqCst), 2);
    }
}
