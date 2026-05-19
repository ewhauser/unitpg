# `spec/038-sequence-state.md`

# Sequence State

## Summary

Implement Rust-owned sequence state for `CREATE SEQUENCE`, `serial`, identity
columns, and `nextval`/`currval`/`setval` behavior needed by application tests.

Sequence state is not table row storage, but it must participate in the same
fixture, epoch, and reset model. It should not use PostgreSQL sequence
relations, WAL logging, or physical storage.

## Goals

```text
support CREATE SEQUENCE metadata and in-memory values
support nextval, currval, and setval for common test paths
support serial and identity defaults through sequence-backed defaults
capture sequence values in fixtures
isolate or reset sequence state across epochs according to policy
avoid PostgreSQL sequence relation storage and WAL
```

## Non-Goals

```text
do not implement WAL-logged sequence cache behavior
do not implement PostgreSQL physical sequence pages
do not guarantee crash-recovery semantics
do not implement logical replication sequence behavior
do not implement every ALTER SEQUENCE option in the first pass
```

## Data Model

```rust
struct SequenceState {
    sequence_id: SequenceId,
    name: QualifiedName,
    data_type: SequenceType,
    increment_by: i64,
    min_value: i64,
    max_value: i64,
    cycle: bool,
    cache_size: u64,
    last_value: i64,
    is_called: bool,
}

struct SessionSequenceState {
    currval: HashMap<SequenceId, i64>,
}

struct SequenceDelta {
    values: HashMap<SequenceId, SequenceState>,
}
```

Sequence definitions live in catalog metadata. Sequence values live in
Rust-owned storage state.

## Transaction Policy

PostgreSQL sequences are generally not transactional. fastpg should choose the
policy that best serves repeatable tests and document it clearly.

Initial recommended policy:

```text
nextval advances immediately within the current fixture/epoch/committed scope
ROLLBACK does not undo nextval inside one session
fixture capture records current sequence values
starting an epoch copies fixture sequence values into epoch sequence delta
finishing an epoch drops epoch sequence deltas
setval updates the current scope immediately
currval is session-local and requires prior nextval or setval in that session
```

This matches common PostgreSQL expectations for rollback while still making
fixture and epoch reset deterministic.

## Serial And Identity

`serial` and identity columns require coordination across parser/analyzer,
catalog defaults, and storage:

```text
CREATE TABLE with serial creates a sequence and default nextval expression
identity columns create sequence-backed defaults
INSERT with omitted column evaluates nextval through PostgreSQL expression code
COPY should apply defaults when column lists omit sequence-backed columns
DROP TABLE drops owned sequences when catalog policy says they are owned
```

Storage owns the sequence value; catalog owns the dependency metadata.

## Fixture And Epoch Behavior

Fixtures record sequence values:

```text
fixture base rows
fixture indexes
fixture sequence snapshot
catalog generation
```

Epoch behavior:

```text
epoch starts with fixture sequence snapshot
nextval inside epoch advances only epoch sequence delta
epoch finish drops sequence delta
later epoch starts from fixture value again
sessions in same epoch share sequence advancement
```

## Acceptance Tests

```text
CREATE SEQUENCE then nextval returns the start value
nextval advances according to increment
currval fails before first nextval in a session
setval changes following nextval behavior
ROLLBACK does not undo nextval under the chosen policy
serial column receives generated values on INSERT
identity column receives generated values on INSERT
fixture capture and epoch reset restore sequence values
two sessions in the same epoch share sequence advancement
```

## Migration Steps

```text
1. Add sequence catalog records for CREATE SEQUENCE.
2. Add Rust SequenceState storage and lookup by sequence OID.
3. Implement nextval, currval, and setval function hooks.
4. Wire serial and identity defaults to sequence-backed defaults.
5. Add fixture sequence snapshots and epoch sequence deltas.
6. Add ownership/drop behavior for sequence dependencies.
7. Add regression tests for rollback and epoch reset policy.
```
