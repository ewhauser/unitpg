#![forbid(unsafe_code)]

use std::fmt::Debug;
use std::io;
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use async_trait::async_trait;
use bytes::{Buf, BufMut, BytesMut};
use fastpg_session::{
    Column, PgType, QueryDescription, QueryExecution, QueryNotice, QueryResult, ServerState,
    SessionState, StartupParameters, Value,
};
use futures::{Sink, SinkExt, StreamExt, stream};
use pgwire::api::auth::StartupHandler;
use pgwire::api::auth::noop::NoopStartupHandler;
use pgwire::api::cancel::CancelHandler;
use pgwire::api::copy::CopyHandler;
use pgwire::api::portal::{Format, Portal};
use pgwire::api::query::{ExtendedQueryHandler, SimpleQueryHandler};
use pgwire::api::results::{
    CopyResponse, DataRowEncoder, DescribePortalResponse, DescribeStatementResponse, FieldFormat,
    FieldInfo, QueryResponse, Response, Tag,
};
use pgwire::api::stmt::{NoopQueryParser, StoredStatement};
use pgwire::api::{
    ClientInfo, ClientPortalStore, DefaultClient, ErrorHandler, PgWireConnectionState,
    PgWireServerHandlers, Type,
};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use pgwire::messages::copy::{CopyData, CopyDone, CopyFail};
use pgwire::messages::data::DataRow;
use pgwire::messages::response::{ReadyForQuery, TransactionStatus};
use pgwire::messages::startup::SecretKey;
use pgwire::messages::{
    DecodeContext, PgWireBackendMessage, PgWireFrontendMessage, ProtocolVersion,
};
use pgwire::tokio::server::{MaybeTls, PgWireMessageServerCodec, negotiate_tls};
use postgres_types::Kind;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::sync::Semaphore;
use tokio_util::codec::{Decoder, Encoder, Framed, FramedParts};

const STARTUP_TIMEOUT_MILLIS: u64 = 60_000;
const FUNCTION_CALL_MESSAGE_TYPE: u8 = b'F';
const FUNCTION_CALL_RESPONSE_MESSAGE_TYPE: u8 = b'V';

const LO_CREATE_OID: u32 = 715;
const LO_OPEN_OID: u32 = 952;
const LO_CLOSE_OID: u32 = 953;
const LOREAD_OID: u32 = 954;
const LOWRITE_OID: u32 = 955;
const LO_LSEEK_OID: u32 = 956;
const LO_CREAT_OID: u32 = 957;
const LO_TELL_OID: u32 = 958;
const LO_UNLINK_OID: u32 = 964;
const LO_TRUNCATE_OID: u32 = 1004;
const LO_LSEEK64_OID: u32 = 3170;
const LO_TELL64_OID: u32 = 3171;
const LO_TRUNCATE64_OID: u32 = 3172;

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

    async fn execute_function_call<C>(
        &self,
        client: &mut C,
        call: FunctionCall,
    ) -> PgWireResult<Vec<u8>>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let session = session_for_client(client)?;
        let query = LargeObjectFunctionQuery::try_from_call(&call)?;
        let execution = self
            .execute_query(session, query.sql.clone(), vec![])
            .await?;
        let (notices, execution) = execution.into_notices_and_execution();
        send_notices(client, &notices).await?;
        query.encode_result(function_call_single_value(execution)?)
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
        let (notices, execution) = execution.into_notices_and_execution();
        send_notices(client, &notices).await?;
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
        let (notices, execution) = execution.into_notices_and_execution();
        send_notices(client, &notices).await?;
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

pub async fn process_socket(
    tcp_socket: TcpStream,
    handlers: Arc<FastPgServerHandlers>,
) -> Result<(), io::Error> {
    let startup_timeout =
        tokio::time::sleep(std::time::Duration::from_millis(STARTUP_TIMEOUT_MILLIS));
    tokio::pin!(startup_timeout);

    let socket = tokio::select! {
        _ = &mut startup_timeout => return Ok(()),
        socket = negotiate_tls::<String>(tcp_socket, None) => socket?,
    };
    let Some(socket) = socket else {
        return Ok(());
    };

    let mut socket = FastPgSocket::from_pgwire_socket(socket);
    process_fastpg_socket_messages(&mut socket, handlers).await
}

