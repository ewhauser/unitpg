# Fast-Fork Agent Guide

This repository is a PostgreSQL fork optimized for disposable unit-test
clusters. The fast fork intentionally does not preserve durability, crash
recovery, replication, backup, or long-lived maintenance behavior unless a spec
explicitly says otherwise.

## Core Rules

- Keep all fast-fork behavior behind build flags.
- Preserve stock PostgreSQL behavior when fast-fork flags are disabled.
- Prefer narrow, mergeable cuts at existing boundaries such as `smgr`, SLRU,
  WAL insertion, buffer management, startup, and catalog helpers.
- Do not optimize for recovery, replication, logical decoding, backup,
  `pg_upgrade`, autovacuum, checksums, or background-worker suites.
- Do preserve parser/planner/executor behavior, MVCC visibility, rollback,
  relcache/syscache correctness, ordinary DDL/query behavior, and constraints
  for supported app-test workloads.
- Update the performance snapshot in `README.md` whenever committing a
  performance change, or state there why the table is unchanged.
- Do not commit generated benchmark results under `bench/results/`, build
  outputs under `bench/.build/`, or Python `__pycache__` directories.
- Use `git -c core.fsmonitor=false status --short` if normal `git status`
  reports an fsmonitor IPC warning.

## Main Fast-Fork Build

Use the validation script as the default build entrypoint:

```sh
./test-fastfork.sh quick --setup-only
```

The default fast-fork validation build lives at:

```text
bench/.build/fastfork-validation
```

The installed binaries from that build live at:

```text
bench/.build/fastfork-validation/tmp_install/usr/local/pgsql/bin
```

To rebuild without reconfiguring:

```sh
./test-fastfork.sh quick --setup-only --no-reconfigure
```

To wipe and reconfigure:

```sh
./test-fastfork.sh quick --setup-only --wipe
```

## Validation Gates

Run the smallest useful gate while iterating:

```sh
./test-fastfork.sh quick --no-reconfigure
```

Before committing implementation changes, run:

```sh
./test-fastfork.sh core --no-reconfigure
```

Use full mode for broad compatibility sweeps:

```sh
./test-fastfork.sh full --no-reconfigure
```

The validation script already:

- configures the fast-fork build flags
- builds or reuses the validation build
- creates the temporary install
- fixes macOS temporary-install library names
- runs the fast-fork fixture snapshot smoke test
- runs the seed-only dirty-restart smoke test
- runs the no-data-directory startup smoke test, including DDL that mutates
  seed-backed catalogs through the in-memory overlay
- skips known incompatible recovery/durability/replication/background-worker
  suites

If a validation failure is related to an intentionally unsupported PostgreSQL
feature, update the skip rationale in `test-fastfork.sh`; do not silently ignore
it.

## Runtime Benchmark

Use `bench/compare_pgbench.py` for transaction/runtime performance. It builds or
reuses a cached stock baseline and compares it with the fast-fork build.

Default permanent-table rollback workload:

```sh
python3 bench/compare_pgbench.py \
  --rounds 3 \
  --transactions 200 \
  --rows 200 \
  --reuse-builds \
  --output-dir bench/results/runtime-compare
```

Fixture snapshot workload:

```sh
python3 bench/compare_pgbench.py \
  --fakewal-workload snapshot \
  --rounds 3 \
  --transactions 200 \
  --rows 200 \
  --reuse-builds \
  --output-dir bench/results/runtime-snapshot-compare
```

To compare against already-built installs:

```sh
python3 bench/compare_pgbench.py \
  --baseline-bin bench/.build/installs/baseline-meson/bin \
  --fakewal-bin bench/.build/fastfork-validation/tmp_install/usr/local/pgsql/bin \
  --rounds 3 \
  --transactions 200 \
  --rows 200 \
  --output-dir bench/results/runtime-compare
```

The rollback workload deliberately uses ordinary permanent tables inside a
transaction, not temp tables. Keep it that way unless the user explicitly asks
for temp-table measurements.

## Startup Benchmark

Use `bench/compare_startup.py` for startup performance. This is the gate for
startup/recovery specs.

Reuse-mode startup comparison:

```sh
python3 bench/compare_startup.py \
  --baseline-bin bench/.build/installs/baseline-meson/bin \
  --fakewal-bin bench/.build/fastfork-validation/tmp_install/usr/local/pgsql/bin \
  --rounds 10 \
  --output-dir bench/results/startup-compare
```

