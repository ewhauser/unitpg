# Typed Rust Catalog Core

## Summary

Refactor the Rust catalog from a dynamic `pg_catalog` row mirror into a typed
catalog core.

The current fastpg catalog path captures selected PostgreSQL catalog tuple
writes into Rust and stores them as catalog-shaped rows. That avoids filesystem
catalog storage, but it makes Rust responsible for maintaining a second
heap-shaped representation of `pg_class`, `pg_attribute`, `pg_index`, and
nearby catalogs.

The target design makes typed Rust metadata the source of truth:

```text
RelationMeta
ColumnMeta
TypeMeta
IndexMeta
ConstraintMeta
NamespaceMeta
SequenceMeta
```

Virtual `pg_catalog` rows become rendered compatibility views over a
generationed `CatalogSnapshot`, not the primary data model.

## Goals

```text
store user-created schema objects as typed Rust metadata
make catalog mutations transaction-local until commit
publish catalog changes by swapping one generationed CatalogSnapshot
serve direct metadata APIs from typed maps, not rendered catalog rows
render virtual pg_catalog rows from typed metadata for SQL compatibility
keep existing PostgreSQL parser/analyzer/planner integration working
keep no-filesystem and no-shared-invalidation assumptions
support incremental migration from the current catalog tuple hook path
preserve current regression coverage for DDL, primary keys, and catalog scans
```

## Non-Goals

```text
do not implement durable catalog storage
do not implement PostgreSQL shared invalidation queues
do not replace the PostgreSQL parser/analyzer/planner in this spec
do not make every PostgreSQL catalog fully typed in the first pass
do not remove all compatibility row storage in the first pass
do not guarantee byte-identical PostgreSQL catalog MVCC behavior
do not add multiple pgcore execution lanes
```

## Current Problem

The current dynamic catalog model is row-shaped:

```text
PostgreSQL DDL
  -> CatalogTupleInsert/Update/Delete(pg_class, pg_attribute, pg_index, ...)
  -> convert HeapTuple values to text arrays
  -> Rust CatalogOverlay stores catalog rows and tombstones
  -> typed Rust APIs reconstruct RelationRecord/IndexRecord from those rows
```

That works for the current smoke and regression cases, but it creates the wrong
center of gravity:

```text
every feature adds more pg_catalog row decoding
typed lookups depend on generic catalog row projection
catalog scans and semantic metadata are coupled to the same row overlay
primary-key/index metadata must be rebuilt from pg_index-shaped rows
DDL correctness depends on mirroring enough PostgreSQL catalog side effects
cache invalidation tracks catalog row changes rather than semantic changes
```

The important distinction is source of truth:

```text
current:
  dynamic pg_catalog rows are truth
  typed APIs are derived from rows

target:
  typed catalog metadata is truth
  pg_catalog rows are rendered from metadata
```

## Performance Thesis

The reason to own catalog metadata in Rust is not that Rust can out-perform
PostgreSQL's catalog code at doing the same heap-shaped work. The reason is to
avoid doing that work for ephemeral test-server use cases.

The fast path this design is trying to unlock is:

```text
server startup:
  load generated static catalog data
  create empty typed maps for user schema
  skip initdb, catalog relation files, and relcache init files

schema and fixture setup:
  publish compact typed metadata snapshots
  keep direct relation/type/index lookups in Rust maps
  avoid planner/executor catalog probes becoming heap/index scans

test reset:
  drop or rewind catalog/storage generations, fixtures, or epochs
  avoid replaying setup SQL or rebuilding filesystem-backed catalog state
```

That thesis only holds if the Rust catalog is semantic. A second
heap-shaped catalog in Rust loses the main advantage: it still has to model
`pg_class`, `pg_attribute`, `pg_type`, `pg_index`, catalog CTIDs, cache
invalidation, and scan behavior, just outside PostgreSQL's native machinery.

The intended split is:

```text
semantic metadata:
  source of truth for fastpg planning, execution, reset, and fixture lifecycle

virtual pg_catalog rows:
  compatibility output for SQL introspection and PostgreSQL code paths that
  still expect catalog-shaped tuples
```

## Target Architecture