#[cfg(unix)]
pub async fn process_socket_unix(
    unix_socket: UnixStream,
    handlers: Arc<FastPgServerHandlers>,
) -> Result<(), io::Error> {
    let addr = "127.0.0.1:0".parse().unwrap();
    let client_info = DefaultClient::new(addr, false);
    let mut socket = FastPgSocket::new(unix_socket, client_info);
    socket.set_state(PgWireConnectionState::AwaitingStartup);
    process_fastpg_socket_messages(&mut socket, handlers).await
}

async fn process_fastpg_socket_messages<S>(
    socket: &mut FastPgSocket<S>,
    handlers: Arc<FastPgServerHandlers>,
) -> Result<(), io::Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
{
    let error_handler = handlers.error_handler();

    while let Some(message) = socket.inner.next().await {
        match message {
            Ok(message) => {
                let wait_for_sync = match socket.state() {
                    PgWireConnectionState::CopyInProgress(is_extended_query) => is_extended_query,
                    _ => message.is_extended_query(),
                };
                if let Err(mut error) =
                    process_fastpg_message(message, socket, handlers.clone()).await
                {
                    error_handler.on_error(socket, &mut error);
                    process_fastpg_error(socket, error, wait_for_sync).await?;
                }
            }
            Err(mut error) => {
                error_handler.on_error(socket, &mut error);
                process_fastpg_error(socket, error, false).await?;
            }
        }
    }

    Ok(())
}

async fn process_fastpg_message<S>(
    message: FastPgFrontendMessage,
    socket: &mut FastPgSocket<S>,
    handlers: Arc<FastPgServerHandlers>,
) -> PgWireResult<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
{
    let handler = handlers.handler.clone();

    match message {
        FastPgFrontendMessage::PgWire(PgWireFrontendMessage::CancelRequest(cancel)) => {
            handlers.cancel_handler().on_cancel_request(cancel).await;
            socket.close().await?;
        }
        FastPgFrontendMessage::PgWire(message) => match socket.state() {
            PgWireConnectionState::AwaitingStartup
            | PgWireConnectionState::AuthenticationInProgress => {
                handler.on_startup(socket, message).await?;
            }
            PgWireConnectionState::AwaitingSync => {
                if let PgWireFrontendMessage::Sync(sync) = message {
                    handler.on_sync(socket, sync).await?;
                    socket.set_state(PgWireConnectionState::ReadyForQuery);
                }
            }
            PgWireConnectionState::CopyInProgress(is_extended_query) => match message {
                PgWireFrontendMessage::CopyData(copy_data) => {
                    handler.on_copy_data(socket, copy_data).await?;
                }
                PgWireFrontendMessage::CopyDone(copy_done) => {
                    let result = handler.on_copy_done(socket, copy_done).await;
                    if !is_extended_query {
                        socket.set_state(PgWireConnectionState::ReadyForQuery);
                    }
                    match result {
                        Ok(_) => {
                            if !is_extended_query {
                                socket
                                    .send(PgWireBackendMessage::ReadyForQuery(ReadyForQuery::new(
                                        TransactionStatus::Idle,
                                    )))
                                    .await?;
                            }
                        }
                        Err(error) => return Err(error),
                    }
                }
                PgWireFrontendMessage::CopyFail(copy_fail) => {
                    let error = handler.on_copy_fail(socket, copy_fail).await;
                    if !is_extended_query {
                        socket.set_state(PgWireConnectionState::ReadyForQuery);
                    }
                    return Err(error);
                }
                _ => {}
            },
            _ => match message {
                PgWireFrontendMessage::Query(query) => {
                    handler.on_query(socket, query).await?;
                }
                PgWireFrontendMessage::Parse(parse) => {
                    handler.on_parse(socket, parse).await?;
                }
                PgWireFrontendMessage::Bind(bind) => {
                    handler.on_bind(socket, bind).await?;
                }
                PgWireFrontendMessage::Execute(execute) => {
                    handler.on_execute(socket, execute).await?;
                }
                PgWireFrontendMessage::Describe(describe) => {
                    handler.on_describe(socket, describe).await?;
                }
                PgWireFrontendMessage::Flush(flush) => {
                    handler.on_flush(socket, flush).await?;
                }
                PgWireFrontendMessage::Sync(sync) => {
                    handler.on_sync(socket, sync).await?;
                }
                PgWireFrontendMessage::Close(close) => {
                    handler.on_close(socket, close).await?;
                }
                _ => {}
            },
        },
        FastPgFrontendMessage::FunctionCall(call) => {
            if !matches!(socket.state(), PgWireConnectionState::ReadyForQuery) {
                return Err(PgWireError::NotReadyForQuery);
            }
            socket.set_state(PgWireConnectionState::QueryInProgress);
            let result = handler.execute_function_call(socket, call).await?;
            socket.send_function_call_response(&result).await?;
            socket.set_state(PgWireConnectionState::ReadyForQuery);
            socket
                .send(PgWireBackendMessage::ReadyForQuery(ReadyForQuery::new(
                    socket.transaction_status(),
                )))
                .await?;
        }
    }

    Ok(())
}

