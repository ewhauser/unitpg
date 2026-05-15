# No Data Directory Startup

## Summary

Remove the requirement for a persistent PostgreSQL data directory in the fastest
test mode. Instead of running `initdb` and keeping seed catalog relation files
on disk, package or generate an in-memory seed image and start the server from
that image directly.

This is the largest startup swing. Conservative fast startup skips redo.
Seed-only startup treats `PGDATA` as an immutable seed. No-data-directory
startup removes even that seed directory from the hot path.

The goal is for a test harness to launch a disposable PostgreSQL-compatible
server without paying `initdb`, data-directory copy, WAL-directory setup,
control-file startup, or seed catalog file I/O costs per worker.

## Goals

- Start a fast-fork server from an in-memory seed image.
- Avoid `initdb` in the common test startup path.
- Avoid persistent `PGDATA` relation files for seed catalogs.
- Keep parser, planner, executor, heap, index, relcache, syscache, MVCC, and
  rollback semantics for supported tests.
- Keep ordinary SQL connection behavior once the server is running.
- Measure startup improvement with the startup benchmark.

## Non-Goals

- Replacing upstream `initdb` for normal builds.
- Persisting data across postmaster restarts.
- Supporting crash recovery, PITR, hot standby, replication, logical decoding,
  or base backup.
- Supporting every extension or object type in the seed image.
- Removing the need for a temporary runtime directory for sockets, logs, lock
  files, or compatibility files.

## Build Flag

Add a new opt-in build option:

- Meson: `-Dtest_no_data_directory_startup=true`
- Autoconf: `--enable-test-no-data-directory-startup`
- C define: `USE_TEST_NO_DATA_DIRECTORY_STARTUP`

This option should require:

- `test_seed_only_startup=true`
- `test_mem_smgr=true`
- `test_mem_slru=true`
- `test_fake_wal=true`

It may also require single-backend mode in a later implementation if shared
postmaster startup assumptions become too expensive to preserve.

## Seed Image Options

### Build-Time Seed Image

Generate a seed catalog image during the build or test setup:

1. Run normal `initdb` once.
2. Extract required seed relation forks and metadata.
3. Store them as a compact binary artifact under `bench/.build/` or an install
   share directory.
4. At postmaster start, map or load the artifact into memory.

This is the most practical first implementation because it reuses upstream
bootstrap/initdb behavior to create correct catalogs.

### Compiled Seed Image

Convert the seed artifact into C data or a linked binary blob. This reduces file
discovery at startup but creates merge and build-size concerns. Treat this as a
later optimization.

### On-Demand Bootstrap

Run bootstrap catalog creation directly into memory at server start. This avoids
an artifact but likely costs too much startup time and touches too much initdb
logic. It is not recommended for the first implementation.

## Runtime Directory

Even without persistent `PGDATA`, the server still needs a runtime home for:

- Unix socket directory
- postmaster PID/lock files if not bypassed
- logs
- configuration source
- extension/control file lookup if supported
- any compatibility paths that currently expect `DataDir`

Introduce an explicit fast-fork runtime directory:

```text
postgres --fastfork-runtime-dir=/tmp/fastfork-...
```

or let the harness create a temporary `PGDATA`-shaped directory containing only
runtime/config files. The key is that catalog/storage seed state does not come
from that directory.

## Storage Design

`memsmgr` becomes the owner of both seed and runtime relation pages.

Seed pages:

- loaded from the seed image
- immutable
- shared across backends where supported
- copied into runtime memory on first write

Runtime pages:

- allocated from shared memory or anonymous mmap
- discarded on postmaster exit
- included in fixture snapshots
- never flushed to durable relation files

The storage manager should expose a seed lookup path:

```c
bool mem_seed_page_exists(RelFileLocatorBackend rlocator,
                          ForkNumber forknum,
                          BlockNumber blocknum);
Block mem_seed_page(RelFileLocatorBackend rlocator,
                    ForkNumber forknum,
                    BlockNumber blocknum);
```

The exact API can differ, but seed reads should not go through `md.c`.

## Metadata Design

The seed image must include or derive:

- catalog version
- system identifier
- database OIDs and tablespace mapping
- relmapper state
- relation fork sizes
- seed relation pages
- initial transaction/OID/multixact counters
- encoding/locale settings expected by the cluster
- minimal control metadata needed by SQL-visible functions

Do not include:

- WAL segments
- SLRU files
- statistics files
- replication slots
- logical decoding state
- postmaster runtime files

## Startup Flow

1. Parse config and runtime directory.
2. Load seed image metadata.
3. Initialize shared memory.
4. Initialize `memsmgr` seed-page provider.
5. Initialize in-memory SLRUs from seed counters.
6. Initialize fake WAL/LSN state.
7. Mark recovery as complete/not active.
8. Initialize relcache/syscache as usual against seed catalogs.
9. Accept connections.

No `StartupXLOG()` redo scan, control-file recovery, WAL directory validation,
or durable checkpoint state is needed.

## Benchmark Integration

Extend the startup benchmark with a no-data-directory mode:

```text
--mode no-data-dir
--seed-image PATH
```

The benchmark should report:

- seed image load time
- postmaster start time
- first query time
- total runtime directory setup time

This lets us compare:

- normal `initdb` + start
- seed-only `PGDATA` start
- no-data-directory seed-image start

## Correctness Requirements

- Server starts without a normal initialized `PGDATA` relation-file tree.
- `SELECT 1` works.
- Catalog lookups work.
- Creating/querying/dropping supported tables works.
- Fixture snapshot/restore works.
- Postmaster restart returns to the original seed image.
- Runtime-created data does not survive restart.
- Unsupported features fail clearly.

## Validation

Run:

```sh
./test-fastfork.sh core --no-reconfigure
```

Run startup benchmark:

```sh
python3 bench/compare_startup.py \
  --rounds 10 \
  --mode no-data-dir \
  --reuse-builds \
  --output-dir bench/results/no-data-directory-startup
```

Run pgbench:

```sh
python3 bench/compare_pgbench.py \
  --rounds 3 \
  --transactions 200 \
  --rows 200 \
  --reuse-builds \
  --output-dir bench/results/no-data-directory-startup-pgbench
```

The implementation is successful if:

- validation passes
- startup benchmark shows a material improvement over seed-only startup for
  fresh-worker harnesses
- runtime benchmark remains faster than stock baseline

## Risks

- This touches PostgreSQL's deepest bootstrap assumptions. Keep it behind a
  separate flag and implement only after the startup benchmark, conservative
  startup, and seed-only startup are measured.
- Extension lookup and shared library paths often assume an install/data
  directory shape. Exclude extension-heavy tests initially.
- Locale, encoding, and collation state must match the seed image exactly.
- Relmapper/control metadata mistakes can produce confusing catalog failures.
- A build-time seed artifact can drift from catalog definitions. Regenerate it
  as part of the build or validate catalog version before startup.
