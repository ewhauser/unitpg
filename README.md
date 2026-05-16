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
- startup/recovery benchmarks, startup fast paths, seed-only restarts, and
  no-data-directory startup

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
| Startup fresh worker, setup+start | 0.164341 s | 0.053804 s | 3.054x | `bench/results/no-data-directory-startup-final2`, baseline copy mode vs fast-fork no-data-dir mode, 10 rounds |
| Startup fresh worker, runtime setup only | 0.137665 s | 0.014868 s | 9.259x | Same run as above; baseline copies PGDATA, fast fork copies only a runtime skeleton |
| Startup reuse, postmaster ready | 0.036009 s | 0.031566 s | 1.141x | `bench/results/seed-only-startup-final2`, 10 rounds, direct first-query polling |
| Startup reuse, first query | 0.008751 s | 0.006006 s | 1.457x | Same run as above |
| Startup copy, postmaster ready | 0.026903 s | 0.025781 s | 1.044x | `bench/results/seed-only-startup-copy-final2`, 10 rounds |
| Startup copy, first query | 0.006554 s | 0.005596 s | 1.171x | Same run as above |

The runtime fixture-restore comparison measured stock PostgreSQL replaying the
rollback-heavy setup workload against the fast fork restoring a captured
fixture snapshot before each test body. Plain rollback remains slower in the
current prototype; use fixture snapshots for the intended fast path. Startup is
now measured by direct polling for the first successful query, so the
postmaster-ready rows include client retry timing. Seed-only startup treats the
data directory as an immutable seed image and proves that clean or immediate
restarts discard runtime-created tables while resetting OID state.
No-data-directory startup keeps a read-only seed backing image plus a mutable
in-memory overlay; migrations can still mutate seed-backed catalogs and
relations, but fresh workers avoid copying relation storage into their runtime
directory.

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

Each archive contains the server runtime needed to initialize, start, and stop a
fast-fork cluster: `initdb`, `pg_ctl`, `postgres`, optional server-adjacent
helpers such as `postmaster` and `pg_isready`, server runtime libraries, and
`share` runtime data files. The release archives intentionally omit source code,
benchmark outputs, headers, PGXS files, documentation, and client/backup
utilities such as `psql`, `pg_dump`, and `pg_basebackup`.

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
- Do not commit generated benchmark results from `bench/results/` or build
  artifacts from `bench/.build/`.
- When a change is meant to improve performance, update the performance table
  above with the benchmark command/result or explain why the table is unchanged.
- If `git status` reports an fsmonitor IPC warning in this worktree, use:

```sh
git -c core.fsmonitor=false status --short
```
