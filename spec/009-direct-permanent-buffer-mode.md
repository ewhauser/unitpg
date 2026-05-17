# Direct Permanent Buffer Mode

## Summary

Extend the existing memory-backed buffer fast path from temp relations to
ordinary relations. Our target unit tests create permanent tables and roll the
whole test transaction back; they do not rely on PostgreSQL temp-table storage.
That means the current temp-only direct buffer mode misses the hot path we now
care about.

With `test_mem_smgr=true`, permanent relation pages already live in `memsmgr`,
but heap and index access still route through the full shared buffer manager.
That adds buffer-table lookup, pin accounting, dirty tracking, eviction,
writeback, and page copies between shared buffers and the memory storage
manager. In an ephemeral test build, most of that work simulates a disk cache
we do not need.

## Goals

- Let ordinary heap and index pages backed by `memsmgr` be accessed directly
  through memory-storage pages.
- Preserve normal `Buffer` handles, page layout, tuple layout, MVCC visibility,
  btree behavior, and executor behavior.
- Keep relation/page locks and content locks needed for correctness.
- Make the rollback-heavy benchmark use ordinary permanent tables, not temp
  tables.
- Keep stock PostgreSQL behavior as the default.
- Validate with `test-fastfork.sh` and the pgbench comparison harness.

## Non-Goals

- Do not preserve durability, crash recovery, WAL replay, base backup, or
  replication behavior.
- Do not remove MVCC, transaction rollback, or catalog visibility semantics.
- Do not make seed/bootstrap catalog pages process-local.
- Do not require application SQL to use temp tables.
- Do not optimize extension, security, replication, or recovery test suites.

## Build Flag

Use the existing buffer fast-path flag rather than adding another top-level
switch:

- Meson: `-Dtest_ephemeral_buffers=true`
- Autoconf: `--enable-test-ephemeral-buffers`
- C define: `USE_TEST_EPHEMERAL_BUFFERS`

Permanent direct buffers should only activate when both
`USE_TEST_EPHEMERAL_BUFFERS` and `USE_TEST_MEM_SMGR` are enabled. The
implementation may also require a future single-backend flag for the broadest
shortcut set, but the first pass should keep shared `Buffer` descriptors and
page locks intact so it can be validated before single-backend surgery.

## Current Boundary

The temp direct-buffer implementation works because local buffers already have
a per-buffer page pointer:

- `src/backend/storage/buffer/localbuf.c`
- `LocalBufferBlockPointers`
- `mem_buffer_direct_page()`

Shared buffers are harder. A shared buffer page is normally computed from the
fixed `BufferBlocks` array:

- `src/include/storage/bufmgr.h`
- `src/backend/storage/buffer/bufmgr.c`
- `BufferGetBlock()`
- `BufHdrGetBlock()`
- `BufferBlocks`

To direct-back permanent relations, shared buffers need a page-pointer
indirection similar to local buffers, or a very narrow bypass at the places that
copy between `smgr` and shared buffer memory.

## Design

Add a direct-page pointer table for shared buffers in test builds:

```c
#if defined(USE_TEST_EPHEMERAL_BUFFERS) && defined(USE_TEST_MEM_SMGR)
extern PGDLLIMPORT Block *SharedBufferBlockPointers;
#endif
```

When a shared buffer is direct-backed, `SharedBufferBlockPointers[buf_id]`
points at the `memsmgr` page for that relation fork/block. When it is not
direct-backed, it points at the ordinary `BufferBlocks` slot. `BufferGetBlock()`
and `BufHdrGetBlock()` should resolve through the pointer table only in the
test build; normal builds keep the existing arithmetic path.

Keep the shared buffer descriptor lifecycle:

1. The buffer table still maps `(spcOid, dbOid, relNumber, fork, block)` to a
   `BufferDesc`.
2. Pins, resource-owner tracking, usage counts, and content locks still exist.
3. The page memory for eligible `memsmgr` relations points directly to the
   `memsmgr` page.
4. Dirty marking remains for caller compatibility, but flush/writeback for
   direct-backed pages becomes a no-op because the backing page is already
   modified.
5. On buffer reuse, invalidation, or relation drop, detach the pointer and
   restore it to the ordinary `BufferBlocks` slot before the descriptor can be
   reused for a non-direct page.

This intentionally keeps normal buffer identity while removing duplicate page
storage and the read/write copy path.

## Eligibility

A page can be direct-backed when:

- the build has both `test_mem_smgr` and `test_ephemeral_buffers`
- the backend is under a postmaster, not bootstrap/standalone initdb
- the relation uses `memsmgr`
- the relation is not a seed on-disk page that has never been materialized into
  `memsmgr`
- the fork/block has a stable `memsmgr` page entry, creating one when extending
  or first dirtying a page
- the page is not currently involved in I/O that expects ordinary shared-buffer
  memory

For pages first read from the seed cluster on disk, the first pass can either:

- keep them on ordinary shared-buffer memory until the first write materializes
  a `memsmgr` page, or
- materialize the seed page into `memsmgr` immediately and point the buffer at
  that copy.

Prefer the second path if benchmark profiling shows read-copy cost matters.
Prefer the first path if it is much safer for the initial implementation.

## Implementation Sketch

### `memsmgr`

Broaden the helper API:

```c
bool mem_buffer_direct_enabled(SMgrRelation reln);
Block mem_buffer_direct_page(SMgrRelation reln, ForkNumber forknum,
                             BlockNumber blocknum, bool create,
                             bool *found);
```

The existing helper currently only returns temp pages. Extend it so permanent
relations can return shared `memsmgr` pages. It must still refuse `md.c` paths
and bootstrap/standalone operation.

### `buf_init.c`

Allocate and initialize `SharedBufferBlockPointers` alongside `BufferBlocks`.
Each pointer initially references its ordinary `BufferBlocks` slot.

### `bufmgr.c` and `bufmgr.h`

In test builds:

- make `BufferGetBlock()` use `SharedBufferBlockPointers[buffer - 1]` for
  shared buffers
- make `BufHdrGetBlock()` use the same pointer table
- track whether a shared buffer is direct-backed
- after buffer allocation/read, ask `memsmgr` whether the page can be
  direct-backed
- if direct-backed, point the descriptor at the `memsmgr` page and avoid the
  `smgrread()` copy into ordinary shared-buffer memory
- on extend/zero-extend, attach the new buffer to the newly created `memsmgr`
  page
- skip `smgrwrite()`, writeback, and fsync registration for direct-backed pages
- detach direct pointers during buffer invalidation, relation drop, database
  drop, and descriptor reuse

### `smgr` / `memsmgr`

The storage manager remains the owner of relation page lifetime. The buffer
manager may point at a page, but it must not free it. Truncate and unlink must
invalidate or detach any buffer descriptors pointing at discarded pages before
those pages are removed from the `memsmgr` hashes.

## Benchmark Update

`bench/unit-test-rollback.pgbench` should model ordinary test DDL:

- `CREATE TABLE`, not `CREATE TEMP TABLE`
- ordinary indexes and constraints
- all setup and mutations inside one transaction
- final `ROLLBACK` drops the created objects

Use this permanent-table rollback workload for the direct permanent buffer path.

## Correctness Requirements

- Heap and index reads/writes through direct-backed buffers behave like normal
  buffers.
- Rollback of permanent DDL and DML remains correct.
- Index scans and constraint checks see the same page contents as heap scans.
- Truncate/drop cannot leave buffer descriptors pointing at stale `memsmgr`
  pages.
- Dirty direct-backed pages are not copied back over newer `memsmgr` contents.
- Non-eligible relations continue using ordinary shared buffer memory.

## Validation

Run:

```sh
./test-fastfork.sh core --no-reconfigure
```

Run the permanent-table rollback benchmark:

```sh
python3 bench/compare_pgbench.py \
  --rounds 3 \
  --transactions 200 \
  --rows 200 \
  --reuse-builds \
  --output-dir bench/results/direct-permanent-buffers
```

The implementation is successful if:

- validation passes
- the permanent-table rollback workload remains faster than stock Postgres
- disabling `test_ephemeral_buffers` shows the direct permanent path contributes
  measurable speedup

## Risks

- `BufferGetBlock()` is widely used and currently cheap. Adding indirection must
  be compile-time gated so stock builds do not pay for it.
- Buffer invalidation bugs can produce use-after-free style corruption if a
  descriptor still points at a removed `memsmgr` page.
- Some buffer-manager code assumes page memory is owned by `BufferBlocks`.
  Audit Valgrind hooks, I/O paths, checksums, page verification, and debug
  memory poisoning.
- Shared-buffer descriptors still carry concurrency semantics. If a later
  single-backend mode removes those semantics, it should be a separate spec and
  benchmark step.
