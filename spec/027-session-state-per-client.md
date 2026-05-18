# `spec/027-session-state-per-client.md`

# Per-Client Session State

## Summary

Every client connection must own an independent `SessionState`.

The Rust server may share immutable configuration, the database root, catalog
snapshots, committed storage, metrics, and the pgcore execution lane across
clients. It must not share mutable per-session state such as transactions,
prepared statements, portals, temporary objects, COPY state, session variables,
or current epoch membership.

The current shape where `FastPgWireHandler` owns one `Arc<SessionState>` is a
bootstrap shortcut. It must be replaced with a handler-level `ServerState` and a
client-attached `SessionState`.

## Goals

```text
one SessionState per accepted pgwire client
no cross-client prepared statement, portal, COPY, transaction, GUC, or temp state
shared ServerState for database/catalog/storage roots
stable session identity for tracing, errors, and storage transaction ownership
pgcore access still serialized through the single pgcore lane
```

## Non-Goals

```text
do not create one PostgreSQL backend process per connection
do not create one pgcore C-global universe per connection
do not make catalog or committed storage private to a client
do not add multiple pgcore execution lanes as part of this change
```

## Current Problem

`fastpg-wire` currently constructs a single session inside the protocol
handler:

```rust
struct FastPgWireHandler {
    session: Arc<SessionState>,
    query_parser: Arc<NoopQueryParser>,
}
```

That makes all clients share one `QueryExecutor` and whatever state hangs below
it. That is acceptable for the first smoke path but incorrect for a database
server, even a single-process test server.

The server process is single process, not single session.

## Target Architecture

```rust
struct ServerState {
    database: Arc<Database>,
    pgcore_lane: Arc<PgCoreLane>,
    config: Arc<ServerConfig>,
    metrics: Arc<Metrics>,
}

struct FastPgWireHandler {
    server: Arc<ServerState>,
    query_parser: Arc<NoopQueryParser>,
}

struct SessionState {
    id: SessionId,
    server: Arc<ServerState>,
    current_user: String,
    current_database: String,
    search_path: SearchPath,
    gucs: SessionGucState,
    transaction: SessionTransactionState,
    prepared_statements: PreparedStatementMap,
    portals: PortalMap,
    copy: Option<CopyState>,
    temp_namespace: Option<NamespaceId>,
    epoch: Option<EpochHandle>,
}
```

`FastPgWireHandler` is shared by the listener. `SessionState` is created during
startup for each client and stored in the pgwire client context.

## Lifecycle

```text
accept socket
  create pgwire client object
  process startup packet
  create SessionState
  attach SessionState to client metadata
  handle SimpleQuery / Parse / Bind / Execute / Copy using that SessionState
  drop SessionState when the client disconnects
```

Dropping a session must:

```text
abort active COPY
rollback active transaction
release portals and prepared statements
release temporary objects
leave current epoch
emit metrics/tracing close event
```

## pgwire Integration

The handler should expose a helper equivalent to:

```rust
fn session_for_client<C>(client: &C) -> PgWireResult<Arc<SessionState>>;
```

`post_startup` creates and attaches the session:

```rust
async fn post_startup<C>(&self, client: &mut C, message: PgWireFrontendMessage) {
    let startup = StartupParameters::from(message);
    let session = Arc::new(SessionState::new(self.server.clone(), startup));
    client.metadata_mut().insert(session);
}
```

Every query path retrieves the session from the client:

```rust
async fn do_query<C>(&self, client: &mut C, sql: &str) -> PgWireResult<Vec<Response>> {
    let session = session_for_client(client)?;
    let result = session.execute(sql, &[])?;
    Ok(encode(result)?)
}
```

COPY state should also move into `SessionState` unless pgwire requires a
client-attached helper for streaming. If a pgwire helper remains client-attached,
it must point back to the owning session and must not be stored in
`FastPgWireHandler`.

## Session State Ownership

Per-client:

```text
transaction state
current statement snapshot
prepared statements
portals
COPY state
GUCs and ParameterStatus values
search_path
current database and user
temporary namespace and temporary relations
current epoch membership
last insert/update command tag state if needed for protocol compatibility
```

Shared:

```text
server config
listener sockets
committed database state
catalog snapshots
committed storage
fixture base state
pgcore execution lane
metrics registry
```

## Transaction Boundary

`SessionState` owns the current transaction handle. The handle may reference
shared committed storage, but uncommitted writes are owned by the session.

This spec composes with `028-transaction-owned-storage-state.md`.

## pgcore Boundary

`PgCoreSession` is a logical Rust wrapper for prepared handles and per-session
PostgreSQL state. It does not imply concurrent entry into C PostgreSQL globals.

Calls from many `SessionState` values still enter the single pgcore lane:

```text
client A SessionState
client B SessionState
client C SessionState
        |
        v
single PgCoreLane
        |
        v
PostgreSQL parser/analyzer/planner/executor globals
```

## Required API Changes

```text
FastPgWireHandler::new(server_state) no longer creates SessionState
StartupHandler creates one SessionState per client
SimpleQueryHandler retrieves SessionState from the client
ExtendedQueryHandler retrieves SessionState from the client
COPY handlers retrieve SessionState from the client
QueryExecutor becomes per-session or is split into per-session and shared parts
```

`SessionState::new` should take shared server state:

```rust
impl SessionState {
    pub fn new(server: Arc<ServerState>, startup: StartupParameters) -> Self;
}
```

## Acceptance Tests

```text
two simultaneous clients can run independent transactions
client A BEGIN/INSERT is not visible to client B before COMMIT
client A ROLLBACK does not remove client B committed data
client A prepared statement name does not collide with client B prepared statement name
client A COPY state does not affect client B queries
client A SET application_name does not change client B ParameterStatus
disconnecting a client rolls back only that client's active transaction
```

## Regression Coverage

Add a Rust integration test that opens two pgwire clients against the same
server process and proves:

```sql
-- client A
BEGIN;
CREATE TEMP TABLE session_private(id int);
PREPARE s AS SELECT 1;

-- client B
SELECT to_regclass('session_private'); -- NULL
PREPARE s AS SELECT 2;                 -- succeeds independently
```

Add a storage-level test that two `SessionState` values share committed rows but
have separate uncommitted overlays.

## Performance Notes

Session creation must be cheap:

```text
allocate small Rust structs
clone Arc<ServerState>
do not initialize a full PostgreSQL backend
do not rebuild virtual pg_catalog
do not clone committed storage
```

Prepared statement caches are per session, but their metadata may reference
shared catalog snapshots and shared plan cache entries only when the cache key
includes catalog generation and session-sensitive state.

## Migration Steps

```text
1. Introduce ServerState and SessionId.
2. Move FastPgWireHandler.session into per-client metadata.
3. Move COPY state behind SessionState or session-owned client metadata.
4. Split QueryExecutor into shared database references plus per-session state.
5. Add two-client isolation tests.
6. Delete the old handler-level session field.
```