async fn process_fastpg_error<S>(
    socket: &mut FastPgSocket<S>,
    error: PgWireError,
    wait_for_sync: bool,
) -> Result<(), io::Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
{
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
            .send(PgWireBackendMessage::ReadyForQuery(ReadyForQuery::new(
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

struct FastPgSocket<S> {
    inner: Framed<S, FastPgMessageServerCodec>,
}

impl<S> FastPgSocket<S> {
    fn new(io: S, client_info: DefaultClient<String>) -> Self {
        Self {
            inner: Framed::new(io, FastPgMessageServerCodec::new(client_info)),
        }
    }

    async fn send_function_call_response(&mut self, result: &[u8]) -> Result<(), io::Error>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let mut message = BytesMut::with_capacity(result.len() + 9);
        message.put_u8(FUNCTION_CALL_RESPONSE_MESSAGE_TYPE);
        message.put_i32((8 + result.len()) as i32);
        message.put_i32(result.len() as i32);
        message.put_slice(result);

        self.inner.flush().await?;
        self.inner.get_mut().write_all(&message).await?;
        self.inner.get_mut().flush().await
    }
}

impl FastPgSocket<MaybeTls> {
    fn from_pgwire_socket(socket: Framed<MaybeTls, PgWireMessageServerCodec<String>>) -> Self {
        let parts = socket.into_parts();
        let client_info = parts.codec.client_info;
        let mut fastpg_parts =
            FramedParts::new(parts.io, FastPgMessageServerCodec::new(client_info));
        fastpg_parts.read_buf = parts.read_buf;
        fastpg_parts.write_buf = parts.write_buf;

        Self {
            inner: Framed::from_parts(fastpg_parts),
        }
    }
}

impl<S> ClientInfo for FastPgSocket<S> {
    fn socket_addr(&self) -> SocketAddr {
        self.inner.codec().client_info.socket_addr
    }

    fn is_secure(&self) -> bool {
        self.inner.codec().client_info.is_secure
    }

    fn protocol_version(&self) -> ProtocolVersion {
        self.inner.codec().client_info.protocol_version()
    }

    fn set_protocol_version(&mut self, version: ProtocolVersion) {
        self.inner
            .codec_mut()
            .client_info
            .set_protocol_version(version);
    }

    fn pid_and_secret_key(&self) -> (i32, SecretKey) {
        self.inner.codec().client_info.pid_and_secret_key()
    }

    fn set_pid_and_secret_key(&mut self, pid: i32, secret_key: SecretKey) {
        self.inner
            .codec_mut()
            .client_info
            .set_pid_and_secret_key(pid, secret_key);
    }

    fn state(&self) -> PgWireConnectionState {
        self.inner.codec().client_info.state()
    }

    fn set_state(&mut self, new_state: PgWireConnectionState) {
        self.inner.codec_mut().client_info.set_state(new_state);
    }

    fn transaction_status(&self) -> TransactionStatus {
        self.inner.codec().client_info.transaction_status()
    }

    fn set_transaction_status(&mut self, new_status: TransactionStatus) {
        self.inner
            .codec_mut()
            .client_info
            .set_transaction_status(new_status);
    }

    fn metadata(&self) -> &std::collections::HashMap<String, String> {
        self.inner.codec().client_info.metadata()
    }

    fn metadata_mut(&mut self) -> &mut std::collections::HashMap<String, String> {
        self.inner.codec_mut().client_info.metadata_mut()
    }

    fn session_extensions(&self) -> &pgwire::api::SessionExtensions {
        self.inner.codec().client_info.session_extensions()
    }
}

impl<S> ClientPortalStore for FastPgSocket<S> {
    type PortalStore = <DefaultClient<String> as ClientPortalStore>::PortalStore;

    fn portal_store(&self) -> &Self::PortalStore {
        self.inner.codec().client_info.portal_store()
    }
}

impl<S> Sink<PgWireBackendMessage> for FastPgSocket<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: PgWireBackendMessage) -> Result<(), Self::Error> {
        Pin::new(&mut self.get_mut().inner).start_send(item)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_close(cx)
    }
}

#[derive(Debug)]
struct FastPgMessageServerCodec {
    client_info: DefaultClient<String>,
    decode_context: DecodeContext,
}

impl FastPgMessageServerCodec {
    fn new(client_info: DefaultClient<String>) -> Self {
        Self {
            client_info,
            decode_context: DecodeContext::default(),
        }
    }
}

impl Decoder for FastPgMessageServerCodec {
    type Item = FastPgFrontendMessage;
    type Error = PgWireError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        self.decode_context.protocol_version = self.client_info.protocol_version;

        match self.client_info.state() {
            PgWireConnectionState::AwaitingSslRequest => {
                self.decode_context.awaiting_frontend_ssl = true;
                self.decode_context.awaiting_frontend_startup = true;
            }
            PgWireConnectionState::AwaitingStartup => {
                self.decode_context.awaiting_frontend_ssl = false;
                self.decode_context.awaiting_frontend_startup = true;
            }
            _ => {
                self.decode_context.awaiting_frontend_startup = false;
                self.decode_context.awaiting_frontend_ssl = false;
            }
        }

        if !self.decode_context.awaiting_frontend_ssl
            && !self.decode_context.awaiting_frontend_startup
            && src.first() == Some(&FUNCTION_CALL_MESSAGE_TYPE)
        {
            return FunctionCall::decode(src)
                .map(|message| message.map(FastPgFrontendMessage::FunctionCall));
        }

        PgWireFrontendMessage::decode(src, &self.decode_context)
            .map(|message| message.map(FastPgFrontendMessage::PgWire))
    }
}

