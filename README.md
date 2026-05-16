# Ephemeral Postgres Fast Fork

This repository is an experimental PostgreSQL fork for running application unit
tests quickly. It is intentionally optimized for disposable test clusters, not
for production databases.

The core assumption is simple: test runners need PostgreSQL semantics for DDL,
queries, indexes, constraints, MVCC, and rollback, but they do not need
durability, crash recovery, replication, backup, archive recovery, or long-lived
maintenance behavior. This fork explores how much work PostgreSQL can skip when
that contract is explicit.

## What This Fork Is For

- Fast local and CI unit-test databases.
- Test workflows that create schema and fixture data, run tests in
  transactions, and roll back.
- Benchmarking startup and runtime costs of removing durability-oriented
  PostgreSQL subsystems.
- Experiments that stay behind build flags so upstream merges remain manageable.

## What This Fork Is Not For

- Production use.
- Durable data.
- Crash recovery or WAL replay.
- Streaming replication, logical decoding, PITR, archive recovery, base backup,
  or `pg_upgrade`.
- Security/permission/event-trigger behavior tests unless the relevant
  fast-fork mode is disabled.

Use upstream PostgreSQL for real databases. Copyright and license information
from PostgreSQL is in [COPYRIGHT](COPYRIGHT). Upstream documentation is at
<https://www.postgresql.org/docs/devel/>.

## Current Fast-Fork Areas

Specs live in [spec/](spec/). The current work is organized around:

- in-memory storage manager
- in-memory transaction-status SLRUs
- early WAL assembly bypass
- observability/statistics bypass
- faster memory-context choices
- fixture snapshot/restore and catalog fast paths
- no durable maintenance
- direct buffer access for ephemeral storage
- trusted DDL shortcuts
- rollback-only test epochs for cheap per-test transaction discard
- startup/recovery benchmarks, startup fast paths, seed-only restarts, and
  no-data-directory startup
- macOS named POSIX semaphore and mmap-only shared-memory experiments to avoid
  SysV IPC usage in sandboxed test launchers

The agent workflow and validation loop are documented in [AGENTS.md](AGENTS.md).

## Current Performance Snapshot

These are local benchmark snapshots from the current fast-fork prototype, not
portable performance guarantees. Keep this section updated when committing new
performance work.

| Area | Baseline | Fast fork | Result | Notes |
| --- | ---: | ---: | ---: | --- |
| Runtime fixture restore | 244.532 TPS | 588.734 TPS | 2.408x TPS | `bench/results/seed-only-startup-pgbench-final`, 3 rounds, 200 transactions, 200 rows |
| Runtime fixture restore latency | 4.089 ms | 1.699 ms | 0.416x latency | Same run as above |
| Runtime plain rollback | 245.491 TPS | 129.768 TPS | 0.529x TPS | `bench/results/conservative-fast-startup-rollback-final`, 3 rounds, permanent-table rollback workload |
| Runtime epoch rollback | 244.376 TPS | 823.818 TPS | 3.371x TPS | `bench/results/epoch-shared-parallel-final2`, 5 rounds, 200 transactions, 200 rows |
| Runtime epoch rollback latency | 4.092 ms | 1.214 ms | 0.297x latency | Same run as above |
| Runtime epoch rollback vs previous main | 671.359 TPS | 829.016 TPS | 1.235x TPS | `bench/results/epoch-shared-parallel-vs-main2`, 5 rounds, previous main fast fork at `de6bce7c894` vs current shared overlay |
| Runtime epoch rollback latency vs previous main | 1.490 ms | 1.206 ms | 0.809x latency | Same run as above |
| Runtime named epoch vs pre-epoch fixture restore | 642.921 TPS | 733.227 TPS | 1.140x TPS | `bench/results/pre-epoch-snapshot-vs-current-named`, 3 rounds, pre-epoch fast fork at `a9e75e77242` running fixture snapshot restore vs current named epoch |
| Runtime named epoch latency vs pre-epoch fixture restore | 1.555 ms | 1.364 ms | 0.877x latency | Same run as above |
| Runtime epoch DDL rollback | 252.352 TPS | 298.446 TPS | 1.183x TPS | `bench/results/epoch-ddl-phase2-final`, 3 rounds, 200 transactions, 200 rows |
| Runtime epoch DDL rollback latency | 3.963 ms | 3.351 ms | 0.846x latency | Same run as above |
| Startup fresh worker, setup+start | 0.164341 s | 0.053804 s | 3.054x | `bench/results/no-data-directory-startup-final2`, baseline copy mode vs fast-fork no-data-dir mode, 10 rounds |
| Startup fresh worker, runtime setup only | 0.137665 s | 0.014868 s | 9.259x | Same run as above; baseline copies PGDATA, fast fork copies only a runtime skeleton |
| Startup reuse, postmaster ready | 0.036009 s | 0.031566 s | 1.141x | `bench/results/seed-only-startup-final2`, 10 rounds, direct first-query polling |
| Startup reuse, first query | 0.008751 s | 0.006006 s | 1.457x | Same run as above |
| Startup copy, postmaster ready | 0.026903 s | 0.025781 s | 1.044x | `bench/results/seed-only-startup-copy-final2`, 10 rounds |
| Startup copy, first query | 0.006554 s | 0.005596 s | 1.171x | Same run as above |

