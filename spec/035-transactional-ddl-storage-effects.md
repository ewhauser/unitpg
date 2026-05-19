# `spec/035-transactional-ddl-storage-effects.md`

# Transactional DDL Storage Effects

## Summary

Make storage effects from DDL commit and roll back with catalog changes.

The catalog generation spec defines how metadata changes become visible. This
spec defines the matching storage-side effects for `CREATE TABLE`, `DROP
TABLE`, `TRUNCATE`, primary-key creation, and later table rewrites.

## Goals

```text
DDL rollback does not leave storage side effects behind
DDL commit applies storage changes exactly once
catalog and storage state stay generation-consistent
DROP TABLE removes row, index, scan, and memory ownership state on commit
TRUNCATE is transactional unless explicitly documented otherwise
primary-key creation builds semantic index state from visible rows
```

## Non-Goals

```text
do not implement heap relfilenodes
do not implement smgr file swaps
do not implement physical CLUSTER or VACUUM FULL
do not implement arbitrary ALTER TABLE rewrites in the first pass
do not implement PostgreSQL lock-manager semantics
```

## Current Problem

Some DDL paths currently call directly into storage clear/drop behavior. That
works for simple regression setup, but it is not transactional enough:

```text
BEGIN;
TRUNCATE t;
ROLLBACK;
```

must restore the visible rows. Likewise, a failed or rolled-back `DROP TABLE`
must not destroy committed row memory.

## Target Data Model

```rust
struct DdlStorageJournal {
    entries: Vec<DdlStorageOp>,
}

enum DdlStorageOp {
    CreateRelation { relation_id: RelationId },
    DropRelation { relation_id: RelationId },
    TruncateRelation { relation_id: RelationId },
    RebuildPrimaryKey { relation_id: RelationId },
}
```

DDL executed inside a transaction records storage intent in the transaction's
DDL journal. Commit applies the journal after catalog commit succeeds. Rollback
drops the journal.

## Operation Semantics

### CREATE TABLE

```text
catalog draft creates relation metadata
storage journal records CreateRelation
commit creates an empty relation region
rollback creates no committed storage
```

### DROP TABLE

```text
catalog draft removes relation metadata
storage journal records DropRelation
commit drops row regions, index regions, scans, and accounting for the relation
rollback preserves existing storage
```

### TRUNCATE

```text
catalog metadata usually remains unchanged
storage journal records TruncateRelation
transaction sees the table as empty after TRUNCATE
commit replaces relation storage with an empty generation
rollback restores previous visible rows
```

The first implementation may represent transactional truncate as a relation-wide
delete marker in the transaction overlay.

### PRIMARY KEY CREATION

```text
catalog draft records primary-key metadata
storage journal records RebuildPrimaryKey
commit validates visible rows for duplicates
commit publishes primary-key semantic index state
rollback leaves previous index state unchanged
```

Duplicate keys should return SQLSTATE `23505`.

## Catalog Coordination

This spec composes with `030-catalog-generation-invalidation.md`:

```text
catalog commit publishes metadata generation
storage commit publishes storage generation
prepared plans invalidate on catalog generation mismatch
storage scans read storage generation from statement snapshot
```

Catalog and storage commit order must not expose a state where catalog says a
relation exists but storage cannot answer basic table AM calls.

## Acceptance Tests

```text
CREATE TABLE then ROLLBACK leaves no relation storage
DROP TABLE then ROLLBACK preserves rows
TRUNCATE then ROLLBACK preserves rows
TRUNCATE then COMMIT removes rows
ALTER TABLE ADD PRIMARY KEY then ROLLBACK does not enforce the key
ALTER TABLE ADD PRIMARY KEY then COMMIT enforces and supports lookup
duplicate rows during primary-key creation fail with SQLSTATE 23505
catalog and storage generations advance together for committed DDL
```

## Migration Steps

```text
1. Add DdlStorageJournal to transaction state.
2. Route CREATE/DROP/TRUNCATE/ADD PRIMARY KEY through the journal.
3. Add transactional truncate overlay marker.
4. Apply journal during commit after catalog validation.
5. Drop journal during rollback and savepoint rollback.
6. Rebuild primary-key semantic indexes from visible rows at commit.
7. Add regression tests for DDL rollback behavior.
```