impl Encoder<PgWireBackendMessage> for FastPgMessageServerCodec {
    type Error = io::Error;

    fn encode(
        &mut self,
        item: PgWireBackendMessage,
        dst: &mut BytesMut,
    ) -> Result<(), Self::Error> {
        item.encode(dst).map_err(Into::into)
    }
}

#[derive(Debug)]
enum FastPgFrontendMessage {
    PgWire(PgWireFrontendMessage),
    FunctionCall(FunctionCall),
}

impl FastPgFrontendMessage {
    fn is_extended_query(&self) -> bool {
        match self {
            FastPgFrontendMessage::PgWire(message) => message.is_extended_query(),
            FastPgFrontendMessage::FunctionCall(_) => false,
        }
    }
}

#[derive(Debug)]
struct FunctionCall {
    function_oid: u32,
    arguments: Vec<Option<Vec<u8>>>,
}

impl FunctionCall {
    fn decode(buf: &mut BytesMut) -> PgWireResult<Option<Self>> {
        if buf.len() < 5 {
            return Ok(None);
        }

        let message_length = i32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
        if message_length < 4 {
            return Err(PgWireError::InvalidMessageType(FUNCTION_CALL_MESSAGE_TYPE));
        }
        let full_length = 1 + message_length as usize;
        if buf.len() < full_length {
            return Ok(None);
        }

        let mut packet = buf.split_to(full_length);
        packet.advance(1);
        let _message_length = packet.get_i32();

        let function_oid = read_u32(&mut packet)?;
        let argument_formats = read_i16_vec(&mut packet)?;
        let argument_count = read_i16(&mut packet)?;
        if argument_count < 0 {
            return Err(function_call_protocol_error(
                "negative function call argument count",
            ));
        }

        let mut arguments = Vec::with_capacity(argument_count as usize);
        for _ in 0..argument_count {
            let length = read_i32(&mut packet)?;
            if length == -1 {
                arguments.push(None);
                continue;
            }
            if length < -1 || packet.remaining() < length as usize {
                return Err(function_call_protocol_error(
                    "invalid function call argument length",
                ));
            }
            arguments.push(Some(packet.split_to(length as usize).to_vec()));
        }

        let result_format = read_i16(&mut packet)?;
        if packet.has_remaining() {
            return Err(function_call_protocol_error(
                "unexpected trailing function call bytes",
            ));
        }
        if !argument_formats.is_empty() && argument_formats.iter().any(|format| *format != 1) {
            return Err(function_call_protocol_error(
                "only binary fastpath arguments are supported",
            ));
        }
        if result_format != 1 {
            return Err(function_call_protocol_error(
                "only binary fastpath results are supported",
            ));
        }

        Ok(Some(Self {
            function_oid,
            arguments,
        }))
    }

