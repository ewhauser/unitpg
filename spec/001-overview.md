Below is a draft for `spec/001-overview.md`. It frames the project as a Rust-led Postgres-compatible **unit-test server**, not as a production Postgres distribution.

# `spec/001-overview.md`

# Rust Postgres-Compatible Test Server Overview

## Summary

Build a Rust-led database server that is **Postgres-compatible enough for application unit tests** by reusing PostgreSQL's SQL semantics while replacing PostgreSQL's durable physical storage with in-memory test infrastructure.

The server speaks the PostgreSQL wire protocol, accepts ordinary Postgres client drivers, runs in one process, uses Tokio for async networking and scheduling, and stores test data in an in-memory engine optimized for fast fixture reset, per-test isolation, and parallel test execution.

SQL execution should reuse as much PostgreSQL C code as practical above the physical storage boundary. The initial target is to reuse PostgreSQL's parser, analyzer, rewriter, planner/optimizer, expression machinery, and substantial executor infrastructure, while replacing heap storage, catalog persistence, WAL, buffers, pagers, and vacuum.

This project is separate from the existing fast-fork PostgreSQL work.

The existing fast-fork direction optimizes inside PostgreSQL's architecture. This project takes the more aggressive route:

```text
keep:
  PostgreSQL wire protocol
  PostgreSQL SQL syntax where feasible
  PostgreSQL parser
  PostgreSQL analyzer and rewriter
  PostgreSQL planner/optimizer
  PostgreSQL expression evaluation and executor nodes where storage-independent
  PostgreSQL-compatible type names, catalog views, errors, and client behavior
  relcache/syscache-style caches when backed by in-memory catalog data

replace:
  PostgreSQL process-per-connection model
  heap pages
  shared buffers
  WAL
  CLOG
  ProcArray
  MVCC tuple headers
  btree pages
  HOT updates
  TOAST tables
  vacuum
  physical pg_catalog storage
  physical relfilenodes
  checkpointer/bgwriter/autovacuum background machinery
```

The server is explicitly for disposable test databases. It does not attempt to be a production database.

## Project Name

Working name:

```text
fastpg
```

Alternative names:

```text
pgtestd
rustpgtest
testgres
ffpg
```

The specs should use `fastpg` until the actual repository name is chosen.

## Motivation

Application unit tests need a database that behaves enough like Postgres to satisfy:

```text
client drivers
ORMs
migration frameworks
application SQL
transaction wrappers
fixture setup
parallel test workers
schema introspection
constraint enforcement
```

They usually do **not** need:

```text
durability
crash recovery
replication
physical heap pages
WAL correctness
vacuum semantics
btree page shape
HOT update behavior
storage extensions
logical decoding
production isolation levels
permission/security fidelity
```

The existing PostgreSQL fork removes many durability costs, but it still pays for much of PostgreSQL’s physical storage and multi-process architecture. To get another order-of-magnitude improvement, this project should avoid those costs entirely.

The core idea is:

```text
Postgres SQL semantics,
test-optimized in-memory runtime.
```

## External Dependencies

### `pgwire`

Use `pgwire` for PostgreSQL wire protocol support.

`pgwire` describes itself as a Rust library for building Postgres-compatible access layers, “like hyper, but for postgres wire protocol.” Its README lists backend TCP/TLS server support on Tokio, simple query and extended query support, startup/auth APIs, result-set encoding, cancellation, copy APIs, notifications, and transaction-state support. ([GitHub][1])

The server must implement the `pgwire` startup and query handlers:

```text
StartupHandler
SimpleQueryHandler
ExtendedQueryHandler
QueryParser / prepared statement support
ResultSet encoding
Error and Notice encoding
Cancel handling
Transaction state reporting
```

The `pgwire` docs also emphasize that the wire protocol itself does not define SQL semantics; it transports startup, simple query, extended query, copy, replication, and response messages. That is important: this project can speak the Postgres wire protocol while routing SQL semantics through a reused PostgreSQL core pipeline. ([GitHub][1])

### Tokio

Use Tokio as the async runtime.

Tokio is an event-driven, non-blocking I/O platform for Rust applications. Its docs describe async task support, channels/synchronization, async TCP/Unix sockets, an I/O driver backed by OS event queues, and a runtime scheduler. Tokio’s `rt-multi-thread` feature enables the multi-threaded work-stealing scheduler. ([Docs.rs][2])

The server should use:

```toml
tokio = { version = "1", features = ["rt-multi-thread", "net", "io-util", "sync", "time", "signal", "macros"] }
```

Initial listener support:

```text
TCP listener
Unix-domain socket listener on Unix platforms
optional TLS only after basic protocol compatibility is working
```

### PostgreSQL Core Pipeline Reuse

Prefer direct reuse of PostgreSQL server code for the SQL semantic pipeline.

The initial core pipeline should be:

```text
SQL string
  -> PostgreSQL raw parser
  -> PostgreSQL analyzer
  -> PostgreSQL rewriter
  -> PostgreSQL planner/optimizer
  -> PostgreSQL PlannedStmt
  -> PostgreSQL executor infrastructure where possible
  -> fastpg in-memory catalog and storage boundaries
```

`pg_query.rs` / `libpg_query` remain useful references or fallback tools for parse-only workflows. `pg_query.rs` uses actual PostgreSQL server source to parse SQL and return PostgreSQL parse trees, and `libpg_query` exposes PostgreSQL parser behavior outside the server. ([Docs.rs][3], [GitHub][4])

The fast path should not translate PostgreSQL SQL into a separate Rust binder and planner unless the Postgres reuse path proves too expensive or too tightly coupled for a specific feature.

The primary implementation boundary is not "Rust versus C"; it is:

