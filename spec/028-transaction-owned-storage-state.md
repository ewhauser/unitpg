# `spec/028-transaction-owned-storage-state.md`

# Transaction-Owned Rust Storage State

## Summary

Rust storage must keep committed database state shared, but transaction state
must be owned by the session and current transaction.

The current `fastpg-storage` global `OnceLock<Mutex<StorageState>>` is a
bootstrap shortcut. It prevents correct client isolation, makes rollback too
global, and hides which session owns uncommitted writes. Replace it with shared
committed storage plus session-owned transaction overlays.

## Goals

```text
committed rows are shared process-wide
uncommitted inserts, updates, deletes, and index changes are owned by one transaction
rollback drops the owning transaction overlay without scanning global state
savepoint rollback drops only nested overlay state
commit merges the transaction overlay into shared committed storage atomically
C tableam callbacks can find the active session/transaction storage context
```

## Non-Goals

```text
do not add WAL
do not add shared buffers
do not add heap pages
do not add vacuum
do not add PostgreSQL process shared memory
do not make the Rust server single-client
```

## Current Problem

`fastpg-storage` currently uses process-global mutable state:

```rust
static STORAGE: OnceLock<Mutex<StorageState>> = OnceLock::new();

fn storage() -> &'static Mutex<StorageState> {
    STORAGE.get_or_init(|| Mutex::new(StorageState::default()))
}
```

This makes storage easy for C callbacks to reach, but it collapses server,
database, session, transaction, and subtransaction ownership into one mutex.

The target architecture needs:

```text
shared committed state
session-owned uncommitted state
transaction-owned rollback memory
explicit current execution context for C callbacks
```

## Target Data Model

```rust
struct Storage {
    committed: RwLock<CommittedStorage>,
    relation_locks: RelationLockTable,
    metrics: StorageMetrics,
}

struct CommittedStorage {
    relations: HashMap<StorageId, CommittedRelation>,
    indexes: HashMap<IndexId, CommittedIndex>,
    commit_generation: u64,
}

struct SessionStorage {
    session_id: SessionId,
    epoch: Option<EpochHandle>,
    transaction: Option<TransactionStorage>,
}

struct TransactionStorage {
    tx_id: TxId,
    snapshot: StorageSnapshot,
    root: StorageOverlay,
    subtransactions: Vec<StorageOverlay>,
    arena: TransactionArena,
    write_set: WriteSet,
}

struct StorageOverlay {
    inserted_rows: RowMap,
    updated_rows: RowUpdateMap,
    deleted_rows: RowDeleteSet,
    index_delta: IndexDeltaMap,
}
```

`Storage` is shared. `SessionStorage` is held by `SessionState`.
`TransactionStorage` exists only while a transaction is active.

## Visibility Model

Reads compose layers in this order:

```text
transaction subtransaction overlays, newest first
transaction root overlay
current epoch overlay, if any
committed storage snapshot
fixture base snapshot, if any
```

Within one transaction:

```text
own writes are visible
rolled-back savepoint writes disappear
deleted rows are hidden
updates shadow older row versions
```

Across sessions:

```text
uncommitted writes are invisible
committed writes become visible according to the isolation policy
independent epoch writes remain isolated
```

Initial isolation is READ COMMITTED unless a later transaction spec says
otherwise. Each statement can acquire a fresh committed generation plus the
session's own transaction overlay.

## Transaction Operations

### BEGIN

```rust
fn begin(&mut self, committed: &Storage) {
    self.transaction = Some(TransactionStorage {
        tx_id: TxId::next(),
        snapshot: committed.snapshot_read_committed(),
        root: StorageOverlay::default(),
        subtransactions: Vec::new(),
        arena: TransactionArena::new(),
        write_set: WriteSet::default(),
    });
}
```

### SAVEPOINT

```rust
fn push_savepoint(&mut self) {
    self.transaction.subtransactions.push(StorageOverlay::default());
}
```

### ROLLBACK TO SAVEPOINT

```rust
fn rollback_to_savepoint(&mut self, savepoint: SavepointId) {
    self.transaction.drop_nested_overlays_after(savepoint);
}
```

