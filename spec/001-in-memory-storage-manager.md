# In-Memory Storage Manager

## Summary

Add a test-only in-memory storage manager that can replace PostgreSQL's
magnetic-disk storage manager when building the fast fork. The goal is to keep
DDL, DML, queries, indexes, multiple backends, and transaction rollback working
for unit-test-style workloads while removing filesystem I/O from relation
storage.

This is an opt-in fork feature. Stock PostgreSQL behavior must remain the
default.

## Goals

- Replace relation storage backed by `md.c` with memory-backed relation forks.
- Keep the change behind a build flag so rebasing on upstream PostgreSQL stays
  manageable.
- Support ordinary SQL test workloads: create/drop tables and indexes, read and
  write heap/index blocks, truncate relations, and run multiple client
  connections.
- Share relation contents across backends. The implementation must not be
  process-local.
- Make durability-oriented smgr operations cheap no-ops.
- Validate correctness with the fast-fork validation script and measure speed
  with the repeatable pgbench benchmark.

## Non-Goals

- Durability across postmaster restarts.
- Crash recovery.
- Replication, base backup, WAL archiving, or logical decoding.
- Preserving relation contents after the disposable test cluster exits.
- Supporting production configurations.

## Build Flag

Add a new build option:

- Meson: `-Dtest_mem_smgr=true`
- Autoconf: `--enable-test-mem-smgr`
- C define: `USE_TEST_MEM_SMGR`

The option should default to `false`. When disabled, all code paths should keep
using `md.c` exactly as upstream PostgreSQL does today.

The fast-fork build can combine this with the existing flags:

```sh
meson setup build-fastfork \
  -Dtest_fake_wal=true \
  -Dtest_no_bg_jobs=true \
  -Dtest_mem_smgr=true
```

## Storage-Manager Boundary

The clean switch point is the storage-manager dispatch table in
`src/backend/storage/smgr/smgr.c`. The relevant boundary is the `f_smgr` table
around `smgrsw[]`, currently routing all storage operations to the magnetic-disk
manager in `src/backend/storage/smgr/md.c`.

With `USE_TEST_MEM_SMGR`, `smgrsw[]` should route to a new memory storage
manager implementation instead of `md.c`. Without the flag, leave the existing
`md.c` entry untouched.

Proposed files:

- `src/backend/storage/smgr/memsmgr.c`
- `src/include/storage/memsmgr.h`

The memory manager should implement the same `f_smgr` interface shape used by
`md.c`:

- `meminit`
- `memshutdown`
- `memopen`
- `memclose`
- `memcreate`
- `memexists`
- `memunlink`
- `memextend`
- `memzeroextend`
- `memprefetch`
- `memmaxcombine`
- `memreadv`
- `memstartreadv`
- `memwritev`
- `memwriteback`
- `memnblocks`
- `memtruncate`
- `memimmedsync`
- `memregistersync`
- `memfd`

## Required Behavior

### Relation Identity

Key stored objects by:

- `RelFileLocatorBackend`
- `ForkNumber`

Each key maps to a relation fork containing a logical block count and the block
data for block numbers `0..nblocks - 1`.

Temporary relation behavior should respect `RelFileLocatorBackend.backend`.
Temporary relations can still be keyed in the same global map; backend identity
keeps them isolated.

### Shared Storage

Relation storage must be visible to all backends in the same postmaster. A
backend-local hash table or backend-local `mmap` is not sufficient.

Acceptable approaches:

- Use PostgreSQL shared memory plus lightweight locks.
- Use anonymous shared `mmap` created by the postmaster before backends fork,
  with metadata and relation pages allocated from that shared region.

The first implementation should prefer fixed-size or chunked allocation over a
complex general-purpose allocator. It is acceptable for this test-only build to
fail clearly when the configured in-memory storage limit is exhausted.

### Block Operations

`memreadv`

- Copy existing blocks into the provided buffers.
- Match `mdreadv` error behavior when reading beyond EOF unless callers already
  rely on zero-fill for a specific path.

`memwritev`

- Copy supplied blocks into storage.
- Grow the relation if writing at the current end.
- Preserve PostgreSQL block size semantics.
- Ignore `skipFsync`.

`memextend`

- Require extension at the expected block number unless the existing smgr API
  allows the same recovery/bootstrap exceptions as `md.c`.
- Copy the supplied block into the new block.

`memzeroextend`

- Grow by `nblocks` zero-filled blocks starting at `blocknum`.

`memnblocks`

- Return the logical block count for the relation fork.

`memtruncate`

- Reduce the logical block count.
- Reclaim memory if practical, but correctness does not require immediate
  physical memory return in the first version.

`memunlink`

