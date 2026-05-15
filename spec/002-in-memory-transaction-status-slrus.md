# In-Memory Transaction-Status SLRUs

## Summary

Move PostgreSQL's transaction-status SLRUs into memory for the fast fork. This
removes disk-backed `pg_xact`, `pg_subtrans`, `pg_multixact`, and commit
timestamp files from unit-test workloads while keeping the transaction-status
state required for MVCC visibility, row locking, subtransactions, and rollback.

This is not a request to remove transaction status. The fast fork still needs
to answer questions like "did this transaction commit?", "what is this
subtransaction's parent?", and "what members belong to this multixact?" for all
live and recently completed transactions in the disposable test cluster.

## Goals

- Keep MVCC visibility, rollback, savepoints, row locks, and multixact behavior
  correct while the test cluster is running.
- Avoid creating, reading, writing, fsyncing, checkpointing, or truncating
  transaction-status SLRU files on disk.
- Keep the implementation opt-in behind a build flag so upstream merges remain
  manageable.
- Reuse PostgreSQL's existing shared-memory SLRU synchronization model where
  practical.
- Make checkpoint, fsync, and disk-truncation paths cheap no-ops for the
  in-memory SLRUs.
- Validate correctness with the fast-fork validation script and measure speed
  with the repeatable pgbench benchmark.

## Non-Goals

- Durability across postmaster restarts.
- Crash recovery of transaction status.
- Replication, hot standby, base backup, or logical decoding support.
- Preserving transaction status after the disposable test cluster exits.
- Removing CLOG, subtransaction, multixact, or commit timestamp semantics from
  the running system.

## Build Flag

Add a new build option:

- Meson: `-Dtest_mem_slru=true`
- Autoconf: `--enable-test-mem-slru`
- C define: `USE_TEST_MEM_SLRU`

The option should default to `false`. When disabled, all SLRU behavior should
match upstream PostgreSQL.

The fast-fork build can combine this with the existing flags:

```sh
meson setup build-fastfork \
  -Dtest_fake_wal=true \
  -Dtest_no_bg_jobs=true \
  -Dtest_mem_slru=true
```

If the in-memory storage manager is also enabled later, the same build can add:

```sh
  -Dtest_mem_smgr=true
```

## Target SLRUs

The first implementation should cover only transaction-status SLRUs:

- `pg_xact` / CLOG: `src/backend/access/transam/clog.c`
- `pg_subtrans`: `src/backend/access/transam/subtrans.c`
- `pg_multixact/offsets` and `pg_multixact/members`:
  `src/backend/access/transam/multixact.c`
- Commit timestamps: `src/backend/access/transam/commit_ts.c`

The generic implementation point is:

- `src/backend/access/transam/slru.c`
- `src/include/access/slru.h`

Other SLRU users should remain disk-backed unless explicitly added to this
fast-fork mode.

## Design

### Preferred Boundary

Add an in-memory mode to the generic SLRU layer rather than hand-rolling four
separate storage implementations. A small field on `SlruOpts` is enough:

```c
bool in_memory;
```

Each target SLRU passes `in_memory = true` when `USE_TEST_MEM_SLRU` is defined.
That keeps the per-SLRU code responsible for status encoding and page-number
math, while `slru.c` owns the disk-vs-memory backing behavior.

### Shared Memory Is Authoritative

Normal SLRU already keeps pages in shared memory, but treats that memory as a
cache over files. In this mode, shared memory becomes the authoritative store.

That means:

- Pages are visible across all backends in the postmaster.
- There is no process-local transaction-status state.
- Page contents live until explicitly truncated or until the postmaster exits.
- Dirty/clean state no longer means "needs disk write"; it only means the page
  has been modified in memory.

### Page Allocation

The implementation needs enough in-memory pages to avoid evicting required
transaction-status data to disk. Acceptable first-version approaches:

- Increase the configured SLRU buffer counts for target SLRUs under
  `USE_TEST_MEM_SLRU`.
- Add an in-memory page table in `slru.c` that maps SLRU page numbers to shared
  memory chunks.
- Fail clearly with `ERROR` if the configured in-memory SLRU capacity is
  exhausted.

Do not silently fall back to disk when in-memory mode runs out of space.

### Reads

`SimpleLruReadPage` and `SimpleLruReadPage_ReadOnly` should first look for the
page in the in-memory authoritative map.

For pages that have not been allocated yet:

- For write-capable callers that are extending/initializing status, allocate or
  zero the page as today's SLRU initialization paths expect.
- For read-only callers, preserve the existing observable semantics for the
  target SLRU. If upstream would report an inaccessible or too-old status, keep
  that behavior rather than inventing a committed/aborted answer.

The important invariant is that missing in-memory status must not make a tuple
incorrectly visible.

### Writes

`SimpleLruWritePage` and `SimpleLruWriteAll` should not open, write, sync, or
close SLRU segment files for in-memory SLRUs.

For in-memory SLRUs:

- Mark the page valid in shared memory.
- Preserve any status bookkeeping required by callers.
- Treat checkpoint writes as complete without I/O.
- Do not register sync requests.

### Truncation

`SimpleLruTruncate` should become metadata cleanup for in-memory SLRUs:

