# No Durable Maintenance Mode

## Summary

Add a test-only mode that removes checkpoint, vacuum, freeze, visibility-map,
and wraparound-maintenance work that only matters for durable, long-running
PostgreSQL clusters. The fast fork still needs MVCC visibility, rollback, row
locking, and transaction status while the test cluster is running. It does not
need to preserve pages across crashes, advance freeze horizons for future
uptime, maintain durable visibility maps, or protect against billion-transaction
wraparound in disposable unit-test runs.

This is the "short-lived cluster" contract: keep correctness for active tests,
but stop paying foreground costs for maintenance whose only purpose is long-term
survival.

## Goals

- Make checkpoints cheap no-ops except for the minimum startup/shutdown shape
  still required by the existing server lifecycle.
- Make ordinary `VACUUM` cheap in fast-fork mode.
- Disable tuple freezing, eager freezing, freeze failsafe, and anti-wraparound
  pressure.
- Disable visibility-map and freespace-map maintenance where it is only an
  optimization or durability aid.
- Skip opportunistic heap page pruning during scans when it is not required for
  query correctness.
- Preserve MVCC visibility, transaction rollback, tuple locking, and normal
  DML semantics for short-lived tests.
- Keep the change behind an opt-in build flag so upstream merges remain
  manageable.
- Validate correctness with the fast-fork validation script and measure speed
  with the repeatable pgbench benchmark.

## Non-Goals

- Durability across postmaster restarts.
- Crash recovery.
- Transaction ID or MultiXact wraparound protection for long-lived clusters.
- Accurate `pg_class.relfrozenxid`, `pg_class.relminmxid`,
  `pg_database.datfrozenxid`, or `pg_database.datminmxid` maintenance.
- Accurate visibility-map or freespace-map contents.
- Production-safe VACUUM behavior.
- Removing transaction status SLRUs or MVCC checks.

## Build Flag

Add a new build option:

- Meson: `-Dtest_no_durable_maintenance=true`
- Autoconf: `--enable-test-no-durable-maintenance`
- C define: `USE_TEST_NO_DURABLE_MAINTENANCE`

The option should default to `false`. When disabled, checkpoint, vacuum,
freeze, visibility-map, and wraparound behavior must match upstream PostgreSQL.

Example fast-fork build:

```sh
meson setup build-fastfork \
  -Dtest_fake_wal=true \
  -Dtest_no_bg_jobs=true \
  -Dtest_no_durable_maintenance=true
```

This mode composes with the other fast-fork specs:

```sh
  -Dtest_mem_smgr=true \
  -Dtest_mem_slru=true \
  -Dtest_no_wal_assembly=true
```

## Primary Targets

### Checkpoints

Main files:

- `src/backend/access/transam/xlog.c`
- `src/backend/postmaster/checkpointer.c`
- `src/backend/storage/buffer/bufmgr.c`
- `src/include/postmaster/bgwriter.h`
- `src/include/access/xlog.h`

Important paths:

- `CreateCheckPoint`
- `CheckPointGuts`
- `RequestCheckpoint`
- `CheckPointBuffers`
- `ProcessSyncRequests`
- `SyncPostCheckpoint`
- SQL `CHECKPOINT`

Under `USE_TEST_NO_DURABLE_MAINTENANCE`, checkpoints should not flush dirty
buffers, write SLRUs, process fsync queues, recycle WAL for durability, or
perform post-checkpoint storage cleanup.

The implementation should keep only whatever minimal metadata updates are
needed for initdb/startup/shutdown in the current fake-WAL build. Core
`RM_XLOG_ID` records can remain conservative until measured and proven safe to
skip.

### VACUUM and Freeze

Main files:

- `src/backend/commands/vacuum.c`
- `src/backend/access/heap/vacuumlazy.c`
- `src/backend/access/heap/heapam.c`
- `src/include/commands/vacuum.h`
- `src/include/access/heapam.h`

Important paths:

- `vacuum_rel`
- `heap_vacuum_rel`
- lazy vacuum phase I/II/III
- `heap_page_prune_and_freeze`
- `heap_page_prune_opt`
- freeze cutoff computation
- failsafe checks
- `vac_update_relstats`
- `vac_update_datfrozenxid`

