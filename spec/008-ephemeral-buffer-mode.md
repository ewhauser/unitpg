# Ephemeral Buffer Mode

## Goal

Remove redundant buffer-cache work for memory-backed test clusters while keeping
Postgres' heap, index, MVCC, and planner semantics intact.

This fork already replaces durable storage with `memsmgr`, but relation access
still routes through the normal buffer manager. For temp-heavy unit-test
workloads that means pages can live in two places at once:

- a local or shared buffer page used by heap/index code
- a `memsmgr` page used as the backing store

The extra layer still performs buffer lookup, pin accounting, dirty tracking,
flush/writeback, and copies between buffer pages and memory-smgr pages. In an
ephemeral test cluster, that work is mostly cache simulation rather than useful
durability or recovery machinery.

## Non-Goals

- Do not change tuple layout, page layout, heap visibility rules, btree page
  format, or executor behavior.
- Do not remove relation locks or content locks needed for correctness.
- Do not make permanent/shared relations process-local.
- Do not optimize for crash recovery, durability, replication, or WAL replay.
- Do not require applications to change SQL.

## Design

Introduce a compile-time flag:

```text
test_ephemeral_buffers
```

When enabled with `test_mem_smgr`, temp relation buffers may use the `memsmgr`
page itself as their buffer memory. The local buffer descriptor remains in
place, so callers still receive ordinary `Buffer` handles and the heap/index
code continues to use standard `BufferGetPage()` access.

The first implementation should be deliberately narrow:

1. Keep normal local buffer descriptors, tags, pins, and resource-owner checks.
2. For temp relation pages backed by `memsmgr`, set the local buffer block
   pointer to the corresponding `memsmgr` page.
3. Mark directly backed local buffers as dirty only for caller compatibility;
   eviction/flush does not need to copy the page back into `memsmgr`, because
   the page is already the backing page.
4. On local-buffer reuse or invalidation, detach the descriptor from the direct
   page without freeing the `memsmgr` page.
5. Leave shared catalog buffers on the normal shared-buffer path until a later,
   more invasive catalog-overlay design removes that churn.

This is intentionally a stepping stone. It removes the easiest duplicate page
copying while avoiding a broad rewrite of `bufmgr.c`.

## Implementation Sketch

Add a small `memsmgr` helper that is available only in test builds:

```c
bool mem_buffer_direct_enabled(SMgrRelation reln);
Block mem_buffer_direct_page(SMgrRelation reln, ForkNumber forknum,
                             BlockNumber blocknum, bool create,
                             bool *found);
```

The helper should return direct page memory only for temp relations managed by
`memsmgr`. It should refuse normal `md.c` relations.

In `localbuf.c`:

- Track a per-local-buffer `direct_backed` bit.
- In `LocalBufferAlloc`, after assigning the new temp buffer tag, ask `memsmgr`
  for the direct page. If it exists, attach it and mark the buffer valid.
- In `ExtendBufferedRelLocal`, after `smgrzeroextend`, attach each new local
  buffer to the corresponding zeroed `memsmgr` page.
- In `FlushLocalBuffer`, skip `smgrwrite()` for direct-backed buffers and just
  clear the dirty bit.
- In `InvalidateLocalBuffer`, clear the direct-backed bit and detach the block
  pointer so a future non-direct use cannot accidentally reference an old page.

## Correctness Requirements

- Existing temp table DDL/DML/query behavior must pass the fast-fork validation
  script.
- A buffer hit must still return the same page content after writes, eviction,
  and re-read.
- Rollback must preserve current transaction semantics.
- Multiple backends must not share temp-buffer direct pages; temp direct pages
  remain backend-local.
- Normal relations and seed catalog pages must continue using existing shared
  buffer semantics.

## Validation

Run the fast-fork validation script:

```sh
./test-fastfork.sh --no-reconfigure
```

Run the repeatable benchmark against the cached baseline:

```sh
python3 bench/compare_pgbench.py \
  --rounds 3 \
  --transactions 12000 \
  --warmup-transactions 100 \
  --reuse-builds \
  --output-dir bench/results/ephemeral-buffers
```

The implementation should be considered successful if:

- validation passes
- the fast fork remains faster than the baseline
- the benchmark summary records a measurable speedup versus the previous fast
  fork with `test_ephemeral_buffers` disabled

## Risks

- The local buffer manager assumes that buffer memory is owned by the local
  buffer context. Direct pointers into `memsmgr` must be detached carefully on
  invalidation.
- Skipping `smgrwrite()` is only correct when the buffer page is already the
  backing `memsmgr` page.
- This does not address the larger catalog-DDL bottleneck. A catalog overlay is
  still expected to be a bigger win for temp-table-heavy unit tests.
