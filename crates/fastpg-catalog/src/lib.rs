#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
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

pub const DEFAULT_COLLATION_OID: Oid = Oid(100);
pub const C_COLLATION_OID: Oid = Oid(950);

const INVALID_OID: Oid = Oid(0);
pub const PG_CATALOG_NAMESPACE_OID: Oid = Oid(11);
pub const PUBLIC_NAMESPACE_OID: Oid = Oid(2200);
const PG_CLASS_RELATION_OID: Oid = Oid(1259);
const PG_ATTRIBUTE_RELATION_OID: Oid = Oid(1249);
const PG_TYPE_RELATION_OID: Oid = Oid(1247);
const PG_INDEX_RELATION_OID: Oid = Oid(2610);
const PG_CONSTRAINT_RELATION_OID: Oid = Oid(2606);
const BTREE_INDEX_AM_OID: Oid = Oid(403);

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
    pub relkind: u8,
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

#[derive(Clone, Debug, Default)]
struct CatalogOverlay {
    rows: BTreeMap<u32, BTreeMap<u64, Arc<CatalogRow>>>,
    tombstones: BTreeMap<u32, BTreeSet<u64>>,
}

impl CatalogOverlay {
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

    fn delete(&mut self, relation_oid: Oid, row_id: u64) {
        self.rows.entry(relation_oid.0).or_default().remove(&row_id);
        self.tombstones
            .entry(relation_oid.0)
            .or_default()
            .insert(row_id);
    }

