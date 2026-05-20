#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use fastpg_types::Oid;

pub const BOOL_OID: Oid = Oid(16);
pub const CHAR_OID: Oid = Oid(18);
pub const NAME_OID: Oid = Oid(19);
pub const INT8_OID: Oid = Oid(20);
pub const INT2_OID: Oid = Oid(21);
pub const INT2VECTOR_OID: Oid = Oid(22);
pub const INT4_OID: Oid = Oid(23);
pub const TEXT_OID: Oid = Oid(25);
pub const OID_OID: Oid = Oid(26);
pub const TID_OID: Oid = Oid(27);
pub const XID_OID: Oid = Oid(28);
pub const CID_OID: Oid = Oid(29);
pub const OIDVECTOR_OID: Oid = Oid(30);
pub const PG_NODE_TREE_OID: Oid = Oid(194);
pub const FLOAT4_OID: Oid = Oid(700);
pub const FLOAT8_OID: Oid = Oid(701);
pub const UNKNOWN_OID: Oid = Oid(705);
pub const CHAR_ARRAY_OID: Oid = Oid(1002);
pub const INT2_ARRAY_OID: Oid = Oid(1005);
pub const INT4_ARRAY_OID: Oid = Oid(1007);
pub const TEXT_ARRAY_OID: Oid = Oid(1009);
pub const OID_ARRAY_OID: Oid = Oid(1028);
pub const ACLITEM_OID: Oid = Oid(1033);
pub const ACLITEM_ARRAY_OID: Oid = Oid(1034);
pub const BPCHAR_OID: Oid = Oid(1042);
pub const VARCHAR_OID: Oid = Oid(1043);
pub const TIMESTAMP_OID: Oid = Oid(1114);
pub const TIMESTAMPTZ_OID: Oid = Oid(1184);
pub const REGCLASS_OID: Oid = Oid(2205);
pub const ANY_OID: Oid = Oid(2276);
pub const ANYARRAY_OID: Oid = Oid(2277);
pub const INTERNAL_OID: Oid = Oid(2281);
pub const LSN_OID: Oid = Oid(3220);
pub const ANYENUM_OID: Oid = Oid(3500);

pub const DEFAULT_COLLATION_OID: Oid = Oid(100);
pub const C_COLLATION_OID: Oid = Oid(950);

const INVALID_OID: Oid = Oid(0);
const POSTGRES_ROLE_OID: Oid = Oid(10);
pub const PG_CATALOG_NAMESPACE_OID: Oid = Oid(11);
pub const PUBLIC_NAMESPACE_OID: Oid = Oid(2200);
const PG_CLASS_RELATION_OID: Oid = Oid(1259);
const PG_ATTRIBUTE_RELATION_OID: Oid = Oid(1249);
const PG_TYPE_RELATION_OID: Oid = Oid(1247);
const PG_PROC_RELATION_OID: Oid = Oid(1255);
const PG_DATABASE_RELATION_OID: Oid = Oid(1262);
const PG_NAMESPACE_RELATION_OID: Oid = Oid(2615);
const PG_INDEX_RELATION_OID: Oid = Oid(2610);
const PG_CONSTRAINT_RELATION_OID: Oid = Oid(2606);
const PG_ENUM_RELATION_OID: Oid = Oid(3501);
const BTREE_INDEX_AM_OID: Oid = Oid(403);
const SYNTHETIC_CATALOG_ROWTYPE_OID_BASE: u32 = 0xF000_0000;
const TEMPLATE1_DATABASE_OID: Oid = Oid(1);
const TEMPLATE0_DATABASE_OID: Oid = Oid(4);
const POSTGRES_DATABASE_OID: Oid = Oid(5);
const SYNTHETIC_DATABASE_OID_BASE: u32 = 0xE100_0000;

