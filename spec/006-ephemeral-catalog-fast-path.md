# Ephemeral Catalog Fast Path

## Summary

Add a test-only fast path for DDL-heavy unit-test workloads by moving newly
created and modified catalog state into an ephemeral in-memory catalog overlay.
PostgreSQL DDL is expensive because it writes many catalog heap tuples, updates
catalog indexes, records dependencies, sends invalidations, advances command
counters, rebuilds relcache/syscache state, and then often rolls all of it back
at the end of a test.

The fast fork should keep SQL semantics for DDL, name lookup, relcache,
syscache, MVCC visibility, and rollback inside the running cluster, but avoid
durable catalog heap/index churn for objects created by disposable tests.

## Goals

- Speed up common unit-test DDL: `CREATE TABLE`, `CREATE INDEX`, `ALTER TABLE`,
  `DROP TABLE`, constraints, defaults, sequences, and temporary schemas.
- Preserve ordinary query behavior against objects created through the fast
  path.
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
- Weakening parser, planner, executor, relcache, syscache, MVCC, or lock
  correctness for the supported DDL subset.

## Build Flag

Add a new build option:

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

This mode composes naturally with later in-memory storage and in-memory SLRU
work:

```sh
  -Dtest_mem_smgr=true \
  -Dtest_mem_slru=true
```

## Primary Targets

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

Run the fast-fork validation script with ephemeral catalog mode enabled:

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
- DDL, name lookup, relcache, syscache, indexes, constraints, dependency-backed
  drops, MVCC, savepoints, rollback, and relation cleanup remain correct for
  supported objects.
- Any skipped tests are explicitly incompatible with the fast-fork feature set
  or missing local test dependencies.

Additional high-value checks:

- Create table, index, constraint, default, sequence; query it; roll back; then
  verify the objects are gone.
- Create DDL in one transaction and verify another backend cannot see it before
  commit.
- Commit supported ephemeral DDL and verify another backend can see it.
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

## Implementation Checklist

- Add Meson and autoconf build options for `test_ephemeral_catalog`.
- Add `USE_TEST_EPHEMERAL_CATALOG` to generated config headers.
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
- Teach `test-fastfork.sh` to configure `-Dtest_ephemeral_catalog=true`.
- Teach `bench/compare_pgbench.py` to configure the fork build with
  `-Dtest_ephemeral_catalog=true`.
- Run the validation and benchmark gates above.

## Risks

- Catalog semantics are broad; too-large a first pass can become unmergeable.
  Start with ordinary table/index/constraint/sequence DDL.
- Syscache and relcache must agree. If one sees overlay rows and the other does
  not, planner/executor behavior will become inconsistent.
- Transaction visibility bugs in catalog state can make tests pass in a single
  backend but fail with multiple connections.
- Dependency handling is needed for safe drop/rollback behavior; stubbing it
  out risks leaked objects.
- Exact catalog-introspection tests may expose unsupported object types. Those
  should either be added deliberately or excluded from the fast-fork validation
  set with a clear reason.
