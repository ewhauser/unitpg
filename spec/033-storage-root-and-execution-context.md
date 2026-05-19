# `spec/033-storage-root-and-execution-context.md`

# Storage Root And Execution Context

## Summary

Replace the bootstrap process-global storage state with an explicit shared
`Storage` root owned by the Rust server, plus execution-local context for
PostgreSQL table AM callbacks.

`032-storage-memory-ownership-accounting.md` defines how rows and values are
owned. This spec defines who owns the storage root and how C callbacks find the
right session, transaction, epoch, and statement context.

## Goals

```text
one shared Storage root per Rust server
one SessionStorage per client session
no silent fallback to a default session during server execution
C table AM callbacks require an active execution context
storage ownership composes with pgcore single-lane execution
tests can still use explicit helper contexts
all committed state lives under Storage, not loose globals
```

## Non-Goals

```text
do not add multiple pgcore lanes
do not add PostgreSQL shared memory
do not add WAL, heap pages, or vacuum
do not implement fixture or epoch semantics here
do not change SQL behavior beyond context ownership
```

## Current Problem

The current Rust storage implementation has session-owned overlays, but the
committed storage root is still a process-global `OnceLock<Mutex<StorageState>>`.
There is also a default session fallback for callbacks that run without an
installed session context.

That is convenient for bootstrap code and unit tests, but it makes ownership
too implicit:

```text
which server owns committed rows?
which session owns uncommitted rows?
which epoch does this statement read?
which transaction should receive writes?
what happens if a C callback runs after error cleanup?
```

The answer should come from an explicit execution context, not from globals.

## Target Data Model

```rust
struct Storage {
    committed: RwLock<CommittedStorage>,
    regions: StorageRegionRegistry,
    metrics: StorageMetrics,
}

struct SessionStorage {
    session_id: SessionId,
    current_epoch: Option<EpochId>,
    transaction: Option<TransactionStorage>,
    scan_handles: ScanRegistry,
}

struct ExecutionStorageContext {
    storage: Arc<Storage>,
    session_storage: Arc<Mutex<SessionStorage>>,
    statement_snapshot: StorageSnapshot,
}
```

`ServerState` owns `Arc<Storage>`. Each `SessionState` owns one
`SessionStorage`. `QueryExecutorShared` or an equivalent server-level object
passes the shared storage root to each per-session executor.

## Callback Contract

PostgreSQL C table AM callbacks may access Rust storage only while an
`ExecutionStorageContext` is installed:

```rust
let _guard = fastpg_storage::enter_execution_context(cx);
postgres_executor_runs();
```

Rules:

```text
callbacks fail loudly if no execution context is installed
the guard clears context during normal return and error unwinding
the context is lane-local or thread-local, never process-global mutable state
the context contains both shared Storage and the current session storage
tests use explicit test contexts instead of relying on production fallbacks
```

## Server Ownership

The server shape becomes:

```text
ServerState
  -> Arc<Storage>
  -> Arc<QueryExecutorShared>

SessionState
  -> Arc<ServerState>
  -> Arc<Mutex<SessionStorage>>
  -> QueryExecutor
```

The pgcore session should carry the storage context it needs, but C handles
remain lane-owned as described in `029-single-pgcore-execution-lane.md`.

## Removal Of Bootstrap Globals

The process-global storage shim may remain only as a short migration layer:

```text
allowed:
  unit-test helper that creates a test Storage and SessionStorage
  temporary internal adapter while call sites are moved

not allowed:
  production server writes committed rows into a global OnceLock
  callbacks silently create a default session
  prepared statements store raw pointers to a session-owned context
```

When the migration completes, callbacks without context should return an
internal fastpg error, not invent a session.

## Acceptance Tests

```text
two ServerState values do not share committed storage
two sessions on one ServerState share committed rows after commit
two sessions do not share uncommitted transaction overlays
C callback without execution context fails clearly in debug/test mode
error during PostgreSQL execution clears the active storage context
disconnecting a session drops its SessionStorage and open scan handles
```

## Migration Steps

```text
1. Introduce Storage as an explicit shared root.
2. Add Storage to ServerState or QueryExecutorShared.
3. Move committed rows from global StorageState into Storage.
4. Replace enter_session_storage with enter_execution_context.
5. Remove default-session fallback from production paths.
6. Convert tests to create explicit Storage plus SessionStorage contexts.
7. Add assertions that callbacks cannot run without context.
```
