# Semantic In-Memory Table Engine

## Summary

Add a test-only semantic storage engine for eligible ordinary user tables without
requiring callers to change how they use PostgreSQL.

The current fast fork removes or bypasses many durability-oriented costs, but it
still usually runs user-table DML through PostgreSQL's physical storage
architecture:

```text
heap pages
shared buffers
line pointers
HOT updates
visibility-map and free-space-map forks
btree pages
buffer pins
buffer content locks
page-level dirty tracking
physical TOAST relations
physical index maintenance
CLOG-backed tuple visibility paths
```

That is still too much machinery for disposable unit-test databases.

This spec introduces a more drastic mode for eligible user tables:

```text
ordinary application SQL
    -> ordinary PostgreSQL parser/planner/executor
    -> semantic in-memory relation
    -> shared-memory row arena
    -> shared-memory semantic indexes
    -> ordinary transaction/savepoint/rollback semantics
    -> no heap pages
    -> no shared buffers for table rows
    -> no HOT chains
    -> no FSM/VM
    -> no physical TOAST path
    -> no btree page splits
```

The PostgreSQL process model remains intact:

```text
postmaster
  -> multiple backend processes
  -> existing client connection pools
  -> existing parallel test workers
  -> shared semantic storage in shared memory
```

This is not a threaded PostgreSQL rewrite. It is a multi-process,
shared-memory semantic storage layer that lets existing parallel test harnesses
keep using multiple PostgreSQL backend processes.

The user-facing workflow should remain ordinary PostgreSQL:

```sql
CREATE TABLE users (
    id bigint primary key,
    email text unique not null,
    name text
);

INSERT INTO users VALUES (1, 'a@example.com', 'A');

BEGIN;
INSERT INTO users VALUES (2, 'b@example.com', 'B');
SELECT * FROM users WHERE id = 2;
ROLLBACK;

SELECT * FROM users WHERE id = 2; -- no row
```

There is no required fast-fork SQL schema, no required fixture-capture call, no
required named-epoch call, and no required table-level syntax such as `USING
fastfork_semantic_tableam`. A test suite should be able to point its existing
PostgreSQL connection string at a semantic-storage fast-fork build and keep
running its normal migrations, fixture setup, transactions, savepoints, queries,
and cleanup.

## Compatibility Rule

Semantic storage must be transparent to ordinary PostgreSQL callers.

Required user-model properties:

```text
no required pg_fastfork_* SQL calls
no required fast-fork schema setup
no required fixture capture step
no required named epoch start/join/leave/finish step
no required CREATE TABLE syntax changes
no required application query changes
```

Operational selection happens outside application SQL:

```text
build option
postmaster configuration
database/session GUCs set by the launcher
relation eligibility checks
```

If a relation is eligible, the fast fork can route its table and index storage
to semantic shared memory automatically. If a relation is not eligible, it can
remain heap-backed from the start. Once a relation is semantic-active, the
semantic copy is authoritative and unsupported operations must error clearly
rather than silently routing part of the relation back to heap storage.

Semantic storage does not invent a new application-visible isolation model. It
must preserve ordinary PostgreSQL transaction semantics for supported
workloads:

```text
transactions commit or roll back normally
savepoints work normally
multiple backends see committed data according to normal snapshots
unique constraints conflict according to normal database/schema/table scope
separate databases or schemas isolate data the same way they do in PostgreSQL
```

In particular, this design does not depend on a test harness using named
fast-fork epochs to avoid uniqueness conflicts. If two concurrent tests share
the same database, schema, and table and both commit the same primary key, that
is the same application-level conflict it would be in PostgreSQL. Test suites
that already isolate through transactions, separate schemas, separate
databases, or separate clusters keep using those mechanisms unchanged.

## Core Idea

Do not make PostgreSQL's physical storage engine faster for eligible unit-test
tables.

Stop using it.

Instead of this:

```text
INSERT
  -> heap_insert
  -> find/extend heap page
  -> buffer lookup
  -> pin buffer
  -> lock buffer
  -> line pointer
  -> tuple header xmin/xmax
  -> index_insert into btree pages
  -> FSM/VM changes
  -> dirty tracking
  -> rollback leaves dead physical state
```

Use this:

```text
INSERT
  -> evaluate defaults and constraints
  -> allocate row in transaction semantic arena
  -> assign virtual row id
  -> add row id to semantic index deltas
  -> tag row with transaction/subtransaction metadata
```

Instead of this:

```text
ROLLBACK
  -> physical heap/index pages contain aborted state
  -> transaction cleanup must unwind general-purpose storage artifacts
  -> later scans must step around dead physical tuples
```

Use this:

```text
ROLLBACK
  -> discard transaction row arena
  -> discard transaction index deltas
  -> discard transaction delete/update log
  -> committed semantic rows were never modified in place
```

The user-visible goal is still PostgreSQL-like semantics for common unit-test
workloads:

```text
SQL parser
planner
executor
constraints
ordinary joins
primary keys
unique indexes
foreign keys
defaults
not-null checks
check constraints
triggers where feasible
RETURNING
transactions
savepoints
multi-backend connection pools
parallel test workers
```

The implementation deliberately does not preserve physical PostgreSQL internals
for semantic tables:

```text
heap page layout
ctid as physical page/offset
HOT chains
btree page format
visibility map behavior
free-space map behavior
TOAST table storage
pageinspect/amcheck behavior
VACUUM behavior
storage-parameter behavior
```

## Goals

- Achieve a step-function runtime improvement beyond page-backed rollback and
  page-backed overlay strategies.
- Keep PostgreSQL's multi-process backend model.
- Preserve ordinary PostgreSQL caller behavior for supported application-test
  workloads.
- Avoid requiring test code, ORMs, migration tools, or fixture loaders to call
  fast-fork-specific SQL functions.
- Replace heap/page/buffer/HOT storage for eligible user tables.
- Replace btree-page index maintenance for eligible user-table indexes.
- Make rollback proportional to transaction semantic overlay size, not physical
  page dirtiness and dead tuple cleanup.
- Keep ordinary PostgreSQL SQL text for application tests.
- Use the existing parser/analyzer/planner/executor as much as practical in
  phase 1.
- Keep system catalogs page-backed initially, so catalog correctness and ORM
  introspection can continue to use real PostgreSQL catalog rows.
- Support ordinary migration and fixture setup through standard DDL/DML.
- Fail clearly for unsupported semantic-table operations.
- Keep stock PostgreSQL behavior unchanged when the build flag is disabled.

## Non-Goals

- No required fast-fork SQL schema or caller-visible lifecycle API.
- No required fixture-capture call.
- No required named-epoch API.
- No single-process PostgreSQL rewrite.
- No multithreaded PostgreSQL backend rewrite.
- No production support.
- No durability.
- No crash recovery of semantic state.
- No WAL, replication, logical decoding, PITR, archive recovery, base backup, or
  `pg_upgrade` support for semantic tables.
