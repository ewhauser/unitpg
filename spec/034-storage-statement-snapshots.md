# `spec/034-storage-statement-snapshots.md`

# Storage Statement Snapshots

## Summary

Add explicit storage commit generations and statement snapshots so reads have a
clear READ COMMITTED boundary without implementing PostgreSQL MVCC tuple
headers.

fastpg does not need heap tuple MVCC, CLOG, ProcArray, or vacuum. It does need
ordinary test behavior where committed writes become visible at statement
boundaries, uncommitted writes stay session-local, and a running scan has a
stable view.

## Goals

```text
support READ COMMITTED statement snapshots
give each committed storage merge a monotonic generation
make scans read one stable committed generation
keep own transaction writes visible to the owning session
avoid PostgreSQL MVCC tuple headers and vacuum
compose with fixture and epoch overlays
make snapshot generation observable in pgcore lane metrics
```

## Non-Goals

```text
do not implement SERIALIZABLE isolation
do not implement PostgreSQL snapshots byte-for-byte
do not expose xmin/xmax/cmin/cmax semantics
do not implement row locks or predicate locks
do not add vacuum or dead-tuple cleanup
```

## Current Problem

Current reads compose committed rows and the current session overlay at callback
time. Scans materialize rows when the scan begins, which gives local scan
stability, but the storage model does not name the committed generation being
read.

That is enough for current tests, but later fixture, epoch, and multi-session
behavior needs explicit answers:

```text
which committed writes can this statement see?
does a second statement see a different generation?
which generation did a prepared execution use?
which epoch overlay was layered over the committed base?
```

## Target Data Model

```rust
struct CommittedStorage {
    generation: StorageGeneration,
    relations: RelationMap,
    indexes: IndexMap,
}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
struct StorageGeneration(u64);

struct StorageSnapshot {
    committed_generation: StorageGeneration,
    fixture_id: Option<FixtureId>,
    epoch_id: Option<EpochId>,
}

struct StatementStorageContext {
    snapshot: StorageSnapshot,
    session_storage: Arc<Mutex<SessionStorage>>,
}
```

Every statement gets a snapshot before entering PostgreSQL execution. Under
READ COMMITTED, each statement gets the latest committed generation at statement
start.

## Visibility Rules

Reads compose layers in this order:

```text
current savepoint overlays, newest first
current transaction overlay
current epoch overlay
fixture base or committed generation from the statement snapshot
```

Rules:

```text
own uncommitted writes are visible
other sessions' uncommitted writes are invisible
commits by other sessions after statement start are invisible to that statement
next statement sees the latest committed generation
rollback drops overlays and does not create a committed generation
commit publishes exactly one new committed generation
```

## Scan Behavior

The table AM may keep materializing visible rows at scan start for now. The
important change is that the materialization must be based on a named
`StorageSnapshot`.

```text
scan_begin captures statement snapshot generation
scan_next reads only rows visible to that scan snapshot
scan_reset rewinds the same scan view
scan_end releases scan materialization
```

## Conflict Policy

Initial fastpg behavior may use coarse commit locking. If two sessions update
the same row concurrently, fastpg may choose either:

```text
last writer wins for READ COMMITTED test compatibility
or a clear conflict error if the write set detects overlap
```

The chosen policy must be explicit and tested. This spec does not require
PostgreSQL row-lock behavior.

## Acceptance Tests

```text
session B does not see session A uncommitted insert
session B sees session A committed insert on its next statement
scan opened before another session commits does not see the later row
scan_reset does not change the scan generation
ROLLBACK does not bump committed generation
COMMIT bumps committed generation exactly once
own uncommitted update is visible inside the transaction
```

## Migration Steps

```text
1. Add StorageGeneration to committed storage.
2. Create StorageSnapshot at statement entry.
3. Store snapshot on execution context and scan handles.
4. Make visible row iteration accept StorageSnapshot.
5. Increment generation on commit merge only.
6. Add tests for multi-session READ COMMITTED visibility.
7. Add lane metrics for storage generation used.
```