```text
ServerState
  -> Arc<Catalog>
       -> ArcSwap<CatalogSnapshot>
       -> AtomicU32 next_oid
       -> CatalogStaticData

SessionState
  -> CatalogSession
       -> transaction stack of CatalogDraft values
```

The catalog owns immutable committed snapshots. Sessions own draft mutations.
Every statement observes one catalog generation.

```rust
struct Catalog {
    current: ArcSwap<CatalogSnapshot>,
    next_oid: AtomicU32,
    static_data: Arc<CatalogStaticData>,
}

struct CatalogSnapshot {
    generation: CatalogGeneration,
    static_data: Arc<CatalogStaticData>,
    namespaces: BTreeMap<Oid, NamespaceMeta>,
    namespaces_by_name: BTreeMap<String, Oid>,
    relations: BTreeMap<Oid, RelationMeta>,
    relations_by_name: BTreeMap<(Oid, String), Oid>,
    types: BTreeMap<Oid, TypeMeta>,
    types_by_name: BTreeMap<(Oid, String), Oid>,
    indexes: BTreeMap<Oid, IndexMeta>,
    constraints: BTreeMap<Oid, ConstraintMeta>,
    sequences: BTreeMap<Oid, SequenceMeta>,
    compat_rows: CompatCatalogRows,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct CatalogGeneration(u64);
```

`CatalogStaticData` continues to hold generated built-in data for PostgreSQL
types, functions, operators, opclasses, casts, and static system catalogs.
Snapshots keep an `Arc` to that static data so direct lookups and renderers can
combine built-in rows with session-created and test-created schema state.

## Typed Metadata

The first typed metadata pass should cover the catalogs that drive the existing
fastpg correctness gates:

```rust
struct NamespaceMeta {
    oid: Oid,
    name: String,
    owner: Oid,
}

struct RelationMeta {
    oid: Oid,
    namespace: Oid,
    owner: Oid,
    name: String,
    relkind: RelKind,
    row_type_oid: Oid,
    storage_id: StorageRelationId,
    columns: Arc<[ColumnMeta]>,
    indexes: Arc<[Oid]>,
    constraints: Arc<[Oid]>,
    options: RelationOptions,
}

struct ColumnMeta {
    attnum: i16,
    name: String,
    type_oid: Oid,
    type_mod: i32,
    is_not_null: bool,
    has_default: bool,
    generated: GeneratedKind,
    is_dropped: bool,
}

struct TypeMeta {
    oid: Oid,
    namespace: Oid,
    owner: Oid,
    name: String,
    kind: TypeKind,
    relation_oid: Oid,
    array_oid: Oid,
    element_oid: Oid,
    typlen: i16,
    typbyval: bool,
    typalign: u8,
    typstorage: u8,
    typcollation: Oid,
}

struct IndexMeta {
    oid: Oid,
    namespace: Oid,
    owner: Oid,
    name: String,
    heap_oid: Oid,
    method_oid: Oid,
    is_unique: bool,
    is_primary: bool,
    nulls_not_distinct: bool,
    is_valid: bool,
    is_ready: bool,
    key_columns: Arc<[IndexColumnMeta]>,
}

struct ConstraintMeta {
    oid: Oid,
    namespace: Oid,
    name: String,
    relation_oid: Oid,
    index_oid: Oid,
    kind: ConstraintKind,
    key_columns: Arc<[i16]>,
}
```

The exact field list may grow, but the rule is that metadata APIs must not
parse rendered `pg_catalog` rows to answer normal fastpg planning/execution
questions.

## Drafts And Transactions

Catalog changes happen in session-local drafts.

```rust
struct CatalogSession {
    stack: Vec<CatalogDraft>,
    explicit_transaction: bool,
}

struct CatalogDraft {
    base_generation: CatalogGeneration,
    created_namespaces: BTreeMap<Oid, NamespaceMeta>,
    created_relations: BTreeMap<Oid, RelationMeta>,
    updated_relations: BTreeMap<Oid, RelationPatch>,
    dropped_relations: BTreeSet<Oid>,
    created_types: BTreeMap<Oid, TypeMeta>,
    updated_types: BTreeMap<Oid, TypePatch>,
    dropped_types: BTreeSet<Oid>,
    created_indexes: BTreeMap<Oid, IndexMeta>,
    updated_indexes: BTreeMap<Oid, IndexPatch>,
    dropped_indexes: BTreeSet<Oid>,
    created_constraints: BTreeMap<Oid, ConstraintMeta>,
    dropped_constraints: BTreeSet<Oid>,
    compat_rows: CompatCatalogDraft,
}
```