The runtime fixture-restore comparison measured stock PostgreSQL replaying the
rollback-heavy setup workload against the fast fork restoring a captured
fixture snapshot before each test body. Plain rollback without epoch enrollment
remains slower in the current prototype; fixture snapshots and rollback-only
epochs are the intended fast paths. The epoch rollback comparison measures stock
PostgreSQL permanent-table rollback against the fast fork running
`pg_fastfork_epoch_begin()` after restoring a fixture in the pgbench session.
The shared epoch overlay comparison keeps base fixture pages in place and
discards per-epoch storage overlays on rollback instead of restoring a copied
snapshot image. The named epoch comparison measures the pool-friendly
`pg_fastfork_epoch_start()` / `pg_fastfork_epoch_join()` API against the
pre-epoch fixture snapshot fast path. The epoch DDL rollback comparison measures
stock PostgreSQL replaying the per-transaction rollback workload against the
fast fork creating and indexing a small table inside each rollback-only epoch.
Startup is now measured by direct polling for the first successful query, so the
postmaster-ready rows include client retry timing. Seed-only startup treats the
data directory as an immutable seed image and proves that clean or immediate
restarts discard runtime-created tables while resetting OID state.
No-data-directory startup keeps a read-only seed backing image plus a mutable
in-memory overlay; migrations can still mutate seed-backed catalogs and
relations, but fresh workers avoid copying relation storage into their runtime
directory.

## Parallel Test Isolation With Named Epochs

Named epochs are the recommended API for large test harnesses that run many
parallel tests against one migrated database. Fixture snapshot/restore is still
useful for serialized tests, but `pg_fastfork_restore('fixture')` is a
database-wide reset. Do not call it before each parallel test in a shared
database, because it changes the shared base state underneath other tests.

The named epoch model keeps the migrated fixture as the shared base image and
gives each test a disposable overlay:

```sql
-- once per worker/package, after migrations and fixture setup
SELECT pg_fastfork_snapshot('fixture');

-- before one test starts
SELECT pg_fastfork_epoch_start('test-id', 'fixture');

-- on every backend/session that may run SQL for that test
SELECT pg_fastfork_epoch_join('test-id');

-- application code can run ordinary DML and transaction blocks here
BEGIN;
INSERT INTO accounts ...;
COMMIT;

-- before the backend/session returns to a general pool
SELECT pg_fastfork_epoch_leave();

-- after all test connections have left
SELECT pg_fastfork_epoch_finish('test-id');
```

`pg_fastfork_epoch_finish()` discards the named overlay. Base fixture pages are
left unchanged, so independent tests can run in separate named epochs at the
same time. Different named epochs can even mutate the same logical rows or
insert the same primary keys without colliding, because the buffer and storage
overlay keys include the epoch identity.

Practical integration notes:

- Keep a coordinator connection open for fixture management. The current
  snapshot registry is backend-local, so the connection that runs
  `pg_fastfork_snapshot('fixture')` should also run
  `pg_fastfork_epoch_start(test_id, 'fixture')`.
- Pooled worker connections do not need the snapshot. They only need
  `pg_fastfork_epoch_join(test_id)` before running test SQL and
  `pg_fastfork_epoch_leave()` before being returned to a general-purpose pool.
- Joining a named epoch does not open a transaction. Application code may use
  ordinary `BEGIN`, `COMMIT`, savepoints, and `ROLLBACK`; committed DML remains
  visible inside the named epoch until `pg_fastfork_epoch_finish()` discards it.
- If a joined backend exits, the server detaches it from the epoch, but explicit
  `pg_fastfork_epoch_leave()` is preferred so harness failures are easier to
  diagnose.