    fn arg_bytes(&self, index: usize) -> PgWireResult<&[u8]> {
        self.arguments
            .get(index)
            .ok_or_else(|| function_call_protocol_error("missing function call argument"))?
            .as_deref()
            .ok_or_else(|| function_call_protocol_error("null function call argument"))
    }

    fn arg_i32(&self, index: usize) -> PgWireResult<i32> {
        let bytes = self.arg_bytes(index)?;
        if bytes.len() != 4 {
            return Err(function_call_protocol_error(
                "expected 4-byte function call argument",
            ));
        }
        Ok(i32::from_be_bytes(
            bytes
                .try_into()
                .expect("validated function call int4 length"),
        ))
    }

    fn arg_u32(&self, index: usize) -> PgWireResult<u32> {
        let bytes = self.arg_bytes(index)?;
        if bytes.len() != 4 {
            return Err(function_call_protocol_error(
                "expected 4-byte function call argument",
            ));
        }
        Ok(u32::from_be_bytes(
            bytes
                .try_into()
                .expect("validated function call oid length"),
        ))
    }

    fn arg_i64(&self, index: usize) -> PgWireResult<i64> {
        let bytes = self.arg_bytes(index)?;
        if bytes.len() != 8 {
            return Err(function_call_protocol_error(
                "expected 8-byte function call argument",
            ));
        }
        Ok(i64::from_be_bytes(
            bytes
                .try_into()
                .expect("validated function call int8 length"),
        ))
    }