- Drop or mark reusable pages older than the cutoff.
- Do not unlink files.
- Preserve wraparound/cutoff checks that protect callers from using status that
  is too old.
- Keep the caller-facing behavior of CLOG, subtrans, multixact, and commit-ts
  truncation intact.

### Physical-Page Checks

`SimpleLruDoesPhysicalPageExist` should use the in-memory page map for target
SLRUs. It should not probe the filesystem in in-memory mode.

### Checkpoint and Fsync Paths

These paths should be no-ops for in-memory SLRUs:

- CLOG checkpoint writes.
- Subtrans checkpoint writes.
- Multixact checkpoint writes.
- Commit-ts checkpoint writes.
- SLRU fsync and file-tag sync handlers.
- SLRU segment unlink/removal.

The code should still update any required in-memory limits or wraparound
metadata, but should not perform disk I/O.

## Per-SLRU Requirements

### pg_xact / CLOG

Keep the existing transaction status encoding:

- in progress
- committed
- aborted
- subcommitted

Commit and abort paths must continue to update CLOG before transactions leave
the proc array in the order required by MVCC. Subtransaction commit handling
must remain correct.

The in-memory implementation may ignore WAL flush LSNs for durability, but it
must not break any in-memory ordering assumptions in transaction commit.

### pg_subtrans

Preserve parent lookup for subtransactions and savepoints. The fast fork's
benchmark explicitly exercises transaction rollback patterns, so subtransaction
status cannot become a stub.

Because upstream already treats `pg_subtrans` as not valid across crashes, it is
a good early target for the in-memory SLRU path.

### pg_multixact

Preserve row-locking behavior that depends on multixact offsets and members.
The implementation must keep both SLRUs consistent:

- offsets map a `MultiXactId` to a member offset
- members store the transaction IDs and lock modes

Truncation must keep the same cutoff invariants so old multixacts are not read
after their status has been discarded.

### Commit Timestamps

If commit timestamps are disabled, keep the current cheap behavior. If enabled,
store commit timestamp data in memory and keep lookup behavior correct during
the lifetime of the test cluster.

Commit-ts checkpoint/truncate paths should update in-memory metadata only.

## Configuration

Add a test-only memory limit so failures are predictable:

- Suggested GUC: `test_mem_slru_size`
- Default: enough for the validation script and benchmark workload.
- Failure mode: `ERROR` explaining that in-memory SLRU capacity is exhausted.

If adding a GUC is too much for the first patch, use a compile-time default and
document the limit in the implementation.

## Validation

The spec is satisfied when both validation paths pass on the current fork.

### Correctness Gate

Run the fast-fork validation script with in-memory transaction-status SLRUs
enabled:

```sh
./test-fastfork.sh --wipe
```

After `test_mem_slru` is wired into the script, this should configure the build
with:

```sh
-Dtest_fake_wal=true -Dtest_no_bg_jobs=true -Dtest_mem_slru=true
```

Passing means:

- The script exits successfully.
- All selected compatible tests pass.
- MVCC, rollback, savepoint, index, parser, resource-owner, and relation tests
  in the selected set continue to pass.
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

After the benchmark harness knows about `test_mem_slru`, the fork build should
include:

```sh
-Dtest_fake_wal=true -Dtest_no_bg_jobs=true -Dtest_mem_slru=true
```

Passing means:

- Baseline and fork runs complete successfully.
- Results are written under `bench/results/`.
- The summary records the fork speed relative to the cached baseline build.

## Implementation Checklist

- Add Meson and autoconf build options for `test_mem_slru`.
- Add `USE_TEST_MEM_SLRU` to generated config headers.
- Add an in-memory-mode flag to `SlruOpts`.
- Enable in-memory mode for CLOG, subtrans, multixact offsets, multixact
  members, and commit-ts under `USE_TEST_MEM_SLRU`.
- Make `SimpleLruReadPage`, `SimpleLruReadPage_ReadOnly`,
  `SimpleLruWritePage`, `SimpleLruWriteAll`, `SimpleLruTruncate`, and
  `SimpleLruDoesPhysicalPageExist` honor in-memory mode.
- Disable SLRU file creation, reads, writes, fsyncs, sync registrations, and
  unlinks for in-memory SLRUs.
- Preserve CLOG status transitions and commit ordering.
- Preserve subtransaction parent lookup.
- Preserve multixact offset/member consistency.
- Preserve commit timestamp lookup when enabled.
- Teach `test-fastfork.sh` to configure `-Dtest_mem_slru=true`.
- Teach `bench/compare_pgbench.py` to configure the fork build with
  `-Dtest_mem_slru=true`.
- Run the validation and benchmark gates above.

## Risks

- Treating SLRU memory as a cache instead of authoritative storage would
  reintroduce disk I/O or lose status under pressure.
- Returning default values for missing CLOG pages can silently corrupt MVCC
  visibility. Missing pages must fail safely.
- Multixact has two coordinated SLRUs; truncating one without the other can
  break row-lock lookup.
- Some checkpoint code may assume SLRU writes contribute to checkpoint progress
  counters. The fast fork should keep counters harmless while skipping I/O.
- Capacity limits need to be explicit so large unit-test suites fail with a
  clear message instead of corrupting transaction status.
