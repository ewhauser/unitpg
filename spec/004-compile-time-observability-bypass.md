# Compile-Time Observability Bypass

## Summary

Add a test-only build mode that compiles out PostgreSQL's hot-path statistics
and observability bookkeeping. Runtime settings like `track_counts=off`,
`track_io_timing=off`, and `track_functions=none` help, but many call sites
still branch, touch backend status memory, increment counters, update wait
events, or maintain progress state.

For the fast fork, these features are not part of the core contract. Unit tests
need DDL, DML, queries, MVCC, rollback, locks, indexes, and errors. They do not
need accurate `pg_stat_*` counters, wait-event names, progress views, function
call statistics, or I/O timing.

## Goals

- Remove pgstat counter updates from relation, database, WAL, SLRU, lock, I/O,
  function, bgwriter, checkpointer, and backend hot paths.
- Remove wait-event writes from hot wait paths.
- Remove command progress updates.
- Remove function-call statistics overhead.
- Make timing helpers return cheap zero/default values.
- Preserve enough backend status behavior for ordinary connections, errors, and
  cancellation to keep working.
- Keep the change behind an opt-in build flag so upstream merges remain
  manageable.
- Validate correctness with the fast-fork validation script and measure speed
  with the repeatable pgbench benchmark.

## Non-Goals

- Accurate `pg_stat_*` views.
- Accurate `pg_stat_activity.wait_event` or progress reporting.
- Accurate function, relation, lock, WAL, SLRU, I/O, bgwriter, or checkpointer
  counters.
- Accurate timing information for query instrumentation or statistics views.
- Preserving observability behavior for production use.
- Removing correctness-critical transaction, lock, buffer, or MVCC state.

## Build Flag

Add a new build option:

- Meson: `-Dtest_no_observability=true`
- Autoconf: `--enable-test-no-observability`
- C define: `USE_TEST_NO_OBSERVABILITY`

The option should default to `false`. When disabled, upstream statistics,
wait-event, progress, and instrumentation behavior must remain unchanged.

Example fast-fork build:

```sh
meson setup build-fastfork \
  -Dtest_fake_wal=true \
  -Dtest_no_bg_jobs=true \
  -Dtest_no_observability=true
```

## Primary Targets

### pgstat Counters

Main files:

- `src/include/pgstat.h`
- `src/backend/utils/activity/pgstat.c`
- `src/backend/utils/activity/pgstat_relation.c`
- `src/backend/utils/activity/pgstat_database.c`
- `src/backend/utils/activity/pgstat_io.c`
- `src/backend/utils/activity/pgstat_wal.c`
- `src/backend/utils/activity/pgstat_slru.c`
- `src/backend/utils/activity/pgstat_lock.c`
- `src/backend/utils/activity/pgstat_function.c`
- `src/backend/utils/activity/pgstat_backend.c`
- `src/backend/utils/activity/pgstat_bgwriter.c`
- `src/backend/utils/activity/pgstat_checkpointer.c`
- `src/backend/utils/activity/pgstat_xact.c`

Hot-path macros and functions should compile to no-ops under
`USE_TEST_NO_OBSERVABILITY`, including:

- `pgstat_count_heap_scan`
- `pgstat_count_heap_getnext`
- `pgstat_count_heap_fetch`
- `pgstat_count_index_scan`
- `pgstat_count_index_tuples`
- `pgstat_count_buffer_read`
- `pgstat_count_buffer_hit`
- `pgstat_count_heap_insert`
- `pgstat_count_heap_update`
- `pgstat_count_heap_delete`
- `pgstat_count_truncate`
- `pgstat_update_heap_dead_tuples`
- `pgstat_count_io_op`
- `pgstat_count_io_op_time`
- `pgstat_count_backend_io_op`
- `pgstat_count_backend_io_op_time`
- `pgstat_count_slru_*`
- `pgstat_count_lock_*`
- `pgstat_report_wal`
- `pgstat_report_stat`

The first implementation should prefer header-level no-op macros/static inline
functions where possible, so call sites do not need widespread churn.

### Wait Events

Main files:

- `src/include/utils/wait_event.h`
- `src/backend/utils/activity/wait_event.c`
- `src/include/storage/proc.h`

Under `USE_TEST_NO_OBSERVABILITY`:

- `pgstat_report_wait_start(wait_event_info)` should compile to no code.
- `pgstat_report_wait_end()` should compile to no code.
- Backend `wait_event_info` may remain present for struct compatibility, but it
  should not be written on wait start/end.
- Wait-event lookup functions can keep returning names for compatibility, but
  hot paths must not update per-backend wait state.

### Progress Reporting

Main files:

- `src/include/utils/backend_progress.h`
- `src/backend/utils/activity/backend_progress.c`

Under `USE_TEST_NO_OBSERVABILITY`:

- `pgstat_progress_start_command`
- `pgstat_progress_update_param`
- `pgstat_progress_update_multi_param`
- `pgstat_progress_end_command`

should compile to no-ops.

Progress views can return no rows or default values.

### Function Statistics

Main files:

- `src/backend/utils/fmgr/fmgr.c`
- `src/backend/utils/activity/pgstat_function.c`
- `src/include/fmgr.h`
- `src/include/pgstat.h`

Under `USE_TEST_NO_OBSERVABILITY`:

- `pgstat_init_function_usage` should not read timing state.
- `pgstat_end_function_usage` should not update counters.
- `track_functions` should behave as effectively disabled.
- Function execution must still call hooks and preserve normal error cleanup.

Do not remove the function-manager hook path; extensions may still rely on it
inside tests.

### Timing Helpers

