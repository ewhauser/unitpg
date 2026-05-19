#![forbid(unsafe_code)]

use std::fmt::Debug;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fastpg_session::{
    Column, CopyTarget, PgType, QueryDescription, QueryExecution, QueryResult, ServerState,
    SessionState, Value,
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
}

impl FastPgWireHandler {
    pub fn new(server_version: impl Into<String>) -> Self {
        Self {
            server: Arc::new(ServerState::new(server_version)),
            query_parser: Arc::new(NoopQueryParser::new()),
        }
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
        _message: PgWireFrontendMessage,
    ) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        session_for_client(client, &self.server);
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
        let session = session_for_client(client, &self.server);
        let execution = session.execute(query, &[]);
        remember_copy_target(client, &execution);
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
        let session = session_for_client(client, &self.server);
        let Some(description) = session.describe(&target.statement) else {
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
        let session = session_for_client(client, &self.server);
        let Some(description) = session.describe(&target.statement.statement) else {
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
        let session = session_for_client(client, &self.server);
        let query = &portal.statement.statement;
        let parameters = match session.describe(query) {
            Some(description) => decode_parameters(portal, &description)?,
            None => vec![],
        };
        let execution = session.execute(query, &parameters);
        remember_copy_target(client, &execution);

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
        let session = session_for_client(client, &self.server);
        let active_copy = active_copy(client)?;
        let mut active_copy = active_copy
            .lock()
            .expect("fastpg active COPY mutex poisoned");
        active_copy
            .push_data(&session, copy_data.data.as_ref())
            .map_err(copy_data_error)?;
        Ok(())
    }

    async fn on_copy_done<C>(&self, client: &mut C, _done: CopyDone) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let session = session_for_client(client, &self.server);
        let rows = {
            let active_copy = active_copy(client)?;
            let mut active_copy = active_copy
                .lock()
                .expect("fastpg active COPY mutex poisoned");
            active_copy.finish(&session).map_err(copy_data_error)?
        };
        session.finish_copy();

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
        let session = session_for_client(client, &self.server);
        session.abort_copy();
        PgWireError::UserError(Box::new(ErrorInfo::new(
            "ERROR".to_owned(),
            "57014".to_owned(),
            format!("COPY cancelled by client: {}", fail.message),
        )))
    }
}

fn session_for_client<C>(client: &C, server: &Arc<ServerState>) -> Arc<SessionState>
where
    C: ClientInfo,
{
    let server = server.clone();
    client
        .session_extensions()
        .get_or_insert_with(move || SessionState::for_server(server))
}

fn parameter_types(description: &QueryDescription) -> Vec<Type> {
    description
        .parameter_types
        .iter()
        .copied()
        .map(to_pgwire_type)
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
        to_pgwire_type(field.data_type),
        format,
    )
}

fn to_pgwire_type(data_type: PgType) -> Type {
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
        QueryExecution::Command { tag, rows } => Ok(command_complete(&tag, rows)),
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
    let rows = result
        .rows
        .iter()
        .map(|row| encode_row(schema.clone(), &result.fields, row))
        .collect::<Vec<_>>();

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
    let mut encoder = DataRowEncoder::new(schema);
    for (field, value) in fields.iter().zip(values) {
        encode_value(&mut encoder, field.data_type, value)?;
    }
    Ok(encoder.take_row())
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

pub fn command_complete(tag: &str, rows: usize) -> Response {
    let tag = if tag == "INSERT" {
        Tag::new(tag).with_oid(0).with_rows(rows)
    } else {
        Tag::new(tag).with_rows(rows)
    };
    Response::Execution(tag)
}

#[derive(Debug)]
struct ActiveCopy {
    target: CopyTarget,
    pending: String,
    rows: usize,
}

impl ActiveCopy {
    fn new(target: CopyTarget) -> Self {
        Self {
            target,
            pending: String::new(),
            rows: 0,
        }
    }

    fn push_data(&mut self, session: &SessionState, data: &[u8]) -> Result<(), String> {
        let chunk = std::str::from_utf8(data).map_err(|error| error.to_string())?;
        self.pending.push_str(chunk);

        while let Some(newline) = self.pending.find('\n') {
            let line = self.pending[..newline].trim_end_matches('\r').to_owned();
            self.pending.drain(..=newline);
            self.process_line(session, &line)?;
        }

        Ok(())
    }

    fn finish(&mut self, session: &SessionState) -> Result<usize, String> {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.process_line(session, line.trim_end_matches('\r'))?;
        }
        Ok(self.rows)
    }

    fn process_line(&mut self, session: &SessionState, line: &str) -> Result<(), String> {
        if session.copy_text_line(&self.target.table, line)? {
            self.rows += 1;
        }
        Ok(())
    }
}

fn remember_copy_target<C>(client: &mut C, execution: &QueryExecution)
where
    C: ClientInfo,
{
    if let QueryExecution::CopyIn(target) = execution {
        client
            .session_extensions()
            .insert(Mutex::new(ActiveCopy::new(target.clone())));
    }
}

fn active_copy<C>(client: &C) -> PgWireResult<Arc<Mutex<ActiveCopy>>>
where
    C: ClientInfo,
{
    client
        .session_extensions()
        .get::<Mutex<ActiveCopy>>()
        .ok_or_else(|| {
            PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".to_owned(),
                "08P01".to_owned(),
                "COPY data received without an active COPY target".to_owned(),
            )))
        })
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

    fn command_complete_tag(response: Response) -> String {
        let Response::Execution(tag) = response else {
            panic!("expected execution response");
        };
        let command_complete = CommandComplete::from(tag);
        command_complete.tag
    }
}