- No promise that physical storage inspection tools work on semantic tables.
- No promise that `ctid` has real heap page meaning.
- No support for tests that assert HOT update behavior, heap pruning behavior,
  btree page behavior, FSM/VM behavior, or VACUUM behavior.
- No full serializable isolation implementation in phase 1.
- No support for arbitrary custom table access methods, custom index access
  methods, or extensions that inspect heap/btree pages.
- No immediate virtual-catalog rewrite in phase 1.
- No immediate compiled-query fast path in phase 1.
- No silent fallback after a table has become semantic-active.
- No automatic isolation between tests that intentionally share the same
  database, schema, table, and committed key space.

## Build Flag

Add a new opt-in build option:

```text
Meson:     -Dtest_semantic_storage=true
Autoconf:  --enable-test-semantic-storage
C define:  USE_TEST_SEMANTIC_STORAGE
```

Default:

```text
false
```

Recommended companion flags:

```text
-Dtest_fake_wal=true
-Dtest_no_wal_assembly=true
-Dtest_no_bg_jobs=true
-Dtest_mem_smgr=true
-Dtest_mem_slru=true
-Dtest_ephemeral_buffers=true
```

Semantic storage should not require ordinary user rows to use `memsmgr` heap
pages, because semantic rows do not live in heap pages. Companion fast-fork
features are still useful for:

```text
system catalogs
migration setup
non-eligible fallback relations
transaction-status bookkeeping
startup speed
```

## Configuration

Add GUCs that can be set by the test launcher or server configuration, not by
application code:

```text
fastfork.semantic_storage = off | auto | require
fastfork.semantic_relation_selection = eligible_user_tables | allowlist
fastfork.semantic_max_shared_memory = size
fastfork.semantic_xact_memory_limit = size
fastfork.semantic_on_unsupported = heap_before_semantic | error
fastfork.semantic_ctid_policy = virtual | error
fastfork.semantic_isolation = read_committed | repeatable_read
fastfork.semantic_index_mode = hash_only | hash_and_ordered
fastfork.semantic_fk_mode = trigger | compiled
fastfork.semantic_trigger_mode = normal | unsupported
fastfork.semantic_explain_mode = approximate | error
fastfork.semantic_parallel_query = off | attach
```

Suggested first-version defaults:

```text
fastfork.semantic_storage = auto
fastfork.semantic_relation_selection = eligible_user_tables
fastfork.semantic_on_unsupported = heap_before_semantic
fastfork.semantic_ctid_policy = virtual
fastfork.semantic_isolation = read_committed
fastfork.semantic_index_mode = hash_only
fastfork.semantic_fk_mode = trigger
fastfork.semantic_trigger_mode = normal
fastfork.semantic_explain_mode = approximate
fastfork.semantic_parallel_query = off
```

Mode behavior:

```text
off
    never use semantic storage

auto
    use semantic storage for relations that are eligible when created or first
    opened; keep ineligible relations heap-backed from the start

require
    error when an ordinary user relation cannot use semantic storage
```

`heap_before_semantic` means a relation can stay heap-backed if it is found
ineligible before semantic storage owns any of its row state. It does not permit
a semantic-active relation to fall back to heap after writes have occurred.

## User Model

### Phase 1: ordinary migrations, fixtures, and tests

The first version should not require callers to split their setup into
"physical migration" and "semantic capture" phases.

Instead, applications run normal SQL:

```sql
CREATE TABLE users (
    id bigint primary key,
    email text unique not null,
    name text
);

CREATE TABLE posts (
    id bigint primary key,
    user_id bigint not null references users(id),
    title text not null
);

INSERT INTO users VALUES (1, 'a@example.com', 'A');
INSERT INTO posts VALUES (1, 1, 'hello');

BEGIN;
UPDATE users SET email = 'new@example.com' WHERE id = 1;
ROLLBACK;
```

Under semantic storage:

```text
system catalogs remain ordinary PostgreSQL relations
eligible user table rows are stored in semantic shared memory
eligible user indexes are semantic maps
ordinary transactions create semantic transaction overlays
ROLLBACK discards semantic transaction overlays
COMMIT publishes semantic transaction overlays as committed row/index state
ordinary SQL continues to read and write through PostgreSQL executor nodes
```

The caller does not need to know whether a specific eligible relation is
heap-backed or semantic-backed.

### Existing test isolation stays external

Semantic storage speeds up ordinary PostgreSQL operations. It does not require
or expose a new test-isolation API.

Existing patterns should continue to work:

```text
wrap each test in BEGIN / ROLLBACK
use savepoints inside tests
use one database per worker
use one schema per worker
use one cluster per worker
use connection pools with multiple backend processes
commit fixture data during setup
run migrations through normal tools
```

The behavior of conflicts remains PostgreSQL-like:

```text
two transactions inserting the same primary key in the same table conflict
two schemas can contain independent tables with the same keys
two databases can contain independent tables with the same keys
rollback removes uncommitted writes
commit makes writes visible to later snapshots
```

### DDL in phase 1

Because callers should not need a capture boundary, phase 1 must support a
useful subset of normal migration DDL for semantic relations.

Required phase 1 support:

```text
CREATE TABLE
CREATE INDEX for supported index shapes
ALTER TABLE ADD COLUMN without complex rewrite semantics
ALTER TABLE ADD CONSTRAINT for supported constraints
DROP TABLE
DROP INDEX
TRUNCATE
RENAME table/column/index
```

Unsupported phase 1 examples:

```text
ALTER TABLE operations that require physical heap rewrite semantics
custom table access methods
custom index access methods
unsupported partitioning behavior
exclusion constraints
storage-inspection extensions
logical replication features
```

Expected failure:

```text
ERROR: semantic storage does not support this operation on relation "users"
HINT: Disable semantic storage for this test or leave the relation heap-backed before it becomes semantic-active.
```

## Storage Selection

Semantic storage should be selected without caller syntax changes.

Preferred implementation shape:

```text
CREATE TABLE uses ordinary parser/catalog paths
pg_class and related catalogs remain real catalog tuples
relcache loads normal catalog metadata
fast-fork eligibility hook marks the relation semantic-active
executor/table/index paths route row and index access to semantic storage
```

Two implementation options are acceptable:

```text
invisible relcache override
    pg_class can continue to look like a normal heap table for compatibility,
    while fast-fork relation state routes eligible row/index access to semantic
    storage

server-selected table AM
    a built-in semantic table AM can be selected by server configuration as the
    default for eligible user tables, without requiring callers to write USING
    clauses
```

The invisible relcache override is preferred if it preserves ORM and catalog
introspection compatibility better. The table AM path is acceptable if it
proves substantially safer to integrate with executor and DDL paths.

Relation identity should remain ordinary PostgreSQL identity:

```text
database oid
namespace oid
relation oid
relfilenode or storage generation where needed
```

There is no fixture name or named epoch identifier in the user model.

## Storage Architecture

### Multi-Process Requirement

Semantic storage must be shared across PostgreSQL backend processes.

