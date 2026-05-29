#![forbid(unsafe_code)]

use std::any::Any;
use std::ffi::CStr;
use std::fmt::Debug;
use std::fmt::Write;
use std::io;
#[cfg(unix)]
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
#[cfg(unix)]
use std::pin::Pin;
use std::sync::Arc;
use std::sync::LazyLock;
#[cfg(unix)]
use std::task::{Context, Poll};

use async_trait::async_trait;
#[cfg(unix)]
use bytes::Buf;
use bytes::{BufMut, Bytes, BytesMut};
use fastpg_session::{
    COPY_ERROR_CONTEXT_PREFIX, Column, PgType, QueryDescription, QueryExecution, QueryNotice,
    QueryResult, ServerState, SessionState, StartupParameters, Value,
};
#[cfg(unix)]
use futures::Stream;
use futures::channel::oneshot as futures_oneshot;
use futures::future::{Either, select};
use futures::{Sink, SinkExt, StreamExt, stream};
#[cfg(unix)]
use pgwire::api::DefaultClient;
use pgwire::api::auth::{DefaultServerParameterProvider, StartupHandler};
use pgwire::api::copy::{
    CopyHandler, send_copy_both_response, send_copy_in_response, send_copy_out_response,
};
use pgwire::api::portal::{Format, Portal};
#[cfg(unix)]
use pgwire::api::query::send_ready_for_query;
use pgwire::api::query::{
    ExtendedQueryHandler, SimpleQueryHandler, send_execution_response, send_query_response,
};
use pgwire::api::results::{
    CopyResponse, DataRowEncoder, DescribePortalResponse, DescribeStatementResponse, FieldFormat,
    FieldInfo, QueryResponse, Response, Tag,
};
use pgwire::api::stmt::{NoopQueryParser, StoredStatement};
use pgwire::api::store::PortalStore;
use pgwire::api::{ClientInfo, ClientPortalStore, ConnectionHandle, PgWireServerHandlers, Type};
use pgwire::api::{PgWireConnectionState, PidSecretKeyGenerator, RandomPidSecretKeyGenerator};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
#[cfg(unix)]
use pgwire::messages::DecodeContext;
use pgwire::messages::ProtocolVersion;
use pgwire::messages::copy::{CopyData, CopyDone, CopyFail};
use pgwire::messages::data::{DataRow, FieldDescription, RowDescription};
use pgwire::messages::response::{
    CommandComplete, EmptyQueryResponse, NoticeResponse, ReadyForQuery, TransactionStatus,
};
use pgwire::messages::simplequery::Query;
use pgwire::messages::startup::{NegotiateProtocolVersion, Startup};
use pgwire::messages::{PgWireBackendMessage, PgWireFrontendMessage};
use postgres_types::Kind;
#[cfg(unix)]
use tokio::io::AsyncWriteExt;
#[cfg(unix)]
use tokio::net::UnixStream;
#[cfg(unix)]
use tokio::sync::{Semaphore, TryAcquireError, oneshot};
#[cfg(unix)]
use tokio_util::codec::{Decoder, Encoder, Framed};

#[derive(Debug)]
pub struct FastPgServerHandlers {
    handler: Arc<FastPgWireHandler>,
}

static PID_GENERATOR: LazyLock<RandomPidSecretKeyGenerator> =
    LazyLock::new(RandomPidSecretKeyGenerator::default);
const COPY_ERROR_FIELDS_PREFIX: &str = "\x1ffastpg-copy-error-fields\n";

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

    pub fn with_inline_session_execution(max_concurrency: NonZeroUsize) -> Self {
        Self {
            handler: Arc::new(
                FastPgWireHandler::with_server_version_execution_concurrency_and_mode(
                    fastpg_compat::DEFAULT_SERVER_VERSION,
                    max_concurrency,
                    true,
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
    inline_session_execution: bool,
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
        Self::with_server_version_execution_concurrency_and_mode(
            server_version,
            max_concurrency,
            false,
        )
    }

    fn with_server_version_execution_concurrency_and_mode(
        server_version: impl Into<String>,
        max_concurrency: NonZeroUsize,
        inline_session_execution: bool,
    ) -> Self {
        Self::with_server_state_execution_concurrency_and_mode(
            Arc::new(ServerState::new(server_version)),
            max_concurrency,
            inline_session_execution,
        )
    }

    pub fn with_server_state_and_execution_concurrency(
        server: Arc<ServerState>,
        max_concurrency: NonZeroUsize,
    ) -> Self {
        Self::with_server_state_execution_concurrency_and_mode(server, max_concurrency, false)
    }

    fn with_server_state_execution_concurrency_and_mode(
        server: Arc<ServerState>,
        max_concurrency: NonZeroUsize,
        inline_session_execution: bool,
    ) -> Self {
        Self {
            server,
            query_parser: Arc::new(NoopQueryParser::new()),
            execution: ExecutionDispatcher::new(max_concurrency, inline_session_execution),
            inline_session_execution,
        }
    }

    async fn describe_query(
        &self,
        session: Arc<SessionState>,
        query: String,
    ) -> PgWireResult<Option<QueryDescription>> {
        self.execution
            .run_session_blocking(session, move |session| session.describe(&query))
            .await
    }

    async fn execute_query(
        &self,
        session: Arc<SessionState>,
        query: String,
        parameters: Vec<Value>,
    ) -> PgWireResult<(QueryExecution, Vec<QueryNotice>)> {
        self.execution
            .run_session_blocking(session, move |session| {
                let execution = session.execute(&query, &parameters);
                let notices = session.take_notices();
                (execution, notices)
            })
            .await
    }

    async fn execute_simple_query(
        &self,
        session: Arc<SessionState>,
        query: String,
    ) -> PgWireResult<(QueryExecution, Vec<QueryNotice>)> {
        self.execution
            .run_session_blocking(session, move |session| {
                let execution = session.execute_simple_text(&query);
                let notices = session.take_notices();
                (execution, notices)
            })
            .await
    }

    fn try_execute_simple_query_inline(
        &self,
        session: Arc<SessionState>,
        query: &str,
    ) -> Option<PgWireResult<(QueryExecution, Vec<QueryNotice>)>> {
        self.execution.try_run_session_inline(session, |session| {
            let execution = session.execute_simple_text(query);
            let notices = session.take_notices();
            (execution, notices)
        })
    }

    #[cfg(feature = "postgres-execution")]
    fn try_execute_simple_query_cstr_inline(
        &self,
        session: Arc<SessionState>,
        query: &CStr,
    ) -> Option<PgWireResult<(QueryExecution, Vec<QueryNotice>)>> {
        self.execution.try_run_session_inline(session, |session| {
            let execution = session.execute_simple_cstr(query);
            let notices = session.take_notices();
            (execution, notices)
        })
    }

    async fn execute_simple_message<C>(
        &self,
        client: &mut C,
        query: String,
    ) -> PgWireResult<QueryExecution>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let session = session_for_client(client)?;
        if postgres_catalog_enabled() && !simple_query_may_copy_from_stdin(&query) {
            let (execution, notices) = self.execute_simple_query(session.clone(), query).await?;
            send_notices(client, notices).await?;
            remember_copy_target(&session, &execution);
            return Ok(execution);
        }

        let statements = split_simple_query_statements(&query);
        if statements.len() > 1 && should_split_simple_query(&statements) {
            let mut executions = Vec::with_capacity(statements.len());
            for (statement_index, statement) in statements.iter().enumerate() {
                let (execution, notices) = self
                    .execute_simple_query(session.clone(), (*statement).to_owned())
                    .await?;
                send_notices(client, notices).await?;
                let enters_copy = execution_enters_copy(&execution);
                let stop_batch = enters_copy || execution_is_error(&execution);
                if enters_copy {
                    session.set_pending_simple_statements(
                        statements[statement_index + 1..]
                            .iter()
                            .map(|statement| (*statement).to_owned())
                            .collect(),
                    );
                }
                remember_copy_target(&session, &execution);
                executions.push(execution);
                if stop_batch {
                    break;
                }
            }
            return Ok(QueryExecution::Batch(executions));
        }

        let (execution, notices) = self.execute_simple_query(session.clone(), query).await?;
        send_notices(client, notices).await?;
        remember_copy_target(&session, &execution);
        Ok(execution)
    }

    async fn push_copy_data(&self, session: Arc<SessionState>, data: Vec<u8>) -> PgWireResult<()> {
        self.execution
            .run_session_blocking(session, move |session| session.push_copy_data(&data))
            .await?
            .map_err(copy_data_error)
    }

    async fn finish_copy(
        &self,
        session: Arc<SessionState>,
    ) -> PgWireResult<(Result<usize, String>, Vec<QueryNotice>)> {
        self.execution
            .run_session_blocking(session, move |session| {
                let result = session.finish_active_copy();
                let notices = session.take_notices();
                (result, notices)
            })
            .await
    }

    async fn drain_pending_simple<C>(
        &self,
        client: &mut C,
        session: Arc<SessionState>,
    ) -> PgWireResult<bool>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let mut pending = session.take_pending_simple_statements();
        while let Some(statement) = pending.pop_front() {
            let (execution, notices) = self
                .execute_query(session.clone(), statement, vec![])
                .await?;
            send_notices(client, notices).await?;
            let enters_copy = execution_enters_copy(&execution);
            let stops_batch = enters_copy || execution_is_error(&execution);
            remember_copy_target(&session, &execution);
            for response in execution_to_responses(execution, FieldFormat::Text)? {
                send_response(client, response).await?;
            }
            if enters_copy {
                session.set_pending_simple_statements(pending.into_iter().collect());
                return Ok(true);
            }
            if stops_batch {
                return Ok(false);
            }
        }
        Ok(false)
    }

    async fn abort_copy(&self, session: Arc<SessionState>) -> PgWireResult<()> {
        self.execution
            .run_session_blocking(session, move |session| session.abort_active_copy())
            .await
    }
}

async fn send_notices<C>(client: &mut C, notices: Vec<QueryNotice>) -> PgWireResult<()>
where
    C: Sink<PgWireBackendMessage> + Unpin,
    C::Error: Debug,
    PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
{
    for notice in notices {
        let mut error = ErrorInfo::new(notice.severity.clone(), notice.sqlstate, notice.message);
        error.severity_nonlocalized = Some(notice.severity);
        error.detail = notice.detail;
        error.hint = notice.hint;
        error.where_context = notice.context;
        if notice.cursorpos > 0 {
            error.position = Some(notice.cursorpos.to_string());
        }
        client
            .send(PgWireBackendMessage::NoticeResponse(NoticeResponse::from(
                error,
            )))
            .await?;
    }
    Ok(())
}

async fn send_response<C>(client: &mut C, response: Response) -> PgWireResult<()>
where
    C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
    C::Error: Debug,
    PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
{
    match response {
        Response::EmptyQuery => {
            client
                .send(PgWireBackendMessage::EmptyQueryResponse(
                    EmptyQueryResponse::new(),
                ))
                .await?;
        }
        Response::Query(results) => {
            send_query_response(client, results, true).await?;
        }
        Response::Execution(tag)
        | Response::TransactionStart(tag)
        | Response::TransactionEnd(tag) => {
            send_execution_response(client, tag).await?;
        }
        Response::Error(error) => {
            client
                .send(PgWireBackendMessage::ErrorResponse((*error).into()))
                .await?;
        }
        Response::CopyIn(result) => {
            send_copy_in_response(client, result).await?;
            client.set_state(PgWireConnectionState::CopyInProgress(false));
        }
        Response::CopyOut(result) => {
            send_copy_out_response(client, result).await?;
        }
        Response::CopyBoth(result) => {
            send_copy_both_response(client, result).await?;
            client.set_state(PgWireConnectionState::CopyInProgress(false));
        }
    }
    Ok(())
}