- Remove the relation fork from the shared map.
- Use warning-style behavior where the smgr contract requires cleanup not to
  abort transaction end.

`memexists`

- Return whether a relation fork exists in the shared map.

### Cheap No-Ops

These should be no-ops in the memory manager:

- `memimmedsync`
- `memregistersync`
- `memwriteback`
- `memprefetch`

`memprefetch` should report success when the requested blocks exist. It should
not issue OS prefetch calls.

`memmaxcombine` should return a conservative value that keeps vector reads and
writes correct. Returning `nblocks`-compatible contiguous ranges is fine when
blocks are stored contiguously; otherwise return `1`.

### Async I/O

The fast fork keeps async I/O workers enabled, so `memstartreadv` must be safe
with the existing AIO path.

The simplest acceptable behavior is to complete memory reads synchronously while
preserving the smgr/AIO contract expected by callers. If that is awkward in the
first patch, route `smgr_startreadv` for `USE_TEST_MEM_SMGR` through the same
copy path as `memreadv` and document why it is intentionally synchronous.

### File Descriptor API

`memfd` cannot return a real relation file descriptor. Callers that still need
`smgr_fd` under `USE_TEST_MEM_SMGR` should be audited.

Preferred behavior:

- Make `memfd` raise `ERROR` with a clear message.
- Add targeted guards for any unsupported caller that appears in validation.

Do not silently create disk files just to satisfy this API.

## Concurrency

The shared map and relation contents need synchronization across backends.

Minimum locking requirements:

- Protect map create/drop/lookup with a shared lock.
- Protect each relation fork's metadata and page array while extending,
  truncating, unlinking, or copying blocks.
- Allow concurrent reads when there is no writer.

The first version may use coarse locking if that keeps the implementation
simple. Benchmark results can then guide whether lock granularity matters.

## Configuration

Add a test-only memory limit so runaway test workloads fail predictably:

- Suggested GUC: `test_mem_smgr_size`
- Default: enough for the benchmark and validation script.
- Failure mode: `ERROR` explaining that in-memory storage is exhausted.

If a GUC is too much for the first patch, use a compile-time default and leave a
follow-up note in the implementation.

## Validation

The spec is satisfied when both validation paths pass on the current fork.

### Correctness Gate

Run the fast-fork validation script with the memory storage manager enabled:

```sh
./test-fastfork.sh --wipe
```

After `test_mem_smgr` is wired into the script, this should configure the build
with:

```sh
-Dtest_fake_wal=true -Dtest_no_bg_jobs=true -Dtest_mem_smgr=true
```

Passing means:

- The script exits successfully.
- All selected compatible tests pass.
- Any skipped tests are explicitly incompatible with the fast-fork feature set
  or missing local test dependencies.

### Performance Gate

Run the repeatable pgbench comparison:

```sh
python3 bench/compare_pgbench.py \
  --rounds 5 \
  --transactions 200 \
  --rows 200
```

After the benchmark harness knows about `test_mem_smgr`, the fork build should
include:

```sh
-Dtest_fake_wal=true -Dtest_no_bg_jobs=true -Dtest_mem_smgr=true
```

Passing means:

- Baseline and fork runs complete successfully.
- Results are written under `bench/results/`.
- The summary records the fork speed relative to the cached baseline build.

## Implementation Checklist

- Add Meson and autoconf build options for `test_mem_smgr`.
- Add `USE_TEST_MEM_SMGR` to generated config headers.
- Add `memsmgr.c` and `memsmgr.h`.
- Wire `smgrsw[]` to `memsmgr` when `USE_TEST_MEM_SMGR` is defined.
- Add shared-memory initialization for relation-map metadata and storage.
- Implement create/exists/read/write/extend/zeroextend/nblocks/truncate/unlink.
- Make sync, writeback, and prefetch operations no-ops.
- Decide and document `memstartreadv` synchronous behavior.
- Make `memfd` fail clearly, then audit any validation failures it exposes.
- Teach `test-fastfork.sh` to configure `-Dtest_mem_smgr=true`.
- Teach `bench/compare_pgbench.py` to configure the fork build with
  `-Dtest_mem_smgr=true`.
- Run the validation and benchmark gates above.

## Risks

- Some PostgreSQL paths may still assume a real file descriptor via `smgr_fd`.
- Shared-memory sizing could be too small for larger unit-test databases.
- Coarse locking may reduce gains for high-client benchmarks.
- Relation cleanup paths may depend on `md.c`'s warning-vs-error behavior during
  transaction end; `memunlink` should match that contract carefully.
- Parallel query and generic background-worker tests are already outside the
  current `test_no_bg_jobs` validation set, so they should not drive this spec.