```text
reuse PostgreSQL semantic machinery
replace PostgreSQL physical storage machinery
```

Memory contexts, resource owners, relcache, syscache, typcache, snapshots, and executor state may be reused inside a scoped PostgreSQL-core execution context, but long-lived database state must remain owned by fastpg's in-memory catalog, storage, transaction, and fixture layers.

## Product Definition

This is a **Postgres-compatible test double**, not a PostgreSQL distribution.

The project should optimize for:

```text
fast startup
fast schema load
fast fixture capture
fast per-test reset
fast transactional rollback
fast parallel test isolation
driver compatibility
ORM compatibility
migration compatibility for common cases
```

The project should not optimize for:

```text
durability
disk efficiency
long-running production workloads
high write durability
replication
production planner cost behavior
exact physical storage behavior
security boundary fidelity
```

## Compatibility Philosophy

The server must be honest about compatibility.

Supported behavior should work normally.

Unsupported behavior should fail clearly with Postgres-shaped errors.

Do not silently pretend that unsupported production features work.

Good failure:

```text
ERROR: feature not supported by fastpg test server: logical replication
HINT: Run this test against real PostgreSQL.
```

Bad failure:

```text
query succeeds but returns subtly wrong catalog, isolation, or storage behavior
```

## Architecture

## High-Level Shape

```text
Postgres clients / ORMs / migration tools
        |
        v
pgwire + Tokio server
        |
        v
session manager
        |
        v
PostgreSQL parser
        |
        v
PostgreSQL analyzer + rewriter
        |
        v
PostgreSQL planner/optimizer
        |
        v
PostgreSQL executor bridge
        |
        +------------------------+
        |                        |
        v                        v
in-memory catalog         in-memory storage engine
virtual pg_catalog        row arenas
relcache/syscache data    tableam callbacks
information_schema        indexes
types/operators           constraints
functions                 epoch overlays
```

The server runs as:

```text
one OS process
many Tokio tasks
many client sessions
many worker threads
shared in-memory database state
per-test epoch overlays
```

It does **not** fork one backend process per connection.

## Runtime Model

### Process Model

One server process owns all database state.

```text
fastpg server process
  -> Tokio runtime
  -> listener tasks
  -> client session tasks
  -> optional CPU worker pool
  -> shared database state
```

Each client connection is a Tokio task.

Each query may execute:

```text
directly inside the session task for small/simple queries
on a CPU worker pool for expensive scans/joins/sorts
```

Do not block Tokio core threads with long-running CPU work.

Tokio’s docs note that async task switching happens at `.await` points and that CPU-bound work should be handled carefully, for example with blocking threads or a separate CPU pool. ([Docs.rs][2])

### Threading Model

The project is multi-threaded.

Use Rust ownership and explicit synchronization around shared database state.

Initial rules:

```text
sessions are independent tasks
database state is shared through Arc<Database>
catalog is versioned
base fixtures are immutable
test epochs are isolated overlays
row arenas are append-only where possible
epoch discard is bulk free
```

Avoid one global mutex for the whole database.

Accept coarse locks at first for correctness, then optimize by layer.

Recommended initial synchronization primitives:

```text
parking_lot::RwLock for catalog and schema metadata
dashmap or sharded maps for relation/index directories
ArcSwap or versioned Arc snapshots for immutable fixture/catalog state
crossbeam/arc-swap for generation-published snapshots
Tokio channels for session/server coordination
```

## Core Design Choice

The hot path is PostgreSQL-semantics-native and fastpg-storage-native.

Reuse C PostgreSQL components wherever they preserve compatibility without forcing the project to recreate PostgreSQL's durable physical storage or multi-process runtime.

### Reuse C PostgreSQL components for:

```text
SQL parser
scanner/tokenizer
analyzer
rewriter
planner/optimizer
PlannedStmt and plan node structures
expression initialization/evaluation
executor dispatcher and storage-independent executor nodes
relcache/syscache/typcache machinery when backed by in-memory catalog data
query normalization/fingerprinting
possibly deparser
possibly selected type input/output functions later
possibly selected date/time/numeric/json routines later
```

### Do not reuse C PostgreSQL physical-runtime components for fastpg storage:

```text
heapam
nbtree page storage
WAL
shared buffers / bufmgr
smgr/md storage manager
CLOG/pg_xact durability machinery
ProcArray as production concurrency machinery
lock manager as production concurrency machinery
vacuum/autovacuum
checkpointer/bgwriter
physical pg_catalog storage
physical TOAST tables
trigger manager as-is if it requires SPI over physical catalogs
RI trigger implementation as-is if it requires physical catalog/storage behavior
```

The long-term rule:

```text
C reuse is allowed when it improves compatibility and its expensive dependencies can be redirected to fastpg's in-memory catalog, storage, transaction, and fixture systems.
```

## Repository Layout

Recommended initial repository:

```text
fastpg/
  Cargo.toml
  Cargo.lock
  README.md
  LICENSE
  rust-toolchain.toml

  spec/
    001-overview.md
    002-wire-protocol.md
    003-server-runtime.md
    004-postgres-core-ffi.md
    005-catalog-and-schema.md
    006-type-system.md
    007-analysis-and-rewrite.md
    008-postgres-planning.md
    009-executor-and-tableam.md
    010-semantic-storage.md
    011-semantic-indexes.md
    012-transactions-and-epochs.md
    013-constraints.md
    014-functions-and-operators.md
    015-ddl-and-migrations.md
    016-virtual-pg-catalog.md
    017-fixtures-and-snapshots.md
    018-copy-and-bulk-load.md
    019-prepared-statements-and-plan-cache.md
    020-error-compatibility.md
    021-orm-compatibility.md
    022-testing-and-postgres-oracle.md
    023-benchmarks.md
    024-c-ffi-policy.md
    025-unsupported-features.md
    026-roadmap.md

  crates/
    fastpg-server/
      src/main.rs

    fastpg-wire/
      src/lib.rs

    fastpg-session/
      src/lib.rs

    fastpg-parser/
      src/lib.rs

    fastpg-pgcore/
      src/lib.rs

    fastpg-catalog/
      src/lib.rs

    fastpg-types/
      src/lib.rs

    fastpg-bind/
      src/lib.rs

    fastpg-plan/
      src/lib.rs

    fastpg-exec/
      src/lib.rs

    fastpg-storage/
      src/lib.rs

    fastpg-index/
      src/lib.rs

    fastpg-tx/
      src/lib.rs

    fastpg-snapshot/
      src/lib.rs

    fastpg-compat/
      src/lib.rs

    fastpg-testkit/
      src/lib.rs

  tests/
    integration/
    orm/
    sqllogictest/
    postgres_oracle/

  benches/
    startup/
    protocol/
    dml/
    epochs/
    orm/
```

## Crate Responsibilities

### `fastpg-server`

Binary crate.

Responsibilities:

```text
parse CLI/config
start Tokio runtime
bind TCP/Unix sockets
initialize global database state
install signal handlers
run health/debug endpoints if needed
```

### `fastpg-wire`

Thin integration with `pgwire`.

Responsibilities:

```text
StartupHandler implementation
SimpleQueryHandler implementation
ExtendedQueryHandler implementation
authentication policy
server parameters
query cancellation
ReadyForQuery transaction status
row description/data row encoding
```

This crate should not know storage internals.

### `fastpg-session`

Per-client state.

Responsibilities:

```text
current database
current user
session variables
transaction state
prepared statements
portals
temporary objects
current epoch
search_path
client encoding
statement timeout
application_name
```

### `fastpg-parser`

Rust-facing parser facade.

Responsibilities:

```text
call PostgreSQL parser through fastpg-pgcore
split multi-statement simple queries
normalize/fingerprint SQL
expose parse summaries needed by protocol and diagnostics
own Rust-safe parser result handles
optionally support pg_query.rs/libpg_query for parse-only tooling
```

### `fastpg-pgcore`

Narrow C ABI around reused PostgreSQL backend components.

Responsibilities:

```text
initialize scoped PostgreSQL-core execution contexts
call raw parser
call analyzer
call rewriter
call planner/optimizer
own opaque handles for RawStmt, Query, PlannedStmt, Plan, and executor state
convert PostgreSQL ereport/longjmp errors into Rust errors
enforce MemoryContext and ResourceOwner lifetime boundaries
mediate access to fastpg catalog/storage adapters
```

### `fastpg-catalog`

In-memory semantic catalog and PostgreSQL catalog provider.

Responsibilities:

```text
schemas/namespaces
relations
columns
indexes
constraints
sequences
functions
operators
types
views
virtual catalog data source
schema generations
OID assignment
synthetic pg_catalog rows for PostgreSQL analyzer/planner/executor lookups
relcache/syscache/typcache backing data
DDL metadata changes without physical catalog tables
```

### `fastpg-types`

Postgres-compatible type layer.

Responsibilities:

```text
type OIDs
type names
Datum/Value representation
text/binary encoding and decoding
input/output functions
casts
typmods
collations where supported
NULL semantics
array/json/numeric/date/time basics
Datum and TupleTableSlot conversion at the fastpg/PostgreSQL boundary
```

### `fastpg-bind`

PostgreSQL analyzer facade and Rust binding metadata.

Responsibilities:

```text
invoke PostgreSQL analyzer through fastpg-pgcore
surface analyzed parameter types
surface target list metadata
surface relation/function/operator dependencies
map PostgreSQL semantic errors to protocol errors
coordinate search_path/session state with PostgreSQL analyzer state
avoid maintaining an independent SQL analyzer unless a feature explicitly needs it
```

### `fastpg-plan`

PostgreSQL planner facade.

Responsibilities:

```text
invoke PostgreSQL rewriter and planner through fastpg-pgcore
own Rust-safe PlannedStmt handles
summarize plan shapes for compatibility tests
prepare plan cache keys
track schema invalidation
expose in-memory relation/index metadata to the planner
keep cost model knobs biased toward predictable in-memory execution
```

### `fastpg-exec`

Executor bridge.

Responsibilities:

```text
execute PostgreSQL PlannedStmt objects where possible
drive PostgreSQL executor lifecycle
stream TupleTableSlot results into pgwire rows
support Result, scan, filter, project, join, aggregate, sort, limit, and ModifyTable nodes
route table access through fastpg in-memory tableam/storage
route expression evaluation through PostgreSQL expression machinery
materialize results for protocol delivery
detect unsupported executor nodes clearly
```

### `fastpg-storage`

In-memory row storage and table access backing.

Responsibilities:

```text
row arenas
fixture base storage
epoch overlay storage
row visibility
insert/update/delete
virtual CTIDs
large varlena storage
memory accounting
table scan descriptors
TupleTableSlot population/extraction
tableam callback implementation support
no heap pages, shared buffers, WAL, smgr, or vacuum
```

### `fastpg-index`

In-memory semantic indexes.

Responsibilities:

```text
primary key indexes
unique indexes
non-unique equality indexes
ordered indexes later
index lookup
index maintenance
epoch-scoped uniqueness
planner-visible index metadata
no nbtree page storage
```

### `fastpg-tx`

Transactions and epochs.

Responsibilities:

```text
BEGIN/COMMIT/ROLLBACK
savepoints
statement snapshots
test epochs
fixture overlays
parallel test isolation
sequence rollback policy
transaction status for pgwire ReadyForQuery
```

### `fastpg-snapshot`

Fixtures and snapshots.

Responsibilities:

```text
capture fixture
restore fixture
fork epoch overlay
discard epoch overlay
serialize optional seed artifact
import/export optional schema+fixture image
```

### `fastpg-compat`

Postgres compatibility helpers.

Responsibilities:

```text
server_version responses
SHOW behavior
common pg_catalog functions
error code mapping
SQLSTATE mapping
ORM compatibility shims
driver-specific quirks
```

### `fastpg-testkit`

Testing support.

Responsibilities:

```text
start ephemeral server in tests
run SQL scripts
compare against real PostgreSQL
capture query logs
normalize results
benchmark helpers
```

## Initial Feature Envelope

## Phase 1 Target

Phase 1 should get a real Postgres client to connect and execute basic SQL.

Supported:

```text
startup packet
clear local trust/no-auth mode
simple query protocol
extended query Parse/Bind/Execute/Sync
prepared statements with positional parameters
SELECT constants
SHOW common settings
BEGIN
COMMIT
ROLLBACK
CREATE TABLE
INSERT
SELECT
UPDATE
DELETE
primary key
unique constraint
not null
basic defaults
basic WHERE
ORDER BY
LIMIT
simple joins
text row format
basic binary format where pgwire/postgres-types makes it easy
```

Phase 1 does not need:

```text
durability
disk files
TLS
SCRAM
COPY
LISTEN/NOTIFY
foreign keys
triggers
views
CTEs
window functions
recursive queries
serializable isolation
PostGIS
extensions
```

## Phase 2 Target

Run real application migrations for a narrow target app/ORM.

Add:

```text
ALTER TABLE common forms
CREATE INDEX
foreign keys
check constraints
serial/identity
sequences
information_schema basics
pg_catalog introspection basics
JSON/JSONB basics
timestamp/date basics
transaction savepoints
fixtures
epoch snapshots
```

## Phase 3 Target

Run a meaningful subset of the application unit test suite.

Add:

```text
parallel test epochs
fixture capture and discard
ORM bootstrap queries
prepared statement cache
common aggregate functions
common string/date/json functions
COPY FROM STDIN if migration/fixture tooling needs it
better error compatibility
```

## Phase 4 Target

Beat the existing fast-fork PostgreSQL path by a large margin.

Benchmark target:

```text
5x faster than existing fast-fork named epoch / rollback path
on representative unit-test DML workloads
```

Stretch target:

```text
10x-20x faster for small ORM CRUD tests with fixture reset
```

## Database State Model

## Database

```rust
struct Database {
    catalog: Arc<Catalog>,
    storage: Arc<Storage>,
    tx_manager: Arc<TxManager>,
    fixtures: Arc<FixtureRegistry>,
}
```

## Catalog

The catalog is semantic metadata, not physical pg_catalog tables. It is the source of truth for both Rust-side metadata and the synthetic catalog tuples exposed to PostgreSQL analyzer, planner, relcache, syscache, typcache, ORM introspection, and `information_schema`.

```rust
struct Catalog {
    generation: AtomicU64,
    namespaces: NamespaceMap,
    relations: RelationMap,
    types: TypeMap,
    functions: FunctionMap,
    operators: OperatorMap,
}
```

Virtual `pg_catalog` and `information_schema` queries are answered from this metadata.

## Relation

```rust
struct Relation {
    oid: Oid,
    namespace: Oid,
    name: String,
    columns: Arc<[Column]>,
    constraints: Arc<[Constraint]>,
    indexes: Arc<[IndexId]>,
    storage_id: StorageId,
    schema_generation: u64,
}
```

## Storage

```rust
struct Storage {
    base_tables: TableMap,
    active_epochs: EpochMap,
}
```

Storage backs PostgreSQL table access through fastpg tableam callbacks. It does not create heap pages, relation files, shared-buffer pages, WAL records, or vacuum work.

A base table is immutable after fixture capture unless explicitly mutated outside test mode.

A test epoch is a copy-on-write overlay.

## Row

```rust
struct Row {
    row_id: RowId,
    values: RowValues,
    inserted_at: VisibilityToken,
    deleted_at: Option<VisibilityToken>,
}
```

Phase 1 may use a simple row representation:

```text
Vec<Option<Value>>
```

Later phases can add compact tuple blobs and arena-backed varlena.

## Index

Primary key and unique indexes should initially be hash maps.

```rust
enum IndexKind {
    PrimaryKey,
    Unique,
    NonUnique,
    Ordered,
}
```

Index lookup is scoped to:

```text
base fixture + current epoch overlay + current transaction overlay
```

Different test epochs must not conflict with each other.

## Transaction and Epoch Model

## Ordinary Transaction

```text
BEGIN
  create transaction overlay

INSERT/UPDATE/DELETE
  write into transaction overlay

COMMIT
  merge transaction overlay into current epoch or base

ROLLBACK
  discard transaction overlay
```

## Test Epoch

A test epoch is a named overlay over a fixture.

```text
fixture base state
  + epoch overlay
  + current transaction overlay
```

API sketch:

```sql
SELECT fastpg_create_fixture('base');
SELECT fastpg_start_epoch('test_123', 'base');
SELECT fastpg_join_epoch('test_123');
SELECT fastpg_leave_epoch();
SELECT fastpg_finish_epoch('test_123');
```

Equivalent client-facing API can also be exposed as SQL comments or configuration if needed.

## Parallelism

Many client sessions may run at once.

Independent test epochs are isolated:

```text
epoch A does not see epoch B writes
epoch B does not see epoch A writes
both share immutable fixture base
```