fn simple_query_is_empty(query: &str) -> bool {
    if query.as_bytes().iter().any(|byte| !byte.is_ascii()) {
        let trimmed = query.trim();
        return trimmed.is_empty() || trimmed == ";";
    }

    let bytes = query.as_bytes();
    let mut start = 0;
    let mut end = bytes.len();
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let trimmed = &bytes[start..end];
    trimmed.is_empty() || trimmed == b";"
}

async fn cancel_receiver<C>(client: &mut C) -> Option<futures_oneshot::Receiver<()>>
where
    C: ClientInfo + ClientPortalStore + Unpin + Send + Sync,
{
    let handle = client.session_extensions().get::<Arc<ConnectionHandle>>()?;
    Some(handle.start_query().await)
}

fn row_description(fields: &[FieldInfo]) -> RowDescription {
    RowDescription::new(
        fields
            .iter()
            .map(|field| {
                FieldDescription::new(
                    field.name().to_owned(),
                    field.table_id().unwrap_or(0),
                    field.column_id().unwrap_or(0),
                    field.datatype().oid(),
                    0,
                    0,
                    field.format().value(),
                )
            })
            .collect(),
    )
}

async fn feed_simple_execution<C>(
    client: &mut C,
    execution: QueryExecution,
    transaction_status: &mut TransactionStatus,
) -> PgWireResult<()>
where
    C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
    C::Error: Debug,
    PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
{
    let mut current = Some(execution);
    let mut pending = Vec::new();
    while let Some(execution) = current.take().or_else(|| pending.pop()) {
        match execution {
            QueryExecution::WithNotices { notices, execution } => {
                send_notices(client, notices).await?;
                current = Some(*execution);
            }
            QueryExecution::Empty => {
                client
                    .feed(PgWireBackendMessage::EmptyQueryResponse(
                        EmptyQueryResponse::new(),
                    ))
                    .await?;
            }
            QueryExecution::Batch(executions) => {
                pending.extend(executions.into_iter().rev());
            }
            QueryExecution::Rows(result) => {
                feed_query_result(client, result, FieldFormat::Text, true).await?;
            }
            QueryExecution::Command { tag, rows } => {
                feed_command_complete(client, tag.as_ref(), rows, transaction_status).await?;
            }
            QueryExecution::CopyIn(target) => {
                send_copy_in_response(
                    client,
                    CopyResponse::new(0, target.columns, stream::empty()),
                )
                .await?;
                client.set_state(PgWireConnectionState::CopyInProgress(false));
            }
            QueryExecution::CopyOut(output) => {
                send_copy_out_response(
                    client,
                    CopyResponse::new(
                        output.format,
                        output.columns,
                        stream::iter(
                            output
                                .chunks
                                .into_iter()
                                .map(|chunk| Ok(CopyData::new(Bytes::from(chunk)))),
                        ),
                    ),
                )
                .await?;
            }
            QueryExecution::Unsupported { query } => {
                feed_error_response(client, unsupported_response(&query)).await?;
                *transaction_status = transaction_status.to_error_state();
            }
            QueryExecution::InvalidParameters { message } => {
                feed_error_response(client, invalid_parameter_response(&message)).await?;
                *transaction_status = transaction_status.to_error_state();
            }
            QueryExecution::Error {
                sqlstate,
                message,
                detail,
                hint,
                context,
                cursorpos,
                internal_query,
                internalpos,
            } => {
                feed_error_response(
                    client,
                    error_response(
                        &sqlstate,
                        &message,
                        WireErrorFields {
                            detail,
                            hint,
                            context,
                            cursorpos,
                            internal_query,
                            internalpos,
                        },
                    ),
                )
                .await?;
                *transaction_status = transaction_status.to_error_state();
            }
        }
    }
    Ok(())
}

async fn feed_query_result<C>(
    client: &mut C,
    result: QueryResult,
    format: FieldFormat,
    send_describe: bool,
) -> PgWireResult<()>
where
    C: Sink<PgWireBackendMessage> + Unpin,
    C::Error: Debug,
    PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
{
    let schema = Arc::new(field_infos(&result.fields, format));
    if send_describe {
        client
            .feed(PgWireBackendMessage::RowDescription(row_description(
                &schema,
            )))
            .await?;
    }

    let fields = result.fields;
    let command_tag = result
        .command_tag
        .as_deref()
        .map(query_response_command_tag)
        .unwrap_or_else(|| "SELECT".to_owned());
    let mut rows = 0;
    for row in result.rows {
        client
            .feed(PgWireBackendMessage::DataRow(encode_row(
                schema.clone(),
                &fields,
                &row,
            )?))
            .await?;
        rows += 1;
    }
    client
        .feed(PgWireBackendMessage::CommandComplete(
            command_complete_message(&command_tag, Some(rows)),
        ))
        .await?;
    Ok(())
}

async fn feed_command_complete<C>(
    client: &mut C,
    command_tag: &str,
    rows: Option<usize>,
    transaction_status: &mut TransactionStatus,
) -> PgWireResult<()>
where
    C: Sink<PgWireBackendMessage> + Unpin,
    C::Error: Debug,
    PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
{
    client
        .feed(PgWireBackendMessage::CommandComplete(
            command_complete_message(command_tag, rows),
        ))
        .await?;

    apply_command_transaction_status(command_tag, transaction_status);

    Ok(())
}

fn command_complete_message(command_tag: &str, rows: Option<usize>) -> CommandComplete {
    let tag = match (command_tag, rows) {
        ("BEGIN", None) => "BEGIN".to_owned(),
        ("COMMIT", None) => "COMMIT".to_owned(),
        ("END", None) => "END".to_owned(),
        ("ROLLBACK", None) => "ROLLBACK".to_owned(),
        ("SELECT", Some(1)) => "SELECT 1".to_owned(),
        ("UPDATE", Some(1)) => "UPDATE 1".to_owned(),
        ("INSERT", Some(1)) => "INSERT 0 1".to_owned(),
        (_, Some(rows)) => {
            let mut tag = String::with_capacity(command_tag.len() + 24);
            tag.push_str(command_tag);
            if command_tag == "INSERT" {
                tag.push_str(" 0");
            }
            tag.push(' ');
            write!(&mut tag, "{rows}").expect("writing CommandComplete tag to String cannot fail");
            tag
        }
        (_, None) => command_tag.to_owned(),
    };
    CommandComplete::new(tag)
}

async fn feed_error_response<C>(client: &mut C, response: Response) -> PgWireResult<()>
where
    C: Sink<PgWireBackendMessage> + Unpin,
    C::Error: Debug,
    PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
{
    if let Response::Error(error) = response {
        client
            .feed(PgWireBackendMessage::ErrorResponse((*error).into()))
            .await?;
    }
    Ok(())
}

impl Default for FastPgWireHandler {
    fn default() -> Self {
        Self::new(fastpg_compat::DEFAULT_SERVER_VERSION)
    }
}

#[async_trait]
impl StartupHandler for FastPgWireHandler {
    async fn on_startup<C>(
        &self,
        client: &mut C,
        message: PgWireFrontendMessage,
    ) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        if let PgWireFrontendMessage::Startup(ref startup) = message {
            let protocol = negotiate_startup_protocol(startup)?;
            if let Some(response) = protocol.response {
                client
                    .send(PgWireBackendMessage::NegotiateProtocolVersion(response))
                    .await?;
            }
            client.set_protocol_version(protocol.version);
            pgwire::api::auth::save_startup_parameters_to_metadata(client, startup);

            let (pid, secret_key) = PID_GENERATOR.generate(client);
            client.set_pid_and_secret_key(pid, secret_key);

            let mut parameters = DefaultServerParameterProvider::default();
            parameters.server_version = self.server.server_version().to_owned();

            let startup = startup_parameters(client, &message);
            let session = if self.inline_session_execution {
                SessionState::new_inline_execution(self.server.clone(), startup)
            } else {
                SessionState::new(self.server.clone(), startup)
            };
            client.session_extensions().insert(session);

            pgwire::api::auth::finish_authentication(client, &parameters).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl SimpleQueryHandler for FastPgWireHandler {
    async fn on_query<C>(&self, client: &mut C, query: Query) -> PgWireResult<()>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: PortalStore,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        if !matches!(client.state(), PgWireConnectionState::ReadyForQuery) {
            return Err(PgWireError::NotReadyForQuery);
        }

        let mut transaction_status = client.transaction_status();
        client.set_state(PgWireConnectionState::QueryInProgress);

        let query = query.query;
        if simple_query_is_empty(&query) {
            client
                .feed(PgWireBackendMessage::EmptyQueryResponse(
                    EmptyQueryResponse::new(),
                ))
                .await?;
        } else {
            let cancel_rx = cancel_receiver(client).await;
            let execution = if let Some(cancel_rx) = cancel_rx {
                match select(
                    Box::pin(self.execute_simple_message(client, query)),
                    cancel_rx,
                )
                .await
                {
                    Either::Left((result, _)) => result,
                    Either::Right(_) => Err(PgWireError::QueryCanceled),
                }
            } else {
                self.execute_simple_message(client, query).await
            }?;
            feed_simple_execution(client, execution, &mut transaction_status).await?;
        }

        if !matches!(client.state(), PgWireConnectionState::CopyInProgress(_)) {
            client.set_state(PgWireConnectionState::ReadyForQuery);
            client.set_transaction_status(transaction_status);
            client
                .feed(PgWireBackendMessage::ReadyForQuery(ReadyForQuery::new(
                    transaction_status,
                )))
                .await?;
            client.flush().await?;
        }

        Ok(())
    }

    async fn do_query<C>(&self, client: &mut C, query: &str) -> PgWireResult<Vec<Response>>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        execution_to_responses(
            self.execute_simple_message(client, query.to_owned())
                .await?,
            FieldFormat::Text,
        )
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
        let (execution, notices) = self
            .execute_query(session.clone(), query, parameters)
            .await?;
        send_notices(client, notices).await?;
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
        let (result, notices) = self.finish_copy(session.clone()).await?;
        send_notices(client, notices).await?;
        let rows = result.map_err(copy_data_error)?;

        client
            .send(PgWireBackendMessage::CommandComplete(
                Tag::new("COPY").with_rows(rows).into(),
            ))
            .await?;
        let entered_copy = self.drain_pending_simple(client, session).await?;
        if !entered_copy {
            client.set_state(PgWireConnectionState::ReadyForQuery);
        }
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

#[cfg(unix)]
#[cfg(unix)]
#[derive(Debug)]
struct FastPgUnixSocket {
    framed: Framed<UnixStream, FastPgUnixServerCodec>,
    cached_session: Option<Arc<SessionState>>,
    response_buffer: BytesMut,
}

#[cfg(unix)]
impl FastPgUnixSocket {
    fn new(unix_socket: UnixStream, codec: FastPgUnixServerCodec) -> Self {
        Self {
            framed: Framed::new(unix_socket, codec),
            cached_session: None,
            response_buffer: BytesMut::with_capacity(512),
        }
    }

    fn codec(&self) -> &FastPgUnixServerCodec {
        self.framed.codec()
    }

    fn codec_mut(&mut self) -> &mut FastPgUnixServerCodec {
        self.framed.codec_mut()
    }

    fn stream_mut(&mut self) -> &mut UnixStream {
        self.framed.get_mut()
    }

    fn cache_session_from_extensions(&mut self) {
        self.cached_session = self.session_extensions().get::<SessionState>();
    }

    fn cached_session(&self) -> PgWireResult<Arc<SessionState>> {
        self.cached_session
            .clone()
            .ok_or_else(missing_session_error)
    }

    fn take_response_buffer(&mut self) -> BytesMut {
        let mut response = std::mem::take(&mut self.response_buffer);
        response.clear();
        if response.capacity() < 512 {
            response.reserve(512 - response.capacity());
        }
        response
    }

    fn put_response_buffer(&mut self, mut response: BytesMut) {
        response.clear();
        self.response_buffer = response;
    }
}

#[cfg(unix)]
impl Stream for FastPgUnixSocket {
    type Item = Result<FastPgFrontendMessage, PgWireError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        Pin::new(&mut this.framed).poll_next(cx)
    }
}

#[cfg(unix)]
impl Sink<PgWireBackendMessage> for FastPgUnixSocket {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        Pin::new(&mut this.framed).poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: PgWireBackendMessage) -> Result<(), Self::Error> {
        let this = self.get_mut();
        Pin::new(&mut this.framed).start_send(item)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        Pin::new(&mut this.framed).poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        Pin::new(&mut this.framed).poll_close(cx)
    }
}

