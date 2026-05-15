# Early WAL Assembly Bypass

## Summary

Add a test-only mode that avoids constructing ordinary WAL records at all. The
current fake-WAL mode makes many relation paths skip WAL and makes
`XLogInsert()` return a fake LSN for non-core records, but any caller that still
reaches `XLogInsert()` has usually already paid for `XLogBeginInsert()`,
`XLogRegisterData()`, `XLogRegisterBuffer()`, rmgr-specific record structs, and
buffer registration work.

This spec moves the fake-WAL decision earlier: before record assembly starts.
The fast fork should keep enough fake LSN and transaction bookkeeping for the
running test cluster to behave correctly, while skipping payload construction,
buffer registration, full-page-image checks, and WAL insertion locks for
ordinary records.

## Goals

- Avoid WAL record assembly for non-durable test builds.
- Preserve MVCC, transaction commit/abort semantics, rollback, catalog
  invalidation behavior, and page LSN expectations inside the running cluster.
- Keep core startup/checkpoint/control-file WAL records working when they are
  still needed for initdb and normal server startup.
- Keep the change behind an opt-in build flag so upstream merges stay
  manageable.
- Make unsupported durability, recovery, replication, and logical-decoding
  paths fail clearly or remain outside the fast-fork validation set.
- Validate correctness with the fast-fork validation script and measure speed
  with the repeatable pgbench benchmark.

## Non-Goals

- Crash recovery.
- WAL archiving, streaming replication, logical decoding, or PITR.
- Producing WAL that can be replayed by PostgreSQL.
- Preserving data across postmaster restarts.
- Removing transaction status, MVCC visibility, or rollback state.

## Build Flag

Add a new build option:

- Meson: `-Dtest_no_wal_assembly=true`
- Autoconf: `--enable-test-no-wal-assembly`
- C define: `USE_TEST_NO_WAL_ASSEMBLY`

The option should default to `false`. When disabled, upstream WAL behavior must
remain unchanged.

`test_no_wal_assembly` should require or imply `test_fake_wal`. It relies on
fake LSN generation and the existing test-only WAL predicates that make
replication, archiving, logical WAL, standby info, hint-bit WAL, and
relation-level WAL unnecessary.

Example fast-fork build:

```sh
meson setup build-fastfork \
  -Dtest_fake_wal=true \
  -Dtest_no_bg_jobs=true \
  -Dtest_no_wal_assembly=true
```

## Current Boundary

Important existing files:

- `src/include/access/xlog.h`
- `src/include/access/xloginsert.h`
- `src/backend/access/transam/xloginsert.c`
- `src/backend/access/transam/xact.c`
- `src/include/utils/rel.h`

The current `USE_TEST_FAKE_WAL` behavior already makes these cheap:

- `XLogIsNeeded()`
- `XLogHintBitIsNeeded()`
- `XLogStandbyInfoActive()`
- `XLogLogicalInfoActive()`
- `RelationNeedsWAL(relation)`

It also makes `XLogInsert()` return a fake LSN for non-`RM_XLOG_ID` records.
However, records that are not guarded by `RelationNeedsWAL()` still assemble
their payload before calling `XLogInsert()`.

Common remaining sources include:

- transaction commit and abort records in `xact.c`
- invalidation records
- CLOG, multixact, and commit-ts truncate/create records
- relation-map and storage-manager metadata records
- database, tablespace, sequence, and rewrite records not already gated by
  `RelationNeedsWAL()`

## Design

### Explicit Early Predicate

Add a cheap predicate that callers can use before `XLogBeginInsert()`:

```c
bool XLogRecordAssemblyRequired(RmgrId rmid, uint8 info);
```

Under normal builds, it always returns `true`.

Under `USE_TEST_NO_WAL_ASSEMBLY`, it returns:

- `true` for core `RM_XLOG_ID` records that are still required for initdb,
  checkpoint shape, and startup expectations.
- `false` for ordinary records that are meaningless without durability,
  recovery, replication, or logical decoding.

If some non-`RM_XLOG_ID` record is discovered to be required for startup shape
or local correctness, add it explicitly rather than falling back broadly.

### Fake LSN Helper

Add a helper for callers that skip assembly but still need an LSN:

```c
XLogRecPtr XLogSkipInsert(RmgrId rmid, uint8 info);
```

Under `USE_TEST_NO_WAL_ASSEMBLY`, this should do the same local bookkeeping that
the current fake path in `XLogInsert()` does for ordinary records:

- Generate a monotonic fake LSN.
- Mark the current transaction ID as logged when needed.
- Mark pending subtransaction top-XID logging when needed.
- Update `ProcLastRecPtr`.
- Update `XactLastRecEnd`.
- Return the fake LSN to callers that need to set page LSNs.

Under normal builds, `XLogSkipInsert()` should not be used. It can be declared
only inside `#ifdef USE_TEST_NO_WAL_ASSEMBLY` or assert/panic if called without
the build flag.

### Call-Site Shape

Convert remaining unavoidable WAL sites from:

```c
XLogBeginInsert();
XLogRegisterData(&xlrec, sizeof(xlrec));
recptr = XLogInsert(RM_FOO_ID, XLOG_FOO_BAR);
```

to:

