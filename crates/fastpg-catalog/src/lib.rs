#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock, RwLock};

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

pub const DEFAULT_COLLATION_OID: Oid = Oid(100);
pub const C_COLLATION_OID: Oid = Oid(950);

const INVALID_OID: Oid = Oid(0);
const BOOTSTRAP_SUPERUSER_OID: Oid = Oid(10);

pub const PG_CATALOG_NAMESPACE_OID: Oid = Oid(11);
pub const PUBLIC_NAMESPACE_OID: Oid = Oid(2200);
const FIRST_DYNAMIC_RELATION_OID: u32 = 16_384;
const PG_CLASS_RELATION_OID: Oid = Oid(1259);
const PG_ATTRIBUTE_RELATION_OID: Oid = Oid(1249);
const PG_TYPE_RELATION_OID: Oid = Oid(1247);
const PG_INDEX_RELATION_OID: Oid = Oid(2610);
const PG_CONSTRAINT_RELATION_OID: Oid = Oid(2606);
const PG_PROC_RELATION_OID: Oid = Oid(1255);
const HEAP_TABLE_AM_OID: Oid = Oid(2);
const BTREE_INDEX_AM_OID: Oid = Oid(403);
const FIRST_NORMAL_TRANSACTION_ID: i32 = 3;
const FIRST_MULTI_XACT_ID: i32 = 1;
const PRIMARY_KEY_INDEX_OID_OFFSET: u32 = 1_000_000_000;
const ARRAY_IN_PROC_OID: Oid = Oid(750);
const ARRAY_OUT_PROC_OID: Oid = Oid(751);
const ARRAY_RECV_PROC_OID: Oid = Oid(2400);
const ARRAY_SEND_PROC_OID: Oid = Oid(2401);
const ARRAY_SUBSCRIPT_HANDLER_PROC_OID: Oid = Oid(6179);

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
}