Dropping a savepoint overlay also drops the arena allocations scoped to that
overlay if arenas are segmented by savepoint.

### COMMIT

```rust
fn commit(&mut self, storage: &Storage) -> Result<(), StorageError> {
    let tx = self.transaction.take().unwrap();
    storage.merge(tx)
}
```

`merge` must be atomic with respect to other commits touching the same relation
or index. A coarse storage write lock is acceptable initially, but the API must
not require global locking forever.

### ROLLBACK

```rust
fn rollback(&mut self) {
    self.transaction.take();
}
```

Rollback is a drop operation. It must not walk committed storage to undo writes.

## Transaction Arena

Transaction-owned rows and by-reference values should be allocated from a
transaction arena:

```rust
struct TransactionArena {
    root: BumpArena,
    savepoints: Vec<BumpArena>,
}
```

Rules:

```text
inserted rows live in the current transaction or savepoint arena
updated row versions live in the current transaction or savepoint arena
rollback drops the relevant arena segment
commit copies or promotes surviving row data into committed storage ownership
large values may use Arc<[u8]> if promotion without copy is needed later
```

The first implementation may use ordinary owned `Vec`/`Box` values behind the
same ownership API. The API should still make rollback a drop of transaction
owned state.

## C TableAM Callback Context

PostgreSQL executor table access reaches Rust through C tableam callbacks.
Those callbacks cannot rely on one global current transaction.

Introduce an execution-local storage context:

```rust
struct ExecutionStorageContext {
    session_id: SessionId,
    storage: Arc<Storage>,
    session_storage: Arc<Mutex<SessionStorage>>,
    statement_snapshot: StorageSnapshot,
}
```

The pgcore lane enters execution with this context:

```rust
pgcore_lane.run(session, |cx| {
    fastpg_storage_enter_execution(storage_context);
    let result = postgres_executor_execute(cx);
    fastpg_storage_exit_execution();
    result
})
```

Because the initial pgcore lane is single-threaded, the C side may use a
thread-local or lane-local pointer while inside executor callbacks. The pointer
must be set and cleared by RAII guards so errors cannot leave stale context.

When multiple pgcore lanes are introduced later, this context must be
lane-local, not process-global.

## Index Ownership

Primary key and unique index changes follow the same ownership rule:

```text
committed index entries live in shared committed storage
uncommitted index entries live in TransactionStorage.index_delta
rollback drops uncommitted index entries
commit validates and merges index deltas atomically
```

Uniqueness checks must see:

```text
committed entries visible to the statement
current transaction inserts and updates
current transaction deletes as removals
current epoch overlay if active
```

## Error Handling

Storage errors must carry enough context for SQLSTATE mapping:

```text
unique violation
not null violation, if enforced in storage
serialization/conflict error, if later added
relation not found
tuple not found
unsupported tableam operation
```

Rollback during error handling must be idempotent.

## Acceptance Tests

```text
client A insert inside BEGIN is visible to client A
client A insert inside BEGIN is not visible to client B
client A ROLLBACK drops only client A overlay rows
client A COMMIT makes rows visible to client B
client A SAVEPOINT insert then ROLLBACK TO drops only nested rows
client A UPDATE then ROLLBACK restores previous visible value
primary key entries inserted then rolled back do not block later inserts
disconnecting a session drops its transaction overlay
```

## Performance Tests

Add microbenchmarks for:

```text
rollback of 1 row
rollback of 10,000 rows
commit of 10,000 inserted rows
savepoint rollback of nested rows
primary key uniqueness check with committed plus transaction delta
```

Rollback should scale with the amount of transaction-owned state, not with the
size of the committed database.

## Migration Steps

```text
1. Introduce Storage and SessionStorage structs without changing tableam behavior.
2. Add ExecutionStorageContext and enter/exit guards.
3. Route inserts/updates/deletes into SessionStorage transaction overlays.
4. Implement commit merge and rollback drop.
5. Move primary key index deltas into transaction overlays.
6. Remove process-global StorageState as the source of transaction truth.
7. Keep a narrow compatibility shim only where C callbacks still need symbol entry points.
```