Do not use backend-private row stores for committed relation data.

Use shared-memory structures addressed through relative offsets, not raw
process-local pointers:

```c
typedef uint64 FastForkSemanticOffset;
typedef uint64 FastForkSemanticRowId;
typedef uint64 FastForkSemanticRelId;
typedef uint64 FastForkSemanticGeneration;
```

All backend processes must be able to attach to:

```text
global semantic control block
database directory
relation metadata table
committed row arenas
transaction row arenas
semantic index maps
delete/update overlay maps
shared lock table
stats counters
```

Recommended allocator shape:

```text
FastForkSemanticSharedState
  -> global control block
  -> database directory
  -> relation directory
  -> index directory
  -> transaction directory
  -> shared arenas
       -> committed row arenas
       -> committed index arenas
       -> per-transaction row arenas
       -> per-transaction index delta arenas
       -> metadata arena
```

A first implementation can use PostgreSQL dynamic shared memory and relative
pointers, or a custom postmaster-created mmap arena inherited/attached by
backend processes.

The important constraints:

```text
no raw pointers stored in shared structures
no backend-private ownership of committed semantic rows
no per-process duplicate committed relation copies
no multi-threading assumption
```

### Relation Representation

Each eligible semantic table gets a relation object:

```c
typedef struct FastForkSemanticRelation
{
    Oid                     database_id;
    Oid                     relid;
    Oid                     relnamespace;
    Oid                     reltype;
    TupleDesc               tupledesc_copy;

    FastForkSemanticRelId   semantic_relid;
    FastForkSemanticOffset  committed_rows;
    FastForkSemanticOffset  row_visibility;
    FastForkSemanticOffset  indexes;

    uint64                  committed_row_count;
    uint64                  live_row_count;

    FastForkSemanticGeneration schema_generation;
    FastForkSemanticGeneration data_generation;

    LWLockPadded           *relation_lock;
    uint32                  flags;
} FastForkSemanticRelation;
```

The system catalog still contains ordinary `pg_class`, `pg_attribute`,
`pg_type`, `pg_index`, and constraint metadata.

The semantic relation object is a fast storage-side mirror of that catalog
state.

### Row Representation

Phase 1 should prioritize correctness and broad type support over the absolute
fastest columnar layout.

Recommended first row format:

```text
shared MinimalTuple-like row blob
+ virtual row id
+ transaction visibility metadata
+ command id metadata
+ null bitmap
+ copied varlena data
```

Sketch:

```c
typedef struct FastForkSemanticRowHeader
{
    FastForkSemanticRowId       row_id;
    FastForkSemanticGeneration  inserted_generation;
    FastForkSemanticGeneration  deleted_generation;

    TransactionId               xmin;
    TransactionId               xmax;
    CommandId                   cmin;
    CommandId                   cmax;

    uint32                      flags;
    uint32                      tuple_len;
    FastForkSemanticOffset      tuple_data;
} FastForkSemanticRowHeader;
```

Committed rows are immutable except for visibility metadata that is safely
updated through semantic delete/update paths. Phase 1 may avoid in-place
mutation by storing tombstones in per-transaction and committed delete maps.

Rows inserted inside a transaction live in the transaction arena until commit:

```text
xmin = current transaction id
xmax = InvalidTransactionId
visibility = own transaction only until commit
```

On commit, pending rows and index deltas are published into committed semantic
storage. On rollback, the transaction arena and deltas are discarded.

### Tuple Data

For phase 1, store row values as copied tuple blobs:

```text
by-value datums copied normally
varlena datums detoasted and copied into semantic arena
expanded objects flattened
external TOAST pointers not preserved
```

Large values:

```text
stored in semantic varlena arena
not stored in physical TOAST relations
returned to executor as normal varlena datums
```

This deliberately removes physical TOAST costs for semantic user tables.

A later phase can add optimized fixed-width layouts:

```text
fixed-width dense row segments
separate varlena pools
column offset tables
SIMD-friendly null bitmap checks
```

Do not start there. The first large win should come from removing heap pages,
buffers, and btree pages.

### Virtual CTID

Semantic rows do not have real heap page/offset locations.

Still, PostgreSQL executor paths often expect an `ItemPointerData`.

Use virtual TIDs:

```text
block = row_id / MaxHeapTuplesPerPage + 1
offset = row_id % MaxHeapTuplesPerPage + 1
```

`ctid` behavior:

```text
SELECT ctid FROM semantic_table;
```

returns a virtual TID.

Supported:

```text
displaying ctid
RETURNING ctid
simple TID recheck inside semantic executor
```

Unsupported:

```text
pageinspect
heap_page_items
physical TID scans that expect heap pages
extensions that dereference ctid into a buffer page
```

GUC:

```text
fastfork.semantic_ctid_policy = virtual | error
```

Default:

```text
virtual
```

## Transaction and Visibility Model

### Ordinary Transaction Visibility

Each backend has ordinary PostgreSQL transaction state plus semantic overlay
metadata:

```text
current top-level transaction id
current subtransaction id
current command id
current snapshot
transaction semantic overlay
subtransaction overlay stack
```

Visible rows for a statement:

```text
committed semantic rows visible to the PostgreSQL snapshot
  minus committed rows deleted by visible transactions
  plus own transaction inserts visible by command id
  minus own transaction deletes visible by command id
```

Across backend processes:

```text
committed changes become visible according to normal PostgreSQL snapshots
uncommitted changes are not visible to other backends
snapshot rules remain the application-visible isolation boundary
```

Recommended phase 1 isolation:

```text
READ COMMITTED
```

Phase 1 may support `REPEATABLE READ` by pinning the normal snapshot and a
semantic generation at transaction start.

Unsupported in phase 1:

```text
SERIALIZABLE
predicate locking
SSI conflict tracking
```

Expected error:

```text
ERROR: semantic storage does not support SERIALIZABLE isolation
HINT: Disable semantic storage for isolation-level tests.
```

### Savepoints

Use nested transaction overlays:

```text
top-level transaction overlay
  -> subtransaction overlay
      -> child subtransaction overlay
```

On savepoint rollback:

```text
discard child inserted rows
discard child delete/update marks
discard child index deltas
restore parent overlay
```

On savepoint release:

```text
merge child overlay into parent overlay
```

On top-level transaction commit:

```text
publish transaction overlay into committed semantic relation/index state
bump relation/index generations
```

On top-level transaction rollback:

```text
discard transaction overlay
```

### Updates

No HOT updates.

For semantic tables, update is one of:

```text
transaction-private in-place update
append replacement row + tombstone old row
```

Recommended phase 1:

```text
append replacement row + tombstone old row
```

This keeps visibility simple and avoids mutating committed row data in place.

Update path:

```text
1. find visible old row
2. evaluate new tuple
3. check constraints
4. insert new semantic row in transaction overlay
5. tombstone old visible row in transaction overlay
6. update semantic indexes:
     - remove old key in transaction delete delta
     - insert new key in transaction insert delta
```

