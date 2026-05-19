# `spec/037-semantic-unique-indexes.md`

# Semantic Unique Indexes

## Summary

Implement semantic uniqueness enforcement for primary keys and supported unique
indexes without implementing PostgreSQL physical btree pages or secondary index
acceleration.

Primary-key uniqueness and lookup exist today. This spec extends the storage
model to cover simple secondary unique constraints when fastpg needs constraint
fidelity for application tests. Non-unique secondary indexes remain catalog
compatibility metadata unless a later performance spec enables them.

## Goals

```text
enforce primary-key uniqueness through semantic indexes
enforce supported secondary unique constraints
support multi-column unique keys
support PostgreSQL NULL behavior for unique constraints
maintain index deltas in transaction and epoch overlays
validate existing rows when creating a unique index
return SQLSTATE 23505 for duplicate keys
avoid physical index pages and index vacuum
```

## Non-Goals

```text
do not accelerate non-unique secondary index scans
do not implement ordered scans
do not implement bitmap scans
do not implement GIN/GiST/SP-GiST/BRIN semantics
do not implement partial or expression unique indexes in the first pass
do not implement deferrable unique constraints in the first pass
```

## Supported Index Shapes

First supported set:

```text
primary key on ordinary columns
UNIQUE on ordinary columns
multi-column UNIQUE on ordinary columns
btree-compatible equality keys
NULLS DISTINCT default behavior
```

Rejected or metadata-only at first:

```text
partial unique indexes
expression unique indexes
NULLS NOT DISTINCT
deferrable unique constraints
unique indexes on unsupported types
non-unique secondary indexes
```

Unsupported unique shapes should either fail clearly at DDL time or be accepted
only as non-enforced compatibility metadata if the project explicitly chooses
that behavior for migration setup.

## Data Model

```rust
struct SemanticIndex {
    index_id: IndexId,
    relation_id: RelationId,
    kind: SemanticIndexKind,
    columns: Vec<ColumnId>,
    entries: IndexMap,
}

enum SemanticIndexKind {
    PrimaryKey,
    Unique,
}

struct IndexDelta {
    inserted: HashMap<IndexKey, RowId>,
    deleted: HashSet<IndexKey>,
}
```

Index memory uses the region ownership model from
`032-storage-memory-ownership-accounting.md`. Index entries own copied key
bytes, not full row values.

## Uniqueness Visibility

Uniqueness checks see:

```text
fixture base index
epoch index deltas
committed storage generation from the statement snapshot
current transaction and savepoint deltas
current transaction deletes as removals
```

Rules:

```text
duplicate visible non-null key fails with SQLSTATE 23505
default UNIQUE allows multiple rows where any key column is NULL
primary key columns must be NOT NULL
update that keeps the key unchanged should not conflict with itself
rollback drops index deltas with row deltas
commit merges row and index deltas atomically
```

## DDL Behavior

`CREATE UNIQUE INDEX` and `ALTER TABLE ADD UNIQUE` on supported shapes:

```text
validate existing visible rows
build semantic unique index state
publish catalog metadata
make future INSERT/UPDATE enforce uniqueness
```

If validation finds duplicates, return `23505` and leave catalog and storage
state unchanged.

## Planner Policy

This spec is about constraint enforcement. It does not require planner-visible
secondary index scan paths.

```text
primary-key equality lookup remains planner-visible
secondary unique indexes may stay hidden from path generation
catalog rows remain visible for ORM introspection
unsupported secondary index scans fail before ExecutorStart
```

## Acceptance Tests

```text
duplicate primary-key insert fails with SQLSTATE 23505
duplicate secondary UNIQUE insert fails with SQLSTATE 23505
multi-column unique key is enforced
multiple NULL values are allowed for default UNIQUE
update to duplicate unique key fails
update that keeps unique key unchanged succeeds
rollback of inserted key releases the key
savepoint rollback releases nested key
CREATE UNIQUE INDEX validates existing duplicates
secondary unique catalog rows remain visible to introspection
```

## Migration Steps

```text
1. Generalize primary-key key extraction into SemanticIndex key extraction.
2. Add SemanticIndex metadata to storage and catalog adapters.
3. Store index entries and deltas in arena-owned regions.
4. Enforce primary keys through the shared unique-index path.
5. Add supported secondary UNIQUE enforcement.
6. Add DDL validation for existing rows.
7. Keep secondary scan acceleration disabled unless a later spec enables it.
```
