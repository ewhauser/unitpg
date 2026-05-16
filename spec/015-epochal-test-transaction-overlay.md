# Epochal Test Transaction Overlay

## Summary

Add a test-only transaction overlay that makes per-test rollback cheap by
discarding an epoch instead of leaving PostgreSQL to clean up every page, tuple,
catalog entry, pending storage delete, and transaction artifact through the
normal rollback path.

The target unit-test workflow is:

```sql
-- Once per test worker:
CREATE TABLE ...;
CREATE INDEX ...;
INSERT INTO ...;
SELECT pg_fastfork_snapshot('fixture');

-- Before each test:
SELECT pg_fastfork_restore('fixture');

-- During each test:
BEGIN;
SELECT pg_fastfork_epoch_begin();
-- application test body mutates ordinary permanent tables
ROLLBACK;
```

The current fast fork already removes much durability work: memory-backed
storage, memory-backed transaction-status SLRUs, fake/early WAL bypass, fixture
snapshot/restore, direct buffers, trusted DDL, and startup shortcuts. This spec
targets a different cost: normal PostgreSQL rollback still has to unwind a
general-purpose transactional database mutation path.

In this mode, a test transaction writes into a per-transaction epoch overlay. On
top-level `ROLLBACK`, the fork discards the epoch's overlay maps, page arenas,
catalog overlay rows, relation-size overrides, and optional sequence state.
Base fixture pages remain untouched. Rollback cost should become mostly
independent of the number of rows and pages modified by the test.

This is not a production feature. It is for disposable unit-test databases where
the test harness wants fast isolation and does not care about durability, crash
recovery, replication, logical decoding, or preserving runtime state after the
test worker exits.

## Goals

- Make top-level per-test `ROLLBACK` cheap for ordinary permanent-table DML.
- Avoid leaving aborted heap tuples, index entries, dirty fixture pages, and
  relation-size changes in the base fixture state.
- Preserve ordinary SQL behavior during the active test transaction for
  supported operations.
- Preserve heap page layout, tuple layout, index page layout, executor
  behavior, MVCC checks, constraints, and planner behavior for supported
  workloads.
- Compose with fixture snapshot/restore: restore establishes a clean base
  image, and epoch rollback protects that image during each test.
- Compose with direct permanent buffers so heap/index code can still use
  ordinary `Buffer` handles.
- Support savepoints and subtransaction aborts with nested sub-epochs.
- Keep stock PostgreSQL behavior unchanged when the build flag is disabled.
- Fail clearly for unsupported operations instead of silently corrupting the
  fixture image.
- Validate with `test-fastfork.sh` and measure speed with the pgbench rollback
  benchmark.

## Non-Goals

- Durability across postmaster restart.
- Crash recovery of epoch state.
- Replication, logical decoding, archive recovery, PITR, base backup, or
  `pg_upgrade`.
- Making `COMMIT` of an epoch transaction fast in the first version.
- Supporting multiple independently active epoch transactions against the same
  shared buffer tags in the first version.
- Supporting arbitrary DDL inside an epoch transaction before catalog overlay
  support is integrated.
- Removing MVCC, transaction IDs, command IDs, resource owners, locks, relcache,
  syscache, or normal transaction cleanup entirely.
- Changing application SQL for normal DML workloads beyond enabling the
  test-only epoch mode.
- Replacing the existing fixture snapshot/restore feature.

## Build Flag

Add a new opt-in build option:

```text
Meson:     -Dtest_epoch_rollback=true
Autoconf:  --enable-test-epoch-rollback
C define:  USE_TEST_EPOCH_ROLLBACK
```

The option defaults to `false`.

The first implementation should require:

```text
-Dtest_mem_smgr=true
-Dtest_mem_slru=true
-Dtest_ephemeral_buffers=true
```

`test_ephemeral_buffers` is required for the first version because page
mutations need a reliable copy-on-write boundary before heap/index code mutates
page memory. Without direct permanent buffers, dirty shared-buffer copies can
diverge from `memsmgr` page ownership, making epoch discard much harder to
reason about.

DDL-in-epoch support should additionally require:

```text
-Dtest_ephemeral_catalog=true
-Dtest_trusted_ddl=true
```

