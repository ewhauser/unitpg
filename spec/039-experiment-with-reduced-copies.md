# `spec/039-experiment-with-reduced-copies.md`

# Experiment With Reduced Copies

## Summary

Experiment with reducing data copies between the PostgreSQL wire protocol,
Rust COPY handling, fastpg storage, and PostgreSQL executor slots.

The first target is not a global zero-copy promise. PostgreSQL's normal SQL
path intentionally turns client bytes into typed Datums before table AM insert.
fastpg can reduce copies in that path, but it cannot make ordinary
`INSERT`/`UPDATE` wire bytes arrive in storage without passing through parser,
analyzer, planner, executor, type input, and `TupleTableSlot` machinery.

The best first target is `COPY FROM STDIN` in the Rust server. The COPY
statement still uses PostgreSQL core to identify a `CopyIn` target, but the
subsequent `CopyData` messages are handled by Rust wire code. That path can
hand storage an owned, sliceable COPY buffer or directly build storage-owned
arena values instead of first building temporary strings and temporary Datum
payloads.

The second target is scan output. fastpg already uses virtual tuple slots for
its table AM, which is the executor-friendly shape for Datum arrays. Storage
can return Datums that point into fastpg-owned arena memory as long as those
Datums use normal PostgreSQL in-memory representations and remain valid for
the slot's lifetime.

This spec defines an experiment, not a required architecture migration.

## Goals

```text
measure every meaningful copy on the fastpg write and read paths
remove avoidable temporary COPY String and Box payload copies
let COPY parsing operate on bytes before converting to Rust str
let storage build final row values directly in storage-owned regions
support arena-backed Datums for executor virtual slots
preserve normal PostgreSQL Datum layout and type semantics
keep ordinary PostgreSQL builds unchanged
make all optimizations opt-in or feature-gated until measured
fall back to safe materialization when a value cannot be borrowed
compare correctness and throughput against the existing harnesses
```

## Non-Goals

```text
do not change global PostgreSQL Datum semantics
do not invent tagged Datums or non-PostgreSQL varlena pointers
do not make normal SQL INSERT bypass parser, planner, or executor
do not store C executor-owned pointers in long-lived Rust storage
do not implement PostgreSQL heap pages, shared buffers, WAL, or TOAST tables
do not require every query operator to understand a new storage-only value kind
do not optimize unsupported production workloads before test workloads
```

## Current Copy Paths

### General SQL Insert And Update

Ordinary SQL writes go through the PostgreSQL core execution path:

```text
client message bytes
  -> PostgreSQL wire/query handling
  -> raw parser
  -> analyzer and rewriter
  -> planner
  -> executor
  -> type input and expression evaluation
  -> TupleTableSlot with Datum/isnull arrays
  -> fastpg table AM insert/update
  -> Rust storage row copy
```

The current fastpg table AM extracts slot values with `slot_getallattrs()`,
allocates C arrays for values, null flags, by-value flags, and value lengths,
then calls `fastpg_rust_relation_insert()`.

For by-reference values, Rust storage copies bytes out of the executor Datum
into storage-owned memory. That copy is required unless storage can safely take
ownership of the executor allocation, which it generally cannot. Executor
memory contexts are statement-scoped and do not match fastpg transaction,
fixture, epoch, or committed storage lifetimes.

General SQL writes can still be improved:

```text
avoid avoidable C scratch allocations
copy by-reference values once into the final storage arena
avoid intermediate Box payloads
special-case common by-value types
instrument how often the executor asks to materialize slots
```

But this path should not be treated as wire-to-storage zero copy.

### Rust COPY FROM STDIN

COPY has a better boundary:

```text
COPY statement
  -> PostgreSQL core prepare/execute path
  -> QueryExecution::CopyIn target

CopyData frames
  -> Rust wire handler
  -> ActiveCopy pending buffer
  -> storage copy_text_line
  -> temporary Datum payloads
  -> fastpg_rust_relation_insert
  -> storage row copy
```

The current Rust COPY implementation stores pending input as a `String`,
requires every chunk to be valid UTF-8 immediately, splits lines as `&str`,
then `copy_text_line()` builds temporary Datum payloads for by-reference values.
Storage then copies those by-reference payloads again into row-owned memory.