#[cfg(unix)]
#[derive(Debug)]
enum FastPgFrontendMessage {
    PgWire(PgWireFrontendMessage),
    FunctionCall(FunctionCall),
    ReadySimpleQuery(Bytes),
}

#[cfg(unix)]
#[derive(Debug)]
struct FastPgUnixServerCodec {
    client_info: DefaultClient<String>,
    decode_context: DecodeContext,
}

#[cfg(unix)]
impl FastPgUnixServerCodec {
    fn new(client_info: DefaultClient<String>) -> Self {
        Self {
            client_info,
            decode_context: DecodeContext::default(),
        }
    }

    fn decode_ready_simple_query(
        &self,
        src: &mut BytesMut,
    ) -> PgWireResult<Option<FastPgFrontendMessage>> {
        if !matches!(
            self.client_info.state(),
            PgWireConnectionState::ReadyForQuery
        ) || src.remaining() < 5
            || src[0] != b'Q'
        {
            return Ok(None);
        }

        let len = i32::from_be_bytes([src[1], src[2], src[3], src[4]]);
        if len < 5 {
            return Ok(None);
        }
        let Ok(len) = usize::try_from(len) else {
            return Ok(None);
        };
        let frame_len = len.saturating_add(1);
        if src.len() < frame_len {
            return Ok(None);
        }
        let query_end = frame_len - 1;
        if src[query_end] != 0 {
            return Ok(None);
        }
        if src[5..query_end].contains(&0) {
            return Ok(None);
        }
        let frame = src.split_to(frame_len).freeze();
        Ok(Some(FastPgFrontendMessage::ReadySimpleQuery(
            frame.slice(5..frame_len),
        )))
    }
}

#[cfg(unix)]
impl Decoder for FastPgUnixServerCodec {
    type Item = FastPgFrontendMessage;
    type Error = PgWireError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        self.decode_context.protocol_version = self.client_info.protocol_version;

        match self.client_info.state() {
            PgWireConnectionState::AwaitingStartup
            | PgWireConnectionState::AuthenticationInProgress => {
                self.decode_context.awaiting_frontend_ssl = false;
                self.decode_context.awaiting_frontend_startup = true;
            }
            _ => {
                self.decode_context.awaiting_frontend_ssl = false;
                self.decode_context.awaiting_frontend_startup = false;
            }
        }

        if let Some(message) = self.decode_ready_simple_query(src)? {
            return Ok(Some(message));
        }

        if !self.decode_context.awaiting_frontend_startup
            && src.remaining() > 1
            && src[0] == FunctionCall::MESSAGE_TYPE
        {
            return FunctionCall::decode(src)
                .map(|message| message.map(FastPgFrontendMessage::FunctionCall));
        }

        PgWireFrontendMessage::decode(src, &self.decode_context)
            .map(|message| message.map(FastPgFrontendMessage::PgWire))
    }
}

#[cfg(unix)]
impl Encoder<PgWireBackendMessage> for FastPgUnixServerCodec {
    type Error = io::Error;

    fn encode(
        &mut self,
        item: PgWireBackendMessage,
        dst: &mut BytesMut,
    ) -> Result<(), Self::Error> {
        item.encode(dst).map_err(Into::into)
    }
}

#[cfg(unix)]
impl ClientInfo for FastPgUnixSocket {
    fn socket_addr(&self) -> SocketAddr {
        self.codec().client_info.socket_addr
    }

    fn is_secure(&self) -> bool {
        self.codec().client_info.is_secure
    }

    fn protocol_version(&self) -> ProtocolVersion {
        self.codec().client_info.protocol_version()
    }

    fn set_protocol_version(&mut self, version: ProtocolVersion) {
        self.codec_mut().client_info.set_protocol_version(version);
    }

    fn pid_and_secret_key(&self) -> (i32, pgwire::messages::startup::SecretKey) {
        self.codec().client_info.pid_and_secret_key()
    }

    fn set_pid_and_secret_key(
        &mut self,
        pid: i32,
        secret_key: pgwire::messages::startup::SecretKey,
    ) {
        self.codec_mut()
            .client_info
            .set_pid_and_secret_key(pid, secret_key);
    }

    fn state(&self) -> PgWireConnectionState {
        self.codec().client_info.state()
    }

    fn set_state(&mut self, new_state: PgWireConnectionState) {
        self.codec_mut().client_info.set_state(new_state);
    }

    fn transaction_status(&self) -> TransactionStatus {
        self.codec().client_info.transaction_status()
    }

    fn set_transaction_status(&mut self, new_status: TransactionStatus) {
        self.codec_mut()
            .client_info
            .set_transaction_status(new_status);
    }

    fn metadata(&self) -> &std::collections::HashMap<String, String> {
        self.codec().client_info.metadata()
    }

    fn metadata_mut(&mut self) -> &mut std::collections::HashMap<String, String> {
        self.codec_mut().client_info.metadata_mut()
    }

    fn session_extensions(&self) -> &pgwire::api::SessionExtensions {
        self.codec().client_info.session_extensions()
    }
}

#[cfg(unix)]
impl ClientPortalStore for FastPgUnixSocket {
    type PortalStore = <DefaultClient<String> as ClientPortalStore>::PortalStore;

    fn portal_store(&self) -> &Self::PortalStore {
        self.codec().client_info.portal_store()
    }
}

#[cfg(unix)]
pub async fn process_socket_unix(
    unix_socket: UnixStream,
    handlers: Arc<FastPgServerHandlers>,
) -> Result<(), io::Error> {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let client_info = DefaultClient::new(addr, false);
    let mut socket = FastPgUnixSocket::new(unix_socket, FastPgUnixServerCodec::new(client_info));
    socket.set_state(PgWireConnectionState::AwaitingStartup);

    loop {
        let message = socket.next().await;

        let Some(message) = message else {
            break;
        };

        let result = match message {
            Ok(FastPgFrontendMessage::PgWire(message)) => {
                let wait_for_sync = match socket.state() {
                    PgWireConnectionState::CopyInProgress(is_extended_query) => is_extended_query,
                    _ => message.is_extended_query(),
                };
                process_pgwire_message(&handlers, &mut socket, message)
                    .await
                    .map_err(|error| (error, wait_for_sync))
            }
            Ok(FastPgFrontendMessage::ReadySimpleQuery(query)) => {
                process_ready_simple_query_unix_fast_path(&handlers, &mut socket, query)
                    .await
                    .map_err(|error| (error, false))
            }
            Ok(FastPgFrontendMessage::FunctionCall(function_call)) => {
                process_function_call(&handlers, &mut socket, function_call)
                    .await
                    .map_err(|error| (error, false))
            }
            Err(error) => Err((error, false)),
        };

        if let Err((error, wait_for_sync)) = result {
            process_unix_error(&mut socket, error, wait_for_sync).await?;
        }
    }

    Ok(())
}

#[cfg(unix)]
async fn process_pgwire_message(
    handlers: &FastPgServerHandlers,
    socket: &mut FastPgUnixSocket,
    message: PgWireFrontendMessage,
) -> PgWireResult<()> {
    if let PgWireFrontendMessage::CancelRequest(_cancel) = message {
        socket.close().await?;
        return Ok(());
    }

    match socket.state() {
        PgWireConnectionState::AwaitingStartup
        | PgWireConnectionState::AuthenticationInProgress => {
            handlers.handler.on_startup(socket, message).await?;
            socket.cache_session_from_extensions();
        }
        PgWireConnectionState::AwaitingSync => {
            if let PgWireFrontendMessage::Sync(sync) = message {
                handlers.handler.on_sync(socket, sync).await?;
                socket.set_state(PgWireConnectionState::ReadyForQuery);
            }
        }
        PgWireConnectionState::CopyInProgress(is_extended_query) => match message {
            PgWireFrontendMessage::CopyData(copy_data) => {
                handlers.handler.on_copy_data(socket, copy_data).await?;
            }
            PgWireFrontendMessage::CopyDone(copy_done) => {
                let result = handlers.handler.on_copy_done(socket, copy_done).await;
                if !is_extended_query
                    && !matches!(socket.state(), PgWireConnectionState::CopyInProgress(_))
                {
                    socket.set_state(PgWireConnectionState::ReadyForQuery);
                }
                match result {
                    Ok(_)
                        if !is_extended_query
                            && !matches!(
                                socket.state(),
                                PgWireConnectionState::CopyInProgress(_)
                            ) =>
                    {
                        send_ready_for_query(socket, TransactionStatus::Idle).await?;
                    }
                    Ok(_) => {}
                    Err(error) => return Err(error),
                }
            }
            PgWireFrontendMessage::CopyFail(copy_fail) => {
                let error = handlers.handler.on_copy_fail(socket, copy_fail).await;
                if !is_extended_query {
                    socket.set_state(PgWireConnectionState::ReadyForQuery);
                }
                return Err(error);
            }
            _ => {}
        },
        _ => match message {
            PgWireFrontendMessage::Query(query) => {
                if let Some(query) =
                    process_simple_query_unix_fast_path(handlers, socket, query).await?
                {
                    handlers.handler.on_query(socket, query).await?;
                }
            }
            PgWireFrontendMessage::Parse(parse) => {
                handlers.handler.on_parse(socket, parse).await?;
            }
            PgWireFrontendMessage::Bind(bind) => {
                handlers.handler.on_bind(socket, bind).await?;
            }
            PgWireFrontendMessage::Execute(execute) => {
                handlers.handler.on_execute(socket, execute).await?;
            }
            PgWireFrontendMessage::Describe(describe) => {
                handlers.handler.on_describe(socket, describe).await?;
            }
            PgWireFrontendMessage::Flush(flush) => {
                handlers.handler.on_flush(socket, flush).await?;
            }
            PgWireFrontendMessage::Sync(sync) => {
                handlers.handler.on_sync(socket, sync).await?;
            }
            PgWireFrontendMessage::Close(close) => {
                handlers.handler.on_close(socket, close).await?;
            }
            _ => {}
        },
    }

    Ok(())
}

