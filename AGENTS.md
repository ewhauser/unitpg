# Agent Notes

## pgbench comparison harness

The pgbench comparison harness lives under `benches/`.

Run the default comparison with:

```sh
make -C benches pgbench-compare
```

For a quick smoke run, use:

```sh
make -C benches pgbench-compare SCALE=1 TRANSACTIONS=1 RUNS=1
```

The harness builds and temp-installs two release-mode Meson variants under
`benches/.build/pgbench/`:

- `normal`: `-Dfastpg=false`
- `fastpg`: `-Dfastpg=true`

It then starts a fresh local Postgres cluster for each run, executes pgbench,
and writes JSON plus Markdown summaries under `benches/results/pgbench/<timestamp>/`.

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
```

`INIT_STEPS=dtg` is intentional. pgbench built-in transaction scripts derive
`:scale` from generated rows, so `dt` alone makes normal Postgres see scale `0`
and abort before the workload starts.

To run pgbench's fuller default initialization and TPC-B-like script:

```sh
make -C benches pgbench-compare-strict
```

To run the same pgbench driver against the Rust single-process server instead
of the transitional Postgres tableam wrapper:

```sh
make -C benches pgbench-compare-rust-server
```

That target defaults to a release Rust build and links the Rust server against
the local Postgres raw parser through `fastpg-pgcore`. The harness builds the
normal Meson variant first, then passes that build directory to Cargo as
`FASTPG_POSTGRES_BUILD_DIR`.

To run the strict pgbench shape against the Rust server through the full
PostgreSQL parser/analyzer/rewriter/planner/executor facade:

```sh
make -C benches pgbench-compare-rust-server-strict
```

That target uses `RUST_PGCORE=full`, builds the Rust server in release mode, and
links it against a `-Dfastpg=true` Postgres backend build so the guarded virtual
catalog hooks are available.

To temporarily run a quicker debug build:

```sh
make -C benches pgbench-compare-rust-server RUST_BUILD_PROFILE=debug
```

To compare the Rust server without the in-process Postgres parser link:

```sh
make -C benches pgbench-compare-rust-server RUST_PGCORE=off
```

To use the full PostgreSQL execution path with the non-strict Rust-server
target:

```sh
make -C benches pgbench-compare-rust-server RUST_PGCORE=full
```

To capture a CPU flamegraph of the Rust server during the pgbench transaction
run:

```sh
make -C benches pgbench-profile-rust-server
```

The profiling target builds `fastpg-server` in release mode, starts the normal
Postgres variant only to reuse its `pgbench` client, then records the Rust
server while the fastpg pgbench run executes. Flamegraphs are written to:

```text
benches/results/pgbench/<timestamp>/fastpg/run-<n>/profile/fastpg-server-flamegraph.svg
```

To run and open the flamegraph immediately:

```sh
make -C benches pgbench-profile-rust-server-open
```

To open the newest saved flamegraph:

```sh
make -C benches pgbench-open-latest-flamegraph
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
`INIT_STEPS=dtg`, while stricter pgbench paths remain implementation targets.

Useful validation commands after harness edits:

```sh
python3 -m py_compile benches/pgbench_compare.py
python3 -m py_compile benches/open_latest_profile.py
cargo test -p fastpg-storage
meson test -C benches/.build/pgbench/fastpg --suite fastpg_parser_probe --print-errorlogs
```

## SQL regression comparison harness

The curated SQL regression harness also lives under `benches/`.

Run the strict Rust-server SQL regression comparison with:

```sh
make -C benches regression-compare-rust-server
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
make -C benches regression-compare-rust-server-inventory
```

That inventory target defaults to `benches/regression/inventory/`. It is for
known compatibility gaps and exploratory upstream-regression reductions, not for
the blocking correctness gate. Current inventory probes cover deeper
`pg_attribute`/`pg_index` catalog scans, joins, and grouped aggregation.

To run the current Rust-server validation bundle:

```sh
make -C benches validate-rust-server
```