The experiment should replace that with a byte-oriented COPY builder:

```text
CopyData bytes
  -> owned COPY buffer or batch buffer
  -> byte ranges for complete rows and fields
  -> direct parse for by-value fields
  -> direct storage-region allocation for final by-reference values
  -> row insert without temporary Datum payload ownership
```

### Scan And Executor Output

fastpg table scans currently store values into virtual slots:

```text
Rust storage row
  -> FFI values/null arrays
  -> slot->tts_values / slot->tts_isnull
  -> ExecStoreVirtualTuple
```

Virtual slots are already the least invasive slot shape for copy reduction.
They hold Datum and null arrays and avoid heap tuple construction until another
executor path explicitly asks for materialization or a heap/minimal tuple copy.

The opportunity is to make by-reference Datums point at storage-owned,
PostgreSQL-compatible memory:

```text
by-value Datum:
  stored directly in slot->tts_values

by-reference Datum:
  pointer into a pinned fastpg storage region
  bytes must match normal PostgreSQL representation for the type
  lifetime must cover executor use of the slot
```

When PostgreSQL calls `ExecMaterializeSlot`, `ExecCopySlotHeapTuple`, or other
copying APIs, fastpg should allow PostgreSQL to copy. The experiment should
measure those fallback copies rather than trying to remove them first.

## Copy Accounting

Add counters before changing representation so that the experiment has a
baseline:

```text
copy_wire_chunk_bytes
copy_pending_buffer_bytes
copy_line_materialization_bytes
copy_field_materialization_bytes
copy_storage_byref_bytes
copy_scan_materialization_bytes
copy_slot_materialization_bytes
copy_heap_tuple_bytes
copy_minimal_tuple_bytes
copy_fallback_count
copy_borrowed_field_count
copy_direct_arena_field_count
```

The counters should be visible in benchmark JSON and optionally in a debug
trace. They should be quiet by default.

The first milestone can approximate heap/minimal tuple byte counts with tuple
size estimates rather than invasive executor hooks. If later profiles show
slot materialization is material, add narrow instrumentation around fastpg slot
paths or a fastpg-specific slot implementation.

## Ownership Contract

The experiment must preserve this boundary:

```text
PostgreSQL-owned memory:
  safe only for the current executor or memory-context lifetime

fastpg storage-owned memory:
  safe for transaction, savepoint, fixture, epoch, committed, or scan lifetime

COPY input-owned memory:
  safe only while storage pins the owning COPY buffer or has copied into a
  storage region
```

Rules:

```text
never keep a pointer into a pgwire frame after the frame owner can drop it
never keep a pointer into a PostgreSQL memory context in long-lived storage
never expose a storage pointer to PostgreSQL after its region can be dropped
never return a by-reference Datum unless the pointed bytes are PostgreSQL ABI
compatible for that type
materialize when escaping, transcoding, type input, or lifetime makes borrowing unsafe
```

For varlena types, a Datum pointer must point to a normal PostgreSQL varlena
object. A raw COPY field slice is not itself a valid `text` Datum because it
does not include the varlena header. The experiment has two possible policies:

```text
direct arena policy:
  allocate final varlena bytes directly in the row arena and copy the field
  payload once from the COPY buffer

slice-backed policy:
  store a storage-internal buffer slice and materialize a varlena Datum only
  when PostgreSQL needs the value as a Datum
```

The direct arena policy is simpler and should be tried first. It reduces copies
without introducing a second logical value representation. The slice-backed
policy is a later experiment for COPY-heavy workloads where inserted rows are
rarely read back through PostgreSQL expression machinery.

## Datum And Slot Policy

The experiment should not hack `Datum` globally. A `Datum` should remain what
PostgreSQL expects:

```text
by-value types:
  normal value bits in the Datum word

by-reference fixed-length types:
  pointer to ABI-compatible bytes with the expected alignment and lifetime

varlena types:
  pointer to a normal PostgreSQL varlena object

cstring types:
  pointer to a NUL-terminated byte sequence
```

