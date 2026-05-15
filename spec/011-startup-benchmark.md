# Startup Benchmark

## Summary

Add a repeatable benchmark for measuring PostgreSQL startup cost separately
from query/runtime cost. The fast fork is an in-memory, disposable database, so
startup and recovery-related work should become a first-class performance
target. Before cutting recovery paths, we need a stable harness that tells us
which change actually makes postmaster startup faster.

This benchmark should measure:

- `initdb` time, separately from postmaster startup
- repeated start/ready/first-query/stop cycles using the same seed data
  directory
- optional fresh-copy startup, where each round starts from a copied seed
  cluster
- stock PostgreSQL with fast settings versus the current fast-fork build

The output should look like the existing pgbench harness: JSON per run, a
summary JSON, and a short Markdown table under `bench/results/`.

## Goals

- Measure startup cost before implementing recovery/startup shortcuts.
- Separate `initdb`, postmaster start, readiness, first query, and stop time.
- Reuse cached baseline and fast-fork installs when available.
- Use the same fast test settings as the pgbench benchmark.
- Make startup results comparable across branches and commits.
- Keep the benchmark outside PostgreSQL core test logic.

## Non-Goals

- Do not change PostgreSQL startup behavior in this spec.
- Do not replace the pgbench workload benchmark.
- Do not require network listeners; use local Unix sockets where possible.
- Do not require crash/recovery correctness tests.
- Do not benchmark replication, hot standby, PITR, archive recovery, or logical
  decoding startup.

## Proposed Files

Add:

- `bench/run_startup.py`
- `bench/compare_startup.py`

Update:

- `bench/README.md`

The runner should follow the style of:

- `bench/run_pgbench.py`
- `bench/compare_pgbench.py`

## Benchmark Shape

### Single Install Runner

`bench/run_startup.py` should accept:

```text
--bin PATH
--label LABEL
--output PATH
--workdir PATH
--rounds N
--mode reuse|copy
--keep-workdir
--config KEY=VALUE
```

Default mode should be `reuse`.

In `reuse` mode:

1. Create a disposable work directory.
2. Run `initdb` once with `--no-sync`.
3. Append fast test settings to `postgresql.conf`.
4. For each round:
   - start the postmaster with `pg_ctl -w start`
   - record elapsed time until `pg_ctl` reports ready
   - run `SELECT 1` through `psql`
   - record first-query latency
   - stop with `pg_ctl -m fast -w stop`

In `copy` mode:

1. Create a seed data directory with `initdb`.
2. For each round:
   - copy the seed data directory to a fresh round directory
   - start the copied cluster
   - run the first query
   - stop and delete the copied cluster unless `--keep-workdir` is set

`copy` mode costs more, but it models test harnesses that create a fresh data
directory per worker or suite. It should record copy time separately from
startup time.

### Comparison Runner

`bench/compare_startup.py` should accept cached install paths or build variants
from the current checkout:

```text
--baseline-bin PATH
--fakewal-bin PATH
--build-root PATH
--output-dir PATH
--rounds N
--mode reuse|copy
--reuse-builds
--rebuild-baseline
```

It should reuse the same build flags and install-cache behavior as
`bench/compare_pgbench.py`.

## Fast Settings

Use the same defaults as `bench/run_pgbench.py`:

```text
listen_addresses = ''
fsync = off
synchronous_commit = off
full_page_writes = off
wal_level = minimal
archive_mode = off
max_wal_senders = 0
max_replication_slots = 0
autovacuum = off
track_counts = off
jit = off
```

The runner should still allow extra `--config` lines so we can test future
startup-specific knobs without editing the script.

## Metrics

Each run JSON should record:

- `label`
- `started_at`
- `bin_dir`
- `postgres_version`
- `settings`
- `mode`
- `rounds`
- `initdb_seconds`
- per-round:
  - `round`
  - `copy_seconds`, if mode is `copy`
  - `pg_ctl_start_seconds`
  - `first_query_seconds`
  - `pg_ctl_stop_seconds`
  - `postgres_log_path`
- summary:
  - min/median/mean/max for start time
  - min/median/mean/max for first-query time
  - min/median/mean/max for stop time
  - total elapsed time

The comparison summary should show:

| Variant | Mode | Rounds | Median start | Mean start | Median first query | Median stop |
| --- | --- | ---: | ---: | ---: | ---: | ---: |

and speedup ratios for fast fork versus baseline.

## Validation

A successful benchmark implementation must pass:

```sh
python3 bench/run_startup.py \
  --bin bench/.build/fastfork-validation/tmp_install/usr/local/pgsql/bin \
  --label fastfork-startup-smoke \
  --rounds 2 \
  --output bench/results/startup-smoke.json
```

and:

```sh
python3 bench/compare_startup.py \
  --baseline-bin bench/.build/installs/baseline-meson/bin \
  --fakewal-bin bench/.build/fastfork-validation/tmp_install/usr/local/pgsql/bin \
  --rounds 5 \
  --output-dir bench/results/startup-compare
```

The smoke benchmark should create a cluster, start it, prove `SELECT 1` works,
stop it, and write JSON output.

## Interpretation

Use this benchmark before and after each recovery/startup spec:

1. Startup benchmark only: establishes baseline numbers.
2. Conservative fast startup: should reduce `pg_ctl_start_seconds`.
3. Seed-only startup: should reduce `pg_ctl_start_seconds` and maybe stop time.
4. No-data-directory startup: should reduce or remove `initdb_seconds` and copy
   time for harnesses that create fresh clusters.

If a change improves pgbench but worsens startup, record both. The goal is not
one universal score; it is to understand which changes help which test harness
shape.

## Risks

- Startup timing is noisy on developer machines. Use medians over multiple
  rounds and keep raw per-round JSON.
- macOS dynamic-library fixups for `tmp_install` must be handled just like the
  existing validation script.
- `pg_ctl -w start` includes readiness polling; that is the useful user-facing
  measurement, but keep logs so we can later split internal phases if needed.
- Copy-mode benchmark can accidentally measure filesystem copy more than
  PostgreSQL startup. Record copy time separately.
