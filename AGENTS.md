# Agent Notes

## pgbench comparison harness

The pgbench comparison harness lives under `benches/`.

Run the default Rust-server smoke comparison with:

```sh
make -C benches pgbench
```

For a quick smoke run, use:

```sh
make -C benches pgbench SCALE=1 TRANSACTIONS=1 RUNS=1
```

The harness builds the normal Postgres client/server tree under
`benches/.build/pgbench/`, uses it as the pgbench client and normal baseline,
then runs the fastpg side through the Rust single-process server. It writes JSON
plus Markdown summaries under `benches/results/pgbench/<timestamp>/`.

The default workload is:

```text
BUILTIN=simple-update
INIT_STEPS=dtg
SCALE=1
TRANSACTIONS=20
CLIENTS=1
JOBS=1
RUNS=3
PROTOCOL=simple
MESON_BUILDTYPE=release
RUST_BUILD_PROFILE=release
RUST_PGCORE=full
```

`INIT_STEPS=dtg` is intentional. pgbench built-in transaction scripts derive
`:scale` from generated rows, so `dt` alone makes normal Postgres see scale `0`
and abort before the workload starts. This default does not create pgbench
primary-key indexes because the `p` init step is missing. Treat `make -C
benches pgbench` as an unindexed smoke path, not as UPDATE performance evidence.

To run the same simple-update workload with primary-key indexes:

```sh
make -C benches pgbench-simple-indexed
```

To run pgbench's fuller default initialization and TPC-B-like script against
the full PostgreSQL parser/analyzer/rewriter/planner/executor facade:

```sh
make -C benches pgbench-tpcb
```

The `pgbench`, `pgbench-simple-indexed`, and `pgbench-tpcb` targets use
`RUST_PGCORE=full`, build the Rust server in release mode, and link it against
a `-Dfastpg=true` Postgres backend build so the guarded virtual catalog hooks
are available. The default fastpg build includes the internal IPC guard.

To temporarily run a quicker debug build:

```sh
make -C benches pgbench RUST_BUILD_PROFILE=debug
```

To capture a CPU flamegraph of the Rust server during the pgbench transaction
run:

```sh
make -C benches profile
```

The profiling target builds `fastpg-server` in release mode, starts the normal
Postgres variant only to reuse its `pgbench` client, then records the Rust
server while the fastpg pgbench run executes. Flamegraphs are written to:

```text
benches/results/pgbench/<timestamp>/fastpg/run-<n>/profile/fastpg-server-flamegraph.svg
```

To run and open the flamegraph immediately:

```sh
make -C benches profile-open
```

To open the newest saved flamegraph:

```sh
make -C benches profile-open-latest
```

To run the storage engine Criterion benchmarks:

```sh
make -C benches bench
```

For a quick Criterion smoke run:

```sh
make -C benches bench-smoke
```

Useful profiling knobs:

```text
PROFILE_TRANSACTIONS=500
PROFILE_RUNS=1
PROFILE_TOOL=flamegraph
PROFILE_PHASE=run
PROFILE_OPEN=0
PROFILE_WARMUP_SECONDS=1.0
```

The harness treats normal Postgres failures as harness failures. It treats
fastpg failures as useful implementation targets and reports the failing phase:
`setup`, `initdb`, `start`, `pgbench-init`, `pgbench-run`, `profile`, or
`stop`.

Current expected state: normal Postgres should pass the quick smoke run. The
Rust-server target should also pass the simple-update smoke run with
`INIT_STEPS=dtg`. Use `pgbench-simple-indexed` or `pgbench-tpcb` for indexed
UPDATE performance comparisons.

Useful validation commands after harness edits:

```sh
python3 -m py_compile benches/pgbench_compare.py
python3 -m py_compile benches/open_latest_profile.py
cargo test -p fastpg-storage
make -C benches pgbench-simple-indexed
make -C benches pgbench-tpcb
make -C benches regression
```

## SQL regression comparison harness

The curated SQL regression harness also lives under `benches/`.

Run the strict SQL regression comparison with:

```sh
make -C benches regression
```

The harness builds/reuses the same normal Postgres client install as the
pgbench harness, starts normal Postgres first, runs every SQL file in
`benches/regression/core/`, then starts the Rust single-process server through
the full PostgreSQL execution path and compares each case's stdout.

Current core coverage is broader than pgbench and is intentionally small enough
to be a correctness gate:

- DDL, INSERT, UPDATE, DELETE, TRUNCATE, and `count(*)`
- `COPY FROM STDIN`
- BEGIN/COMMIT/ROLLBACK behavior
- primary key catalog visibility and primary-key lookups
- plain `count(*)` over Rust-backed storage

Results are written under:

```text
benches/results/regression/<timestamp>/
```

For compatibility inventory work where fastpg failures should be recorded
without making the command fail:

```sh
make -C benches regression REGRESSION_SUITE=inventory REGRESSION_ALLOW_FAILURES=1
```

The inventory suite lives under `benches/regression/inventory/`. It is for
known compatibility gaps, not for the blocking correctness gate. Current
inventory probes cover deeper `pg_attribute`/`pg_index` catalog scans, joins,
and grouped aggregation.

To run the current validation bundle:

```sh
make -C benches validate
```
