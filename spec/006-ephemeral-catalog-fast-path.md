# DDL Snapshot and Ephemeral Catalog Fast Path

## Summary

Add test-only fast paths for DDL-heavy unit-test workloads. The most common
unit-test pattern is:

1. run schema DDL, indexes, constraints, and fixture/sample data setup
2. run each test inside a transaction
3. roll the test transaction back

PostgreSQL DDL is expensive because it writes many catalog heap tuples, updates
catalog indexes, records dependencies, sends invalidations, advances command
counters, rebuilds relcache/syscache state, and then often rolls all of it back
or recreates it for the next test.

The fast fork should keep SQL semantics for DDL, name lookup, relcache,
syscache, MVCC visibility, and rollback inside the running cluster. It should
avoid repeatedly paying full catalog/storage setup cost when the test workload
is disposable.

The recommended first implementation is a DDL fixture snapshot/restore path:
create the schema and fixture data once, capture an in-memory snapshot, then
restore that snapshot before each test. A full in-memory catalog overlay remains
the follow-on design for tests that create unique schema objects dynamically
inside each test.

## Recommended Strategy

Use a two-tier design:

1. **Fixture snapshot/restore first.**
   Optimize the common setup-once/run-many-tests path by restoring a captured
   in-memory database state rather than replaying DDL or rolling catalog changes
   through normal MVCC cleanup for every test.
2. **Catalog overlay second.**
   Optimize tests that generate fresh DDL inside each test transaction by moving
   supported catalog writes into an in-memory overlay.

Snapshot/restore should be the default recommendation because it preserves
ordinary PostgreSQL DDL behavior during setup. It avoids needing to model every
catalog edge case in the first fast path, and it handles tables, indexes,
constraints, sequences, defaults, and seed data together.

## Goals

- Speed up common unit-test setup: `CREATE TABLE`, `CREATE INDEX`,
  `ALTER TABLE`, constraints, defaults, sequences, and fixture/sample data.
- Restore the post-setup database state quickly before each test.
- Preserve ordinary query behavior against objects restored from a snapshot.
- Preserve ordinary PostgreSQL DDL behavior while creating the snapshot.
- Preserve ordinary query behavior against objects created through the fast
  overlay path.
- Preserve transaction visibility: uncommitted DDL is visible to its transaction
  and not to unrelated transactions.
- Preserve rollback: aborting the test transaction discards catalog changes and
  removes created relations.
- Share committed ephemeral catalog state across backends in the same postmaster
  when tests use multiple connections.
- Keep stock PostgreSQL catalog behavior as the default.
- Validate correctness with the fast-fork validation script and measure speed
  with the repeatable pgbench benchmark.

## Non-Goals

- Durable catalog changes across postmaster restarts.
- Crash recovery of catalog state.
- Logical decoding, replication, base backup, or `pg_upgrade` support.
- Perfect support for every PostgreSQL extension object type in the first
  version.
- Replacing long-lived bootstrap system catalogs.
- Automatically rewriting ordinary user DDL into temporary tables.
- Allowing snapshot restore while other backends are actively using the
  database in the first version.
- Weakening parser, planner, executor, relcache, syscache, MVCC, or lock
  correctness for the supported DDL subset.

## Build Flag

Use the existing fast-fork catalog build option:

- Meson: `-Dtest_ephemeral_catalog=true`
- Autoconf: `--enable-test-ephemeral-catalog`
- C define: `USE_TEST_EPHEMERAL_CATALOG`

The option should default to `false`. When disabled, all catalog writes,
lookups, invalidations, and dependency handling must match upstream PostgreSQL.

Example fast-fork build:

```sh
meson setup build-fastfork \
  -Dtest_fake_wal=true \
  -Dtest_no_bg_jobs=true \
  -Dtest_ephemeral_catalog=true
```

This mode composes naturally with in-memory storage and in-memory SLRU work:

```sh
  -Dtest_mem_smgr=true \
  -Dtest_mem_slru=true
```

## Phase 1: Fixture Snapshot/Restore

### User Model

The intended unit-test workflow is:

```sql
-- Once per test process or suite:
CREATE TABLE ...;
CREATE INDEX ...;
INSERT INTO ...; -- fixture/sample data
SELECT pg_fastfork_snapshot('fixture');

-- Before each test:
SELECT pg_fastfork_restore('fixture');
BEGIN;
-- run test
ROLLBACK;
```

The exact SQL API can change, but the behavior should remain:

- snapshot captures a named in-memory database state
- restore makes the current database match that snapshot
- restore is much faster than dropping/recreating schema or replaying DDL
- normal SQL semantics remain unchanged after restore

### Snapshot Contents

A fixture snapshot must capture enough state to make restored tests behave as
if setup had just completed:

- `memsmgr` relation fork metadata and page contents
- catalog heap/index relation contents for objects created during setup
- relation mapper and relfilenode identity needed to open restored relations
- sequence state for fixture-created sequences
- OID and relfilenode counters, so restored tests produce repeatable object IDs
  where PostgreSQL semantics allow it
- transaction status horizon required for snapshot visibility
- relcache/syscache invalidation generation, so stale descriptors are not reused

When `test_mem_smgr` is enabled, relation data should be captured as in-memory
pages. The preferred implementation is copy-on-write page snapshots rather than
full page copies:

- snapshot records the current fork/page map generation
- later page writes clone pages when they would modify a snapshotted page
- restore drops pages and fork metadata newer than the snapshot generation
- restore reinstates the saved fork map, relation sizes, and sequence state

This makes fixture data cheap to keep while allowing tests to mutate it freely.

### Restore Semantics

The first version should keep restore deliberately narrow and safe:

- restore is only allowed outside an active transaction block
- restore requires no other active backend in the target database, except the
  caller
- restore invalidates all local relcache/syscache entries
- restore sends coarse shared invalidations if other idle backends are allowed
  in a later version
- restore discards uncommitted relation storage and catalog state newer than the
  snapshot
- restore resets transaction-local state only through ordinary transaction-end
  cleanup; it must not leave resource owners, locks, portals, or snapshots
  dangling

If the first implementation only supports a single backend, it should fail
clearly when another backend is connected. Multi-connection restore can come
later with a coordinated quiesce/invalidation protocol.

### Storage Boundary

The snapshot boundary should sit below SQL but above physical durability:

- `memsmgr` owns relation page snapshot/restore
- transaction-status SLRUs own any required transaction horizon reset
- relcache/syscache receive coarse invalidation hooks
- sequence state is restored through sequence/catalog state rather than by
  replaying SQL

Avoid replaying SQL during restore. Replay would preserve behavior, but it would
miss the main performance goal.

### Why This Comes Before a Catalog Overlay

DDL setup can use stock PostgreSQL catalog behavior once. That means the first
fast path does not need to emulate every catalog write, dependency edge, or
relcache/syscache interaction. Snapshot/restore also handles fixture data, which
a catalog-only overlay does not.

The catalog overlay is still valuable, but it should target the narrower case
where tests create new schema objects inside the per-test transaction after the
fixture snapshot has already been restored.

## Phase 2: Catalog Overlay

Phase 2 implements an in-memory catalog overlay for per-test dynamic DDL. It is
not the preferred first step for the common setup-once workflow.

## Primary Overlay Targets

### DDL Entry Points

Main files:

- `src/backend/commands/tablecmds.c`
- `src/backend/commands/indexcmds.c`
- `src/backend/commands/sequence.c`
- `src/backend/commands/typecmds.c`
- `src/backend/catalog/heap.c`
- `src/backend/catalog/index.c`

Important entry points:

- `DefineRelation`
- `DefineIndex`
- `RemoveRelations`
- `heap_create_with_catalog`
- `index_create`
- sequence creation and ownership wiring
- table constraint/default storage