The useful hack is at the producer boundary:

```text
COPY producer:
  parse bytes and build final storage-owned values directly

storage scan producer:
  return Datums pointing into pinned storage memory when safe

table AM producer:
  fill virtual slots without forming heap tuples unless required

optional custom slot producer:
  lazily expose storage rows through TupleTableSlotOps if profiling proves
  virtual slots still copy too much
```

Any path that needs a normal PostgreSQL-owned value can request
materialization. That fallback is acceptable if it is measured and does not
dominate the target workloads.

## Experiment 1: Byte-Oriented COPY Builder

Replace `ActiveCopy`'s `String` pending buffer with a byte buffer:

```rust
struct ActiveCopy {
    target: CopyTarget,
    pending: Vec<u8>,
    rows: usize,
}
```

COPY processing should scan for newline bytes, trim optional carriage returns,
and pass complete line byte ranges to storage without converting the whole line
to a Rust `String`.

Introduce a storage COPY API that is distinct from the Datum-array insert API:

```rust
pub fn copy_text_line_bytes(table: &str, line: CopyTextLine<'_>) -> Result<bool, String>;

pub struct CopyTextLine<'a> {
    pub bytes: &'a [u8],
}
```

The first implementation may still call the existing insert machinery after it
builds final values, but it should avoid temporary `String` and `Box<[u8]>`
payloads for common cases.

Field parsing rules:

```text
\N maps to NULL
by-value integer types parse directly from field bytes
unescaped text-like fields allocate final varlena bytes directly in row arena
escaped fields use a scratch decoder and then allocate final varlena bytes
unsupported encodings or types fall back to current safe materialization
```

The row builder should make ownership explicit:

```rust
struct RowBuildTarget<'a> {
    relation_id: u32,
    storage_region: &'a mut StorageRegion,
    cells: Vec<Cell>,
}

impl<'a> RowBuildTarget<'a> {
    fn push_null(&mut self);
    fn push_by_value(&mut self, value: usize);
    fn push_varlena_from_copy_field(&mut self, bytes: &[u8]);
    fn push_materialized_varlena(&mut self, bytes: &[u8]);
}
```

`push_varlena_from_copy_field()` still copies field payload bytes into the
storage arena under the direct arena policy. The win is that it copies once
into the final owner instead of first building a temporary Datum payload and
then copying that payload into storage.

Acceptance tests:

```text
COPY text with unescaped fields inserts correct rows
COPY text with escaped tabs/newlines inserts correct rows
COPY text split across CopyData frames inserts correct rows
COPY text with \N preserves NULL behavior
invalid UTF-8 or invalid type literals return PostgreSQL-shaped errors
temporary COPY buffers can be dropped after insert without corrupting rows
copy_field_materialization_bytes decreases for unescaped text COPY
```

## Experiment 2: Arena-Backed Datums In Virtual Slots

Keep `TTSOpsVirtual` as the default table AM slot callback and change storage
output so by-reference Datums point into pinned fastpg storage memory.

The table AM scan path should establish a scan lifetime:

```text
scan begin pins visible row regions or creates a scan region
scan next returns Datum values and null flags borrowed from that lifetime
slot stores those Datums with ExecStoreVirtualTuple
scan end releases the pin or scan region
```

Storage may still materialize rows into a scan-owned region if the source row
could be invalidated by rollback, savepoint rollback, fixture discard, or epoch
discard while the executor is still reading. That materialization must be
accounted as scan copy bytes.

Acceptance tests:

```text
SELECT of text inserted by COPY returns correct values
scan values survive until the executor clears or replaces the slot
scan end releases scan-region accounting
ROLLBACK cannot invalidate values still needed by an active scan
ExecMaterializeSlot on a fastpg virtual slot still returns correct values
heap tuple copy fallback still returns correct values
```

## Experiment 3: Optional Fastpg TupleTableSlotOps

Only after the first two experiments are measured, try a custom slot
implementation behind a build flag:

```c
typedef struct FastPgTupleTableSlot
{
	TupleTableSlot base;
	uint64 scan_handle;
	uint64 row_id;
	uint16 loaded_attrs;
	bool owns_scan_pin;
} FastPgTupleTableSlot;
```

