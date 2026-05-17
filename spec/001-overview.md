Below is a draft for `spec/001-overview.md`. It frames the project as a new Rust Postgres-compatible **unit-test server**, not as another Postgres fork.

# `spec/001-overview.md`

# Rust Postgres-Compatible Test Server Overview

## Summary

Build a new Rust database server that is **Postgres-compatible enough for application unit tests**, but is not PostgreSQL internally.

The server speaks the PostgreSQL wire protocol, accepts ordinary Postgres client drivers, runs in one process, uses Tokio for async networking and scheduling, and stores test data in an in-memory semantic engine optimized for fast fixture reset, per-test isolation, and parallel test execution.

This project is separate from the existing fast-fork PostgreSQL work.

The existing fast-fork direction optimizes inside PostgreSQL’s architecture. This project takes the more aggressive route:

```text
keep:
  PostgreSQL wire protocol
  PostgreSQL SQL syntax where feasible
  PostgreSQL parser, initially through libpg_query / pg_query.rs
  PostgreSQL-compatible type names, catalog views, errors, and client behavior

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
  relcache/syscache
  physical pg_catalog storage
  PostgreSQL executor hot path
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
Postgres-compatible surface,
test-optimized Rust runtime.
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

The `pgwire` docs also emphasize that the wire protocol itself does not define SQL semantics; it transports startup, simple query, extended query, copy, replication, and response messages. That is important: this project can implement a custom Rust SQL engine while still speaking the Postgres wire protocol. ([GitHub][1])

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

### PostgreSQL Parser Reuse

Use `pg_query.rs` / `libpg_query` for SQL parsing initially.

`pg_query.rs` uses actual PostgreSQL server source to parse SQL and return PostgreSQL parse trees. Its docs say it can also normalize queries and that building the crate builds parts of PostgreSQL server source and statically links them into the Rust library. ([Docs.rs][3])

`libpg_query` is a C library for accessing the PostgreSQL parser outside the server. Its README says it uses actual PostgreSQL server source to parse SQL and return PostgreSQL’s internal parse tree, and is the base library for Rust, Go, Ruby, Node, Python, and other bindings. ([GitHub][4])

The initial parser stack should be:

```text
SQL string
  -> pg_query.rs
  -> PostgreSQL parse tree / protobuf AST
  -> Rust binder
  -> Rust logical plan
  -> Rust executor
```

Do **not** initially reuse PostgreSQL analyzer, planner, or executor as the hot path. They are too entangled with PostgreSQL backend state, memory contexts, catalogs, snapshots, relation caches, and physical storage assumptions.

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
exact planner behavior
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
SQL parser: pg_query.rs / libpg_query
        |
        v
Rust binder / analyzer
        |
        v
Rust planner
        |
        v
Rust executor
        |
        +------------------------+
        |                        |
        v                        v
semantic catalog          semantic storage engine
virtual pg_catalog        row arenas
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

The hot path is Rust-native.

Reuse C PostgreSQL components selectively, but only where they do not force the project to recreate PostgreSQL’s slow runtime.

### Reuse C PostgreSQL components for:

```text
SQL parser
scanner/tokenizer
query normalization/fingerprinting
possibly deparser
possibly selected type input/output functions later
possibly selected date/time/numeric/json routines later
```

### Do not initially reuse C PostgreSQL components for:

```text
executor
planner
analyzer
relcache
syscache
heapam
btree
WAL
storage manager
memory contexts
resource owners
ProcArray
lock manager
trigger manager as-is
RI trigger implementation as-is
```

The long-term rule:

```text
C reuse is allowed only if it improves compatibility without importing PostgreSQL’s physical storage or process architecture into the hot path.
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
    004-sql-parser-ffi.md
    005-catalog-and-schema.md
    006-type-system.md
    007-binder-and-name-resolution.md
    008-logical-plan.md
    009-executor.md
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

Parser boundary.

Responsibilities:

```text
call pg_query.rs
split multi-statement simple queries
normalize/fingerprint SQL
convert PostgreSQL AST/protobuf into fastpg AST
own all unsafe parser FFI wrappers
```

### `fastpg-catalog`

Semantic catalog.

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
```

### `fastpg-bind`

Binder/analyzer.

Responsibilities:

```text
name resolution
search_path resolution
column resolution
type inference
parameter typing
operator lookup
function lookup
INSERT target mapping
UPDATE target mapping
DDL semantic validation
```

### `fastpg-plan`

Logical planner.

Responsibilities:

```text
logical plan nodes
simple cost model
index selection
join order for common cases
projection/filter pushdown
plan cache keys
```

### `fastpg-exec`

Executor.

Responsibilities:

```text
execute logical plans
expression evaluation
scan/filter/project
joins
aggregates
sort/limit
DML
RETURNING
trigger hooks if supported
result materialization
```

### `fastpg-storage`

Semantic row storage.

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
```

### `fastpg-index`

Semantic indexes.

Responsibilities:

```text
primary key indexes
unique indexes
non-unique equality indexes
ordered indexes later
index lookup
index maintenance
epoch-scoped uniqueness
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

The catalog is semantic metadata, not physical pg_catalog tables.

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

Reuse C PostgreSQL code only where the boundary is clean.

A clean boundary has:

```text
explicit inputs
explicit outputs
no dependence on PostgreSQL backend globals
no dependence on MemoryContext lifetime
no dependence on relcache/syscache
no dependence on real heap pages
no dependence on process-local backend identity
safe or contained threading behavior
```

## Allowed Initial Reuse

```text
libpg_query parser
libpg_query scanner
pg_query.rs normalization/fingerprinting
possibly deparser
```

## Candidate Later Reuse

Each candidate requires its own spec and benchmark:

```text
numeric input/output
date/time parsing and formatting
interval parsing and formatting
json/jsonb parser pieces
selected built-in functions
selected operator implementations
Postgres error message formatting conventions
```

## Forbidden Hot-Path Reuse Initially

```text
PostgreSQL executor
PostgreSQL planner
heapam
nbtree
bufmgr
smgr
WAL
CLOG
ProcArray
lock manager
trigger manager
RI trigger SPI queries
relcache/syscache
```

These are not forbidden forever, but importing them early defeats the purpose of the project.

## Safety Rules for FFI

All C calls live in one or more FFI crates.

```text
fastpg-parser-ffi
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
no C-owned pointers stored in long-lived Rust database state
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

Instead, it exposes virtual catalog tables and functions backed by semantic metadata.

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

## `004-sql-parser-ffi.md`

Define parser integration.

Must cover:

```text
pg_query.rs usage
multi-statement splitting
normalization/fingerprinting
AST conversion
FFI safety
parser thread-safety
Postgres version pinning
error conversion
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

## `007-binder-and-name-resolution.md`

Define the analyzer/binder.

Must cover:

```text
search_path
range table binding
column resolution
parameter typing
operator lookup
function lookup
type coercion
INSERT/UPDATE/DELETE binding
DDL binding
error messages
```

## `008-logical-plan.md`

Define planning.

Must cover:

```text
logical plan nodes
scan planning
index selection
joins
aggregates
sort/limit
DML plans
prepared plan cache keys
schema invalidation
cost model
```

## `009-executor.md`

Define execution.

Must cover:

```text
expression evaluator
projection
filters
joins
aggregates
sort
DML
RETURNING
result streaming
memory limits
CPU worker interaction
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
parser first
hot path Rust-native
separate spec for every additional C component
benchmark before and after reuse
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
pg_query.rs/libpg_query for initial parser
Rust-native binder/planner/executor/storage hot path
semantic in-memory tables
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
