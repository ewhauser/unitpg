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

The harness builds and temp-installs two Meson variants under
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

The harness treats normal Postgres failures as harness failures. It treats
fastpg failures as useful implementation targets and reports the failing phase:
`setup`, `initdb`, `start`, `pgbench-init`, `pgbench-run`, or `stop`.

Current expected state: normal Postgres should pass the quick smoke run, while
fastpg is expected to fail during pgbench initialization until the in-memory
storage path can load the full pgbench fixture. The Rust-server target is
expected to fail earlier, at the first unsupported pgbench setup query, until
the Rust executor supports enough DDL/COPY/INSERT behavior for pgbench setup.

Useful validation commands after harness edits:

```sh
python3 -m py_compile benches/pgbench_compare.py
cargo test -p fastpg-storage
meson test -C benches/.build/pgbench/fastpg --suite fastpg_parser_probe --print-errorlogs
```