#[cfg(unix)]
async fn process_simple_query_unix_fast_path(
    handlers: &FastPgServerHandlers,
    socket: &mut FastPgUnixSocket,
    query: Query,
) -> PgWireResult<Option<Query>> {
    let query = query.query;
    match process_simple_query_unix_fast_path_str(handlers, socket, &query, None).await? {
        SimpleQueryFastPathResult::Handled => Ok(None),
        SimpleQueryFastPathResult::Fallback => Ok(Some(Query::new(query))),
    }
}

#[cfg(unix)]
async fn process_ready_simple_query_unix_fast_path(
    handlers: &FastPgServerHandlers,
    socket: &mut FastPgUnixSocket,
    query: Bytes,
) -> PgWireResult<()> {
    let query_len = query
        .len()
        .checked_sub(1)
        .ok_or_else(|| protocol_error("simple query frame missing trailing NUL byte".to_owned()))?;
    let c_query = CStr::from_bytes_with_nul(&query)
        .map_err(|error| protocol_error(format!("invalid simple query C string: {error}")))?;
    let query = std::str::from_utf8(&query[..query_len])
        .map_err(|error| protocol_error(format!("invalid simple query UTF-8: {error}")))?;
    match process_simple_query_unix_fast_path_str(handlers, socket, query, Some(c_query)).await? {
        SimpleQueryFastPathResult::Handled => Ok(()),
        SimpleQueryFastPathResult::Fallback => {
            handlers
                .handler
                .on_query(socket, Query::new(query.to_owned()))
                .await
        }
    }
}

#[cfg(unix)]
enum SimpleQueryFastPathResult {
    Handled,
    Fallback,
}

#[cfg(unix)]
async fn process_simple_query_unix_fast_path_str(
    handlers: &FastPgServerHandlers,
    socket: &mut FastPgUnixSocket,
    query: &str,
    query_cstr: Option<&CStr>,
) -> PgWireResult<SimpleQueryFastPathResult> {
    if !matches!(socket.state(), PgWireConnectionState::ReadyForQuery) {
        return Ok(SimpleQueryFastPathResult::Fallback);
    }

    let mut transaction_status = socket.transaction_status();
    socket.set_state(PgWireConnectionState::QueryInProgress);

    let execution = if simple_query_is_empty(query) {
        QueryExecution::Empty
    } else {
        if !postgres_catalog_enabled() || simple_query_may_copy_from_stdin(query) {
            socket.set_state(PgWireConnectionState::ReadyForQuery);
            return Ok(SimpleQueryFastPathResult::Fallback);
        }

        let session = socket.cached_session()?;
        let inline_result = if let Some(query_cstr) = query_cstr {
            #[cfg(feature = "postgres-execution")]
            {
                handlers
                    .handler
                    .try_execute_simple_query_cstr_inline(session.clone(), query_cstr)
            }
            #[cfg(not(feature = "postgres-execution"))]
            {
                let _ = query_cstr;
                None
            }
        } else {
            handlers
                .handler
                .try_execute_simple_query_inline(session.clone(), query)
        };
        let (execution, notices) = match inline_result {
            Some(result) => result?,
            None => {
                handlers
                    .handler
                    .execute_simple_query(session.clone(), query.to_owned())
                    .await?
            }
        };

        if !notices.is_empty() {
            send_notices(socket, notices).await?;
            feed_simple_execution(socket, execution, &mut transaction_status).await?;
            if !matches!(socket.state(), PgWireConnectionState::CopyInProgress(_)) {
                socket.set_state(PgWireConnectionState::ReadyForQuery);
                socket.set_transaction_status(transaction_status);
                socket
                    .feed(PgWireBackendMessage::ReadyForQuery(ReadyForQuery::new(
                        transaction_status,
                    )))
                    .await?;
                socket.flush().await?;
            }
            return Ok(SimpleQueryFastPathResult::Handled);
        }

        remember_copy_target(&session, &execution);
        execution
    };

    let mut response = socket.take_response_buffer();
    if encode_simple_execution_unix_response(&execution, &mut transaction_status, &mut response)? {
        socket.set_state(PgWireConnectionState::ReadyForQuery);
        socket.set_transaction_status(transaction_status);
        encode_ready_for_query_unix(&mut response, transaction_status);
        socket.stream_mut().write_all(&response).await?;
        socket.put_response_buffer(response);
    } else {
        feed_simple_execution(socket, execution, &mut transaction_status).await?;
        if !matches!(socket.state(), PgWireConnectionState::CopyInProgress(_)) {
            socket.set_state(PgWireConnectionState::ReadyForQuery);
            socket.set_transaction_status(transaction_status);
            socket
                .feed(PgWireBackendMessage::ReadyForQuery(ReadyForQuery::new(
                    transaction_status,
                )))
                .await?;
            socket.flush().await?;
        }
    }

    Ok(SimpleQueryFastPathResult::Handled)
}

#[cfg(unix)]
fn encode_simple_execution_unix_response(
    execution: &QueryExecution,
    transaction_status: &mut TransactionStatus,
    response: &mut BytesMut,
) -> PgWireResult<bool> {
    match execution {
        QueryExecution::Empty => {
            encode_empty_query_unix(response);
            Ok(true)
        }
        QueryExecution::Command { tag, rows } => {
            encode_command_complete_unix(response, tag.as_ref(), *rows);
            apply_command_transaction_status(tag.as_ref(), transaction_status);
            Ok(true)
        }
        QueryExecution::Rows(result) => {
            encode_row_description_unix(response, &result.fields)?;
            for row in &result.rows {
                encode_data_row_unix(response, row);
            }
            let command_tag = result.command_tag.as_deref().unwrap_or("SELECT");
            encode_command_complete_unix(
                response,
                command_tag,
                Some(result.command_rows.unwrap_or(result.rows.len())),
            );
            Ok(true)
        }
        _ => Ok(false),
    }
}

#[cfg(unix)]
fn encode_message_header_unix(response: &mut BytesMut, tag: u8, body_len: usize) {
    response.put_u8(tag);
    response.put_i32((body_len + 4) as i32);
}

#[cfg(unix)]
fn encode_empty_query_unix(response: &mut BytesMut) {
    encode_message_header_unix(response, b'I', 0);
}

#[cfg(unix)]
fn encode_ready_for_query_unix(response: &mut BytesMut, status: TransactionStatus) {
    encode_message_header_unix(response, b'Z', 1);
    response.put_u8(status as u8);
}

#[cfg(unix)]
fn encode_command_complete_unix(response: &mut BytesMut, command_tag: &str, rows: Option<usize>) {
    let body_len = command_complete_tag_len(command_tag, rows) + 1;
    encode_message_header_unix(response, b'C', body_len);
    put_command_complete_tag_unix(response, command_tag, rows);
    response.put_u8(0);
}

#[cfg(unix)]
fn command_complete_tag_len(command_tag: &str, rows: Option<usize>) -> usize {
    match (command_tag, rows) {
        ("BEGIN", None) => 5,
        ("COMMIT", None) => 6,
        ("END", None) => 3,
        ("ROLLBACK", None) => 8,
        ("SELECT", Some(1)) => 8,
        ("UPDATE", Some(1)) => 8,
        ("INSERT", Some(1)) => 10,
        (_, Some(rows)) => {
            command_tag.len() + if command_tag == "INSERT" { 3 } else { 1 } + decimal_len(rows)
        }
        (_, None) => command_tag.len(),
    }
}

#[cfg(unix)]
fn put_command_complete_tag_unix(response: &mut BytesMut, command_tag: &str, rows: Option<usize>) {
    match (command_tag, rows) {
        ("BEGIN", None) => response.put_slice(b"BEGIN"),
        ("COMMIT", None) => response.put_slice(b"COMMIT"),
        ("END", None) => response.put_slice(b"END"),
        ("ROLLBACK", None) => response.put_slice(b"ROLLBACK"),
        ("SELECT", Some(1)) => response.put_slice(b"SELECT 1"),
        ("UPDATE", Some(1)) => response.put_slice(b"UPDATE 1"),
        ("INSERT", Some(1)) => response.put_slice(b"INSERT 0 1"),
        (_, Some(rows)) => {
            response.put_slice(command_tag.as_bytes());
            if command_tag == "INSERT" {
                response.put_slice(b" 0");
            }
            response.put_u8(b' ');
            put_decimal_unix(response, rows);
        }
        (_, None) => response.put_slice(command_tag.as_bytes()),
    }
}

#[cfg(unix)]
fn decimal_len(mut value: usize) -> usize {
    let mut len = 1;
    while value >= 10 {
        value /= 10;
        len += 1;
    }
    len
}