For a row inserted earlier in the same transaction, update may mutate the
transaction-local row in place before it is committed.

### Deletes

Delete path:

```text
1. find visible row
2. record tombstone in transaction overlay
3. update semantic index delete delta
```

Committed row data is not physically removed during the transaction.

### Inserts

Insert path:

```text
1. build tuple
2. evaluate defaults
3. enforce not-null
4. enforce check constraints
5. enforce unique indexes
6. enforce FKs, initially through existing trigger path or semantic lookup
7. allocate row in transaction overlay arena
8. add semantic index delta entries
```

## Semantic Indexes

### Index Representation

For eligible indexes on semantic tables, do not build physical btree pages.

Create semantic index objects:

```c
typedef struct FastForkSemanticIndex
{
    Oid                     index_relid;
    Oid                     heap_relid;

    FastForkSemanticRelId   semantic_relid;
    uint16                  natts;
    AttrNumber              key_attrs[INDEX_MAX_KEYS];

    bool                    unique;
    bool                    primary;
    bool                    immediate;
    bool                    partial;
    bool                    expression;

    Oid                     opclass_oids[INDEX_MAX_KEYS];
    Oid                     collation_oids[INDEX_MAX_KEYS];

    FastForkSemanticOffset  committed_index_map;
    FastForkSemanticOffset  transaction_delta_maps;

    LWLockPadded           *index_lock;
    uint32                  flags;
} FastForkSemanticIndex;
```

Phase 1 support:

```text
primary key indexes
unique indexes on simple columns
non-unique equality indexes on simple columns
foreign-key lookup indexes
```

Phase 2 support:

```text
ordered range indexes
multi-column ordered indexes
ORDER BY / LIMIT acceleration
partial indexes
expression indexes
collation-sensitive ordered indexes
```

Unsupported in phase 1:

```text
GIN
GiST
SP-GiST
BRIN
hash index semantics beyond simple internal hash maps
custom index AMs
expression indexes
partial indexes unless predicate evaluation is trivial
```

### Hash Index Maps

Use shared-memory hash maps for equality lookups:

```text
key datum tuple -> row id or row-id vector
```

Unique index:

```text
key -> row id
```

Non-unique index:

```text
key -> row-id vector
```

Index entries are split into:

```text
committed index map
current transaction insert delta
current transaction delete delta
```

Lookup path:

```text
1. look in current transaction insert delta
2. look in committed semantic index
3. filter out rows deleted/updated by the current transaction
4. filter by normal snapshot visibility
```

Uniqueness check path:

```text
1. check current transaction insert delta
2. check committed semantic index
3. ignore rows deleted by the current transaction where PostgreSQL semantics allow
4. ignore row versions not visible to the current uniqueness check
5. error if a visible duplicate exists
```

Uniqueness scope is normal PostgreSQL scope:

```text
database + schema + table + index + visible transaction state
```

Semantic storage must not use a global cross-test key space, and it must not use
named epochs to hide ordinary uniqueness conflicts.

### Ordered Indexes

Phase 1 can skip ordered indexes unless benchmark evidence says they are
required.

Phase 2 should add an ordered map:

```text
key -> ordered row-id set
```

Possible implementations:

```text
shared-memory skiplist
shared-memory rb-tree
ART/radix tree for sortable keys
sorted immutable committed vector + transaction delta lists
```

For unit tests, a very effective first ordered-index design may be:

```text
committed index = sorted immutable vector
transaction inserts = small sorted vector or skiplist
transaction deletes = row-id tombstone set
query = merge committed + transaction inserts while filtering tombstones
```

## Constraints

### Not Null and Check Constraints

Phase 1 should support:

```text
NOT NULL
simple CHECK constraints
immutable expression checks already supported by executor expression evaluation
```

Implementation:

```text
reuse existing expression evaluation where possible
compile/cache expression state by relation + schema generation
```

### Unique and Primary Key Constraints

Unique and primary key constraints must be enforced by semantic indexes.

Scope:

```text
committed rows visible to the uniqueness check + current transaction overlay
```

Not scope:

```text
all active tests globally
all schemas globally
all databases globally
```

That distinction preserves ordinary PostgreSQL behavior.

### Foreign Keys

Phase 1 option:

```text
fastfork.semantic_fk_mode = trigger
```

This leaves PostgreSQL's RI trigger machinery active. The trigger queries use
normal SQL, but referenced semantic tables are read through semantic storage.
This is simpler and preserves behavior, though it does not remove all FK
overhead.

Phase 2 option:

```text
fastfork.semantic_fk_mode = compiled
```

Compiled FK path:

```text
referencing insert/update
  -> compute FK key
  -> lookup referenced semantic unique/primary index
  -> error if missing

referenced delete/update
  -> lookup referencing semantic index
  -> enforce NO ACTION / RESTRICT / CASCADE / SET NULL / SET DEFAULT
```

Phase 2 can bypass SPI and trigger manager for built-in RI triggers.

### User Triggers

Phase 1 default:

```text
fastfork.semantic_trigger_mode = normal
```

User triggers still fire through normal executor trigger paths.

Supported:

```text
BEFORE INSERT/UPDATE/DELETE
AFTER INSERT/UPDATE/DELETE
row-level triggers
statement-level triggers
```

Caveat:

```text
trigger functions that inspect heap pages, physical ctid behavior, or storage internals are unsupported
```

If trigger cleanup or semantics become unsafe, mark relation ineligible before
it becomes semantic-active.

## Catalog Strategy

### Phase 1: physical catalogs, semantic user-table storage

System catalogs remain ordinary PostgreSQL relations.

This means normal migration DDL can still use:

```text
pg_class
pg_attribute
pg_type
pg_index
pg_constraint
pg_depend
relcache
syscache
typcache
```

Semantic storage mirrors catalog metadata into shared-memory relation and index
objects.

Advantages:

```text
callers run normal migrations
ORM introspection still queries real pg_catalog rows
DDL correctness remains mostly PostgreSQL's problem
the first storage win is measurable without virtual catalogs
```

Tradeoff:

```text
DDL/setup speed does not get the full semantic benefit yet
```

That is acceptable because the first target is runtime speed for ordinary unit
tests after migration and fixture setup, without caller-visible API changes.

### Phase 2: broader semantic DDL

After semantic DML is proven, broaden DDL support:

```text
ALTER TABLE variants that rewrite existing rows
partitioning subsets
generated columns
identity behavior
REINDEX
semantic relation recopy/rewrite
schema-generation invalidation
```

### Phase 3: virtual catalog rows

The most aggressive setup-speed phase is virtualizing common catalog reads:

```text
pg_class
pg_attribute
pg_type
pg_index
pg_constraint
pg_namespace
information_schema.columns
information_schema.tables
```

Instead of physically maintaining every catalog tuple/index for unit-test user
objects, generate catalog rows from semantic metadata.

This is a separate large phase. It should not block phase 1 because it is not
required for transparent caller usage.

## Eligibility

A relation is eligible when:

```text
ordinary permanent table
not a system catalog
not a TOAST table exposed as user target
not partitioned parent without support
not foreign table
not materialized view in phase 1
not unlogged/durable distinction needed
not using a custom table AM
row type can be copied into semantic arena
indexes are supported or can be ignored safely
constraints are supported or can be enforced by normal trigger/executor paths
triggers are supported or relation is marked ineligible before activation
```

A relation is ineligible when it uses:

```text
custom table AM
custom index AM required for correctness
exclusion constraints
unsupported generated columns
unsupported identity behavior
unsupported partitioning behavior
unsupported row-level security tests
storage-inspection extensions
logical replication features
```

When `fastfork.semantic_on_unsupported = heap_before_semantic`, ineligible
relations remain page-backed before semantic activation.

After a relation is semantic-active, unsupported operations must error rather
than silently falling back.

## Shared-Memory Layout

### Global Control

```c
typedef struct FastForkSemanticControl
{
    pg_atomic_uint64        global_generation;
    pg_atomic_uint64        next_row_id;
    pg_atomic_uint64        next_semantic_relid;

    FastForkSemanticOffset  database_directory;
    FastForkSemanticOffset  relation_directory;
    FastForkSemanticOffset  index_directory;
    FastForkSemanticOffset  transaction_directory;

    LWLockPadded           *control_lock;
} FastForkSemanticControl;
```

### Database State

```c
typedef struct FastForkSemanticDatabase
{
    Oid                     database_id;

    FastForkSemanticGeneration generation;

    FastForkSemanticOffset  relation_list;
    FastForkSemanticOffset  committed_arena;
    FastForkSemanticOffset  committed_index_arena;

    uint64                  relation_count;
    uint64                  row_count;
    Size                    memory_bytes;
} FastForkSemanticDatabase;
```

### Transaction State

Each backend process has local transaction overlay metadata, but row/index
storage may be in shared transaction arenas so commit can publish changes to
other processes without copying through backend-private memory.

```c
typedef struct FastForkSemanticXactOverlay
{
    TransactionId           xid;
    SubTransactionId        subid;

    FastForkSemanticOffset  pending_inserts;
    FastForkSemanticOffset  pending_deletes;
    FastForkSemanticOffset  pending_updates;
    FastForkSemanticOffset  pending_index_deltas;

    bool                    has_writes;
} FastForkSemanticXactOverlay;
```

On transaction commit:

```text
merge pending overlay into committed relation/index state
bump affected relation and index generations
```

On transaction abort:

```text
discard pending overlay
```

## Locking

### Multi-Process Locking Rules

Because the system remains multi-process, semantic storage still needs
synchronization. The point is to avoid page/buffer locks, not all locks.

Use coarse but cheap locks in phase 1:

```text
relation metadata lock
per-relation semantic DML lock
per-index semantic lock
transaction publish lock
partitioned hash locks for index maps
```

Phase 1 locking strategy:

```text
committed rows are immutable except through semantic tombstone/update maps
writes use per-relation or per-index locks
reads use normal snapshots plus generation checks
short locks protect map traversal and publication
```

Allocator contention should be minimized with:

```text
per-transaction arenas
per-backend allocation chunks
size-class freelists
append-only allocation during active transaction
bulk discard at transaction rollback
bulk publish at transaction commit
```

### Row Locks

Phase 1 can support row locks with a semantic row-lock table keyed by:

```text
database oid
relation oid
virtual row id
```

`SELECT FOR UPDATE`:

```text
lock virtual row id
conflict with another backend according to normal table scope
release through normal transaction cleanup
```

Unsupported phase 1:

```text
complex predicate locks
serializable row-lock semantics
```

For most unit tests, relation-level write locks may be sufficient initially.

## Executor Integration

### Table Access

Use a built-in test-only table access method or equivalent fast-fork table
storage hook:

```text
fastfork_semantic_tableam
```

Required callbacks:

```text
scan_begin
scan_getnextslot
scan_end
tuple_insert
tuple_update
tuple_delete
tuple_fetch_row_version
tuple_lock
relation_set_new_filelocator, semantic allocate
relation_nontransactional_truncate
relation_copy_for_cluster, unsupported phase 1
```

Do not require callers to specify this table AM. It must be selected by server
configuration, relcache override, or eligibility hook.

Do not rely on table AM alone if it cannot intercept all physical heap
assumptions. Add fast-fork executor hooks where needed.

### Sequential Scan

Semantic seq scan:

```text
iterate committed row array
filter snapshot visibility and tombstones
iterate current transaction inserted rows
filter command-id visibility
return virtual tuple slot
```

No buffer access.

No page access.

No heap visibility checks.

### Index Scan

Semantic index scan:

```text
compute scan key
lookup current transaction insert delta
lookup committed semantic index
merge row-id candidates
filter transaction tombstones
filter snapshot visibility
fetch semantic rows
return virtual tuple slots
```

For phase 1, index scan support can focus on equality quals:

```sql
WHERE id = $1
WHERE email = $1
WHERE foreign_key_id = $1
```

Unsupported index scan shapes can fall back to semantic seq scan, not physical
heap scan.

### Bitmap Scan

Phase 1 may disable bitmap scans for semantic tables.

Planner path:

```text
do not generate bitmap heap scan paths for semantic relations
```

or executor path:

```text
semantic bitmap = row-id set
```

The simple choice is to disable initially.

### Planner Integration

Planner must know semantic relations are cheap to scan and cheap to
index-lookup.

Add semantic relation size estimates:

```text
committed live row count
current transaction inserted row count
current transaction deleted row count
```

Add semantic path nodes or adapt existing path nodes:

```text
SemanticSeqScanPath
SemanticIndexScanPath
```

Costing should strongly prefer semantic index lookups for unique/equality
predicates.

Phase 1 may use existing `SeqScan` and `IndexScan` plan node types with
semantic executor routing, but explain output should be honest:

```text
Seq Scan on users  (semantic storage)
Index Scan using users_pkey on users  (semantic storage)
```

### DML Integration

Existing executor DML nodes should call semantic table operations for semantic
relations:

```text
ModifyTable
  -> semantic insert/update/delete callbacks
```

Triggers and `RETURNING` remain above the storage layer.

`RETURNING *`:

```text
materialize semantic tuple slot
```

`RETURNING ctid`:

```text
return virtual ctid
```

## Type and Datum Handling

Semantic storage must support common PostgreSQL data types:

```text
integers
bigints
numeric
text
varchar
boolean
timestamps
dates
uuid
json/jsonb
arrays
bytea
enums
composite values if copied safely
```

Datum copy rule:

```text
anything pass-by-reference must be copied into the semantic arena
anything toasted must be detoasted before storage
expanded objects must be flattened
```

Collation:

```text
hash equality indexes use type equality operators
ordered indexes require collation-aware comparison and may be phase 2
```

## Sequences and Identity

Sequence objects remain outside semantic table rows.

Phase 1:

```text
use existing sequence fast path if available
otherwise normal sequence behavior
```