Multiple sessions may join the same epoch if the application test uses a connection pool.

Within one epoch:

```text
committed transaction writes are visible to later statements in the same epoch
uncommitted writes are visible only to the owning transaction/session
```

## PostgreSQL C Component Reuse Policy

## Principle

Reuse C PostgreSQL code where it gives compatibility leverage and where expensive physical-runtime dependencies can be replaced or redirected.

A usable boundary has:

```text
explicit ownership
scoped MemoryContext and ResourceOwner lifetime
contained PostgreSQL backend globals
no dependence on real heap pages
no dependence on shared buffers
no dependence on WAL for correctness
no dependence on physical pg_catalog tables
thread confinement or documented synchronization
errors converted across the Rust/C boundary
```

## Allowed Initial Reuse

```text
PostgreSQL raw parser
PostgreSQL analyzer
PostgreSQL rewriter
PostgreSQL planner/optimizer
PostgreSQL expression initialization/evaluation
PostgreSQL executor nodes that can use fastpg tableam/storage
MemoryContext and ResourceOwner inside scoped execution contexts
relcache/syscache/typcache when backed by fastpg in-memory catalog rows
pg_query.rs/libpg_query normalization/fingerprinting if useful
```

## Candidate Later Reuse

Each candidate requires its own spec and benchmark:

```text
additional executor nodes
selected DDL execution code
numeric input/output
date/time parsing and formatting
interval parsing and formatting
json/jsonb parser pieces
selected built-in functions
selected operator implementations
Postgres error message formatting conventions
```

## Forbidden Physical-Runtime Reuse

```text
heapam
nbtree page storage
bufmgr
smgr
WAL
CLOG/pg_xact durability machinery
ProcArray as production concurrency machinery
lock manager as production concurrency machinery
vacuum/autovacuum
checkpointer/bgwriter
physical pg_catalog storage
physical TOAST tables
```

These are not forbidden because they are C. They are forbidden because they recreate PostgreSQL's physical storage and production runtime costs.

## Safety Rules for FFI

All C calls live in one or more FFI crates.

```text
fastpg-parser-ffi
fastpg-pgcore-ffi
fastpg-pgfunc-ffi, later if needed
```

Rules:

```text
unsafe code isolated
panic must not cross FFI boundary
C errors converted to Rust errors
thread-safety documented for every FFI function
parser calls protected by mutex if libpg_query is not proven thread-safe
FFI outputs copied into Rust-owned structures before use
C-owned pointers may live only inside scoped parser/analyzer/planner/executor handles
no C-owned pointers stored in long-lived fastpg database state
```

A future `024-c-ffi-policy.md` spec should define this fully.

## Wire Protocol Contract

Use `pgwire` as the wire layer.

The server should support:

```text
Startup
AuthenticationOk / local trust initially
ParameterStatus
BackendKeyData / cancellation
ReadyForQuery
Simple Query
Extended Query
Parse
Bind
Describe
Execute
Sync
Close
Terminate
ErrorResponse
NoticeResponse
RowDescription
DataRow
CommandComplete
```

Text format is required first.

Binary format is added type-by-type.

Initial clients to test:

```text
psql
tokio-postgres
libpq
pg gem / Rails
psycopg
SQLAlchemy
node-postgres
JDBC, later
Prisma, later
```

## Server Parameters

Initial `ParameterStatus` values should be configurable but Postgres-like:

```text
server_version = 17.x-compatible or configurable
server_encoding = UTF8
client_encoding = UTF8
DateStyle = ISO, MDY
integer_datetimes = on
standard_conforming_strings = on
TimeZone = UTC by default
application_name = client-provided if any
```

These values matter because ORMs inspect them.

## SQL Surface

The SQL surface should be driven by real workload capture.

Before broad implementation, collect SQL from:

```text
migrations
fixture setup
test suite
ORM connection bootstrap
schema introspection
prepared statements
```

Store fingerprints and normalized forms:

```text
tests/fixtures/sql_fingerprints/*.json
```

Use `pg_query.rs` normalization/fingerprinting for this early because it already exposes normalization and fingerprinting APIs. ([Docs.rs][3])

Implementation priority should follow actual frequency and importance, not theoretical SQL completeness.

## Initial Supported SQL

### Transaction Control

```sql
BEGIN;
COMMIT;
ROLLBACK;
SAVEPOINT s;
ROLLBACK TO s;
RELEASE SAVEPOINT s;
```

### DDL

```sql
CREATE SCHEMA ...
CREATE TABLE ...
DROP TABLE ...
ALTER TABLE ADD COLUMN ...
ALTER TABLE ALTER COLUMN SET DEFAULT ...
ALTER TABLE ADD CONSTRAINT ...
CREATE INDEX ...
CREATE UNIQUE INDEX ...
CREATE SEQUENCE ...
DROP INDEX ...
TRUNCATE ...
```

### DML

```sql
INSERT ...
INSERT ... RETURNING ...
SELECT ...
UPDATE ...
UPDATE ... RETURNING ...
DELETE ...
DELETE ... RETURNING ...
```

### Query Features

```text
WHERE
AND/OR/NOT
comparison operators
IS NULL / IS NOT NULL
IN
BETWEEN
LIKE / ILIKE, later
ORDER BY
LIMIT/OFFSET
INNER JOIN
LEFT JOIN
COUNT/SUM/MIN/MAX
GROUP BY, later
HAVING, later
CTEs, later
```

## Type System

Initial types:

```text
bool
int2
int4
int8
float4
float8
numeric, simplified first
text
varchar
char
bytea
uuid
date
timestamp
timestamptz
json
jsonb
arrays, limited
serial/identity pseudo-types
```