#[cfg(unix)]
fn put_decimal_unix(response: &mut BytesMut, mut value: usize) {
    let mut digits = [0u8; 20];
    let mut index = digits.len();
    loop {
        index -= 1;
        digits[index] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    response.put_slice(&digits[index..]);
}

#[cfg(unix)]
fn encode_row_description_unix(response: &mut BytesMut, fields: &[Column]) -> PgWireResult<()> {
    let mut body_len = 2;
    for field in fields {
        body_len += field.name.len() + 1 + 18;
    }
    encode_message_header_unix(response, b'T', body_len);
    response.put_i16(fields.len() as i16);
    for field in fields {
        response.put_slice(field.name.as_bytes());
        response.put_u8(0);
        response.put_u32(0);
        response.put_i16(0);
        response.put_u32(field.type_oid);
        response.put_i16(0);
        response.put_i32(0);
        response.put_i16(0);
    }
    Ok(())
}

#[cfg(unix)]
fn encode_data_row_unix(response: &mut BytesMut, row: &[Value]) {
    let body_len = 2 + row.iter().map(encoded_text_field_len).sum::<usize>();
    encode_message_header_unix(response, b'D', body_len);
    response.put_i16(row.len() as i16);
    for value in row {
        encode_text_field(response, value);
    }
}

#[cfg(unix)]
fn encoded_text_field_len(value: &Value) -> usize {
    match value {
        Value::Null => 4,
        Value::Int2(value) => 4 + decimal_i64_len(i64::from(*value)),
        Value::Int4(value) => 4 + decimal_i64_len(i64::from(*value)),
        Value::Int8(value) => 4 + decimal_i64_len(*value),
        Value::Text(value) | Value::RawText(value) => 4 + value.len(),
    }
}

#[cfg(unix)]
fn decimal_i64_len(value: i64) -> usize {
    if value < 0 {
        1 + decimal_len(value.unsigned_abs() as usize)
    } else {
        decimal_len(value as usize)
    }
}

fn apply_command_transaction_status(command_tag: &str, transaction_status: &mut TransactionStatus) {
    match command_tag {
        "BEGIN" => {
            *transaction_status = transaction_status.to_in_transaction_state();
        }
        "COMMIT" | "END" | "ROLLBACK" => {
            *transaction_status = transaction_status.to_idle_state();
        }
        _ => {
            let first = command_tag
                .as_bytes()
                .iter()
                .copied()
                .find(|byte| !byte.is_ascii_whitespace())
                .map(|byte| byte.to_ascii_uppercase());
            if let Some(b'B' | b'C' | b'E' | b'R') = first {
                match command_tag.split_whitespace().next() {
                    Some(command) if command.eq_ignore_ascii_case("BEGIN") => {
                        *transaction_status = transaction_status.to_in_transaction_state();
                    }
                    Some(command)
                        if command.eq_ignore_ascii_case("COMMIT")
                            || command.eq_ignore_ascii_case("END")
                            || command.eq_ignore_ascii_case("ROLLBACK") =>
                    {
                        *transaction_status = transaction_status.to_idle_state();
                    }
                    _ => {}
                }
            }
        }
    }
}

#[cfg(unix)]
async fn process_unix_error(
    socket: &mut FastPgUnixSocket,
    error: PgWireError,
    wait_for_sync: bool,
) -> Result<(), io::Error> {
    let error_info: ErrorInfo = error.into();
    let is_fatal = error_info.is_fatal();
    socket
        .send(PgWireBackendMessage::ErrorResponse(error_info.into()))
        .await?;

    let transaction_status = socket.transaction_status().to_error_state();
    socket.set_transaction_status(transaction_status);

    if wait_for_sync {
        socket.set_state(PgWireConnectionState::AwaitingSync);
    } else {
        socket.set_state(PgWireConnectionState::ReadyForQuery);
        socket
            .feed(PgWireBackendMessage::ReadyForQuery(ReadyForQuery::new(
                transaction_status,
            )))
            .await?;
    }
    socket.flush().await?;

    if is_fatal {
        return socket.close().await;
    }

    Ok(())
}

#[cfg(unix)]
#[derive(Clone, Debug)]
struct FunctionCall {
    function_id: u32,
    arg_format_codes: Vec<i16>,
    args: Vec<Option<Vec<u8>>>,
    result_format_code: i16,
}

#[cfg(unix)]
impl FunctionCall {
    const MESSAGE_TYPE: u8 = b'F';

    fn decode(src: &mut BytesMut) -> PgWireResult<Option<Self>> {
        if src.len() < 5 {
            return Ok(None);
        }
        let length = i32::from_be_bytes([src[1], src[2], src[3], src[4]]);
        if length < 4 {
            return Err(PgWireError::InvalidMessageType(Self::MESSAGE_TYPE));
        }

        let packet_length = 1usize + length as usize;
        if src.len() < packet_length {
            return Ok(None);
        }

        let mut packet = src.split_to(packet_length);
        packet.advance(5);

        let function_id = read_u32(&mut packet)?;
        let num_arg_formats = read_i16(&mut packet)? as usize;
        let mut arg_format_codes = Vec::with_capacity(num_arg_formats);
        for _ in 0..num_arg_formats {
            arg_format_codes.push(read_i16(&mut packet)?);
        }

        let num_args = read_i16(&mut packet)? as usize;
        let mut args = Vec::with_capacity(num_args);
        for _ in 0..num_args {
            let arg_len = read_i32(&mut packet)?;
            if arg_len == -1 {
                args.push(None);
            } else if arg_len < 0 {
                return Err(protocol_error(format!(
                    "invalid argument length in FunctionCall message: {arg_len}"
                )));
            } else {
                let arg_len = arg_len as usize;
                if packet.remaining() < arg_len {
                    return Err(protocol_error(
                        "truncated argument in FunctionCall message".to_owned(),
                    ));
                }
                args.push(Some(packet.split_to(arg_len).to_vec()));
            }
        }

        let result_format_code = read_i16(&mut packet)?;
        if packet.has_remaining() {
            return Err(protocol_error(
                "trailing data in FunctionCall message".to_owned(),
            ));
        }

        Ok(Some(Self {
            function_id,
            arg_format_codes,
            args,
            result_format_code,
        }))
    }

    fn arg_format_code(&self, index: usize) -> i16 {
        match self.arg_format_codes.as_slice() {
            [] => 0,
            [format] => *format,
            formats => formats[index],
        }
    }
}

#[cfg(unix)]
async fn process_function_call(
    handlers: &FastPgServerHandlers,
    socket: &mut FastPgUnixSocket,
    function_call: FunctionCall,
) -> PgWireResult<()> {
    if !matches!(socket.state(), PgWireConnectionState::ReadyForQuery) {
        return Err(PgWireError::NotReadyForQuery);
    }

    socket.set_state(PgWireConnectionState::QueryInProgress);

    let session = session_for_client(socket)?;
    let result = handlers
        .handler
        .execute_fastpath_function(session, function_call)
        .await?;
    send_function_call_response(socket, result.as_deref()).await?;

    let transaction_status = socket.transaction_status();
    socket.set_state(PgWireConnectionState::ReadyForQuery);
    socket
        .send(PgWireBackendMessage::ReadyForQuery(ReadyForQuery::new(
            transaction_status,
        )))
        .await?;
    Ok(())
}

#[cfg(unix)]
impl FastPgWireHandler {
    async fn execute_fastpath_function(
        &self,
        session: Arc<SessionState>,
        function_call: FunctionCall,
    ) -> PgWireResult<Option<Vec<u8>>> {
        let spec = fastpath_function(function_call.function_id)?;
        if function_call.args.len() != spec.args.len() {
            return Err(protocol_error(format!(
                "function call message contains {} arguments but function requires {}",
                function_call.args.len(),
                spec.args.len()
            )));
        }
        if function_call.arg_format_codes.len() > 1
            && function_call.arg_format_codes.len() != function_call.args.len()
        {
            return Err(protocol_error(format!(
                "function call message contains {} argument formats but {} arguments",
                function_call.arg_format_codes.len(),
                function_call.args.len()
            )));
        }
        if function_call.result_format_code != 1 {
            return Err(protocol_error(format!(
                "unsupported FunctionCall result format code: {}",
                function_call.result_format_code
            )));
        }

        let mut sql = format!("SELECT pg_catalog.{}(", spec.name);
        for (index, (arg, arg_type)) in function_call.args.iter().zip(spec.args).enumerate() {
            if index > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&function_arg_sql(
                arg.as_deref(),
                arg_type,
                function_call.arg_format_code(index),
            )?);
        }
        sql.push(')');

        let (execution, _notices) = self.execute_query(session, sql, vec![]).await?;
        match execution {
            QueryExecution::Rows(result) => encode_function_result(result, spec.ret),
            QueryExecution::Error {
                sqlstate,
                message,
                detail,
                hint,
                context,
                cursorpos,
                internal_query,
                internalpos,
            } => Err(PgWireError::UserError(Box::new(error_info(
                &sqlstate,
                &message,
                WireErrorFields {
                    detail,
                    hint,
                    context,
                    cursorpos,
                    internal_query,
                    internalpos,
                },
            )))),
            other => Err(protocol_error(format!(
                "fastpath function {} returned unexpected execution result {other:?}",
                spec.name
            ))),
        }
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum FastpathArgType {
    Bytea,
    Int4,
    Int8,
    Oid,
}

#[cfg(unix)]
impl FastpathArgType {
    fn sql_type(self) -> &'static str {
        match self {
            Self::Bytea => "bytea",
            Self::Int4 => "int4",
            Self::Int8 => "int8",
            Self::Oid => "oid",
        }
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum FastpathReturnType {
    Bytea,
    Int4,
    Int8,
    Oid,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
struct FastpathFunction {
    oid: u32,
    name: &'static str,
    args: &'static [FastpathArgType],
    ret: FastpathReturnType,
}

#[cfg(unix)]
const LO_OPEN_ARGS: &[FastpathArgType] = &[FastpathArgType::Oid, FastpathArgType::Int4];
#[cfg(unix)]
const LO_CLOSE_ARGS: &[FastpathArgType] = &[FastpathArgType::Int4];
#[cfg(unix)]
const LOREAD_ARGS: &[FastpathArgType] = &[FastpathArgType::Int4, FastpathArgType::Int4];
#[cfg(unix)]
const LOWRITE_ARGS: &[FastpathArgType] = &[FastpathArgType::Int4, FastpathArgType::Bytea];
#[cfg(unix)]
const LO_LSEEK_ARGS: &[FastpathArgType] = &[
    FastpathArgType::Int4,
    FastpathArgType::Int4,
    FastpathArgType::Int4,
];
#[cfg(unix)]
const LO_LSEEK64_ARGS: &[FastpathArgType] = &[
    FastpathArgType::Int4,
    FastpathArgType::Int8,
    FastpathArgType::Int4,
];
#[cfg(unix)]
const LO_CREAT_ARGS: &[FastpathArgType] = &[FastpathArgType::Int4];
#[cfg(unix)]
const LO_CREATE_ARGS: &[FastpathArgType] = &[FastpathArgType::Oid];
#[cfg(unix)]
const LO_TELL_ARGS: &[FastpathArgType] = &[FastpathArgType::Int4];
#[cfg(unix)]
const LO_TRUNCATE_ARGS: &[FastpathArgType] = &[FastpathArgType::Int4, FastpathArgType::Int4];
#[cfg(unix)]
const LO_TRUNCATE64_ARGS: &[FastpathArgType] = &[FastpathArgType::Int4, FastpathArgType::Int8];
#[cfg(unix)]
const LO_UNLINK_ARGS: &[FastpathArgType] = &[FastpathArgType::Oid];

#[cfg(unix)]
const FASTPATH_FUNCTIONS: &[FastpathFunction] = &[
    FastpathFunction {
        oid: 952,
        name: "lo_open",
        args: LO_OPEN_ARGS,
        ret: FastpathReturnType::Int4,
    },
    FastpathFunction {
        oid: 953,
        name: "lo_close",
        args: LO_CLOSE_ARGS,
        ret: FastpathReturnType::Int4,
    },
    FastpathFunction {
        oid: 954,
        name: "loread",
        args: LOREAD_ARGS,
        ret: FastpathReturnType::Bytea,
    },
    FastpathFunction {
        oid: 955,
        name: "lowrite",
        args: LOWRITE_ARGS,
        ret: FastpathReturnType::Int4,
    },
    FastpathFunction {
        oid: 956,
        name: "lo_lseek",
        args: LO_LSEEK_ARGS,
        ret: FastpathReturnType::Int4,
    },
    FastpathFunction {
        oid: 3170,
        name: "lo_lseek64",
        args: LO_LSEEK64_ARGS,
        ret: FastpathReturnType::Int8,
    },
    FastpathFunction {
        oid: 957,
        name: "lo_creat",
        args: LO_CREAT_ARGS,
        ret: FastpathReturnType::Oid,
    },
    FastpathFunction {
        oid: 715,
        name: "lo_create",
        args: LO_CREATE_ARGS,
        ret: FastpathReturnType::Oid,
    },
    FastpathFunction {
        oid: 958,
        name: "lo_tell",
        args: LO_TELL_ARGS,
        ret: FastpathReturnType::Int4,
    },
    FastpathFunction {
        oid: 3171,
        name: "lo_tell64",
        args: LO_TELL_ARGS,
        ret: FastpathReturnType::Int8,
    },
    FastpathFunction {
        oid: 1004,
        name: "lo_truncate",
        args: LO_TRUNCATE_ARGS,
        ret: FastpathReturnType::Int4,
    },
    FastpathFunction {
        oid: 3172,
        name: "lo_truncate64",
        args: LO_TRUNCATE64_ARGS,
        ret: FastpathReturnType::Int4,
    },
    FastpathFunction {
        oid: 964,
        name: "lo_unlink",
        args: LO_UNLINK_ARGS,
        ret: FastpathReturnType::Int4,
    },
];

#[cfg(unix)]
fn fastpath_function(oid: u32) -> PgWireResult<FastpathFunction> {
    FASTPATH_FUNCTIONS
        .iter()
        .copied()
        .find(|function| function.oid == oid)
        .ok_or_else(|| {
            PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".to_owned(),
                "42883".to_owned(),
                format!("function with OID {oid} does not exist"),
            )))
        })
}

#[cfg(unix)]
fn function_arg_sql(
    value: Option<&[u8]>,
    arg_type: &FastpathArgType,
    format_code: i16,
) -> PgWireResult<String> {
    let Some(value) = value else {
        return Ok(format!("NULL::{}", arg_type.sql_type()));
    };

    if format_code == 1 {
        return binary_function_arg_sql(value, *arg_type);
    }
    if format_code != 0 {
        return Err(protocol_error(format!(
            "unsupported FunctionCall argument format code: {format_code}"
        )));
    }

    let text = std::str::from_utf8(value)
        .map_err(|error| protocol_error(format!("invalid text FunctionCall argument: {error}")))?;
    Ok(format!(
        "{}::{}",
        sql_quote_literal(text),
        arg_type.sql_type()
    ))
}

#[cfg(unix)]
fn binary_function_arg_sql(value: &[u8], arg_type: FastpathArgType) -> PgWireResult<String> {
    match arg_type {
        FastpathArgType::Bytea => Ok(format!("decode('{}', 'hex')", hex_encode(value))),
        FastpathArgType::Int4 => Ok(format!("{}::int4", read_be_i32(value)?)),
        FastpathArgType::Int8 => Ok(format!("{}::int8", read_be_i64(value)?)),
        FastpathArgType::Oid => Ok(format!("{}::oid", read_be_u32(value)?)),
    }
}

#[cfg(unix)]
fn encode_function_result(
    result: QueryResult,
    result_type: FastpathReturnType,
) -> PgWireResult<Option<Vec<u8>>> {
    let Some(row) = result.rows.into_iter().next() else {
        return Err(protocol_error(
            "fastpath function returned no rows".to_owned(),
        ));
    };
    let Some(value) = row.into_iter().next() else {
        return Err(protocol_error(
            "fastpath function returned no columns".to_owned(),
        ));
    };

    match (result_type, value) {
        (_, Value::Null) => Ok(None),
        (FastpathReturnType::Bytea, Value::Text(value) | Value::RawText(value)) => {
            parse_bytea_text(&value).map(Some)
        }
        (FastpathReturnType::Int4, Value::Int4(value)) => Ok(Some(value.to_be_bytes().to_vec())),
        (FastpathReturnType::Int4, Value::Text(value) | Value::RawText(value)) => {
            let value = value
                .parse::<i32>()
                .map_err(|error| protocol_error(format!("invalid int4 result: {error}")))?;
            Ok(Some(value.to_be_bytes().to_vec()))
        }
        (FastpathReturnType::Int8, Value::Int8(value)) => Ok(Some(value.to_be_bytes().to_vec())),
        (FastpathReturnType::Int8, Value::Text(value) | Value::RawText(value)) => {
            let value = value
                .parse::<i64>()
                .map_err(|error| protocol_error(format!("invalid int8 result: {error}")))?;
            Ok(Some(value.to_be_bytes().to_vec()))
        }
        (FastpathReturnType::Oid, Value::Int4(value)) => {
            Ok(Some((value as u32).to_be_bytes().to_vec()))
        }
        (FastpathReturnType::Oid, Value::Text(value) | Value::RawText(value)) => {
            let value = value
                .parse::<u32>()
                .map_err(|error| protocol_error(format!("invalid oid result: {error}")))?;
            Ok(Some(value.to_be_bytes().to_vec()))
        }
        (expected, actual) => Err(protocol_error(format!(
            "cannot encode fastpath result {actual:?} as {expected:?}"
        ))),
    }
}

#[cfg(unix)]
async fn send_function_call_response(
    socket: &mut FastPgUnixSocket,
    result: Option<&[u8]>,
) -> PgWireResult<()> {
    let result_len = result.map(|bytes| bytes.len()).unwrap_or(usize::MAX);
    let message_len = if let Some(bytes) = result {
        8usize + bytes.len()
    } else {
        8
    };
    let mut buf = BytesMut::with_capacity(1 + message_len);
    buf.put_u8(b'V');
    buf.put_i32(message_len as i32);
    if let Some(bytes) = result {
        buf.put_i32(result_len as i32);
        buf.put_slice(bytes);
    } else {
        buf.put_i32(-1);
    }

    socket.flush().await?;
    socket.stream_mut().write_all(&buf).await?;
    socket.stream_mut().flush().await?;
    Ok(())
}

#[cfg(unix)]
fn error_info(sqlstate: &str, message: &str, fields: WireErrorFields) -> ErrorInfo {
    let WireErrorFields {
        detail,
        hint,
        context,
        cursorpos,
        internal_query,
        internalpos,
    } = fields;
    let mut error = ErrorInfo::new("ERROR".to_owned(), sqlstate.to_owned(), message.to_owned());
    error.detail = detail;
    error.hint = hint;
    error.where_context = context;
    if cursorpos > 0 {
        error.position = Some(cursorpos.to_string());
    }
    if internalpos > 0 {
        error.internal_position = Some(internalpos.to_string());
    }
    error.internal_query = internal_query;
    error
}

#[cfg(unix)]
fn read_i16(buf: &mut BytesMut) -> PgWireResult<i16> {
    if buf.remaining() < 2 {
        return Err(protocol_error("truncated FunctionCall message".to_owned()));
    }
    Ok(buf.get_i16())
}

#[cfg(unix)]
fn read_i32(buf: &mut BytesMut) -> PgWireResult<i32> {
    if buf.remaining() < 4 {
        return Err(protocol_error("truncated FunctionCall message".to_owned()));
    }
    Ok(buf.get_i32())
}

#[cfg(unix)]
fn read_u32(buf: &mut BytesMut) -> PgWireResult<u32> {
    if buf.remaining() < 4 {
        return Err(protocol_error("truncated FunctionCall message".to_owned()));
    }
    Ok(buf.get_u32())
}

#[cfg(unix)]
fn read_be_i32(value: &[u8]) -> PgWireResult<i32> {
    let bytes: [u8; 4] = value.try_into().map_err(|_| {
        protocol_error(format!(
            "expected 4-byte int4 argument, got {}",
            value.len()
        ))
    })?;
    Ok(i32::from_be_bytes(bytes))
}

#[cfg(unix)]
fn read_be_i64(value: &[u8]) -> PgWireResult<i64> {
    let bytes: [u8; 8] = value.try_into().map_err(|_| {
        protocol_error(format!(
            "expected 8-byte int8 argument, got {}",
            value.len()
        ))
    })?;
    Ok(i64::from_be_bytes(bytes))
}

#[cfg(unix)]
fn read_be_u32(value: &[u8]) -> PgWireResult<u32> {
    let bytes: [u8; 4] = value.try_into().map_err(|_| {
        protocol_error(format!("expected 4-byte oid argument, got {}", value.len()))
    })?;
    Ok(u32::from_be_bytes(bytes))
}

#[cfg(unix)]
fn sql_quote_literal(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            quoted.push('\'');
        }
        quoted.push(ch);
    }
    quoted.push('\'');
    quoted
}

