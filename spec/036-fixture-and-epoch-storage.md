# `spec/036-fixture-and-epoch-storage.md`

# Fixture And Epoch Storage

## Summary

Add immutable fixture storage and per-test epoch overlays so tests can load a
schema and base fixture once, then run many isolated tests by dropping epoch
regions instead of replaying setup SQL.

This is the storage feature that most directly serves fastpg's project goal.
It depends on the arena/accounting model from
`032-storage-memory-ownership-accounting.md` and the snapshot model from
`034-storage-statement-snapshots.md`.

## Goals

```text
capture a fixture from current committed storage
run test sessions inside named epoch overlays
discard an epoch without walking the fixture base
allow multiple sessions to join the same epoch
isolate independent epochs from each other
keep fixture rows and indexes immutable
account fixture and epoch memory separately
record catalog and sequence state associated with a fixture
```

## Non-Goals

```text
do not persist fixtures to disk in the first pass
do not implement PostgreSQL database templates
do not copy whole fixture tables for each test
do not require epoch APIs for ordinary single-session SQL
do not implement production isolation levels
```

## Target Data Model

```rust
struct Fixture {
    id: FixtureId,
    name: String,
    catalog_generation: CatalogGeneration,
    storage_generation: StorageGeneration,
    sequence_snapshot: SequenceSnapshot,
    region: StorageRegionId,
    indexes: FixtureIndexSet,
}

struct Epoch {
    id: EpochId,
    name: String,
    fixture_id: FixtureId,
    region: StorageRegionId,
    relation_deltas: RelationDeltaMap,
    index_deltas: IndexDeltaMap,
    sequence_delta: SequenceDelta,
}

struct SessionStorage {
    current_epoch: Option<EpochId>,
    transaction: Option<TransactionStorage>,
}
```

## Visibility

Reads inside an epoch compose:

```text
session transaction overlays
epoch relation/index deltas
fixture immutable base
```

Reads outside an epoch use ordinary committed storage.

Independent epochs never see each other's writes. Multiple sessions in the same
epoch see committed epoch writes according to READ COMMITTED statement
snapshots.

## Fixture Capture

Fixture capture freezes the current committed state:

```text
row regions become immutable fixture regions
primary-key and unique indexes become immutable fixture index regions
catalog generation is recorded
storage generation is recorded
sequence values are recorded
memory accounting moves bytes into fixture accounting
```

The first implementation may copy committed rows into a fixture region.
Later implementations may freeze or share chunks when ownership rules allow it.

## Epoch Lifecycle

```sql
SELECT fastpg_create_fixture('base');
SELECT fastpg_start_epoch('test_123', 'base');
SELECT fastpg_join_epoch('test_123');
SELECT fastpg_leave_epoch();
SELECT fastpg_finish_epoch('test_123');
```

Equivalent wire/session APIs are allowed, but SQL-callable functions are useful
for test harnesses.

Rules:

```text
start_epoch creates an empty epoch region over a fixture
join_epoch sets SessionStorage.current_epoch
leave_epoch clears current_epoch without discarding data
finish_epoch requires no active sessions or force=true
finish_epoch drops the epoch region and all deltas
```

## Writes In Epochs

Committed transactions inside an epoch merge into the epoch overlay, not the
fixture base.

```text
INSERT writes new rows into epoch region
UPDATE writes replacement rows into epoch region
DELETE records tombstones in epoch delta
TRUNCATE records relation-wide tombstone in epoch delta
unique checks see fixture base plus current epoch plus current transaction
```

## Acceptance Tests

```text
fixture capture preserves loaded rows
epoch A writes are invisible to epoch B
two sessions in epoch A see each other's committed writes
ROLLBACK inside an epoch drops only the transaction overlay
finish_epoch drops epoch rows and index deltas
fixture rows remain visible to later epochs
fixture discard updates memory accounting
sequence state is restored or advanced according to sequence policy
```

## Performance Tests

```text
capture fixture with 100 small tables
start and finish 1,000 empty epochs
epoch discard after 500 inserted rows
parallel epochs over one fixture
schema plus fixture plus N test loop compared with replaying setup SQL
```

## Migration Steps

```text
1. Add Fixture and Epoch registries under Storage.
2. Add current_epoch to SessionStorage if not already present.
3. Implement fixture capture using StorageRegion ownership.
4. Implement start/join/leave/finish epoch APIs.
5. Route committed transaction writes into epoch overlays when joined.
6. Make visibility read fixture plus epoch plus transaction layers.
7. Add unique-index and sequence integration.
8. Add memory accounting and benchmark output for fixture/epoch bytes.
```