Identity/serial inserts:

```text
evaluate default nextval()
store resulting value in semantic row
```

Sequence rollback policy remains governed by ordinary PostgreSQL sequence
semantics unless a separate fast-fork sequence feature is enabled by server
configuration.

## TOAST

Do not use physical TOAST relations for semantic tables.

Initial load/insert/update path:

```text
detoast external values if encountered
copy value into semantic arena
```

Read path:

```text
return normal varlena datum
```

Unsupported:

```text
queries against physical TOAST table for semantic relation
tests that inspect toast relfilenode contents
toast pointer identity expectations
```

## Vacuum, Analyze, and Maintenance

For semantic tables:

```text
VACUUM = no-op or clear semantic garbage that is older than every active snapshot
ANALYZE = update semantic row-count/stat estimates
CLUSTER = unsupported phase 1
REINDEX = rebuild semantic index maps
```

Because committed semantic rows are not stored as heap pages and rollback
discards transaction overlays wholesale, there is no heap pruning, HOT pruning,
visibility-map maintenance, or free-space-map maintenance for semantic rows.

## PostgreSQL Parallel Query

This spec preserves multi-process client/backend parallelism.

PostgreSQL parallel query workers are separate.

Phase 1 may mark semantic tables parallel-query unsafe:

```text
parallel seq scan on semantic relation: unsupported
parallel index scan on semantic relation: unsupported
```

That does not block parallel test execution, because different test clients
still use separate backend processes.

Phase 2 can support parallel query workers by attaching workers to:

```text
same semantic database state
same transaction snapshot
same statement generation
same shared row/index maps
```

But parallel query support should not be required for the first semantic storage
benchmark.

## Unsupported Operations

Semantic relations should error for:

```text
pageinspect
amcheck
heap-specific functions
btree page inspection
logical decoding
replication identity relying on heap storage
custom table AM access
custom index AM access
unsupported ALTER TABLE
unsupported CREATE INDEX
unsupported isolation levels
parallel query workers, unless semantic attach support is implemented
physical TID scans outside semantic executor
```

Important: do not silently route a semantic-active table back to heap storage.

Silent fallback risks split-brain state:

```text
semantic committed/transaction state
physical heap/index state
```

Once a relation is semantic-active, semantic storage is authoritative until the
relation is dropped or the server exits.

## Diagnostics

Do not add required SQL functions for application callers.

Optional diagnostics can be exposed for developers and benchmarks, preferably as
`pg_catalog` views or functions that are never part of the correctness path:

```sql
SELECT * FROM pg_stat_fastfork_semantic;
SELECT * FROM pg_stat_fastfork_semantic_relations;
SELECT pg_fastfork_relation_is_semantic('users'::regclass);
```

These are observability tools only. A test suite must not need to call them for
setup, isolation, capture, rollback, or cleanup.

Example stats columns:

```text
database_oid
relation_oid
relation_count
committed_row_count
committed_memory_bytes
active_transaction_count
transaction_row_count
transaction_memory_bytes
semantic_index_count
unsupported_relation_count
```

## Implementation Sketch

### New Files

Suggested files:

```text
src/backend/access/fastfork/semantic.c
src/backend/access/fastfork/semantic_relation.c
src/backend/access/fastfork/semantic_row.c
src/backend/access/fastfork/semantic_index.c
src/backend/access/fastfork/semantic_scan.c
src/backend/access/fastfork/semantic_ddl.c
src/backend/access/fastfork/semantic_xact.c
src/backend/access/fastfork/semantic_tableam.c
src/backend/utils/adt/fastfork_semanticfuncs.c

src/include/access/fastfork_semantic.h
src/include/access/fastfork_semantic_relation.h
src/include/access/fastfork_semantic_index.h
src/include/access/fastfork_semantic_xact.h
```

Likely touched files:

```text
src/backend/executor/nodeSeqscan.c
src/backend/executor/nodeIndexscan.c
src/backend/executor/nodeModifyTable.c
src/backend/optimizer/path/allpaths.c
src/backend/optimizer/path/costsize.c
src/backend/access/table/tableam.c
src/backend/catalog/index.c
src/backend/catalog/heap.c
src/backend/commands/tablecmds.c
src/backend/commands/vacuum.c
src/backend/commands/analyze.c
src/backend/utils/cache/relcache.c
src/backend/storage/ipc/ipci.c
src/backend/storage/lmgr/lwlocknames.txt
src/backend/access/transam/xact.c
```

### Insert Pseudocode

```c
void
fastfork_semantic_tuple_insert(Relation rel,
                               TupleTableSlot *slot,
                               CommandId cid,
                               int options,
                               BulkInsertState bistate)
{
    FastForkSemanticRelation *srel = LookupSemanticRelation(rel);
    FastForkSemanticXactOverlay *xact = CurrentSemanticXactOverlay();

    FastForkSemanticTuple tuple =
        FastForkSemanticCopySlotToArena(slot, xact->arena);

    FastForkSemanticCheckNotNull(srel, &tuple);
    FastForkSemanticCheckConstraints(srel, &tuple);
    FastForkSemanticCheckUniqueIndexes(srel, &tuple, xact);

    FastForkSemanticRowId rowid =
        FastForkSemanticAppendRow(srel, xact, &tuple, GetCurrentCommandId(false));

    FastForkSemanticIndexInsertAll(srel, xact, rowid, &tuple);

    slot->tts_tid = FastForkSemanticRowIdToVirtualTid(rowid);
}
```

### Scan Pseudocode

```c
bool
fastfork_semantic_scan_getnextslot(TableScanDesc scan,
                                   ScanDirection direction,
                                   TupleTableSlot *slot)
{
    FastForkSemanticScanDesc *sscan = (FastForkSemanticScanDesc *) scan;

    for (;;)
    {
        FastForkSemanticRowRef row = FastForkSemanticNextRow(sscan);

        if (!row.valid)
            return false;

        if (!FastForkSemanticRowVisible(row, sscan->snapshot))
            continue;

        FastForkSemanticFillSlot(slot, row);
        return true;
    }
}
```

### Index Lookup Pseudocode

```c
FastForkSemanticRowSet
FastForkSemanticIndexLookup(FastForkSemanticIndex *sidx,
                            ScanKey keys,
                            Snapshot snapshot)
{
    RowSet result = RowSetCreate();

    RowSetAddAll(result, XactIndexDeltaLookup(sidx, keys));
    RowSetAddAll(result, CommittedIndexLookup(sidx, keys));

    RowSetFilter(result, FastForkSemanticRowVisible, snapshot);
    RowSetFilter(result, !FastForkSemanticRowDeletedInCurrentXact, snapshot);

    return result;
}
```

## Validation

### Core Validation

Run the existing fast-fork validation:

```sh
./test-fastfork.sh core --no-reconfigure
```

with:

```text
-Dtest_semantic_storage=true
```

and recommended companion flags.

Stock behavior must remain unchanged when:

```text
-Dtest_semantic_storage=false
```

## Required SQL Tests

All required SQL tests should use ordinary PostgreSQL SQL only. They should not
call any fast-fork setup, capture, or epoch functions.

### Create, insert, and select

```sql
CREATE TABLE users (
    id bigint primary key,
    email text unique not null,
    name text
);

INSERT INTO users VALUES
    (1, 'a@example.com', 'A'),
    (2, 'b@example.com', 'B');

SELECT * FROM users ORDER BY id;
```

Expected:

```text
same rows as PostgreSQL
relation reports semantic-active through diagnostics
no heap/buffer reads for semantic table rows, according to test counters
```

### Rollback

```sql
BEGIN;
INSERT INTO users VALUES (3, 'c@example.com', 'C');
SELECT * FROM users WHERE id = 3;
ROLLBACK;

SELECT * FROM users WHERE id = 3;
```

Expected:

```text
row visible inside transaction
no row after rollback
rollback discards semantic overlay without heap dead tuples
```

### Commit visibility across backends

Session A:

```sql
BEGIN;
INSERT INTO users VALUES (200, 'shared@example.com', 'A');
COMMIT;
```

Session B:

```sql
SELECT * FROM users WHERE id = 200;
```

Expected:

```text
B sees committed row according to normal PostgreSQL snapshot rules
```

### Concurrent uniqueness

Session A:

```sql
BEGIN;
INSERT INTO users VALUES (300, 'dup@example.com', 'A');
```

Session B:

```sql
BEGIN;
INSERT INTO users VALUES (301, 'dup@example.com', 'B');
```

Expected:

```text
normal PostgreSQL unique-conflict behavior for the same table and unique index
```

If the same test uses separate schemas or databases, duplicate values are
allowed exactly as they are in PostgreSQL.

### Update without HOT

```sql
BEGIN;
UPDATE users SET email = 'new@example.com' WHERE id = 1;

SELECT * FROM users WHERE email = 'new@example.com';
SELECT * FROM users WHERE email = 'a@example.com';
ROLLBACK;
```

Expected:

```text
new row version visible inside transaction
old indexed key not visible inside transaction
old row restored after rollback
no heap HOT chain created
```

### Delete

```sql
BEGIN;
DELETE FROM users WHERE id = 1;
SELECT * FROM users WHERE id = 1;
ROLLBACK;
SELECT * FROM users WHERE id = 1;
```

Expected:

```text
no row inside transaction after delete
row returns after rollback
```

### Savepoint rollback

```sql
BEGIN;
INSERT INTO users VALUES (400, 'savepoint-a@example.com', 'A');

SAVEPOINT s;
INSERT INTO users VALUES (401, 'savepoint-b@example.com', 'B');
ROLLBACK TO s;

SELECT * FROM users WHERE id = 400; -- visible
SELECT * FROM users WHERE id = 401; -- not visible
COMMIT;
```

Expected:

```text
row 400 committed
row 401 discarded
```

### Foreign key

```sql
CREATE TABLE parent(id bigint primary key);
CREATE TABLE child(id bigint primary key, parent_id bigint references parent(id));

INSERT INTO parent VALUES (1);

INSERT INTO child VALUES (1, 1);   -- succeeds
INSERT INTO child VALUES (2, 999); -- fails
```

### Trigger behavior

```sql
CREATE TABLE audit_log(msg text);
CREATE TABLE trigger_users(id bigint primary key, email text);

CREATE FUNCTION log_user_insert() RETURNS trigger ...
CREATE TRIGGER users_ai AFTER INSERT ON trigger_users
FOR EACH ROW EXECUTE FUNCTION log_user_insert();

INSERT INTO trigger_users VALUES (1, 'trigger@example.com');
SELECT * FROM audit_log;
```

Expected:

```text
trigger fires
audit table behavior is correct if eligible or heap-backed from creation
```

### TOAST-sized value

```sql
CREATE TABLE docs(id bigint primary key, body text);
INSERT INTO docs VALUES (1, repeat('x', 100000));

SELECT length(body) FROM docs WHERE id = 1;

BEGIN;
UPDATE docs SET body = repeat('y', 120000) WHERE id = 1;
SELECT length(body) FROM docs WHERE id = 1;
ROLLBACK;
```

Expected:

```text
large values round-trip
no physical TOAST writes for semantic DML
rollback restores original large value
```

### Unsupported physical inspection

```sql
SELECT * FROM heap_page_items(get_raw_page('users', 0));
```

Expected:

```text
ERROR: physical page inspection is not supported for semantic fast-fork tables
```

## Benchmark

Benchmarks must use the same SQL shape a normal caller uses. They should not
require fast-fork setup, capture, or epoch functions.

Add an ordinary transaction rollback workload:

```sh
python3 bench/compare_pgbench.py \
  --fakewal-workload semantic-rollback \
  --rounds 5 \
  --transactions 1000 \
  --rows 200 \
  --clients 1 \
  --reuse-builds \
  --output-dir bench/results/semantic-rollback
```

Add a parallel client workload:

```sh
python3 bench/compare_pgbench.py \
  --fakewal-workload semantic-parallel-clients \
  --rounds 5 \
  --transactions 1000 \
  --rows 200 \
  --clients 8 \
  --reuse-builds \
  --output-dir bench/results/semantic-parallel-clients
```

Compare:

```text
stock PostgreSQL
current fast fork page-backed transaction rollback
semantic storage transaction rollback
semantic storage committed DML
semantic storage parallel clients using ordinary PostgreSQL transactions
```

Workload shapes:

```text
pk insert + pk lookup + rollback
pk insert + commit + cross-backend read
unique insert conflict
foreign-key insert
update indexed column
delete indexed row
select by primary key
select by foreign key
small join
large text insert/update
parallel clients using the same SQL as ordinary PostgreSQL callers
```

Required benchmark counters:

```text
semantic rows inserted
semantic rows deleted
semantic index lookups
semantic index inserts
semantic transaction memory bytes
heap buffer hits for semantic table rows
heap buffer reads for semantic table rows
btree page accesses for semantic indexes
rollback latency
commit publish latency
```

Acceptance target:

```text
semantic storage should be at least 5x faster than the current page-backed rollback path on the DML-heavy semantic-rollback workload
```

Secondary targets:

```text
rollback latency is near O(number of semantic overlay maps), not O(number of heap/index pages)
semantic-table DML does not touch heap buffers for table rows
semantic-table DML does not touch btree pages for semantic indexes
parallel clients preserve normal PostgreSQL conflict and visibility behavior
```

## Implementation Phases

### Phase 1: transparent semantic DML plus common DDL

Deliver the runtime win without caller-visible workflow changes.

Scope:

```text
automatic semantic activation for eligible user tables
ordinary CREATE TABLE / CREATE INDEX support
ordinary INSERT / UPDATE / DELETE / SELECT support
semantic seq scan
semantic equality index lookup
primary key
unique indexes
simple non-unique indexes
READ COMMITTED
savepoints
top-level commit
top-level rollback
multi-backend committed visibility
virtual ctid
large varlena copy without physical TOAST writes
system catalogs remain physical
unsupported operations error clearly
```