```c
if (!XLogRecordAssemblyRequired(RM_FOO_ID, XLOG_FOO_BAR))
	recptr = XLogSkipInsert(RM_FOO_ID, XLOG_FOO_BAR);
else
{
	XLogBeginInsert();
	XLogRegisterData(&xlrec, sizeof(xlrec));
	recptr = XLogInsert(RM_FOO_ID, XLOG_FOO_BAR);
}
```

Prefer small local helper functions for noisy rmgr sites so the production path
stays readable.

### Transaction Commit and Abort

Transaction records are the most important target. In the fast fork, transaction
commit and abort still need local ordering and status updates, but the WAL
payload does not need to exist.

The early-bypass path for `xact.c` must preserve:

- CLOG status updates.
- Subtransaction status updates.
- Commit timestamp updates when enabled.
- ProcArray removal ordering.
- Relcache and catcache invalidation delivery.
- Dropped-relation cleanup.
- Fake LSN assignment for pages or callers that expect one.

It must skip:

- building `xl_xact_commit` and `xl_xact_abort` payloads
- collecting dropped relfilenodes into WAL payloads
- registering invalidation WAL payloads
- origin/twophase/logical metadata payload assembly when those features are
  outside the fast-fork mode

If a commit/abort path needs the same data for non-WAL cleanup, keep that data
structure but stop copying it into WAL-specific record buffers.

### Relation and Index WAL Sites

Most heap and index modification paths already skip WAL when
`RelationNeedsWAL()` is false. Do not add broad churn there first.

The first pass should audit for sites that call `XLogBeginInsert()` without a
cheap `RelationNeedsWAL()` or `XLogIsNeeded()` guard. Those are the best early
targets.

### SLRU and Metadata WAL Sites

The bypass should cover transaction-status and metadata records that still emit
WAL despite fake durability, especially:

- `clog.c` truncate records
- `multixact.c` create/truncate records
- `commit_ts.c` truncate records
- `catalog/storage.c` create/drop records
- `relmapper.c` updates
- `inval.c` invalidation records
- database and tablespace create/drop records

For each target, preserve the local state mutation and skip only the WAL
payload assembly.

### Generic Guardrails

`XLogBeginInsert()` can become a no-op under `USE_TEST_NO_WAL_ASSEMBLY`, but
that alone is not enough. The performance win comes from avoiding all the work
before and around the `XLogRegister*()` calls, especially buffer registration
and rmgr-specific payload construction.

The implementation should:

- Keep existing assertions in normal builds.
- Avoid hiding accidental WAL assembly in fast-fork builds.
- Optionally add counters for skipped WAL assemblies by rmgr so benchmark runs
  can show whether the bypass is active.

## Validation

The spec is satisfied when both validation paths pass on the current fork.

### Correctness Gate

Run the fast-fork validation script with early WAL assembly bypass enabled:

```sh
./test-fastfork.sh --wipe
```

After `test_no_wal_assembly` is wired into the script, this should configure the
build with:

```sh
-Dtest_fake_wal=true -Dtest_no_bg_jobs=true -Dtest_no_wal_assembly=true
```

Passing means:

- The script exits successfully.
- All selected compatible tests pass.
- MVCC, rollback, savepoint, DDL, index, resource-owner, and relation tests in
  the selected set continue to pass.
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

After the benchmark harness knows about `test_no_wal_assembly`, the fork build
should include:

```sh
-Dtest_fake_wal=true -Dtest_no_bg_jobs=true -Dtest_no_wal_assembly=true
```

Passing means:

- Baseline and fork runs complete successfully.
- Results are written under `bench/results/`.
- The summary records the fork speed relative to the cached baseline build.
- Optional skipped-WAL counters, if implemented, show that ordinary records are
  bypassed before assembly.

## Implementation Checklist

- Add Meson and autoconf build options for `test_no_wal_assembly`.
- Add `USE_TEST_NO_WAL_ASSEMBLY` to generated config headers.
- Make the build option require or imply `USE_TEST_FAKE_WAL`.
- Add `XLogRecordAssemblyRequired(rmid, info)`.
- Add `XLogSkipInsert(rmid, info)` using the existing fake-LSN bookkeeping from
  `XLogInsert()`.
- Convert high-value non-relation WAL sites to call the early predicate before
  building WAL payloads.
- Start with transaction commit/abort, CLOG, multixact, commit-ts, invalidation,
  relation-map, storage metadata, database, and tablespace records.
- Keep `RM_XLOG_ID` records on the normal path unless a specific record is
  proven safe to skip.
- Teach `test-fastfork.sh` to configure `-Dtest_no_wal_assembly=true`.
- Teach `bench/compare_pgbench.py` to configure the fork build with
  `-Dtest_no_wal_assembly=true`.
- Run the validation and benchmark gates above.

## Risks

- Some WAL payload construction may also prepare data needed for local cleanup.
  Those local side effects must be preserved.
- Skipping transaction commit/abort assembly incorrectly could break CLOG
  ordering, invalidation delivery, or relation cleanup.
- Returning fake LSNs that do not advance monotonically can break page-LSN and
  flush assumptions even without durability.
- A broad no-op in `XLogRegister*()` would reduce work but leave too much
  rmgr-specific payload construction in place; the main win requires call-site
  guards.
- Core `RM_XLOG_ID` records may still be needed for initdb and startup shape, so
  they should remain conservative until measured and proven unnecessary.
