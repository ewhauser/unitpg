# Conservative Fast Startup

## Summary

Add a test-only startup mode that skips crash recovery and WAL redo while
preserving the existing seed data directory and normal postmaster lifecycle.
This is the first recovery-related cut: keep enough PostgreSQL startup shape to
remain mergeable, but avoid the work that only matters when a previous durable
cluster state must be recovered.

The fast fork is an in-memory database. Runtime writes live in memory and do not
need to survive restart. On startup, the server should treat the data directory
as a valid seed catalog image, initialize transient transaction/storage state,
and start accepting connections without scanning or replaying WAL.

## Goals

- Avoid WAL redo and crash recovery during fast-fork postmaster startup.
- Skip recovery-signal, standby, timeline, archive recovery, and PITR state
  machines.
- Keep `initdb` and bootstrap behavior conservative.
- Keep seed catalog files readable by `memsmgr`.
- Preserve normal parser/planner/executor behavior after startup.
- Fail clearly for recovery/standby modes that are outside the fast fork.
- Measure improvement with the startup benchmark.

## Non-Goals

- Crash recovery.
- Hot standby.
- Archive recovery.
- Timeline history management.
- Logical decoding startup state.
- Preserving data written by a previous postmaster run.
- Removing the data directory or seed catalog files.

## Build Flag

Add a new opt-in build option:

- Meson: `-Dtest_no_recovery_startup=true`
- Autoconf: `--enable-test-no-recovery-startup`
- C define: `USE_TEST_NO_RECOVERY_STARTUP`

This option should require or imply:

- `test_fake_wal=true`
- `test_mem_smgr=true`

It composes naturally with:

- `test_no_wal_assembly=true`
- `test_mem_slru=true`
- `test_no_durable_maintenance=true`

## Current Startup Boundary

Main files:

- `src/backend/postmaster/startup.c`
- `src/backend/access/transam/xlog.c`
- `src/backend/access/transam/xlogrecovery.c`
- `src/backend/access/transam/xlogutils.c`
- `src/backend/access/transam/timeline.c`
- `src/backend/access/transam/xlogarchive.c`
- `src/backend/access/transam/slru.c`
- `src/backend/access/transam/clog.c`
- `src/backend/access/transam/subtrans.c`
- `src/backend/access/transam/multixact.c`
- `src/include/access/xlog.h`
- `src/include/access/xlogrecovery.h`

Important paths:

- `StartupProcessMain`
- `StartupXLOG`
- control-file read/update paths
- checkpoint record discovery
- redo pointer validation
- WAL segment directory scans
- timeline selection and history reads
- recovery/standby signal handling
- SLRU startup initialization

## Design

### Fast Startup Path

In fast-fork builds, add a branch early in startup:

```c
#ifdef USE_TEST_NO_RECOVERY_STARTUP
if (FastForkNoRecoveryStartupActive())
{
	FastForkStartupWithoutRecovery();
	return;
}
#endif
```

`FastForkStartupWithoutRecovery()` should:

1. Read enough control-file metadata to locate the seed cluster and initialize
   transaction counters.
2. Reject `recovery.signal`, `standby.signal`, restore commands, recovery
   targets, and archive recovery configuration.
3. Initialize shared recovery state as "not in recovery".
4. Initialize `TransamVariables` from safe seed/control values.
5. Initialize in-memory SLRU state.
6. Initialize fake WAL/LSN counters.
7. Mark startup complete so the postmaster can accept connections.

It should not:

- scan `pg_wal`
- find or read checkpoint records
- replay WAL records
- read timeline history files
- enter archive recovery
- update min recovery point
- perform end-of-recovery checkpoint work
- create durable recovery metadata

### Control File Handling

Keep control-file reads initially. They are cheap and preserve compatibility
with existing `initdb` output.

Avoid control-file writes that only describe durable recovery state:

- "in production"
- checkpoint location
- previous checkpoint
- min recovery point
- timeline changes
- shutdown checkpoint metadata

If some control-file write is still required for postmaster lifecycle
assertions, keep it local and document why. The next seed-only spec should
remove more of this dependency.

### SLRU and Transaction State

Startup still needs a coherent transaction horizon for MVCC within the running
cluster. In fast-fork mode:

- initialize `pg_xact`, `pg_subtrans`, multixact, and commit-ts memory state
  from seed values
- do not scan disk SLRU directories for recovery
- do not read or replay SLRU WAL records
- do not flush SLRUs at startup or shutdown for durability

This relies on the in-memory SLRU spec.

### Unsupported Recovery Inputs

Fail clearly if any of these are present:

- `recovery.signal`
- `standby.signal`
- `restore_command`
- recovery target settings
- standby mode/hot standby expectations
- configured replication slots that require durable restart state

The error should explain that the fast fork does not support recovery startup.

## Correctness Requirements

- `initdb` still works.
- A fresh postmaster starts and accepts connections.
- `SELECT 1` works after startup.
- Existing fast-fork validation passes.
- Runtime-created data does not need to survive restart.
- Unsupported recovery modes fail clearly rather than silently ignoring user
  configuration.

## Validation

Run:

```sh
./test-fastfork.sh core --no-reconfigure
```

Run the startup benchmark:

```sh
python3 bench/compare_startup.py \
  --rounds 10 \
  --reuse-builds \
  --output-dir bench/results/conservative-fast-startup
```

Run the existing pgbench comparison to ensure runtime behavior did not regress:

```sh
python3 bench/compare_pgbench.py \
  --rounds 3 \
  --transactions 200 \
  --rows 200 \
  --reuse-builds \
  --output-dir bench/results/conservative-fast-startup-pgbench
```

The implementation is successful if:

- validation passes
- median startup time improves versus the previous fast fork
- pgbench remains faster than stock baseline

## Risks

- `StartupXLOG()` initializes more than redo. Audit each skipped side effect and
  recreate only the pieces needed by active backends.
- Control-file state may be assumed by shutdown or SQL-visible functions.
  Preserve harmless reads and stub writes carefully.
- Some tests may expect recovery settings to be accepted. Those tests should be
  excluded from the fast-fork validation set.
- Skipping redo means any dirty seed data directory from a previous run is
  invalid input. The seed-only startup spec should make that contract explicit.