until the catalog overlay is mature enough to stand alone.

## User Model

The first version uses an explicit SQL function so the test harness opts into
rollback-only behavior deliberately:

```sql
BEGIN;
SELECT pg_fastfork_epoch_begin();

-- test body
INSERT INTO accounts ...;
UPDATE users SET ...;
DELETE FROM sessions WHERE ...;

ROLLBACK;
```

`pg_fastfork_epoch_begin()` must be called before the transaction performs any
operation that would need epoch tracking.

It should fail clearly if:

- the build does not support epoch rollback
- the caller is not inside a transaction block
- the transaction already performed unsupported writes
- another active backend makes the database ineligible for first-version epoch
  mode
- the transaction is already marked as a normal transaction
- the transaction uses prepared transactions or two-phase commit
- the transaction is in a recovery, replication, logical decoding, or
  unsupported background-worker context

The first version should treat epoch transactions as rollback-only. If a
top-level epoch transaction reaches `COMMIT`, it should raise:

```text
ERROR: fast-fork epoch transactions are rollback-only
HINT: Disable epoch rollback for setup transactions, or use fixture snapshot/restore after setup.
```

## Design

Keep the fixture/base image immutable while a test transaction runs.

A relation page has one of three states:

```text
base fixture page
  visible before the epoch starts

epoch overlay page
  private mutable copy for the active test epoch

subepoch overlay page
  private mutable copy for a savepoint/subtransaction
```

Reads resolve the currently visible page:

```text
current subepoch overlay
  else parent epoch overlay
  else base fixture page
```

Writes first make the page private to the current epoch or subepoch. Heap,
index, visibility-map, FSM, TOAST, and sequence changes then mutate the private
page, not the fixture page.

On rollback:

```text
detach buffers that point at epoch pages
discard epoch overlay maps
discard epoch page arenas
discard epoch relation-size overrides
discard epoch catalog overlay rows
release normal transaction resources
bump fast-fork invalidation generations
```

The rollback path should not walk every modified tuple and should not
physically undo page contents one change at a time.

## Storage Overlay

Use the same relation identity shape as `memsmgr`:

```text
RelFileLocatorBackend
ForkNumber
BlockNumber
```

Each active epoch maintains overlay state for:

```text
relation fork existence
logical relation size
copied pages
new pages
truncated pages
unlinked relation/fork tombstones
```

The base `memsmgr` map remains authoritative for fixture pages. Epoch maps are
consulted first.

Add an epoch-aware page lookup helper in `memsmgr`:

```c
Block mem_epoch_lookup_page(RelFileLocatorBackend rlocator,
                            ForkNumber forknum,
                            BlockNumber blocknum,
                            FastForkEpochAccess access,
                            bool *found);
```

`FastForkEpochAccess` should distinguish at least:

```c
FASTFORK_EPOCH_READ
FASTFORK_EPOCH_WRITE
FASTFORK_EPOCH_EXTEND
FASTFORK_EPOCH_ZERO_EXTEND
```

Read behavior consults subepoch overlay, parent epoch overlay, then base page.
Write behavior copies the visible page into the current subepoch before any
in-place mutation. Extend creates a zeroed or caller-provided page in the
current subepoch and advances the relation-size override. Truncate and unlink
record overrides and tombstones without changing the base image.

Epoch pages should be allocated from epoch-local arenas. On rollback, the
system should be able to discard the arena wholesale rather than freeing
individual pages one by one. A practical first version can use reusable arenas
and expose a memory cap through:

```text
fastfork.epoch_memory_limit
```

Failure mode:

```text
ERROR: fast-fork epoch memory limit exceeded
HINT: Increase fastfork.epoch_memory_limit or disable epoch rollback for this test.
```

## Buffer Manager Boundary

Heap and index code mutate page memory in place after obtaining a buffer. If a
direct-backed buffer points at a base fixture page, an in-place mutation would
corrupt the fixture image before `memsmgr` can copy the page.

The first implementation should only enable epoch rollback when permanent
direct buffer mode is active. Each shared buffer descriptor in a test build
should be able to record:

```c
bool fastfork_direct_backed;
bool fastfork_epoch_backed;
FastForkEpochId fastfork_epoch_id;
FastForkPageGeneration fastfork_page_generation;
```