Exit criteria:

```text
core validation passes
transparent SQL tests pass
semantic-table DML avoids buffers/pages
semantic benchmark shows large speedup
no required application SQL changes
```

### Phase 2: constraints and planner quality

Add:

```text
compiled FK checks
ordered semantic indexes
ORDER BY / LIMIT index support
partial index support
expression index support where practical
better semantic costing
semantic ANALYZE stats
row-lock support
REINDEX rebuilds semantic maps
broader ALTER TABLE support
```

### Phase 3: transparent database/schema cloning support

Add support for ordinary test-suite isolation patterns that clone databases or
schemas without fast-fork SQL calls:

```text
CREATE DATABASE ... TEMPLATE ...
DROP DATABASE cleanup
CREATE SCHEMA / DROP SCHEMA worker isolation
metadata remapping for semantic relation directories
copy-on-write committed semantic row arenas for cloned databases
```

This phase is only for existing PostgreSQL workflows. It must not introduce a
new application-visible fast-fork lifecycle API.

### Phase 4: virtual catalog fast path

Add generated catalog rows for common ORM introspection:

```text
pg_class
pg_attribute
pg_type
pg_index
pg_constraint
pg_namespace
information_schema.tables
information_schema.columns
```

This attacks migration and ORM connection-bootstrap overhead.

### Phase 5: compiled query fast paths

Add normalized SQL execution shortcuts for common ORM statements:

```sql
SELECT * FROM t WHERE id = $1;
INSERT INTO t (...) VALUES (...);
UPDATE t SET ... WHERE id = $1;
DELETE FROM t WHERE id = $1;
SELECT * FROM t WHERE fk = $1 ORDER BY created_at DESC LIMIT $2;
```

This is intentionally later. The first big win should be storage replacement
under ordinary PostgreSQL usage.

## Risks

- Multi-process shared-memory data structures are harder than backend-private
  row stores.
- Relative pointers and shared-memory allocation bugs can corrupt all active
  tests.
- Semantic storage can diverge from physical catalogs if DDL invalidation is
  wrong.
- Transparent activation may surprise extensions that expect physical heap
  storage.
- Preserving caller behavior without a capture boundary requires more DDL
  support in phase 1.
- Unique constraints must follow ordinary PostgreSQL scope, not an artificial
  global or test-epoch scope.
- Same-database multi-backend visibility must be correct enough for application
  test pools.
- Trigger behavior can re-enter SQL and touch semantic tables recursively.
- Foreign keys through existing triggers may remain a meaningful cost until
  compiled FK checks land.
- Virtual `ctid` may surprise tests that depend on physical tuple identity.
- Unsupported operations must error early; silent heap fallback after semantic
  activation is unsafe.
- System catalogs remaining physical means setup speed will not get the full
  benefit until later phases.
- Planner/executor assumptions about heap tuples, buffer pins, and TIDs may
  require more hooks than table AM alone provides.
- Memory usage can grow quickly if transactions are large; strict memory limits
  and stats are required.
- PostgreSQL parallel query workers are separate from parallel test sessions and
  should be explicitly unsupported until implemented.

## Implementation Checklist

### Build and config

- Add `test_semantic_storage` build option.
- Add `USE_TEST_SEMANTIC_STORAGE`.
- Add semantic storage GUCs.
- Add shared-memory initialization.
- Add semantic LWLock tranche.
- Add optional semantic stats views/functions.

### Shared storage

- Implement relative-pointer shared-memory arena.
- Implement database directory.
- Implement relation directory.
- Implement index directory.
- Implement transaction directory.
- Implement per-transaction arenas.
- Implement memory limits.

### Transparent relation activation

- Hook relation creation/opening through the existing catalog/relcache path.
- Mark eligible user tables semantic-active without caller syntax changes.
- Keep ineligible relations heap-backed before semantic activation.
- Copy tuple descriptors into semantic metadata.
- Build semantic index metadata from ordinary catalog/index definitions.
- Reject unsupported relations with clear diagnostics in `require` mode.

### DDL

- Implement semantic handling for ordinary `CREATE TABLE`.
- Implement semantic handling for supported `CREATE INDEX`.
- Implement semantic handling for supported `ALTER TABLE`.
- Implement semantic handling for `DROP TABLE`.
- Implement semantic handling for `DROP INDEX`.
- Implement semantic handling for `TRUNCATE`.
- Add schema-generation invalidation.

### DML

- Implement semantic table scan.
- Implement semantic insert.
- Implement semantic update.
- Implement semantic delete.
- Implement virtual ctid.
- Implement savepoint overlays.
- Implement transaction commit publish.
- Implement transaction rollback discard.

### Indexes

- Implement unique hash index.
- Implement non-unique equality hash index.
- Implement committed index map.
- Implement transaction index delta maps.
- Implement uniqueness checks with ordinary PostgreSQL scope.
- Implement index scan executor integration.
- Disable unsupported bitmap/ordered paths initially.

### Multi-process behavior

- Let multiple backend processes read committed semantic state.
- Let multiple backend processes write through ordinary transaction semantics.
- Add transaction publish synchronization.
- Add snapshot/generation checks.
- Add same-database committed visibility tests.
- Add normal conflict behavior tests.

### Constraints and triggers

- Support not-null.
- Support check constraints.
- Support unique constraints.
- Keep FK triggers working in phase 1.
- Keep user triggers working where feasible.
- Mark unsafe trigger relations ineligible before semantic activation.

### Unsupported operations

- Reject physical page inspection.
- Reject unsupported DDL after semantic activation.
- Reject unsupported isolation levels.
- Reject unsupported custom AMs/extensions.
- Reject logical decoding/replication paths.
- Add clear error messages and hints.

### Benchmarks

- Add `semantic-rollback` workload.
- Add `semantic-parallel-clients` workload.
- Add counters proving semantic-table DML avoids heap/btree page paths.
- Compare against current page-backed rollback benchmark.
- Require a large speedup before considering the phase successful.

## Success Definition

This spec is successful only if it changes the performance shape without
changing the caller's PostgreSQL usage shape.

A small improvement is not enough.

The feature should demonstrate that eligible unit-test tables can execute common
DML without:

```text
shared buffer lookup for table rows
heap page access for table rows
line pointer manipulation
HOT update logic
FSM/VM maintenance
btree page access for semantic indexes
physical TOAST writes
page-backed rollback cleanup
```

The target is a semantic PostgreSQL-compatible test runtime inside the existing
multi-process PostgreSQL server architecture.

In other words:

```text
keep PostgreSQL processes
keep PostgreSQL SQL
keep PostgreSQL catalogs initially
keep ordinary transactions and savepoints
keep ordinary multi-backend client behavior
avoid required fast-fork schema calls
avoid required application changes

but stop storing eligible unit-test rows like durable heap pages
```