    fn merge(&mut self, other: CatalogOverlay) {
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

#[derive(Debug)]
struct CatalogState {
    next_overlay_row_ids: BTreeMap<u32, u64>,
    overlay: CatalogOverlay,
    generation: u64,
}

impl Default for CatalogState {
    fn default() -> Self {
        Self {
            next_overlay_row_ids: BTreeMap::new(),
            overlay: CatalogOverlay::default(),
            generation: 1,
        }
    }
}

static CATALOG: OnceLock<RwLock<CatalogState>> = OnceLock::new();

#[derive(Debug, Default)]
pub struct CatalogSession {
    transaction_stack: Vec<CatalogOverlay>,
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

fn visible_session_overlays() -> Vec<CatalogOverlay> {
    let session = current_catalog_session();
    match session.lock() {
        Ok(session) => session.transaction_stack.clone(),
        Err(poisoned) => poisoned.into_inner().transaction_stack.clone(),
    }
}

fn ensure_catalog_transaction(session: &mut CatalogSession) {
    if session.transaction_stack.is_empty() {
        session.transaction_stack.push(CatalogOverlay::default());
    }
}

fn commit_catalog_overlay(overlay: CatalogOverlay) {
    if overlay.is_empty() {
        return;
    }
    with_catalog(|state| {
        state.overlay.merge(overlay);
        bump_generation(state);
    });
}

fn commit_top_catalog_overlay(session: &mut CatalogSession) {
    let Some(overlay) = session.transaction_stack.pop() else {
        return;
    };
    if let Some(parent) = session.transaction_stack.last_mut() {
        parent.merge(overlay);
    } else {
        commit_catalog_overlay(overlay);
    }
}

pub fn begin_explicit_transaction() {
    with_catalog_session(|session| {
        if !session.explicit_transaction {
            while !session.transaction_stack.is_empty() {
                commit_top_catalog_overlay(session);
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
            commit_top_catalog_overlay(session);
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
            commit_top_catalog_overlay(session);
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
        session.transaction_stack.push(CatalogOverlay::default());
    });
}

pub fn commit_subtransaction() {
    with_catalog_session(|session| {
        if session.transaction_stack.len() > 1 {
            commit_top_catalog_overlay(session);
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
    let session_overlays = visible_session_overlays();
    with_catalog_read(|state| {
        let mut rows = BTreeMap::<u64, CatalogRow>::new();
        for row in table.rows {
            rows.insert(row.row_id, static_row_to_catalog_row(table, row));
        }
        state.overlay.apply_to_rows(relation_oid, &mut rows);
        for overlay in &session_overlays {
            overlay.apply_to_rows(relation_oid, &mut rows);
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
            .insert(row);
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
            .delete(relation_oid, row_id);
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

fn bump_generation(state: &mut CatalogState) {
    state.generation = state.generation.saturating_add(1).max(1);
}

pub fn current_generation() -> u64 {
    with_catalog_read(|state| state.generation)
}

fn relation_pg_class_row_by_oid(oid: Oid) -> Option<CatalogRow> {
    let table = static_catalog_by_relation_oid(PG_CLASS_RELATION_OID)?;
    catalog_rows(table.oid).into_iter().find(|row| {
        catalog_row_value(table, row, "oid")
            .and_then(catalog_value_oid)
            .is_some_and(|row_oid| row_oid == oid)
    })
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
    let mut columns = catalog_rows(table.oid)
        .into_iter()
        .filter_map(|row| {
            let attrelid =
                catalog_row_value(table, &row, "attrelid").and_then(catalog_value_oid)?;
            if attrelid != relation_oid {
                return None;
            }
            let attnum = catalog_row_value(table, &row, "attnum").and_then(catalog_value_i16)?;
            if attnum <= 0 {
                return None;
            }
            let attisdropped = catalog_row_value(table, &row, "attisdropped")
                .and_then(catalog_value_bool)
                .unwrap_or(false);
            if attisdropped {
                return None;
            }
            let name = catalog_row_value(table, &row, "attname").and_then(catalog_value_string)?;
            let type_oid =
                catalog_row_value(table, &row, "atttypid").and_then(catalog_value_oid)?;
            let type_mod = catalog_row_value(table, &row, "atttypmod")
                .and_then(catalog_value_i32)
                .unwrap_or(-1);
            let is_not_null = catalog_row_value(table, &row, "attnotnull")
                .and_then(catalog_value_bool)
                .unwrap_or(false);
            Some((
                attnum,
                ColumnRecord {
                    name: normalize_identifier(&name),
                    type_oid,
                    type_mod,
                    is_not_null,
                },
            ))
        })
        .collect::<Vec<_>>();
    columns.sort_by_key(|(attnum, _)| *attnum);
    columns.into_iter().map(|(_, column)| column).collect()
}

fn primary_key_pg_index_row(relation_oid: Oid) -> Option<CatalogRow> {
    let table = static_catalog_by_relation_oid(PG_INDEX_RELATION_OID)?;
    catalog_rows(table.oid).into_iter().find(|row| {
        let indrelid = catalog_row_value(table, row, "indrelid").and_then(catalog_value_oid);
        let is_primary = catalog_row_value(table, row, "indisprimary")
            .and_then(catalog_value_bool)
            .unwrap_or(false);
        indrelid == Some(relation_oid) && is_primary
    })
}

pub fn primary_key_index_oid_for_relation_oid(relation_oid: Oid) -> Option<Oid> {
    let table = static_catalog_by_relation_oid(PG_INDEX_RELATION_OID)?;
    let row = primary_key_pg_index_row(relation_oid)?;
    catalog_row_value(table, &row, "indexrelid").and_then(catalog_value_oid)
}

pub fn primary_key_relation_oid_for_index_oid(index_oid: Oid) -> Option<Oid> {
    let table = static_catalog_by_relation_oid(PG_INDEX_RELATION_OID)?;
    catalog_rows(table.oid).into_iter().find_map(|row| {
        let row_index_oid =
            catalog_row_value(table, &row, "indexrelid").and_then(catalog_value_oid)?;
        if row_index_oid != index_oid {
            return None;
        }
        let is_primary = catalog_row_value(table, &row, "indisprimary")
            .and_then(catalog_value_bool)
            .unwrap_or(false);
        if !is_primary {
            return None;
        }
        catalog_row_value(table, &row, "indrelid").and_then(catalog_value_oid)
    })
}

fn primary_key_constraint_oid_for_relation(
    relation_oid: Oid,
    index_oid: Option<Oid>,
) -> Option<Oid> {
    let table = static_catalog_by_relation_oid(PG_CONSTRAINT_RELATION_OID)?;
    catalog_rows(table.oid).into_iter().find_map(|row| {
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
    columns: &[ColumnRecord],
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
            if attnum <= 0 {
                return None;
            }
            columns
                .get(usize::try_from(attnum - 1).ok()?)
                .map(|column| column.name.clone())
        })
        .collect::<Vec<_>>();
    let constraint_oid = primary_key_constraint_oid_for_relation(relation_oid, index_oid);
    (primary_key, constraint_oid)
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
    let name = catalog_row_value(table, row, "relname").and_then(catalog_value_string)?;
    let relkind = catalog_row_value(table, row, "relkind")
        .and_then(catalog_value_u8)
        .unwrap_or(b'r');
    let columns = relation_columns_from_pg_attribute(oid);
    let (primary_key, primary_key_constraint_oid) =
        relation_primary_key_from_pg_index(oid, &columns);
    Some(RelationRecord {
        oid,
        type_oid,
        namespace,
        name: normalize_identifier(&name),
        relkind,
        columns,
        primary_key,
        primary_key_constraint_oid,
    })
}

pub fn relation_by_name(name: &str) -> Option<RelationRecord> {
    let name = normalize_identifier(name);
    let table = static_catalog_by_relation_oid(PG_CLASS_RELATION_OID)?;
    let mut matches = catalog_rows(table.oid)
        .into_iter()
        .filter_map(|row| {
            let row_name =
                catalog_row_value(table, &row, "relname").and_then(catalog_value_string)?;
            if normalize_identifier(&row_name) != name {
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

pub fn relations() -> Vec<RelationRecord> {
    let Some(table) = static_catalog_by_relation_oid(PG_CLASS_RELATION_OID) else {
        return Vec::new();
    };
    catalog_rows(table.oid)
        .into_iter()
        .filter_map(|row| relation_record_from_pg_class_row(&row))
        .collect()
}

pub fn relation_by_oid(oid: Oid) -> Option<RelationRecord> {
    relation_pg_class_row_by_oid(oid).and_then(|row| relation_record_from_pg_class_row(&row))
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

pub fn lookup_type(oid: Oid) -> Option<CatalogTypeRecord> {
    if let Some(record) = lookup_builtin_type(oid) {
        return Some(record.into());
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
    let table = static_catalog_by_relation_oid(PG_TYPE_RELATION_OID)?;
    catalog_rows(PG_TYPE_RELATION_OID)
        .into_iter()
        .find_map(|row| {
            let record = catalog_type_from_row(table, &row)?;
            (record.namespace == namespace && record.name == canonical_name).then_some(record)
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
            ("pg_am", VirtualCatalogPolicy::Static),
            ("pg_opfamily", VirtualCatalogPolicy::Static),
            ("pg_amop", VirtualCatalogPolicy::Static),
            ("pg_amproc", VirtualCatalogPolicy::Static),
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
        assert_eq!(
            relation_by_oid(relation.oid).unwrap().name,
            "pgbench_accounts"
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
        assert!(!catalog_rows(PG_CLASS_RELATION_OID).iter().any(|row| {
            value_name(row_value("pg_class", row, "relname")) == Some("pgbench_accounts")
        }));
    }
}
