# `spec/032-storage-memory-ownership-accounting.md`

# Storage Memory Ownership And Accounting

## Summary

Replace the current row/value ownership model with explicit storage arenas and
memory accounting before building more fixture, epoch, snapshot, unique-index,
or sequence state on top of it.

fastpg is optimized for tiny test tables, but test suites still load real
fixtures, by-reference values, and repeated per-test overlays. Storage needs a
clear ownership model so rollback, savepoint rollback, fixture discard, and
epoch discard are mostly arena drops rather than scattered `Vec` and `Box`
cleanup.

This spec defines the memory substrate for later storage specs. It does not
change SQL semantics by itself.

## Goals

```text
make every stored row and by-reference value have an explicit owner
make rollback and savepoint rollback drop transaction-owned memory directly
make fixture and epoch discard drop whole storage regions
avoid C-owned pointers in long-lived Rust storage
make memory usage observable and limitable by region
support small-table workloads without per-row allocator noise
provide stable row references for primary-key and unique indexes
preserve current virtual CTID behavior
```

## Non-Goals

```text
do not implement PostgreSQL heap pages
do not implement physical TOAST tables
do not implement shared buffers
do not implement WAL or crash recovery
do not implement vacuum or free-space maps
do not add a Rust planner
do not optimize for long-lived production workloads
```

## Current Problem

`fastpg-storage` currently stores rows and by-reference payloads in ordinary
owned containers. `RelationRows` owns committed payloads, `TransactionOverlay`
owns row segments, and scans clone visible rows into `ScanState`.

That works for current correctness tests, but it makes the next features harder:

```text
fixture capture needs immutable base storage
epoch discard needs near-O(1) memory release
savepoint rollback should drop only nested allocations
indexes need stable row identifiers without owning row bytes
memory limits need region-level accounting
large values need Rust ownership without physical TOAST
```

The important thing to avoid is building fixture and epoch behavior on top of a
model where every feature grows its own ad hoc `Vec<Box<[u8]>>` ownership.

## Workload Assumptions

```text
common table size: fewer than 20-30 rows
larger fixture table size: roughly 100-500 rows
larger outliers: possible, but not the default design center
schema and fixture load may create many small tables
test reset should be cheaper than replaying schema and fixture SQL
```

The design should favor simple region allocation and cheap drops over complex
per-row reclamation. Memory reuse can be added later if profiles demand it.

## Ownership Model

All storage memory belongs to one of these regions:

```text
Committed:
  shared database state outside a fixture or epoch

Fixture:
  immutable captured base rows and indexes

Epoch:
  copy-on-write rows, deletes, updates, and index deltas for one test epoch

Transaction:
  uncommitted rows, updates, deletes, and index deltas for one session

Savepoint:
  nested transaction allocations that can be dropped independently

Scan:
  short-lived row references or materialized row handles for one executor scan
```

Each region owns an arena:

```rust
struct StorageRegion {
    id: StorageRegionId,
    kind: StorageRegionKind,
    arena: RowArena,
    accounting: RegionAccounting,
}

enum StorageRegionKind {
    Committed,
    Fixture(FixtureId),
    Epoch(EpochId),
    Transaction(TxId),
    Savepoint(TxId, SavepointId),
    Scan(ScanId),
}
```

Rows and values never outlive their owning region. When a transaction commits,
surviving rows are promoted into the parent region. When it rolls back, its
region is dropped.

## Row Representation

Rows should move from clone-heavy cell vectors toward stable row records:

```rust
struct StoredRow {
    row_id: RowId,
    relation_id: RelationId,
    values: RowValues,
    size_bytes: usize,
}

struct RowValues {
    nulls: NullBitmap,
    datums: Box<[StoredDatum]>,
}

enum StoredDatum {
    ByValue(usize),
    ByRef(ValueRef),
}

struct ValueRef {
    region_id: StorageRegionId,
    offset: ArenaOffset,
    len: usize,
}
```

The first implementation may keep `Vec<Cell>` internally, but it must route all
by-reference bytes through `RowArena` and expose an ownership boundary that can
be replaced without changing fixture, epoch, or index code.

## Row Arena

The arena API should be small:

```rust
struct RowArena {
    chunks: Vec<ArenaChunk>,
    bytes_allocated: usize,
}

impl RowArena {
    fn alloc_bytes(&mut self, bytes: &[u8]) -> ValueRef;
    fn alloc_row(&mut self, row: RowValues) -> RowRef;
    fn checkpoint(&self) -> ArenaCheckpoint;
    fn rewind_to(&mut self, checkpoint: ArenaCheckpoint);
    fn clear(self);
}
```

Rules:

```text
inserts allocate in the current transaction/savepoint region
updates allocate the replacement row in the current transaction/savepoint region
COPY allocates rows in the current transaction region
commit promotes surviving rows into the epoch or committed region
rollback drops or rewinds the current region
savepoint rollback rewinds only the savepoint region
fixture capture freezes rows into a fixture region
epoch discard drops the epoch region
```

Promotion may initially copy row bytes. Zero-copy promotion is allowed later if
the arena representation can transfer whole chunks safely.

## Large Values

fastpg must not implement PostgreSQL physical TOAST tables for the current
project goal, but it still needs safe in-memory large-value ownership.

Policy:

```text
store varlena payload bytes in Rust-owned arenas
return PostgreSQL-compatible varlena bytes to executor slots
never store executor-owned varlena pointers in long-lived storage
never expose ValueRef after its owning region is dropped
reject values larger than the configured per-row or per-region limit
```

The current table AM may continue to report that no TOAST table is needed.
`relation_fetch_toast_slice` should remain unsupported unless a later spec adds
a logical large-value slicing API.

## Memory Accounting

Track memory at multiple levels:

```rust
struct StorageAccounting {
    committed_bytes: AtomicUsize,
    fixture_bytes: AtomicUsize,
    epoch_bytes: AtomicUsize,
    transaction_bytes: AtomicUsize,
    scan_bytes: AtomicUsize,
}

struct RegionAccounting {
    rows: usize,
    row_bytes: usize,
    byref_bytes: usize,
    index_bytes: usize,
    overhead_bytes: usize,
}
```

Initial limits:

```text
max committed bytes
max fixture bytes
max bytes per epoch
max bytes per transaction
max bytes per row
max scan materialization bytes
```

A limit violation should return a PostgreSQL-shaped error:

```text
SQLSTATE: 54000
message: fastpg memory limit exceeded for transaction storage
```

## Scan Memory

The current scan path materializes visible rows into a `ScanState`. That is
acceptable for the small-table design center, but it should be accounted.

Rules:

```text
scan materialization must not borrow from dropped transaction regions
scan memory counts against scan_bytes or transaction_bytes
scan end releases scan-owned materialization
large scans may later switch to iterator handles instead of row clones
```

The first implementation may keep materialized scans, with a guardrail that
reports how much memory they allocate.

## Index References

Indexes should reference stable row identities, not own row bytes:

```rust
struct IndexEntry {
    key: IndexKey,
    row_id: RowId,
    visibility: VisibilityRef,
}
```

Index keys may copy only the key bytes needed for lookup. Full row values stay
owned by row regions.

This is required so the unique-index and fixture/epoch specs can drop index
delta regions without walking unrelated row storage.

## Observability

Expose counters through debug trace or benchmark output:

```text
storage committed bytes
storage fixture bytes
storage epoch bytes
storage transaction bytes
storage scan bytes
rows allocated by region
by-reference bytes copied
arena chunks allocated
arena rewinds
arena drops
memory-limit rejections
```

The trace should be quiet by default.

## Acceptance Tests

```text
inserted by-reference values survive executor input buffer mutation
ROLLBACK drops transaction-owned rows and by-reference bytes
ROLLBACK TO SAVEPOINT drops only nested rows and bytes
COMMIT promotes surviving rows into committed or epoch storage
scan end releases scan materialization accounting
fixture discard drops all fixture-owned row and index memory
epoch discard drops all epoch-owned row and index memory
memory limit violation returns SQLSTATE 54000
primary-key index entries reference row IDs rather than row payload ownership
```

## Performance Tests

```text
rollback of 1 row
rollback of 500 rows
rollback of 10,000 rows
savepoint rollback of nested by-reference values
fixture capture of 100 small tables
epoch discard after many small table writes
COPY of 500 text rows
```

The first performance target is not maximum insert throughput. It is making
rollback, savepoint rollback, and epoch discard scale with the region being
dropped rather than the whole database.

## Migration Steps

```text
1. Add RowArena, StorageRegion, and accounting structs behind the current API.
2. Route by-reference cell copies through RowArena.
3. Track row, by-reference, scan, and index bytes per region.
4. Add transaction and savepoint arena checkpoints.
5. Make rollback and savepoint rollback drop or rewind owned regions.
6. Make commit promote surviving rows through one storage API.
7. Change scans to account for materialized row memory.
8. Add configurable memory limits and SQLSTATE mapping.
9. Refactor primary-key indexes to store row IDs plus copied key bytes only.
10. Use this region model as the base for fixture and epoch storage specs.
```

## Open Questions

```text
Should committed promotion initially copy rows or transfer arena chunks?
Should scan materialization live in a scan region or the statement region?
Should memory limits default to unlimited or conservative test-friendly values?
Should ValueRef encode region generation to catch stale references in debug builds?
Should very large values use Arc<[u8]> before a full arena implementation?
```