The custom slot would be responsible for:

```text
lazy getsomeattrs from a storage row handle
clear/release of scan pins or row handles
tableoid and CTID system attributes
materialize into the slot memory context when required
copyslot from arbitrary source slots
copy_heap_tuple and copy_minimal_tuple fallbacks
```

This is allowed to be ugly and local. It must not change executor behavior for
normal PostgreSQL builds or non-fastpg relations.

Risks:

```text
executor code has fast paths for built-in slot ops
virtual slots already avoid many heap tuple copies
custom slots increase C ABI and lifetime surface area
system attributes and row identity need careful behavior
projection, hashing, sorting, tuplestore, and SPI may force materialization
```

Success criteria:

```text
custom slots reduce measured copies or CPU time on real fastpg profiles
fallback materialization count remains explainable
all existing regression and pgbench harnesses pass
no new lifetime hazards appear under rollback or savepoint rollback tests
```

If the custom slot does not produce a measurable win, delete it and keep the
byte-oriented COPY and arena-backed virtual slot work.

## Experiment 4: General SQL Write Scratch Reduction

General SQL writes still receive executor slots. The first write-side cleanup
should reduce scratch allocation, not bypass the executor.

Possible changes:

```text
reuse per-relation scratch arrays for values/isnull/byval/value_lens
use stack allocation for small natts counts
pass attribute metadata once per relation instead of recomputing every insert
copy by-reference Datums directly into the final storage region
account bytes copied out of executor-owned Datums
```

Do not store executor Datum pointers beyond the insert/update call. The storage
copy is the ownership boundary for general SQL writes.

## Benchmarks

The experiment should use both microbenchmarks and the existing harnesses:

```text
COPY 500 rows of int/text values
COPY rows split at every possible byte offset around tabs and newlines
COPY escaped text-heavy rows
INSERT 500 rows through normal SQL
SELECT all rows after COPY
SELECT primary-key row after COPY
ROLLBACK after COPY
pgbench simple-update Rust server smoke
strict Rust-server regression comparison
```

Benchmark output should include:

```text
rows inserted
COPY bytes received
storage by-reference bytes copied
temporary field bytes allocated
scan materialization bytes
slot materialization count
heap/minimal tuple copy count
transaction throughput
latency distribution where available
```

## Validation Commands

```sh
python3 -m py_compile benches/pgbench_compare.py benches/open_latest_profile.py benches/regression_compare.py benches/upstream_regression_inventory.py
cargo test -p fastpg-storage
make -C benches pgbench-compare-rust-server SCALE=1 TRANSACTIONS=1 RUNS=1
make -C benches regression-compare-rust-server
```

Before declaring a reduced-copy variant successful, also run:

```sh
make -C benches validate-rust-server
```

## Migration Steps

```text
1. Add copy accounting counters without changing behavior.
2. Add benchmark output for copy counters.
3. Replace ActiveCopy pending String with a byte buffer.
4. Add copy_text_line_bytes beside copy_text_line.
5. Parse COPY text fields from bytes for current supported types.
6. Build by-reference COPY values directly in the storage row region.
7. Remove temporary COPY Box payloads for the unescaped common path.
8. Make scan output return arena-backed Datums where lifetime permits.
9. Add scan-region pins or materialization accounting where lifetime requires.
10. Profile pgbench and COPY-heavy fixtures.
11. Try custom FastPgTupleTableSlotOps only if profiles still show slot copies.
12. Keep, narrow, or delete the custom slot experiment based on measured wins.
```

## Open Questions

```text
Should direct arena COPY remain the permanent representation for varlena values?
Is a slice-backed storage-only value worth the complexity for COPY-heavy tests?
Where should copy counters live: storage, session, wire, or benchmark collector?
Should COPY decoding support only server_encoding UTF-8 at first?
Can scan pins be cheap enough, or should scans keep materializing visible rows?
Which executor paths force slot materialization in current regression coverage?
How often do ORMs read back COPY-loaded fixture rows before rollback/reset?
```
