# PostgreSQL Test-Speed Benchmark

This directory contains a repeatable `pgbench` workload for comparing stock
PostgreSQL against a test-only fork.

The workload in `unit-test-rollback.pgbench` is shaped like an application unit
test: create schema objects, insert data, create indexes, run indexed joins and
mutations, use a savepoint, and roll the whole transaction back.
It deliberately uses ordinary permanent tables inside the transaction rather
than temp tables, because the target application test pattern does not rely on
PostgreSQL temp-table storage.

The optional `unit-test-snapshot.pgbench` workload uses the fast-fork fixture
snapshot API. Its first warmup transaction creates the schema/data fixture and
captures `pg_fastfork_snapshot('bench_fixture')`; later transactions restore
that fixture before running the same query/mutation body. This workload is for
the fast-fork build and currently expects `--clients 1`. Because the first
snapshot implementation stores snapshots in the backend session, the runner
does not run a separate warmup pgbench process for this workload; use enough
transactions that the one-time fixture setup is amortized.

## Baseline

Build and install PostgreSQL, then point the runner at that install's `bin`
directory:

```sh
python3 bench/run_pgbench.py \
  --bin /path/to/postgres/install/bin \
  --label baseline-fast-settings \
  --output bench/results/baseline-fast-settings.json
```

Run the same command against the forked build with a different label/output:

```sh
python3 bench/run_pgbench.py \
  --bin /path/to/fork/install/bin \
  --label fork \
  --output bench/results/fork.json
```

To measure the fixture snapshot path against a fast-fork build:

```sh
python3 bench/run_pgbench.py \
  --bin /path/to/fork/install/bin \
  --label fork-snapshot \
  --workload snapshot \
  --output bench/results/fork-snapshot.json
```

The runner initializes a disposable cluster and applies these fast test
settings: `fsync=off`, `synchronous_commit=off`, `full_page_writes=off`,
`wal_level=minimal`, `archive_mode=off`, `max_wal_senders=0`,
`max_replication_slots=0`, `autovacuum=off`, `track_counts=off`, and `jit=off`.

Use `--transactions`, `--clients`, `--jobs`, and `--rows` to scale the run.
Keep the same values for both builds when comparing results.

## Repeatable Comparison

To build a baseline server and a fake-WAL server from this checkout, run the
same workload against both, and write a comparison under `bench/results/`:

```sh
python3 bench/compare_pgbench.py \
  --rounds 5 \
  --transactions 200 \
  --rows 200
```

To compare stock Postgres doing DDL/data setup in every transaction against the
fast-fork snapshot/restore path, use:

```sh
python3 bench/compare_pgbench.py \
  --fakewal-workload snapshot \
  --rounds 5 \
  --transactions 200 \
  --rows 200
```

The comparison runner uses Meson by default and installs both builds under
`bench/.build/`. It writes `summary.md`, `summary.json`, per-run JSON files,
and build logs into a timestamped result directory.

## Startup Benchmark

To measure startup cost for one installed PostgreSQL build:

```sh
python3 bench/run_startup.py \
  --bin /path/to/postgres/install/bin \
  --label baseline-startup \
  --rounds 10 \
  --output bench/results/baseline-startup.json
```

The startup runner records `initdb` separately from repeated
start/first-query/stop cycles. By default it reuses the same initialized data
directory for each round. To model harnesses that copy a fresh seed cluster per
worker, use `--mode copy`.

To compare the cached baseline install against the fast-fork install:

```sh
python3 bench/compare_startup.py \
  --rounds 10 \
  --output-dir bench/results/startup-compare
```

Like the pgbench comparison, the startup comparison can reuse existing installs:

```sh
python3 bench/compare_startup.py \
  --baseline-bin bench/.build/installs/baseline-meson/bin \
  --fakewal-bin bench/.build/fastfork-validation/tmp_install/usr/local/pgsql/bin \
  --rounds 10 \
  --output-dir bench/results/startup-compare
```

The baseline install is cached by default. If `bench/.build/installs/baseline-*`
already has the needed PostgreSQL binaries, later runs reuse it without
rebuilding. Use `--rebuild-baseline` only when you intentionally want to refresh
the stock PostgreSQL build. The fake-WAL build also enables the
no-background-jobs build flag by default, which keeps async I/O workers enabled
but disables maintenance jobs like checkpointer/bgwriter/walwriter/autovacuum.
The fake-WAL build is rebuilt each run by default. Use `--keep-bg-jobs` to
isolate fake WAL by itself, `--reuse-builds` for faster iteration, or
`--build-system configure` to use the autoconf build instead.

If Meson is not installed locally:

```sh
brew install meson
```

For a fork build with fake WAL enabled, configure with either:

```sh
./configure --enable-test-fake-wal --enable-test-no-bg-jobs
```

or, with Meson:

```sh
meson setup build -Dtest_fake_wal=true -Dtest_no_bg_jobs=true
```