#[cfg(unix)]
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[cfg(unix)]
fn parse_bytea_text(value: &str) -> PgWireResult<Vec<u8>> {
    if let Some(hex) = value.strip_prefix("\\x") {
        return parse_hex_bytea(hex);
    }

    let mut bytes = Vec::with_capacity(value.len());
    let mut chars = value.as_bytes().iter().copied().peekable();
    while let Some(byte) = chars.next() {
        if byte != b'\\' {
            bytes.push(byte);
            continue;
        }

        let Some(next) = chars.next() else {
            bytes.push(b'\\');
            break;
        };

        if is_octal_digit(next) {
            let Some(second) = chars.next() else {
                return Err(protocol_error("truncated bytea octal escape".to_owned()));
            };
            let Some(third) = chars.next() else {
                return Err(protocol_error("truncated bytea octal escape".to_owned()));
            };
            if !is_octal_digit(second) || !is_octal_digit(third) {
                return Err(protocol_error("invalid bytea octal escape".to_owned()));
            }
            let value = ((next - b'0') << 6) | ((second - b'0') << 3) | (third - b'0');
            bytes.push(value);
        } else {
            bytes.push(next);
        }
    }

    Ok(bytes)
}

#[cfg(unix)]
fn parse_hex_bytea(hex: &str) -> PgWireResult<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return Err(protocol_error("invalid bytea hex length".to_owned()));
    }
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_value(pair[0])?;
            let low = hex_value(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

#[cfg(unix)]
fn hex_value(byte: u8) -> PgWireResult<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(protocol_error("invalid bytea hex digit".to_owned())),
    }
}

#[cfg(unix)]
fn is_octal_digit(byte: u8) -> bool {
    matches!(byte, b'0'..=b'7')
}

#[cfg(unix)]
fn protocol_error(message: String) -> PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new(
        "ERROR".to_owned(),
        "08P01".to_owned(),
        message,
    )))
}

fn default_execution_concurrency() -> NonZeroUsize {
    std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN)
}

#[derive(Clone, Debug)]
struct ExecutionDispatcher {
    permits: Arc<Semaphore>,
    inline_session_execution: bool,
}