Commit merges the top draft into its parent draft or publishes a new snapshot:

```rust
fn commit_top_draft(session: &mut CatalogSession, catalog: &Catalog) {
    let Some(draft) = session.stack.pop() else {
        return;
    };

    if let Some(parent) = session.stack.last_mut() {
        parent.merge(draft);
        return;
    }

    if draft.is_empty() {
        return;
    }

    let base = catalog.current.load_full();
    let next = draft.apply_to(&base);
    catalog.current.store(Arc::new(next));
}
```

Rollback drops the draft. Savepoint rollback drops only the nested draft.

## Mutation Boundary

The first migration should keep the existing C interception points, but change
their purpose. The hook becomes a decoder and adapter, not the catalog storage
model.

```c
void
CatalogTupleInsert(Relation rel, HeapTuple tup)
{
    if (fastpg_is_typed_catalog_input(rel))
    {
        fastpg_rust_catalog_apply_tuple(RelationGetRelid(rel),
                                        FASTPG_CATALOG_INSERT,
                                        tup);
        return;
    }

    if (fastpg_is_compat_virtual_catalog(rel))
    {
        fastpg_rust_catalog_apply_compat_tuple(RelationGetRelid(rel),
                                               FASTPG_CATALOG_INSERT,
                                               tup);
        return;
    }

    simple_heap_insert(rel, tup);
    CatalogIndexInsert(...);
}
```

Rust decodes important catalog rows into typed mutations:

```rust
fn apply_catalog_tuple(
    catalog_oid: Oid,
    op: CatalogTupleOp,
    row: DecodedCatalogRow,
) -> Result<(), CatalogError> {
    match catalog_oid {
        PG_CLASS_RELATION_OID => apply_pg_class_row(op, row),
        PG_ATTRIBUTE_RELATION_OID => apply_pg_attribute_row(op, row),
        PG_TYPE_RELATION_OID => apply_pg_type_row(op, row),
        PG_INDEX_RELATION_OID => apply_pg_index_row(op, row),
        PG_CONSTRAINT_RELATION_OID => apply_pg_constraint_row(op, row),
        PG_NAMESPACE_RELATION_OID => apply_pg_namespace_row(op, row),
        _ => apply_compat_catalog_row(catalog_oid, op, row),
    }
}
```

The adapter may initially receive text-form values from C, matching the current
boundary. A later cleanup can pass typed datums or generated per-catalog decode
functions to remove string round trips.

## Direct Lookup APIs

Metadata APIs read typed snapshots first.

```rust
fn relation_by_oid(snapshot: &CatalogSnapshot, oid: Oid) -> Option<RelationMeta> {
    snapshot
        .relations
        .get(&oid)
        .cloned()
        .or_else(|| snapshot.static_data.relation_by_oid(oid))
}

fn relation_by_name(
    snapshot: &CatalogSnapshot,
    namespace: Oid,
    name: &str,
) -> Option<RelationMeta> {
    let key = (namespace, normalize_identifier(name));
    let oid = snapshot.relations_by_name.get(&key)?;
    relation_by_oid(snapshot, *oid)
}

fn primary_key_index(snapshot: &CatalogSnapshot, relation_oid: Oid) -> Option<IndexMeta> {
    let relation = relation_by_oid(snapshot, relation_oid)?;
    relation
        .indexes
        .iter()
        .filter_map(|index_oid| snapshot.indexes.get(index_oid))
        .find(|index| index.is_primary)
        .cloned()
}
```

C-facing functions keep their current shape where possible:

```c
bool fastpg_rust_catalog_relation_by_oid(uint32 oid,
                                         FastPgRustCatalogRelation *out);
bool fastpg_rust_catalog_relation_column_by_index(uint32 relation_oid,
                                                  size_t column_index,
                                                  FastPgRustCatalogColumn *out);
bool fastpg_rust_catalog_primary_key_index_info(uint32 index_oid,
                                                FastPgRustPrimaryKeyIndexInfo *out);
```

