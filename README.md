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
- startup/recovery benchmarks and startup fast paths

The agent workflow and validation loop are documented in [AGENTS.md](AGENTS.md).

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

## Development Notes

- Keep fast-fork changes behind explicit build flags.
- Keep stock PostgreSQL behavior unchanged when those flags are disabled.
- Validate correctness before trusting benchmark numbers.
- Do not commit generated benchmark results from `bench/results/` or build
  artifacts from `bench/.build/`.
- If `git status` reports an fsmonitor IPC warning in this worktree, use:

```sh
git -c core.fsmonitor=false status --short
```