impl ExecutionDispatcher {
    fn new(max_concurrency: NonZeroUsize, inline_session_execution: bool) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(max_concurrency.get())),
            inline_session_execution,
        }
    }

    #[cfg(test)]
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
        Ok(tokio::task::block_in_place(move || {
            let _permit = permit;
            operation()
        }))
    }

    async fn run_session_blocking<R>(
        &self,
        session: Arc<SessionState>,
        operation: impl FnOnce(Arc<SessionState>) -> R + Send + 'static,
    ) -> PgWireResult<R>
    where
        R: Send + 'static,
    {
        if self.inline_session_execution {
            let run = || {
                let result = catch_unwind(AssertUnwindSafe(|| operation(session)));
                match result {
                    Ok(result) => Ok(result),
                    Err(payload) => resume_unwind(payload),
                }
            };
            match self.permits.try_acquire() {
                Ok(permit) => {
                    let _permit = permit;
                    return run();
                }
                Err(TryAcquireError::NoPermits) => {
                    let permit = self.permits.acquire().await.map_err(api_io_error)?;
                    let _permit = permit;
                    return run();
                }
                Err(error) => return Err(api_io_error(error)),
            }
        }

        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(api_io_error)?;
        let (sender, receiver) = oneshot::channel::<Result<R, Box<dyn Any + Send + 'static>>>();
        let operation_session = session.clone();
        session.enqueue_on_backend(move || {
            let _permit = permit;
            let result = catch_unwind(AssertUnwindSafe(|| operation(operation_session)));
            let _ = sender.send(result);
        });

        match receiver.await.map_err(api_io_error)? {
            Ok(result) => Ok(result),
            Err(payload) => resume_unwind(payload),
        }
    }

    fn try_run_session_inline<R>(
        &self,
        session: Arc<SessionState>,
        operation: impl FnOnce(Arc<SessionState>) -> R,
    ) -> Option<PgWireResult<R>> {
        if !self.inline_session_execution {
            return None;
        }

        match self.permits.try_acquire() {
            Ok(permit) => {
                let _permit = permit;
                let result = catch_unwind(AssertUnwindSafe(|| operation(session)));
                match result {
                    Ok(result) => Some(Ok(result)),
                    Err(payload) => resume_unwind(payload),
                }
            }
            Err(TryAcquireError::NoPermits) => None,
            Err(error) => Some(Err(api_io_error(error))),
        }
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

struct StartupProtocolNegotiation {
    version: ProtocolVersion,
    response: Option<NegotiateProtocolVersion>,
}

fn negotiate_startup_protocol(startup: &Startup) -> PgWireResult<StartupProtocolNegotiation> {
    let unsupported_options = startup
        .parameters
        .keys()
        .filter(|key| key.starts_with("_pq_."))
        .cloned()
        .collect::<Vec<_>>();

    let version = match ProtocolVersion::from_version_number(
        startup.protocol_number_major,
        startup.protocol_number_minor,
    ) {
        Some(version) => version,
        None if startup.protocol_number_major == Startup::PG_PROTOCOL_LATEST
            && startup.protocol_number_minor > ProtocolVersion::PROTOCOL3_2.version_number().1 =>
        {
            ProtocolVersion::PROTOCOL3_2
        }
        None => {
            return Err(PgWireError::UnsupportedProtocolVersion(
                startup.protocol_number_major,
                startup.protocol_number_minor,
            ));
        }
    };

    let (major, minor) = version.version_number();
    let response = (startup.protocol_number_minor != minor || !unsupported_options.is_empty())
        .then(|| {
            NegotiateProtocolVersion::new(pg_protocol_number(major, minor), unsupported_options)
        });

    Ok(StartupProtocolNegotiation { version, response })
}

fn pg_protocol_number(major: u16, minor: u16) -> i32 {
    (((major as u32) << 16) | minor as u32) as i32
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

const INT8_ARRAY_OID: u32 = 1016;

fn decode_parameters(
    portal: &Portal<String>,
    description: &QueryDescription,
) -> PgWireResult<Vec<Value>> {
    description
        .parameter_types
        .iter()
        .copied()
        .enumerate()
        .map(|(idx, data_type)| {
            let type_oid = description
                .parameter_type_oids
                .get(idx)
                .copied()
                .unwrap_or_else(|| data_type.default_type_oid());
            decode_parameter(portal, idx, type_oid, data_type)
        })
        .collect()
}

fn decode_parameter(
    portal: &Portal<String>,
    idx: usize,
    type_oid: u32,
    data_type: PgType,
) -> PgWireResult<Value> {
    match type_oid {
        INT8_ARRAY_OID => portal
            .parameter::<Vec<Option<i64>>>(idx, &Type::INT8_ARRAY)
            .map(|value| {
                value
                    .map(|values| Value::Text(int8_array_literal(&values)))
                    .unwrap_or(Value::Null)
            }),
        _ => match data_type {
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
        },
    }
}

fn int8_array_literal(values: &[Option<i64>]) -> String {
    let mut literal = String::from("{");
    for (idx, value) in values.iter().enumerate() {
        if idx > 0 {
            literal.push(',');
        }
        match value {
            Some(value) => write!(&mut literal, "{value}").expect("writing to String cannot fail"),
            None => literal.push_str("NULL"),
        }
    }
    literal.push('}');
    literal
}

fn postgres_catalog_enabled() -> bool {
    !cfg!(feature = "rust-catalog")
}

fn should_split_simple_query(statements: &[&str]) -> bool {
    !postgres_catalog_enabled()
        || statements
            .iter()
            .any(|statement| simple_statement_is_copy_from_stdin(statement))
}

fn simple_statement_is_copy_from_stdin(statement: &str) -> bool {
    let lower = statement.trim_start().to_ascii_lowercase();
    lower.starts_with("copy ") && lower.contains(" from stdin")
}

fn simple_query_may_copy_from_stdin(query: &str) -> bool {
    contains_ascii_case_insensitive(query.as_bytes(), b"copy")
        && contains_ascii_case_insensitive(query.as_bytes(), b"from stdin")
}

fn contains_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack
        .windows(needle.len())
        .any(|candidate| candidate.eq_ignore_ascii_case(needle))
}

fn split_simple_query_statements(query: &str) -> Vec<&str> {
    let bytes = query.as_bytes();
    let mut statements = Vec::new();
    let mut start = 0;
    let mut index = 0;
    let mut single_quoted = false;
    let mut double_quoted = false;
    let mut line_comment = false;
    let mut block_comment_depth = 0usize;
    let mut paren_depth = 0usize;
    let mut pending_begin = false;
    let mut atomic_body_depth = 0usize;
    let mut case_depth = 0usize;
    let mut dollar_quote: Option<Vec<u8>> = None;

    while index < bytes.len() {
        if let Some(tag) = dollar_quote.as_ref() {
            if bytes
                .get(index..index.saturating_add(tag.len()))
                .is_some_and(|candidate| candidate == tag.as_slice())
            {
                index += tag.len();
                dollar_quote = None;
            } else {
                index += 1;
            }
            continue;
        }

        let byte = bytes[index];
        if line_comment {
            if byte == b'\n' {
                line_comment = false;
            }
            index += 1;
            continue;
        }
        if block_comment_depth > 0 {
            if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
                block_comment_depth += 1;
                index += 2;
            } else if byte == b'*' && bytes.get(index + 1) == Some(&b'/') {
                block_comment_depth -= 1;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if single_quoted {
            if byte == b'\'' {
                if bytes.get(index + 1) == Some(&b'\'') {
                    index += 2;
                } else {
                    single_quoted = false;
                    index += 1;
                }
            } else {
                index += 1;
            }
            continue;
        }
        if double_quoted {
            if byte == b'"' {
                if bytes.get(index + 1) == Some(&b'"') {
                    index += 2;
                } else {
                    double_quoted = false;
                    index += 1;
                }
            } else {
                index += 1;
            }
            continue;
        }

        match byte {
            b'\'' => {
                single_quoted = true;
                pending_begin = false;
                index += 1;
            }
            b'"' => {
                double_quoted = true;
                pending_begin = false;
                index += 1;
            }
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                line_comment = true;
                index += 2;
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                block_comment_depth = 1;
                index += 2;
            }
            b'(' => {
                paren_depth += 1;
                pending_begin = false;
                index += 1;
            }
            b')' => {
                paren_depth = paren_depth.saturating_sub(1);
                pending_begin = false;
                index += 1;
            }
            b'$' => {
                if let Some(end) = dollar_quote_end(&bytes[index..]) {
                    dollar_quote = Some(bytes[index..index + end].to_vec());
                    pending_begin = false;
                    index += end;
                } else {
                    index += 1;
                }
            }
            b';' if paren_depth == 0 && atomic_body_depth == 0 => {
                let statement = &query[start..=index];
                if !statement.trim().is_empty() {
                    statements.push(statement);
                }
                index += 1;
                start = index;
            }
            b';' => {
                pending_begin = false;
                index += 1;
            }
            byte if is_identifier_start(byte) => {
                let end = identifier_end(bytes, index + 1);
                let word = &bytes[index..end];
                if keyword_eq(word, b"begin") {
                    pending_begin = true;
                } else if pending_begin && keyword_eq(word, b"atomic") {
                    atomic_body_depth += 1;
                    pending_begin = false;
                } else {
                    if atomic_body_depth > 0 {
                        if keyword_eq(word, b"case") {
                            case_depth += 1;
                        } else if keyword_eq(word, b"end") {
                            if case_depth > 0 {
                                case_depth -= 1;
                            } else {
                                atomic_body_depth = atomic_body_depth.saturating_sub(1);
                            }
                        }
                    }
                    pending_begin = false;
                }
                index = end;
            }
            byte if byte.is_ascii_whitespace() => {
                index += 1;
            }
            _ => index += 1,
        }
    }

    let statement = &query[start..];
    if !statement.trim().is_empty() {
        statements.push(statement);
    }
    statements
}

fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_identifier_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

fn identifier_end(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && is_identifier_continue(bytes[index]) {
        index += 1;
    }
    index
}

fn keyword_eq(word: &[u8], keyword: &[u8]) -> bool {
    word.eq_ignore_ascii_case(keyword)
}

fn dollar_quote_end(bytes: &[u8]) -> Option<usize> {
    if bytes.first() != Some(&b'$') {
        return None;
    }
    let mut index = 1;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'$' {
            return Some(index + 1);
        }
        if !(byte.is_ascii_alphanumeric() || byte == b'_') {
            return None;
        }
        index += 1;
    }
    None
}

fn execution_to_response(execution: QueryExecution, format: FieldFormat) -> PgWireResult<Response> {
    match execution {
        QueryExecution::WithNotices { execution, .. } => execution_to_response(*execution, format),
        QueryExecution::Empty => Ok(Response::EmptyQuery),
        QueryExecution::Batch(_) => Ok(error_response(
            "0A000",
            "fastpg cannot return a query batch in this protocol path",
            WireErrorFields::default(),
        )),
        QueryExecution::Rows(result) => query_result_response(result, format),
        QueryExecution::Command { tag, rows } => Ok(command_response(tag.as_ref(), rows)),
        QueryExecution::CopyIn(target) => Ok(Response::CopyIn(CopyResponse::new(
            0,
            target.columns,
            stream::empty(),
        ))),
        QueryExecution::CopyOut(output) => Ok(Response::CopyOut(CopyResponse::new(
            output.format,
            output.columns,
            stream::iter(
                output
                    .chunks
                    .into_iter()
                    .map(|chunk| Ok(CopyData::new(Bytes::from(chunk)))),
            ),
        ))),
        QueryExecution::Unsupported { query } => Ok(unsupported_response(&query)),
        QueryExecution::InvalidParameters { message } => Ok(invalid_parameter_response(&message)),
        QueryExecution::Error {
            sqlstate,
            message,
            detail,
            hint,
            context,
            cursorpos,
            internal_query,
            internalpos,
        } => Ok(error_response(
            &sqlstate,
            &message,
            WireErrorFields {
                detail,
                hint,
                context,
                cursorpos,
                internal_query,
                internalpos,
            },
        )),
    }
}

fn execution_to_responses(
    execution: QueryExecution,
    format: FieldFormat,
) -> PgWireResult<Vec<Response>> {
    match execution {
        QueryExecution::Batch(executions) => executions
            .into_iter()
            .map(|execution| execution_to_response(execution, format))
            .collect(),
        execution => Ok(vec![execution_to_response(execution, format)?]),
    }
}

fn execution_enters_copy(execution: &QueryExecution) -> bool {
    match execution {
        QueryExecution::CopyIn(_) => true,
        QueryExecution::Batch(executions) => executions.iter().any(execution_enters_copy),
        _ => false,
    }
}

