#![forbid(unsafe_code)]

use std::fmt::Debug;
use std::io;
#[cfg(unix)]
use std::net::SocketAddr;
use std::num::NonZeroUsize;
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
use futures::{Sink, SinkExt, stream};
#[cfg(unix)]
use futures::{Stream, StreamExt};
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
use pgwire::api::{ClientInfo, ClientPortalStore, PgWireServerHandlers, Type};
use pgwire::api::{PgWireConnectionState, PidSecretKeyGenerator, RandomPidSecretKeyGenerator};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use pgwire::messages::copy::{CopyData, CopyDone, CopyFail};
use pgwire::messages::data::DataRow;
use pgwire::messages::response::EmptyQueryResponse;
#[cfg(not(unix))]
use pgwire::messages::response::NoticeResponse;
#[cfg(unix)]
use pgwire::messages::response::{NoticeResponse, ReadyForQuery, TransactionStatus};
#[cfg(unix)]
use pgwire::messages::{DecodeContext, ProtocolVersion};
use pgwire::messages::{PgWireBackendMessage, PgWireFrontendMessage};
use postgres_types::Kind;
#[cfg(unix)]
use tokio::io::AsyncWriteExt;
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::sync::Semaphore;
#[cfg(unix)]
use tokio::time::{Duration, sleep};
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
    ) -> PgWireResult<(QueryExecution, Vec<QueryNotice>)> {
        self.execution
            .run_blocking(move || {
                let execution = session.execute(&query, &parameters);
                let notices = session.take_notices();
                (execution, notices)
            })
            .await
    }

    async fn push_copy_data(&self, session: Arc<SessionState>, data: Vec<u8>) -> PgWireResult<()> {
        self.execution
            .run_blocking(move || session.push_copy_data(&data))
            .await?
            .map_err(copy_data_error)
    }

    async fn finish_copy(
        &self,
        session: Arc<SessionState>,
    ) -> PgWireResult<(Result<usize, String>, Vec<QueryNotice>)> {
        self.execution
            .run_blocking(move || {
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
            .run_blocking(move || session.abort_active_copy())
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
            pgwire::api::auth::protocol_negotiation(client, startup).await?;
            pgwire::api::auth::save_startup_parameters_to_metadata(client, startup);

            let (pid, secret_key) = PID_GENERATOR.generate(client);
            client.set_pid_and_secret_key(pid, secret_key);

            let mut parameters = DefaultServerParameterProvider::default();
            parameters.server_version = self.server.server_version().to_owned();

            client.session_extensions().insert(SessionState::new(
                self.server.clone(),
                startup_parameters(client, &message),
            ));

            pgwire::api::auth::finish_authentication(client, &parameters).await?;
        }
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
        let statements = split_simple_query_statements(query);
        if statements.len() > 1 && should_split_simple_query(&statements) {
            let mut responses = Vec::with_capacity(statements.len());
            for (statement_index, statement) in statements.iter().enumerate() {
                let (execution, notices) = self
                    .execute_query(session.clone(), (*statement).to_owned(), vec![])
                    .await?;
                send_notices(client, notices).await?;
                let stop_batch =
                    execution_enters_copy(&execution) || execution_is_error(&execution);
                if execution_enters_copy(&execution) {
                    session.set_pending_simple_statements(
                        statements[statement_index + 1..]
                            .iter()
                            .map(|statement| (*statement).to_owned())
                            .collect(),
                    );
                }
                remember_copy_target(&session, &execution);
                responses.extend(execution_to_responses(execution, FieldFormat::Text)?);
                if stop_batch {
                    break;
                }
            }
            return Ok(responses);
        }

        let (execution, notices) = self
            .execute_query(session.clone(), query.to_owned(), vec![])
            .await?;
        send_notices(client, notices).await?;
        remember_copy_target(&session, &execution);
        execution_to_responses(execution, FieldFormat::Text)
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
const STARTUP_TIMEOUT_MILLIS: u64 = 60_000;

#[cfg(unix)]
#[derive(Debug)]
struct FastPgUnixSocket {
    framed: Framed<UnixStream, FastPgUnixServerCodec>,
}

#[cfg(unix)]
impl FastPgUnixSocket {
    fn new(unix_socket: UnixStream, codec: FastPgUnixServerCodec) -> Self {
        Self {
            framed: Framed::new(unix_socket, codec),
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
    let startup_timeout = sleep(Duration::from_millis(STARTUP_TIMEOUT_MILLIS));
    tokio::pin!(startup_timeout);

    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let client_info = DefaultClient::new(addr, false);
    let mut socket = FastPgUnixSocket::new(unix_socket, FastPgUnixServerCodec::new(client_info));
    socket.set_state(PgWireConnectionState::AwaitingStartup);

    loop {
        let message = if matches!(
            socket.state(),
            PgWireConnectionState::AwaitingStartup
                | PgWireConnectionState::AuthenticationInProgress
        ) {
            tokio::select! {
                _ = &mut startup_timeout => None,
                message = socket.next() => message,
            }
        } else {
            socket.next().await
        };

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
                handlers.handler.on_query(socket, query).await?;
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
        (FastpathReturnType::Bytea, Value::Text(value)) => parse_bytea_text(&value).map(Some),
        (FastpathReturnType::Int4, Value::Int4(value)) => Ok(Some(value.to_be_bytes().to_vec())),
        (FastpathReturnType::Int4, Value::Text(value)) => {
            let value = value
                .parse::<i32>()
                .map_err(|error| protocol_error(format!("invalid int4 result: {error}")))?;
            Ok(Some(value.to_be_bytes().to_vec()))
        }
        (FastpathReturnType::Int8, Value::Int8(value)) => Ok(Some(value.to_be_bytes().to_vec())),
        (FastpathReturnType::Int8, Value::Text(value)) => {
            let value = value
                .parse::<i64>()
                .map_err(|error| protocol_error(format!("invalid int8 result: {error}")))?;
            Ok(Some(value.to_be_bytes().to_vec()))
        }
        (FastpathReturnType::Oid, Value::Int4(value)) => {
            Ok(Some((value as u32).to_be_bytes().to_vec()))
        }
        (FastpathReturnType::Oid, Value::Text(value)) => {
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
        Ok(tokio::task::block_in_place(move || {
            let _permit = permit;
            operation()
        }))
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

fn postgres_catalog_enabled() -> bool {
    std::env::var("FASTPG_CATALOG_MODE")
        .map(|value| value.eq_ignore_ascii_case("postgres"))
        .unwrap_or(false)
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use pgwire::messages::response::CommandComplete;

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
