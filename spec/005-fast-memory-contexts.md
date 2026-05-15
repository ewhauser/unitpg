# Fast Memory Contexts

## Summary

Add a test-only allocator strategy for short-lived PostgreSQL memory contexts.
The fast fork's target workload creates many small, short-lived objects while
parsing, planning, executing DDL, building catalog tuples, running SPI, and
rolling transactions back. PostgreSQL's `AllocSetContext` is a strong general
purpose default, but it pays bookkeeping costs that are not always needed for
unit-test-style workloads.

PostgreSQL already has specialized memory context implementations:

- `AllocSetContext`: general-purpose allocator with freelists.
- `GenerationContext`: good when allocations have similar lifetimes.
- `BumpContext`: fastest/densest for many short-lived allocations that are
  released only by resetting or deleting the whole context.
- `SlabContext`: good for fixed-size chunks.

This spec uses those existing boundaries more aggressively in the fast fork,
then optionally evaluates an external allocator for the remaining `malloc()`
traffic.

## Goals

- Reduce allocator overhead in parser, planner, executor, SPI, portal,
  per-query, and per-transaction contexts.
- Keep SQL behavior unchanged: memory lifetime, rollback, error cleanup, and
  context reset/delete semantics must remain correct.
- Prefer existing PostgreSQL memory context types before inventing a new
  allocator.
- Keep the change behind an opt-in build flag so upstream merges remain
  manageable.
- Fail safely if a context is switched to `BumpContext` but later tries to
  `pfree()`, `repalloc()`, or inspect chunk ownership.
- Validate correctness with the fast-fork validation script and measure speed
  with the repeatable pgbench benchmark.

## Non-Goals

- Changing PostgreSQL's public memory context API.
- Rewriting arbitrary code to stop using `palloc()`.
- Removing memory contexts entirely.
- Weakening error cleanup or transaction abort cleanup.
- Changing long-lived cache contexts such as `CacheMemoryContext` in the first
  pass.
- Making production builds use the test-only allocator policy.

## Build Flag

Add a new build option:

- Meson: `-Dtest_fast_memory_contexts=true`
- Autoconf: `--enable-test-fast-memory-contexts`
- C define: `USE_TEST_FAST_MEMORY_CONTEXTS`

The option should default to `false`. When disabled, all memory context choices
must match upstream PostgreSQL.

Example fast-fork build:

```sh
meson setup build-fastfork \
  -Dtest_fake_wal=true \
  -Dtest_no_bg_jobs=true \
  -Dtest_fast_memory_contexts=true
```

## Primary Targets

### Per-Query and Executor Contexts

Main files:

- `src/backend/executor/execUtils.c`
- `src/backend/tcop/postgres.c`
- `src/backend/utils/mmgr/portalmem.c`
- `src/backend/executor/spi.c`

Targets:

- `ExecutorState` / `es_query_cxt`
- per-tuple expression contexts
- message/row-description scratch contexts
- portal-owned query execution contexts
- SPI execution contexts that are reset or deleted as a group

Preferred first-pass policy:

- Use `BumpContext` for contexts that are reset wholesale and whose allocations
  are not individually freed or resized.
- Use `GenerationContext` for contexts that have mostly group lifetimes but may
  still see occasional `pfree()`.
- Keep `AllocSetContext` where `repalloc()`, `pfree()`, or memory ownership
  inspection is common.

### Parser and Planner Scratch Contexts

Main files:

- `src/backend/tcop/postgres.c`
- `src/backend/optimizer/*`
- `src/backend/utils/cache/plancache.c`
- `src/backend/commands/prepare.c`
- `src/backend/commands/explain.c`

Targets:

- raw parse tree scratch contexts
- analyzer/rewrite temporary contexts
- planner scratch contexts
- custom/generic plan build contexts that are discarded after planning

The first implementation should focus on contexts whose output is copied into a
longer-lived context before the scratch context is reset. These are good
`BumpContext` candidates.

Cached plans and query trees need more care:

- If the context owns data that may live across transactions or prepared
  statement executions, do not use `BumpContext` unless no callers individually
  free or resize chunks.