Each type needs:

```text
OID
name aliases
text input
text output
binary encode/decode where required
comparison
hash
cast rules
NULL behavior
```

The type system spec must define where Rust-native behavior is acceptable and where exact PostgreSQL behavior is required.

## Catalog Compatibility

The project does not physically store `pg_catalog`.

Instead, it keeps a full in-memory semantic catalog and exposes virtual catalog tables and functions backed by that metadata.

The same in-memory catalog must also provide the synthetic catalog tuples needed by PostgreSQL analyzer, planner, relcache, syscache, typcache, executor initialization, and ORM introspection.

Initial virtual catalog objects:

```text
pg_catalog.pg_class
pg_catalog.pg_namespace
pg_catalog.pg_attribute
pg_catalog.pg_type
pg_catalog.pg_index
pg_catalog.pg_constraint
pg_catalog.pg_proc
pg_catalog.pg_operator
pg_catalog.pg_database
pg_catalog.pg_settings
information_schema.tables
information_schema.columns
information_schema.key_column_usage
information_schema.table_constraints
```

The first catalog goal is ORM compatibility, not full catalog completeness.

The virtual catalog spec should be based on actual ORM introspection queries.

## Storage Design

No heap pages.

No shared buffers.

No WAL.

No physical TOAST.

No vacuum.

No smgr/md relation files for fastpg table data.

Use semantic in-memory storage:

```text
table = row arena + visibility metadata + index handles
index = hash/ordered map + epoch deltas
fixture = immutable base table set
epoch = overlay of inserts/updates/deletes/index deltas
transaction = smaller overlay merged into epoch on commit
```

## Fixture Design

Fast unit tests need fast reset.

The server should support:

```text
load schema
load base fixture
capture fixture
run test in epoch overlay
discard epoch overlay
repeat
```

Fixture capture:

```text
freezes base rows
freezes base indexes
records catalog generation
records sequence values
records constraint metadata
```

Epoch discard:

```text
drops row overlay arena
drops index delta maps
drops delete/update maps
resets sequence policy according to config
```

## Configuration

Initial config file format:

```toml
[server]
host = "127.0.0.1"
port = 5432
unix_socket_dir = "/tmp"
workers = "num_cpus"

[compat]
server_version = "17.0"
timezone = "UTC"
auth = "trust"

[storage]
memory_limit = "2GB"
fixture_memory_limit = "1GB"
epoch_memory_limit = "256MB"

[parser]
use_pg_query = true
parser_mutex = true

[debug]
log_sql = false
trace_protocol = false
compare_with_postgres = false
```

CLI:

```sh
fastpg serve --config fastpg.toml
fastpg serve --port 55432 --auth trust
fastpg capture-sql --output sql-fingerprints.json
fastpg oracle --postgres-url postgres://...
```

## Testing Strategy

## Compatibility Tests

Run against real clients:

```text
psql smoke tests
libpq smoke tests
tokio-postgres tests
ORM connection tests
migration tool tests
```

## PostgreSQL Oracle Tests

For supported SQL, compare against real PostgreSQL:

```text
same schema
same data
same query
compare rows
compare column names
compare type OIDs
compare errors where feasible
```

Differences are allowed only if documented.

## SQL Logic Tests

Use sqllogictest-style files for deterministic behavior:

```text
basic expressions
DDL
DML
joins
constraints
transactions
epochs
```

## Fuzz / Differential Testing

Future:

```text
generate small schemas
generate random DML
compare with PostgreSQL for supported subset
```

## Performance Tests

Benchmarks must include:

```text
startup time
connection time
simple query latency
prepared query latency
fixture capture time
epoch discard time
single-row insert/update/delete
primary key lookup
foreign key insert
parallel epoch throughput
ORM CRUD test loop
migration + fixture + N tests
```

Targets:

```text
startup under tens of milliseconds
connection/session creation negligible relative to query work
epoch discard near O(1) with respect to base fixture size
common CRUD tests 5x+ faster than existing fast-fork PostgreSQL path
```

## Required Follow-Up Specs

The rest of the project should be specified as layers. Each spec should define scope, compatibility contract, data structures, unsupported behavior, tests, and benchmarks.

## `002-wire-protocol.md`

Define how `pgwire` is integrated.

Must cover:

```text
StartupHandler
SimpleQueryHandler
ExtendedQueryHandler
Parse/Bind/Execute/Sync
prepared statements
portals
server parameters
transaction status
cancel requests
error and notice mapping
text/binary result encoding
client compatibility matrix
```

## `003-server-runtime.md`

Define Tokio runtime architecture.

Must cover:

```text
listener tasks
session tasks
CPU worker pool
blocking vs async work
cancellation
statement timeout
graceful shutdown
connection lifecycle
shared database ownership
metrics/tracing
```

## `004-postgres-core-ffi.md`

Define the narrow C ABI around reused PostgreSQL backend components.

Must cover:

```text
PostgreSQL version pinning
raw parser entry points
analyzer entry points
rewriter entry points
planner entry points
executor lifecycle entry points
multi-statement splitting
normalization/fingerprinting
opaque handle ownership
MemoryContext ownership
ResourceOwner ownership
thread confinement
error conversion
catalog/storage adapter boundaries
```

## `005-catalog-and-schema.md`

Define semantic catalog.

Must cover:

```text
schemas
relations
columns
indexes
constraints
sequences
OIDs
name resolution data
schema generations
DDL metadata changes
catalog snapshots
```

## `006-type-system.md`

Define Postgres-compatible types.

Must cover:

```text
Value enum
type OIDs
text/binary encoding
input/output
casts
comparison
hashing
numeric
date/time
uuid
json/jsonb
arrays
collations
NULL behavior
```