Add an epoch hook before page mutation:

```c
void FastForkEpochEnsureBufferPrivate(Buffer buffer);
```

Likely entry points include `LockBuffer(buffer, BUFFER_LOCK_EXCLUSIVE)`,
`LockBufferForCleanup`, buffer extension paths, heap insert/update/delete
page-lock paths, btree page split paths, visibility map and FSM modifications,
and sequence page modifications depending on the sequence policy.

Under active epoch mode, the first implementation should either skip hint-bit
setting on base pages or ensure the page is copied into the current epoch
before setting hint bits. Prefer skipping hint-bit updates on base pages for the
first patch.

On top-level rollback, scan shared buffer descriptors, detach descriptors that
point at aborted epoch pages, and then discard epoch page arenas. Scanning
`NBuffers` is acceptable in the first version.

## Transaction Boundary

Add transaction hooks:

```c
void AtStart_FastForkEpoch(void);
void AtSubStart_FastForkEpoch(SubTransactionId subid);
void AtSubCommit_FastForkEpoch(SubTransactionId subid);
void AtSubAbort_FastForkEpoch(SubTransactionId subid);
void AtEOXact_FastForkEpoch(bool isCommit);
```

On top-level abort:

```text
mark epoch aborting
run required normal transaction cleanup
detach epoch-backed buffers
discard subepochs and top-level epoch maps
discard epoch catalog overlay rows
discard or preserve sequence state according to policy
bump relcache/syscache generation if catalog state changed
release locks, snapshots, portals, resource owners normally
mark transaction status aborted normally
clear backend epoch state
```

On top-level commit, error if the epoch has writes. A later materializing commit
path can merge epoch pages into the base map, but that is not performance
critical for rollback-heavy unit tests.

Subtransaction abort discards the child subepoch without discarding parent epoch
changes. Subtransaction commit merges child metadata into the parent epoch by
relabeling or reparenting overlay metadata.

Do not remove transaction-status semantics. The system still needs normal
transaction status for snapshots, row locking, subtransactions, command
visibility, and active transaction bookkeeping.

## Catalog, DDL, And Caches

Phase 1 supports ordinary DML against schema created before the epoch starts:

```sql
INSERT
UPDATE
DELETE
SELECT ... FOR UPDATE
TRUNCATE, if relation-size overlay support is complete
COPY INTO existing tables, if it uses supported heap/index paths
```

DDL inside an epoch should fail clearly unless catalog overlay support is
enabled:

```text
ERROR: DDL inside fast-fork epoch transactions requires test_ephemeral_catalog
```

When `test_ephemeral_catalog` is enabled, catalog writes inside an epoch should
go into the epoch's catalog overlay and be tagged with epoch ID, subepoch ID,
inserting/deleting transaction IDs, command ID, and catalog generation. On
subtransaction abort, discard child catalog rows. On top-level rollback,
discard all epoch catalog rows and broadly invalidate relcache/syscache entries
that reference epoch rows.

First-version DDL support can use broad invalidation. Fine-grained invalidation
can come after correctness is proven.

The first incremental implementation may allow a conservative DDL subset inside
rollback-only epochs before the shared catalog overlay exists. In that bridge
mode, DDL still uses PostgreSQL's normal catalog path during the active epoch,
but top-level abort skips redundant per-relation storage unlinking and restores
the epoch base snapshot to discard the catalog and relation changes. This is a
correctness and compatibility step; the full catalog overlay remains the target
for larger DDL speedups.

## Sequences

Sequence behavior needs an explicit policy because PostgreSQL sequence
increments are normally not rolled back by transaction abort.

Add:

```text
fastfork.epoch_sequence_policy = preserve | rewind
```

Recommended default:

```text
preserve
```

`preserve` matches ordinary PostgreSQL transaction semantics: `nextval()`
changes survive epoch rollback. `rewind` prefers strict unit-test isolation and
restores sequence state to the epoch-start value.

## Locks And Concurrency

The implemented first multi-connection slice uses one shared database epoch.
The first backend to run `pg_fastfork_epoch_begin()` starts the epoch. Other
test connections can join by running the same function inside their own
transaction:

```sql
BEGIN;
SELECT pg_fastfork_epoch_begin();
```

All joined transactions share one `memsmgr` overlay. Rollback of an individual
participant decrements the shared participant count but leaves the overlay alive
for other joined transactions. The final participant rollback drops database
buffers, discards the overlay maps/pages, resets OID state, and clears the
shared epoch state.

This deliberately avoids giving each backend its own overlay because shared
buffer descriptors are still keyed by relation/fork/block, not by epoch. Writes
during a shared epoch require the connection to have joined the epoch; otherwise
the fork errors instead of letting a normal transaction mutate or commit into
the shared rollback overlay. Snapshot/restore remain base-image operations and
should be run while the pool is quiesced, before starting the shared test epoch.

## Unsupported Operations

The first version should fail clearly for:

- `COMMIT` of an epoch transaction with writes
- two-phase commit / prepared transactions
- logical decoding
- replication slots
- parallel query touching epoch-backed pages
- background workers touching epoch-backed pages
- DDL without catalog overlay support
- extension scripts unless explicitly supported
- concurrent index builds
- operations requiring durable WAL or crash recovery
- operations requiring real relation file descriptors
- multi-backend concurrent use of the same database during an epoch
- changing tablespaces or relation persistence in unsupported ways
- unsupported sequence behavior if the selected sequence policy cannot be
  honored

Failure should happen before mutation whenever possible. Once an epoch has
copied or modified pages, unsupported operations should raise `ERROR` rather
than silently falling back to normal behavior.

## Implementation Sketch

Suggested files:

```text
src/backend/storage/smgr/fastfork_epoch.c
src/include/storage/fastfork_epoch.h
src/backend/utils/adt/fastfork_epochfuncs.c
```

Potentially share code with existing fast-fork support files if the repository
already has a preferred location.

Core structs:

```c
typedef uint64 FastForkEpochId;
typedef uint64 FastForkSubepochId;
typedef uint64 FastForkPageGeneration;

typedef enum FastForkEpochAccess
{
    FASTFORK_EPOCH_READ,
    FASTFORK_EPOCH_WRITE,
    FASTFORK_EPOCH_EXTEND,
    FASTFORK_EPOCH_ZERO_EXTEND
} FastForkEpochAccess;

typedef struct FastForkEpoch
{
    FastForkEpochId id;
    Oid database_id;
    BackendId owner_backend;
    TransactionId top_xid;

    bool rollback_only;
    bool has_writes;
    bool has_catalog_writes;
    bool aborting;

    FastForkSubepochId current_subepoch;

    HTAB *relation_overrides;
    HTAB *page_overrides;
    HTAB *catalog_overrides;

    FastForkEpochArena *arena;
    FastForkPageGeneration generation;
} FastForkEpoch;
```

SQL API:

```sql
SELECT pg_fastfork_epoch_begin();
SELECT pg_fastfork_epoch_status();
```

`pg_fastfork_epoch_status()` can expose validation information such as active
state, epoch ID, page count, relation count, memory bytes, subepoch depth, and
sequence policy.

## Validation

Run:

```sh
./test-fastfork.sh core --no-reconfigure
```

with:

```sh
-Dtest_fake_wal=true
-Dtest_no_wal_assembly=true
-Dtest_no_bg_jobs=true
-Dtest_mem_smgr=true
-Dtest_mem_slru=true
-Dtest_ephemeral_buffers=true
-Dtest_epoch_rollback=true
```

Required SQL tests:

- Basic DML rollback against a restored fixture.
- Index correctness during and after epoch rollback.
- TOAST correctness.
- FSM and visibility-map correctness.
- Savepoint rollback and subtransaction commit correctness.
- Truncate correctness if relation-size overlay support is complete.
- Sequence policy behavior for `preserve` and `rewind`.
- Unsupported commit error.
- Unsupported DDL error without catalog overlay.
- DDL rollback tests once catalog overlay support is enabled.

## Performance Gate

Add an epoch rollback mode to the pgbench comparison harness:

```sh
python3 bench/compare_pgbench.py \
  --fakewal-workload epoch-rollback \
  --rounds 5 \
  --transactions 200 \
  --rows 200 \
  --reuse-builds \
  --output-dir bench/results/epoch-rollback
```