- `GenerationContext` may be safer for long-lived plan contexts that still have
  grouped lifetimes.

### DDL and Catalog-Heavy Scratch Contexts

Main files:

- `src/backend/commands/tablecmds.c`
- `src/backend/commands/indexcmds.c`
- `src/backend/commands/trigger.c`
- `src/backend/catalog/objectaddress.c`
- `src/backend/catalog/namespace.c`
- `src/backend/utils/cache/relcache.c`
- `src/backend/utils/cache/typcache.c`

Unit-test workloads tend to be DDL-heavy. Many of these paths create short
scratch contexts that are deleted at the end of a command or loop.

Preferred first-pass policy:

- Convert short-lived per-command scratch contexts to `GenerationContext`.
- Convert per-row/per-object scratch contexts to `BumpContext` only after
  confirming they are reset wholesale and do not call `pfree()`/`repalloc()` on
  their allocations.
- Leave cache contexts alone unless profiling identifies a very specific safe
  conversion.

### Existing BumpContext Users

PostgreSQL already uses `BumpContext` in executor nodes such as hash aggregate,
recursive union, setop, and subplan paths. These are useful examples for the
first patch:

- `src/backend/executor/nodeAgg.c`
- `src/backend/executor/nodeRecursiveunion.c`
- `src/backend/executor/nodeSetOp.c`
- `src/backend/executor/nodeSubplan.c`

New conversions should follow the same pattern: contexts are local, lifetimes
are obvious, and memory is released by reset/delete rather than individual
freeing.

## Design

### Policy Helpers

Add test-only wrappers for context creation so call sites remain readable:

```c
MemoryContext FastQueryContextCreate(MemoryContext parent,
									 const char *name,
									 Size minContextSize,
									 Size initBlockSize,
									 Size maxBlockSize);

MemoryContext FastScratchContextCreate(MemoryContext parent,
									   const char *name,
									   Size minContextSize,
									   Size initBlockSize,
									   Size maxBlockSize);

MemoryContext FastMaybeFreeableContextCreate(MemoryContext parent,
											 const char *name,
											 Size minContextSize,
											 Size initBlockSize,
											 Size maxBlockSize);
```

Under normal builds, these wrappers should call `AllocSetContextCreate`.

Under `USE_TEST_FAST_MEMORY_CONTEXTS`:

- `FastQueryContextCreate` should prefer `GenerationContext`.
- `FastScratchContextCreate` should prefer `BumpContext`.
- `FastMaybeFreeableContextCreate` should prefer `GenerationContext`.

Use explicit wrapper names rather than changing `AllocSetContextCreate`
globally. That keeps the patch merge-friendly and avoids surprising long-lived
contexts.

### BumpContext Safety

`BumpContext` is only valid when allocations are never individually freed,
resized, or inspected for chunk context/space. The first implementation should
make this hard to misuse:

- Convert only contexts with clear reset/delete lifetimes.
- In development, run at least one validation pass with
  `MEMORY_CONTEXT_CHECKING` if practical.
- If a converted path fails because code calls `pfree()` or `repalloc()`, move
  that context back to `GenerationContext` unless the caller can be narrowly
  fixed.
- Do not convert contexts that expose allocated pointers broadly across
  subsystem boundaries.

### GenerationContext as the Default Fast-Safe Choice

`GenerationContext` keeps normal chunk headers and supports `pfree()` while
still reducing some freelist churn for grouped lifetimes. It should be the
default replacement where the lifetime is mostly grouped but not airtight.

Good candidates:

- planner scratch contexts
- portal execution contexts
- SPI execution contexts
- DDL per-command scratch contexts

### Block Sizing

The fast fork should tune block sizes for the benchmark workload:

- Use larger initial blocks for known allocation-heavy query and DDL contexts to
  reduce repeated `malloc()` calls.
- Keep small contexts small enough that trivial queries do not waste excessive
  memory.
- Prefer context reset/delete over returning memory to the operating system
  during a single unit-test transaction.

Suggested initial policy:

- parser/planner scratch: `BumpContext`, start small, grow to default max
- executor query context: `GenerationContext`, default sizes
- per-tuple expression context: keep existing behavior unless profiling shows a
  clear win
- SPI exec/proc contexts: `GenerationContext`
- DDL scratch loops: `BumpContext` only for obvious per-loop reset contexts

### Optional External Allocator Experiment

After the memory-context swaps pass validation, evaluate linking the fast-fork
build against a faster process allocator such as `jemalloc` or `mimalloc`.

This should be a separate build option:

- Meson: `-Dtest_fast_malloc=jemalloc|mimalloc|system`
- Autoconf: optional follow-up

The external allocator should be treated as an experiment, not the primary
design, because PostgreSQL memory contexts already amortize many `malloc()`
calls. Measure it only after context-level improvements so the result is not
confused by avoidable `AllocSetContext` churn.

## Validation

The spec is satisfied when both validation paths pass on the current fork.

### Correctness Gate

Run the fast-fork validation script with fast memory contexts enabled:

```sh
./test-fastfork.sh --wipe
```

After `test_fast_memory_contexts` is wired into the script, this should
configure the build with:

```sh
-Dtest_fake_wal=true -Dtest_no_bg_jobs=true -Dtest_fast_memory_contexts=true
```

Passing means:

- The script exits successfully.
- All selected compatible tests pass.
- MVCC, rollback, savepoint, DDL, index, parser, planner, executor,
  resource-owner, SPI, and relation behavior remain correct.
- Any skipped tests are explicitly incompatible with the fast-fork feature set
  or missing local test dependencies.

Additional high-value checks:

- Run a validation build with memory context checking if feasible.
- Run a DDL-heavy pgbench script with multiple clients to exercise portals,
  SPI-like command execution, catalog lookup, and transaction abort cleanup.

### Performance Gate

Run the repeatable pgbench comparison:

```sh
python3 bench/compare_pgbench.py \
  --rounds 5 \
  --transactions 200 \
  --rows 200
```

After the benchmark harness knows about `test_fast_memory_contexts`, the fork
build should include:

```sh
-Dtest_fake_wal=true -Dtest_no_bg_jobs=true -Dtest_fast_memory_contexts=true
```

Passing means:

- Baseline and fork runs complete successfully.
- Results are written under `bench/results/`.
- The summary records the fork speed relative to the cached baseline build.

## Implementation Checklist

- Add Meson and autoconf build options for `test_fast_memory_contexts`.
- Add `USE_TEST_FAST_MEMORY_CONTEXTS` to generated config headers.
- Add fast context creation wrappers in the memory-context layer.
- Under normal builds, make wrappers call `AllocSetContextCreate`.
- Under fast-fork builds, route scratch/query/maybe-freeable wrappers to
  `BumpContext` or `GenerationContext` according to the policy above.
- Convert a small first batch of obvious short-lived contexts.
- Run validation; if a `BumpContext` conversion exposes `pfree()`/`repalloc()`
  misuse, downgrade that context to `GenerationContext`.
- Expand conversions based on profiler evidence from the benchmark.
- Teach `test-fastfork.sh` to configure `-Dtest_fast_memory_contexts=true`.
- Teach `bench/compare_pgbench.py` to configure the fork build with
  `-Dtest_fast_memory_contexts=true`.
- Optionally add `test_fast_malloc` as a separate measured experiment.
- Run the validation and benchmark gates above.

## Risks

- `BumpContext` does not support ordinary `pfree()`, `repalloc()`,
  `GetMemoryChunkContext()`, or `GetMemoryChunkSpace()` in production builds.
  Misusing it can turn a subtle lifetime assumption into a hard failure.
- Parser/planner/cached-plan pointers often cross context boundaries; only
  scratch contexts should be converted first.
- Larger block sizes can improve speed but increase peak memory for large test
  suites.
- External allocators may improve or hurt performance depending on workload and
  platform; keep them separately measurable.
- Overly broad replacement of `AllocSetContextCreate` would make upstream
  merging painful and risks changing long-lived cache behavior.
