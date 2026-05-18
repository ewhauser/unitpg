# `spec/029-single-pgcore-execution-lane.md`

# Single pgcore Execution Lane

## Summary

Keep exactly one pgcore execution lane for now.

The Rust server may run many Tokio client sessions concurrently, but all entry
into reused PostgreSQL C backend code must be serialized until PostgreSQL
globals, caches, memory contexts, resource owners, and callback state are either
isolated per lane or proven thread-safe.

This is a correctness boundary, not the final performance architecture.

## Goals

```text
preserve the Tokio single-process server architecture
allow many clients to be connected at once
serialize PostgreSQL C backend execution through one lane
make lane queue time and execution time measurable
make prepared/planned C handles lane-owned
avoid accidental concurrent access to PostgreSQL backend globals
```

## Non-Goals

```text
do not add multiple pgcore lanes yet
do not run PostgreSQL executor concurrently on multiple threads
do not fork PostgreSQL backend processes
do not remove Rust catalog or Rust storage
do not use PostgreSQL shared memory as a workaround
```

## Why One Lane

The PostgreSQL backend code being reused assumes backend-global state such as:

```text
CurrentMemoryContext
CurrentResourceOwner
transaction state globals
snapshot globals
relcache/syscache/typcache process state
GUC state
error context stack
portal and executor globals
```

Some of this can eventually be made lane-local or session-local, but that work
must be explicit. Until then, one serialized lane is the only safe shape.

## Target Architecture

```rust
struct PgCoreLane {
    worker: PgCoreWorker,
    queue: LaneQueue,
    metrics: PgCoreLaneMetrics,
}

struct PgCoreRequest {
    session_id: SessionId,
    catalog_generation: u64,
    storage_context: ExecutionStorageContext,
    operation: PgCoreOperation,
}

enum PgCoreOperation {
    Prepare { sql: String, parameter_types: Vec<Oid> },
    Describe { prepared: PreparedHandle },
    Execute { prepared: PreparedHandle, params: Vec<PgCoreParam> },
    SimpleQuery { sql: String },
}
```

The lane may initially be a `Mutex` around direct calls. The API should still
look like an execution lane so it can become a dedicated worker thread without
changing callers.

```text
Tokio session task
  -> await PgCoreLane::run(...)
  -> one serialized C PostgreSQL execution section
  -> return Rust-owned result
```

## Lane Ownership Rules

```text
only PgCoreLane may call fastpg-pgcore C FFI entry points
C handles are created, used, and freed on the lane
prepared statement handles are lane-affine
executor lifecycle runs entirely on the lane
storage tableam callbacks run while the lane has an active execution context
no async await occurs while C PostgreSQL is mid-execution
```

Rust result data returned to pgwire must be Rust-owned. It must not borrow from
PostgreSQL `MemoryContext` storage after the lane operation finishes.

## Session Interaction

Each `SessionState` has logical pgcore state:

```rust
struct SessionPgCoreState {
    prepared: HashMap<String, PreparedHandle>,
    portals: HashMap<String, PortalHandle>,
    guc_overlay: SessionGucState,
}
```

The logical session state can be owned by the Rust session, but every operation
that touches C handles must enter the lane.

Prepared handles should be opaque and lane-affine:

```rust
struct PreparedHandle {
    id: PreparedId,
    lane_id: PgCoreLaneId,
    catalog_generation: u64,
}
```

Avoid broad `Send`/`Sync` claims on raw prepared C pointers. If handles must be
stored behind Arcs for pgwire compatibility, the Arc should contain an opaque
identifier and all pointer dereference should happen on the lane.

## Catalog and Storage Boundaries

Before entering C execution, the lane request must include:

```text
session identity
current catalog generation
catalog snapshot or provider handle
storage execution context
transaction snapshot
session GUC/search_path state needed by analyzer/planner
```

The lane installs these as the active fastpg context, runs PostgreSQL, copies
the result into Rust-owned data, and clears the active context.

## Metrics

The lane must report:

```text
queue wait time
C execution time
operation kind
statement count
row count
error SQLSTATE
catalog generation used
storage commit generation used
```

This is required so we can tell whether performance bottlenecks are:

```text
waiting for the single lane
PostgreSQL C execution
Rust storage callbacks
result materialization
pgwire encoding
client/network behavior
```

## When Multiple Lanes Are Allowed

Do not add multiple lanes until all of these have a design and tests:

```text
PostgreSQL backend globals are isolated per lane or protected
MemoryContext roots are lane-local
ResourceOwner roots are lane-local
transaction/snapshot state is lane-local
relcache/syscache/typcache invalidation is generation-aware per lane
active Rust catalog/storage callback context is lane-local
C error handling cannot cross lane state
prepared handles are tied to exactly one lane or can be rebuilt on another lane
```

Multiple lanes should be added only after the no-internal-IPC guard and
regression harness pass with one lane.

## Acceptance Tests

```text
many clients can issue queries concurrently without overlapping C FFI entry
instrumentation proves at most one active pgcore operation at a time
query errors clear the lane active context
panic/error in Rust storage callback clears the lane active context
prepared statements remain usable across many operations on the same lane
prepared handles cannot be executed on an unknown lane
```

## Performance Tests

Add a benchmark mode that reports:

```text
total TPS
median lane queue wait
p95 lane queue wait
median C execution time
p95 C execution time
percent time waiting for lane
```

Single-client pgbench should have near-zero lane queue wait. Multi-client
pgbench may show queueing; that is acceptable until the multi-lane isolation
work is complete.

## Migration Steps

```text
1. Wrap the existing pgcore lock in a named PgCoreLane API.
2. Add active-operation assertions and metrics.
3. Make fastpg-exec call pgcore only through PgCoreLane.
4. Replace raw C pointer Send/Sync exposure with lane-affine opaque handles.
5. Add concurrent-client tests that assert serialized C entry.
6. Document multi-lane prerequisites before changing lane count.
```