The first implementation should target ordinary unit-test schema objects:

- heap tables
- indexes
- toast metadata needed for created tables
- sequences used by identity/serial columns
- column defaults
- check constraints
- primary/unique constraints backed by indexes

### Catalog Tuple Boundary

Main file:

- `src/backend/catalog/indexing.c`

Catalog write helpers are a clean interception point:

- `CatalogTupleInsert`
- `CatalogTupleInsertWithInfo`
- `CatalogTuplesMultiInsertWithInfo`
- `CatalogTupleUpdate`
- `CatalogTupleUpdateWithInfo`
- `CatalogTupleDelete`

Under `USE_TEST_EPHEMERAL_CATALOG`, supported catalog relations should write to
the in-memory overlay instead of doing normal catalog heap/index writes.
Unsupported catalog relations should fall back to normal behavior or raise a
clear unsupported error, depending on the object type.

### Lookup and Cache Boundaries

Main files:

- `src/backend/utils/cache/catcache.c`
- `src/backend/utils/cache/relcache.c`
- `src/backend/catalog/namespace.c`
- `src/backend/catalog/objectaddress.c`
- `src/backend/catalog/dependency.c`

Reads must see overlay rows through the same APIs callers already use:

- syscache lookups
- catcache scans
- relcache build/rebuild
- namespace/name lookup
- object address lookup
- dependency checks for supported objects

Avoid broad call-site rewrites. The overlay should integrate at cache/catalog
access boundaries so parser, planner, and DDL code continue to use existing
lookup APIs.

## Design

### Catalog Overlay

Create a shared in-memory overlay keyed by catalog OID and catalog key.

At minimum, support these catalogs for ordinary table/index DDL:

- `pg_class`
- `pg_attribute`
- `pg_type`
- `pg_index`
- `pg_constraint`
- `pg_depend`
- `pg_attrdef`
- `pg_sequence`
- `pg_namespace` for temporary/test-created schemas if needed

Each overlay row stores:

- catalog OID
- tuple data in catalog tuple format
- object identity and relevant unique lookup keys
- inserting transaction ID
- deleting transaction ID, if deleted
- command ID or equivalent visibility metadata
- subtransaction nesting information for rollback

The overlay should preserve enough heap tuple shape that existing syscache and
relcache code can consume rows without special object-specific structs.

### Visibility

The overlay must honor transaction visibility rules for catalog changes:

- A transaction sees its own DDL after the appropriate command boundary.
- Other transactions do not see uncommitted DDL.
- Committed ephemeral DDL is visible to other backends in the same postmaster.
- Aborted DDL disappears.
- Dropped objects become invisible according to normal transaction visibility.

Use transaction IDs and command IDs rather than ad hoc backend flags. This keeps
the behavior aligned with MVCC and makes multi-connection tests possible.

### Rollback and Subtransactions

Track overlay changes by top-level transaction and subtransaction.

On subtransaction abort:

- discard rows inserted by the subtransaction
- restore rows deleted or updated by the subtransaction
- remove relcache/syscache entries for discarded objects

On top-level abort:

- discard all uncommitted overlay changes from the transaction
- drop relation storage created by those objects
- clear related relcache/syscache state

On top-level commit:

- mark overlay rows committed in memory
- make them visible to other backends
- keep them ephemeral; do not flush them to disk

If the first implementation only supports rollback-only workloads, it should
fail clearly when a transaction tries to commit supported ephemeral DDL. The
preferred target is commit-visible-in-memory because it supports tests with
multiple connections.

### Catalog Indexes

The overlay needs lightweight in-memory indexes for the lookup patterns used by
syscache, relcache, and namespace lookup. Initial indexes should cover:

- relation by OID
- relation by `(relname, relnamespace)`
- type by OID
- type by `(typname, typnamespace)`
- attributes by `(attrelid, attnum)`
- attributes by `(attrelid, attname)`
- indexes by `indrelid`
- constraints by OID
- constraints by `(conname, connamespace)`
- dependencies by depender and referenced object

These do not need to mirror PostgreSQL's physical catalog indexes exactly. They
only need to serve the lookup paths used by supported DDL and queries.

### Relcache and Syscache Integration

Relcache and syscache must treat overlay rows as authoritative for supported
objects.

Required behavior:

- building a relation descriptor for an ephemeral table/index works
- planner and executor can open ephemeral relations by OID
- namespace lookup finds ephemeral relations
- `DROP`, `ALTER`, and dependency checks can find ephemeral objects
- cache invalidation removes or rebuilds affected entries

Prefer overlay-aware lookup functions inside cache/catalog layers over changing
DDL call sites one by one.

### Dependency Handling

Dependency records are required for `DROP`, constraint/index ownership, serial
sequences, and extension-like object relationships. Under the fast path:

- write supported dependency rows into the overlay
- make `performDeletion` and object-address lookup consult overlay dependency
  rows
- preserve restrict/cascade behavior for supported object types

Unsupported dependency types should either fall back to normal catalog storage
or produce a clear error in fast-fork mode.

### Command Counters and Invalidations

The fast path should avoid expensive global invalidation churn where possible,
but it cannot skip local visibility transitions.

Required behavior:

- command-counter boundaries make newly created objects visible to later
  commands in the same transaction
- relcache/syscache entries are invalidated or refreshed when overlay rows
  change
- cross-backend invalidations are sent for committed ephemeral DDL when another
  backend could have cached stale state

The first version can use coarse invalidation for overlay-backed catalogs. Once
correct, benchmark results can guide whether finer-grained invalidation matters.

### Storage Interaction

Catalog overlay does not replace relation storage. Creating an ephemeral table
must still create relation forks through the storage manager. Dropping or
rolling back the table must unlink those forks.

When combined with `test_mem_smgr`, both catalog metadata and relation storage
stay in memory. Without `test_mem_smgr`, this mode may still reduce catalog
write/index overhead while relation data uses normal storage.

### Unsupported Objects

The first implementation can be conservative. Unsupported DDL should fail
clearly in fast-fork mode instead of silently corrupting catalog state.

Examples likely outside the first pass:

- extensions and extension scripts
- publications/subscriptions
- foreign data wrappers and foreign tables
- custom access methods
- event triggers
- privileges/security labels/default ACLs beyond simple owner behavior
- concurrent index builds
- partitioned-table edge cases beyond simple parent/child metadata

## Validation

The spec is satisfied when both validation paths pass on the current fork.

### Correctness Gate

Run the fast-fork validation script with snapshot/restore and ephemeral catalog
mode enabled:

```sh
./test-fastfork.sh --wipe
```

After `test_ephemeral_catalog` is wired into the script, this should configure
the build with:

```sh
-Dtest_fake_wal=true -Dtest_no_bg_jobs=true -Dtest_ephemeral_catalog=true
```

Passing means:

- The script exits successfully.
- All selected compatible tests pass.
- Snapshot create/restore succeeds after schema and fixture setup.
- A restored test can query fixture tables, use indexes, enforce constraints,
  and mutate fixture data.
- Rolling back a test transaction returns to the restored state.
- Restoring the named fixture again discards all changes from the previous test.
- DDL, name lookup, relcache, syscache, indexes, constraints, dependency-backed
  drops, MVCC, savepoints, rollback, and relation cleanup remain correct for
  supported snapshot and overlay objects.
- Any skipped tests are explicitly incompatible with the fast-fork feature set
  or missing local test dependencies.

Additional high-value checks:

- Create table, index, constraint, default, sequence, and seed rows; snapshot;
  mutate rows and sequences; restore; verify all state matches the snapshot.
- Restore twice in a row and verify OID, relfilenode, sequence, and row state
  remain stable.