impl ColumnRecord {
    pub fn new(name: impl Into<String>, type_oid: Oid, type_mod: i32, is_not_null: bool) -> Self {
        Self {
            name: normalize_identifier(&name.into()),
            type_oid,
            type_mod,
            is_not_null,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelationRecord {
    pub oid: Oid,
    pub type_oid: Oid,
    pub namespace: Oid,
    pub name: String,
    pub columns: Vec<ColumnRecord>,
    pub primary_key: Vec<String>,
    pub primary_key_constraint_oid: Option<Oid>,
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

#[derive(Debug, Default)]
struct CatalogOverlay {
    rows: BTreeMap<u32, BTreeMap<u64, Arc<CatalogRow>>>,
    tombstones: BTreeMap<u32, BTreeSet<u64>>,
}

impl CatalogOverlay {
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

    fn delete(&mut self, relation_oid: Oid, row_id: u64) {
        self.rows.entry(relation_oid.0).or_default().remove(&row_id);
        self.tombstones
            .entry(relation_oid.0)
            .or_default()
            .insert(row_id);
    }
}

#[derive(Debug)]
struct CatalogState {
    next_relation_oid: u32,
    next_object_oid: u32,
    relations_by_name: BTreeMap<String, RelationRecord>,
    relation_names_by_oid: BTreeMap<u32, String>,
    overlay: CatalogOverlay,
    generation: u64,
}

impl Default for CatalogState {
    fn default() -> Self {
        let first_dynamic_oid = FIRST_DYNAMIC_RELATION_OID.max(max_static_oid().saturating_add(1));
        Self {
            next_relation_oid: first_dynamic_oid,
            next_object_oid: first_dynamic_oid,
            relations_by_name: BTreeMap::new(),
            relation_names_by_oid: BTreeMap::new(),
            overlay: CatalogOverlay::default(),
            generation: 1,
        }
    }
}

static CATALOG: OnceLock<RwLock<CatalogState>> = OnceLock::new();

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

fn catalog_relation_oid(name: &str) -> Option<Oid> {
    static_catalog_by_name(name).map(|table| table.oid)
}

fn max_static_oid() -> u32 {
    generated_catalog::STATIC_CATALOG_TABLES
        .iter()
        .flat_map(|table| {
            let oid_column = table.columns.iter().position(|column| column.name == "oid");
            table
                .rows
                .iter()
                .filter_map(move |row| {
                    oid_column
                        .and_then(|index| row.values.get(index))
                        .and_then(|value| static_value_as_u32(*value))
                })
                .chain(std::iter::once(table.oid.0))
                .chain(std::iter::once(table.rowtype_oid.0))
        })
        .max()
        .unwrap_or(FIRST_DYNAMIC_RELATION_OID)
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

fn static_row_to_catalog_row(table: &StaticCatalogTable, row: &StaticCatalogRow) -> CatalogRow {
    CatalogRow {
        relation_oid: table.oid,
        row_id: row.row_id,
        values: row
            .values
            .iter()
            .copied()
            .zip(table.columns.iter())
            .map(|(value, column)| static_value_to_catalog_value(column, value))
            .collect(),
    }
}

pub fn catalog_rows(relation_oid: Oid) -> Vec<CatalogRow> {
    let Some(table) = static_catalog_by_relation_oid(relation_oid) else {
        return Vec::new();
    };
    with_catalog_read(|state| {
        let tombstones = state.overlay.tombstones.get(&relation_oid.0);
        let mut rows = BTreeMap::<u64, CatalogRow>::new();
        for row in table.rows {
            if tombstones.is_some_and(|tombstones| tombstones.contains(&row.row_id)) {
                continue;
            }
            rows.insert(row.row_id, static_row_to_catalog_row(table, row));
        }
        if let Some(overlay_rows) = state.overlay.rows.get(&relation_oid.0) {
            for (row_id, row) in overlay_rows {
                rows.insert(*row_id, row.as_ref().clone());
            }
        }
        rows.into_values().collect()
    })
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

fn default_catalog_value(column: &StaticCatalogColumn) -> CatalogValue {
    if !column.attnotnull {
        return CatalogValue::Null;
    }
    match column.type_oid {
        BOOL_OID => CatalogValue::Bool(false),
        CHAR_OID => CatalogValue::Char(0),
        INT2_OID => CatalogValue::Int16(0),
        INT4_OID => CatalogValue::Int32(0),
        FLOAT4_OID => CatalogValue::Float32(0.0),
        NAME_OID => CatalogValue::Name(String::new()),
        TEXT_OID | PG_NODE_TREE_OID => CatalogValue::Text(String::new()),
        OIDVECTOR_OID => CatalogValue::OidVector(Vec::new()),
        INT2VECTOR_OID => CatalogValue::Int2Vector(Vec::new()),
        OID_OID | REGCLASS_OID => CatalogValue::Oid(INVALID_OID),
        _ if column.type_name.starts_with("reg") => CatalogValue::Oid(INVALID_OID),
        _ if column.type_name.starts_with('_') => CatalogValue::Null,
        _ => CatalogValue::Raw(String::new()),
    }
}

fn catalog_row_from_named_values(
    relation_oid: Oid,
    row_id: u64,
    named_values: Vec<(&'static str, CatalogValue)>,
) -> CatalogRow {
    let table = static_catalog_by_relation_oid(relation_oid)
        .unwrap_or_else(|| panic!("missing generated catalog table {}", relation_oid.0));
    let named_values = named_values.into_iter().collect::<BTreeMap<_, _>>();
    CatalogRow {
        relation_oid,
        row_id,
        values: table
            .columns
            .iter()
            .map(|column| {
                named_values
                    .get(column.name)
                    .cloned()
                    .unwrap_or_else(|| default_catalog_value(column))
            })
            .collect(),
    }
}

fn relation_primary_key_index_oid(relation: &RelationRecord) -> Option<Oid> {
    if relation.primary_key.is_empty() {
        return None;
    }
    relation
        .oid
        .0
        .checked_add(PRIMARY_KEY_INDEX_OID_OFFSET)
        .map(Oid)
}

fn primary_key_index_name(relation: &RelationRecord) -> String {
    format!("{}_pkey", relation.name)
}

fn relation_column_attnums(
    relation: &RelationRecord,
    columns: &[String],
) -> Result<Vec<i16>, CatalogError> {
    columns
        .iter()
        .map(|column_name| {
            relation
                .columns
                .iter()
                .position(|column| &column.name == column_name)
                .and_then(|index| i16::try_from(index + 1).ok())
                .ok_or_else(|| {
                    CatalogError::new("42703", format!("column \"{column_name}\" does not exist"))
                })
        })
        .collect()
}

struct PgClassOverlayInput<'a> {
    oid: Oid,
    namespace: Oid,
    name: &'a str,
    reltype: Oid,
    relkind: u8,
    relam: Oid,
    column_count: usize,
    has_index: bool,
}

fn pg_class_overlay_row(input: PgClassOverlayInput<'_>) -> CatalogRow {
    catalog_row_from_named_values(
        PG_CLASS_RELATION_OID,
        input.oid.0 as u64,
        vec![
            ("oid", CatalogValue::Oid(input.oid)),
            ("relname", CatalogValue::Name(input.name.to_owned())),
            ("relnamespace", CatalogValue::Oid(input.namespace)),
            ("reltype", CatalogValue::Oid(input.reltype)),
            ("reloftype", CatalogValue::Oid(INVALID_OID)),
            ("relowner", CatalogValue::Oid(BOOTSTRAP_SUPERUSER_OID)),
            ("relam", CatalogValue::Oid(input.relam)),
            ("relfilenode", CatalogValue::Oid(input.oid)),
            ("reltablespace", CatalogValue::Oid(INVALID_OID)),
            ("relpages", CatalogValue::Int32(0)),
            ("reltuples", CatalogValue::Float32(-1.0)),
            ("relallvisible", CatalogValue::Int32(0)),
            ("relallfrozen", CatalogValue::Int32(0)),
            ("reltoastrelid", CatalogValue::Oid(INVALID_OID)),
            ("relhasindex", CatalogValue::Bool(input.has_index)),
            ("relisshared", CatalogValue::Bool(false)),
            ("relpersistence", CatalogValue::Char(b'p')),
            ("relkind", CatalogValue::Char(input.relkind)),
            (
                "relnatts",
                CatalogValue::Int16(input.column_count.min(i16::MAX as usize) as i16),
            ),
            ("relchecks", CatalogValue::Int16(0)),
            ("relhasrules", CatalogValue::Bool(false)),
            ("relhastriggers", CatalogValue::Bool(false)),
            ("relhassubclass", CatalogValue::Bool(false)),
            ("relrowsecurity", CatalogValue::Bool(false)),
            ("relforcerowsecurity", CatalogValue::Bool(false)),
            ("relispopulated", CatalogValue::Bool(true)),
            ("relreplident", CatalogValue::Char(b'n')),
            ("relispartition", CatalogValue::Bool(false)),
            ("relrewrite", CatalogValue::Oid(INVALID_OID)),
            (
                "relfrozenxid",
                CatalogValue::Int32(FIRST_NORMAL_TRANSACTION_ID),
            ),
            ("relminmxid", CatalogValue::Int32(FIRST_MULTI_XACT_ID)),
        ],
    )
}

fn pg_type_overlay_row(relation: &RelationRecord) -> CatalogRow {
    let record_type = lookup_type(Oid(2249));
    catalog_row_from_named_values(
        PG_TYPE_RELATION_OID,
        relation.type_oid.0 as u64,
        vec![
            ("oid", CatalogValue::Oid(relation.type_oid)),
            ("typname", CatalogValue::Name(relation.name.clone())),
            ("typnamespace", CatalogValue::Oid(relation.namespace)),
            ("typowner", CatalogValue::Oid(BOOTSTRAP_SUPERUSER_OID)),
            ("typlen", CatalogValue::Int16(-1)),
            ("typbyval", CatalogValue::Bool(false)),
            ("typtype", CatalogValue::Char(b'c')),
            ("typcategory", CatalogValue::Char(b'C')),
            ("typispreferred", CatalogValue::Bool(false)),
            ("typisdefined", CatalogValue::Bool(true)),
            ("typdelim", CatalogValue::Char(b',')),
            ("typrelid", CatalogValue::Oid(relation.oid)),
            ("typsubscript", CatalogValue::Oid(INVALID_OID)),
            ("typelem", CatalogValue::Oid(INVALID_OID)),
            ("typarray", CatalogValue::Oid(INVALID_OID)),
            (
                "typinput",
                CatalogValue::Oid(
                    record_type
                        .as_ref()
                        .map(|record| record.typinput)
                        .unwrap_or(INVALID_OID),
                ),
            ),
            (
                "typoutput",
                CatalogValue::Oid(
                    record_type
                        .as_ref()
                        .map(|record| record.typoutput)
                        .unwrap_or(INVALID_OID),
                ),
            ),
            (
                "typreceive",
                CatalogValue::Oid(
                    record_type
                        .as_ref()
                        .map(|record| record.typreceive)
                        .unwrap_or(INVALID_OID),
                ),
            ),
            (
                "typsend",
                CatalogValue::Oid(
                    record_type
                        .as_ref()
                        .map(|record| record.typsend)
                        .unwrap_or(INVALID_OID),
                ),
            ),
            ("typmodin", CatalogValue::Oid(INVALID_OID)),
            ("typmodout", CatalogValue::Oid(INVALID_OID)),
            ("typanalyze", CatalogValue::Oid(INVALID_OID)),
            ("typalign", CatalogValue::Char(b'd')),
            ("typstorage", CatalogValue::Char(b'x')),
            ("typnotnull", CatalogValue::Bool(false)),
            ("typbasetype", CatalogValue::Oid(INVALID_OID)),
            ("typtypmod", CatalogValue::Int32(-1)),
            ("typndims", CatalogValue::Int32(0)),
            ("typcollation", CatalogValue::Oid(INVALID_OID)),
        ],
    )
}

const SYSTEM_ATTRIBUTE_COLUMNS: &[(&str, i16, Oid)] = &[
    ("ctid", -1, TID_OID),
    ("xmin", -2, XID_OID),
    ("cmin", -3, CID_OID),
    ("xmax", -4, XID_OID),
    ("cmax", -5, CID_OID),
    ("tableoid", -6, OID_OID),
];

fn pg_attribute_overlay_row_for_column(
    relation_oid: Oid,
    attnum: i16,
    name: &str,
    type_oid: Oid,
    type_mod: i32,
    is_not_null: bool,
) -> Option<CatalogRow> {
    let type_record = lookup_type(type_oid)?;
    Some(catalog_row_from_named_values(
        PG_ATTRIBUTE_RELATION_OID,
        ((relation_oid.0 as u64) << 16) | u64::from(attnum as u16),
        vec![
            ("attrelid", CatalogValue::Oid(relation_oid)),
            ("attname", CatalogValue::Name(name.to_owned())),
            ("atttypid", CatalogValue::Oid(type_oid)),
            ("attlen", CatalogValue::Int16(type_record.typlen)),
            ("attnum", CatalogValue::Int16(attnum)),
            ("atttypmod", CatalogValue::Int32(type_mod)),
            ("attndims", CatalogValue::Int16(0)),
            ("attbyval", CatalogValue::Bool(type_record.typbyval)),
            ("attalign", CatalogValue::Char(type_record.typalign)),
            ("attstorage", CatalogValue::Char(type_record.typstorage)),
            ("attcompression", CatalogValue::Char(0)),
            ("attnotnull", CatalogValue::Bool(is_not_null)),
            ("atthasdef", CatalogValue::Bool(false)),
            ("atthasmissing", CatalogValue::Bool(false)),
            ("attidentity", CatalogValue::Char(0)),
            ("attgenerated", CatalogValue::Char(0)),
            ("attisdropped", CatalogValue::Bool(false)),
            ("attislocal", CatalogValue::Bool(true)),
            ("attinhcount", CatalogValue::Int16(0)),
            ("attcollation", CatalogValue::Oid(type_record.typcollation)),
            ("attstattarget", CatalogValue::Int16(-1)),
        ],
    ))
}

fn pg_attribute_overlay_row(
    relation_oid: Oid,
    attnum: i16,
    column: &ColumnRecord,
) -> Option<CatalogRow> {
    pg_attribute_overlay_row_for_column(
        relation_oid,
        attnum,
        &column.name,
        column.type_oid,
        column.type_mod,
        column.is_not_null,
    )
}

fn insert_system_attribute_overlay_rows(state: &mut CatalogState, relation_oid: Oid) {
    for (name, attnum, type_oid) in SYSTEM_ATTRIBUTE_COLUMNS {
        if let Some(row) =
            pg_attribute_overlay_row_for_column(relation_oid, *attnum, name, *type_oid, -1, true)
        {
            state.overlay.insert(row);
        }
    }
}

fn insert_relation_attribute_overlay_rows(state: &mut CatalogState, relation: &RelationRecord) {
    insert_system_attribute_overlay_rows(state, relation.oid);
    for (index, column) in relation.columns.iter().enumerate() {
        let Some(attnum) = i16::try_from(index + 1).ok() else {
            continue;
        };
        if let Some(row) = pg_attribute_overlay_row(relation.oid, attnum, column) {
            state.overlay.insert(row);
        }
    }
}

fn insert_index_attribute_overlay_rows(
    state: &mut CatalogState,
    relation: &RelationRecord,
    index_oid: Oid,
) {
    insert_system_attribute_overlay_rows(state, index_oid);
    for (index, primary_key_column) in relation.primary_key.iter().enumerate() {
        let Some(attnum) = i16::try_from(index + 1).ok() else {
            continue;
        };
        let Some(column) = relation
            .columns
            .iter()
            .find(|column| &column.name == primary_key_column)
        else {
            continue;
        };
        if let Some(row) = pg_attribute_overlay_row(index_oid, attnum, column) {
            state.overlay.insert(row);
        }
    }
}

fn delete_attribute_overlay_rows(state: &mut CatalogState, relation_oid: Oid, column_count: usize) {
    for (_name, attnum, _type_oid) in SYSTEM_ATTRIBUTE_COLUMNS {
        state.overlay.delete(
            PG_ATTRIBUTE_RELATION_OID,
            ((relation_oid.0 as u64) << 16) | u64::from(*attnum as u16),
        );
    }
    for index in 0..column_count {
        let Some(attnum) = i16::try_from(index + 1).ok() else {
            continue;
        };
        state.overlay.delete(
            PG_ATTRIBUTE_RELATION_OID,
            ((relation_oid.0 as u64) << 16) | u64::from(attnum as u16),
        );
    }
}

fn insert_relation_overlay_rows(state: &mut CatalogState, relation: &RelationRecord) {
    state
        .overlay
        .insert(pg_class_overlay_row(PgClassOverlayInput {
            oid: relation.oid,
            namespace: relation.namespace,
            name: &relation.name,
            reltype: relation.type_oid,
            relkind: b'r',
            relam: HEAP_TABLE_AM_OID,
            column_count: relation.columns.len(),
            has_index: !relation.primary_key.is_empty(),
        }));
    state.overlay.insert(pg_type_overlay_row(relation));
    insert_relation_attribute_overlay_rows(state, relation);
}

fn delete_relation_overlay_rows(state: &mut CatalogState, relation: &RelationRecord) {
    state
        .overlay
        .delete(PG_CLASS_RELATION_OID, relation.oid.0 as u64);
    state
        .overlay
        .delete(PG_TYPE_RELATION_OID, relation.type_oid.0 as u64);
    delete_attribute_overlay_rows(state, relation.oid, relation.columns.len());
    if let Some(index_oid) = relation_primary_key_index_oid(relation) {
        state
            .overlay
            .delete(PG_CLASS_RELATION_OID, index_oid.0 as u64);
        state
            .overlay
            .delete(PG_INDEX_RELATION_OID, index_oid.0 as u64);
        delete_attribute_overlay_rows(state, index_oid, relation.primary_key.len());
        if let Some(constraint_oid) = relation.primary_key_constraint_oid {
            state
                .overlay
                .delete(PG_CONSTRAINT_RELATION_OID, constraint_oid.0 as u64);
        }
    }
}

fn catalog_value_oid(value: &CatalogValue) -> Option<Oid> {
    match value {
        CatalogValue::Oid(oid) => Some(*oid),
        CatalogValue::Int32(value) => u32::try_from(*value).ok().map(Oid),
        CatalogValue::Int16(value) => u32::try_from(*value).ok().map(Oid),
        CatalogValue::Raw(value) => value.parse::<u32>().ok().map(Oid),
        _ => None,
    }
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

pub fn btree_opclass_for_type(type_oid: Oid) -> Option<Oid> {
    static_btree_opclass_for_type(type_oid)
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

fn pg_index_overlay_row(
    relation: &RelationRecord,
    index_oid: Oid,
    key_attnums: Vec<i16>,
) -> Result<CatalogRow, CatalogError> {
    let mut collations = Vec::with_capacity(key_attnums.len());
    let mut opclasses = Vec::with_capacity(key_attnums.len());
    for attnum in &key_attnums {
        let column = relation
            .columns
            .get((*attnum as usize).saturating_sub(1))
            .ok_or_else(|| CatalogError::new("42703", "primary key column does not exist"))?;
        let type_record = lookup_type(column.type_oid).ok_or_else(|| {
            CatalogError::new(
                "42704",
                format!("type OID {} does not exist", column.type_oid.0),
            )
        })?;
        let opclass = static_btree_opclass_for_type(column.type_oid).ok_or_else(|| {
            CatalogError::new(
                "0A000",
                format!(
                    "fastpg does not have a btree opclass for type OID {}",
                    column.type_oid.0
                ),
            )
        })?;
        collations.push(type_record.typcollation);
        opclasses.push(opclass);
    }
    let key_count = key_attnums.len().min(i16::MAX as usize) as i16;
    Ok(catalog_row_from_named_values(
        PG_INDEX_RELATION_OID,
        index_oid.0 as u64,
        vec![
            ("indexrelid", CatalogValue::Oid(index_oid)),
            ("indrelid", CatalogValue::Oid(relation.oid)),
            ("indnatts", CatalogValue::Int16(key_count)),
            ("indnkeyatts", CatalogValue::Int16(key_count)),
            ("indisunique", CatalogValue::Bool(true)),
            ("indnullsnotdistinct", CatalogValue::Bool(false)),
            ("indisprimary", CatalogValue::Bool(true)),
            ("indisexclusion", CatalogValue::Bool(false)),
            ("indimmediate", CatalogValue::Bool(true)),
            ("indisclustered", CatalogValue::Bool(false)),
            ("indisvalid", CatalogValue::Bool(true)),
            ("indcheckxmin", CatalogValue::Bool(false)),
            ("indisready", CatalogValue::Bool(true)),
            ("indislive", CatalogValue::Bool(true)),
            ("indisreplident", CatalogValue::Bool(false)),
            ("indkey", CatalogValue::Int2Vector(key_attnums)),
            ("indcollation", CatalogValue::OidVector(collations)),
            ("indclass", CatalogValue::OidVector(opclasses)),
            (
                "indoption",
                CatalogValue::Int2Vector(vec![0; key_count as usize]),
            ),
        ],
    ))
}

fn pg_constraint_overlay_row(
    relation: &RelationRecord,
    constraint_oid: Oid,
    index_oid: Oid,
    key_attnums: Vec<i16>,
) -> CatalogRow {
    catalog_row_from_named_values(
        PG_CONSTRAINT_RELATION_OID,
        constraint_oid.0 as u64,
        vec![
            ("oid", CatalogValue::Oid(constraint_oid)),
            (
                "conname",
                CatalogValue::Name(primary_key_index_name(relation)),
            ),
            ("connamespace", CatalogValue::Oid(relation.namespace)),
            ("contype", CatalogValue::Char(b'p')),
            ("condeferrable", CatalogValue::Bool(false)),
            ("condeferred", CatalogValue::Bool(false)),
            ("conenforced", CatalogValue::Bool(true)),
            ("convalidated", CatalogValue::Bool(true)),
            ("conrelid", CatalogValue::Oid(relation.oid)),
            ("contypid", CatalogValue::Oid(INVALID_OID)),
            ("conindid", CatalogValue::Oid(index_oid)),
            ("conparentid", CatalogValue::Oid(INVALID_OID)),
            ("confrelid", CatalogValue::Oid(INVALID_OID)),
            ("confupdtype", CatalogValue::Char(b' ')),
            ("confdeltype", CatalogValue::Char(b' ')),
            ("confmatchtype", CatalogValue::Char(b' ')),
            ("conislocal", CatalogValue::Bool(true)),
            ("coninhcount", CatalogValue::Int16(0)),
            ("connoinherit", CatalogValue::Bool(true)),
            ("conperiod", CatalogValue::Bool(false)),
            ("conkey", CatalogValue::Int2Vector(key_attnums)),
        ],
    )
}

fn insert_primary_key_overlay_rows(
    state: &mut CatalogState,
    relation: &RelationRecord,
    key_attnums: Vec<i16>,
) -> Result<(), CatalogError> {
    let Some(index_oid) = relation_primary_key_index_oid(relation) else {
        return Ok(());
    };
    state
        .overlay
        .insert(pg_class_overlay_row(PgClassOverlayInput {
            oid: relation.oid,
            namespace: relation.namespace,
            name: &relation.name,
            reltype: relation.type_oid,
            relkind: b'r',
            relam: HEAP_TABLE_AM_OID,
            column_count: relation.columns.len(),
            has_index: true,
        }));
    state
        .overlay
        .insert(pg_class_overlay_row(PgClassOverlayInput {
            oid: index_oid,
            namespace: relation.namespace,
            name: &primary_key_index_name(relation),
            reltype: INVALID_OID,
            relkind: b'i',
            relam: BTREE_INDEX_AM_OID,
            column_count: key_attnums.len(),
            has_index: false,
        }));
    state.overlay.insert(pg_index_overlay_row(
        relation,
        index_oid,
        key_attnums.clone(),
    )?);
    insert_index_attribute_overlay_rows(state, relation, index_oid);
    if let Some(constraint_oid) = relation.primary_key_constraint_oid {
        state.overlay.insert(pg_constraint_overlay_row(
            relation,
            constraint_oid,
            index_oid,
            key_attnums,
        ));
    }
    Ok(())
}

fn next_relation_oid(state: &mut CatalogState) -> Result<Oid, CatalogError> {
    let oid = state.next_relation_oid;
    if oid == 0 {
        return Err(CatalogError::new(
            "54000",
            "fastpg relation OID space is exhausted",
        ));
    }
    state.next_relation_oid = state
        .next_relation_oid
        .checked_add(1)
        .ok_or_else(|| CatalogError::new("54000", "fastpg relation OID space is exhausted"))?;
    state.next_object_oid = state.next_object_oid.max(state.next_relation_oid);
    Ok(Oid(oid))
}

fn next_object_oid(state: &mut CatalogState) -> Result<Oid, CatalogError> {
    let oid = state.next_object_oid;
    if oid == 0 {
        return Err(CatalogError::new(
            "54000",
            "fastpg object OID space is exhausted",
        ));
    }
    state.next_object_oid = state
        .next_object_oid
        .checked_add(1)
        .ok_or_else(|| CatalogError::new("54000", "fastpg object OID space is exhausted"))?;
    Ok(Oid(oid))
}

fn bump_generation(state: &mut CatalogState) {
    state.generation = state.generation.saturating_add(1).max(1);
}

pub fn current_generation() -> u64 {
    with_catalog_read(|state| state.generation)
}

pub fn create_relation(
    name: &str,
    columns: Vec<ColumnRecord>,
    if_not_exists: bool,
) -> Result<Option<RelationRecord>, CatalogError> {
    let name = normalize_identifier(name);
    if name.is_empty() {
        return Err(CatalogError::new("42602", "relation name cannot be empty"));
    }
    if columns.iter().any(|column| column.name.is_empty()) {
        return Err(CatalogError::new("42602", "column name cannot be empty"));
    }

    with_catalog(|state| {
        if state.relations_by_name.contains_key(&name) {
            if if_not_exists {
                return Ok(None);
            }
            return Err(CatalogError::new(
                "42P07",
                format!("relation \"{name}\" already exists"),
            ));
        }

        let relation_oid = next_relation_oid(state)?;
        let type_oid = next_object_oid(state)?;
        let relation = RelationRecord {
            oid: relation_oid,
            type_oid,
            namespace: PUBLIC_NAMESPACE_OID,
            name: name.clone(),
            columns,
            primary_key: Vec::new(),
            primary_key_constraint_oid: None,
        };
        insert_relation_overlay_rows(state, &relation);
        state
            .relation_names_by_oid
            .insert(relation.oid.0, name.clone());
        state.relations_by_name.insert(name, relation.clone());
        bump_generation(state);
        Ok(Some(relation))
    })
}

pub fn drop_relation(name: &str, missing_ok: bool) -> Result<Option<RelationRecord>, CatalogError> {
    let name = normalize_identifier(name);
    with_catalog(|state| match state.relations_by_name.remove(&name) {
        Some(relation) => {
            delete_relation_overlay_rows(state, &relation);
            state.relation_names_by_oid.remove(&relation.oid.0);
            bump_generation(state);
            Ok(Some(relation))
        }
        None if missing_ok => Ok(None),
        None => Err(CatalogError::new(
            "42P01",
            format!("relation \"{name}\" does not exist"),
        )),
    })
}

pub fn truncate_relation(name: &str) -> Result<RelationRecord, CatalogError> {
    let name = normalize_identifier(name);
    with_catalog(|state| {
        let relation = state.relations_by_name.get(&name).cloned().ok_or_else(|| {
            CatalogError::new("42P01", format!("relation \"{name}\" does not exist"))
        })?;
        bump_generation(state);
        Ok(relation)
    })
}

pub fn relation_by_name(name: &str) -> Option<RelationRecord> {
    let name = normalize_identifier(name);
    with_catalog_read(|state| state.relations_by_name.get(&name).cloned())
}

pub fn relations() -> Vec<RelationRecord> {
    with_catalog_read(|state| state.relations_by_name.values().cloned().collect())
}

pub fn relation_by_oid(oid: Oid) -> Option<RelationRecord> {
    with_catalog_read(|state| {
        state
            .relation_names_by_oid
            .get(&oid.0)
            .and_then(|name| state.relations_by_name.get(name))
            .cloned()
    })
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

pub fn add_primary_key(name: &str, columns: Vec<String>) -> Result<(), CatalogError> {
    let name = normalize_identifier(name);
    let columns = columns
        .into_iter()
        .map(|column| normalize_identifier(&column))
        .collect::<Vec<_>>();
    if columns.is_empty() {
        return Err(CatalogError::new(
            "42602",
            "primary key must include at least one column",
        ));
    }

    with_catalog(|state| {
        let key_attnums = {
            let relation = state.relations_by_name.get(&name).ok_or_else(|| {
                CatalogError::new("42P01", format!("relation \"{name}\" does not exist"))
            })?;
            relation_column_attnums(relation, &columns)?
        };
        let constraint_oid = state
            .relations_by_name
            .get(&name)
            .and_then(|relation| relation.primary_key_constraint_oid)
            .map(Ok)
            .unwrap_or_else(|| next_object_oid(state))?;
        let relation_snapshot = {
            let relation = state.relations_by_name.get_mut(&name).ok_or_else(|| {
                CatalogError::new("42P01", format!("relation \"{name}\" does not exist"))
            })?;
            for column in &mut relation.columns {
                if columns
                    .iter()
                    .any(|primary_key_column| primary_key_column == &column.name)
                {
                    column.is_not_null = true;
                }
            }
            relation.primary_key = columns;
            relation.primary_key_constraint_oid = Some(constraint_oid);
            relation.clone()
        };
        insert_relation_attribute_overlay_rows(state, &relation_snapshot);
        insert_primary_key_overlay_rows(state, &relation_snapshot, key_attnums)?;
        bump_generation(state);
        Ok(())
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateTypeColumn {
    pub name: String,
    pub type_oid: Oid,
    pub type_mod: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CreateTypeKind {
    Shell,
    Base,
    Composite { columns: Vec<CreateTypeColumn> },
    Enum { labels: Vec<String> },
    Range { subtype: Oid, collation: Oid },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateFunctionSpec {
    pub name: String,
    pub return_type: Oid,
    pub arg_types: Vec<Oid>,
    pub language: Oid,
    pub strict: bool,
    pub leakproof: bool,
    pub returns_set: bool,
    pub volatility: u8,
    pub parallel: u8,
    pub source: String,
}

fn pg_type_row_for_created_type(input: CreatedTypeInput<'_>) -> CatalogRow {
    catalog_row_from_named_values(
        PG_TYPE_RELATION_OID,
        input.oid.0 as u64,
        vec![
            ("oid", CatalogValue::Oid(input.oid)),
            ("typname", CatalogValue::Name(input.name.to_owned())),
            ("typnamespace", CatalogValue::Oid(input.namespace)),
            ("typowner", CatalogValue::Oid(BOOTSTRAP_SUPERUSER_OID)),
            ("typlen", CatalogValue::Int16(input.typlen)),
            ("typbyval", CatalogValue::Bool(input.typbyval)),
            ("typtype", CatalogValue::Char(input.typtype)),
            ("typcategory", CatalogValue::Char(input.typcategory)),
            ("typispreferred", CatalogValue::Bool(false)),
            ("typisdefined", CatalogValue::Bool(input.typisdefined)),
            ("typdelim", CatalogValue::Char(b',')),
            ("typrelid", CatalogValue::Oid(input.typrelid)),
            ("typsubscript", CatalogValue::Oid(input.typsubscript)),
            ("typelem", CatalogValue::Oid(input.typelem)),
            ("typarray", CatalogValue::Oid(input.typarray)),
            ("typinput", CatalogValue::Oid(input.typinput)),
            ("typoutput", CatalogValue::Oid(input.typoutput)),
            ("typreceive", CatalogValue::Oid(input.typreceive)),
            ("typsend", CatalogValue::Oid(input.typsend)),
            ("typmodin", CatalogValue::Oid(INVALID_OID)),
            ("typmodout", CatalogValue::Oid(INVALID_OID)),
            ("typanalyze", CatalogValue::Oid(INVALID_OID)),
            ("typalign", CatalogValue::Char(input.typalign)),
            ("typstorage", CatalogValue::Char(input.typstorage)),
            ("typnotnull", CatalogValue::Bool(false)),
            ("typbasetype", CatalogValue::Oid(input.typbasetype)),
            ("typtypmod", CatalogValue::Int32(-1)),
            ("typndims", CatalogValue::Int32(0)),
            ("typcollation", CatalogValue::Oid(input.typcollation)),
        ],
    )
}

struct CreatedTypeInput<'a> {
    oid: Oid,
    namespace: Oid,
    name: &'a str,
    typlen: i16,
    typbyval: bool,
    typtype: u8,
    typcategory: u8,
    typisdefined: bool,
    typrelid: Oid,
    typelem: Oid,
    typarray: Oid,
    typbasetype: Oid,
    typcollation: Oid,
    typsubscript: Oid,
    typinput: Oid,
    typoutput: Oid,
    typreceive: Oid,
    typsend: Oid,
    typalign: u8,
    typstorage: u8,
}

fn pg_array_type_row(type_oid: Oid, array_oid: Oid, namespace: Oid, name: &str) -> CatalogRow {
    pg_type_row_for_created_type(CreatedTypeInput {
        oid: array_oid,
        namespace,
        name,
        typlen: -1,
        typbyval: false,
        typtype: b'b',
        typcategory: b'A',
        typisdefined: true,
        typrelid: INVALID_OID,
        typelem: type_oid,
        typarray: INVALID_OID,
        typbasetype: INVALID_OID,
        typcollation: INVALID_OID,
        typsubscript: ARRAY_SUBSCRIPT_HANDLER_PROC_OID,
        typinput: ARRAY_IN_PROC_OID,
        typoutput: ARRAY_OUT_PROC_OID,
        typreceive: ARRAY_RECV_PROC_OID,
        typsend: ARRAY_SEND_PROC_OID,
        typalign: b'd',
        typstorage: b'x',
    })
}

fn pg_enum_row(enum_oid: Oid, type_oid: Oid, label: &str, sort_order: f32) -> CatalogRow {
    let relation_oid = catalog_relation_oid("pg_enum").expect("generated pg_enum catalog");
    catalog_row_from_named_values(
        relation_oid,
        enum_oid.0 as u64,
        vec![
            ("oid", CatalogValue::Oid(enum_oid)),
            ("enumtypid", CatalogValue::Oid(type_oid)),
            ("enumsortorder", CatalogValue::Float32(sort_order)),
            ("enumlabel", CatalogValue::Name(label.to_owned())),
        ],
    )
}

fn pg_range_row(range_oid: Oid, subtype: Oid, collation: Oid) -> CatalogRow {
    let relation_oid = catalog_relation_oid("pg_range").expect("generated pg_range catalog");
    let subtype_opclass = static_btree_opclass_for_type(subtype).unwrap_or(INVALID_OID);
    catalog_row_from_named_values(
        relation_oid,
        range_oid.0 as u64,
        vec![
            ("rngtypid", CatalogValue::Oid(range_oid)),
            ("rngsubtype", CatalogValue::Oid(subtype)),
            ("rngmultitypid", CatalogValue::Oid(INVALID_OID)),
            ("rngcollation", CatalogValue::Oid(collation)),
            ("rngsubopc", CatalogValue::Oid(subtype_opclass)),
            ("rngcanonical", CatalogValue::Oid(INVALID_OID)),
            ("rngsubdiff", CatalogValue::Oid(INVALID_OID)),
        ],
    )
}

fn insert_array_type_row(
    state: &mut CatalogState,
    type_oid: Oid,
    namespace: Oid,
    name: &str,
) -> Result<Oid, CatalogError> {
    let array_name = format!("_{name}");
    if overlay_type_by_name(state, &array_name, namespace).is_some() {
        return Ok(INVALID_OID);
    }
    let array_oid = next_object_oid(state)?;
    state.overlay.insert(pg_array_type_row(
        type_oid,
        array_oid,
        namespace,
        &array_name,
    ));
    Ok(array_oid)
}

pub fn create_type(name: &str, kind: CreateTypeKind) -> Result<Oid, CatalogError> {
    let name = normalize_identifier(name);
    if name.is_empty() {
        return Err(CatalogError::new("42602", "type name cannot be empty"));
    }

    with_catalog(|state| {
        let namespace = PUBLIC_NAMESPACE_OID;
        let existing = overlay_type_by_name(state, &name, namespace);
        if let Some(existing) = existing.as_ref().filter(|record| record.typisdefined) {
            return Ok(existing.oid);
        }

        let type_oid = existing
            .as_ref()
            .map(|record| record.oid)
            .map(Ok)
            .unwrap_or_else(|| next_object_oid(state))?;
        let array_oid = match kind {
            CreateTypeKind::Shell => INVALID_OID,
            _ => insert_array_type_row(state, type_oid, namespace, &name)?,
        };

        match kind {
            CreateTypeKind::Shell => {
                state
                    .overlay
                    .insert(pg_type_row_for_created_type(CreatedTypeInput {
                        oid: type_oid,
                        namespace,
                        name: &name,
                        typlen: -1,
                        typbyval: false,
                        typtype: b'p',
                        typcategory: b'P',
                        typisdefined: false,
                        typrelid: INVALID_OID,
                        typelem: INVALID_OID,
                        typarray: INVALID_OID,
                        typbasetype: INVALID_OID,
                        typcollation: INVALID_OID,
                        typsubscript: INVALID_OID,
                        typinput: INVALID_OID,
                        typoutput: INVALID_OID,
                        typreceive: INVALID_OID,
                        typsend: INVALID_OID,
                        typalign: b'i',
                        typstorage: b'x',
                    }));
            }
            CreateTypeKind::Base => {
                state
                    .overlay
                    .insert(pg_type_row_for_created_type(CreatedTypeInput {
                        oid: type_oid,
                        namespace,
                        name: &name,
                        typlen: -1,
                        typbyval: false,
                        typtype: b'b',
                        typcategory: b'U',
                        typisdefined: true,
                        typrelid: INVALID_OID,
                        typelem: INVALID_OID,
                        typarray: array_oid,
                        typbasetype: INVALID_OID,
                        typcollation: INVALID_OID,
                        typsubscript: INVALID_OID,
                        typinput: INVALID_OID,
                        typoutput: INVALID_OID,
                        typreceive: INVALID_OID,
                        typsend: INVALID_OID,
                        typalign: b'i',
                        typstorage: b'x',
                    }));
            }
            CreateTypeKind::Enum { labels } => {
                state
                    .overlay
                    .insert(pg_type_row_for_created_type(CreatedTypeInput {
                        oid: type_oid,
                        namespace,
                        name: &name,
                        typlen: 4,
                        typbyval: true,
                        typtype: b'e',
                        typcategory: b'E',
                        typisdefined: true,
                        typrelid: INVALID_OID,
                        typelem: INVALID_OID,
                        typarray: array_oid,
                        typbasetype: INVALID_OID,
                        typcollation: INVALID_OID,
                        typsubscript: INVALID_OID,
                        typinput: INVALID_OID,
                        typoutput: INVALID_OID,
                        typreceive: INVALID_OID,
                        typsend: INVALID_OID,
                        typalign: b'i',
                        typstorage: b'p',
                    }));
                for (index, label) in labels.iter().enumerate() {
                    let enum_oid = next_object_oid(state)?;
                    state.overlay.insert(pg_enum_row(
                        enum_oid,
                        type_oid,
                        label,
                        (index + 1) as f32,
                    ));
                }
            }
            CreateTypeKind::Range { subtype, collation } => {
                if !type_oid_exists_in_state(state, subtype) {
                    return Err(CatalogError::new(
                        "42704",
                        format!("range subtype OID {} does not exist", subtype.0),
                    ));
                }
                state
                    .overlay
                    .insert(pg_type_row_for_created_type(CreatedTypeInput {
                        oid: type_oid,
                        namespace,
                        name: &name,
                        typlen: -1,
                        typbyval: false,
                        typtype: b'r',
                        typcategory: b'R',
                        typisdefined: true,
                        typrelid: INVALID_OID,
                        typelem: INVALID_OID,
                        typarray: array_oid,
                        typbasetype: INVALID_OID,
                        typcollation: collation,
                        typsubscript: INVALID_OID,
                        typinput: INVALID_OID,
                        typoutput: INVALID_OID,
                        typreceive: INVALID_OID,
                        typsend: INVALID_OID,
                        typalign: b'd',
                        typstorage: b'x',
                    }));
                state
                    .overlay
                    .insert(pg_range_row(type_oid, subtype, collation));
            }
            CreateTypeKind::Composite { columns } => {
                for column in &columns {
                    if !type_oid_exists_in_state(state, column.type_oid) {
                        return Err(CatalogError::new(
                            "42704",
                            format!("type OID {} does not exist", column.type_oid.0),
                        ));
                    }
                }
                let relation_oid = next_relation_oid(state)?;
                let relation = RelationRecord {
                    oid: relation_oid,
                    type_oid,
                    namespace,
                    name: name.clone(),
                    columns: columns
                        .into_iter()
                        .map(|column| ColumnRecord {
                            name: normalize_identifier(&column.name),
                            type_oid: column.type_oid,
                            type_mod: column.type_mod,
                            is_not_null: false,
                        })
                        .collect(),
                    primary_key: Vec::new(),
                    primary_key_constraint_oid: None,
                };
                state
                    .overlay
                    .insert(pg_class_overlay_row(PgClassOverlayInput {
                        oid: relation.oid,
                        namespace: relation.namespace,
                        name: &relation.name,
                        reltype: relation.type_oid,
                        relkind: b'c',
                        relam: HEAP_TABLE_AM_OID,
                        column_count: relation.columns.len(),
                        has_index: false,
                    }));
                state
                    .overlay
                    .insert(pg_type_row_for_created_type(CreatedTypeInput {
                        oid: type_oid,
                        namespace,
                        name: &name,
                        typlen: -1,
                        typbyval: false,
                        typtype: b'c',
                        typcategory: b'C',
                        typisdefined: true,
                        typrelid: relation_oid,
                        typelem: INVALID_OID,
                        typarray: array_oid,
                        typbasetype: INVALID_OID,
                        typcollation: INVALID_OID,
                        typsubscript: INVALID_OID,
                        typinput: INVALID_OID,
                        typoutput: INVALID_OID,
                        typreceive: INVALID_OID,
                        typsend: INVALID_OID,
                        typalign: b'd',
                        typstorage: b'x',
                    }));
                insert_relation_attribute_overlay_rows(state, &relation);
                state
                    .relation_names_by_oid
                    .insert(relation.oid.0, name.clone());
                state.relations_by_name.insert(name.clone(), relation);
            }
        }
        bump_generation(state);
        Ok(type_oid)
    })
}

fn pg_proc_row(oid: Oid, spec: &CreateFunctionSpec) -> CatalogRow {
    catalog_row_from_named_values(
        PG_PROC_RELATION_OID,
        oid.0 as u64,
        vec![
            ("oid", CatalogValue::Oid(oid)),
            (
                "proname",
                CatalogValue::Name(normalize_identifier(&spec.name)),
            ),
            ("pronamespace", CatalogValue::Oid(PUBLIC_NAMESPACE_OID)),
            ("proowner", CatalogValue::Oid(BOOTSTRAP_SUPERUSER_OID)),
            ("prolang", CatalogValue::Oid(spec.language)),
            ("procost", CatalogValue::Float32(1.0)),
            ("prorows", CatalogValue::Float32(0.0)),
            ("provariadic", CatalogValue::Oid(INVALID_OID)),
            ("prosupport", CatalogValue::Oid(INVALID_OID)),
            ("prokind", CatalogValue::Char(b'f')),
            ("prosecdef", CatalogValue::Bool(false)),
            ("proleakproof", CatalogValue::Bool(spec.leakproof)),
            ("proisstrict", CatalogValue::Bool(spec.strict)),
            ("proretset", CatalogValue::Bool(spec.returns_set)),
            ("provolatile", CatalogValue::Char(spec.volatility)),
            ("proparallel", CatalogValue::Char(spec.parallel)),
            (
                "pronargs",
                CatalogValue::Int16(spec.arg_types.len().min(i16::MAX as usize) as i16),
            ),
            ("pronargdefaults", CatalogValue::Int16(0)),
            ("prorettype", CatalogValue::Oid(spec.return_type)),
            (
                "proargtypes",
                CatalogValue::OidVector(spec.arg_types.clone()),
            ),
            ("prosrc", CatalogValue::Text(spec.source.clone())),
        ],
    )
}

pub fn create_function(spec: CreateFunctionSpec) -> Result<Oid, CatalogError> {
    let name = normalize_identifier(&spec.name);
    if name.is_empty() {
        return Err(CatalogError::new("42602", "function name cannot be empty"));
    }
    if lookup_type(spec.return_type).is_none() {
        return Err(CatalogError::new(
            "42704",
            format!("return type OID {} does not exist", spec.return_type.0),
        ));
    }
    for arg_type in &spec.arg_types {
        if lookup_type(*arg_type).is_none() {
            return Err(CatalogError::new(
                "42704",
                format!("argument type OID {} does not exist", arg_type.0),
            ));
        }
    }

    with_catalog(|state| {
        let oid = next_object_oid(state)?;
        state.overlay.insert(pg_proc_row(oid, &spec));
        bump_generation(state);
        Ok(oid)
    })
}

fn access_method_oid_by_name(name: &str) -> Option<Oid> {
    let table = static_catalog_by_name("pg_am")?;
    let name = normalize_identifier(name);
    catalog_rows(table.oid).into_iter().find_map(|row| {
        let amname = catalog_row_value(table, &row, "amname").and_then(catalog_value_string)?;
        let oid = catalog_row_value(table, &row, "oid").and_then(catalog_value_oid)?;
        (amname == name).then_some(oid)
    })
}

pub fn create_opclass(
    name: &str,
    method_name: &str,
    input_type: Oid,
    is_default: bool,
) -> Result<Oid, CatalogError> {
    let name = normalize_identifier(name);
    if lookup_type(input_type).is_none() {
        return Err(CatalogError::new(
            "42704",
            format!(
                "operator class input type OID {} does not exist",
                input_type.0
            ),
        ));
    }
    let method_oid = access_method_oid_by_name(method_name).ok_or_else(|| {
        CatalogError::new(
            "42704",
            format!(
                "access method \"{}\" does not exist",
                normalize_identifier(method_name)
            ),
        )
    })?;
    with_catalog(|state| {
        let opfamily_oid = next_object_oid(state)?;
        let opclass_oid = next_object_oid(state)?;
        let opfamily_relation_oid =
            catalog_relation_oid("pg_opfamily").expect("generated pg_opfamily catalog");
        let opclass_relation_oid =
            catalog_relation_oid("pg_opclass").expect("generated pg_opclass catalog");
        state.overlay.insert(catalog_row_from_named_values(
            opfamily_relation_oid,
            opfamily_oid.0 as u64,
            vec![
                ("oid", CatalogValue::Oid(opfamily_oid)),
                ("opfmethod", CatalogValue::Oid(method_oid)),
                ("opfname", CatalogValue::Name(name.clone())),
                ("opfnamespace", CatalogValue::Oid(PUBLIC_NAMESPACE_OID)),
                ("opfowner", CatalogValue::Oid(BOOTSTRAP_SUPERUSER_OID)),
            ],
        ));
        state.overlay.insert(catalog_row_from_named_values(
            opclass_relation_oid,
            opclass_oid.0 as u64,
            vec![
                ("oid", CatalogValue::Oid(opclass_oid)),
                ("opcmethod", CatalogValue::Oid(method_oid)),
                ("opcname", CatalogValue::Name(name)),
                ("opcnamespace", CatalogValue::Oid(PUBLIC_NAMESPACE_OID)),
                ("opcowner", CatalogValue::Oid(BOOTSTRAP_SUPERUSER_OID)),
                ("opcfamily", CatalogValue::Oid(opfamily_oid)),
                ("opcintype", CatalogValue::Oid(input_type)),
                ("opcdefault", CatalogValue::Bool(is_default)),
                ("opckeytype", CatalogValue::Oid(INVALID_OID)),
            ],
        ));
        bump_generation(state);
        Ok(opclass_oid)
    })
}

#[cfg(test)]
pub fn clear_for_tests() {
    with_catalog(|state| *state = CatalogState::default());
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

fn overlay_type_by_oid(state: &CatalogState, oid: Oid) -> Option<CatalogTypeRecord> {
    let table = static_catalog_by_relation_oid(PG_TYPE_RELATION_OID)?;
    state
        .overlay
        .rows
        .get(&PG_TYPE_RELATION_OID.0)?
        .get(&(oid.0 as u64))
        .and_then(|row| catalog_type_from_row(table, row))
}

fn type_oid_exists_in_state(state: &CatalogState, oid: Oid) -> bool {
    lookup_builtin_type(oid).is_some() || overlay_type_by_oid(state, oid).is_some()
}

fn overlay_type_by_name(
    state: &CatalogState,
    name: &str,
    namespace: Oid,
) -> Option<CatalogTypeRecord> {
    let table = static_catalog_by_relation_oid(PG_TYPE_RELATION_OID)?;
    state
        .overlay
        .rows
        .get(&PG_TYPE_RELATION_OID.0)?
        .values()
        .find_map(|row| {
            let record = catalog_type_from_row(table, row)?;
            (record.namespace == namespace && record.name == name).then_some(record)
        })
}

pub fn lookup_type(oid: Oid) -> Option<CatalogTypeRecord> {
    lookup_builtin_type(oid)
        .map(CatalogTypeRecord::from)
        .or_else(|| {
            with_catalog_read(|state| {
                state
                    .overlay
                    .tombstones
                    .get(&PG_TYPE_RELATION_OID.0)
                    .is_none_or(|tombstones| !tombstones.contains(&(oid.0 as u64)))
                    .then(|| overlay_type_by_oid(state, oid))
                    .flatten()
            })
        })
}

pub fn type_by_name(name: &str, namespace: Oid) -> Option<CatalogTypeRecord> {
    let canonical_name = canonical_catalog_type_name(name);
    if namespace == PG_CATALOG_NAMESPACE_OID
        && let Some(record) = generated_catalog::STATIC_TYPES
            .iter()
            .copied()
            .find(|record| record.name == canonical_name)
    {
        return Some(record.into());
    }
    with_catalog_read(|state| overlay_type_by_name(state, &canonical_name, namespace))
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
    fn classifies_pgbench_critical_virtual_catalogs() {
        let required = [
            ("pg_type", VirtualCatalogPolicy::Dynamic),
            ("pg_proc", VirtualCatalogPolicy::Static),
            ("pg_operator", VirtualCatalogPolicy::Static),
            ("pg_aggregate", VirtualCatalogPolicy::Static),
            ("pg_namespace", VirtualCatalogPolicy::Static),
            ("pg_cast", VirtualCatalogPolicy::Static),
            ("pg_class", VirtualCatalogPolicy::Dynamic),
            ("pg_attribute", VirtualCatalogPolicy::Dynamic),
            ("pg_index", VirtualCatalogPolicy::Dynamic),
            ("pg_constraint", VirtualCatalogPolicy::Dynamic),
            ("pg_statistic", VirtualCatalogPolicy::Empty),
            ("pg_statistic_ext", VirtualCatalogPolicy::Empty),
            ("pg_statistic_ext_data", VirtualCatalogPolicy::Empty),
            ("pg_authid", VirtualCatalogPolicy::Empty),
            ("pg_auth_members", VirtualCatalogPolicy::Empty),
            ("pg_parameter_acl", VirtualCatalogPolicy::Empty),
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
    fn creates_drops_and_truncates_relations() {
        clear_for_tests();
        let initial_generation = current_generation();
        let relation = create_relation(
            "PgBench_Accounts",
            vec![
                ColumnRecord::new("aid", INT4_OID, -1, true),
                ColumnRecord::new("filler", BPCHAR_OID, -1, false),
            ],
            false,
        )
        .unwrap()
        .expect("created");

        assert!(current_generation() > initial_generation);
        let after_create_generation = current_generation();
        assert!(
            create_relation("pgbench_accounts", Vec::new(), true)
                .unwrap()
                .is_none()
        );
        assert_eq!(current_generation(), after_create_generation);
        assert_eq!(relation.name, "pgbench_accounts");
        assert_eq!(relation.columns[0].name, "aid");
        assert_eq!(
            relation_by_name("pgbench_accounts").unwrap().oid,
            relation.oid
        );
        assert_eq!(
            relation_by_oid(relation.oid).unwrap().name,
            "pgbench_accounts"
        );
        assert_eq!(
            truncate_relation("pgbench_accounts").unwrap().oid,
            relation.oid
        );
        let after_truncate_generation = current_generation();
        assert!(after_truncate_generation > after_create_generation);
        add_primary_key("pgbench_accounts", vec!["aid".to_owned()]).unwrap();
        let after_primary_key_generation = current_generation();
        assert!(after_primary_key_generation > after_truncate_generation);
        assert_eq!(
            relation_by_name("pgbench_accounts").unwrap().primary_key,
            vec!["aid"]
        );
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
        assert_eq!(
            drop_relation("pgbench_accounts", false)
                .unwrap()
                .unwrap()
                .oid,
            relation.oid
        );
        assert!(current_generation() > after_primary_key_generation);
        assert!(relation_by_name("pgbench_accounts").is_none());
        assert!(!catalog_rows(PG_CLASS_RELATION_OID).iter().any(|row| {
            value_name(row_value("pg_class", row, "relname")) == Some("pgbench_accounts")
        }));
    }
}