## `007-analysis-and-rewrite.md`

Define PostgreSQL analyzer and rewriter reuse.

Must cover:

```text
search_path
session state visible to PostgreSQL analyzer
in-memory catalog data visible to analyzer
range table metadata surfaced to Rust
parameter typing surfaced to Rust
target list metadata surfaced to Rust
operator/function/type lookup through PostgreSQL catalogs
rewrite rules supported or explicitly rejected
DDL analysis
error messages
```

## `008-postgres-planning.md`

Define PostgreSQL planner/optimizer reuse.

Must cover:

```text
PlannedStmt ownership
plan node support matrix
scan planning against in-memory relations
index metadata exposed to planner
joins
aggregates
sort/limit
DML plans
prepared plan cache keys
schema invalidation
in-memory cost model tuning
unsupported plan behavior
```

## `009-executor-and-tableam.md`

Define execution through PostgreSQL executor infrastructure and fastpg table access.

Must cover:

```text
ExecutorStart/ExecutorRun/ExecutorFinish/ExecutorEnd ownership
expression evaluator reuse
projection
filters
joins
aggregates
sort
DML
RETURNING
result streaming
in-memory TableAmRoutine
TupleTableSlot conversion
virtual CTIDs
memory limits
CPU worker interaction
unsupported executor nodes
```

## `010-semantic-storage.md`

Define row storage.

Must cover:

```text
row arenas
row IDs
virtual CTID
fixture base rows
epoch overlays
transaction overlays
large values
memory accounting
insert/update/delete
scan visibility
```

## `011-semantic-indexes.md`

Define indexes.

Must cover:

```text
primary key hash index
unique index
non-unique index
ordered index roadmap
epoch delta maps
uniqueness scope
index maintenance
index scan API
```

## `012-transactions-and-epochs.md`

Define transaction and test isolation.

Must cover:

```text
BEGIN/COMMIT/ROLLBACK
savepoints
statement snapshots
READ COMMITTED
epoch start/join/leave/finish
same-epoch multi-session behavior
parallel independent epochs
sequence rollback policy
```

## `013-constraints.md`

Define constraints.

Must cover:

```text
NOT NULL
defaults
CHECK
primary key
unique
foreign keys
deferrable constraints, if supported
ON DELETE / ON UPDATE actions
constraint errors
```

## `014-functions-and-operators.md`

Define built-in functions/operators.

Must cover:

```text
operator catalog
function catalog
expression evaluation
string functions
date/time functions
json/jsonb functions
array functions
aggregate functions
C Postgres function reuse policy
```

## `015-ddl-and-migrations.md`

Define DDL support.

Must cover:

```text
CREATE TABLE
ALTER TABLE
CREATE INDEX
DROP
TRUNCATE
CREATE SCHEMA
CREATE SEQUENCE
migration compatibility
unsupported DDL
schema change invalidation
```

## `016-virtual-pg-catalog.md`

Define virtual catalog and information schema.

Must cover:

```text
pg_class
pg_attribute
pg_type
pg_namespace
pg_index
pg_constraint
pg_proc
pg_operator
pg_settings
information_schema
ORM introspection compatibility
```

## `017-fixtures-and-snapshots.md`

Define fixture capture and restore.

Must cover:

```text
fixture capture
immutable base state
epoch overlay creation
snapshot serialization, optional
sequence capture
catalog generation capture
memory accounting
```

## `018-copy-and-bulk-load.md`

Define COPY and bulk fixture load.

Must cover:

```text
COPY FROM STDIN
COPY TO STDOUT
CSV/text formats
binary COPY, later
bulk index build
bulk constraint validation
fixture loading benchmark
```

## `019-prepared-statements-and-plan-cache.md`

Define prepared statements.

Must cover:

```text
extended query protocol semantics
parameter types
prepared statement lifecycle
portal lifecycle
plan cache
schema invalidation
generic vs custom plans
ORM prepared statement compatibility
```

## `020-error-compatibility.md`

Define errors.

Must cover:

```text
SQLSTATE mapping
Postgres-like error fields
constraint violation errors
syntax errors
type errors
unsupported feature errors
position reporting
```

## `021-orm-compatibility.md`

Define ORM-specific compatibility.

Must cover:

```text
Rails ActiveRecord
Django
SQLAlchemy
Prisma
node-postgres
Hibernate/JDBC
common startup queries
common introspection queries
known quirks
```

## `022-testing-and-postgres-oracle.md`

Define test infrastructure.

Must cover:

```text
real PostgreSQL comparison
golden SQL tests
driver tests
ORM smoke tests
differential tests
query capture
failure triage
```

## `023-benchmarks.md`

Define performance harness.

Must cover:

```text
benchmark workloads
startup
connection churn
simple query
prepared query
CRUD
fixtures
epochs
parallel tests
comparison against PostgreSQL and fast-fork PostgreSQL
success thresholds
```

## `024-c-ffi-policy.md`

Define C reuse and safety.

Must cover:

```text
allowed C components
forbidden C components
thread-safety
mutex requirements
ownership
panic/error boundaries
licensing
build system
version pinning
```

## `025-unsupported-features.md`

Define explicit non-support.

Must cover:

```text
durability
replication
logical decoding
RLS
permissions
extensions
PostGIS
serializable isolation
physical storage inspection
VACUUM semantics
EXPLAIN exactness
pageinspect
custom AMs
```

## `026-roadmap.md`

Define phased delivery.

Must cover:

```text
MVP
ORM compatibility milestone
migration compatibility milestone
fixture/epoch milestone
benchmark milestone
full app-test milestone
future compatibility expansions
```

## `027-session-state-per-client.md`

Define per-client Rust session ownership.