Under this mode, plain `VACUUM` should return successfully without scanning and
rewriting the heap/indexes. `VACUUM ANALYZE` can either run the `ANALYZE` half
or become a full no-op in the first implementation; choose the simpler path
that keeps validation passing and document it.

Unsupported first-pass forms should fail clearly:

- `VACUUM FULL`
- `VACUUM FREEZE`
- parallel VACUUM
- anti-wraparound VACUUM

If compatibility with tests that issue `VACUUM FREEZE` becomes important, make
it a successful no-op rather than running freeze machinery.

### Visibility Map and Free Space Map

Main files:

- `src/backend/access/heap/visibilitymap.c`
- `src/include/access/visibilitymap.h`
- `src/backend/storage/freespace/freespace.c`

Important paths:

- `visibilitymap_clear`
- `visibilitymap_pin`
- `visibilitymap_set`
- `visibilitymap_get_status`
- `visibilitymap_count`
- `visibilitymap_prepare_truncate`
- freespace recording during vacuum/pruning

Visibility-map bits are optimizations and durability aids in this workload. In
fast-fork mode:

- clearing VM bits can be a no-op
- setting VM bits can be a no-op
- pinning VM pages can be a no-op
- counting VM bits can return zero visible/frozen pages
- all-visible/all-frozen checks should return false unless a caller requires a
  stronger answer for correctness

Returning false is conservative: it may avoid index-only scan optimizations, but
it will not make invisible tuples visible.

Freespace-map updates can also be skipped. The heap can keep finding or
extending pages using existing non-FSM fallbacks. If this causes severe table
bloat in the benchmark, add a small in-memory free-space hint table as a
follow-up.

### Opportunistic Pruning

Main files:

- `src/backend/access/heap/pruneheap.c`
- `src/backend/access/heap/heapam.c`
- `src/include/access/heapam.h`

Heap scans call opportunistic pruning to clean pages and reduce future work.
That cleanup is useful in production but often wasted in rollback-heavy tests.

Under this mode:

- `heap_page_prune_opt` should return immediately.
- Scan paths should not attempt cleanup locks just to prune.
- Required pruning for correctness-sensitive paths may remain enabled.

Do not disable tuple visibility checks. Dead tuples can remain on pages as long
as MVCC still filters them correctly.

### Wraparound and Horizon Maintenance

Main files:

- `src/backend/access/transam/varsup.c`
- `src/backend/access/transam/xlog.c`
- `src/backend/postmaster/autovacuum.c`
- `src/backend/commands/vacuum.c`
- `src/include/access/transam.h`

Under this mode:

- anti-wraparound autovacuum should not be scheduled
- freeze/failsafe warnings should be disabled or downgraded
- `vac_update_datfrozenxid` should be a no-op
- CLOG/subtrans/multixact truncation driven by freeze horizons should be a no-op
- `relfrozenxid`, `relminmxid`, `datfrozenxid`, and `datminmxid` can remain at
  creation/default values

Transaction ID allocation and status lookup must remain correct for the
lifetime of the test cluster. This mode assumes the test cluster will not run
long enough to approach wraparound.

## Design

### Keep MVCC, Drop Maintenance

The core invariant is:

> If a query would be correct immediately after normal PostgreSQL DML, it must
> remain correct in the fast fork even if no maintenance ever runs.

That means the implementation must keep:

- transaction ID assignment
- CLOG / transaction-status lookups
- subtransaction state
- multixact state for tuple locks
- tuple xmin/xmax visibility checks
- command ID visibility
- rollback cleanup for created/dropped relation storage

It can skip:

- freezing old XIDs
- advancing frozen horizons
- removing dead tuples for space reuse
- index vacuum cleanup
- VM/FSM updates
- checkpoint flushing and fsyncing

### User-Facing Command Behavior

SQL commands should be predictable:

- `CHECKPOINT` should succeed as a no-op.
- Plain `VACUUM` should succeed as a no-op.
- `VACUUM ANALYZE` should either run only `ANALYZE` or succeed as a no-op.
- Unsupported maintenance forms should error with a message that names the
  fast-fork build mode.

This keeps application tests that call `VACUUM` or `CHECKPOINT` from failing
unless they explicitly depend on production maintenance effects.

### Catalog Stats

Catalog stats related to vacuum/freeze can become stale in this mode:

- `relpages`
- `reltuples`
- `relallvisible`
- `relallfrozen`
- `relfrozenxid`
- `relminmxid`