    fn require_args(&self, expected: usize) -> PgWireResult<()> {
        if self.arguments.len() != expected {
            return Err(function_call_protocol_error(
                "unexpected function call argument count",
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct LargeObjectFunctionQuery {
    sql: String,
    result: FastpathResultKind,
}

impl LargeObjectFunctionQuery {
    fn try_from_call(call: &FunctionCall) -> PgWireResult<Self> {
        let result = match call.function_oid {
            LO_OPEN_OID => {
                call.require_args(2)?;
                Self::int4(format!(
                    "SELECT pg_catalog.lo_open({}::oid, {}::integer)",
                    call.arg_u32(0)?,
                    call.arg_i32(1)?
                ))
            }
            LO_CLOSE_OID => {
                call.require_args(1)?;
                Self::int4(format!(
                    "SELECT pg_catalog.lo_close({}::integer)",
                    call.arg_i32(0)?
                ))
            }
            LOREAD_OID => {
                call.require_args(2)?;
                Self {
                    sql: format!(
                        "SELECT encode(pg_catalog.loread({}::integer, {}::integer), 'hex')",
                        call.arg_i32(0)?,
                        call.arg_i32(1)?
                    ),
                    result: FastpathResultKind::HexBytes,
                }
            }
            LOWRITE_OID => {
                call.require_args(2)?;
                Self::int4(format!(
                    "SELECT pg_catalog.lowrite({}::integer, decode('{}', 'hex'))",
                    call.arg_i32(0)?,
                    hex_from_bytes(call.arg_bytes(1)?)
                ))
            }
            LO_LSEEK_OID => {
                call.require_args(3)?;
                Self::int4(format!(
                    "SELECT pg_catalog.lo_lseek({}::integer, {}::integer, {}::integer)",
                    call.arg_i32(0)?,
                    call.arg_i32(1)?,
                    call.arg_i32(2)?
                ))
            }
            LO_CREAT_OID => {
                call.require_args(1)?;
                Self::oid(format!(
                    "SELECT pg_catalog.lo_creat({}::integer)",
                    call.arg_i32(0)?
                ))
            }
            LO_CREATE_OID => {
                call.require_args(1)?;
                Self::oid(format!(
                    "SELECT pg_catalog.lo_create({}::oid)",
                    call.arg_u32(0)?
                ))
            }
            LO_TELL_OID => {
                call.require_args(1)?;
                Self::int4(format!(
                    "SELECT pg_catalog.lo_tell({}::integer)",
                    call.arg_i32(0)?
                ))
            }
            LO_UNLINK_OID => {
                call.require_args(1)?;
                Self::int4(format!(
                    "SELECT pg_catalog.lo_unlink({}::oid)",
                    call.arg_u32(0)?
                ))
            }
            LO_TRUNCATE_OID => {
                call.require_args(2)?;
                Self::int4(format!(
                    "SELECT pg_catalog.lo_truncate({}::integer, {}::integer)",
                    call.arg_i32(0)?,
                    call.arg_i32(1)?
                ))
            }
            LO_LSEEK64_OID => {
                call.require_args(3)?;
                Self::int8(format!(
                    "SELECT pg_catalog.lo_lseek64({}::integer, {}::bigint, {}::integer)",
                    call.arg_i32(0)?,
                    call.arg_i64(1)?,
                    call.arg_i32(2)?
                ))
            }
            LO_TELL64_OID => {
                call.require_args(1)?;
                Self::int8(format!(
                    "SELECT pg_catalog.lo_tell64({}::integer)",
                    call.arg_i32(0)?
                ))
            }
            LO_TRUNCATE64_OID => {
                call.require_args(2)?;
                Self::int4(format!(
                    "SELECT pg_catalog.lo_truncate64({}::integer, {}::bigint)",
                    call.arg_i32(0)?,
                    call.arg_i64(1)?
                ))
            }
            _ => {
                return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                    "ERROR".to_owned(),
                    "0A000".to_owned(),
                    format!(
                        "fastpg does not support function call protocol for function oid {}",
                        call.function_oid
                    ),
                ))));
            }
        };

        Ok(result)
    }

    fn int4(sql: String) -> Self {
        Self {
            sql,
            result: FastpathResultKind::Int4,
        }
    }

    fn int8(sql: String) -> Self {
        Self {
            sql,
            result: FastpathResultKind::Int8,
        }
    }

    fn oid(sql: String) -> Self {
        Self {
            sql,
            result: FastpathResultKind::Oid,
        }
    }

    fn encode_result(&self, value: Option<String>) -> PgWireResult<Vec<u8>> {
        let Some(value) = value else {
            return Err(function_call_protocol_error(
                "large object function returned null",
            ));
        };

        match self.result {
            FastpathResultKind::Int4 => value
                .parse::<i32>()
                .map(|value| value.to_be_bytes().to_vec())
                .map_err(function_call_parse_error),
            FastpathResultKind::Int8 => value
                .parse::<i64>()
                .map(|value| value.to_be_bytes().to_vec())
                .map_err(function_call_parse_error),
            FastpathResultKind::Oid => value
                .parse::<u32>()
                .map(|value| value.to_be_bytes().to_vec())
                .map_err(function_call_parse_error),
            FastpathResultKind::HexBytes => bytes_from_hex(value.trim()),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum FastpathResultKind {
    Int4,
    Int8,
    Oid,
    HexBytes,
}

fn function_call_single_value(execution: QueryExecution) -> PgWireResult<Option<String>> {
    match execution {
        QueryExecution::Rows(result) => {
            let Some(row) = result.rows.first() else {
                return Ok(None);
            };
            let Some(value) = row.first() else {
                return Ok(None);
            };
            Ok(match value {
                Value::Int2(value) => Some(value.to_string()),
                Value::Int4(value) => Some(value.to_string()),
                Value::Int8(value) => Some(value.to_string()),
                Value::Text(value) => Some(value.clone()),
                Value::Null => None,
            })
        }
        QueryExecution::Error {
            sqlstate,
            message,
            detail,
            hint,
            context,
            cursorpos,
        } => {
            let mut error = ErrorInfo::new("ERROR".to_owned(), sqlstate, message);
            error.detail = detail;
            error.hint = hint;
            error.where_context = context;
            if cursorpos > 0 {
                error.position = Some(cursorpos.to_string());
            }
            Err(PgWireError::UserError(Box::new(error)))
        }
        QueryExecution::Unsupported { query } => {
            Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".to_owned(),
                "0A000".to_owned(),
                format!("feature not supported by fastpg test server yet: {query}"),
            ))))
        }
        QueryExecution::InvalidParameters { message } => Err(PgWireError::UserError(Box::new(
            ErrorInfo::new("ERROR".to_owned(), "08P01".to_owned(), message),
        ))),
        QueryExecution::WithNotices { execution, .. } => function_call_single_value(*execution),
        _ => Err(function_call_protocol_error(
            "large object function did not return a row",
        )),
    }
}

