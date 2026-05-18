# `spec/030-catalog-generation-invalidation.md`

# Rust Catalog Generation Invalidation

## Summary

Replace no-op shared invalidation behavior in the Rust-server path with an
explicit Rust catalog generation counter.

fastpg does not use PostgreSQL's multi-process shared invalidation machinery in
the Rust single-process server. Instead, all catalog-changing DDL commits must
publish a new Rust catalog generation. Prepared statements, plans, relcache-like
lookups, syscache-like lookups, and virtual catalog snapshots must record the
generation they were built against and refresh or fail clearly when it changes.

## Goals

```text
catalog changes have one monotonic generation counter
DDL rollback does not publish a new committed generation
DDL commit publishes exactly one new committed generation
prepared statements and plans are invalidated by generation mismatch
virtual pg_catalog rows are read from generationed snapshots
PostgreSQL relcache/syscache adapter state is refreshed by generation checks
no PostgreSQL shared invalidation IPC is used in the Rust-server path
```

## Non-Goals

```text
do not implement PostgreSQL shared invalidation queues
do not use shared memory for catalog invalidation
do not make every DML statement bump catalog generation
do not guarantee byte-identical PostgreSQL invalidation timing
do not add multiple pgcore lanes as part of this spec
```

## Current Problem

The Rust-server path is single process and should not depend on PostgreSQL
backend-to-backend invalidation. A no-op invalidation hook can let stale C or
Rust metadata survive after DDL:

```text
prepare SELECT a FROM t
ALTER TABLE t ADD COLUMN b int
execute old prepared plan
```

The correct fastpg behavior is not to simulate PostgreSQL IPC. It is to make
catalog state generationed and force all cached metadata to prove freshness.

## Target Data Model

```rust
struct Catalog {
    current: ArcSwap<CatalogSnapshot>,
    next_oid: AtomicU32,
}

struct CatalogSnapshot {
    generation: CatalogGeneration,
    namespaces: NamespaceMap,
    relations: RelationMap,
    types: TypeMap,
    functions: FunctionMap,
    operators: OperatorMap,
    settings: SettingsMap,
}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
struct CatalogGeneration(u64);
```

Catalog mutations happen in transaction-local draft state:

```rust
struct CatalogTransaction {
    base_generation: CatalogGeneration,
    draft: CatalogDraft,
    changed: bool,
}
```

Commit publishes a new snapshot if `changed` is true:

```rust
fn commit_catalog_tx(tx: CatalogTransaction, catalog: &Catalog) -> CatalogGeneration {
    if !tx.changed {
        return tx.base_generation;
    }

    catalog.current.store(Arc::new(tx.draft.finish(tx.base_generation.next())));
    tx.base_generation.next()
}
```

Rollback drops the draft and keeps the previous generation.

## Cache Contract

Every cache key or cached object that depends on catalog metadata must record a
generation:

```rust
struct PreparedStatement {
    sql: String,
    parameter_types: Vec<Oid>,
    catalog_generation: CatalogGeneration,
    pgcore_handle: PreparedHandle,
}

struct CachedPlan {
    key: PlanCacheKey,
    catalog_generation: CatalogGeneration,
    plan: PlannedHandle,
}

struct RelationMetadata {
    oid: Oid,
    catalog_generation: CatalogGeneration,
    columns: Arc<[Column]>,
}
```

Before reuse:

```rust
if cached.catalog_generation != catalog.current_generation() {
    return Err(CacheInvalidated);
}
```

The caller may then re-prepare/re-plan, or return a PostgreSQL-compatible error
when reusing the old object would violate protocol semantics.

## C pgcore Boundary

Every pgcore lane operation receives the catalog generation it should execute
against:

```rust
struct PgCoreRequest {
    session_id: SessionId,
    catalog_generation: CatalogGeneration,
    operation: PgCoreOperation,
}
```

The Rust catalog provider exposes generation-aware lookup functions to C:

```c
bool fastpg_lookup_relation(uint32 generation,
                            uint32 relation_oid,
                            FastPgRustCatalogRelation *out);
```

If C asks for metadata using a stale generation, the provider must fail loudly
with an internal invalidation error rather than silently returning mixed
metadata.

Initial implementation may use the current generation for all lookups inside
one lane operation. Later, prepared plans can pin a snapshot generation for the
duration of execution.

## Replacing Shared Invalidation

For `USE_FASTPG` Rust-server execution:

```text
PostgreSQL shared invalidation send operations become catalog generation bumps
PostgreSQL shared invalidation receive operations become generation checks
relcache/syscache adapter misses rebuild from Rust CatalogSnapshot
no sinval IPC queue is required
```

Do not wire PostgreSQL shared memory invalidation queues into the Rust server.
If PostgreSQL code reaches an IPC-only invalidation path while the no-internal-
IPC guard is enabled, that is a bug to fix or bypass.

## DDL Semantics

Generation bumps on commit:

```text
CREATE SCHEMA
CREATE TABLE
ALTER TABLE
DROP TABLE
TRUNCATE if relation metadata changes
CREATE INDEX
DROP INDEX
ALTER INDEX
CREATE SEQUENCE
DROP SEQUENCE
changes to functions/operators/types/settings exposed through catalogs
```

No generation bump:

```text
DML only
transaction rollback
savepoint rollback that discards catalog draft changes
VACUUM no-op
ANALYZE no-op unless it publishes planner-visible stats generation
```

If planner-visible statistics become mutable later, use a separate statistics
generation or include stats changes explicitly in the catalog generation policy.

## Interaction With Prepared Statements

Extended protocol prepared statements are session-owned but catalog-generation
bound.

On generation mismatch:

```text
if the prepared SQL can be transparently re-prepared, do so
if PostgreSQL would report a cached-plan invalidation error, match that behavior
if parameters or result columns changed, send updated Describe data only after re-describe
```

The first implementation may invalidate and re-prepare on next execute.

## Interaction With Virtual pg_catalog

Virtual catalog queries read from one `CatalogSnapshot`.

Rules:

```text
one statement sees one catalog generation
rows from pg_class/pg_attribute/pg_index agree within that statement
OID lookups and name lookups come from the same snapshot
information_schema reads the same snapshot as pg_catalog
```

This prevents mixed old/new metadata when DDL commits concurrently with
introspection.

## Acceptance Tests

```text
CREATE TABLE increments catalog generation on commit
CREATE TABLE then ROLLBACK does not increment committed catalog generation
ALTER TABLE invalidates prepared SELECT metadata
DROP TABLE invalidates prepared SELECT execution
pg_attribute and pg_class agree after ALTER TABLE
two clients see old metadata before DDL commit and new metadata after commit
savepoint rollback of DDL does not publish discarded metadata
```

## Regression Tests

Add pgwire tests:

```text
client A PREPARE select from t
client B ALTER TABLE t ADD COLUMN b int COMMIT
client A EXECUTE prepared statement
```

Expected behavior must be one of:

```text
successful transparent re-prepare with correct result metadata
Postgres-compatible cached-plan invalidation error
```

It must never silently use stale column metadata.

## Performance Notes

Generation checks are cheap:

```text
load current generation
compare u64
reuse cached object if equal
```

DDL is rare relative to DML in test workloads. It is acceptable for DDL commit
to clone or rebuild catalog maps initially. Hot-path query execution should only
pay the generation comparison.

## Migration Steps

```text
1. Introduce CatalogGeneration newtype and current_generation API.
2. Store generation on relation/type/function/operator metadata snapshots.
3. Add generation to prepared statement and plan cache records.
4. Make DDL draft catalog changes publish a new generation only on commit.
5. Update virtual pg_catalog providers to read from one CatalogSnapshot.
6. Replace no-op invalidation hooks with generation checks in USE_FASTPG paths.
7. Add stale-prepared-plan and virtual-catalog consistency tests.
```