- Attempt restore while another backend is connected and verify the first
  version fails clearly, or verify coordinated restore if multi-backend support
  has been implemented.
- Create table, index, constraint, default, sequence; query it; roll back; then
  verify the overlay-created objects are gone.
- Create DDL in one transaction and verify another backend cannot see it before
  commit, if the overlay phase supports multiple backends.
- Commit supported ephemeral DDL and verify another backend can see it, if the
  overlay phase supports committed overlay rows.
- Abort a subtransaction that created a table and verify outer transaction
  catalog state remains valid.

### Performance Gate

Run the repeatable pgbench comparison:

```sh
python3 bench/compare_pgbench.py \
  --rounds 5 \
  --transactions 200 \
  --rows 200
```

After the benchmark harness knows about `test_ephemeral_catalog`, the fork
build should include:

```sh
-Dtest_fake_wal=true -Dtest_no_bg_jobs=true -Dtest_ephemeral_catalog=true
```

Passing means:

- Baseline and fork runs complete successfully.
- Results are written under `bench/results/`.
- The summary records the fork speed relative to the cached baseline build.
- Snapshot/restore benchmark mode records the cost of restore separately from
  test execution.
- Per-test restore plus rollback is faster than replaying schema DDL and
  fixture inserts for each test.

## Implementation Checklist

### Phase 1: Snapshot/Restore

- Add a test-only SQL/API surface for creating, restoring, listing, and dropping
  named fast-fork snapshots.
- Add `memsmgr` snapshot support for fork metadata, relation sizes, and page
  contents.
- Implement copy-on-write page handling for pages retained by a snapshot.
- Restore sequence state and any counters required for repeatable test setup.
- Restore or validate transaction-status state needed for MVCC visibility.
- Add a database quiescence check; initially require a single active backend.
- Invalidate relcache/syscache state after restore.
- Add clear errors when restore is attempted inside a transaction block or while
  unsupported backends are active.
- Teach the benchmark harness to measure setup replay versus snapshot restore.
- Add validation tests for table/index/constraint/sequence/fixture restore.

### Phase 2: Catalog Overlay

- Add shared-memory overlay storage for supported catalog rows.
- Add in-memory indexes for supported syscache/relcache lookup keys.
- Intercept supported catalog writes in `CatalogTupleInsert`,
  `CatalogTupleUpdate`, and `CatalogTupleDelete`.
- Make syscache/catcache lookups consult overlay rows.
- Make relcache build/rebuild consult overlay rows for supported objects.
- Make namespace relation/type lookup consult overlay indexes.
- Make dependency recording and deletion consult overlay rows for supported
  objects.
- Track overlay changes by transaction/subtransaction for commit and rollback.
- Wire relcache/syscache invalidation to overlay changes.
- Ensure relation storage is unlinked when overlay-created relations abort or
  are dropped.
- Add clear unsupported-DDL errors for object types outside the first pass.
- Run the validation and benchmark gates above.

## Risks

- Snapshot restore must not leave backend-local state dangling. The first
  version should require restore outside a transaction and with no active
  competing backends.
- Copy-on-write bugs can make fixture data leak between tests. Add targeted
  tests that mutate heap, index, toast, and sequence state after restore.
- Catalog semantics are broad; too-large an overlay first pass can become
  unmergeable. Start overlay work with ordinary table/index/constraint/sequence
  DDL after snapshot/restore is working.
- Syscache and relcache must agree. If one sees overlay rows and the other does
  not, planner/executor behavior will become inconsistent.
- Transaction visibility bugs in catalog state can make tests pass in a single
  backend but fail with multiple connections.
- Dependency handling is needed for safe drop/rollback behavior; stubbing it
  out risks leaked objects.
- Exact catalog-introspection tests may expose unsupported object types. Those
  should either be added deliberately or excluded from the fast-fork validation
  set with a clear reason.