fn read_i16(buf: &mut BytesMut) -> PgWireResult<i16> {
    if buf.remaining() < 2 {
        return Err(function_call_protocol_error("truncated function call"));
    }
    Ok(buf.get_i16())
}

fn read_i32(buf: &mut BytesMut) -> PgWireResult<i32> {
    if buf.remaining() < 4 {
        return Err(function_call_protocol_error("truncated function call"));
    }
    Ok(buf.get_i32())
}

fn read_u32(buf: &mut BytesMut) -> PgWireResult<u32> {
    if buf.remaining() < 4 {
        return Err(function_call_protocol_error("truncated function call"));
    }
    Ok(buf.get_u32())
}

fn read_i16_vec(buf: &mut BytesMut) -> PgWireResult<Vec<i16>> {
    let count = read_i16(buf)?;
    if count < 0 {
        return Err(function_call_protocol_error(
            "negative function call format count",
        ));
    }
    (0..count).map(|_| read_i16(buf)).collect()
}

fn function_call_protocol_error(message: &str) -> PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new(
        "ERROR".to_owned(),
        "08P01".to_owned(),
        message.to_owned(),
    )))
}

fn function_call_parse_error(error: impl std::error::Error + Send + Sync + 'static) -> PgWireError {
    PgWireError::ApiError(Box::new(error))
}