- DDL inside session-bound named epochs is not supported yet. Run migrations and
  schema setup before `pg_fastfork_snapshot('fixture')`. Use the older
  transaction-bound `BEGIN; SELECT pg_fastfork_epoch_begin(); ... ROLLBACK;`
  path for DDL rollback experiments.
- `pg_fastfork_restore('fixture')` should only run while the database is
  quiesced and no named epochs are active.

For benchmark coverage, use:

```sh
python3 bench/compare_pgbench.py \
  --fakewal-workload epoch-named \
  --rounds 3 \
  --transactions 200 \
  --rows 200 \
  --reuse-builds \
  --output-dir bench/results/runtime-named-epoch-compare
```

## Build And Validate

The main fast-fork validation entrypoint is:

```sh
./test-fastfork.sh core
```

For a fast rebuild while iterating:

```sh
./test-fastfork.sh quick --setup-only --no-reconfigure
./test-fastfork.sh core --no-reconfigure
```

The validation build is kept under:

```text
bench/.build/fastfork-validation
```

The installed binaries are under:

```text
bench/.build/fastfork-validation/tmp_install/usr/local/pgsql/bin
```

## Precompiled Server Releases

Merges to `main` or this repository's current `master` default branch publish a
GitHub release containing minimal fast-fork server archives for:

- `linux-x86_64`
- `linux-aarch64`
- `macos-aarch64`

Each archive contains the server runtime needed to initialize, start, stop, and
connect to a fast-fork cluster: `initdb`, `pg_ctl`, `postgres`, `psql`, optional
server-adjacent helpers such as `postmaster` and `pg_isready`, server runtime
libraries, pgvector extension files, and `share` runtime data files. The release
archives intentionally omit source code, benchmark outputs, headers, PGXS files,
documentation, and backup or auxiliary client utilities such as `pg_dump` and
`pg_basebackup`.

## Measure Runtime Performance

The pgbench harness compares stock PostgreSQL with the fast fork using a
unit-test-shaped workload.

```sh
python3 bench/compare_pgbench.py \
  --rounds 3 \
  --transactions 200 \
  --rows 200 \
  --reuse-builds \
  --output-dir bench/results/runtime-compare
```

To measure the fixture snapshot/restore path:

```sh
python3 bench/compare_pgbench.py \
  --fakewal-workload snapshot \
  --rounds 3 \
  --transactions 200 \
  --rows 200 \
  --reuse-builds \
  --output-dir bench/results/runtime-snapshot-compare
```

To measure the pool-friendly named epoch path:

```sh
python3 bench/compare_pgbench.py \
  --fakewal-workload epoch-named \
  --rounds 3 \
  --transactions 200 \
  --rows 200 \
  --reuse-builds \
  --output-dir bench/results/runtime-named-epoch-compare
```

## Measure Startup Performance

Startup benchmarks measure `initdb`, repeated postmaster startup, first query,
and shutdown separately.

```sh
python3 bench/compare_startup.py \
  --rounds 10 \
  --reuse-builds \
  --output-dir bench/results/startup-compare
```

Use copy mode to model test harnesses that copy a fresh seed cluster per worker:

```sh
python3 bench/compare_startup.py \
  --rounds 10 \
  --mode copy \
  --reuse-builds \
  --output-dir bench/results/startup-copy-compare
```

Use no-data-dir mode to model fresh workers that keep only a compatibility
runtime skeleton and read seed relation pages through the memory storage
manager:

```sh
python3 bench/compare_startup.py \
  --rounds 10 \
  --mode no-data-dir \
  --reuse-builds \
  --output-dir bench/results/startup-no-data-dir-compare
```

## Development Notes

- Keep fast-fork changes behind explicit build flags.
- Keep stock PostgreSQL behavior unchanged when those flags are disabled.
- Validate correctness before trusting benchmark numbers.
- On macOS, the fast-fork validation build uses named POSIX semaphores instead
  of SysV semaphores and mmap-only shared memory without the SysV interlock.
  This avoids SysV IPC usage in sandboxed test launchers and avoids leaving
  `ipcs` resources behind after killed test postmasters.
- Do not commit generated benchmark results from `bench/results/` or build
  artifacts from `bench/.build/`.
- When a change is meant to improve performance, update the performance table
  above with the benchmark command/result or explain why the table is unchanged.
- If `git status` reports an fsmonitor IPC warning in this worktree, use:

```sh
git -c core.fsmonitor=false status --short
```