Timing helpers used only for statistics should become cheap defaults:

- `pgstat_prepare_io_time(...)`
- buffer read/write timing counts
- connection active/idle timing counts
- WAL I/O timing counts

The implementation should avoid `INSTR_TIME_SET_CURRENT()` calls for statistics
timing in this mode.

### Executor Instrumentation

Executor instrumentation for explicit `EXPLAIN ANALYZE` is not a primary target
for the first patch. It is usually opt-in per query and removing it would break
user-visible SQL behavior.

If later profiling shows meaningful overhead from executor instrumentation even
when not requested, add a separate flag. Do not conflate that with pgstat and
wait-event removal.

## Compatibility Behavior

SQL-visible statistics functions and views should remain safe:

- `pg_stat_*` views may return zeros, null wait events, default timestamps, or
  empty rows depending on what is easiest and least surprising.
- `pg_stat_activity` should still expose live backend rows if that is cheap, but
  wait-event and progress fields can be null/default.
- Reset functions such as `pg_stat_reset()` should succeed as no-ops.
- Tests that assert exact statistics counters should be outside the fast-fork
  validation set.

The fast fork should not crash or raise unexpected errors simply because a test
queries a stats view.

## Design

### Header-Level No-Ops First

Use `#ifdef USE_TEST_NO_OBSERVABILITY` in public headers to erase the most
frequent call sites:

- counter macros in `pgstat.h`
- wait-event inline functions in `wait_event.h`
- progress prototypes or static inline replacements in `backend_progress.h`
- function stats helpers in `pgstat.h`

This keeps the implementation merge-friendly by avoiding large mechanical edits
across executor, access methods, buffer manager, storage, and command code.

### Keep Minimal Initialization

Some backend status and pgstat initialization may still be tied to shared-memory
layout, SQL views, or extension-visible APIs. The first version should keep
minimal initialization if it avoids destabilizing startup.

Acceptable behavior:

- Allocate existing shared-memory structs but stop updating hot-path counters.
- Skip stats flush work.
- Skip stats persistence or restoration.
- Keep fetch functions returning empty/default entries.

More aggressive removal of shared-memory allocation can be a follow-up after the
hot-path no-ops pass is validated.

### Transactional Stats

Transactional relation stats currently participate in commit/abort cleanup.
Under `USE_TEST_NO_OBSERVABILITY`, this bookkeeping should be skipped unless it
is required for non-stat correctness.

In particular:

- Per-transaction relation stats stacks should not be created.
- Commit/abort stats flushes should be no-ops.
- Two-phase stats paths can be no-ops because prepared transactions are outside
  the fast-fork feature set.

Do not remove transaction resource-owner or relation cleanup behavior that is
not purely statistical.

### Compile-Time GUC Defaults

Runtime GUCs should default to disabled values in this build:

- `track_counts = off`
- `track_io_timing = off`
- `track_wal_io_timing = off`
- `track_functions = none`

If users set them to enabled values, either ignore the setting with a warning or
accept it while keeping compile-time no-op behavior. Prefer an explicit warning
so test behavior is not mysterious.

## Validation

The spec is satisfied when both validation paths pass on the current fork.

### Correctness Gate

Run the fast-fork validation script with observability bypass enabled:

```sh
./test-fastfork.sh --wipe
```

After `test_no_observability` is wired into the script, this should configure
the build with:

```sh
-Dtest_fake_wal=true -Dtest_no_bg_jobs=true -Dtest_no_observability=true
```

Passing means:

- The script exits successfully.
- All selected compatible tests pass.
- MVCC, rollback, savepoint, DDL, index, parser, resource-owner, and relation
  behavior remain correct.
- Any skipped tests are explicitly incompatible with the fast-fork feature set
  or missing local test dependencies.

### Performance Gate

Run the repeatable pgbench comparison:

```sh
python3 bench/compare_pgbench.py \
  --rounds 5 \
  --transactions 200 \
  --rows 200
```

After the benchmark harness knows about `test_no_observability`, the fork build
should include:

```sh
-Dtest_fake_wal=true -Dtest_no_bg_jobs=true -Dtest_no_observability=true
```

Passing means:

- Baseline and fork runs complete successfully.
- Results are written under `bench/results/`.
- The summary records the fork speed relative to the cached baseline build.

## Implementation Checklist

- Add Meson and autoconf build options for `test_no_observability`.
- Add `USE_TEST_NO_OBSERVABILITY` to generated config headers.
- Compile hot-path pgstat count macros/functions to no-ops.
- Compile wait-event start/end reporting to no-ops.
- Compile progress reporting functions to no-ops.
- Compile function usage stats to no-ops while preserving function-manager
  hooks.
- Make statistics timing helpers avoid clock reads.
- Make stats flush, persistence, restore, and reset paths cheap no-ops where
  safe.
- Keep SQL-visible stats views safe by returning empty/default data.
- Set or force relevant stats GUCs to disabled values.
- Teach `test-fastfork.sh` to configure `-Dtest_no_observability=true`.
- Teach `bench/compare_pgbench.py` to configure the fork build with
  `-Dtest_no_observability=true`.
- Run the validation and benchmark gates above.

## Risks

- Some code may use pgstat hooks for cleanup-like behavior, especially
  transactional relation stats. Audit before deleting state updates.
- Extensions or tests may query `pg_stat_activity`; keep the view safe even if
  values are less informative.
- Removing wait-event writes should not remove actual waits, latch behavior, or
  interrupt handling.
- Removing function stats must not bypass fmgr hooks or error cleanup.
- Exact statistics regression tests will fail by design and should stay outside
  the fast-fork validation set.