Must cover:

```text
SessionState lifecycle
ServerState vs SessionState ownership
pgwire client metadata
prepared statement and portal isolation
COPY state isolation
transaction state ownership
session GUC/search_path isolation
disconnect cleanup
```

## `028-transaction-owned-storage-state.md`

Define session-owned storage transaction overlays.

Must cover:

```text
shared committed storage
session transaction overlays
subtransaction overlays
rollback as drop
commit merge
transaction arenas
tableam callback execution context
index delta ownership
```

## `029-single-pgcore-execution-lane.md`

Define the serialized PostgreSQL C execution lane.

Must cover:

```text
pgcore lane API
C global serialization
lane-affine prepared handles
active catalog/storage context
metrics for queue and execution time
multi-lane prerequisites
```

## `030-catalog-generation-invalidation.md`

Define Rust catalog generation invalidation.

Must cover:

```text
catalog generation counter
DDL commit publication
rollback behavior
prepared statement invalidation
plan invalidation
virtual pg_catalog snapshots
replacement for no-op shared invalidation
```

## MVP Milestone

The first end-to-end milestone should be intentionally small:

```text
cargo run --bin fastpg-server
psql connects
SELECT 1 works
SHOW server_version works
CREATE TABLE works
INSERT works
SELECT from table works
BEGIN/ROLLBACK works
prepared SELECT works through extended protocol
```

Acceptance test:

```sh
psql "postgres://localhost:55432/postgres" -c "SELECT 1"
```

Then:

```sql
CREATE TABLE users(id bigint primary key, email text unique not null);
INSERT INTO users VALUES (1, 'a@example.com');
SELECT * FROM users WHERE id = 1;
BEGIN;
INSERT INTO users VALUES (2, 'b@example.com');
ROLLBACK;
SELECT * FROM users WHERE id = 2;
```

Expected:

```text
id=1 row exists
id=2 row does not exist after rollback
```

## First Real Compatibility Milestone

Run one ORM’s connection/bootstrap path.

Recommended first ORM:

```text
Rails ActiveRecord or SQLAlchemy
```

Criteria:

```text
connects using normal Postgres URL
runs startup queries
runs basic migration
creates table
inserts row
selects row
rolls back transaction
disconnects cleanly
```

## First Performance Milestone

Benchmark against:

```text
real PostgreSQL
existing fast-fork PostgreSQL
fastpg
```

Workload:

```text
load schema
load fixture
run N tests
each test does:
  start epoch
  perform 10-100 CRUD statements
  discard epoch
```

Success target:

```text
fastpg is at least 5x faster than existing fast-fork PostgreSQL on representative unit-test workload
```

## Design Risks

### Compatibility Risk

The hardest work is not protocol parsing. It is PostgreSQL semantics:

```text
type coercion
operator lookup
NULL behavior
date/time behavior
numeric precision
jsonb behavior
constraint timing
catalog introspection
prepared statement parameter typing
ORM quirks
```

Mitigation:

```text
drive implementation from captured real SQL
compare supported behavior with real PostgreSQL
keep unsupported behavior explicit
```

### C Reuse Risk

Reusing too much C PostgreSQL can accidentally recreate PostgreSQL.

Mitigation:

```text
reuse SQL semantics first
replace physical storage/runtime costs first
draw explicit boundaries at catalog provider, table access method, transaction state, and fixture state
benchmark every new reused subsystem against the in-memory goal
fail clearly when a PostgreSQL component tries to require physical storage
```

### Async Runtime Risk

CPU-heavy queries can block Tokio worker threads.

Mitigation:

```text
short queries execute inline
long scans/joins use CPU pool
instrument per-query runtime
add cooperative cancellation
```

### Catalog Risk

ORMs are catalog-heavy and picky.

Mitigation:

```text
record actual ORM catalog queries
implement virtual catalog rows by compatibility tests
treat catalog compatibility as a first-class layer
```

### Scope Risk

“Postgres compatible” can become infinite.

Mitigation:

```text
unit-test-only contract
explicit unsupported list
application-query-driven priority
real PostgreSQL fallback only for testing/oracle, not hot-path split-brain
```

## Design Non-Negotiables

The following decisions define the project:

```text
one process
multi-threaded Tokio runtime
pgwire for protocol
direct PostgreSQL parser/analyzer/rewriter/planner reuse
PostgreSQL executor infrastructure where it can run on fastpg storage
in-memory catalog as source of truth
semantic in-memory tables
no heap pages
no shared buffers for fastpg table data
no WAL
no vacuum
epoch fixture isolation
virtual catalog
unit-test-only compatibility contract
unsupported features fail clearly
```

Changing any of these requires updating this overview and the roadmap specs.

## Success Definition

The project succeeds if application test suites can point ordinary Postgres clients at `fastpg` and get:

```text
correct-enough Postgres behavior
normal driver compatibility
normal ORM compatibility
very fast fixture reset
parallel test isolation
large speedup over PostgreSQL and fast-fork PostgreSQL
clear errors for unsupported production features
```

The core performance hypothesis is:

```text
Deleting PostgreSQL’s physical storage and process architecture
is more valuable than optimizing it for unit tests.
```

This project should prove or disprove that hypothesis quickly with real workload benchmarks.

[1]: https://raw.githubusercontent.com/sunng87/pgwire/master/README.md "raw.githubusercontent.com"
[2]: https://docs.rs/tokio/latest/tokio/ "tokio - Rust"
[3]: https://docs.rs/pg_query "pg_query - Rust"
[4]: https://github.com/pganalyze/libpg_query "GitHub - pganalyze/libpg_query: C library for accessing the PostgreSQL parser outside of the server environment · GitHub"