Internally those functions must not reconstruct metadata by scanning rendered
`pg_class` or `pg_attribute` rows.

## Virtual pg_catalog Rendering

Virtual catalog scans render rows from one snapshot:

```rust
fn catalog_rows(snapshot: &CatalogSnapshot, relation_oid: Oid) -> Vec<CatalogRow> {
    match relation_oid {
        PG_CLASS_RELATION_OID => render_pg_class(snapshot),
        PG_ATTRIBUTE_RELATION_OID => render_pg_attribute(snapshot),
        PG_TYPE_RELATION_OID => render_pg_type(snapshot),
        PG_INDEX_RELATION_OID => render_pg_index(snapshot),
        PG_CONSTRAINT_RELATION_OID => render_pg_constraint(snapshot),
        PG_NAMESPACE_RELATION_OID => render_pg_namespace(snapshot),
        _ => render_static_or_compat_rows(snapshot, relation_oid),
    }
}
```

Rendering rules:

```text
one scan uses one captured CatalogSnapshot
rows from pg_class and pg_attribute agree for the same generation
row_id and CTID values remain stable inside one generation
rendered rows include static built-in rows plus typed dynamic rows
compat rows are appended only for catalogs that are not typed yet
```

The renderer may use generated catalog column descriptors to produce
`CatalogRow` values, but generated descriptors are schema information, not
dynamic state.

## Compatibility Rows

Some catalogs are not worth typing immediately. Keep a compatibility bucket for
them:

```rust
struct CompatCatalogRows {
    rows: BTreeMap<Oid, BTreeMap<u64, CatalogRow>>,
    tombstones: BTreeMap<Oid, BTreeSet<u64>>,
}
```

Rules:

```text
compat rows are not used by direct relation/type/index lookup APIs
compat rows are allowed for low-priority introspection-only catalogs
typed catalogs must not fall back to compat rows after migration
each migration step removes one catalog from the compatibility bucket
```

This lets the implementation ship incrementally without preserving the current
row mirror as the permanent architecture.

## OID And Name Allocation

OID allocation belongs to the catalog root:

```rust
impl Catalog {
    fn allocate_oid(&self) -> Oid {
        Oid(self.next_oid.fetch_add(1, Ordering::Relaxed))
    }
}
```

Rules:

```text
allocated OIDs are never reused inside one server lifetime
rollback may leave OID gaps
explicit OIDs from PostgreSQL catalog tuple input are preserved
name indexes are updated atomically with typed metadata on commit
duplicate names fail during draft validation before publish
```

## Invalidation And Cache Contract

This spec builds on `030-catalog-generation-invalidation.md`.

Every committed semantic catalog change publishes exactly one new generation.
The generation belongs to the `CatalogSnapshot`, not to a row overlay.

```rust
struct PreparedStatement {
    catalog_generation: CatalogGeneration,
    // ...
}

fn ensure_catalog_fresh(
    prepared: &PreparedStatement,
    catalog: &Catalog,
) -> Result<(), CatalogError> {
    if prepared.catalog_generation != catalog.current_generation() {
        return Err(CatalogError::cache_invalidated());
    }
    Ok(())
}
```

No generation bump happens for:

```text
DML only
transaction rollback
savepoint rollback
no-op compatibility catalog writes
```

## Relation To In-Memory Storage

Typed catalog ownership and memory-backed heap storage solve different
problems.

```text
typed catalog core:
  makes metadata lookup, DDL, reset, and virtual pg_catalog rendering fast
  avoids growing a second heap-shaped catalog implementation

memory-backed storage:
  keeps PostgreSQL heap/index/catalog semantics while removing filesystem IO
  is safer for compatibility but less direct for fast metadata paths
```

This spec chooses typed catalog ownership for fastpg's Rust-server path. It
does not prevent a later memory-backed storage manager, but catalog metadata
should not depend on filesystem-backed PostgreSQL catalog heaps.

A real-PostgreSQL-catalog-on-memory-storage spike is still useful as a
compatibility check. It should answer whether PostgreSQL's native catalog
machinery can be kept while replacing only the physical storage layer. That
would be a different tradeoff:

```text
possible benefit:
  delete most bespoke catalog, catcache, and relcache compatibility shims

possible cost:
  reintroduce enough heap/index/WAL/CLOG/relmap/bootstrap semantics that
  ephemeral startup and reset stop being cheap
```

Do not let this typed catalog design drift into the middle ground. Either the
Rust catalog remains semantic and generationed, or a separate spike proves that
native PostgreSQL catalog semantics over memory storage are cheaper overall.

## Migration Plan

### Step 1: Add Typed Snapshot Beside Existing Overlay

```text
add CatalogSnapshot typed maps
add CatalogDraft transaction stack
keep current CatalogOverlay as CompatCatalogRows
add tests for create/commit/rollback/savepoint metadata visibility
```

### Step 2: Type `pg_class` And `pg_attribute`

```text
decode pg_class tuple writes into RelationMeta shells
decode pg_attribute tuple writes into ColumnMeta arrays
serve relation_by_oid/name and relation_column_by_index from typed maps
render pg_class and pg_attribute rows from typed maps
```

### Step 3: Type `pg_type`

```text
decode table rowtype and scalar type tuple writes into TypeMeta
serve type_by_oid/name from typed maps plus static built-ins
render pg_type rows from typed maps
remove dynamic pg_type dependency from row overlay lookups
```

### Step 4: Type `pg_index` And `pg_constraint`

```text
decode primary-key and unique-index metadata into IndexMeta
decode primary-key constraints into ConstraintMeta
serve primary_key_index_info from IndexMeta
rebuild storage unique indexes from typed IndexMeta on commit
render pg_index and pg_constraint rows from typed maps
```

### Step 5: Capture Statement Snapshots

```text
virtual catalog scans capture Arc<CatalogSnapshot> at scan start
direct C lookup APIs use the active statement catalog snapshot
prepared statements record catalog generation at prepare time
```

### Step 6: Shrink Compatibility Overlay

```text
remove typed catalogs from CompatCatalogRows
reject typed catalog writes that would only update compat rows
document remaining compatibility-only catalogs
```

## Acceptance Tests

```text
CREATE TABLE commits one typed RelationMeta with matching ColumnMeta rows
ROLLBACK after CREATE TABLE leaves no relation metadata
ROLLBACK TO SAVEPOINT discards nested relation metadata only
SELECT from pg_class shows typed relations rendered as catalog rows
SELECT from pg_attribute shows columns for typed relations
relation_by_name and SELECT pg_class agree within one generation
CREATE TABLE with PRIMARY KEY creates typed IndexMeta and ConstraintMeta
primary-key lookups use IndexMeta rather than pg_index row scanning
DROP TABLE removes relation, type, index, and constraint metadata atomically
prepared statement reuse fails or refreshes after catalog generation changes
two sessions do not see each other's uncommitted catalog drafts
catalog generation does not bump for DML-only transactions
```

The blocking regression gate should include:

```sh
make -C benches regression
```

After migration touches storage/index behavior, also run:

```sh
cargo test -p fastpg-storage
make -C benches pgbench-simple-indexed
```

## Risks

```text
PostgreSQL DDL may emit catalog tuple writes in an order that creates temporary
  incomplete typed metadata
ALTER TABLE may require richer RelationPatch handling than CREATE TABLE
compat rows can silently become a second source of truth if not aggressively
  removed from typed catalogs
string-based tuple decoding can preserve current behavior but hide type bugs
rendered catalog rows must stay consistent enough for existing systable scans
```

Mitigations:

```text
allow incomplete draft objects but validate before commit
make typed direct APIs ignore incomplete draft objects unless statement-visible
add per-catalog render tests for pg_class/pg_attribute/pg_type/pg_index
track a denylist of typed catalogs that may not use CompatCatalogRows
keep generated static catalog descriptors as the render/schema authority
```

## Open Questions

```text
Should CREATE TABLE eventually bypass PostgreSQL catalog tuple writes entirely?
Should the C boundary pass typed Datums instead of output-function strings?
How much ALTER TABLE support belongs in the first typed migration?
Should planner-visible statistics use catalog generation or a separate stats generation?
Should sequence metadata land in this catalog snapshot or remain under storage state?
```