fn execution_is_error(execution: &QueryExecution) -> bool {
    match execution {
        QueryExecution::Error { .. } => true,
        QueryExecution::Batch(executions) => executions.iter().any(execution_is_error),
        _ => false,
    }
}

fn query_result_response(result: QueryResult, format: FieldFormat) -> PgWireResult<Response> {
    let schema = Arc::new(field_infos(&result.fields, format));
    let fields = result.fields;
    let command_tag = result
        .command_tag
        .as_deref()
        .map(query_response_command_tag);
    let row_schema = schema.clone();
    let rows = result
        .rows
        .into_iter()
        .map(move |row| encode_row(row_schema.clone(), &fields, &row));

    let mut response = QueryResponse::new(schema, stream::iter(rows));
    if let Some(command_tag) = command_tag {
        response.set_command_tag(&command_tag);
    }
    Ok(Response::Query(response))
}

fn query_response_command_tag(tag: &str) -> String {
    if tag.eq_ignore_ascii_case("INSERT") {
        "INSERT 0".to_owned()
    } else {
        tag.to_owned()
    }
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
        Value::Text(value) | Value::RawText(value) => encode_text_bytes(row, value.as_bytes()),
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
        (PgType::Varchar, Value::RawText(value)) => encoder.encode_field(&Some(value.as_str())),
        (PgType::Int2, Value::RawText(value)) => {
            let value = parse_binary_text_value::<i16>(value, "int2")?;
            encoder.encode_field(&Some(value))
        }
        (PgType::Int4, Value::RawText(value)) => {
            let value = parse_binary_text_value::<i32>(value, "int4")?;
            encoder.encode_field(&Some(value))
        }
        (PgType::Int8, Value::RawText(value)) => {
            let value = parse_binary_text_value::<i64>(value, "int8")?;
            encoder.encode_field(&Some(value))
        }
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

fn parse_binary_text_value<T>(value: &str, type_name: &'static str) -> PgWireResult<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value.parse::<T>().map_err(|error| {
        PgWireError::ApiError(Box::new(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("cannot encode text value {value:?} as {type_name}: {error}"),
        )))
    })
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

#[derive(Default)]
struct WireErrorFields {
    detail: Option<String>,
    hint: Option<String>,
    context: Option<String>,
    cursorpos: i32,
    internal_query: Option<String>,
    internalpos: i32,
}

fn error_response(sqlstate: &str, message: &str, fields: WireErrorFields) -> Response {
    let WireErrorFields {
        detail,
        hint,
        context,
        cursorpos,
        internal_query,
        internalpos,
    } = fields;
    let mut error = ErrorInfo::new("ERROR".to_owned(), sqlstate.to_owned(), message.to_owned());
    error.detail = detail;
    error.hint = hint;
    error.where_context = context;
    if cursorpos > 0 {
        error.position = Some(cursorpos.to_string());
    }
    if internalpos > 0 {
        error.internal_position = Some(internalpos.to_string());
    }
    error.internal_query = internal_query;
    Response::Error(Box::new(error))
}

fn command_response(tag: &str, rows: Option<usize>) -> Response {
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

pub fn command_complete(command_tag: &str, rows: Option<usize>) -> Response {
    let mut tag = Tag::new(command_tag);
    if let Some(rows) = rows {
        tag = if command_tag == "INSERT" {
            tag.with_oid(0).with_rows(rows)
        } else {
            tag.with_rows(rows)
        };
    };
    Response::Execution(tag)
}

fn remember_copy_target(session: &SessionState, execution: &QueryExecution) {
    match execution {
        QueryExecution::CopyIn(target) => session.begin_copy(target.clone()),
        QueryExecution::Batch(executions) => {
            for execution in executions {
                remember_copy_target(session, execution);
            }
        }
        _ => {}
    }
}

fn copy_data_error(message: String) -> PgWireError {
    if let Some(rest) = message.strip_prefix(COPY_ERROR_FIELDS_PREFIX) {
        let mut parts = rest.splitn(4, '\n');
        let message = parts.next().unwrap_or("COPY failed").to_owned();
        let detail = parts
            .next()
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let hint = parts
            .next()
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let context = parts
            .next()
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let mut error = ErrorInfo::new("ERROR".to_owned(), "22P04".to_owned(), message);
        error.detail = detail;
        error.hint = hint;
        error.where_context = context;
        return PgWireError::UserError(Box::new(error));
    }

    let (message, context) = if let Some(rest) = message.strip_prefix(COPY_ERROR_CONTEXT_PREFIX) {
        let mut parts = rest.splitn(2, '\n');
        (
            parts.next().unwrap_or("COPY failed").to_owned(),
            parts.next().map(str::to_owned),
        )
    } else if let Some((message, context)) = message.split_once('\n') {
        if context.starts_with("COPY ") {
            (message.to_owned(), Some(context.to_owned()))
        } else {
            (message.to_owned(), None)
        }
    } else {
        (message, None)
    };
    let mut error = ErrorInfo::new("ERROR".to_owned(), "22P04".to_owned(), message);
    error.where_context = context;
    PgWireError::UserError(Box::new(error))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use pgwire::messages::extendedquery::Bind;
    use pgwire::messages::response::CommandComplete;
    use postgres_types::ToSql;

    use super::*;

    #[test]
    fn insert_command_complete_includes_legacy_oid_field() {
        assert_eq!(
            command_complete_tag(command_complete("INSERT", Some(3))),
            "INSERT 0 3"
        );
    }

    #[test]
    fn non_insert_command_complete_keeps_row_count_shape() {
        assert_eq!(
            command_complete_tag(command_complete("UPDATE", Some(2))),
            "UPDATE 2"
        );
    }

    #[test]
    fn transaction_commands_drive_pgwire_transaction_status() {
        assert!(matches!(
            command_response("BEGIN", None),
            Response::TransactionStart(_)
        ));
        assert!(matches!(
            command_response("COMMIT", None),
            Response::TransactionEnd(_)
        ));
        assert!(matches!(
            command_response("ROLLBACK", None),
            Response::TransactionEnd(_)
        ));
    }

    #[test]
    fn simple_query_splitter_keeps_atomic_sql_function_body_together() {
        let statements = split_simple_query_statements(
            "CREATE FUNCTION f() RETURNS boolean LANGUAGE SQL
             BEGIN ATOMIC
                 SELECT CASE WHEN true THEN false ELSE true END;
                 SELECT false;
             END;
             SELECT f();",
        );

        assert_eq!(statements.len(), 2);
        assert!(statements[0].contains("SELECT false;"));
        assert!(statements[1].contains("SELECT f();"));
    }

    #[test]
    fn simple_query_splitter_keeps_rule_action_lists_together() {
        let statements = split_simple_query_statements(
            "CREATE RULE r AS ON INSERT TO t DO INSTEAD (NOTIFY foo; NOTIFY bar);
             SELECT 1;",
        );

        assert_eq!(statements.len(), 2);
        assert!(statements[0].contains("NOTIFY foo; NOTIFY bar"));
        assert!(statements[1].contains("SELECT 1;"));
    }

    #[test]
    fn simple_query_splitter_keeps_nested_block_comments_together() {
        let statements = split_simple_query_statements(
            "/* outer /* inner ; */ still comment ; */ SELECT 1; SELECT 2;",
        );

        assert_eq!(statements.len(), 2);
        assert!(statements[0].contains("SELECT 1;"));
        assert!(statements[1].contains("SELECT 2;"));
    }

    #[test]
    fn simple_query_copy_from_stdin_detector_allows_leading_statements() {
        let query = "SELECT 0; COPY test3 FROM STDIN; SELECT 1;";

        assert!(simple_query_may_copy_from_stdin(query));
        assert!(should_split_simple_query(&split_simple_query_statements(
            query
        )));
    }

    #[test]
    fn startup_protocol_grease_negotiates_full_protocol_number() {
        let negotiation = negotiate_startup_protocol(&startup_with_protocol(
            3,
            9999,
            BTreeMap::from([("_pq_.test_protocol_negotiation".to_owned(), String::new())]),
        ))
        .unwrap();

        assert_eq!(negotiation.version, ProtocolVersion::PROTOCOL3_2);
        let response = negotiation.response.unwrap();
        assert_eq!(
            response.newest_minor_protocol,
            Startup::PROTOCOL_VERSION_3_2
        );
        assert_eq!(
            response.unsupported_options,
            vec!["_pq_.test_protocol_negotiation".to_owned()]
        );
    }

    #[test]
    fn startup_protocol_reports_options_for_protocol_3_0() {
        let negotiation = negotiate_startup_protocol(&startup_with_protocol(
            3,
            0,
            BTreeMap::from([("_pq_.future_option".to_owned(), String::new())]),
        ))
        .unwrap();

        assert_eq!(negotiation.version, ProtocolVersion::PROTOCOL3_0);
        let response = negotiation.response.unwrap();
        assert_eq!(
            response.newest_minor_protocol,
            Startup::PROTOCOL_VERSION_3_0
        );
        assert_eq!(
            response.unsupported_options,
            vec!["_pq_.future_option".to_owned()]
        );
    }

    #[test]
    fn startup_protocol_supported_version_skips_negotiation() {
        let negotiation =
            negotiate_startup_protocol(&startup_with_protocol(3, 2, BTreeMap::new())).unwrap();

        assert_eq!(negotiation.version, ProtocolVersion::PROTOCOL3_2);
        assert!(negotiation.response.is_none());
    }

    #[test]
    fn binary_int8_array_parameter_uses_type_oid_for_decoding() {
        let portal = portal_with_binary_parameter(encode_int8_array_parameter(vec![Some(1)]));
        let description =
            QueryDescription::with_type_oids(vec![PgType::Varchar], vec![INT8_ARRAY_OID], vec![]);

        assert_eq!(
            decode_parameters(&portal, &description).unwrap(),
            vec![Value::Text("{1}".to_owned())]
        );
    }

    #[test]
    fn int8_array_literal_formats_empty_nulls_and_values() {
        assert_eq!(int8_array_literal(&[]), "{}");
        assert_eq!(
            int8_array_literal(&[Some(-1), None, Some(3)]),
            "{-1,NULL,3}"
        );
    }

    fn startup_with_protocol(
        major: u16,
        minor: u16,
        parameters: BTreeMap<String, String>,
    ) -> Startup {
        let mut startup = Startup::new();
        startup.protocol_number_major = major;
        startup.protocol_number_minor = minor;
        startup.parameters = parameters;
        startup
    }

    fn command_complete_tag(response: Response) -> String {
        let Response::Execution(tag) = response else {
            panic!("expected execution response");
        };
        let command_complete = CommandComplete::from(tag);
        command_complete.tag
    }

    fn portal_with_binary_parameter(parameter: Bytes) -> Portal<String> {
        let bind = Bind::new(None, None, vec![1], vec![Some(parameter)], Vec::new());
        Portal::try_new(&bind, Arc::new(StoredStatement::default())).unwrap()
    }

    fn encode_int8_array_parameter(values: Vec<Option<i64>>) -> Bytes {
        let mut buffer = BytesMut::new();
        values
            .to_sql(&Type::INT8_ARRAY, &mut buffer)
            .expect("int8 array should encode");
        buffer.freeze()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn execution_dispatcher_caps_concurrent_blocking_work() {
        let dispatcher = ExecutionDispatcher::new(NonZeroUsize::new(2).unwrap(), false);
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