The benchmark should compare:

```text
stock PostgreSQL permanent-table rollback
current fast fork without epoch rollback
fast fork with epoch rollback
```

The workload should restore fixture state, begin an epoch transaction, perform
ordinary permanent-table `INSERT`/`UPDATE`/`DELETE` work through indexes and
constraints, and roll back.

Passing means:

- validation passes
- benchmark completes
- fast fork with epoch rollback is faster than stock PostgreSQL on
  permanent-table rollback
- fast fork with epoch rollback is faster than the same fast fork with epoch
  rollback disabled
- rollback latency does not scale linearly with the number of modified rows
- fixture snapshot/restore remains correct and does not regress materially

## Implementation Checklist

### Phase 1: DML-only epoch rollback

- Add `test_epoch_rollback` Meson/autoconf build option.
- Add `USE_TEST_EPOCH_ROLLBACK` config define.
- Add SQL function `pg_fastfork_epoch_begin()`.
- Add backend-local epoch state and shared epoch metadata.
- Add epoch page arenas and overlay maps.
- Add relation-size override support.
- Add relation/fork tombstone support.
- Add `memsmgr` epoch lookup for read/write/extend/truncate/unlink.
- Require permanent direct buffer mode for epoch activation.
- Add buffer descriptor epoch metadata.
- Add `FastForkEpochEnsureBufferPrivate()` before in-place page mutation.
- Audit heap, btree, FSM, visibility map, TOAST, and sequence page mutation
  paths.
- Skip or copy-on-write-protect hint-bit updates.
- Add buffer detachment on epoch rollback.
- Add top-level transaction abort hook.
- Add subtransaction begin/commit/abort hooks.
- Support savepoint rollback through subepochs.
- Mark epoch transactions rollback-only.
- Reject `COMMIT` with writes.
- Reject DDL unless catalog overlay support is enabled.
- Add shared epoch participant state so multiple connections can join the same
  rollback-only epoch.
- Add sequence policy GUC with default `preserve`.
- Add memory limit GUC and clear exhaustion errors.
- Add SQL regression tests for DML, indexes, savepoints, truncate, TOAST, and
  sequences.
- Add pgbench `epoch-rollback` workload.

### Phase 2: Catalog overlay integration

- Tag catalog overlay rows with epoch/subepoch IDs.
- Route supported catalog writes into epoch catalog overlay.
- Make relcache/syscache lookups see current epoch catalog rows.
- Discard catalog overlay rows on subtransaction/top-level rollback.
- Add coarse relcache/syscache generation bump.
- Support ordinary table/index/constraint/sequence DDL inside epoch.
- Add DDL rollback tests.
- Add benchmark variant with test-created objects inside epoch.

### Phase 3: Richer Multi-connection Epoch Support

- Add an optional coordinated quiesce protocol for harnesses that want the
  server to enforce final rollback barriers across all pool connections.
- Decide whether read-only nonparticipants should auto-join or continue to fail
  on utility/write paths while a shared epoch is active.
- Add broader multi-connection tests for DDL, sequences, catalog cache
  invalidation, and connection churn during active epochs.

## Risks

- The largest correctness risk is missing an in-place page mutation before
  copy-on-write. One missed path can corrupt the fixture image.
- Hint-bit and visibility-map updates are easy to underestimate because they
  look like optimizations but still mutate pages.
- Shared buffer descriptors are not epoch-keyed. First-version single-backend
  enforcement is important.
- Buffer descriptors can point at discarded epoch memory if detachment ordering
  is wrong.
- Subtransaction behavior can become subtly wrong if child overlays are merged
  or discarded incorrectly.
- Sequence rollback semantics differ from ordinary table rollback. The policy
  must be explicit and tested.
- DDL support can become too broad too early. Phase 1 should support DML
  against existing fixture schema first.
- Catalog overlay and relcache/syscache invalidation must agree.
- Memory usage can spike when a test updates many pages. The memory limit must
  produce clear failures.
- A fallback to normal rollback after epoch writes begin is unsafe.
- The feature's success depends on benchmark shape. The benchmark must use
  ordinary permanent tables, indexes, and rollback-heavy test bodies rather than
  temp-only workloads.