Copy-mode startup comparison:

```sh
python3 bench/compare_startup.py \
  --baseline-bin bench/.build/installs/baseline-meson/bin \
  --fakewal-bin bench/.build/fastfork-validation/tmp_install/usr/local/pgsql/bin \
  --rounds 10 \
  --mode copy \
  --output-dir bench/results/startup-copy-compare
```

No-data-directory startup comparison:

```sh
python3 bench/compare_startup.py \
  --baseline-bin bench/.build/installs/baseline-meson/bin \
  --fakewal-bin bench/.build/fastfork-validation/tmp_install/usr/local/pgsql/bin \
  --rounds 10 \
  --mode no-data-dir \
  --output-dir bench/results/startup-no-data-dir-compare
```

For a single install smoke test:

```sh
python3 bench/run_startup.py \
  --bin bench/.build/fastfork-validation/tmp_install/usr/local/pgsql/bin \
  --label fastfork-startup-smoke \
  --rounds 2 \
  --output bench/results/startup-smoke.json
```

Record `initdb`, setup+start, start, first-query, stop, copy-mode time, and
no-data-dir runtime setup time separately when reporting startup changes.

When working on no-data-directory startup, treat the seed image as an immutable
backing layer, not an immutable database. DDL and migrations must be able to
modify seed-backed catalogs and relations by shadowing pages in the runtime
memory overlay.

## Measure/Iterate Loop

For each performance spec:

1. Build the current fast fork:

   ```sh
   ./test-fastfork.sh quick --setup-only --no-reconfigure
   ```

2. Capture a before number if the change is performance-sensitive:

   ```sh
   python3 bench/compare_pgbench.py --rounds 3 --transactions 200 --rows 200 --reuse-builds --output-dir bench/results/before
   ```

   For startup work, use:

   ```sh
   python3 bench/compare_startup.py --rounds 10 --reuse-builds --output-dir bench/results/startup-before
   ```

3. Implement the smallest useful slice behind the relevant flag.

4. Rebuild:

   ```sh
   ./test-fastfork.sh quick --setup-only --no-reconfigure
   ```

5. Validate:

   ```sh
   ./test-fastfork.sh core --no-reconfigure
   ```

6. Measure after:

   ```sh
   python3 bench/compare_pgbench.py --rounds 3 --transactions 200 --rows 200 --reuse-builds --output-dir bench/results/after
   ```

   Or for startup:

   ```sh
   python3 bench/compare_startup.py --rounds 10 --reuse-builds --output-dir bench/results/startup-after
   ```

7. If the benchmark is slower or correctness regresses, keep iterating before
   committing.

8. Before committing, run:

   ```sh
   git -c core.fsmonitor=false diff --check
   git -c core.fsmonitor=false status --short
   ```

## Current Spec Order

Specs live in `spec/` and are intended to be implemented incrementally:

- `001-in-memory-storage-manager.md`
- `002-in-memory-transaction-status-slrus.md`
- `003-early-wal-assembly-bypass.md`
- `004-compile-time-observability-bypass.md`
- `005-fast-memory-contexts.md`
- `006-ephemeral-catalog-fast-path.md`
- `007-no-durable-maintenance.md`
- `008-ephemeral-buffer-mode.md`
- `009-direct-permanent-buffer-mode.md`
- `010-trusted-ddl-mode.md`
- `011-startup-benchmark.md`
- `012-conservative-fast-startup.md`
- `013-seed-only-startup.md`
- `014-no-data-directory-startup.md`

When adding a new spec, use the next `NNN-name.md` prefix.

## Reporting Results

Always update `README.md` when committing a performance change. For each
performance change, either update the `Current Performance Snapshot` table with
the latest benchmark result or explicitly note in the commit/PR summary why the
README performance table was not changed.

When reporting a performance change, include:

- commit or working-tree state
- validation command and result
- benchmark command
- output directory
- median baseline number
- median fast-fork number
- speedup ratio
- any known caveats, such as skipped TAP tests or incompatible recovery suites

Keep claims tied to the benchmark that produced them. Do not generalize a
startup result into a runtime result, or a snapshot result into the permanent
rollback workload.