pub const VIRTUAL_CATALOG_STATIC: u8 = 1;
pub const VIRTUAL_CATALOG_DYNAMIC: u8 = 2;
pub const VIRTUAL_CATALOG_EMPTY: u8 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StaticCatalogValue {
    Null,
    Raw(&'static str),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StaticCatalogColumn {
    pub name: &'static str,
    pub type_name: &'static str,
    pub type_oid: Oid,
    pub attlen: i16,
    pub attnum: i16,
    pub attndims: i32,
    pub attbyval: bool,
    pub attalign: u8,
    pub attstorage: u8,
    pub attnotnull: bool,
    pub attcollation: Oid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StaticCatalogRow {
    pub row_id: u64,
    pub values: &'static [StaticCatalogValue],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StaticCatalogTable {
    pub oid: Oid,
    pub name: &'static str,
    pub rowtype_oid: Oid,
    pub columns: &'static [StaticCatalogColumn],
    pub rows: &'static [StaticCatalogRow],
}

#[derive(Clone, Debug, PartialEq)]
pub enum CatalogValue {
    Null,
    Bool(bool),
    Char(u8),
    Int16(i16),
    Int32(i32),
    Float32(f32),
    Oid(Oid),
    Name(String),
    Text(String),
    OidVector(Vec<Oid>),
    Int2Vector(Vec<i16>),
    Raw(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct CatalogRow {
    pub relation_oid: Oid,
    pub row_id: u64,
    pub values: Vec<CatalogValue>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VirtualCatalogPolicy {
    Static,
    Dynamic,
    Empty,
}

impl VirtualCatalogPolicy {
    pub const fn code(self) -> u8 {
        match self {
            Self::Static => VIRTUAL_CATALOG_STATIC,
            Self::Dynamic => VIRTUAL_CATALOG_DYNAMIC,
            Self::Empty => VIRTUAL_CATALOG_EMPTY,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VirtualCatalogRecord {
    pub relation_oid: Oid,
    pub name: &'static str,
    pub policy: VirtualCatalogPolicy,
}

pub fn virtual_catalogs() -> &'static [VirtualCatalogRecord] {
    generated_catalog::STATIC_VIRTUAL_CATALOGS
}

pub fn virtual_catalog_by_relation_oid(relation_oid: Oid) -> Option<VirtualCatalogRecord> {
    generated_catalog::STATIC_VIRTUAL_CATALOGS
        .iter()
        .copied()
        .find(|record| record.relation_oid == relation_oid)
}

pub fn virtual_catalog_by_name(name: &str, namespace: Oid) -> Option<VirtualCatalogRecord> {
    if namespace != PG_CATALOG_NAMESPACE_OID {
        return None;
    }
    let name = normalize_identifier(name);
    generated_catalog::STATIC_VIRTUAL_CATALOGS
        .iter()
        .copied()
        .find(|record| record.name == name.as_str())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PgNamespaceRecord {
    pub oid: Oid,
    pub name: &'static str,
    pub owner: Oid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PgOperatorRecord {
    pub oid: Oid,
    pub name: &'static str,
    pub namespace: Oid,
    pub owner: Oid,
    pub kind: u8,
    pub can_merge: bool,
    pub can_hash: bool,
    pub left_type: Oid,
    pub right_type: Oid,
    pub result_type: Oid,
    pub commutator: Oid,
    pub negator: Oid,
    pub code: Oid,
    pub rest: Oid,
    pub join: Oid,
}

pub fn builtin_operator_by_oid(oid: Oid) -> Option<&'static PgOperatorRecord> {
    generated_catalog::STATIC_OPERATORS
        .iter()
        .find(|record| record.oid == oid)
}

pub fn builtin_operator_by_signature(
    name: &str,
    left_type: Oid,
    right_type: Oid,
    namespace: Oid,
) -> Option<&'static PgOperatorRecord> {
    let name = normalize_identifier(name);
    generated_catalog::STATIC_OPERATORS.iter().find(|record| {
        record.name == name.as_str()
            && record.left_type == left_type
            && record.right_type == right_type
            && record.namespace == namespace
    })
}

pub fn builtin_operators_by_name(name: &str) -> impl Iterator<Item = &'static PgOperatorRecord> {
    let name = normalize_identifier(name);
    generated_catalog::STATIC_OPERATORS
        .iter()
        .filter(move |record| record.name == name.as_str())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PgCastRecord {
    pub oid: Oid,
    pub source_type: Oid,
    pub target_type: Oid,
    pub function: Oid,
    pub context: u8,
    pub method: u8,
}

pub fn builtin_cast_by_source_target(
    source_type: Oid,
    target_type: Oid,
) -> Option<&'static PgCastRecord> {
    generated_catalog::STATIC_CASTS
        .iter()
        .find(|record| record.source_type == source_type && record.target_type == target_type)
}

pub fn builtin_namespaces() -> &'static [PgNamespaceRecord] {
    generated_catalog::STATIC_NAMESPACES
}

pub fn builtin_namespace_by_oid(oid: Oid) -> Option<&'static PgNamespaceRecord> {
    generated_catalog::STATIC_NAMESPACES
        .iter()
        .find(|record| record.oid == oid)
}

pub fn builtin_namespace_by_name(name: &str) -> Option<&'static PgNamespaceRecord> {
    let name = normalize_identifier(name);
    generated_catalog::STATIC_NAMESPACES
        .iter()
        .find(|record| record.name == name.as_str())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelationName {
    pub namespace: Oid,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColumnRecord {
    pub name: String,
    pub type_oid: Oid,
    pub type_mod: i32,
    pub is_not_null: bool,
    pub has_default: bool,
    pub generated: u8,
}

impl ColumnRecord {
    pub fn new(name: impl Into<String>, type_oid: Oid, type_mod: i32, is_not_null: bool) -> Self {
        Self {
            name: normalize_identifier(&name.into()),
            type_oid,
            type_mod,
            is_not_null,
            has_default: false,
            generated: 0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhysicalColumnRecord {
    pub name: String,
    pub type_oid: Oid,
    pub type_mod: i32,
    pub is_not_null: bool,
    pub has_default: bool,
    pub generated: u8,
    pub is_dropped: bool,
    pub attlen: i16,
    pub attbyval: bool,
    pub attalign: u8,
    pub attstorage: u8,
    pub attcollation: Oid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelationRecord {
    pub row_id: u64,
    pub oid: Oid,
    pub type_oid: Oid,
    pub namespace: Oid,
    pub owner: Oid,
    pub name: String,
    pub relkind: u8,
    pub columns: Vec<ColumnRecord>,
    pub primary_key: Vec<String>,
    pub primary_key_constraint_oid: Option<Oid>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelationSummaryRecord {
    pub row_id: u64,
    pub oid: Oid,
    pub type_oid: Oid,
    pub namespace: Oid,
    pub owner: Oid,
    pub name: String,
    pub relkind: u8,
    pub column_count: u16,
    pub has_primary_key: bool,
    pub has_indexes: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexRecord {
    pub index_oid: Oid,
    pub relation_oid: Oid,
    pub key_attnums: Vec<i16>,
    pub is_unique: bool,
    pub nulls_not_distinct: bool,
    pub is_primary: bool,
    pub is_immediate: bool,
    pub is_valid: bool,
    pub is_ready: bool,
    pub is_live: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogError {
    pub sqlstate: String,
    pub message: String,
}

impl CatalogError {
    pub fn new(sqlstate: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            sqlstate: sqlstate.into(),
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct RelationMeta {
    row_id: u64,
    oid: Oid,
    type_oid: Oid,
    namespace: Oid,
    owner: Oid,
    name: String,
    relkind: u8,
    relnatts: u16,
    relhasindex: bool,
    row: CatalogRow,
}

#[derive(Clone, Debug, PartialEq)]
struct ColumnMeta {
    row_id: u64,
    relation_oid: Oid,
    attnum: i16,
    record: ColumnRecord,
    row: CatalogRow,
}

#[derive(Clone, Debug, PartialEq)]
struct TypeMeta {
    row_id: u64,
    record: CatalogTypeRecord,
    row: CatalogRow,
}

#[derive(Clone, Debug, PartialEq)]
struct IndexMeta {
    row_id: u64,
    record: IndexRecord,
    row: CatalogRow,
}

#[derive(Clone, Debug, PartialEq)]
struct ConstraintMeta {
    row_id: u64,
    oid: Oid,
    name: String,
    namespace: Oid,
    relation_oid: Oid,
    index_oid: Oid,
    kind: u8,
    key_attnums: Vec<i16>,
    row: CatalogRow,
}

#[derive(Clone, Debug, PartialEq)]
struct NamespaceMeta {
    row_id: u64,
    oid: Oid,
    name: String,
    owner: Oid,
    row: CatalogRow,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct CompatCatalogRows {
    rows: BTreeMap<u32, BTreeMap<u64, Arc<CatalogRow>>>,
    tombstones: BTreeMap<u32, BTreeSet<u64>>,
}

impl CompatCatalogRows {
    fn is_empty(&self) -> bool {
        self.rows.values().all(BTreeMap::is_empty)
            && self.tombstones.values().all(BTreeSet::is_empty)
    }

    fn insert(&mut self, row: CatalogRow) {
        self.tombstones
            .entry(row.relation_oid.0)
            .or_default()
            .remove(&row.row_id);
        self.rows
            .entry(row.relation_oid.0)
            .or_default()
            .insert(row.row_id, Arc::new(row));
    }

    fn untombstone(&mut self, relation_oid: Oid, row_id: u64) {
        if let Some(tombstones) = self.tombstones.get_mut(&relation_oid.0) {
            tombstones.remove(&row_id);
        }
    }

    fn delete(&mut self, relation_oid: Oid, row_id: u64) {
        self.rows.entry(relation_oid.0).or_default().remove(&row_id);
        self.tombstones
            .entry(relation_oid.0)
            .or_default()
            .insert(row_id);
    }

    fn merge(&mut self, other: CompatCatalogRows) {
        for (relation_oid, tombstones) in other.tombstones {
            for row_id in tombstones {
                self.delete(Oid(relation_oid), row_id);
            }
        }
        for rows in other.rows.into_values() {
            for row in rows.into_values() {
                self.insert(row.as_ref().clone());
            }
        }
    }

    fn apply_to_rows(&self, relation_oid: Oid, rows: &mut BTreeMap<u64, CatalogRow>) {
        if let Some(tombstones) = self.tombstones.get(&relation_oid.0) {
            for row_id in tombstones {
                rows.remove(row_id);
            }
        }
        if let Some(overlay_rows) = self.rows.get(&relation_oid.0) {
            for (row_id, row) in overlay_rows {
                rows.insert(*row_id, row.as_ref().clone());
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct CatalogSnapshot {
    generation: u64,
    namespaces: BTreeMap<u64, NamespaceMeta>,
    namespace_oids: BTreeMap<u32, u64>,
    namespace_names: BTreeMap<String, u64>,
    relations: BTreeMap<u64, RelationMeta>,
    relation_oids: BTreeMap<u32, u64>,
    relation_names: BTreeMap<(u32, String), u64>,
    columns: BTreeMap<u64, ColumnMeta>,
    columns_by_relation: BTreeMap<u32, BTreeMap<i16, u64>>,
    types: BTreeMap<u64, TypeMeta>,
    type_oids: BTreeMap<u32, u64>,
    type_names: BTreeMap<(u32, String), u64>,
    indexes: BTreeMap<u64, IndexMeta>,
    index_oids: BTreeMap<u32, u64>,
    indexes_by_relation: BTreeMap<u32, Vec<u64>>,
    constraints: BTreeMap<u64, ConstraintMeta>,
    constraint_oids: BTreeMap<u32, u64>,
    constraints_by_relation: BTreeMap<u32, Vec<u64>>,
    compat_rows: CompatCatalogRows,
}

impl CatalogSnapshot {
    fn new(generation: u64) -> Self {
        Self {
            generation,
            ..Self::default()
        }
    }

    fn rebuild_indexes(&mut self) {
        self.namespace_oids.clear();
        self.namespace_names.clear();
        for (row_id, namespace) in &self.namespaces {
            self.namespace_oids.insert(namespace.oid.0, *row_id);
            self.namespace_names
                .insert(normalize_identifier(&namespace.name), *row_id);
        }

        self.relation_oids.clear();
        self.relation_names.clear();
        for (row_id, relation) in &self.relations {
            self.relation_oids.insert(relation.oid.0, *row_id);
            self.relation_names.insert(
                (relation.namespace.0, normalize_identifier(&relation.name)),
                *row_id,
            );
        }

        self.columns_by_relation.clear();
        for (row_id, column) in &self.columns {
            self.columns_by_relation
                .entry(column.relation_oid.0)
                .or_default()
                .insert(column.attnum, *row_id);
        }

        self.type_oids.clear();
        self.type_names.clear();
        for (row_id, pg_type) in &self.types {
            self.type_oids.insert(pg_type.record.oid.0, *row_id);
            self.type_names.insert(
                (
                    pg_type.record.namespace.0,
                    normalize_identifier(&pg_type.record.name),
                ),
                *row_id,
            );
        }

        self.index_oids.clear();
        self.indexes_by_relation.clear();
        for (row_id, index) in &self.indexes {
            self.index_oids.insert(index.record.index_oid.0, *row_id);
            self.indexes_by_relation
                .entry(index.record.relation_oid.0)
                .or_default()
                .push(*row_id);
        }

        self.constraint_oids.clear();
        self.constraints_by_relation.clear();
        for (row_id, constraint) in &self.constraints {
            self.constraint_oids.insert(constraint.oid.0, *row_id);
            self.constraints_by_relation
                .entry(constraint.relation_oid.0)
                .or_default()
                .push(*row_id);
        }
    }

    fn relation_meta_by_oid(&self, oid: Oid) -> Option<&RelationMeta> {
        self.relation_oids
            .get(&oid.0)
            .and_then(|row_id| self.relations.get(row_id))
    }

    fn relation_meta_by_name(&self, name: &str, namespace: Oid) -> Option<&RelationMeta> {
        let name = normalize_identifier(name);
        self.relation_names
            .get(&(namespace.0, name))
            .and_then(|row_id| self.relations.get(row_id))
    }

    fn type_meta_by_oid(&self, oid: Oid) -> Option<&TypeMeta> {
        self.type_oids
            .get(&oid.0)
            .and_then(|row_id| self.types.get(row_id))
    }

    fn type_meta_by_name(&self, name: &str, namespace: Oid) -> Option<&TypeMeta> {
        let name = canonical_catalog_type_name(name);
        self.type_names
            .get(&(namespace.0, name))
            .and_then(|row_id| self.types.get(row_id))
    }

    fn index_meta_by_oid(&self, oid: Oid) -> Option<&IndexMeta> {
        self.index_oids
            .get(&oid.0)
            .and_then(|row_id| self.indexes.get(row_id))
    }

    fn index_metas_for_relation(&self, relation_oid: Oid) -> Vec<&IndexMeta> {
        self.indexes_by_relation
            .get(&relation_oid.0)
            .into_iter()
            .flat_map(|row_ids| row_ids.iter())
            .filter_map(|row_id| self.indexes.get(row_id))
            .collect()
    }

    fn constraint_metas_for_relation(&self, relation_oid: Oid) -> Vec<&ConstraintMeta> {
        self.constraints_by_relation
            .get(&relation_oid.0)
            .into_iter()
            .flat_map(|row_ids| row_ids.iter())
            .filter_map(|row_id| self.constraints.get(row_id))
            .collect()
    }

    fn remove_relation(&mut self, row_id: u64) {
        self.relations.remove(&row_id);
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct CatalogDraft {
    namespaces: BTreeMap<u64, Option<NamespaceMeta>>,
    relations: BTreeMap<u64, Option<RelationMeta>>,
    columns: BTreeMap<u64, Option<ColumnMeta>>,
    types: BTreeMap<u64, Option<TypeMeta>>,
    indexes: BTreeMap<u64, Option<IndexMeta>>,
    constraints: BTreeMap<u64, Option<ConstraintMeta>>,
    compat_rows: CompatCatalogRows,
}

impl CatalogDraft {
    fn is_empty(&self) -> bool {
        self.namespaces.is_empty()
            && self.relations.is_empty()
            && self.columns.is_empty()
            && self.types.is_empty()
            && self.indexes.is_empty()
            && self.constraints.is_empty()
            && self.compat_rows.is_empty()
    }

    fn merge(&mut self, other: CatalogDraft) {
        for (row_id, mutation) in &other.namespaces {
            if mutation.is_some() {
                self.compat_rows
                    .untombstone(PG_NAMESPACE_RELATION_OID, *row_id);
            }
        }
        for (row_id, mutation) in &other.relations {
            if mutation.is_some() {
                self.compat_rows.untombstone(PG_CLASS_RELATION_OID, *row_id);
            }
        }
        for (row_id, mutation) in &other.columns {
            if mutation.is_some() {
                self.compat_rows
                    .untombstone(PG_ATTRIBUTE_RELATION_OID, *row_id);
            }
        }
        for (row_id, mutation) in &other.types {
            if mutation.is_some() {
                self.compat_rows.untombstone(PG_TYPE_RELATION_OID, *row_id);
            }
        }
        for (row_id, mutation) in &other.indexes {
            if mutation.is_some() {
                self.compat_rows.untombstone(PG_INDEX_RELATION_OID, *row_id);
            }
        }
        for (row_id, mutation) in &other.constraints {
            if mutation.is_some() {
                self.compat_rows
                    .untombstone(PG_CONSTRAINT_RELATION_OID, *row_id);
            }
        }
        self.namespaces.extend(other.namespaces);
        self.relations.extend(other.relations);
        self.columns.extend(other.columns);
        self.types.extend(other.types);
        self.indexes.extend(other.indexes);
        self.constraints.extend(other.constraints);
        self.compat_rows.merge(other.compat_rows);
    }

    fn apply_to_snapshot(&self, snapshot: &mut CatalogSnapshot) {
        for (row_id, mutation) in &self.namespaces {
            match mutation {
                Some(namespace) => {
                    snapshot.namespaces.insert(*row_id, namespace.clone());
                }
                None => {
                    snapshot.namespaces.remove(row_id);
                }
            }
        }

        for (row_id, mutation) in &self.relations {
            match mutation {
                Some(relation) => {
                    snapshot.relations.insert(*row_id, relation.clone());
                }
                None => {
                    snapshot.remove_relation(*row_id);
                }
            }
        }

        for (row_id, mutation) in &self.columns {
            match mutation {
                Some(column) => {
                    snapshot.columns.insert(*row_id, column.clone());
                }
                None => {
                    snapshot.columns.remove(row_id);
                }
            }
        }

        for (row_id, mutation) in &self.types {
            match mutation {
                Some(pg_type) => {
                    snapshot.types.insert(*row_id, pg_type.clone());
                }
                None => {
                    snapshot.types.remove(row_id);
                }
            }
        }

        for (row_id, mutation) in &self.indexes {
            match mutation {
                Some(index) => {
                    snapshot.indexes.insert(*row_id, index.clone());
                }
                None => {
                    snapshot.indexes.remove(row_id);
                }
            }
        }

        for (row_id, mutation) in &self.constraints {
            match mutation {
                Some(constraint) => {
                    snapshot.constraints.insert(*row_id, constraint.clone());
                }
                None => {
                    snapshot.constraints.remove(row_id);
                }
            }
        }

        snapshot.compat_rows.merge(self.compat_rows.clone());
        snapshot.rebuild_indexes();
    }

    fn upsert_catalog_row(&mut self, table: &StaticCatalogTable, row: CatalogRow) {
        let row_id = row.row_id;
        let handled = match table.oid {
            PG_NAMESPACE_RELATION_OID => namespace_meta_from_row(table, &row, row_id)
                .map(|namespace| self.namespaces.insert(row_id, Some(namespace)))
                .is_some(),
            PG_CLASS_RELATION_OID => relation_meta_from_row(table, &row, row_id)
                .map(|relation| self.relations.insert(row_id, Some(relation)))
                .is_some(),
            PG_ATTRIBUTE_RELATION_OID => column_meta_from_row(table, &row, row_id)
                .map(|column| self.columns.insert(row_id, Some(column)))
                .is_some(),
            PG_TYPE_RELATION_OID => type_meta_from_row(table, &row, row_id)
                .map(|pg_type| self.types.insert(row_id, Some(pg_type)))
                .is_some(),
            PG_INDEX_RELATION_OID => index_meta_from_row(table, &row, row_id)
                .map(|index| self.indexes.insert(row_id, Some(index)))
                .is_some(),
            PG_CONSTRAINT_RELATION_OID => constraint_meta_from_row(table, &row, row_id)
                .map(|constraint| self.constraints.insert(row_id, Some(constraint)))
                .is_some(),
            _ => false,
        };
        if handled {
            self.compat_rows.untombstone(table.oid, row_id);
        } else {
            self.compat_rows.insert(row);
        }
    }

    fn delete_catalog_row(&mut self, relation_oid: Oid, row_id: u64) {
        match relation_oid {
            PG_NAMESPACE_RELATION_OID => {
                self.namespaces.insert(row_id, None);
            }
            PG_CLASS_RELATION_OID => {
                self.relations.insert(row_id, None);
            }
            PG_ATTRIBUTE_RELATION_OID => {
                self.columns.insert(row_id, None);
            }
            PG_TYPE_RELATION_OID => {
                self.types.insert(row_id, None);
            }
            PG_INDEX_RELATION_OID => {
                self.indexes.insert(row_id, None);
            }
            PG_CONSTRAINT_RELATION_OID => {
                self.constraints.insert(row_id, None);
            }
            _ => {}
        }
        self.compat_rows.delete(relation_oid, row_id);
    }
}

fn namespace_meta_from_row(
    table: &StaticCatalogTable,
    row: &CatalogRow,
    row_id: u64,
) -> Option<NamespaceMeta> {
    let oid = catalog_row_value(table, row, "oid").and_then(catalog_value_oid)?;
    let name = catalog_row_value(table, row, "nspname").and_then(catalog_value_string)?;
    let owner = catalog_row_value(table, row, "nspowner")
        .and_then(catalog_value_oid)
        .unwrap_or(POSTGRES_ROLE_OID);
    Some(NamespaceMeta {
        row_id,
        oid,
        name: normalize_identifier(&name),
        owner,
        row: row.clone(),
    })
}

fn relation_meta_from_row(
    table: &StaticCatalogTable,
    row: &CatalogRow,
    row_id: u64,
) -> Option<RelationMeta> {
    let oid = catalog_row_value(table, row, "oid").and_then(catalog_value_oid)?;
    let type_oid = catalog_row_value(table, row, "reltype")
        .and_then(catalog_value_oid)
        .unwrap_or(INVALID_OID);
    let namespace = catalog_row_value(table, row, "relnamespace")
        .and_then(catalog_value_oid)
        .unwrap_or(PUBLIC_NAMESPACE_OID);
    let owner = catalog_row_value(table, row, "relowner")
        .and_then(catalog_value_oid)
        .unwrap_or(POSTGRES_ROLE_OID);
    let name = catalog_row_value(table, row, "relname").and_then(catalog_value_string)?;
    let relkind = catalog_row_value(table, row, "relkind")
        .and_then(catalog_value_u8)
        .unwrap_or(b'r');
    let relnatts = catalog_row_value(table, row, "relnatts")
        .and_then(catalog_value_i16)
        .map(|value| value.max(0) as u16)
        .unwrap_or(0);
    let relhasindex = catalog_row_value(table, row, "relhasindex")
        .and_then(catalog_value_bool)
        .unwrap_or(false);
    Some(RelationMeta {
        row_id,
        oid,
        type_oid,
        namespace,
        owner,
        name: normalize_identifier(&name),
        relkind,
        relnatts,
        relhasindex,
        row: row.clone(),
    })
}

fn column_meta_from_row(
    table: &StaticCatalogTable,
    row: &CatalogRow,
    row_id: u64,
) -> Option<ColumnMeta> {
    let relation_oid = catalog_row_value(table, row, "attrelid").and_then(catalog_value_oid)?;
    let (attnum, record) = column_record_from_pg_attribute_row(table, row, relation_oid)?;
    Some(ColumnMeta {
        row_id,
        relation_oid,
        attnum,
        record,
        row: row.clone(),
    })
}

fn type_meta_from_row(
    table: &StaticCatalogTable,
    row: &CatalogRow,
    row_id: u64,
) -> Option<TypeMeta> {
    let oid = catalog_row_value(table, row, "oid").and_then(catalog_value_oid)?;
    let name = catalog_row_value(table, row, "typname").and_then(catalog_value_string)?;
    let record = CatalogTypeRecord {
        row_id,
        oid,
        name: canonical_catalog_type_name(&name),
        namespace: catalog_row_value(table, row, "typnamespace")
            .and_then(catalog_value_oid)
            .unwrap_or(PG_CATALOG_NAMESPACE_OID),
        owner: catalog_row_value(table, row, "typowner")
            .and_then(catalog_value_oid)
            .unwrap_or(POSTGRES_ROLE_OID),
        typlen: catalog_row_value(table, row, "typlen")
            .and_then(catalog_value_i16)
            .unwrap_or(-1),
        typbyval: catalog_row_value(table, row, "typbyval")
            .and_then(catalog_value_bool)
            .unwrap_or(false),
        typalign: catalog_row_value(table, row, "typalign")
            .and_then(catalog_value_u8)
            .unwrap_or(b'i'),
        typdelim: catalog_row_value(table, row, "typdelim")
            .and_then(catalog_value_u8)
            .unwrap_or(b','),
        typinput: catalog_row_value(table, row, "typinput")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typoutput: catalog_row_value(table, row, "typoutput")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typreceive: catalog_row_value(table, row, "typreceive")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typsend: catalog_row_value(table, row, "typsend")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typmodin: catalog_row_value(table, row, "typmodin")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typmodout: catalog_row_value(table, row, "typmodout")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typisdefined: catalog_row_value(table, row, "typisdefined")
            .and_then(catalog_value_bool)
            .unwrap_or(true),
        typtype: catalog_row_value(table, row, "typtype")
            .and_then(catalog_value_u8)
            .unwrap_or(b'b'),
        typcategory: catalog_row_value(table, row, "typcategory")
            .and_then(catalog_value_u8)
            .unwrap_or(b'U'),
        typispreferred: catalog_row_value(table, row, "typispreferred")
            .and_then(catalog_value_bool)
            .unwrap_or(false),
        typrelid: catalog_row_value(table, row, "typrelid")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typelem: catalog_row_value(table, row, "typelem")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typarray: catalog_row_value(table, row, "typarray")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typbasetype: catalog_row_value(table, row, "typbasetype")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typtypmod: catalog_row_value(table, row, "typtypmod")
            .and_then(catalog_value_i32)
            .unwrap_or(-1),
        typcollation: catalog_row_value(table, row, "typcollation")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typsubscript: catalog_row_value(table, row, "typsubscript")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typstorage: catalog_row_value(table, row, "typstorage")
            .and_then(catalog_value_u8)
            .unwrap_or(b'p'),
    };
    Some(TypeMeta {
        row_id,
        record,
        row: row.clone(),
    })
}

fn index_meta_from_row(
    _table: &StaticCatalogTable,
    row: &CatalogRow,
    row_id: u64,
) -> Option<IndexMeta> {
    Some(IndexMeta {
        row_id,
        record: pg_index_record_from_row(row)?,
        row: row.clone(),
    })
}

fn constraint_meta_from_row(
    table: &StaticCatalogTable,
    row: &CatalogRow,
    row_id: u64,
) -> Option<ConstraintMeta> {
    let oid = catalog_row_value(table, row, "oid").and_then(catalog_value_oid)?;
    let name = catalog_row_value(table, row, "conname")
        .and_then(catalog_value_string)
        .unwrap_or_default();
    let namespace = catalog_row_value(table, row, "connamespace")
        .and_then(catalog_value_oid)
        .unwrap_or(PUBLIC_NAMESPACE_OID);
    let relation_oid = catalog_row_value(table, row, "conrelid")
        .and_then(catalog_value_oid)
        .unwrap_or(INVALID_OID);
    let index_oid = catalog_row_value(table, row, "conindid")
        .and_then(catalog_value_oid)
        .unwrap_or(INVALID_OID);
    let kind = catalog_row_value(table, row, "contype")
        .and_then(catalog_value_u8)
        .unwrap_or(0);
    let key_attnums = catalog_row_value(table, row, "conkey")
        .and_then(catalog_value_int2_vector)
        .unwrap_or_default();
    Some(ConstraintMeta {
        row_id,
        oid,
        name: normalize_identifier(&name),
        namespace,
        relation_oid,
        index_oid,
        kind,
        key_attnums,
        row: row.clone(),
    })
}

#[derive(Debug)]
struct CatalogState {
    next_overlay_row_ids: BTreeMap<u32, u64>,
    snapshot: CatalogSnapshot,
}

impl Default for CatalogState {
    fn default() -> Self {
        Self {
            next_overlay_row_ids: BTreeMap::new(),
            snapshot: CatalogSnapshot::new(1),
        }
    }
}

static CATALOG: OnceLock<RwLock<CatalogState>> = OnceLock::new();
static CATALOG_GENERATION: AtomicU64 = AtomicU64::new(1);
static PRIMARY_KEY_INDEX_OID_CACHE: OnceLock<Mutex<PrimaryKeyIndexOidCache>> = OnceLock::new();
static CATALOG_LOOKUP_CACHE: OnceLock<Mutex<CatalogLookupCache>> = OnceLock::new();

#[derive(Debug)]
struct PrimaryKeyIndexOidCache {
    generation: u64,
    entries: BTreeMap<u32, Option<Oid>>,
}

impl Default for PrimaryKeyIndexOidCache {
    fn default() -> Self {
        Self {
            generation: current_generation(),
            entries: BTreeMap::new(),
        }
    }
}

#[derive(Debug)]
struct CatalogLookupCache {
    generation: u64,
    relation_columns: BTreeMap<(u32, i16), Option<ColumnRecord>>,
    index_records_by_oid: BTreeMap<u32, Option<IndexRecord>>,
    index_records_by_relation: BTreeMap<u32, Vec<IndexRecord>>,
    unique_index_records_by_relation: BTreeMap<u32, Vec<IndexRecord>>,
}

impl Default for CatalogLookupCache {
    fn default() -> Self {
        Self {
            generation: current_generation(),
            relation_columns: BTreeMap::new(),
            index_records_by_oid: BTreeMap::new(),
            index_records_by_relation: BTreeMap::new(),
            unique_index_records_by_relation: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Default)]
pub struct CatalogSession {
    transaction_stack: Vec<CatalogDraft>,
    explicit_transaction: bool,
}

pub type CatalogSessionHandle = Arc<Mutex<CatalogSession>>;

pub fn new_catalog_session() -> CatalogSessionHandle {
    Arc::new(Mutex::new(CatalogSession::default()))
}

static DEFAULT_CATALOG_SESSION: OnceLock<CatalogSessionHandle> = OnceLock::new();

thread_local! {
    static CURRENT_CATALOG_SESSION: RefCell<Option<CatalogSessionHandle>> = const { RefCell::new(None) };
}

#[derive(Debug)]
pub struct CatalogSessionGuard {
    previous: Option<CatalogSessionHandle>,
}

pub fn enter_catalog_session(handle: CatalogSessionHandle) -> CatalogSessionGuard {
    let previous = CURRENT_CATALOG_SESSION.with(|slot| slot.replace(Some(handle)));
    CatalogSessionGuard { previous }
}

impl Drop for CatalogSessionGuard {
    fn drop(&mut self) {
        CURRENT_CATALOG_SESSION.with(|slot| {
            slot.replace(self.previous.take());
        });
    }
}

fn default_catalog_session() -> CatalogSessionHandle {
    DEFAULT_CATALOG_SESSION
        .get_or_init(new_catalog_session)
        .clone()
}

fn current_catalog_session() -> CatalogSessionHandle {
    CURRENT_CATALOG_SESSION
        .with(|slot| slot.borrow().clone())
        .unwrap_or_else(default_catalog_session)
}

fn catalog() -> &'static RwLock<CatalogState> {
    CATALOG.get_or_init(|| RwLock::new(CatalogState::default()))
}

fn with_catalog<R>(f: impl FnOnce(&mut CatalogState) -> R) -> R {
    match catalog().write() {
        Ok(mut state) => f(&mut state),
        Err(poisoned) => {
            let mut state = poisoned.into_inner();
            f(&mut state)
        }
    }
}

fn with_catalog_read<R>(f: impl FnOnce(&CatalogState) -> R) -> R {
    match catalog().read() {
        Ok(state) => f(&state),
        Err(poisoned) => {
            let state = poisoned.into_inner();
            f(&state)
        }
    }
}

fn with_catalog_session<R>(f: impl FnOnce(&mut CatalogSession) -> R) -> R {
    let session = current_catalog_session();
    match session.lock() {
        Ok(mut session) => f(&mut session),
        Err(poisoned) => {
            let mut session = poisoned.into_inner();
            f(&mut session)
        }
    }
}

fn visible_session_drafts() -> Vec<CatalogDraft> {
    let session = current_catalog_session();
    match session.lock() {
        Ok(session) => session
            .transaction_stack
            .iter()
            .filter(|draft| !draft.is_empty())
            .cloned()
            .collect(),
        Err(poisoned) => poisoned
            .into_inner()
            .transaction_stack
            .iter()
            .filter(|draft| !draft.is_empty())
            .cloned()
            .collect(),
    }
}

fn with_visible_catalog_snapshot<R>(f: impl FnOnce(&CatalogSnapshot) -> R) -> R {
    let session_drafts = visible_session_drafts();
    if session_drafts.is_empty() {
        return with_catalog_read(|state| f(&state.snapshot));
    }

    with_catalog_read(|state| {
        let mut visible_snapshot = state.snapshot.clone();
        for draft in session_drafts {
            draft.apply_to_snapshot(&mut visible_snapshot);
        }
        f(&visible_snapshot)
    })
}

pub fn has_uncommitted_catalog_changes() -> bool {
    let session = current_catalog_session();
    match session.lock() {
        Ok(session) => session
            .transaction_stack
            .iter()
            .any(|overlay| !overlay.is_empty()),
        Err(poisoned) => poisoned
            .into_inner()
            .transaction_stack
            .iter()
            .any(|overlay| !overlay.is_empty()),
    }
}

fn ensure_catalog_transaction(session: &mut CatalogSession) {
    if session.transaction_stack.is_empty() {
        session.transaction_stack.push(CatalogDraft::default());
    }
}

fn commit_catalog_draft(draft: CatalogDraft) {
    if draft.is_empty() {
        return;
    }
    with_catalog(|state| {
        draft.apply_to_snapshot(&mut state.snapshot);
        bump_generation(state);
    });
}

fn commit_top_catalog_draft(session: &mut CatalogSession) {
    let Some(draft) = session.transaction_stack.pop() else {
        return;
    };
    if let Some(parent) = session.transaction_stack.last_mut() {
        parent.merge(draft);
    } else {
        commit_catalog_draft(draft);
    }
}

pub fn begin_explicit_transaction() {
    with_catalog_session(|session| {
        if !session.explicit_transaction {
            while !session.transaction_stack.is_empty() {
                commit_top_catalog_draft(session);
            }
        }
        ensure_catalog_transaction(session);
        session.explicit_transaction = true;
    });
}

pub fn begin_implicit_transaction() {
    with_catalog_session(ensure_catalog_transaction);
}

pub fn commit_explicit_transaction() {
    with_catalog_session(|session| {
        while !session.transaction_stack.is_empty() {
            commit_top_catalog_draft(session);
        }
        session.explicit_transaction = false;
    });
}

pub fn abort_explicit_transaction() {
    with_catalog_session(|session| {
        session.transaction_stack.clear();
        session.explicit_transaction = false;
    });
}

pub fn commit_implicit_transaction() {
    with_catalog_session(|session| {
        if session.explicit_transaction {
            return;
        }
        while !session.transaction_stack.is_empty() {
            commit_top_catalog_draft(session);
        }
    });
}

pub fn abort_implicit_transaction() {
    with_catalog_session(|session| {
        if !session.explicit_transaction {
            session.transaction_stack.clear();
        }
    });
}

pub fn begin_subtransaction() {
    with_catalog_session(|session| {
        ensure_catalog_transaction(session);
        session.transaction_stack.push(CatalogDraft::default());
    });
}

pub fn commit_subtransaction() {
    with_catalog_session(|session| {
        if session.transaction_stack.len() > 1 {
            commit_top_catalog_draft(session);
        }
    });
}

pub fn abort_subtransaction() {
    with_catalog_session(|session| {
        if session.transaction_stack.len() > 1 {
            session.transaction_stack.pop();
        }
    });
}

fn normalize_identifier(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

pub fn static_catalogs() -> &'static [StaticCatalogTable] {
    generated_catalog::STATIC_CATALOG_TABLES
}

pub fn static_catalog_by_relation_oid(relation_oid: Oid) -> Option<&'static StaticCatalogTable> {
    generated_catalog::STATIC_CATALOG_TABLES
        .iter()
        .find(|table| table.oid == relation_oid)
}

pub fn static_catalog_by_name(name: &str) -> Option<&'static StaticCatalogTable> {
    let name = normalize_identifier(name);
    generated_catalog::STATIC_CATALOG_TABLES
        .iter()
        .find(|table| table.name == name.as_str())
}

fn synthetic_catalog_rowtype_oid(relation_oid: Oid) -> Oid {
    Oid(SYNTHETIC_CATALOG_ROWTYPE_OID_BASE | relation_oid.0)
}

fn static_catalog_table_rowtype_oid(table: &StaticCatalogTable) -> Oid {
    if table.rowtype_oid != INVALID_OID {
        table.rowtype_oid
    } else {
        synthetic_catalog_rowtype_oid(table.oid)
    }
}

pub fn static_catalog_rowtype_oid(relation_oid: Oid) -> Option<Oid> {
    static_catalog_by_relation_oid(relation_oid).map(static_catalog_table_rowtype_oid)
}

fn static_catalog_by_rowtype_oid(rowtype_oid: Oid) -> Option<&'static StaticCatalogTable> {
    generated_catalog::STATIC_CATALOG_TABLES
        .iter()
        .find(|table| static_catalog_table_rowtype_oid(table) == rowtype_oid)
}

fn static_value_as_u32(value: StaticCatalogValue) -> Option<u32> {
    match value {
        StaticCatalogValue::Raw("-") => Some(0),
        StaticCatalogValue::Raw("NAMEDATALEN") => Some(64),
        StaticCatalogValue::Raw(value) => value.parse::<u32>().ok(),
        StaticCatalogValue::Null => None,
    }
}

fn static_value_as_i32(value: StaticCatalogValue) -> Option<i32> {
    match value {
        StaticCatalogValue::Raw("-") => Some(0),
        StaticCatalogValue::Raw("NAMEDATALEN") => Some(64),
        StaticCatalogValue::Raw(value) => value.parse::<i32>().ok(),
        StaticCatalogValue::Null => None,
    }
}

fn static_value_as_f32(value: StaticCatalogValue) -> Option<f32> {
    match value {
        StaticCatalogValue::Raw(value) => value.parse::<f32>().ok(),
        StaticCatalogValue::Null => None,
    }
}

fn static_value_as_bool(value: StaticCatalogValue) -> Option<bool> {
    match value {
        StaticCatalogValue::Raw("t") => Some(true),
        StaticCatalogValue::Raw("f") => Some(false),
        StaticCatalogValue::Raw("true") => Some(true),
        StaticCatalogValue::Raw("false") => Some(false),
        StaticCatalogValue::Null => None,
        StaticCatalogValue::Raw(_) => None,
    }
}

fn static_value_as_char(value: StaticCatalogValue) -> Option<u8> {
    match value {
        StaticCatalogValue::Raw("\\0") => Some(0),
        StaticCatalogValue::Raw(value) => value.as_bytes().first().copied().or(Some(0)),
        StaticCatalogValue::Null => None,
    }
}

fn parse_oid_vector(value: &str) -> Vec<Oid> {
    value
        .split_whitespace()
        .filter_map(|part| part.parse::<u32>().ok())
        .map(Oid)
        .collect()
}

fn parse_int2_vector(value: &str) -> Vec<i16> {
    value
        .split_whitespace()
        .filter_map(|part| part.parse::<i16>().ok())
        .collect()
}

fn parse_pg_array_elements(value: &str) -> Option<Vec<Option<String>>> {
    let inner = value.strip_prefix('{')?.strip_suffix('}')?;
    let mut elements = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut saw_quote = false;
    for ch in inner.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quoted => escaped = true,
            '"' => {
                quoted = !quoted;
                saw_quote = true;
            }
            ',' if !quoted => {
                if !saw_quote && current == "NULL" {
                    elements.push(None);
                } else {
                    elements.push(Some(std::mem::take(&mut current)));
                }
                saw_quote = false;
            }
            _ => current.push(ch),
        }
    }
    if !saw_quote && current == "NULL" {
        elements.push(None);
    } else if inner.is_empty() {
        return Some(Vec::new());
    } else {
        elements.push(Some(current));
    }
    Some(elements)
}

fn static_value_to_catalog_value(
    column: &StaticCatalogColumn,
    value: StaticCatalogValue,
) -> CatalogValue {
    match value {
        StaticCatalogValue::Null => CatalogValue::Null,
        StaticCatalogValue::Raw(raw) => match column.type_oid {
            BOOL_OID => CatalogValue::Bool(static_value_as_bool(value).unwrap_or(false)),
            CHAR_OID => CatalogValue::Char(static_value_as_char(value).unwrap_or(0)),
            INT2_OID => CatalogValue::Int16(static_value_as_i32(value).unwrap_or(0) as i16),
            INT4_OID => CatalogValue::Int32(static_value_as_i32(value).unwrap_or(0)),
            FLOAT4_OID => CatalogValue::Float32(static_value_as_f32(value).unwrap_or(0.0)),
            NAME_OID => CatalogValue::Name(raw.to_owned()),
            TEXT_OID | PG_NODE_TREE_OID => CatalogValue::Text(raw.to_owned()),
            OIDVECTOR_OID => CatalogValue::OidVector(parse_oid_vector(raw)),
            INT2VECTOR_OID => CatalogValue::Int2Vector(parse_int2_vector(raw)),
            OID_OID | REGCLASS_OID => {
                CatalogValue::Oid(Oid(static_value_as_u32(value).unwrap_or(0)))
            }
            _ if column.type_name.starts_with("reg") || column.type_name == "oid" => {
                CatalogValue::Oid(Oid(static_value_as_u32(value).unwrap_or(0)))
            }
            _ if column.type_name.starts_with('_') => CatalogValue::Raw(raw.to_owned()),
            _ => CatalogValue::Raw(raw.to_owned()),
        },
    }
}

fn oid_vector_value(value: &CatalogValue) -> Option<Vec<Oid>> {
    match value {
        CatalogValue::OidVector(values) => Some(values.clone()),
        CatalogValue::Raw(value) | CatalogValue::Text(value) | CatalogValue::Name(value) => {
            Some(parse_oid_vector(value))
        }
        _ => None,
    }
}

fn constvalue_bytes_for_default(type_oid: Oid, default: &str) -> Option<Vec<u8>> {
    match type_oid {
        BOOL_OID => Some(vec![u8::from(matches!(default, "t" | "true" | "1"))]),
        INT2_OID => default
            .parse::<i16>()
            .ok()
            .map(|value| value.to_ne_bytes().to_vec()),
        INT4_OID => default
            .parse::<i32>()
            .ok()
            .map(|value| value.to_ne_bytes().to_vec()),
        INT8_OID => default
            .parse::<i64>()
            .ok()
            .map(|value| value.to_ne_bytes().to_vec()),
        OID_OID | REGCLASS_OID => default
            .parse::<u32>()
            .ok()
            .map(|value| value.to_ne_bytes().to_vec()),
        FLOAT4_OID => default
            .parse::<f32>()
            .ok()
            .map(|value| value.to_ne_bytes().to_vec()),
        FLOAT8_OID => default
            .parse::<f64>()
            .ok()
            .map(|value| value.to_ne_bytes().to_vec()),
        TEXT_OID | VARCHAR_OID | BPCHAR_OID | NAME_OID => {
            let mut bytes = Vec::with_capacity(4 + default.len());
            let total_len = 4 + default.len();
            bytes.extend_from_slice(&((total_len as u32) << 2).to_ne_bytes());
            bytes.extend_from_slice(default.as_bytes());
            Some(bytes)
        }
        _ => None,
    }
}

fn const_node_for_default(type_oid: Oid, default: Option<&str>) -> Option<String> {
    let type_record = lookup_builtin_type(type_oid)?;
    let is_null = default.is_none();
    let constvalue = if let Some(default) = default {
        let mut bytes = constvalue_bytes_for_default(type_oid, default)?;
        if type_record.typbyval {
            bytes.resize(8, 0);
        }
        let bytes = bytes
            .iter()
            .map(|byte| (*byte as i8).to_string())
            .collect::<Vec<_>>()
            .join(" ");
        format!("{} [ {bytes} ]", type_record.typlen)
    } else {
        "<>".to_owned()
    };
    Some(format!(
        "{{CONST :consttype {} :consttypmod -1 :constcollid {} :constlen {} :constbyval {} :constisnull {} :location -1 :constvalue {}}}",
        type_oid.0,
        type_record.typcollation.0,
        type_record.typlen,
        if type_record.typbyval {
            "true"
        } else {
            "false"
        },
        if is_null { "true" } else { "false" },
        constvalue
    ))
}

fn normalize_pg_proc_bootstrap_defaults(table: &StaticCatalogTable, values: &mut [CatalogValue]) {
    if table.oid != PG_PROC_RELATION_OID {
        return;
    }
    let Some(pronargs_index) = table
        .columns
        .iter()
        .position(|column| column.name == "pronargs")
    else {
        return;
    };
    let Some(pronargdefaults_index) = table
        .columns
        .iter()
        .position(|column| column.name == "pronargdefaults")
    else {
        return;
    };
    let Some(proargtypes_index) = table
        .columns
        .iter()
        .position(|column| column.name == "proargtypes")
    else {
        return;
    };
    let Some(proargdefaults_index) = table
        .columns
        .iter()
        .position(|column| column.name == "proargdefaults")
    else {
        return;
    };
    let Some(defaults_text) = values
        .get(proargdefaults_index)
        .and_then(catalog_value_string)
    else {
        return;
    };
    if !defaults_text.starts_with('{') {
        return;
    }
    let Some(defaults) = parse_pg_array_elements(&defaults_text) else {
        return;
    };
    let Some(pronargs) = values.get(pronargs_index).and_then(catalog_value_i16) else {
        return;
    };
    let Some(arg_types) = values.get(proargtypes_index).and_then(oid_vector_value) else {
        return;
    };
    let pronargs = usize::from(u16::try_from(pronargs).unwrap_or(0));
    if defaults.len() > pronargs || arg_types.len() < pronargs {
        return;
    }
    let default_arg_types = &arg_types[pronargs - defaults.len()..pronargs];
    let Some(nodes) = default_arg_types
        .iter()
        .zip(defaults.iter())
        .map(|(type_oid, default)| const_node_for_default(*type_oid, default.as_deref()))
        .collect::<Option<Vec<_>>>()
    else {
        return;
    };
    values[pronargdefaults_index] =
        CatalogValue::Int16(defaults.len().min(i16::MAX as usize) as i16);
    values[proargdefaults_index] = CatalogValue::Text(format!("({})", nodes.join(" ")));
}

fn static_row_to_catalog_row(table: &StaticCatalogTable, row: &StaticCatalogRow) -> CatalogRow {
    let mut values = row
        .values
        .iter()
        .copied()
        .zip(table.columns.iter())
        .map(|(value, column)| static_value_to_catalog_value(column, value))
        .collect::<Vec<_>>();
    normalize_pg_proc_bootstrap_defaults(table, &mut values);
    CatalogRow {
        relation_oid: table.oid,
        row_id: row.row_id,
        values,
    }
}

fn static_row_column_value(
    table: &StaticCatalogTable,
    row: &StaticCatalogRow,
    column_name: &str,
) -> Option<CatalogValue> {
    let column_index = table
        .columns
        .iter()
        .position(|column| column.name == column_name)?;
    let column = table.columns.get(column_index)?;
    let value = row.values.get(column_index).copied()?;
    Some(static_value_to_catalog_value(column, value))
}

fn static_row_column_oid(
    table: &StaticCatalogTable,
    row: &StaticCatalogRow,
    column_name: &str,
) -> Option<Oid> {
    static_row_column_value(table, row, column_name).and_then(|value| catalog_value_oid(&value))
}

fn static_row_column_bool(
    table: &StaticCatalogTable,
    row: &StaticCatalogRow,
    column_name: &str,
) -> Option<bool> {
    static_row_column_value(table, row, column_name).and_then(|value| catalog_value_bool(&value))
}

fn static_row_column_i16(
    table: &StaticCatalogTable,
    row: &StaticCatalogRow,
    column_name: &str,
) -> Option<i16> {
    static_row_column_value(table, row, column_name).and_then(|value| catalog_value_i16(&value))
}

fn static_row_column_identifier_matches(
    table: &StaticCatalogTable,
    row: &StaticCatalogRow,
    column_name: &str,
    normalized: &str,
) -> bool {
    let Some(column_index) = table
        .columns
        .iter()
        .position(|column| column.name == column_name)
    else {
        return false;
    };
    match row.values.get(column_index).copied() {
        Some(StaticCatalogValue::Raw(candidate)) => {
            candidate == normalized || normalize_identifier(candidate) == normalized
        }
        Some(value) => table
            .columns
            .get(column_index)
            .map(|column| static_value_to_catalog_value(column, value))
            .is_some_and(|value| catalog_value_identifier_matches(&value, normalized)),
        None => false,
    }
}

fn catalog_row_identifier_matches(
    table: &StaticCatalogTable,
    row: &CatalogRow,
    column_name: &str,
    normalized: &str,
) -> bool {
    catalog_row_value(table, row, column_name)
        .is_some_and(|value| catalog_value_identifier_matches(value, normalized))
}

fn catalog_rows_matching_static<StaticMatches, RowMatches>(
    relation_oid: Oid,
    _static_matches: StaticMatches,
    row_matches: RowMatches,
) -> Vec<CatalogRow>
where
    StaticMatches: Fn(&StaticCatalogTable, &StaticCatalogRow) -> bool,
    RowMatches: Fn(&CatalogRow) -> bool,
{
    if static_catalog_by_relation_oid(relation_oid).is_none() {
        return Vec::new();
    }
    catalog_rows(relation_oid)
        .into_iter()
        .filter(row_matches)
        .collect()
}

pub fn catalog_rows(relation_oid: Oid) -> Vec<CatalogRow> {
    let Some(table) = static_catalog_by_relation_oid(relation_oid) else {
        return Vec::new();
    };
    with_visible_catalog_snapshot(|snapshot| {
        let mut rows = BTreeMap::<u64, CatalogRow>::new();
        for row in table.rows {
            rows.insert(row.row_id, static_row_to_catalog_row(table, row));
        }
        match relation_oid {
            PG_NAMESPACE_RELATION_OID => {
                for namespace in snapshot.namespaces.values() {
                    rows.insert(namespace.row_id, namespace.row.clone());
                }
            }
            PG_CLASS_RELATION_OID => {
                for relation in snapshot.relations.values() {
                    rows.insert(relation.row_id, relation.row.clone());
                }
            }
            PG_ATTRIBUTE_RELATION_OID => {
                for column in snapshot.columns.values() {
                    rows.insert(column.row_id, column.row.clone());
                }
            }
            PG_TYPE_RELATION_OID => {
                for pg_type in snapshot.types.values() {
                    rows.insert(pg_type.row_id, pg_type.row.clone());
                }
            }
            PG_INDEX_RELATION_OID => {
                for index in snapshot.indexes.values() {
                    rows.insert(index.row_id, index.row.clone());
                }
            }
            PG_CONSTRAINT_RELATION_OID => {
                for constraint in snapshot.constraints.values() {
                    rows.insert(constraint.row_id, constraint.row.clone());
                }
            }
            _ => {}
        }
        snapshot.compat_rows.apply_to_rows(relation_oid, &mut rows);
        rows.into_values().collect()
    })
}

fn canonical_database_name(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        "postgres".to_owned()
    } else {
        name.to_owned()
    }
}

fn synthetic_database_oid(name: &str) -> Oid {
    match name {
        "template1" => TEMPLATE1_DATABASE_OID,
        "template0" => TEMPLATE0_DATABASE_OID,
        "postgres" => POSTGRES_DATABASE_OID,
        _ => {
            let mut hash = 0x811c_9dc5u32;
            for byte in name.as_bytes() {
                hash ^= u32::from(*byte);
                hash = hash.wrapping_mul(0x0100_0193);
            }
            Oid(SYNTHETIC_DATABASE_OID_BASE | (hash & 0x00ff_ffff))
        }
    }
}

fn database_oid_by_name(name: &str) -> Option<Oid> {
    let table = static_catalog_by_relation_oid(PG_DATABASE_RELATION_OID)?;
    catalog_rows(PG_DATABASE_RELATION_OID)
        .into_iter()
        .find(|row| catalog_row_identifier_matches(table, row, "datname", name))
        .and_then(|row| catalog_row_value(table, &row, "oid").and_then(catalog_value_oid))
}

fn database_row_by_oid(oid: Oid) -> Option<CatalogRow> {
    let table = static_catalog_by_relation_oid(PG_DATABASE_RELATION_OID)?;
    catalog_rows(PG_DATABASE_RELATION_OID)
        .into_iter()
        .find(|row| {
            catalog_row_value(table, row, "oid")
                .and_then(catalog_value_oid)
                .is_some_and(|row_oid| row_oid == oid)
        })
}

fn synthetic_database_row(table: &StaticCatalogTable, oid: Oid, name: &str) -> Option<CatalogRow> {
    let base = table.rows.iter().find(|row| {
        static_row_column_oid(table, row, "oid")
            .is_some_and(|row_oid| row_oid == TEMPLATE1_DATABASE_OID)
    })?;
    let mut row = static_row_to_catalog_row(table, base);
    row.row_id = u64::from(oid.0);
    set_catalog_row_value(table, &mut row, "oid", CatalogValue::Oid(oid));
    set_catalog_row_value(
        table,
        &mut row,
        "datname",
        CatalogValue::Name(name.to_owned()),
    );
    set_catalog_row_value(table, &mut row, "datistemplate", CatalogValue::Bool(false));
    set_catalog_row_value(table, &mut row, "datallowconn", CatalogValue::Bool(true));
    set_catalog_row_value(table, &mut row, "dathasloginevt", CatalogValue::Bool(false));
    set_catalog_row_value(table, &mut row, "datconnlimit", CatalogValue::Int32(-1));
    Some(row)
}

pub fn ensure_database(name: &str) -> Oid {
    let name = canonical_database_name(name);
    if let Some(oid) = database_oid_by_name(&name) {
        return oid;
    }
    let oid = synthetic_database_oid(&name);
    if database_row_by_oid(oid).is_some() {
        return oid;
    }
    let Some(table) = static_catalog_by_relation_oid(PG_DATABASE_RELATION_OID) else {
        return oid;
    };
    let Some(row) = synthetic_database_row(table, oid, &name) else {
        return oid;
    };
    let mut draft = CatalogDraft::default();
    draft.upsert_catalog_row(table, row);
    commit_catalog_draft(draft);
    oid
}

pub fn catalog_row_value<'a>(
    table: &'a StaticCatalogTable,
    row: &'a CatalogRow,
    column_name: &str,
) -> Option<&'a CatalogValue> {
    table
        .columns
        .iter()
        .position(|column| column.name == column_name)
        .and_then(|index| row.values.get(index))
}

fn set_catalog_row_value(
    table: &StaticCatalogTable,
    row: &mut CatalogRow,
    column_name: &str,
    value: CatalogValue,
) {
    if let Some(index) = table
        .columns
        .iter()
        .position(|column| column.name == column_name)
        && let Some(slot) = row.values.get_mut(index)
    {
        *slot = value;
    }
}

fn catalog_value_from_text(column: &StaticCatalogColumn, value: Option<&str>) -> CatalogValue {
    let Some(raw) = value else {
        return CatalogValue::Null;
    };
    match column.type_oid {
        BOOL_OID => match raw {
            "t" | "true" => CatalogValue::Bool(true),
            "f" | "false" => CatalogValue::Bool(false),
            _ => CatalogValue::Raw(raw.to_owned()),
        },
        CHAR_OID => CatalogValue::Char(match raw {
            "\\0" => 0,
            _ => raw.as_bytes().first().copied().unwrap_or(0),
        }),
        INT2_OID => raw
            .parse::<i16>()
            .map(CatalogValue::Int16)
            .unwrap_or_else(|_| CatalogValue::Raw(raw.to_owned())),
        INT4_OID => raw
            .parse::<i32>()
            .map(CatalogValue::Int32)
            .unwrap_or_else(|_| CatalogValue::Raw(raw.to_owned())),
        FLOAT4_OID => raw
            .parse::<f32>()
            .map(CatalogValue::Float32)
            .unwrap_or_else(|_| CatalogValue::Raw(raw.to_owned())),
        NAME_OID => CatalogValue::Name(raw.to_owned()),
        TEXT_OID | PG_NODE_TREE_OID => CatalogValue::Text(raw.to_owned()),
        OIDVECTOR_OID => CatalogValue::OidVector(parse_oid_vector(raw)),
        INT2VECTOR_OID => CatalogValue::Int2Vector(parse_int2_vector(raw)),
        OID_OID | REGCLASS_OID => raw
            .parse::<u32>()
            .map(|oid| CatalogValue::Oid(Oid(oid)))
            .unwrap_or_else(|_| CatalogValue::Raw(raw.to_owned())),
        _ if column.type_name.starts_with("reg") || column.type_name == "oid" => raw
            .parse::<u32>()
            .map(|oid| CatalogValue::Oid(Oid(oid)))
            .unwrap_or_else(|_| CatalogValue::Raw(raw.to_owned())),
        _ => CatalogValue::Raw(raw.to_owned()),
    }
}

fn row_id_from_catalog_values(table: &StaticCatalogTable, values: &[CatalogValue]) -> Option<u64> {
    table
        .columns
        .iter()
        .position(|column| column.name == "oid")
        .and_then(|index| values.get(index))
        .and_then(catalog_value_oid)
        .map(|oid| oid.0 as u64)
}

fn next_overlay_row_id(state: &mut CatalogState, table: &StaticCatalogTable) -> u64 {
    let first_dynamic_row_id = table
        .rows
        .iter()
        .map(|row| row.row_id)
        .max()
        .unwrap_or(0)
        .saturating_add(1)
        .max(1);
    let next = state
        .next_overlay_row_ids
        .entry(table.oid.0)
        .or_insert(first_dynamic_row_id);
    let row_id = *next;
    *next = next.saturating_add(1).max(1);
    row_id
}

pub fn upsert_catalog_row(
    relation_oid: Oid,
    row_id: u64,
    values: Vec<Option<String>>,
) -> Result<u64, CatalogError> {
    let table = static_catalog_by_relation_oid(relation_oid).ok_or_else(|| {
        CatalogError::new(
            "42P01",
            format!(
                "generated catalog relation {} does not exist",
                relation_oid.0
            ),
        )
    })?;
    if values.len() != table.columns.len() {
        return Err(CatalogError::new(
            "42804",
            format!(
                "catalog row for {} has {} values but {} columns",
                table.name,
                values.len(),
                table.columns.len()
            ),
        ));
    }
    let values = table
        .columns
        .iter()
        .zip(values.iter())
        .map(|(column, value)| catalog_value_from_text(column, value.as_deref()))
        .collect::<Vec<_>>();
    let row_id = if row_id != 0 {
        row_id
    } else if let Some(row_id) = row_id_from_catalog_values(table, &values) {
        row_id
    } else {
        with_catalog(|state| next_overlay_row_id(state, table))
    };
    let row = CatalogRow {
        relation_oid,
        row_id,
        values,
    };
    with_catalog_session(|session| {
        ensure_catalog_transaction(session);
        session
            .transaction_stack
            .last_mut()
            .expect("catalog transaction was just ensured")
            .upsert_catalog_row(table, row);
    });
    Ok(row_id)
}

pub fn delete_catalog_row(relation_oid: Oid, row_id: u64) -> Result<(), CatalogError> {
    if static_catalog_by_relation_oid(relation_oid).is_none() {
        return Err(CatalogError::new(
            "42P01",
            format!(
                "generated catalog relation {} does not exist",
                relation_oid.0
            ),
        ));
    }
    with_catalog_session(|session| {
        ensure_catalog_transaction(session);
        session
            .transaction_stack
            .last_mut()
            .expect("catalog transaction was just ensured")
            .delete_catalog_row(relation_oid, row_id);
    });
    Ok(())
}

fn catalog_value_oid(value: &CatalogValue) -> Option<Oid> {
    match value {
        CatalogValue::Oid(oid) => Some(*oid),
        CatalogValue::Int32(value) => u32::try_from(*value).ok().map(Oid),
        CatalogValue::Int16(value) => u32::try_from(*value).ok().map(Oid),
        CatalogValue::Raw(value) => resolve_generated_catalog_oid_name(value),
        _ => None,
    }
}

pub fn resolve_generated_catalog_oid_name(value: &str) -> Option<Oid> {
    if value == "-" {
        return Some(INVALID_OID);
    }
    if let Ok(oid) = value.parse::<u32>() {
        return Some(Oid(oid));
    }
    let name = normalize_identifier(value);
    generated_catalog::STATIC_PROCS
        .iter()
        .find(|record| record.name == name)
        .map(|record| record.oid)
        .or_else(|| {
            generated_catalog::STATIC_TYPES
                .iter()
                .find(|record| record.name == name)
                .map(|record| record.oid)
        })
        .or_else(|| static_catalog_by_name(&name).map(|table| table.oid))
}

fn catalog_value_bool(value: &CatalogValue) -> Option<bool> {
    match value {
        CatalogValue::Bool(value) => Some(*value),
        CatalogValue::Raw(value) => match value.as_str() {
            "t" | "true" => Some(true),
            "f" | "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn catalog_value_i16(value: &CatalogValue) -> Option<i16> {
    match value {
        CatalogValue::Int16(value) => Some(*value),
        CatalogValue::Int32(value) => i16::try_from(*value).ok(),
        CatalogValue::Raw(value) => value.parse::<i16>().ok(),
        _ => None,
    }
}

fn catalog_value_i32(value: &CatalogValue) -> Option<i32> {
    match value {
        CatalogValue::Int16(value) => Some(i32::from(*value)),
        CatalogValue::Int32(value) => Some(*value),
        CatalogValue::Raw(value) => value.parse::<i32>().ok(),
        _ => None,
    }
}

fn catalog_value_f32(value: &CatalogValue) -> Option<f32> {
    match value {
        CatalogValue::Float32(value) => Some(*value),
        CatalogValue::Raw(value) => value.parse::<f32>().ok(),
        _ => None,
    }
}

fn catalog_value_u8(value: &CatalogValue) -> Option<u8> {
    match value {
        CatalogValue::Char(value) => Some(*value),
        CatalogValue::Raw(value) => value.as_bytes().first().copied(),
        _ => None,
    }
}

fn catalog_value_string(value: &CatalogValue) -> Option<String> {
    match value {
        CatalogValue::Name(value) | CatalogValue::Text(value) | CatalogValue::Raw(value) => {
            Some(value.clone())
        }
        _ => None,
    }
}

fn catalog_value_identifier_matches(value: &CatalogValue, normalized: &str) -> bool {
    let candidate = match value {
        CatalogValue::Name(value) | CatalogValue::Text(value) | CatalogValue::Raw(value) => value,
        _ => return false,
    };
    candidate == normalized || normalize_identifier(candidate) == normalized
}

pub fn btree_opclass_for_type(type_oid: Oid) -> Option<Oid> {
    static_btree_opclass_for_type(type_oid).or_else(|| {
        let record = lookup_type(type_oid)?;
        (record.typtype == b'e').then(|| static_btree_opclass_for_type(ANYENUM_OID))?
    })
}

fn static_btree_opclass_for_type(type_oid: Oid) -> Option<Oid> {
    let table = static_catalog_by_name("pg_opclass")?;
    table.rows.iter().find_map(|static_row| {
        let row = static_row_to_catalog_row(table, static_row);
        let opcmethod = catalog_row_value(table, &row, "opcmethod").and_then(catalog_value_oid)?;
        let opcintype = catalog_row_value(table, &row, "opcintype").and_then(catalog_value_oid)?;
        let opcdefault =
            catalog_row_value(table, &row, "opcdefault").and_then(catalog_value_bool)?;
        let oid = catalog_row_value(table, &row, "oid").and_then(catalog_value_oid)?;
        (opcmethod == BTREE_INDEX_AM_OID && opcintype == type_oid && opcdefault).then_some(oid)
    })
}

fn bump_generation(state: &mut CatalogState) {
    state.snapshot.generation = state.snapshot.generation.saturating_add(1).max(1);
    CATALOG_GENERATION.store(state.snapshot.generation, Ordering::Relaxed);
}

pub fn current_generation() -> u64 {
    CATALOG_GENERATION.load(Ordering::Relaxed)
}

fn primary_key_index_oid_cache() -> &'static Mutex<PrimaryKeyIndexOidCache> {
    PRIMARY_KEY_INDEX_OID_CACHE.get_or_init(|| Mutex::new(PrimaryKeyIndexOidCache::default()))
}

fn primary_key_index_oid_cache_lookup(relation_oid: Oid) -> Option<Option<Oid>> {
    if has_uncommitted_catalog_changes() {
        return None;
    }
    let generation = current_generation();
    let mut cache = primary_key_index_oid_cache()
        .lock()
        .expect("primary key index OID cache mutex poisoned");
    if cache.generation != generation {
        cache.generation = generation;
        cache.entries.clear();
    }
    cache.entries.get(&relation_oid.0).copied()
}

fn primary_key_index_oid_cache_store(relation_oid: Oid, index_oid: Option<Oid>) {
    if has_uncommitted_catalog_changes() {
        return;
    }
    let generation = current_generation();
    let mut cache = primary_key_index_oid_cache()
        .lock()
        .expect("primary key index OID cache mutex poisoned");
    if cache.generation != generation {
        cache.generation = generation;
        cache.entries.clear();
    }
    cache.entries.insert(relation_oid.0, index_oid);
}

fn catalog_lookup_cache() -> &'static Mutex<CatalogLookupCache> {
    CATALOG_LOOKUP_CACHE.get_or_init(|| Mutex::new(CatalogLookupCache::default()))
}

fn with_catalog_lookup_cache<R>(f: impl FnOnce(&mut CatalogLookupCache) -> R) -> R {
    let generation = current_generation();
    let mut cache = catalog_lookup_cache()
        .lock()
        .expect("catalog lookup cache mutex poisoned");
    if cache.generation != generation {
        cache.generation = generation;
        cache.relation_columns.clear();
        cache.index_records_by_oid.clear();
        cache.index_records_by_relation.clear();
        cache.unique_index_records_by_relation.clear();
    }
    f(&mut cache)
}

fn relation_column_cache_lookup(relation_oid: Oid, attnum: i16) -> Option<Option<ColumnRecord>> {
    if has_uncommitted_catalog_changes() {
        return None;
    }
    with_catalog_lookup_cache(|cache| {
        cache
            .relation_columns
            .get(&(relation_oid.0, attnum))
            .cloned()
    })
}

fn relation_column_cache_store(relation_oid: Oid, attnum: i16, column: Option<ColumnRecord>) {
    if has_uncommitted_catalog_changes() {
        return;
    }
    with_catalog_lookup_cache(|cache| {
        cache
            .relation_columns
            .insert((relation_oid.0, attnum), column);
    });
}

fn index_record_by_oid_cache_lookup(index_oid: Oid) -> Option<Option<IndexRecord>> {
    if has_uncommitted_catalog_changes() {
        return None;
    }
    with_catalog_lookup_cache(|cache| cache.index_records_by_oid.get(&index_oid.0).cloned())
}

fn index_record_by_oid_cache_store(index_oid: Oid, record: Option<IndexRecord>) {
    if has_uncommitted_catalog_changes() {
        return;
    }
    with_catalog_lookup_cache(|cache| {
        cache.index_records_by_oid.insert(index_oid.0, record);
    });
}

fn index_records_by_relation_cache_lookup(relation_oid: Oid) -> Option<Vec<IndexRecord>> {
    if has_uncommitted_catalog_changes() {
        return None;
    }
    with_catalog_lookup_cache(|cache| {
        cache
            .index_records_by_relation
            .get(&relation_oid.0)
            .cloned()
    })
}

fn index_records_by_relation_cache_store(relation_oid: Oid, records: Vec<IndexRecord>) {
    if has_uncommitted_catalog_changes() {
        return;
    }
    with_catalog_lookup_cache(|cache| {
        cache
            .index_records_by_relation
            .insert(relation_oid.0, records);
    });
}

fn unique_index_records_by_relation_cache_lookup(relation_oid: Oid) -> Option<Vec<IndexRecord>> {
    if has_uncommitted_catalog_changes() {
        return None;
    }
    with_catalog_lookup_cache(|cache| {
        cache
            .unique_index_records_by_relation
            .get(&relation_oid.0)
            .cloned()
    })
}

fn unique_index_records_by_relation_cache_store(relation_oid: Oid, records: Vec<IndexRecord>) {
    if has_uncommitted_catalog_changes() {
        return;
    }
    with_catalog_lookup_cache(|cache| {
        cache
            .unique_index_records_by_relation
            .insert(relation_oid.0, records);
    });
}

#[cfg(test)]
fn clear_catalog_lookup_caches() {
    if let Some(cache) = PRIMARY_KEY_INDEX_OID_CACHE.get() {
        let mut cache = cache
            .lock()
            .expect("primary key index OID cache mutex poisoned");
        cache.generation = current_generation();
        cache.entries.clear();
    }
    if let Some(cache) = CATALOG_LOOKUP_CACHE.get() {
        let mut cache = cache.lock().expect("catalog lookup cache mutex poisoned");
        cache.generation = current_generation();
        cache.relation_columns.clear();
        cache.index_records_by_oid.clear();
        cache.index_records_by_relation.clear();
        cache.unique_index_records_by_relation.clear();
    }
}

fn relation_pg_class_row_by_oid(oid: Oid) -> Option<CatalogRow> {
    let table = static_catalog_by_relation_oid(PG_CLASS_RELATION_OID)?;
    catalog_rows_matching_static(
        table.oid,
        |table, row| static_row_column_oid(table, row, "oid").is_some_and(|row_oid| row_oid == oid),
        |row| {
            catalog_row_value(table, row, "oid")
                .and_then(catalog_value_oid)
                .is_some_and(|row_oid| row_oid == oid)
        },
    )
    .into_iter()
    .next()
}

fn pg_class_row_oid_by_name_in_namespace(
    table: &StaticCatalogTable,
    row: &CatalogRow,
    name: &str,
    namespace: Oid,
) -> Option<Oid> {
    let row_name_matches = catalog_row_identifier_matches(table, row, "relname", name);
    let row_namespace_matches = catalog_row_value(table, row, "relnamespace")
        .and_then(catalog_value_oid)
        .is_some_and(|row_namespace| row_namespace == namespace);
    if !row_name_matches || !row_namespace_matches {
        return None;
    }
    catalog_row_value(table, row, "oid").and_then(catalog_value_oid)
}

fn catalog_value_int2_vector(value: &CatalogValue) -> Option<Vec<i16>> {
    match value {
        CatalogValue::Int2Vector(values) => Some(values.clone()),
        CatalogValue::Raw(value) | CatalogValue::Text(value) | CatalogValue::Name(value) => {
            Some(parse_int2_vector(value))
        }
        _ => None,
    }
}

fn relation_columns_from_pg_attribute(relation_oid: Oid) -> Vec<ColumnRecord> {
    let Some(table) = static_catalog_by_relation_oid(PG_ATTRIBUTE_RELATION_OID) else {
        return Vec::new();
    };
    let mut columns = catalog_rows_matching_static(
        table.oid,
        |table, row| {
            static_row_column_oid(table, row, "attrelid")
                .is_some_and(|attrelid| attrelid == relation_oid)
        },
        |row| {
            catalog_row_value(table, row, "attrelid")
                .and_then(catalog_value_oid)
                .is_some_and(|attrelid| attrelid == relation_oid)
        },
    )
    .into_iter()
    .filter_map(|row| column_record_from_pg_attribute_row(table, &row, relation_oid))
    .collect::<Vec<_>>();
    columns.sort_by_key(|(attnum, _)| *attnum);
    columns.into_iter().map(|(_, column)| column).collect()
}

fn column_record_from_pg_attribute_row(
    table: &StaticCatalogTable,
    row: &CatalogRow,
    relation_oid: Oid,
) -> Option<(i16, ColumnRecord)> {
    let attrelid = catalog_row_value(table, row, "attrelid").and_then(catalog_value_oid)?;
    if attrelid != relation_oid {
        return None;
    }
    let attnum = catalog_row_value(table, row, "attnum").and_then(catalog_value_i16)?;
    if attnum <= 0 {
        return None;
    }
    let attisdropped = catalog_row_value(table, row, "attisdropped")
        .and_then(catalog_value_bool)
        .unwrap_or(false);
    if attisdropped {
        return None;
    }
    let name = catalog_row_value(table, row, "attname").and_then(catalog_value_string)?;
    let type_oid = catalog_row_value(table, row, "atttypid").and_then(catalog_value_oid)?;
    let type_mod = catalog_row_value(table, row, "atttypmod")
        .and_then(catalog_value_i32)
        .unwrap_or(-1);
    let is_not_null = catalog_row_value(table, row, "attnotnull")
        .and_then(catalog_value_bool)
        .unwrap_or(false);
    let has_default = catalog_row_value(table, row, "atthasdef")
        .and_then(catalog_value_bool)
        .unwrap_or(false);
    let generated = catalog_row_value(table, row, "attgenerated")
        .and_then(catalog_value_u8)
        .unwrap_or(0);
    Some((
        attnum,
        ColumnRecord {
            name: normalize_identifier(&name),
            type_oid,
            type_mod,
            is_not_null,
            has_default,
            generated,
        },
    ))
}

fn physical_column_record_from_pg_attribute_row(
    table: &StaticCatalogTable,
    row: &CatalogRow,
    relation_oid: Oid,
) -> Option<(i16, PhysicalColumnRecord)> {
    let attrelid = catalog_row_value(table, row, "attrelid").and_then(catalog_value_oid)?;
    if attrelid != relation_oid {
        return None;
    }
    let attnum = catalog_row_value(table, row, "attnum").and_then(catalog_value_i16)?;
    if attnum <= 0 {
        return None;
    }
    let is_dropped = catalog_row_value(table, row, "attisdropped")
        .and_then(catalog_value_bool)
        .unwrap_or(false);
    let name = catalog_row_value(table, row, "attname").and_then(catalog_value_string)?;
    let type_oid = catalog_row_value(table, row, "atttypid")
        .and_then(catalog_value_oid)
        .unwrap_or(INVALID_OID);
    let type_mod = catalog_row_value(table, row, "atttypmod")
        .and_then(catalog_value_i32)
        .unwrap_or(-1);
    let is_not_null = !is_dropped
        && catalog_row_value(table, row, "attnotnull")
            .and_then(catalog_value_bool)
            .unwrap_or(false);
    let has_default = !is_dropped
        && catalog_row_value(table, row, "atthasdef")
            .and_then(catalog_value_bool)
            .unwrap_or(false);
    let generated = if is_dropped {
        0
    } else {
        catalog_row_value(table, row, "attgenerated")
            .and_then(catalog_value_u8)
            .unwrap_or(0)
    };
    let attlen = catalog_row_value(table, row, "attlen")
        .and_then(catalog_value_i16)
        .unwrap_or(0);
    let attbyval = catalog_row_value(table, row, "attbyval")
        .and_then(catalog_value_bool)
        .unwrap_or(false);
    let attalign = catalog_row_value(table, row, "attalign")
        .and_then(catalog_value_u8)
        .unwrap_or(b'i');
    let attstorage = catalog_row_value(table, row, "attstorage")
        .and_then(catalog_value_u8)
        .unwrap_or(b'p');
    let attcollation = catalog_row_value(table, row, "attcollation")
        .and_then(catalog_value_oid)
        .unwrap_or(INVALID_OID);

    Some((
        attnum,
        PhysicalColumnRecord {
            name: normalize_identifier(&name),
            type_oid,
            type_mod,
            is_not_null,
            has_default,
            generated,
            is_dropped,
            attlen,
            attbyval,
            attalign,
            attstorage,
            attcollation,
        },
    ))
}

pub fn relation_column_by_attnum(relation_oid: Oid, attnum: i16) -> Option<ColumnRecord> {
    if let Some(cached) = relation_column_cache_lookup(relation_oid, attnum) {
        return cached;
    }
    let result = relation_column_by_attnum_uncached(relation_oid, attnum);
    relation_column_cache_store(relation_oid, attnum, result.clone());
    result
}

fn relation_column_by_attnum_uncached(relation_oid: Oid, attnum: i16) -> Option<ColumnRecord> {
    if attnum <= 0 {
        return None;
    }
    if let Some(column) = with_visible_catalog_snapshot(|snapshot| {
        relation_column_from_snapshot(snapshot, relation_oid, attnum)
    }) {
        return Some(column);
    }
    let table = static_catalog_by_relation_oid(PG_ATTRIBUTE_RELATION_OID)?;
    catalog_rows_matching_static(
        table.oid,
        |table, row| {
            static_row_column_oid(table, row, "attrelid")
                .is_some_and(|attrelid| attrelid == relation_oid)
                && static_row_column_i16(table, row, "attnum")
                    .is_some_and(|row_attnum| row_attnum == attnum)
        },
        |row| {
            let relation_matches = catalog_row_value(table, row, "attrelid")
                .and_then(catalog_value_oid)
                .is_some_and(|attrelid| attrelid == relation_oid);
            let attnum_matches = catalog_row_value(table, row, "attnum")
                .and_then(catalog_value_i16)
                .is_some_and(|row_attnum| row_attnum == attnum);
            relation_matches && attnum_matches
        },
    )
    .into_iter()
    .find_map(|row| {
        let (row_attnum, column) = column_record_from_pg_attribute_row(table, &row, relation_oid)?;
        (row_attnum == attnum).then_some(column)
    })
}

pub fn relation_physical_column_by_attnum(
    relation_oid: Oid,
    attnum: i16,
) -> Option<PhysicalColumnRecord> {
    if attnum <= 0 {
        return None;
    }
    let table = static_catalog_by_relation_oid(PG_ATTRIBUTE_RELATION_OID)?;
    catalog_rows_matching_static(
        table.oid,
        |table, row| {
            static_row_column_oid(table, row, "attrelid")
                .is_some_and(|attrelid| attrelid == relation_oid)
                && static_row_column_i16(table, row, "attnum")
                    .is_some_and(|row_attnum| row_attnum == attnum)
        },
        |row| {
            let relation_matches = catalog_row_value(table, row, "attrelid")
                .and_then(catalog_value_oid)
                .is_some_and(|attrelid| attrelid == relation_oid);
            let attnum_matches = catalog_row_value(table, row, "attnum")
                .and_then(catalog_value_i16)
                .is_some_and(|row_attnum| row_attnum == attnum);
            relation_matches && attnum_matches
        },
    )
    .into_iter()
    .find_map(|row| {
        let (row_attnum, column) =
            physical_column_record_from_pg_attribute_row(table, &row, relation_oid)?;
        (row_attnum == attnum).then_some(column)
    })
}

fn pg_index_record_from_row(row: &CatalogRow) -> Option<IndexRecord> {
    let table = static_catalog_by_relation_oid(PG_INDEX_RELATION_OID)?;
    let index_oid = catalog_row_value(table, row, "indexrelid").and_then(catalog_value_oid)?;
    let relation_oid = catalog_row_value(table, row, "indrelid").and_then(catalog_value_oid)?;
    let indnkeyatts = catalog_row_value(table, row, "indnkeyatts")
        .and_then(catalog_value_i16)
        .unwrap_or(0)
        .max(0) as usize;
    let mut key_attnums = catalog_row_value(table, row, "indkey")
        .and_then(catalog_value_int2_vector)
        .unwrap_or_default();
    if indnkeyatts > 0 && key_attnums.len() > indnkeyatts {
        key_attnums.truncate(indnkeyatts);
    }

    Some(IndexRecord {
        index_oid,
        relation_oid,
        key_attnums,
        is_unique: catalog_row_value(table, row, "indisunique")
            .and_then(catalog_value_bool)
            .unwrap_or(false),
        nulls_not_distinct: catalog_row_value(table, row, "indnullsnotdistinct")
            .and_then(catalog_value_bool)
            .unwrap_or(false),
        is_primary: catalog_row_value(table, row, "indisprimary")
            .and_then(catalog_value_bool)
            .unwrap_or(false),
        is_immediate: catalog_row_value(table, row, "indimmediate")
            .and_then(catalog_value_bool)
            .unwrap_or(true),
        is_valid: catalog_row_value(table, row, "indisvalid")
            .and_then(catalog_value_bool)
            .unwrap_or(true),
        is_ready: catalog_row_value(table, row, "indisready")
            .and_then(catalog_value_bool)
            .unwrap_or(true),
        is_live: catalog_row_value(table, row, "indislive")
            .and_then(catalog_value_bool)
            .unwrap_or(true),
    })
}

pub fn index_record_by_index_oid(index_oid: Oid) -> Option<IndexRecord> {
    if let Some(index) = with_visible_catalog_snapshot(|snapshot| {
        snapshot
            .index_meta_by_oid(index_oid)
            .map(|index| index.record.clone())
    }) {
        return Some(index);
    }
    if let Some(cached) = index_record_by_oid_cache_lookup(index_oid) {
        return cached;
    }
    let result = index_record_by_index_oid_uncached(index_oid);
    index_record_by_oid_cache_store(index_oid, result.clone());
    result
}

fn index_record_by_index_oid_uncached(index_oid: Oid) -> Option<IndexRecord> {
    let table = static_catalog_by_relation_oid(PG_INDEX_RELATION_OID)?;
    catalog_rows_matching_static(
        table.oid,
        |table, row| {
            static_row_column_oid(table, row, "indexrelid")
                .is_some_and(|row_index_oid| row_index_oid == index_oid)
        },
        |row| {
            catalog_row_value(table, row, "indexrelid")
                .and_then(catalog_value_oid)
                .is_some_and(|row_index_oid| row_index_oid == index_oid)
        },
    )
    .into_iter()
    .find_map(|row| {
        let record = pg_index_record_from_row(&row)?;
        (record.index_oid == index_oid).then_some(record)
    })
}

pub fn index_records_for_relation_oid(relation_oid: Oid) -> Vec<IndexRecord> {
    let typed_indexes = with_visible_catalog_snapshot(|snapshot| {
        snapshot
            .index_metas_for_relation(relation_oid)
            .into_iter()
            .map(|index| index.record.clone())
            .collect::<Vec<_>>()
    });
    if !typed_indexes.is_empty() {
        return typed_indexes;
    }
    if let Some(cached) = index_records_by_relation_cache_lookup(relation_oid) {
        return cached;
    }
    let result = index_records_for_relation_oid_uncached(relation_oid);
    index_records_by_relation_cache_store(relation_oid, result.clone());
    result
}

fn index_records_for_relation_oid_uncached(relation_oid: Oid) -> Vec<IndexRecord> {
    let Some(table) = static_catalog_by_relation_oid(PG_INDEX_RELATION_OID) else {
        return Vec::new();
    };
    catalog_rows_matching_static(
        table.oid,
        |table, row| {
            static_row_column_oid(table, row, "indrelid")
                .is_some_and(|indrelid| indrelid == relation_oid)
        },
        |row| {
            catalog_row_value(table, row, "indrelid")
                .and_then(catalog_value_oid)
                .is_some_and(|indrelid| indrelid == relation_oid)
        },
    )
    .into_iter()
    .filter_map(|row| {
        let record = pg_index_record_from_row(&row)?;
        (record.relation_oid == relation_oid).then_some(record)
    })
    .collect()
}

pub fn unique_index_records_for_relation_oid(relation_oid: Oid) -> Vec<IndexRecord> {
    if let Some(cached) = unique_index_records_by_relation_cache_lookup(relation_oid) {
        return cached;
    }
    let result: Vec<_> = index_records_for_relation_oid(relation_oid)
        .into_iter()
        .filter(|record| record.is_unique && record.is_valid && record.is_ready && record.is_live)
        .collect();
    unique_index_records_by_relation_cache_store(relation_oid, result.clone());
    result
}

pub fn unique_index_oids_for_relation_oid(relation_oid: Oid) -> Vec<Oid> {
    let typed_oids = with_visible_catalog_snapshot(|snapshot| {
        snapshot
            .index_metas_for_relation(relation_oid)
            .into_iter()
            .filter(|index| {
                index.record.is_unique
                    && index.record.is_valid
                    && index.record.is_ready
                    && index.record.is_live
            })
            .map(|index| index.record.index_oid)
            .collect::<Vec<_>>()
    });
    if !typed_oids.is_empty() {
        return typed_oids;
    }
    let Some(table) = static_catalog_by_relation_oid(PG_INDEX_RELATION_OID) else {
        return Vec::new();
    };
    catalog_rows_matching_static(
        table.oid,
        |table, row| {
            static_row_column_oid(table, row, "indrelid")
                .is_some_and(|indrelid| indrelid == relation_oid)
                && static_row_column_bool(table, row, "indisunique").unwrap_or(false)
                && static_row_column_bool(table, row, "indisvalid").unwrap_or(true)
                && static_row_column_bool(table, row, "indisready").unwrap_or(true)
                && static_row_column_bool(table, row, "indislive").unwrap_or(true)
        },
        |row| {
            let relation_matches = catalog_row_value(table, row, "indrelid")
                .and_then(catalog_value_oid)
                .is_some_and(|indrelid| indrelid == relation_oid);
            let is_unique = catalog_row_value(table, row, "indisunique")
                .and_then(catalog_value_bool)
                .unwrap_or(false);
            let is_valid = catalog_row_value(table, row, "indisvalid")
                .and_then(catalog_value_bool)
                .unwrap_or(true);
            let is_ready = catalog_row_value(table, row, "indisready")
                .and_then(catalog_value_bool)
                .unwrap_or(true);
            let is_live = catalog_row_value(table, row, "indislive")
                .and_then(catalog_value_bool)
                .unwrap_or(true);
            relation_matches && is_unique && is_valid && is_ready && is_live
        },
    )
    .into_iter()
    .filter_map(|row| catalog_row_value(table, &row, "indexrelid").and_then(catalog_value_oid))
    .collect()
}

pub fn relation_oid_for_index_oid(index_oid: Oid) -> Option<Oid> {
    index_record_by_index_oid(index_oid).map(|record| record.relation_oid)
}

fn primary_key_pg_index_row(relation_oid: Oid) -> Option<CatalogRow> {
    let table = static_catalog_by_relation_oid(PG_INDEX_RELATION_OID)?;
    catalog_rows_matching_static(
        table.oid,
        |table, row| {
            static_row_column_oid(table, row, "indrelid")
                .is_some_and(|indrelid| indrelid == relation_oid)
                && static_row_column_bool(table, row, "indisprimary").unwrap_or(false)
        },
        |row| {
            pg_index_record_from_row(row)
                .is_some_and(|record| record.relation_oid == relation_oid && record.is_primary)
        },
    )
    .into_iter()
    .next()
}

pub fn primary_key_index_oid_for_relation_oid(relation_oid: Oid) -> Option<Oid> {
    if let Some(cached) = primary_key_index_oid_cache_lookup(relation_oid) {
        return cached;
    }
    let result = primary_key_index_oid_for_relation_oid_uncached(relation_oid);
    primary_key_index_oid_cache_store(relation_oid, result);
    result
}

fn primary_key_index_oid_for_relation_oid_uncached(relation_oid: Oid) -> Option<Oid> {
    if let Some(index) = with_visible_catalog_snapshot(|snapshot| {
        primary_key_index_record_from_snapshot(snapshot, relation_oid)
    }) {
        return Some(index.index_oid);
    }
    primary_key_pg_index_row(relation_oid)
        .and_then(|row| pg_index_record_from_row(&row))
        .map(|record| record.index_oid)
}

pub fn primary_key_index_record_for_relation_oid(relation_oid: Oid) -> Option<IndexRecord> {
    if let Some(index) = with_visible_catalog_snapshot(|snapshot| {
        primary_key_index_record_from_snapshot(snapshot, relation_oid)
    }) {
        return Some(index);
    }
    let row = primary_key_pg_index_row(relation_oid)?;
    let record = pg_index_record_from_row(&row)?;
    (record.relation_oid == relation_oid && record.is_primary).then_some(record)
}

pub fn primary_key_relation_oid_for_index_oid(index_oid: Oid) -> Option<Oid> {
    let record = index_record_by_index_oid(index_oid)?;
    record.is_primary.then_some(record.relation_oid)
}

fn primary_key_constraint_oid_for_relation(
    relation_oid: Oid,
    index_oid: Option<Oid>,
) -> Option<Oid> {
    if let Some(oid) = with_visible_catalog_snapshot(|snapshot| {
        primary_key_constraint_oid_from_snapshot(snapshot, relation_oid, index_oid)
    }) {
        return Some(oid);
    }
    let table = static_catalog_by_relation_oid(PG_CONSTRAINT_RELATION_OID)?;
    catalog_rows_matching_static(
        table.oid,
        |table, row| {
            static_row_column_oid(table, row, "conrelid")
                .is_some_and(|conrelid| conrelid == relation_oid)
        },
        |row| {
            catalog_row_value(table, row, "conrelid")
                .and_then(catalog_value_oid)
                .is_some_and(|conrelid| conrelid == relation_oid)
        },
    )
    .into_iter()
    .find_map(|row| {
        let conrelid = catalog_row_value(table, &row, "conrelid").and_then(catalog_value_oid)?;
        if conrelid != relation_oid {
            return None;
        }
        let contype = catalog_row_value(table, &row, "contype").and_then(catalog_value_u8)?;
        if contype != b'p' {
            return None;
        }
        if let Some(index_oid) = index_oid {
            let conindid = catalog_row_value(table, &row, "conindid").and_then(catalog_value_oid);
            if conindid.is_some_and(|conindid| conindid != index_oid) {
                return None;
            }
        }
        catalog_row_value(table, &row, "oid").and_then(catalog_value_oid)
    })
}

fn relation_primary_key_from_pg_index(
    relation_oid: Oid,
    _columns: &[ColumnRecord],
) -> (Vec<String>, Option<Oid>) {
    let Some(table) = static_catalog_by_relation_oid(PG_INDEX_RELATION_OID) else {
        return (Vec::new(), None);
    };
    let Some(row) = primary_key_pg_index_row(relation_oid) else {
        return (
            Vec::new(),
            primary_key_constraint_oid_for_relation(relation_oid, None),
        );
    };
    let index_oid = catalog_row_value(table, &row, "indexrelid").and_then(catalog_value_oid);
    let indkey = catalog_row_value(table, &row, "indkey")
        .and_then(catalog_value_int2_vector)
        .unwrap_or_default();
    let primary_key = indkey
        .into_iter()
        .filter_map(|attnum| {
            relation_column_by_attnum(relation_oid, attnum).map(|column| column.name)
        })
        .collect::<Vec<_>>();
    let constraint_oid = primary_key_constraint_oid_for_relation(relation_oid, index_oid);
    (primary_key, constraint_oid)
}

fn relation_columns_from_snapshot(
    snapshot: &CatalogSnapshot,
    relation_oid: Oid,
) -> Vec<ColumnRecord> {
    snapshot
        .columns_by_relation
        .get(&relation_oid.0)
        .into_iter()
        .flat_map(|columns| columns.values())
        .filter_map(|row_id| snapshot.columns.get(row_id))
        .map(|column| column.record.clone())
        .collect()
}

fn relation_column_from_snapshot(
    snapshot: &CatalogSnapshot,
    relation_oid: Oid,
    attnum: i16,
) -> Option<ColumnRecord> {
    snapshot
        .columns_by_relation
        .get(&relation_oid.0)?
        .get(&attnum)
        .and_then(|row_id| snapshot.columns.get(row_id))
        .map(|column| column.record.clone())
}

fn primary_key_index_record_from_snapshot(
    snapshot: &CatalogSnapshot,
    relation_oid: Oid,
) -> Option<IndexRecord> {
    snapshot
        .index_metas_for_relation(relation_oid)
        .into_iter()
        .find(|index| index.record.is_primary)
        .map(|index| index.record.clone())
}

fn primary_key_constraint_oid_from_snapshot(
    snapshot: &CatalogSnapshot,
    relation_oid: Oid,
    index_oid: Option<Oid>,
) -> Option<Oid> {
    snapshot
        .constraint_metas_for_relation(relation_oid)
        .into_iter()
        .find_map(|constraint| {
            if constraint.kind != b'p' {
                return None;
            }
            if let Some(index_oid) = index_oid
                && constraint.index_oid != INVALID_OID
                && constraint.index_oid != index_oid
            {
                return None;
            }
            Some(constraint.oid)
        })
}

fn relation_primary_key_from_snapshot(
    snapshot: &CatalogSnapshot,
    relation_oid: Oid,
    columns: &[ColumnRecord],
) -> (Vec<String>, Option<Oid>) {
    let Some(index) = primary_key_index_record_from_snapshot(snapshot, relation_oid) else {
        return (
            Vec::new(),
            primary_key_constraint_oid_from_snapshot(snapshot, relation_oid, None),
        );
    };
    let primary_key = index
        .key_attnums
        .iter()
        .filter_map(|attnum| {
            if *attnum <= 0 {
                return None;
            }
            columns
                .get(usize::try_from(*attnum - 1).ok()?)
                .map(|column| column.name.clone())
        })
        .collect::<Vec<_>>();
    let constraint_oid =
        primary_key_constraint_oid_from_snapshot(snapshot, relation_oid, Some(index.index_oid));
    (primary_key, constraint_oid)
}

fn relation_record_from_meta(
    snapshot: &CatalogSnapshot,
    relation: &RelationMeta,
) -> RelationRecord {
    let columns = relation_columns_from_snapshot(snapshot, relation.oid);
    let (primary_key, primary_key_constraint_oid) =
        relation_primary_key_from_snapshot(snapshot, relation.oid, &columns);
    RelationRecord {
        row_id: relation.row_id,
        oid: relation.oid,
        type_oid: relation.type_oid,
        namespace: relation.namespace,
        owner: relation.owner,
        name: relation.name.clone(),
        relkind: relation.relkind,
        columns,
        primary_key,
        primary_key_constraint_oid,
    }
}

fn relation_summary_from_meta(
    snapshot: &CatalogSnapshot,
    relation: &RelationMeta,
) -> RelationSummaryRecord {
    let typed_columns = relation_columns_from_snapshot(snapshot, relation.oid);
    let primary_key = primary_key_index_record_from_snapshot(snapshot, relation.oid);
    let has_primary_key = primary_key.is_some();
    let has_indexes = relation.relhasindex
        || has_primary_key
        || !snapshot.index_metas_for_relation(relation.oid).is_empty();
    RelationSummaryRecord {
        row_id: relation.row_id,
        oid: relation.oid,
        type_oid: relation.type_oid,
        namespace: relation.namespace,
        owner: relation.owner,
        name: relation.name.clone(),
        relkind: relation.relkind,
        column_count: relation
            .relnatts
            .max(typed_columns.len().min(u16::MAX as usize) as u16),
        has_primary_key,
        has_indexes,
    }
}

fn relation_record_from_pg_class_row(row: &CatalogRow) -> Option<RelationRecord> {
    let table = static_catalog_by_relation_oid(PG_CLASS_RELATION_OID)?;
    let oid = catalog_row_value(table, row, "oid").and_then(catalog_value_oid)?;
    let type_oid = catalog_row_value(table, row, "reltype")
        .and_then(catalog_value_oid)
        .unwrap_or(INVALID_OID);
    let namespace = catalog_row_value(table, row, "relnamespace")
        .and_then(catalog_value_oid)
        .unwrap_or(PUBLIC_NAMESPACE_OID);
    let owner = catalog_row_value(table, row, "relowner")
        .and_then(catalog_value_oid)
        .unwrap_or(POSTGRES_ROLE_OID);
    let name = catalog_row_value(table, row, "relname").and_then(catalog_value_string)?;
    let relkind = catalog_row_value(table, row, "relkind")
        .and_then(catalog_value_u8)
        .unwrap_or(b'r');
    let columns = relation_columns_from_pg_attribute(oid);
    let (primary_key, primary_key_constraint_oid) =
        relation_primary_key_from_pg_index(oid, &columns);
    Some(RelationRecord {
        row_id: row.row_id,
        oid,
        type_oid,
        namespace,
        owner,
        name: normalize_identifier(&name),
        relkind,
        columns,
        primary_key,
        primary_key_constraint_oid,
    })
}

fn relation_summary_from_pg_class_row(row: &CatalogRow) -> Option<RelationSummaryRecord> {
    let table = static_catalog_by_relation_oid(PG_CLASS_RELATION_OID)?;
    let oid = catalog_row_value(table, row, "oid").and_then(catalog_value_oid)?;
    let type_oid = catalog_row_value(table, row, "reltype")
        .and_then(catalog_value_oid)
        .unwrap_or(INVALID_OID);
    let namespace = catalog_row_value(table, row, "relnamespace")
        .and_then(catalog_value_oid)
        .unwrap_or(PUBLIC_NAMESPACE_OID);
    let owner = catalog_row_value(table, row, "relowner")
        .and_then(catalog_value_oid)
        .unwrap_or(POSTGRES_ROLE_OID);
    let name = catalog_row_value(table, row, "relname").and_then(catalog_value_string)?;
    let relkind = catalog_row_value(table, row, "relkind")
        .and_then(catalog_value_u8)
        .unwrap_or(b'r');
    let column_count = catalog_row_value(table, row, "relnatts")
        .and_then(catalog_value_i16)
        .map(|value| value.max(0) as usize)
        .unwrap_or_else(|| relation_columns_from_pg_attribute(oid).len())
        .min(u16::MAX as usize) as u16;
    let has_primary_key = primary_key_index_oid_for_relation_oid(oid).is_some();
    let has_indexes = catalog_row_value(table, row, "relhasindex")
        .and_then(catalog_value_bool)
        .unwrap_or_else(|| !index_records_for_relation_oid(oid).is_empty())
        || has_primary_key;

    Some(RelationSummaryRecord {
        row_id: row.row_id,
        oid,
        type_oid,
        namespace,
        owner,
        name: normalize_identifier(&name),
        relkind,
        column_count,
        has_primary_key,
        has_indexes,
    })
}

pub fn relation_by_name(name: &str) -> Option<RelationRecord> {
    let name = normalize_identifier(name);
    if let Some(relation) = with_visible_catalog_snapshot(|snapshot| {
        let mut matches = snapshot
            .relations
            .values()
            .filter(|relation| relation.name == name)
            .map(|relation| relation_record_from_meta(snapshot, relation))
            .collect::<Vec<_>>();
        matches.sort_by_key(|relation| match relation.namespace {
            PUBLIC_NAMESPACE_OID => 0,
            PG_CATALOG_NAMESPACE_OID => 1,
            _ => 2,
        });
        matches.into_iter().next()
    }) {
        return Some(relation);
    }
    let table = static_catalog_by_relation_oid(PG_CLASS_RELATION_OID)?;
    let mut matches = catalog_rows_matching_static(
        table.oid,
        |table, row| static_row_column_identifier_matches(table, row, "relname", &name),
        |row| catalog_row_identifier_matches(table, row, "relname", &name),
    )
    .into_iter()
    .filter_map(|row| {
        if !catalog_row_identifier_matches(table, &row, "relname", &name) {
            return None;
        }
        relation_record_from_pg_class_row(&row)
    })
    .collect::<Vec<_>>();
    matches.sort_by_key(|relation| match relation.namespace {
        PUBLIC_NAMESPACE_OID => 0,
        PG_CATALOG_NAMESPACE_OID => 1,
        _ => 2,
    });
    matches.into_iter().next()
}

pub fn relation_by_name_in_namespace(name: &str, namespace: Oid) -> Option<RelationRecord> {
    let name = normalize_identifier(name);
    if let Some(relation) = with_visible_catalog_snapshot(|snapshot| {
        snapshot
            .relation_meta_by_name(&name, namespace)
            .map(|relation| relation_record_from_meta(snapshot, relation))
    }) {
        return Some(relation);
    }
    let table = static_catalog_by_relation_oid(PG_CLASS_RELATION_OID)?;
    catalog_rows_matching_static(
        table.oid,
        |table, row| {
            static_row_column_identifier_matches(table, row, "relname", &name)
                && static_row_column_oid(table, row, "relnamespace")
                    .is_some_and(|row_namespace| row_namespace == namespace)
        },
        |row| {
            let row_name = catalog_row_identifier_matches(table, row, "relname", &name);
            let row_namespace = catalog_row_value(table, row, "relnamespace")
                .and_then(catalog_value_oid)
                .is_some_and(|row_namespace| row_namespace == namespace);
            row_name && row_namespace
        },
    )
    .into_iter()
    .find_map(|row| {
        if !catalog_row_identifier_matches(table, &row, "relname", &name) {
            return None;
        }
        let row_namespace =
            catalog_row_value(table, &row, "relnamespace").and_then(catalog_value_oid)?;
        if row_namespace != namespace {
            return None;
        }
        relation_record_from_pg_class_row(&row)
    })
}

pub fn relation_summary_by_name_in_namespace(
    name: &str,
    namespace: Oid,
) -> Option<RelationSummaryRecord> {
    let name = normalize_identifier(name);
    if let Some(relation) = with_visible_catalog_snapshot(|snapshot| {
        snapshot
            .relation_meta_by_name(&name, namespace)
            .map(|relation| relation_summary_from_meta(snapshot, relation))
    }) {
        return Some(relation);
    }
    let table = static_catalog_by_relation_oid(PG_CLASS_RELATION_OID)?;
    catalog_rows_matching_static(
        table.oid,
        |table, row| {
            static_row_column_identifier_matches(table, row, "relname", &name)
                && static_row_column_oid(table, row, "relnamespace")
                    .is_some_and(|row_namespace| row_namespace == namespace)
        },
        |row| {
            let row_name = catalog_row_identifier_matches(table, row, "relname", &name);
            let row_namespace = catalog_row_value(table, row, "relnamespace")
                .and_then(catalog_value_oid)
                .is_some_and(|row_namespace| row_namespace == namespace);
            row_name && row_namespace
        },
    )
    .into_iter()
    .find_map(|row| {
        if !catalog_row_identifier_matches(table, &row, "relname", &name) {
            return None;
        }
        let row_namespace =
            catalog_row_value(table, &row, "relnamespace").and_then(catalog_value_oid)?;
        if row_namespace != namespace {
            return None;
        }
        relation_summary_from_pg_class_row(&row)
    })
}

pub fn relation_oid_by_name_in_namespace(name: &str, namespace: Oid) -> Option<Oid> {
    let name = normalize_identifier(name);
    if let Some(relation_oid) = with_visible_catalog_snapshot(|snapshot| {
        snapshot
            .relation_meta_by_name(&name, namespace)
            .map(|relation| relation.oid)
    }) {
        return Some(relation_oid);
    }
    let table = static_catalog_by_relation_oid(PG_CLASS_RELATION_OID)?;
    catalog_rows(table.oid)
        .into_iter()
        .find_map(|row| pg_class_row_oid_by_name_in_namespace(table, &row, &name, namespace))
}

pub fn relations() -> Vec<RelationRecord> {
    let mut dynamic_relations = with_visible_catalog_snapshot(|snapshot| {
        snapshot
            .relations
            .values()
            .map(|relation| {
                let record = relation_record_from_meta(snapshot, relation);
                (record.oid.0, record)
            })
            .collect::<BTreeMap<_, _>>()
    });
    let Some(table) = static_catalog_by_relation_oid(PG_CLASS_RELATION_OID) else {
        return dynamic_relations.into_values().collect();
    };
    for relation in catalog_rows(table.oid)
        .into_iter()
        .filter_map(|row| relation_record_from_pg_class_row(&row))
    {
        dynamic_relations.insert(relation.oid.0, relation);
    }
    dynamic_relations.into_values().collect()
}

pub fn relation_by_oid(oid: Oid) -> Option<RelationRecord> {
    if let Some(relation) = with_visible_catalog_snapshot(|snapshot| {
        snapshot
            .relation_meta_by_oid(oid)
            .map(|relation| relation_record_from_meta(snapshot, relation))
    }) {
        return Some(relation);
    }
    relation_pg_class_row_by_oid(oid).and_then(|row| relation_record_from_pg_class_row(&row))
}

pub fn relation_oid_exists(oid: Oid) -> bool {
    relation_by_oid(oid).is_some()
}

pub fn relation_summary_by_oid(oid: Oid) -> Option<RelationSummaryRecord> {
    if let Some(relation) = with_visible_catalog_snapshot(|snapshot| {
        snapshot
            .relation_meta_by_oid(oid)
            .map(|relation| relation_summary_from_meta(snapshot, relation))
    }) {
        return Some(relation);
    }
    relation_pg_class_row_by_oid(oid).and_then(|row| relation_summary_from_pg_class_row(&row))
}

pub fn relation_column_count(name: &str) -> Result<usize, CatalogError> {
    relation_by_name(name)
        .map(|relation| relation.columns.len())
        .ok_or_else(|| {
            CatalogError::new(
                "42P01",
                format!("relation \"{}\" does not exist", normalize_identifier(name)),
            )
        })
}

#[cfg(test)]
pub fn clear_for_tests() {
    with_catalog(|state| {
        *state = CatalogState::default();
        CATALOG_GENERATION.store(state.snapshot.generation, Ordering::Relaxed);
    });
    clear_catalog_lookup_caches();
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PgTypeRecord {
    pub oid: Oid,
    pub name: &'static str,
    pub namespace: Oid,
    pub owner: Oid,
    pub typlen: i16,
    pub typbyval: bool,
    pub typalign: u8,
    pub typdelim: u8,
    pub typinput: Oid,
    pub typoutput: Oid,
    pub typreceive: Oid,
    pub typsend: Oid,
    pub typmodin: Oid,
    pub typmodout: Oid,
    pub typisdefined: bool,
    pub typtype: u8,
    pub typcategory: u8,
    pub typispreferred: bool,
    pub typrelid: Oid,
    pub typelem: Oid,
    pub typarray: Oid,
    pub typbasetype: Oid,
    pub typtypmod: i32,
    pub typcollation: Oid,
    pub typsubscript: Oid,
    pub typstorage: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogTypeRecord {
    pub row_id: u64,
    pub oid: Oid,
    pub name: String,
    pub namespace: Oid,
    pub owner: Oid,
    pub typlen: i16,
    pub typbyval: bool,
    pub typalign: u8,
    pub typdelim: u8,
    pub typinput: Oid,
    pub typoutput: Oid,
    pub typreceive: Oid,
    pub typsend: Oid,
    pub typmodin: Oid,
    pub typmodout: Oid,
    pub typisdefined: bool,
    pub typtype: u8,
    pub typcategory: u8,
    pub typispreferred: bool,
    pub typrelid: Oid,
    pub typelem: Oid,
    pub typarray: Oid,
    pub typbasetype: Oid,
    pub typtypmod: i32,
    pub typcollation: Oid,
    pub typsubscript: Oid,
    pub typstorage: u8,
}

impl From<PgTypeRecord> for CatalogTypeRecord {
    fn from(record: PgTypeRecord) -> Self {
        Self {
            row_id: 0,
            oid: record.oid,
            name: record.name.to_owned(),
            namespace: record.namespace,
            owner: record.owner,
            typlen: record.typlen,
            typbyval: record.typbyval,
            typalign: record.typalign,
            typdelim: record.typdelim,
            typinput: record.typinput,
            typoutput: record.typoutput,
            typreceive: record.typreceive,
            typsend: record.typsend,
            typmodin: record.typmodin,
            typmodout: record.typmodout,
            typisdefined: record.typisdefined,
            typtype: record.typtype,
            typcategory: record.typcategory,
            typispreferred: record.typispreferred,
            typrelid: record.typrelid,
            typelem: record.typelem,
            typarray: record.typarray,
            typbasetype: record.typbasetype,
            typtypmod: record.typtypmod,
            typcollation: record.typcollation,
            typsubscript: record.typsubscript,
            typstorage: record.typstorage,
        }
    }
}

fn catalog_type_from_row(
    table: &StaticCatalogTable,
    row: &CatalogRow,
) -> Option<CatalogTypeRecord> {
    Some(CatalogTypeRecord {
        row_id: row.row_id,
        oid: catalog_row_value(table, row, "oid").and_then(catalog_value_oid)?,
        name: catalog_row_value(table, row, "typname").and_then(catalog_value_string)?,
        namespace: catalog_row_value(table, row, "typnamespace").and_then(catalog_value_oid)?,
        owner: catalog_row_value(table, row, "typowner").and_then(catalog_value_oid)?,
        typlen: catalog_row_value(table, row, "typlen").and_then(catalog_value_i16)?,
        typbyval: catalog_row_value(table, row, "typbyval").and_then(catalog_value_bool)?,
        typalign: catalog_row_value(table, row, "typalign").and_then(catalog_value_u8)?,
        typdelim: catalog_row_value(table, row, "typdelim").and_then(catalog_value_u8)?,
        typinput: catalog_row_value(table, row, "typinput").and_then(catalog_value_oid)?,
        typoutput: catalog_row_value(table, row, "typoutput").and_then(catalog_value_oid)?,
        typreceive: catalog_row_value(table, row, "typreceive").and_then(catalog_value_oid)?,
        typsend: catalog_row_value(table, row, "typsend").and_then(catalog_value_oid)?,
        typmodin: catalog_row_value(table, row, "typmodin").and_then(catalog_value_oid)?,
        typmodout: catalog_row_value(table, row, "typmodout").and_then(catalog_value_oid)?,
        typisdefined: catalog_row_value(table, row, "typisdefined").and_then(catalog_value_bool)?,
        typtype: catalog_row_value(table, row, "typtype").and_then(catalog_value_u8)?,
        typcategory: catalog_row_value(table, row, "typcategory").and_then(catalog_value_u8)?,
        typispreferred: catalog_row_value(table, row, "typispreferred")
            .and_then(catalog_value_bool)?,
        typrelid: catalog_row_value(table, row, "typrelid").and_then(catalog_value_oid)?,
        typelem: catalog_row_value(table, row, "typelem").and_then(catalog_value_oid)?,
        typarray: catalog_row_value(table, row, "typarray").and_then(catalog_value_oid)?,
        typbasetype: catalog_row_value(table, row, "typbasetype").and_then(catalog_value_oid)?,
        typtypmod: catalog_row_value(table, row, "typtypmod").and_then(catalog_value_i32)?,
        typcollation: catalog_row_value(table, row, "typcollation").and_then(catalog_value_oid)?,
        typsubscript: catalog_row_value(table, row, "typsubscript").and_then(catalog_value_oid)?,
        typstorage: catalog_row_value(table, row, "typstorage").and_then(catalog_value_u8)?,
    })
}

fn synthetic_catalog_rowtype_for_table(table: &StaticCatalogTable) -> Option<CatalogTypeRecord> {
    let template: CatalogTypeRecord = lookup_builtin_type(Oid(83))?.into();
    Some(CatalogTypeRecord {
        row_id: 0,
        oid: static_catalog_table_rowtype_oid(table),
        name: table.name.to_owned(),
        namespace: PG_CATALOG_NAMESPACE_OID,
        owner: POSTGRES_ROLE_OID,
        typlen: template.typlen,
        typbyval: template.typbyval,
        typalign: template.typalign,
        typdelim: template.typdelim,
        typinput: template.typinput,
        typoutput: template.typoutput,
        typreceive: template.typreceive,
        typsend: template.typsend,
        typmodin: template.typmodin,
        typmodout: template.typmodout,
        typisdefined: true,
        typtype: b'c',
        typcategory: b'C',
        typispreferred: false,
        typrelid: table.oid,
        typelem: INVALID_OID,
        typarray: INVALID_OID,
        typbasetype: INVALID_OID,
        typtypmod: -1,
        typcollation: INVALID_OID,
        typsubscript: INVALID_OID,
        typstorage: b'x',
    })
}

pub fn lookup_builtin_type(oid: Oid) -> Option<PgTypeRecord> {
    generated_catalog::STATIC_TYPES
        .iter()
        .find(|record| record.oid == oid)
        .copied()
}

fn canonical_catalog_type_name(name: &str) -> String {
    match normalize_identifier(name).as_str() {
        "boolean" => "bool".to_owned(),
        "character" => "bpchar".to_owned(),
        "character varying" => "varchar".to_owned(),
        "decimal" => "numeric".to_owned(),
        "integer" | "int" => "int4".to_owned(),
        "smallint" => "int2".to_owned(),
        "bigint" => "int8".to_owned(),
        "real" => "float4".to_owned(),
        "double precision" => "float8".to_owned(),
        "time without time zone" => "time".to_owned(),
        "time with time zone" => "timetz".to_owned(),
        "timestamp without time zone" => "timestamp".to_owned(),
        "timestamp with time zone" => "timestamptz".to_owned(),
        "bit varying" => "varbit".to_owned(),
        other => other.to_owned(),
    }
}

pub fn lookup_type(oid: Oid) -> Option<CatalogTypeRecord> {
    if let Some(record) = lookup_builtin_type(oid) {
        return Some(record.into());
    }
    if let Some(pg_type) = with_visible_catalog_snapshot(|snapshot| {
        snapshot
            .type_meta_by_oid(oid)
            .map(|pg_type| pg_type.record.clone())
    }) {
        return Some(pg_type);
    }
    if let Some(pg_type) =
        static_catalog_by_rowtype_oid(oid).and_then(synthetic_catalog_rowtype_for_table)
    {
        return Some(pg_type);
    }

    let table = static_catalog_by_relation_oid(PG_TYPE_RELATION_OID)?;
    catalog_rows(PG_TYPE_RELATION_OID)
        .into_iter()
        .find_map(|row| {
            let record = catalog_type_from_row(table, &row)?;
            (record.oid == oid).then_some(record)
        })
}

pub fn type_by_name(name: &str, namespace: Oid) -> Option<CatalogTypeRecord> {
    let canonical_name = canonical_catalog_type_name(name);
    if let Some(pg_type) = with_visible_catalog_snapshot(|snapshot| {
        snapshot
            .type_meta_by_name(&canonical_name, namespace)
            .map(|pg_type| pg_type.record.clone())
    }) {
        return Some(pg_type);
    }
    let table = static_catalog_by_relation_oid(PG_TYPE_RELATION_OID)?;
    catalog_rows(PG_TYPE_RELATION_OID)
        .into_iter()
        .find_map(|row| {
            let record = catalog_type_from_row(table, &row)?;
            (record.namespace == namespace && record.name == canonical_name).then_some(record)
        })
        .or_else(|| {
            (namespace == PG_CATALOG_NAMESPACE_OID)
                .then(|| static_catalog_by_name(&canonical_name))
                .flatten()
                .and_then(synthetic_catalog_rowtype_for_table)
        })
}

pub fn builtin_type_by_name(name: &str, namespace: Oid) -> Option<PgTypeRecord> {
    if namespace != PG_CATALOG_NAMESPACE_OID {
        return None;
    }

    let canonical_name = canonical_catalog_type_name(name);

    generated_catalog::STATIC_TYPES
        .iter()
        .copied()
        .find(|record| record.name == canonical_name.as_str())
}

pub fn enum_oids_by_sort_order(enum_type_oid: Oid) -> Vec<Oid> {
    let Some(table) = static_catalog_by_relation_oid(PG_ENUM_RELATION_OID) else {
        return Vec::new();
    };
    let mut rows = catalog_rows(PG_ENUM_RELATION_OID)
        .into_iter()
        .filter_map(|row| {
            let oid = catalog_row_value(table, &row, "oid").and_then(catalog_value_oid)?;
            let row_type =
                catalog_row_value(table, &row, "enumtypid").and_then(catalog_value_oid)?;
            if row_type != enum_type_oid {
                return None;
            }
            let sort_order =
                catalog_row_value(table, &row, "enumsortorder").and_then(catalog_value_f32)?;
            Some((sort_order, oid))
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.0
            .total_cmp(&right.0)
            .then_with(|| left.1.0.cmp(&right.1.0))
    });
    rows.into_iter().map(|(_, oid)| oid).collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PgProcRecord {
    pub oid: Oid,
    pub name: &'static str,
    pub namespace: Oid,
    pub owner: Oid,
    pub language: Oid,
    pub cost: u32,
    pub rows: u32,
    pub variadic: Oid,
    pub support: Oid,
    pub kind: u8,
    pub security_definer: bool,
    pub leakproof: bool,
    pub strict: bool,
    pub returns_set: bool,
    pub volatility: u8,
    pub parallel: u8,
    pub return_type: Oid,
    pub arg_types: &'static [Oid],
    pub arg_defaults: u16,
    pub source: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PgAggregateRecord {
    pub function_oid: Oid,
    pub kind: u8,
    pub direct_arg_count: u16,
    pub transition_fn: Oid,
    pub final_fn: Oid,
    pub combine_fn: Oid,
    pub serial_fn: Oid,
    pub deserial_fn: Oid,
    pub moving_transition_fn: Oid,
    pub moving_inverse_fn: Oid,
    pub moving_final_fn: Oid,
    pub final_extra: bool,
    pub moving_final_extra: bool,
    pub final_modify: u8,
    pub moving_final_modify: u8,
    pub sort_operator: Oid,
    pub transition_type: Oid,
    pub transition_space: i32,
    pub moving_transition_type: Oid,
    pub moving_transition_space: i32,
    pub init_value: Option<&'static str>,
    pub moving_init_value: Option<&'static str>,
}

pub fn builtin_proc_by_oid(oid: Oid) -> Option<&'static PgProcRecord> {
    generated_catalog::STATIC_PROCS
        .iter()
        .find(|record| record.oid == oid)
}

pub fn builtin_procs_by_name(name: &str) -> impl Iterator<Item = &'static PgProcRecord> {
    let name = normalize_identifier(name);
    generated_catalog::STATIC_PROCS
        .iter()
        .filter(move |record| record.name == name.as_str())
}

pub fn builtin_aggregate_by_proc_oid(function_oid: Oid) -> Option<&'static PgAggregateRecord> {
    generated_catalog::STATIC_AGGREGATES
        .iter()
        .find(|record| record.function_oid == function_oid)
}

mod generated_catalog {
    include!(concat!(env!("OUT_DIR"), "/generated_static_catalog.rs"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::{Mutex, MutexGuard};

    static CATALOG_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn catalog_test_lock() -> MutexGuard<'static, ()> {
        CATALOG_TEST_LOCK
            .lock()
            .expect("catalog test mutex poisoned")
    }

    #[test]
    fn has_pgbench_scalar_types() {
        for oid in [
            INT4_OID,
            INT8_OID,
            TEXT_OID,
            TID_OID,
            XID_OID,
            CID_OID,
            BPCHAR_OID,
            VARCHAR_OID,
            TIMESTAMP_OID,
            TIMESTAMPTZ_OID,
        ] {
            let record = lookup_builtin_type(oid).expect("builtin type");
            assert_eq!(record.oid, oid);
            assert!(record.typisdefined);
            assert_ne!(record.typinput, INVALID_OID);
            assert_ne!(record.typoutput, INVALID_OID);
        }
    }

    #[test]
    fn resolves_builtin_types_by_catalog_name() {
        assert_eq!(
            builtin_type_by_name("int4", PG_CATALOG_NAMESPACE_OID)
                .expect("int4")
                .oid,
            INT4_OID
        );
        assert_eq!(
            builtin_type_by_name("integer", PG_CATALOG_NAMESPACE_OID)
                .expect("integer alias")
                .oid,
            INT4_OID
        );
        assert_eq!(
            builtin_type_by_name("timestamp with time zone", PG_CATALOG_NAMESPACE_OID)
                .expect("timestamptz alias")
                .oid,
            TIMESTAMPTZ_OID
        );
        assert!(builtin_type_by_name("int4", PUBLIC_NAMESPACE_OID).is_none());
    }

    #[test]
    fn has_timestamp_assignment_casts() {
        let cast = builtin_cast_by_source_target(TIMESTAMPTZ_OID, TIMESTAMP_OID)
            .expect("timestamptz to timestamp cast");
        assert_eq!(cast.function, Oid(2027));
        assert_eq!(cast.context, b'a');
        assert_eq!(cast.method, b'f');

        let proc = builtin_proc_by_oid(cast.function).expect("cast function proc");
        assert_eq!(proc.return_type, TIMESTAMP_OID);
        assert_eq!(proc.arg_types, [TIMESTAMPTZ_OID]);
        assert_eq!(proc.volatility, b's');
    }

    fn row_value<'a>(table_name: &str, row: &'a CatalogRow, column_name: &str) -> &'a CatalogValue {
        let table = static_catalog_by_name(table_name).expect("catalog table");
        catalog_row_value(table, row, column_name).expect("catalog column")
    }

    fn value_name(value: &CatalogValue) -> Option<&str> {
        match value {
            CatalogValue::Name(value) => Some(value),
            _ => None,
        }
    }

    fn value_oid(value: &CatalogValue) -> Option<Oid> {
        match value {
            CatalogValue::Oid(value) => Some(*value),
            _ => None,
        }
    }

    fn value_i16(value: &CatalogValue) -> Option<i16> {
        match value {
            CatalogValue::Int16(value) => Some(*value),
            _ => None,
        }
    }

    fn upsert_named_catalog_row(table_name: &str, row_id: u64, values: &[(&str, &str)]) -> u64 {
        let table = static_catalog_by_name(table_name).expect("catalog table");
        let values = values
            .iter()
            .map(|(column, value)| (*column, (*value).to_owned()))
            .collect::<BTreeMap<_, _>>();
        let row = table
            .columns
            .iter()
            .map(|column| values.get(column.name).cloned())
            .collect::<Vec<_>>();
        upsert_catalog_row(table.oid, row_id, row).expect("upsert catalog row")
    }

    #[test]
    fn generated_static_catalog_has_core_rows() {
        assert!(static_catalog_by_name("pg_type").is_some());
        assert!(static_catalog_by_name("pg_proc").is_some());
        assert!(static_catalog_by_name("pg_am").is_some());
        assert!(static_catalog_by_name("pg_opclass").is_some());

        assert_eq!(
            builtin_type_by_name("float8", PG_CATALOG_NAMESPACE_OID)
                .expect("float8")
                .oid,
            FLOAT8_OID
        );
        assert!(builtin_procs_by_name("generate_series").count() > 0);
        assert!(
            catalog_rows(Oid(2601))
                .iter()
                .any(|row| { value_name(row_value("pg_am", row, "amname")) == Some("btree") })
        );
        assert!(
            catalog_rows(Oid(2601))
                .iter()
                .any(|row| { value_name(row_value("pg_am", row, "amname")) == Some("hash") })
        );
        assert!(btree_opclass_for_type(INT4_OID).is_some());
        assert!(builtin_cast_by_source_target(INT4_OID, OID_OID).is_some());
    }

    #[test]
    fn static_virtual_catalog_rowtypes_are_lookupable() {
        let pg_operator = static_catalog_by_name("pg_operator").expect("pg_operator");
        assert_eq!(pg_operator.rowtype_oid, INVALID_OID);

        let rowtype_oid = static_catalog_rowtype_oid(pg_operator.oid).expect("rowtype oid");
        assert_ne!(rowtype_oid, INVALID_OID);

        let rowtype = lookup_type(rowtype_oid).expect("synthetic catalog rowtype");
        assert_eq!(rowtype.oid, rowtype_oid);
        assert_eq!(rowtype.name, "pg_operator");
        assert_eq!(rowtype.namespace, PG_CATALOG_NAMESPACE_OID);
        assert_eq!(rowtype.typtype, b'c');
        assert_eq!(rowtype.typcategory, b'C');
        assert_eq!(rowtype.typrelid, pg_operator.oid);

        let by_name =
            type_by_name("pg_operator", PG_CATALOG_NAMESPACE_OID).expect("rowtype by name");
        assert_eq!(by_name, rowtype);
    }

    #[test]
    fn pg_proc_bootstrap_defaults_are_normalized() {
        let table = static_catalog_by_name("pg_proc").expect("pg_proc");
        let row = catalog_rows(PG_PROC_RELATION_OID)
            .into_iter()
            .find(|row| value_name(row_value("pg_proc", row, "proname")) == Some("parse_ident"))
            .expect("parse_ident proc row");
        assert_eq!(
            value_i16(row_value("pg_proc", &row, "pronargdefaults")),
            Some(1)
        );
        let defaults = catalog_row_value(table, &row, "proargdefaults")
            .and_then(catalog_value_string)
            .expect("proargdefaults");
        assert!(defaults.starts_with("({CONST :consttype 16"));
        assert!(defaults.contains(":constvalue 1 [ 1 0 0 0 0 0 0 0 ]"));
    }

    #[test]
    fn ensure_database_adds_synthetic_pg_database_row() {
        let _guard = catalog_test_lock();
        clear_for_tests();
        abort_implicit_transaction();
        let oid = ensure_database("regression");
        let table = static_catalog_by_name("pg_database").expect("pg_database");
        let row = catalog_rows(PG_DATABASE_RELATION_OID)
            .into_iter()
            .find(|row| {
                row_value("pg_database", row, "datname")
                    == &CatalogValue::Name("regression".to_owned())
            })
            .expect("regression database row");

        assert_eq!(
            catalog_row_value(table, &row, "oid").and_then(catalog_value_oid),
            Some(oid)
        );
        assert_eq!(row.row_id, u64::from(oid.0));
    }

    #[test]
    fn classifies_pgbench_critical_virtual_catalogs() {
        let required = [
            ("pg_type", VirtualCatalogPolicy::Dynamic),
            ("pg_proc", VirtualCatalogPolicy::Dynamic),
            ("pg_operator", VirtualCatalogPolicy::Dynamic),
            ("pg_aggregate", VirtualCatalogPolicy::Static),
            ("pg_namespace", VirtualCatalogPolicy::Static),
            ("pg_cast", VirtualCatalogPolicy::Dynamic),
            ("pg_class", VirtualCatalogPolicy::Dynamic),
            ("pg_attribute", VirtualCatalogPolicy::Dynamic),
            ("pg_index", VirtualCatalogPolicy::Dynamic),
            ("pg_constraint", VirtualCatalogPolicy::Dynamic),
            ("pg_am", VirtualCatalogPolicy::Static),
            ("pg_opclass", VirtualCatalogPolicy::Dynamic),
            ("pg_opfamily", VirtualCatalogPolicy::Dynamic),
            ("pg_amop", VirtualCatalogPolicy::Dynamic),
            ("pg_amproc", VirtualCatalogPolicy::Dynamic),
            ("pg_rewrite", VirtualCatalogPolicy::Dynamic),
            ("pg_statistic", VirtualCatalogPolicy::Static),
            ("pg_statistic_ext", VirtualCatalogPolicy::Static),
            ("pg_statistic_ext_data", VirtualCatalogPolicy::Static),
            ("pg_authid", VirtualCatalogPolicy::Static),
            ("pg_auth_members", VirtualCatalogPolicy::Static),
            ("pg_parameter_acl", VirtualCatalogPolicy::Static),
        ];

        for (name, policy) in required {
            let record = virtual_catalogs()
                .iter()
                .find(|record| record.name == name)
                .unwrap_or_else(|| panic!("{name} should have virtual catalog policy"));
            assert_eq!(record.policy, policy, "{name}");
        }
    }

    #[test]
    fn generic_overlay_rows_drive_relation_views() {
        let _guard = catalog_test_lock();
        clear_for_tests();
        abort_implicit_transaction();
        let relation_oid = Oid(50_000);
        let type_oid = Oid(50_001);
        let index_oid = Oid(50_002);
        let constraint_oid = Oid(50_003);
        let initial_generation = current_generation();

        upsert_named_catalog_row(
            "pg_class",
            relation_oid.0 as u64,
            &[
                ("oid", "50000"),
                ("relname", "pgbench_accounts"),
                ("relnamespace", "2200"),
                ("reltype", "50001"),
                ("relowner", "10"),
                ("relam", "2"),
                ("relfilenode", "50000"),
                ("relhasindex", "f"),
                ("relpersistence", "p"),
                ("relkind", "r"),
                ("relnatts", "2"),
            ],
        );
        upsert_named_catalog_row(
            "pg_type",
            type_oid.0 as u64,
            &[
                ("oid", "50001"),
                ("typname", "pgbench_accounts"),
                ("typnamespace", "2200"),
                ("typowner", "10"),
                ("typlen", "-1"),
                ("typbyval", "f"),
                ("typtype", "c"),
                ("typcategory", "C"),
                ("typisdefined", "t"),
                ("typdelim", ","),
                ("typrelid", "50000"),
                ("typalign", "d"),
                ("typstorage", "x"),
            ],
        );
        upsert_named_catalog_row(
            "pg_attribute",
            0,
            &[
                ("attrelid", "50000"),
                ("attname", "aid"),
                ("atttypid", "23"),
                ("attlen", "4"),
                ("attnum", "1"),
                ("atttypmod", "-1"),
                ("attbyval", "t"),
                ("attalign", "i"),
                ("attstorage", "p"),
                ("attnotnull", "t"),
                ("attisdropped", "f"),
            ],
        );
        upsert_named_catalog_row(
            "pg_attribute",
            0,
            &[
                ("attrelid", "50000"),
                ("attname", "filler"),
                ("atttypid", "1042"),
                ("attlen", "-1"),
                ("attnum", "2"),
                ("atttypmod", "-1"),
                ("attbyval", "f"),
                ("attalign", "i"),
                ("attstorage", "x"),
                ("attnotnull", "f"),
                ("attisdropped", "f"),
            ],
        );
        commit_implicit_transaction();
        assert!(current_generation() > initial_generation);
        let after_create_generation = current_generation();
        let relation = relation_by_name("PgBench_Accounts").expect("relation");
        assert_eq!(relation.name, "pgbench_accounts");
        assert_eq!(relation.columns[0].name, "aid");
        assert_eq!(
            relation_by_name("pgbench_accounts").unwrap().oid,
            relation.oid
        );
        assert!(relation_oid_exists(relation.oid));
        assert_eq!(
            relation_oid_by_name_in_namespace("pgbench_accounts", PUBLIC_NAMESPACE_OID),
            Some(relation.oid)
        );
        assert_eq!(
            relation_by_oid(relation.oid).unwrap().name,
            "pgbench_accounts"
        );
        let relation_summary = relation_summary_by_oid(relation.oid).unwrap();
        assert_eq!(relation_summary.name, "pgbench_accounts");
        assert_eq!(relation_summary.column_count, 2);
        assert!(!relation_summary.has_indexes);
        assert!(!relation_summary.has_primary_key);
        let relation_summary =
            relation_summary_by_name_in_namespace("PgBench_Accounts", PUBLIC_NAMESPACE_OID)
                .unwrap();
        assert_eq!(relation_summary.name, "pgbench_accounts");
        assert_eq!(relation_summary.column_count, 2);
        assert!(!relation_summary.has_indexes);
        assert!(!relation_summary.has_primary_key);
        assert_eq!(
            relation_column_by_attnum(relation.oid, 1).unwrap().name,
            "aid"
        );
        upsert_named_catalog_row(
            "pg_class",
            relation_oid.0 as u64,
            &[
                ("oid", "50000"),
                ("relname", "pgbench_accounts"),
                ("relnamespace", "2200"),
                ("reltype", "50001"),
                ("relowner", "10"),
                ("relam", "2"),
                ("relfilenode", "50000"),
                ("relhasindex", "t"),
                ("relpersistence", "p"),
                ("relkind", "r"),
                ("relnatts", "2"),
            ],
        );
        upsert_named_catalog_row(
            "pg_class",
            index_oid.0 as u64,
            &[
                ("oid", "50002"),
                ("relname", "pgbench_accounts_pkey"),
                ("relnamespace", "2200"),
                ("reltype", "0"),
                ("relowner", "10"),
                ("relam", "403"),
                ("relfilenode", "50002"),
                ("relhasindex", "f"),
                ("relpersistence", "p"),
                ("relkind", "i"),
                ("relnatts", "1"),
            ],
        );
        upsert_named_catalog_row(
            "pg_attribute",
            0,
            &[
                ("attrelid", "50002"),
                ("attname", "aid"),
                ("atttypid", "23"),
                ("attlen", "4"),
                ("attnum", "1"),
                ("atttypmod", "-1"),
                ("attbyval", "t"),
                ("attalign", "i"),
                ("attstorage", "p"),
                ("attnotnull", "t"),
                ("attisdropped", "f"),
            ],
        );
        upsert_named_catalog_row(
            "pg_index",
            index_oid.0 as u64,
            &[
                ("indexrelid", "50002"),
                ("indrelid", "50000"),
                ("indnatts", "1"),
                ("indnkeyatts", "1"),
                ("indisunique", "t"),
                ("indisprimary", "t"),
                ("indisvalid", "t"),
                ("indisready", "t"),
                ("indislive", "t"),
                ("indkey", "1"),
            ],
        );
        upsert_named_catalog_row(
            "pg_constraint",
            constraint_oid.0 as u64,
            &[
                ("oid", "50003"),
                ("conname", "pgbench_accounts_pkey"),
                ("connamespace", "2200"),
                ("contype", "p"),
                ("conrelid", "50000"),
                ("conindid", "50002"),
                ("conkey", "1"),
            ],
        );
        commit_implicit_transaction();
        let after_primary_key_generation = current_generation();
        assert!(after_primary_key_generation > after_create_generation);
        assert_eq!(
            relation_by_name("pgbench_accounts").unwrap().primary_key,
            vec!["aid"]
        );
        assert_eq!(
            unique_index_oids_for_relation_oid(relation.oid),
            vec![index_oid]
        );
        assert_eq!(
            primary_key_index_oid_for_relation_oid(relation.oid),
            Some(index_oid)
        );
        assert_eq!(
            primary_key_index_record_for_relation_oid(relation.oid)
                .unwrap()
                .index_oid,
            index_oid
        );
        let relation_summary = relation_summary_by_oid(relation.oid).unwrap();
        assert_eq!(relation_summary.column_count, 2);
        assert!(relation_summary.has_indexes);
        assert!(relation_summary.has_primary_key);
        let relation_summary =
            relation_summary_by_name_in_namespace("pgbench_accounts", PUBLIC_NAMESPACE_OID)
                .unwrap();
        assert_eq!(relation_summary.column_count, 2);
        assert!(relation_summary.has_indexes);
        assert!(relation_summary.has_primary_key);
        assert!(catalog_rows(PG_CLASS_RELATION_OID).iter().any(|row| {
            value_name(row_value("pg_class", row, "relname")) == Some("pgbench_accounts")
        }));
        assert!(catalog_rows(PG_TYPE_RELATION_OID).iter().any(|row| {
            value_name(row_value("pg_type", row, "typname")) == Some("pgbench_accounts")
        }));
        assert!(catalog_rows(PG_ATTRIBUTE_RELATION_OID).iter().any(|row| {
            value_oid(row_value("pg_attribute", row, "attrelid")) == Some(relation.oid)
                && value_name(row_value("pg_attribute", row, "attname")) == Some("aid")
        }));
        assert!(catalog_rows(PG_INDEX_RELATION_OID).iter().any(|row| {
            value_oid(row_value("pg_index", row, "indrelid")) == Some(relation.oid)
        }));
        assert!(catalog_rows(PG_CONSTRAINT_RELATION_OID).iter().any(|row| {
            value_oid(row_value("pg_constraint", row, "conrelid")) == Some(relation.oid)
                && value_name(row_value("pg_constraint", row, "conname"))
                    == Some("pgbench_accounts_pkey")
        }));
        delete_catalog_row(PG_CLASS_RELATION_OID, relation.oid.0 as u64).unwrap();
        commit_implicit_transaction();
        assert!(current_generation() > after_primary_key_generation);
        assert!(relation_by_name("pgbench_accounts").is_none());
        assert!(!relation_oid_exists(relation.oid));
        assert!(!catalog_rows(PG_CLASS_RELATION_OID).iter().any(|row| {
            value_name(row_value("pg_class", row, "relname")) == Some("pgbench_accounts")
        }));
    }

    #[test]
    fn physical_columns_preserve_dropped_attnum_holes() {
        let _guard = catalog_test_lock();
        clear_for_tests();
        abort_implicit_transaction();
        let relation_oid = Oid(50_100);

        upsert_named_catalog_row(
            "pg_class",
            relation_oid.0 as u64,
            &[
                ("oid", "50100"),
                ("relname", "dropped_columns"),
                ("relnamespace", "2200"),
                ("reltype", "50101"),
                ("relowner", "10"),
                ("relam", "2"),
                ("relfilenode", "50100"),
                ("relhasindex", "f"),
                ("relpersistence", "p"),
                ("relkind", "r"),
                ("relnatts", "3"),
            ],
        );
        upsert_named_catalog_row(
            "pg_attribute",
            0,
            &[
                ("attrelid", "50100"),
                ("attname", "........pg.dropped.1........"),
                ("atttypid", "0"),
                ("attlen", "4"),
                ("attnum", "1"),
                ("atttypmod", "-1"),
                ("attbyval", "t"),
                ("attalign", "i"),
                ("attstorage", "p"),
                ("attnotnull", "f"),
                ("attisdropped", "t"),
            ],
        );
        upsert_named_catalog_row(
            "pg_attribute",
            0,
            &[
                ("attrelid", "50100"),
                ("attname", "live_b"),
                ("atttypid", "23"),
                ("attlen", "4"),
                ("attnum", "2"),
                ("atttypmod", "-1"),
                ("attbyval", "t"),
                ("attalign", "i"),
                ("attstorage", "p"),
                ("attnotnull", "f"),
                ("attisdropped", "f"),
            ],
        );
        upsert_named_catalog_row(
            "pg_attribute",
            0,
            &[
                ("attrelid", "50100"),
                ("attname", "live_c"),
                ("atttypid", "25"),
                ("attlen", "-1"),
                ("attnum", "3"),
                ("atttypmod", "-1"),
                ("attbyval", "f"),
                ("attalign", "i"),
                ("attstorage", "x"),
                ("attnotnull", "f"),
                ("attisdropped", "f"),
                ("attcollation", "100"),
            ],
        );
        commit_implicit_transaction();

        let relation = relation_by_oid(relation_oid).expect("relation");
        assert_eq!(
            relation
                .columns
                .iter()
                .map(|column| column.name.as_str())
                .collect::<Vec<_>>(),
            vec!["live_b", "live_c"]
        );
        assert_eq!(
            relation_summary_by_oid(relation_oid)
                .expect("relation summary")
                .column_count,
            3
        );
        assert!(relation_column_by_attnum(relation_oid, 1).is_none());
        assert_eq!(
            relation_column_by_attnum(relation_oid, 2)
                .expect("attnum 2")
                .name,
            "live_b"
        );

        let dropped =
            relation_physical_column_by_attnum(relation_oid, 1).expect("physical attnum 1");
        assert!(dropped.is_dropped);
        assert_eq!(dropped.type_oid, INVALID_OID);
        assert_eq!(dropped.attlen, 4);

        let live = relation_physical_column_by_attnum(relation_oid, 3).expect("physical attnum 3");
        assert!(!live.is_dropped);
        assert_eq!(live.name, "live_c");
        assert_eq!(live.type_oid, TEXT_OID);
        assert_eq!(live.attcollation, DEFAULT_COLLATION_OID);
    }
}