fn hex_from_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn bytes_from_hex(hex: &str) -> PgWireResult<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return Err(function_call_protocol_error(
            "large object function returned invalid hex",
        ));
    }

    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for chunk in hex.as_bytes().chunks_exact(2) {
        let high = hex_digit(chunk[0])?;
        let low = hex_digit(chunk[1])?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn hex_digit(digit: u8) -> PgWireResult<u8> {
    match digit {
        b'0'..=b'9' => Ok(digit - b'0'),
        b'a'..=b'f' => Ok(digit - b'a' + 10),
        b'A'..=b'F' => Ok(digit - b'A' + 10),
        _ => Err(function_call_protocol_error(
            "large object function returned invalid hex",
        )),
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

fn execution_to_response(execution: QueryExecution, format: FieldFormat) -> PgWireResult<Response> {
    match execution {
        QueryExecution::WithNotices { execution, .. } => execution_to_response(*execution, format),
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
            context,
            cursorpos,
        } => Ok(error_response(
            &sqlstate, &message, detail, hint, context, cursorpos,
        )),
    }
}

async fn send_notices<C>(client: &mut C, notices: &[QueryNotice]) -> PgWireResult<()>
where
    C: Sink<PgWireBackendMessage> + Unpin + Send,
    C::Error: Debug,
    PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
{
    for notice in notices {
        let mut info = ErrorInfo::new(
            notice.severity.clone(),
            notice.sqlstate.clone(),
            notice.message.clone(),
        );
        info.detail = notice.detail.clone();
        info.hint = notice.hint.clone();
        info.where_context = notice.context.clone();
        info.severity_nonlocalized = Some(notice.severity.clone());
        if notice.cursorpos > 0 {
            info.position = Some(notice.cursorpos.to_string());
        }
        client
            .send(PgWireBackendMessage::NoticeResponse(info.into()))
            .await?;
    }
    Ok(())
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
    context: Option<String>,
    cursorpos: i32,
) -> Response {
    let mut error = ErrorInfo::new("ERROR".to_owned(), sqlstate.to_owned(), message.to_owned());
    error.detail = detail;
    error.hint = hint;
    error.where_context = context;
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

    #[test]
    fn decodes_large_object_function_call_message() {
        let mut message = BytesMut::new();
        message.put_u8(FUNCTION_CALL_MESSAGE_TYPE);
        message.put_i32(24);
        message.put_u32(LO_UNLINK_OID);
        message.put_i16(1);
        message.put_i16(1);
        message.put_i16(1);
        message.put_i32(4);
        message.put_u32(42);
        message.put_i16(1);

        let call = FunctionCall::decode(&mut message)
            .expect("decode should succeed")
            .expect("message should be complete");

        assert_eq!(call.function_oid, LO_UNLINK_OID);
        assert_eq!(call.arg_u32(0).unwrap(), 42);
        assert!(message.is_empty());
    }

    #[test]
    fn large_object_function_query_encodes_binary_results() {
        let call = FunctionCall {
            function_oid: LO_UNLINK_OID,
            arguments: vec![Some(42_u32.to_be_bytes().to_vec())],
        };
        let query = LargeObjectFunctionQuery::try_from_call(&call).unwrap();

        assert_eq!(query.sql, "SELECT pg_catalog.lo_unlink(42::oid)");
        assert_eq!(
            query.encode_result(Some("1".to_owned())).unwrap(),
            vec![0, 0, 0, 1]
        );
    }

    #[test]
    fn large_object_read_result_decodes_hex_payload() {
        let query = LargeObjectFunctionQuery {
            sql: "SELECT encode(pg_catalog.loread(1, 4), 'hex')".to_owned(),
            result: FastpathResultKind::HexBytes,
        };

        assert_eq!(
            query.encode_result(Some("00af10ff".to_owned())).unwrap(),
            vec![0x00, 0xaf, 0x10, 0xff]
        );
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