That is acceptable for the first version. If planner regressions show up in the
benchmark, add cheap local estimates rather than running real vacuum.

### Interaction With Other Fast-Fork Specs

This mode is most valuable when combined with:

- fake WAL
- no background jobs
- in-memory transaction-status SLRUs
- in-memory storage manager
- compile-time observability bypass

It should not depend on all of them, but the validation script can enable the
full fast-fork bundle as features land.

## Validation

The spec is satisfied when both validation paths pass on the current fork.

### Correctness Gate

Run the fast-fork validation script with durable maintenance disabled:

```sh
./test-fastfork.sh --wipe
```

After `test_no_durable_maintenance` is wired into the script, this should
configure the build with:

```sh
-Dtest_fake_wal=true -Dtest_no_bg_jobs=true -Dtest_no_durable_maintenance=true
```

Passing means:

- The script exits successfully.
- All selected compatible tests pass.
- MVCC, rollback, savepoints, tuple locking, DDL, indexes, parser, executor,
  resource-owner, and relation behavior remain correct.
- `CHECKPOINT` and plain `VACUUM` do not fail unexpectedly if exercised by the
  selected tests.
- Any skipped tests are explicitly incompatible with the fast-fork feature set
  or missing local test dependencies.

Additional high-value checks:

- Insert/update/delete rows, verify visibility before and after rollback.
- Run `VACUUM`, then verify query results are unchanged.
- Run `CHECKPOINT`, then verify query results are unchanged.
- Exercise index scans after many updates/deletes with no vacuum cleanup.
- Exercise savepoint rollback after updates/deletes with no pruning.

### Performance Gate

Run the repeatable pgbench comparison:

```sh
python3 bench/compare_pgbench.py \
  --rounds 5 \
  --transactions 200 \
  --rows 200
```

After the benchmark harness knows about `test_no_durable_maintenance`, the fork
build should include:

```sh
-Dtest_fake_wal=true -Dtest_no_bg_jobs=true -Dtest_no_durable_maintenance=true
```

Passing means:

- Baseline and fork runs complete successfully.
- Results are written under `bench/results/`.
- The summary records the fork speed relative to the cached baseline build.

## Implementation Checklist

- Add Meson and autoconf build options for `test_no_durable_maintenance`.
- Add `USE_TEST_NO_DURABLE_MAINTENANCE` to generated config headers.
- Make `CHECKPOINT` and `RequestCheckpoint` cheap no-ops in fast-fork mode.
- Make `CheckPointGuts` skip buffer writes, SLRU writes, fsync processing, and
  post-checkpoint cleanup.
- Make plain `VACUUM` succeed without heap/index scans.
- Decide whether `VACUUM ANALYZE` runs only `ANALYZE` or is a no-op.
- Make unsupported VACUUM variants fail clearly or become documented no-ops.
- Disable freeze cutoff computation, eager freeze, failsafe, and
  anti-wraparound scheduling.
- Make `vac_update_relstats` and `vac_update_datfrozenxid` no-ops or cheap
  metadata-only updates.
- Make VM set/clear/pin/count operations cheap and conservative.
- Skip FSM maintenance that is only used for space reuse.
- Make `heap_page_prune_opt` return immediately.
- Preserve required visibility checks and tuple-lock/multixact behavior.
- Teach `test-fastfork.sh` to configure
  `-Dtest_no_durable_maintenance=true`.
- Teach `bench/compare_pgbench.py` to configure the fork build with
  `-Dtest_no_durable_maintenance=true`.
- Run the validation and benchmark gates above.

## Risks

- Disabling pruning can increase table and index bloat inside a long-running
  test process. This is acceptable for short transactions but should be
  measured with large suites.
- Returning conservative false values from the visibility map may reduce
  index-only scan use, trading one optimization for simpler correctness.
- Some regression tests assert exact VACUUM, freeze, visibility-map, or stats
  behavior. Those should stay outside the fast-fork validation set unless the
  feature is deliberately supported.
- If a test suite runs millions or billions of transactions in one cluster,
  wraparound assumptions can break. Add a clear guardrail long before danger.
- Checkpoint code also handles some lifecycle metadata. Keep startup/shutdown
  shape conservative until other fast-fork storage changes make it unnecessary.
