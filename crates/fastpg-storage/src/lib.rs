#![deny(unsafe_op_in_unsafe_fn)]

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::CStr;
use std::os::raw::c_char;
use std::slice;
use std::sync::{Arc, Mutex, OnceLock};

use fastpg_catalog::{
    BPCHAR_OID, CatalogError, CatalogValue, ColumnRecord, INT2_OID, INT2VECTOR_OID, INT4_OID,
    INT8_OID, NAME_OID, OID_OID, OIDVECTOR_OID, PG_CATALOG_NAMESPACE_OID, PG_NODE_TREE_OID,
    TEXT_OID, TIMESTAMP_OID, VARCHAR_OID, add_primary_key,
    btree_opclass_for_type as catalog_btree_opclass_for_type, builtin_aggregate_by_proc_oid,
    builtin_cast_by_source_target, builtin_namespace_by_name, builtin_namespace_by_oid,
    builtin_operator_by_oid, builtin_operator_by_signature, builtin_operators_by_name,
    builtin_proc_by_oid, builtin_procs_by_name, builtin_type_by_name, catalog_row_value,
    catalog_rows, create_relation, drop_relation, lookup_builtin_type, relation_by_name,
    relation_by_oid, relation_column_count, static_catalog_by_name, static_catalog_by_relation_oid,
    truncate_relation, virtual_catalog_by_name, virtual_catalog_by_relation_oid,
};
use fastpg_types::Oid;

const NAMEDATALEN: usize = 64;
const FASTPG_PROC_MAX_ARGS: usize = 8;
const FASTPG_PROC_SOURCE_LEN: usize = 64;
const PRIMARY_KEY_INDEX_OID_OFFSET: u32 = 1_000_000_000;
const FASTPG_MAX_INDEX_KEYS: usize = 32;

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FastPgRustCatalogType {
    pub oid: u32,
    pub namespace_oid: u32,
    pub owner_oid: u32,
    pub name: [c_char; NAMEDATALEN],
    pub typlen: i16,
    pub typbyval: u8,
    pub typalign: u8,
    pub typdelim: u8,
    pub _padding: u8,
    pub typinput: u32,
    pub typoutput: u32,
    pub typreceive: u32,
    pub typsend: u32,
    pub typmodin: u32,
    pub typmodout: u32,
    pub typisdefined: u8,
    pub typtype: u8,
    pub typcategory: u8,
    pub typispreferred: u8,
    pub typrelid: u32,
    pub typelem: u32,
    pub typarray: u32,
    pub typbasetype: u32,
    pub typtypmod: i32,
    pub typcollation: u32,
    pub typsubscript: u32,
    pub typstorage: u8,
    pub _trailing_padding: [u8; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FastPgRustCatalogRelation {
    pub oid: u32,
    pub namespace_oid: u32,
    pub name: [c_char; NAMEDATALEN],
    pub column_count: u16,
    pub relkind: u8,
    pub has_primary_key: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FastPgRustCatalogColumn {
    pub name: [c_char; NAMEDATALEN],
    pub type_oid: u32,
    pub type_mod: i32,
    pub is_not_null: u8,
    pub _padding: [u8; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FastPgRustPrimaryKeyIndexInfo {
    pub index_oid: u32,
    pub heap_oid: u32,
    pub key_count: u16,
    pub _padding: [u8; 2],
    pub attnums: [i16; FASTPG_MAX_INDEX_KEYS],
    pub type_oids: [u32; FASTPG_MAX_INDEX_KEYS],
    pub collation_oids: [u32; FASTPG_MAX_INDEX_KEYS],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FastPgRustCatalogNamespace {
    pub oid: u32,
    pub owner_oid: u32,
    pub name: [c_char; NAMEDATALEN],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FastPgRustCatalogProc {
    pub oid: u32,
    pub namespace_oid: u32,
    pub owner_oid: u32,
    pub language_oid: u32,
    pub name: [c_char; NAMEDATALEN],
    pub source: [c_char; FASTPG_PROC_SOURCE_LEN],
    pub cost: f32,
    pub rows: f32,
    pub variadic_oid: u32,
    pub support_oid: u32,
    pub return_type_oid: u32,
    pub arg_count: u16,
    pub arg_default_count: u16,
    pub kind: u8,
    pub security_definer: u8,
    pub leakproof: u8,
    pub is_strict: u8,
    pub returns_set: u8,
    pub volatility: u8,
    pub parallel: u8,
    pub _padding: u8,
    pub arg_type_oids: [u32; FASTPG_PROC_MAX_ARGS],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FastPgRustCatalogAggregate {
    pub function_oid: u32,
    pub transition_fn_oid: u32,
    pub final_fn_oid: u32,
    pub combine_fn_oid: u32,
    pub serial_fn_oid: u32,
    pub deserial_fn_oid: u32,
    pub moving_transition_fn_oid: u32,
    pub moving_inverse_fn_oid: u32,
    pub moving_final_fn_oid: u32,
    pub sort_operator_oid: u32,
    pub transition_type_oid: u32,
    pub moving_transition_type_oid: u32,
    pub transition_space: i32,
    pub moving_transition_space: i32,
    pub direct_arg_count: u16,
    pub kind: u8,
    pub final_extra: u8,
    pub moving_final_extra: u8,
    pub final_modify: u8,
    pub moving_final_modify: u8,
    pub has_init_value: u8,
    pub has_moving_init_value: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FastPgRustCatalogOperator {
    pub oid: u32,
    pub namespace_oid: u32,
    pub owner_oid: u32,
    pub name: [c_char; NAMEDATALEN],
    pub kind: u8,
    pub can_merge: u8,
    pub can_hash: u8,
    pub _padding: u8,
    pub left_type_oid: u32,
    pub right_type_oid: u32,
    pub result_type_oid: u32,
    pub commutator_oid: u32,
    pub negator_oid: u32,
    pub code_fn_oid: u32,
    pub rest_fn_oid: u32,
    pub join_fn_oid: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FastPgRustCatalogCast {
    pub oid: u32,
    pub source_type_oid: u32,
    pub target_type_oid: u32,
    pub function_oid: u32,
    pub context: u8,
    pub method: u8,
    pub _padding: [u8; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FastPgRustCatalogOpclass {
    pub oid: u32,
    pub method_oid: u32,
    pub namespace_oid: u32,
    pub owner_oid: u32,
    pub family_oid: u32,
    pub input_type_oid: u32,
    pub key_type_oid: u32,
    pub is_default: u8,
    pub _padding: [u8; 3],
    pub name: [c_char; NAMEDATALEN],
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RowId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Cell {
    value: usize,
    is_null: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Row {
    row_id: u64,
    cells: Vec<Cell>,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum IndexKeyPart {
    Null,
    ByValue(usize),
    Bytes(Vec<u8>),
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct IndexKey(Vec<IndexKeyPart>);

#[derive(Clone, Debug, Eq, PartialEq)]
struct PrimaryKeyColumnSpec {
    column_index: usize,
    typbyval: bool,
    typlen: i16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PrimaryKeySpec {
    columns: Vec<PrimaryKeyColumnSpec>,
}

#[derive(Default, Debug)]
struct RowSegment {
    rows: Vec<Row>,
    payloads: Vec<Box<[u8]>>,
}

#[derive(Debug)]
struct RelationRows {
    committed_row_ids: BTreeSet<u64>,
    committed_row_index: HashMap<u64, Row>,
    committed_payloads: Vec<Box<[u8]>>,
    primary_key_index: BTreeMap<IndexKey, u64>,
    next_row_id: u64,
}

impl Default for RelationRows {
    fn default() -> Self {
        Self {
            committed_row_ids: BTreeSet::new(),
            committed_row_index: HashMap::new(),
            committed_payloads: Vec::new(),
            primary_key_index: BTreeMap::new(),
            next_row_id: 1,
        }
    }
}

impl RelationRows {
    fn allocate_row_id(&mut self) -> Option<u64> {
        let row_id = self.next_row_id;
        if row_id == 0 {
            return None;
        }
        self.next_row_id = self.next_row_id.checked_add(1)?;
        Some(row_id)
    }
}

#[derive(Default, Debug)]
struct TransactionOverlay {
    relations: HashMap<u32, RowSegment>,
    deleted_row_ids: HashMap<u32, BTreeSet<u64>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ScanState {
    rows: Vec<Row>,
    payloads: Vec<Box<[u8]>>,
    next_index: usize,
}

#[derive(Debug)]
struct StorageState {
    relations: HashMap<u32, RelationRows>,
    primary_key_specs: HashMap<u32, Option<PrimaryKeySpec>>,
    primary_key_generation: u64,
}

impl Default for StorageState {
    fn default() -> Self {
        Self {
            relations: HashMap::new(),
            primary_key_specs: HashMap::new(),
            primary_key_generation: fastpg_catalog::current_generation(),
        }
    }
}

#[derive(Debug)]
pub struct SessionStorage {
    transaction_stack: Vec<TransactionOverlay>,
    explicit_transaction: bool,
    scans: HashMap<u64, ScanState>,
    next_scan_handle: u64,
}

impl Default for SessionStorage {
    fn default() -> Self {
        Self {
            transaction_stack: Vec::new(),
            explicit_transaction: false,
            scans: HashMap::new(),
            next_scan_handle: 1,
        }
    }
}

pub type SessionStorageHandle = Arc<Mutex<SessionStorage>>;

pub fn new_session_storage() -> SessionStorageHandle {
    Arc::new(Mutex::new(SessionStorage::default()))
}

static DEFAULT_SESSION_STORAGE: OnceLock<SessionStorageHandle> = OnceLock::new();

thread_local! {
    static CURRENT_SESSION_STORAGE: RefCell<Option<SessionStorageHandle>> = const { RefCell::new(None) };
}

#[derive(Debug)]
pub struct SessionStorageGuard {
    previous: Option<SessionStorageHandle>,
}

pub fn enter_session_storage(handle: SessionStorageHandle) -> SessionStorageGuard {
    let previous = CURRENT_SESSION_STORAGE.with(|slot| slot.replace(Some(handle)));
    SessionStorageGuard { previous }
}

impl Drop for SessionStorageGuard {
    fn drop(&mut self) {
        CURRENT_SESSION_STORAGE.with(|slot| {
            slot.replace(self.previous.take());
        });
    }
}

fn default_session_storage() -> SessionStorageHandle {
    DEFAULT_SESSION_STORAGE
        .get_or_init(new_session_storage)
        .clone()
}

fn current_session_storage() -> SessionStorageHandle {
    CURRENT_SESSION_STORAGE
        .with(|slot| slot.borrow().clone())
        .unwrap_or_else(default_session_storage)
}

impl SessionStorage {
    fn allocate_scan_handle(&mut self) -> u64 {
        let handle = self.next_scan_handle;
        self.next_scan_handle = self.next_scan_handle.checked_add(1).unwrap_or(1);
        if self.next_scan_handle == 0 {
            self.next_scan_handle = 1;
        }
        handle
    }

    fn ensure_transaction(&mut self) {
        if self.transaction_stack.is_empty() {
            self.transaction_stack.push(TransactionOverlay::default());
        }
    }
}

impl StorageState {
    fn begin_explicit_transaction(&mut self, session: &mut SessionStorage) {
        if !session.explicit_transaction {
            self.commit_implicit_transaction(session);
        }
        session.ensure_transaction();
        session.explicit_transaction = true;
    }

    fn commit_explicit_transaction(&mut self, session: &mut SessionStorage) {
        while !session.transaction_stack.is_empty() {
            self.commit_top_overlay(session);
        }
        session.explicit_transaction = false;
    }

    fn abort_explicit_transaction(&mut self, session: &mut SessionStorage) {
        session.transaction_stack.clear();
        session.explicit_transaction = false;
    }

    fn commit_implicit_transaction(&mut self, session: &mut SessionStorage) {
        if session.explicit_transaction {
            return;
        }
        while !session.transaction_stack.is_empty() {
            self.commit_top_overlay(session);
        }
    }

    fn abort_implicit_transaction(&mut self, session: &mut SessionStorage) {
        if !session.explicit_transaction {
            session.transaction_stack.clear();
        }
    }

    fn is_explicit_transaction(&self, session: &SessionStorage) -> bool {
        session.explicit_transaction
    }

    fn visible_row_count(&self, session: &SessionStorage, relid: u32) -> usize {
        let Some(relation) = self.relations.get(&relid) else {
            return self
                .visible_overlay_stack(session)
                .iter()
                .filter_map(|overlay| overlay.relations.get(&relid))
                .map(|segment| segment.rows.len())
                .sum();
        };

        let mut row_count = committed_row_count(relation);
        let mut visible_overlay_row_ids = BTreeSet::new();
        let mut deleted_committed_row_ids = BTreeSet::new();

        for overlay in self.visible_overlay_stack(session) {
            if let Some(deleted_row_ids) = overlay.deleted_row_ids.get(&relid) {
                for row_id in deleted_row_ids {
                    if visible_overlay_row_ids.remove(row_id) {
                        row_count = row_count.saturating_sub(1);
                    } else if !deleted_committed_row_ids.contains(row_id)
                        && committed_contains_row_id(relation, *row_id)
                    {
                        deleted_committed_row_ids.insert(*row_id);
                        row_count = row_count.saturating_sub(1);
                    }
                }
            }

            if let Some(segment) = overlay.relations.get(&relid) {
                for row in &segment.rows {
                    if visible_overlay_row_ids.insert(row.row_id) {
                        row_count += 1;
                    }
                }
            }
        }

        row_count
    }

    fn visible_rows(&self, session: &SessionStorage, relid: u32) -> Vec<Row> {
        let mut rows = BTreeMap::new();
        if let Some(relation) = self.relations.get(&relid) {
            for row_id in &relation.committed_row_ids {
                if let Some(row) = relation.committed_row_index.get(row_id) {
                    rows.insert(*row_id, row.clone());
                }
            }
        }
        for overlay in self.visible_overlay_stack(session) {
            if let Some(deleted_row_ids) = overlay.deleted_row_ids.get(&relid) {
                for row_id in deleted_row_ids {
                    rows.remove(row_id);
                }
            }
            if let Some(segment) = overlay.relations.get(&relid) {
                for row in &segment.rows {
                    rows.insert(row.row_id, row.clone());
                }
            }
        }
        rows.into_values().collect()
    }

    fn find_visible_row(&self, session: &SessionStorage, relid: u32, row_id: u64) -> Option<Row> {
        if row_id == 0 {
            return None;
        }

        for overlay in self.visible_overlay_stack(session).iter().rev() {
            if let Some(segment) = overlay.relations.get(&relid)
                && let Some(row) = segment.rows.iter().find(|row| row.row_id == row_id)
            {
                return Some(row.clone());
            }
            if overlay
                .deleted_row_ids
                .get(&relid)
                .is_some_and(|deleted| deleted.contains(&row_id))
            {
                return None;
            }
        }

        self.find_committed_row(relid, row_id)
    }

    fn find_committed_row(&self, relid: u32, row_id: u64) -> Option<Row> {
        self.relations
            .get(&relid)
            .and_then(|relation| relation.committed_row_index.get(&row_id).cloned())
    }

    fn find_visible_row_by_primary_key(
        &self,
        session: &SessionStorage,
        relid: u32,
        primary_key_spec: &PrimaryKeySpec,
        key: &IndexKey,
    ) -> Option<Row> {
        let mut committed_candidate = self
            .relations
            .get(&relid)
            .and_then(|relation| relation.primary_key_index.get(key).copied());

        for overlay in self.visible_overlay_stack(session).iter().rev() {
            if let Some(segment) = overlay.relations.get(&relid)
                && let Some(row) = segment
                    .rows
                    .iter()
                    .find(|row| primary_key_for_row(primary_key_spec, row).as_ref() == Some(key))
            {
                return Some(row.clone());
            }

            if let Some(row_id) = committed_candidate
                && overlay
                    .deleted_row_ids
                    .get(&relid)
                    .is_some_and(|deleted| deleted.contains(&row_id))
            {
                committed_candidate = None;
            }
        }

        committed_candidate.and_then(|row_id| self.find_committed_row(relid, row_id))
    }

    fn has_primary_key_conflict(
        &mut self,
        session: &SessionStorage,
        relid: u32,
        row: &Row,
        replacing_row_id: Option<u64>,
    ) -> bool {
        let Some(primary_key_spec) = self.primary_key_spec(relid) else {
            return false;
        };
        let Some(key) = primary_key_for_row(&primary_key_spec, row) else {
            return false;
        };

        self.find_visible_row_by_primary_key(session, relid, &primary_key_spec, &key)
            .is_some_and(|existing| Some(existing.row_id) != replacing_row_id)
    }

    fn primary_key_spec(&mut self, relid: u32) -> Option<PrimaryKeySpec> {
        let catalog_generation = fastpg_catalog::current_generation();
        if self.primary_key_generation != catalog_generation {
            self.primary_key_specs.clear();
            self.primary_key_generation = catalog_generation;
        }
        if let Some(spec) = self.primary_key_specs.get(&relid) {
            return spec.clone();
        }

        let spec = relation_by_oid(Oid(relid)).and_then(|relation| primary_key_spec(&relation));
        self.primary_key_specs.insert(relid, spec.clone());
        spec
    }

    fn invalidate_primary_key_spec(&mut self, relid: u32) {
        self.primary_key_specs.remove(&relid);
    }

    fn rebuild_primary_key_index(&mut self, relid: u32) {
        self.invalidate_primary_key_spec(relid);
        let primary_key_spec = self.primary_key_spec(relid);
        let Some(relation) = self.relations.get_mut(&relid) else {
            return;
        };

        relation.primary_key_index.clear();
        for row_id in &relation.committed_row_ids {
            let Some(row) = relation.committed_row_index.get(row_id) else {
                continue;
            };
            if let Some(primary_key_spec) = &primary_key_spec
                && let Some(key) = primary_key_for_row(primary_key_spec, row)
            {
                relation.primary_key_index.insert(key, *row_id);
            }
        }
    }

    fn clear_relation(&mut self, session: &mut SessionStorage, relid: u32) {
        self.invalidate_primary_key_spec(relid);
        self.relations.insert(relid, RelationRows::default());
        for overlay in &mut session.transaction_stack {
            overlay.relations.remove(&relid);
            overlay.deleted_row_ids.remove(&relid);
        }
    }

    fn commit_top_overlay(&mut self, session: &mut SessionStorage) {
        let Some(overlay) = session.transaction_stack.pop() else {
            return;
        };

        if let Some(parent) = session.transaction_stack.last_mut() {
            merge_overlay_into_overlay(parent, overlay);
        } else {
            self.commit_overlay_to_relations(overlay);
        }
    }

    fn visible_overlay_stack<'a>(&self, session: &'a SessionStorage) -> &'a [TransactionOverlay] {
        &session.transaction_stack
    }

    fn commit_overlay_to_relations(&mut self, overlay: TransactionOverlay) {
        let TransactionOverlay {
            relations,
            deleted_row_ids,
        } = overlay;
        let mut unchanged_primary_key_updates: HashMap<u32, BTreeSet<u64>> = HashMap::new();

        for (relid, deleted_row_ids) in &deleted_row_ids {
            if deleted_row_ids.is_empty() {
                continue;
            }
            let primary_key_spec = self.primary_key_spec(*relid);
            let unchanged_row_ids = self.remove_committed_entries(
                *relid,
                primary_key_spec.as_ref(),
                deleted_row_ids,
                relations.get(relid),
            );
            if !unchanged_row_ids.is_empty() {
                unchanged_primary_key_updates.insert(*relid, unchanged_row_ids);
            }
        }

        for (relid, mut segment) in relations {
            if segment.rows.is_empty() {
                continue;
            }
            let primary_key_spec = self.primary_key_spec(relid);
            let unchanged_row_ids = unchanged_primary_key_updates.get(&relid);
            let relation = self.relations.entry(relid).or_default();
            for row in &segment.rows {
                relation.committed_row_ids.insert(row.row_id);
                relation.committed_row_index.insert(row.row_id, row.clone());
                if unchanged_row_ids.is_some_and(|row_ids| row_ids.contains(&row.row_id)) {
                    continue;
                }
                if let Some(primary_key_spec) = &primary_key_spec
                    && let Some(key) = primary_key_for_row(primary_key_spec, row)
                {
                    relation.primary_key_index.insert(key, row.row_id);
                }
            }
            relation.committed_payloads.append(&mut segment.payloads);
        }
    }

    fn remove_committed_entries(
        &mut self,
        relid: u32,
        primary_key_spec: Option<&PrimaryKeySpec>,
        deleted_row_ids: &BTreeSet<u64>,
        replacement_segment: Option<&RowSegment>,
    ) -> BTreeSet<u64> {
        let mut unchanged_primary_key_row_ids = BTreeSet::new();
        if deleted_row_ids.is_empty() {
            return unchanged_primary_key_row_ids;
        }

        if let Some(relation) = self.relations.get_mut(&relid) {
            for row_id in deleted_row_ids {
                relation.committed_row_ids.remove(row_id);
                let Some(row) = relation.committed_row_index.remove(row_id) else {
                    continue;
                };
                let Some(primary_key_spec) = primary_key_spec else {
                    continue;
                };
                if let Some(replacement) =
                    replacement_segment.and_then(|segment| find_row_in_segment(segment, *row_id))
                    && primary_key_unchanged(primary_key_spec, &row, replacement)
                {
                    unchanged_primary_key_row_ids.insert(*row_id);
                    continue;
                }
                if let Some(key) = primary_key_for_row(primary_key_spec, &row) {
                    relation.primary_key_index.remove(&key);
                }
            }
        }
        unchanged_primary_key_row_ids
    }
}

fn committed_row_count(relation: &RelationRows) -> usize {
    relation.committed_row_index.len()
}

fn committed_contains_row_id(relation: &RelationRows, row_id: u64) -> bool {
    relation.committed_row_index.contains_key(&row_id)
}

fn merge_overlay_into_overlay(parent: &mut TransactionOverlay, overlay: TransactionOverlay) {
    for (relid, deleted_row_ids) in overlay.deleted_row_ids {
        if deleted_row_ids.is_empty() {
            continue;
        }
        if let Some(parent_segment) = parent.relations.get_mut(&relid) {
            remove_rows_from_segment(parent_segment, &deleted_row_ids);
        }
        parent
            .deleted_row_ids
            .entry(relid)
            .or_default()
            .extend(deleted_row_ids);
    }

    for (relid, mut segment) in overlay.relations {
        if segment.rows.is_empty() {
            continue;
        }
        let parent_segment = parent.relations.entry(relid).or_default();
        parent_segment.rows.append(&mut segment.rows);
        parent_segment.payloads.append(&mut segment.payloads);
    }
}

fn remove_rows_from_segment(segment: &mut RowSegment, deleted_row_ids: &BTreeSet<u64>) {
    segment
        .rows
        .retain(|row| !deleted_row_ids.contains(&row.row_id));
}

fn find_row_in_segment(segment: &RowSegment, row_id: u64) -> Option<&Row> {
    segment.rows.iter().find(|row| row.row_id == row_id)
}

fn primary_key_unchanged(primary_key_spec: &PrimaryKeySpec, old_row: &Row, new_row: &Row) -> bool {
    primary_key_spec.columns.iter().all(|column| {
        let Some(old_cell) = old_row.cells.get(column.column_index) else {
            return false;
        };
        let Some(new_cell) = new_row.cells.get(column.column_index) else {
            return false;
        };
        if old_cell.is_null || new_cell.is_null {
            return old_cell.is_null == new_cell.is_null;
        }
        if column.typbyval {
            return old_cell.value == new_cell.value;
        }
        byref_key_bytes(old_cell.value, column.typlen)
            == byref_key_bytes(new_cell.value, column.typlen)
    })
}

static STORAGE: OnceLock<Mutex<StorageState>> = OnceLock::new();
static PRIMARY_KEY_INDEX_INFO_CACHE: OnceLock<
    Mutex<HashMap<u32, Option<FastPgRustPrimaryKeyIndexInfo>>>,
> = OnceLock::new();

fn storage() -> &'static Mutex<StorageState> {
    STORAGE.get_or_init(|| Mutex::new(StorageState::default()))
}

fn primary_key_index_info_cache()
-> &'static Mutex<HashMap<u32, Option<FastPgRustPrimaryKeyIndexInfo>>> {
    PRIMARY_KEY_INDEX_INFO_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_storage<R>(f: impl FnOnce(&mut StorageState, &mut SessionStorage) -> R) -> R {
    let session = current_session_storage();
    let mut session = match session.lock() {
        Ok(session) => session,
        Err(poisoned) => poisoned.into_inner(),
    };
    match storage().lock() {
        Ok(mut state) => f(&mut state, &mut session),
        Err(poisoned) => {
            let mut state = poisoned.into_inner();
            f(&mut state, &mut session)
        }
    }
}

fn clear_primary_key_index_info_cache() {
    match primary_key_index_info_cache().lock() {
        Ok(mut cache) => cache.clear(),
        Err(poisoned) => poisoned.into_inner().clear(),
    }
}

fn cached_primary_key_index_info(index_oid: Oid) -> Option<FastPgRustPrimaryKeyIndexInfo> {
    let cached = match primary_key_index_info_cache().lock() {
        Ok(cache) => cache.get(&index_oid.0).copied(),
        Err(poisoned) => poisoned.into_inner().get(&index_oid.0).copied(),
    };
    if let Some(index_info) = cached {
        return index_info;
    }

    let index_info = primary_key_index_relation(index_oid)
        .and_then(|relation| primary_key_index_info(&relation, index_oid));
    match primary_key_index_info_cache().lock() {
        Ok(mut cache) => {
            cache.insert(index_oid.0, index_info);
        }
        Err(poisoned) => {
            poisoned.into_inner().insert(index_oid.0, index_info);
        }
    }
    index_info
}

unsafe fn c_str_to_string(value: *const c_char) -> Result<String, String> {
    if value.is_null() {
        return Err("null string pointer".to_owned());
    }
    unsafe { CStr::from_ptr(value) }
        .to_str()
        .map(str::to_owned)
        .map_err(|error| format!("invalid UTF-8 string: {error}"))
}

unsafe fn c_str_array_to_strings(
    values: *const *const c_char,
    len: usize,
) -> Result<Vec<String>, String> {
    if len == 0 {
        return Ok(Vec::new());
    }
    if values.is_null() {
        return Err("null string array pointer".to_owned());
    }
    unsafe { slice::from_raw_parts(values, len) }
        .iter()
        .map(|value| unsafe { c_str_to_string(*value) })
        .collect()
}

unsafe fn u32_array<'a>(values: *const u32, len: usize) -> Result<&'a [u32], String> {
    if len == 0 {
        return Ok(&[]);
    }
    if values.is_null() {
        return Err("null OID array pointer".to_owned());
    }
    Ok(unsafe { slice::from_raw_parts(values, len) })
}

unsafe fn i32_array<'a>(values: *const i32, len: usize) -> Result<&'a [i32], String> {
    if len == 0 {
        return Ok(&[]);
    }
    if values.is_null() {
        return Err("null typmod array pointer".to_owned());
    }
    Ok(unsafe { slice::from_raw_parts(values, len) })
}

unsafe fn u8_array<'a>(values: *const u8, len: usize) -> Result<&'a [u8], String> {
    if len == 0 {
        return Ok(&[]);
    }
    if values.is_null() {
        return Err("null flag array pointer".to_owned());
    }
    Ok(unsafe { slice::from_raw_parts(values, len) })
}

unsafe fn write_c_output(buffer: *mut c_char, buffer_len: usize, value: &str) {
    if buffer.is_null() || buffer_len == 0 {
        return;
    }

    let bytes = value.as_bytes();
    let copy_len = bytes.len().min(buffer_len - 1);
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.cast::<u8>(), copy_len);
        *buffer.add(copy_len) = 0;
    }
}

fn fixed_c_bytes<const N: usize>(value: &str) -> [c_char; N] {
    let mut out = [0 as c_char; N];
    if N == 0 {
        return out;
    }
    for (index, byte) in value.as_bytes().iter().copied().take(N - 1).enumerate() {
        out[index] = byte as c_char;
    }
    out
}

fn fixed_c_name(value: &str) -> [c_char; NAMEDATALEN] {
    fixed_c_bytes(value)
}

unsafe fn write_catalog_error(
    sqlstate_out: *mut c_char,
    sqlstate_len: usize,
    message_out: *mut c_char,
    message_len: usize,
    sqlstate: &str,
    message: &str,
) {
    unsafe {
        write_c_output(sqlstate_out, sqlstate_len, sqlstate);
        write_c_output(message_out, message_len, message);
    }
}

fn relation_to_ffi(relation: &fastpg_catalog::RelationRecord) -> FastPgRustCatalogRelation {
    FastPgRustCatalogRelation {
        oid: relation.oid.0,
        namespace_oid: relation.namespace.0,
        name: fixed_c_name(&relation.name),
        column_count: relation.columns.len().min(u16::MAX as usize) as u16,
        relkind: b'r',
        has_primary_key: u8::from(!relation.primary_key.is_empty()),
    }
}

fn primary_key_index_to_ffi(
    relation: &fastpg_catalog::RelationRecord,
    index_oid: Oid,
) -> FastPgRustCatalogRelation {
    FastPgRustCatalogRelation {
        oid: index_oid.0,
        namespace_oid: relation.namespace.0,
        name: fixed_c_name(&primary_key_index_name(relation)),
        column_count: relation.primary_key.len().min(u16::MAX as usize) as u16,
        relkind: b'i',
        has_primary_key: 0,
    }
}

fn virtual_catalog_column_count(relation_oid: Oid) -> u16 {
    static_catalog_by_relation_oid(relation_oid)
        .map(|table| table.columns.len().min(u16::MAX as usize) as u16)
        .unwrap_or(0)
}

fn virtual_catalog_relation_to_ffi(
    relation: fastpg_catalog::VirtualCatalogRecord,
) -> FastPgRustCatalogRelation {
    FastPgRustCatalogRelation {
        oid: relation.relation_oid.0,
        namespace_oid: PG_CATALOG_NAMESPACE_OID.0,
        name: fixed_c_name(relation.name),
        column_count: virtual_catalog_column_count(relation.relation_oid),
        relkind: b'r',
        has_primary_key: 0,
    }
}

fn column_to_ffi(column: &ColumnRecord) -> FastPgRustCatalogColumn {
    FastPgRustCatalogColumn {
        name: fixed_c_name(&column.name),
        type_oid: column.type_oid.0,
        type_mod: column.type_mod,
        is_not_null: u8::from(column.is_not_null),
        _padding: [0; 3],
    }
}

fn static_catalog_column_to_ffi(
    column: &fastpg_catalog::StaticCatalogColumn,
) -> FastPgRustCatalogColumn {
    FastPgRustCatalogColumn {
        name: fixed_c_name(column.name),
        type_oid: column.type_oid.0,
        type_mod: -1,
        is_not_null: u8::from(column.attnotnull),
        _padding: [0; 3],
    }
}

fn namespace_to_ffi(record: &fastpg_catalog::PgNamespaceRecord) -> FastPgRustCatalogNamespace {
    FastPgRustCatalogNamespace {
        oid: record.oid.0,
        owner_oid: record.owner.0,
        name: fixed_c_name(record.name),
    }
}

fn proc_to_ffi(record: &fastpg_catalog::PgProcRecord) -> Option<FastPgRustCatalogProc> {
    let mut arg_type_oids = [0; FASTPG_PROC_MAX_ARGS];
    if record.arg_types.len() > FASTPG_PROC_MAX_ARGS {
        return None;
    }
    for (index, oid) in record.arg_types.iter().enumerate() {
        arg_type_oids[index] = oid.0;
    }

    Some(FastPgRustCatalogProc {
        oid: record.oid.0,
        namespace_oid: record.namespace.0,
        owner_oid: record.owner.0,
        language_oid: record.language.0,
        name: fixed_c_name(record.name),
        source: fixed_c_bytes(record.source),
        cost: record.cost as f32,
        rows: record.rows as f32,
        variadic_oid: record.variadic.0,
        support_oid: record.support.0,
        return_type_oid: record.return_type.0,
        arg_count: record.arg_types.len() as u16,
        arg_default_count: record.arg_defaults,
        kind: record.kind,
        security_definer: u8::from(record.security_definer),
        leakproof: u8::from(record.leakproof),
        is_strict: u8::from(record.strict),
        returns_set: u8::from(record.returns_set),
        volatility: record.volatility,
        parallel: record.parallel,
        _padding: 0,
        arg_type_oids,
    })
}

fn aggregate_to_ffi(record: &fastpg_catalog::PgAggregateRecord) -> FastPgRustCatalogAggregate {
    FastPgRustCatalogAggregate {
        function_oid: record.function_oid.0,
        transition_fn_oid: record.transition_fn.0,
        final_fn_oid: record.final_fn.0,
        combine_fn_oid: record.combine_fn.0,
        serial_fn_oid: record.serial_fn.0,
        deserial_fn_oid: record.deserial_fn.0,
        moving_transition_fn_oid: record.moving_transition_fn.0,
        moving_inverse_fn_oid: record.moving_inverse_fn.0,
        moving_final_fn_oid: record.moving_final_fn.0,
        sort_operator_oid: record.sort_operator.0,
        transition_type_oid: record.transition_type.0,
        moving_transition_type_oid: record.moving_transition_type.0,
        transition_space: record.transition_space,
        moving_transition_space: record.moving_transition_space,
        direct_arg_count: record.direct_arg_count,
        kind: record.kind,
        final_extra: u8::from(record.final_extra),
        moving_final_extra: u8::from(record.moving_final_extra),
        final_modify: record.final_modify,
        moving_final_modify: record.moving_final_modify,
        has_init_value: u8::from(record.init_value.is_some()),
        has_moving_init_value: u8::from(record.moving_init_value.is_some()),
    }
}

fn operator_to_ffi(record: &fastpg_catalog::PgOperatorRecord) -> FastPgRustCatalogOperator {
    FastPgRustCatalogOperator {
        oid: record.oid.0,
        namespace_oid: record.namespace.0,
        owner_oid: record.owner.0,
        name: fixed_c_name(record.name),
        kind: record.kind,
        can_merge: u8::from(record.can_merge),
        can_hash: u8::from(record.can_hash),
        _padding: 0,
        left_type_oid: record.left_type.0,
        right_type_oid: record.right_type.0,
        result_type_oid: record.result_type.0,
        commutator_oid: record.commutator.0,
        negator_oid: record.negator.0,
        code_fn_oid: record.code.0,
        rest_fn_oid: record.rest.0,
        join_fn_oid: record.join.0,
    }
}

fn cast_to_ffi(record: &fastpg_catalog::PgCastRecord) -> FastPgRustCatalogCast {
    FastPgRustCatalogCast {
        oid: record.oid.0,
        source_type_oid: record.source_type.0,
        target_type_oid: record.target_type.0,
        function_oid: record.function.0,
        context: record.context,
        method: record.method,
        _padding: [0; 2],
    }
}

fn catalog_value_oid(value: &CatalogValue) -> Option<Oid> {
    match value {
        CatalogValue::Oid(value) => Some(*value),
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

fn catalog_value_name(value: &CatalogValue) -> Option<&str> {
    match value {
        CatalogValue::Name(value) | CatalogValue::Text(value) | CatalogValue::Raw(value) => {
            Some(value)
        }
        _ => None,
    }
}

fn opclass_to_ffi(row: &fastpg_catalog::CatalogRow) -> Option<FastPgRustCatalogOpclass> {
    let table = static_catalog_by_name("pg_opclass")?;
    let oid = catalog_row_value(table, row, "oid").and_then(catalog_value_oid)?;
    let method_oid = catalog_row_value(table, row, "opcmethod").and_then(catalog_value_oid)?;
    let name = catalog_row_value(table, row, "opcname").and_then(catalog_value_name)?;
    let namespace_oid =
        catalog_row_value(table, row, "opcnamespace").and_then(catalog_value_oid)?;
    let owner_oid = catalog_row_value(table, row, "opcowner").and_then(catalog_value_oid)?;
    let family_oid = catalog_row_value(table, row, "opcfamily").and_then(catalog_value_oid)?;
    let input_type_oid = catalog_row_value(table, row, "opcintype").and_then(catalog_value_oid)?;
    let is_default = catalog_row_value(table, row, "opcdefault").and_then(catalog_value_bool)?;
    let key_type_oid = catalog_row_value(table, row, "opckeytype").and_then(catalog_value_oid)?;

    Some(FastPgRustCatalogOpclass {
        oid: oid.0,
        method_oid: method_oid.0,
        namespace_oid: namespace_oid.0,
        owner_oid: owner_oid.0,
        family_oid: family_oid.0,
        input_type_oid: input_type_oid.0,
        key_type_oid: key_type_oid.0,
        is_default: u8::from(is_default),
        _padding: [0; 3],
        name: fixed_c_name(name),
    })
}

fn opclass_by_oid(oid: Oid) -> Option<FastPgRustCatalogOpclass> {
    catalog_rows(static_catalog_by_name("pg_opclass")?.oid)
        .into_iter()
        .find_map(|row| {
            let record = opclass_to_ffi(&row)?;
            (record.oid == oid.0).then_some(record)
        })
}

fn opclass_by_name(
    method_oid: Oid,
    name: &str,
    namespace_oid: Oid,
) -> Option<FastPgRustCatalogOpclass> {
    let name = name.trim().to_ascii_lowercase();
    catalog_rows(static_catalog_by_name("pg_opclass")?.oid)
        .into_iter()
        .find_map(|row| {
            let record = opclass_to_ffi(&row)?;
            let record_name = c_name_to_string(&record.name);
            (record.method_oid == method_oid.0
                && record.namespace_oid == namespace_oid.0
                && record_name == name)
                .then_some(record)
        })
}

fn c_name_to_string(name: &[c_char; NAMEDATALEN]) -> String {
    let len = name
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(NAMEDATALEN);
    let bytes = name[..len]
        .iter()
        .map(|byte| *byte as u8)
        .collect::<Vec<_>>();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn invalid_ffi_argument(message: String) -> CatalogError {
    CatalogError::new("22023", message)
}

fn primary_key_spec(relation: &fastpg_catalog::RelationRecord) -> Option<PrimaryKeySpec> {
    if relation.primary_key.is_empty() {
        return None;
    }

    let mut columns = Vec::with_capacity(relation.primary_key.len());
    for primary_key_column in &relation.primary_key {
        let column_index = relation
            .columns
            .iter()
            .position(|column| &column.name == primary_key_column)?;
        let column = relation.columns.get(column_index)?;
        let pg_type = lookup_builtin_type(column.type_oid)?;
        columns.push(PrimaryKeyColumnSpec {
            column_index,
            typbyval: pg_type.typbyval,
            typlen: pg_type.typlen,
        });
    }

    Some(PrimaryKeySpec { columns })
}

fn primary_key_index_oid(relation: &fastpg_catalog::RelationRecord) -> Option<Oid> {
    if relation.primary_key.is_empty() {
        return None;
    }
    relation
        .oid
        .0
        .checked_add(PRIMARY_KEY_INDEX_OID_OFFSET)
        .map(Oid)
}

fn primary_key_index_name(relation: &fastpg_catalog::RelationRecord) -> String {
    format!("{}_pkey", relation.name)
}

fn primary_key_index_relation(index_oid: Oid) -> Option<fastpg_catalog::RelationRecord> {
    let relation_oid = index_oid.0.checked_sub(PRIMARY_KEY_INDEX_OID_OFFSET)?;
    let relation = relation_by_oid(Oid(relation_oid))?;
    primary_key_index_oid(&relation)
        .is_some_and(|primary_key_index_oid| primary_key_index_oid == index_oid)
        .then_some(relation)
}

fn primary_key_index_relation_by_name(name: &str) -> Option<fastpg_catalog::RelationRecord> {
    let relation_name = name.strip_suffix("_pkey")?;
    let relation = relation_by_name(relation_name)?;
    (!relation.primary_key.is_empty() && primary_key_index_name(&relation) == name)
        .then_some(relation)
}

fn primary_key_column(
    relation: &fastpg_catalog::RelationRecord,
    column_index: usize,
) -> Option<&ColumnRecord> {
    let column_name = relation.primary_key.get(column_index)?;
    relation
        .columns
        .iter()
        .find(|column| &column.name == column_name)
}

fn primary_key_index_info(
    relation: &fastpg_catalog::RelationRecord,
    index_oid: Oid,
) -> Option<FastPgRustPrimaryKeyIndexInfo> {
    let key_count = relation.primary_key.len();
    if key_count == 0 || key_count > FASTPG_MAX_INDEX_KEYS {
        return None;
    }

    let mut attnums = [0i16; FASTPG_MAX_INDEX_KEYS];
    let mut type_oids = [0u32; FASTPG_MAX_INDEX_KEYS];
    let mut collation_oids = [0u32; FASTPG_MAX_INDEX_KEYS];
    for (key_index, primary_key_column) in relation.primary_key.iter().enumerate() {
        let column_index = relation
            .columns
            .iter()
            .position(|column| &column.name == primary_key_column)?;
        let column = relation.columns.get(column_index)?;
        let type_record = lookup_builtin_type(column.type_oid)?;
        attnums[key_index] = (column_index + 1).try_into().ok()?;
        type_oids[key_index] = column.type_oid.0;
        collation_oids[key_index] = type_record.typcollation.0;
    }

    Some(FastPgRustPrimaryKeyIndexInfo {
        index_oid: index_oid.0,
        heap_oid: relation.oid.0,
        key_count: key_count as u16,
        _padding: [0; 2],
        attnums,
        type_oids,
        collation_oids,
    })
}

fn primary_key_for_row(primary_key_spec: &PrimaryKeySpec, row: &Row) -> Option<IndexKey> {
    let mut parts = Vec::with_capacity(primary_key_spec.columns.len());

    for column in &primary_key_spec.columns {
        let cell = row.cells.get(column.column_index)?;
        parts.push(index_key_part(column, cell)?);
    }

    Some(IndexKey(parts))
}

fn primary_key_for_datums(
    primary_key_spec: &PrimaryKeySpec,
    values: &[usize],
    is_null: &[u8],
) -> Option<IndexKey> {
    if primary_key_spec.columns.len() != values.len() || values.len() != is_null.len() {
        return None;
    }

    let mut parts = Vec::with_capacity(primary_key_spec.columns.len());
    for (key_index, column) in primary_key_spec.columns.iter().enumerate() {
        let cell = Cell {
            value: values[key_index],
            is_null: is_null[key_index] != 0,
        };
        parts.push(index_key_part(column, &cell)?);
    }

    Some(IndexKey(parts))
}

fn index_key_part(column: &PrimaryKeyColumnSpec, cell: &Cell) -> Option<IndexKeyPart> {
    if cell.is_null {
        return Some(IndexKeyPart::Null);
    }

    if column.typbyval {
        return Some(IndexKeyPart::ByValue(cell.value));
    }

    let bytes = byref_key_bytes(cell.value, column.typlen)?;
    Some(IndexKeyPart::Bytes(bytes))
}

fn byref_key_bytes(value: usize, typlen: i16) -> Option<Vec<u8>> {
    if value == 0 {
        return None;
    }

    let len = if typlen > 0 {
        typlen as usize
    } else if typlen == -1 {
        varlena_payload_len(value)?
    } else if typlen == -2 {
        c_string_payload_len(value)?
    } else {
        return None;
    };

    let bytes = unsafe { slice::from_raw_parts(value as *const u8, len) };
    Some(bytes.to_vec())
}

fn varlena_payload_len(value: usize) -> Option<usize> {
    let raw = unsafe { std::ptr::read_unaligned(value as *const u32) };
    let len = if cfg!(target_endian = "little") {
        (raw >> 2) as usize
    } else {
        raw as usize
    };
    (len >= 4).then_some(len)
}

fn c_string_payload_len(value: usize) -> Option<usize> {
    let mut len = 0usize;
    loop {
        let byte = unsafe { std::ptr::read((value as *const u8).add(len)) };
        len = len.checked_add(1)?;
        if byte == 0 {
            return Some(len);
        }
    }
}

pub fn copy_text_line(table: &str, line: &str) -> Result<bool, String> {
    let line = line.trim_end_matches('\n').trim_end_matches('\r');
    if line == "\\." {
        return Ok(false);
    }

    let relation = relation_by_name(table)
        .ok_or_else(|| format!("relation \"{}\" does not exist", table.trim()))?;
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.len() != relation.columns.len() {
        return Err(format!(
            "COPY row for relation \"{}\" has {} fields but {} columns",
            relation.name,
            fields.len(),
            relation.columns.len()
        ));
    }

    let mut values = Vec::with_capacity(relation.columns.len());
    let mut is_null = Vec::with_capacity(relation.columns.len());
    let mut byval = Vec::with_capacity(relation.columns.len());
    let mut value_lens = Vec::with_capacity(relation.columns.len());
    let mut varlena_payloads = Vec::<Box<[u8]>>::new();

    for (field, column) in fields.iter().zip(&relation.columns) {
        if *field == "\\N" {
            values.push(0);
            is_null.push(1);
            byval.push(0);
            value_lens.push(0);
            continue;
        }

        let copy_value = copy_text_field_to_datum(field, column.type_oid)?;
        values.push(copy_value.value);
        is_null.push(0);
        byval.push(u8::from(copy_value.byval));
        value_lens.push(copy_value.value_len);
        if let Some(payload) = copy_value.payload {
            let pointer = payload.as_ptr() as usize;
            values.pop();
            values.push(pointer);
            varlena_payloads.push(payload);
        }
    }

    let mut row_id = 0u64;
    let inserted = unsafe {
        fastpg_rust_relation_insert(
            relation.oid.0,
            values.as_ptr(),
            is_null.as_ptr(),
            byval.as_ptr(),
            value_lens.as_ptr(),
            relation.columns.len(),
            &mut row_id,
        )
    };
    if inserted {
        Ok(true)
    } else {
        Err(format!(
            "failed to insert COPY row into \"{}\"",
            relation.name
        ))
    }
}

struct CopyDatum {
    value: usize,
    byval: bool,
    value_len: usize,
    payload: Option<Box<[u8]>>,
}

fn copy_text_field_to_datum(field: &str, type_oid: Oid) -> Result<CopyDatum, String> {
    match type_oid {
        INT2_OID => field
            .parse::<i16>()
            .map(|value| CopyDatum {
                value: value as usize,
                byval: true,
                value_len: 0,
                payload: None,
            })
            .map_err(|error| format!("invalid int2 literal {field:?}: {error}")),
        INT4_OID => field
            .parse::<i32>()
            .map(|value| CopyDatum {
                value: value as usize,
                byval: true,
                value_len: 0,
                payload: None,
            })
            .map_err(|error| format!("invalid int4 literal {field:?}: {error}")),
        INT8_OID => field
            .parse::<i64>()
            .map(|value| CopyDatum {
                value: value as usize,
                byval: true,
                value_len: 0,
                payload: None,
            })
            .map_err(|error| format!("invalid int8 literal {field:?}: {error}")),
        OID_OID => field
            .parse::<u32>()
            .map(|value| CopyDatum {
                value: value as usize,
                byval: true,
                value_len: 0,
                payload: None,
            })
            .map_err(|error| format!("invalid oid literal {field:?}: {error}")),
        TEXT_OID | BPCHAR_OID | VARCHAR_OID => {
            let decoded = decode_copy_text_field(field);
            let payload = postgres_text_payload(decoded.as_bytes());
            Ok(CopyDatum {
                value: 0,
                byval: false,
                value_len: payload.len(),
                payload: Some(payload),
            })
        }
        TIMESTAMP_OID => Ok(CopyDatum {
            value: 0,
            byval: true,
            value_len: 0,
            payload: None,
        }),
        other => Err(format!("COPY does not support type OID {}", other.0)),
    }
}

fn postgres_text_payload(value: &[u8]) -> Box<[u8]> {
    let len = (value.len() + 4) as u32;
    let header = if cfg!(target_endian = "little") {
        len << 2
    } else {
        len
    };
    let mut payload = Vec::with_capacity(value.len() + 4);
    payload.extend_from_slice(&header.to_ne_bytes());
    payload.extend_from_slice(value);
    payload.into_boxed_slice()
}

fn decode_copy_text_field(field: &str) -> String {
    let mut decoded = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            decoded.push(ch);
            continue;
        }

        match chars.next() {
            Some('b') => decoded.push('\u{0008}'),
            Some('f') => decoded.push('\u{000c}'),
            Some('n') => decoded.push('\n'),
            Some('r') => decoded.push('\r'),
            Some('t') => decoded.push('\t'),
            Some('\\') => decoded.push('\\'),
            Some(other) => decoded.push(other),
            None => decoded.push('\\'),
        }
    }
    decoded
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_catalog_type_by_oid(
    oid: u32,
    out: *mut FastPgRustCatalogType,
) -> bool {
    let Some(record) = lookup_builtin_type(Oid(oid)) else {
        return false;
    };

    if out.is_null() {
        return false;
    }

    unsafe {
        *out = FastPgRustCatalogType {
            oid: record.oid.0,
            namespace_oid: record.namespace.0,
            owner_oid: record.owner.0,
            name: fixed_c_name(record.name),
            typlen: record.typlen,
            typbyval: u8::from(record.typbyval),
            typalign: record.typalign,
            typdelim: record.typdelim,
            _padding: 0,
            typinput: record.typinput.0,
            typoutput: record.typoutput.0,
            typreceive: record.typreceive.0,
            typsend: record.typsend.0,
            typmodin: record.typmodin.0,
            typmodout: record.typmodout.0,
            typisdefined: u8::from(record.typisdefined),
            typtype: record.typtype,
            typcategory: record.typcategory,
            typispreferred: u8::from(record.typispreferred),
            typrelid: record.typrelid.0,
            typelem: record.typelem.0,
            typarray: record.typarray.0,
            typbasetype: record.typbasetype.0,
            typtypmod: record.typtypmod,
            typcollation: record.typcollation.0,
            typsubscript: record.typsubscript.0,
            typstorage: record.typstorage,
            _trailing_padding: [0; 3],
        };
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_catalog_type_by_name(
    name: *const c_char,
    namespace_oid: u32,
    out: *mut FastPgRustCatalogType,
) -> bool {
    if out.is_null() {
        return false;
    }
    let Ok(name) = (unsafe { c_str_to_string(name) }) else {
        return false;
    };
    let Some(record) = builtin_type_by_name(&name, Oid(namespace_oid)) else {
        return false;
    };

    unsafe {
        *out = FastPgRustCatalogType {
            oid: record.oid.0,
            namespace_oid: record.namespace.0,
            owner_oid: record.owner.0,
            name: fixed_c_name(record.name),
            typlen: record.typlen,
            typbyval: u8::from(record.typbyval),
            typalign: record.typalign,
            typdelim: record.typdelim,
            _padding: 0,
            typinput: record.typinput.0,
            typoutput: record.typoutput.0,
            typreceive: record.typreceive.0,
            typsend: record.typsend.0,
            typmodin: record.typmodin.0,
            typmodout: record.typmodout.0,
            typisdefined: u8::from(record.typisdefined),
            typtype: record.typtype,
            typcategory: record.typcategory,
            typispreferred: u8::from(record.typispreferred),
            typrelid: record.typrelid.0,
            typelem: record.typelem.0,
            typarray: record.typarray.0,
            typbasetype: record.typbasetype.0,
            typtypmod: record.typtypmod,
            typcollation: record.typcollation.0,
            typsubscript: record.typsubscript.0,
            typstorage: record.typstorage,
            _trailing_padding: [0; 3],
        };
    }
    true
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_catalog_policy_by_relation_oid(relation_oid: u32) -> u8 {
    virtual_catalog_by_relation_oid(Oid(relation_oid))
        .map(|record| record.policy.code())
        .unwrap_or(0)
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_catalog_relation_oid_by_name(
    name: *const c_char,
    namespace_oid: u32,
    oid_out: *mut u32,
) -> bool {
    if oid_out.is_null() {
        return false;
    }
    let Ok(name) = (unsafe { c_str_to_string(name) }) else {
        return false;
    };
    if let Some(relation) = virtual_catalog_by_name(&name, Oid(namespace_oid)) {
        unsafe {
            *oid_out = relation.relation_oid.0;
        }
        return true;
    }
    if let Some(relation) = primary_key_index_relation_by_name(&name) {
        if relation.namespace.0 != namespace_oid {
            return false;
        }
        let Some(index_oid) = primary_key_index_oid(&relation) else {
            return false;
        };
        unsafe {
            *oid_out = index_oid.0;
        }
        return true;
    }
    let Some(relation) = relation_by_name(&name) else {
        return false;
    };
    if relation.namespace.0 != namespace_oid {
        return false;
    }

    unsafe {
        *oid_out = relation.oid.0;
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_catalog_relation_by_oid(
    oid: u32,
    out: *mut FastPgRustCatalogRelation,
) -> bool {
    if out.is_null() {
        return false;
    }

    unsafe {
        if let Some(relation) = relation_by_oid(Oid(oid)) {
            *out = relation_to_ffi(&relation);
            return true;
        }
        if let Some(relation) = primary_key_index_relation(Oid(oid))
            && let Some(index_oid) = primary_key_index_oid(&relation)
        {
            *out = primary_key_index_to_ffi(&relation, index_oid);
            return true;
        }
        if let Some(relation) = virtual_catalog_by_relation_oid(Oid(oid)) {
            *out = virtual_catalog_relation_to_ffi(relation);
            return true;
        }
    }
    false
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_catalog_relation_column_by_index(
    relation_oid: u32,
    column_index: usize,
    out: *mut FastPgRustCatalogColumn,
) -> bool {
    if out.is_null() {
        return false;
    }
    let relation = if let Some(relation) = relation_by_oid(Oid(relation_oid)) {
        relation
    } else if let Some(relation) = primary_key_index_relation(Oid(relation_oid)) {
        let Some(column) = primary_key_column(&relation, column_index) else {
            return false;
        };
        unsafe {
            *out = column_to_ffi(column);
        }
        return true;
    } else if let Some(table) = static_catalog_by_relation_oid(Oid(relation_oid)) {
        let Some(column) = table.columns.get(column_index) else {
            return false;
        };
        unsafe {
            *out = static_catalog_column_to_ffi(column);
        }
        return true;
    } else {
        return false;
    };
    let Some(column) = relation.columns.get(column_index) else {
        return false;
    };

    unsafe {
        *out = column_to_ffi(column);
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_catalog_primary_key_index_info(
    index_oid: u32,
    out: *mut FastPgRustPrimaryKeyIndexInfo,
) -> bool {
    if out.is_null() {
        return false;
    }
    let Some(index_info) = cached_primary_key_index_info(Oid(index_oid)) else {
        return false;
    };
    unsafe {
        *out = index_info;
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_catalog_primary_key_index_oid(
    relation_oid: u32,
    oid_out: *mut u32,
) -> bool {
    if oid_out.is_null() {
        return false;
    }
    let Some(relation) = relation_by_oid(Oid(relation_oid)) else {
        return false;
    };
    let Some(index_oid) = primary_key_index_oid(&relation) else {
        return false;
    };
    unsafe {
        *oid_out = index_oid.0;
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_catalog_namespace_by_oid(
    oid: u32,
    out: *mut FastPgRustCatalogNamespace,
) -> bool {
    if out.is_null() {
        return false;
    }
    let Some(record) = builtin_namespace_by_oid(Oid(oid)) else {
        return false;
    };

    unsafe {
        *out = namespace_to_ffi(record);
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_catalog_namespace_by_name(
    name: *const c_char,
    out: *mut FastPgRustCatalogNamespace,
) -> bool {
    if out.is_null() {
        return false;
    }
    let Ok(name) = (unsafe { c_str_to_string(name) }) else {
        return false;
    };
    let Some(record) = builtin_namespace_by_name(&name) else {
        return false;
    };

    unsafe {
        *out = namespace_to_ffi(record);
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_catalog_proc_by_oid(
    oid: u32,
    out: *mut FastPgRustCatalogProc,
) -> bool {
    if out.is_null() {
        return false;
    }
    let Some(record) = builtin_proc_by_oid(Oid(oid)) else {
        return false;
    };
    let Some(ffi_record) = proc_to_ffi(record) else {
        return false;
    };

    unsafe {
        *out = ffi_record;
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_catalog_proc_count_by_name(name: *const c_char) -> usize {
    let Ok(name) = (unsafe { c_str_to_string(name) }) else {
        return 0;
    };
    builtin_procs_by_name(&name).count()
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_catalog_proc_by_name_index(
    name: *const c_char,
    index: usize,
    out: *mut FastPgRustCatalogProc,
) -> bool {
    if out.is_null() {
        return false;
    }
    let Ok(name) = (unsafe { c_str_to_string(name) }) else {
        return false;
    };
    let Some(record) = builtin_procs_by_name(&name).nth(index) else {
        return false;
    };
    let Some(ffi_record) = proc_to_ffi(record) else {
        return false;
    };

    unsafe {
        *out = ffi_record;
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_catalog_aggregate_by_proc_oid(
    function_oid: u32,
    out: *mut FastPgRustCatalogAggregate,
) -> bool {
    if out.is_null() {
        return false;
    }
    let Some(record) = builtin_aggregate_by_proc_oid(Oid(function_oid)) else {
        return false;
    };

    unsafe {
        *out = aggregate_to_ffi(record);
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_catalog_aggregate_init_value(
    function_oid: u32,
    moving: bool,
    out: *mut c_char,
    out_len: usize,
) -> bool {
    let Some(record) = builtin_aggregate_by_proc_oid(Oid(function_oid)) else {
        return false;
    };
    let value = if moving {
        record.moving_init_value
    } else {
        record.init_value
    };
    let Some(value) = value else {
        return false;
    };
    unsafe {
        write_c_output(out, out_len, value);
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_catalog_operator_by_oid(
    oid: u32,
    out: *mut FastPgRustCatalogOperator,
) -> bool {
    if out.is_null() {
        return false;
    }
    let Some(record) = builtin_operator_by_oid(Oid(oid)) else {
        return false;
    };

    unsafe {
        *out = operator_to_ffi(record);
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_catalog_operator_by_signature(
    name: *const c_char,
    left_type_oid: u32,
    right_type_oid: u32,
    namespace_oid: u32,
    out: *mut FastPgRustCatalogOperator,
) -> bool {
    if out.is_null() {
        return false;
    }
    let Ok(name) = (unsafe { c_str_to_string(name) }) else {
        return false;
    };
    let Some(record) = builtin_operator_by_signature(
        &name,
        Oid(left_type_oid),
        Oid(right_type_oid),
        Oid(namespace_oid),
    ) else {
        return false;
    };

    unsafe {
        *out = operator_to_ffi(record);
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass a valid, null-terminated string pointer for `name`.
pub unsafe extern "C" fn fastpg_rust_catalog_operator_count_by_name(name: *const c_char) -> usize {
    let Ok(name) = (unsafe { c_str_to_string(name) }) else {
        return 0;
    };
    builtin_operators_by_name(&name).count()
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass a valid, null-terminated string pointer for `name` and a
/// valid output pointer for `out`.
pub unsafe extern "C" fn fastpg_rust_catalog_operator_by_name_index(
    name: *const c_char,
    index: usize,
    out: *mut FastPgRustCatalogOperator,
) -> bool {
    if out.is_null() {
        return false;
    }
    let Ok(name) = (unsafe { c_str_to_string(name) }) else {
        return false;
    };
    let Some(record) = builtin_operators_by_name(&name).nth(index) else {
        return false;
    };

    unsafe {
        *out = operator_to_ffi(record);
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass a valid output pointer for `out`.
pub unsafe extern "C" fn fastpg_rust_catalog_cast_by_source_target(
    source_type_oid: u32,
    target_type_oid: u32,
    out: *mut FastPgRustCatalogCast,
) -> bool {
    if out.is_null() {
        return false;
    }
    let Some(record) = builtin_cast_by_source_target(Oid(source_type_oid), Oid(target_type_oid))
    else {
        return false;
    };

    unsafe {
        *out = cast_to_ffi(record);
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass a valid output pointer for `out`.
pub unsafe extern "C" fn fastpg_rust_catalog_opclass_by_oid(
    oid: u32,
    out: *mut FastPgRustCatalogOpclass,
) -> bool {
    if out.is_null() {
        return false;
    }
    let Some(record) = opclass_by_oid(Oid(oid)) else {
        return false;
    };
    unsafe {
        *out = record;
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass a valid, null-terminated string pointer for `name` and a
/// valid output pointer for `out`.
pub unsafe extern "C" fn fastpg_rust_catalog_opclass_by_name(
    method_oid: u32,
    name: *const c_char,
    namespace_oid: u32,
    out: *mut FastPgRustCatalogOpclass,
) -> bool {
    if out.is_null() {
        return false;
    }
    let Ok(name) = (unsafe { c_str_to_string(name) }) else {
        return false;
    };
    let Some(record) = opclass_by_name(Oid(method_oid), &name, Oid(namespace_oid)) else {
        return false;
    };
    unsafe {
        *out = record;
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass a valid output pointer for `oid_out`.
pub unsafe extern "C" fn fastpg_rust_catalog_btree_opclass_for_type(
    type_oid: u32,
    oid_out: *mut u32,
) -> bool {
    if oid_out.is_null() {
        return false;
    }
    let Some(opclass_oid) = catalog_btree_opclass_for_type(Oid(type_oid)) else {
        return false;
    };
    unsafe {
        *oid_out = opclass_oid.0;
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_catalog_create_relation(
    name: *const c_char,
    column_names: *const *const c_char,
    type_oids: *const u32,
    type_mods: *const i32,
    not_nulls: *const u8,
    column_count: usize,
    if_not_exists: bool,
    sqlstate_out: *mut c_char,
    sqlstate_len: usize,
    message_out: *mut c_char,
    message_len: usize,
) -> bool {
    let result = (|| {
        let name = unsafe { c_str_to_string(name) }.map_err(invalid_ffi_argument)?;
        let column_names = unsafe { c_str_array_to_strings(column_names, column_count) }
            .map_err(invalid_ffi_argument)?;
        let type_oids =
            unsafe { u32_array(type_oids, column_count) }.map_err(invalid_ffi_argument)?;
        let type_mods =
            unsafe { i32_array(type_mods, column_count) }.map_err(invalid_ffi_argument)?;
        let not_nulls =
            unsafe { u8_array(not_nulls, column_count) }.map_err(invalid_ffi_argument)?;
        let columns = column_names
            .into_iter()
            .enumerate()
            .map(|(index, name)| {
                ColumnRecord::new(
                    name,
                    Oid(type_oids[index]),
                    type_mods[index],
                    not_nulls[index] != 0,
                )
            })
            .collect::<Vec<_>>();
        let created = create_relation(&name, columns, if_not_exists)?;
        if created.is_some() {
            clear_primary_key_index_info_cache();
        }
        Ok::<(), CatalogError>(())
    })();

    match result {
        Ok(()) => true,
        Err(error) => {
            unsafe {
                write_catalog_error(
                    sqlstate_out,
                    sqlstate_len,
                    message_out,
                    message_len,
                    &error.sqlstate,
                    &error.message,
                );
            }
            false
        }
    }
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_catalog_drop_relation(
    name: *const c_char,
    missing_ok: bool,
    sqlstate_out: *mut c_char,
    sqlstate_len: usize,
    message_out: *mut c_char,
    message_len: usize,
) -> bool {
    let result = (|| {
        let name = unsafe { c_str_to_string(name) }.map_err(invalid_ffi_argument)?;
        let dropped = drop_relation(&name, missing_ok)?;
        if let Some(relation) = dropped {
            clear_primary_key_index_info_cache();
            with_storage(|state, session| state.clear_relation(session, relation.oid.0));
        }
        Ok::<(), fastpg_catalog::CatalogError>(())
    })();

    match result {
        Ok(()) => true,
        Err(error) => {
            unsafe {
                write_catalog_error(
                    sqlstate_out,
                    sqlstate_len,
                    message_out,
                    message_len,
                    &error.sqlstate,
                    &error.message,
                );
            }
            false
        }
    }
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_catalog_truncate_relation(
    name: *const c_char,
    sqlstate_out: *mut c_char,
    sqlstate_len: usize,
    message_out: *mut c_char,
    message_len: usize,
) -> bool {
    let result = (|| {
        let name = unsafe { c_str_to_string(name) }.map_err(invalid_ffi_argument)?;
        let relation = truncate_relation(&name)?;
        with_storage(|state, session| state.clear_relation(session, relation.oid.0));
        Ok::<(), fastpg_catalog::CatalogError>(())
    })();

    match result {
        Ok(()) => true,
        Err(error) => {
            unsafe {
                write_catalog_error(
                    sqlstate_out,
                    sqlstate_len,
                    message_out,
                    message_len,
                    &error.sqlstate,
                    &error.message,
                );
            }
            false
        }
    }
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_catalog_relation_column_count(
    name: *const c_char,
    count_out: *mut usize,
    sqlstate_out: *mut c_char,
    sqlstate_len: usize,
    message_out: *mut c_char,
    message_len: usize,
) -> bool {
    let result = (|| {
        let name = unsafe { c_str_to_string(name) }.map_err(invalid_ffi_argument)?;
        let count = relation_column_count(&name)?;
        if count_out.is_null() {
            return Err(CatalogError::new(
                "22023",
                "null column count output pointer",
            ));
        }
        unsafe {
            *count_out = count;
        }
        Ok::<(), CatalogError>(())
    })();

    match result {
        Ok(()) => true,
        Err(error) => {
            unsafe {
                write_catalog_error(
                    sqlstate_out,
                    sqlstate_len,
                    message_out,
                    message_len,
                    &error.sqlstate,
                    &error.message,
                );
            }
            false
        }
    }
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_catalog_add_primary_key(
    name: *const c_char,
    column_names: *const *const c_char,
    column_count: usize,
    sqlstate_out: *mut c_char,
    sqlstate_len: usize,
    message_out: *mut c_char,
    message_len: usize,
) -> bool {
    let result = (|| {
        let name = unsafe { c_str_to_string(name) }.map_err(invalid_ffi_argument)?;
        let column_names = unsafe { c_str_array_to_strings(column_names, column_count) }
            .map_err(invalid_ffi_argument)?;
        add_primary_key(&name, column_names)?;
        clear_primary_key_index_info_cache();
        if let Some(relation) = relation_by_name(&name) {
            with_storage(|state, _session| state.rebuild_primary_key_index(relation.oid.0));
        }
        Ok::<(), CatalogError>(())
    })();

    match result {
        Ok(()) => true,
        Err(error) => {
            unsafe {
                write_catalog_error(
                    sqlstate_out,
                    sqlstate_len,
                    message_out,
                    message_len,
                    &error.sqlstate,
                    &error.message,
                );
            }
            false
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_xact_begin() {
    with_storage(|state, session| state.begin_explicit_transaction(session));
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_xact_begin_implicit() {
    with_storage(|_state, session| session.ensure_transaction());
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_xact_commit() {
    with_storage(|state, session| state.commit_explicit_transaction(session));
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_xact_abort() {
    with_storage(|state, session| state.abort_explicit_transaction(session));
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_xact_commit_if_implicit() {
    commit_implicit_transaction();
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_xact_abort_if_implicit() {
    abort_implicit_transaction();
}

pub fn commit_implicit_transaction() {
    with_storage(|state, session| state.commit_implicit_transaction(session));
}

pub fn abort_implicit_transaction() {
    with_storage(|state, session| state.abort_implicit_transaction(session));
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_xact_is_explicit() -> bool {
    with_storage(|state, session| state.is_explicit_transaction(session))
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_subxact_begin() {
    with_storage(|_state, session| {
        session.ensure_transaction();
        session
            .transaction_stack
            .push(TransactionOverlay::default());
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_subxact_commit() {
    with_storage(|state, session| {
        if session.transaction_stack.len() > 1 {
            state.commit_top_overlay(session);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_subxact_abort() {
    with_storage(|_state, session| {
        if session.transaction_stack.len() > 1 {
            session.transaction_stack.pop();
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_relation_clear(relid: u32) {
    with_storage(|state, session| state.clear_relation(session, relid));
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_relation_row_count(relid: u32) -> usize {
    with_storage(|state, session| state.visible_row_count(session, relid))
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_relation_contains_row(relid: u32, row_id: u64) -> bool {
    with_storage(|state, session| state.find_visible_row(session, relid, row_id).is_some())
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_primary_key_index_lookup(
    index_relid: u32,
    values: *const usize,
    is_null: *const u8,
    nkeys: usize,
    row_id_out: *mut u64,
) -> bool {
    if nkeys > 0 && (values.is_null() || is_null.is_null()) {
        return false;
    }
    let Some(relation) = primary_key_index_relation(Oid(index_relid)) else {
        return false;
    };

    let values = if nkeys == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(values, nkeys) }
    };
    let is_null = if nkeys == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(is_null, nkeys) }
    };
    let row = with_storage(|state, session| {
        let primary_key_spec = state.primary_key_spec(relation.oid.0)?;
        let key = primary_key_for_datums(&primary_key_spec, values, is_null)?;
        Some(state.find_visible_row_by_primary_key(
            session,
            relation.oid.0,
            &primary_key_spec,
            &key,
        ))
    })
    .flatten();
    let Some(row) = row else {
        return false;
    };
    if !row_id_out.is_null() {
        unsafe {
            *row_id_out = row.row_id;
        }
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_relation_insert(
    relid: u32,
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
    row_id_out: *mut u64,
) -> bool {
    let Some((values, is_null, byval, value_lens)) =
        (unsafe { row_input_arrays(values, is_null, byval, value_lens, natts) })
    else {
        return false;
    };

    with_storage(|state, session| {
        let row_id = match state.relations.entry(relid).or_default().allocate_row_id() {
            Some(row_id) => row_id,
            None => return false,
        };

        session.ensure_transaction();
        let cells = {
            let segment = session
                .transaction_stack
                .last_mut()
                .expect("transaction was just ensured")
                .relations
                .entry(relid)
                .or_default();

            match copy_cells_to_segment(segment, values, is_null, byval, value_lens) {
                Some(cells) => cells,
                None => return false,
            }
        };

        let row = Row { row_id, cells };
        if state.has_primary_key_conflict(session, relid, &row, None) {
            return false;
        }

        let segment = session
            .transaction_stack
            .last_mut()
            .expect("transaction was just ensured")
            .relations
            .entry(relid)
            .or_default();
        segment.rows.push(row);
        if !row_id_out.is_null() {
            unsafe {
                *row_id_out = row_id;
            }
        }
        true
    })
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_relation_update(
    relid: u32,
    row_id: u64,
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
) -> bool {
    if row_id == 0 {
        return false;
    }
    let Some((values, is_null, byval, value_lens)) =
        (unsafe { row_input_arrays(values, is_null, byval, value_lens, natts) })
    else {
        return false;
    };

    with_storage(|state, session| {
        if state.find_visible_row(session, relid, row_id).is_none() {
            return false;
        }

        session.ensure_transaction();
        let cells = {
            let overlay = session
                .transaction_stack
                .last_mut()
                .expect("transaction was just ensured");
            let segment = overlay.relations.entry(relid).or_default();
            match copy_cells_to_segment(segment, values, is_null, byval, value_lens) {
                Some(cells) => cells,
                None => return false,
            }
        };
        let row = Row { row_id, cells };
        if state.has_primary_key_conflict(session, relid, &row, Some(row_id)) {
            return false;
        }

        let overlay = session
            .transaction_stack
            .last_mut()
            .expect("transaction was just ensured");
        overlay
            .deleted_row_ids
            .entry(relid)
            .or_default()
            .insert(row_id);
        let segment = overlay.relations.entry(relid).or_default();
        segment.rows.retain(|row| row.row_id != row_id);
        segment.rows.push(row);
        true
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_relation_delete(relid: u32, row_id: u64) -> bool {
    if row_id == 0 {
        return false;
    }

    with_storage(|state, session| {
        if state.find_visible_row(session, relid, row_id).is_none() {
            return false;
        }

        session.ensure_transaction();
        let overlay = session
            .transaction_stack
            .last_mut()
            .expect("transaction was just ensured");
        overlay
            .deleted_row_ids
            .entry(relid)
            .or_default()
            .insert(row_id);
        if let Some(segment) = overlay.relations.get_mut(&relid) {
            segment.rows.retain(|row| row.row_id != row_id);
        }
        true
    })
}

type RowInputArrays<'a> = (&'a [usize], &'a [u8], &'a [u8], &'a [usize]);

unsafe fn row_input_arrays<'a>(
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
) -> Option<RowInputArrays<'a>> {
    if natts > 0
        && (values.is_null() || is_null.is_null() || byval.is_null() || value_lens.is_null())
    {
        return None;
    }

    let values = if natts == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(values, natts) }
    };
    let is_null = if natts == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(is_null, natts) }
    };
    let byval = if natts == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(byval, natts) }
    };
    let value_lens = if natts == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(value_lens, natts) }
    };

    Some((values, is_null, byval, value_lens))
}

fn copy_cells_to_segment(
    segment: &mut RowSegment,
    values: &[usize],
    is_null: &[u8],
    byval: &[u8],
    value_lens: &[usize],
) -> Option<Vec<Cell>> {
    let mut cells = Vec::with_capacity(values.len());
    for index in 0..values.len() {
        if is_null[index] != 0 {
            cells.push(Cell {
                value: 0,
                is_null: true,
            });
            continue;
        }

        if byval[index] != 0 {
            cells.push(Cell {
                value: values[index],
                is_null: false,
            });
            continue;
        }

        let len = value_lens[index];
        if values[index] == 0 && len > 0 {
            return None;
        }
        let bytes = if len == 0 {
            Vec::new().into_boxed_slice()
        } else {
            let source = unsafe { slice::from_raw_parts(values[index] as *const u8, len) };
            source.to_vec().into_boxed_slice()
        };
        let value = bytes.as_ptr() as usize;
        segment.payloads.push(bytes);
        cells.push(Cell {
            value,
            is_null: false,
        });
    }
    Some(cells)
}

fn datum(value: usize) -> Cell {
    Cell {
        value,
        is_null: false,
    }
}

fn int2_datum(value: i16) -> Cell {
    datum(value as usize)
}

fn int4_datum(value: i32) -> Cell {
    datum(value as usize)
}

fn null_datum() -> Cell {
    Cell {
        value: 0,
        is_null: true,
    }
}

fn bool_datum(value: bool) -> Cell {
    datum(usize::from(value))
}

fn char_datum(value: u8) -> Cell {
    datum(value as usize)
}

fn float4_datum(value: f32) -> Cell {
    datum(value.to_bits() as usize)
}

fn name_datum(value: &str, payloads: &mut Vec<Box<[u8]>>) -> Cell {
    let mut bytes = [0u8; NAMEDATALEN];
    for (index, byte) in value
        .as_bytes()
        .iter()
        .copied()
        .take(NAMEDATALEN - 1)
        .enumerate()
    {
        bytes[index] = byte;
    }
    payloads.push(bytes.to_vec().into_boxed_slice());
    datum(payloads.last().expect("payload was just pushed").as_ptr() as usize)
}

fn varlena_4b_header(size: usize) -> u32 {
    let size = size.min(0x3fff_ffff) as u32;
    #[cfg(target_endian = "little")]
    {
        size << 2
    }
    #[cfg(target_endian = "big")]
    {
        size
    }
}

fn push_i32_ne(bytes: &mut Vec<u8>, value: i32) {
    bytes.extend_from_slice(&value.to_ne_bytes());
}

fn push_u32_ne(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_ne_bytes());
}

fn push_i16_ne(bytes: &mut Vec<u8>, value: i16) {
    bytes.extend_from_slice(&value.to_ne_bytes());
}

fn int2vector_datum(values: &[i16], payloads: &mut Vec<Box<[u8]>>) -> Cell {
    let total_len = 24 + std::mem::size_of_val(values);
    let mut bytes = Vec::with_capacity(total_len);
    push_u32_ne(&mut bytes, varlena_4b_header(total_len));
    push_i32_ne(&mut bytes, 1);
    push_i32_ne(&mut bytes, 0);
    push_u32_ne(&mut bytes, INT2_OID.0);
    push_i32_ne(&mut bytes, values.len().min(i32::MAX as usize) as i32);
    push_i32_ne(&mut bytes, 0);
    for value in values {
        push_i16_ne(&mut bytes, *value);
    }
    payloads.push(bytes.into_boxed_slice());
    datum(payloads.last().expect("payload was just pushed").as_ptr() as usize)
}

fn oidvector_datum(values: &[u32], payloads: &mut Vec<Box<[u8]>>) -> Cell {
    let total_len = 24 + std::mem::size_of_val(values);
    let mut bytes = Vec::with_capacity(total_len);
    push_u32_ne(&mut bytes, varlena_4b_header(total_len));
    push_i32_ne(&mut bytes, 1);
    push_i32_ne(&mut bytes, 0);
    push_u32_ne(&mut bytes, OID_OID.0);
    push_i32_ne(&mut bytes, values.len().min(i32::MAX as usize) as i32);
    push_i32_ne(&mut bytes, 0);
    for value in values {
        push_u32_ne(&mut bytes, *value);
    }
    payloads.push(bytes.into_boxed_slice());
    datum(payloads.last().expect("payload was just pushed").as_ptr() as usize)
}

fn text_datum(value: &str, payloads: &mut Vec<Box<[u8]>>) -> Cell {
    payloads.push(postgres_text_payload(value.as_bytes()));
    datum(payloads.last().expect("payload was just pushed").as_ptr() as usize)
}

fn catalog_value_to_cell(
    column: &fastpg_catalog::StaticCatalogColumn,
    value: &CatalogValue,
    payloads: &mut Vec<Box<[u8]>>,
) -> Cell {
    match value {
        CatalogValue::Null => null_datum(),
        CatalogValue::Bool(value) => bool_datum(*value),
        CatalogValue::Char(value) => char_datum(*value),
        CatalogValue::Int16(value) => int2_datum(*value),
        CatalogValue::Int32(value) => int4_datum(*value),
        CatalogValue::Float32(value) => float4_datum(*value),
        CatalogValue::Oid(value) => datum(value.0 as usize),
        CatalogValue::Name(value) => name_datum(value, payloads),
        CatalogValue::Text(value) => text_datum(value, payloads),
        CatalogValue::OidVector(values) => {
            let values = values.iter().map(|oid| oid.0).collect::<Vec<_>>();
            oidvector_datum(&values, payloads)
        }
        CatalogValue::Int2Vector(values) => int2vector_datum(values, payloads),
        CatalogValue::Raw(value) => match column.type_oid {
            NAME_OID => name_datum(value, payloads),
            TEXT_OID | PG_NODE_TREE_OID => text_datum(value, payloads),
            OID_OID => value
                .parse::<u32>()
                .map(|value| datum(value as usize))
                .unwrap_or_else(|_| datum(0)),
            INT2_OID => value
                .parse::<i16>()
                .map(int2_datum)
                .unwrap_or_else(|_| int2_datum(0)),
            INT4_OID => value
                .parse::<i32>()
                .map(int4_datum)
                .unwrap_or_else(|_| int4_datum(0)),
            OIDVECTOR_OID => {
                let values = value
                    .split_whitespace()
                    .filter_map(|part| part.parse::<u32>().ok())
                    .collect::<Vec<_>>();
                oidvector_datum(&values, payloads)
            }
            INT2VECTOR_OID => {
                let values = value
                    .split_whitespace()
                    .filter_map(|part| part.parse::<i16>().ok())
                    .collect::<Vec<_>>();
                int2vector_datum(&values, payloads)
            }
            _ => null_datum(),
        },
    }
}

fn catalog_scan_state(relation_oid: Oid) -> Option<ScanState> {
    let table = static_catalog_by_relation_oid(relation_oid)?;
    let mut payloads = Vec::new();
    let rows = catalog_rows(relation_oid)
        .into_iter()
        .filter_map(|catalog_row| {
            if catalog_row.values.len() != table.columns.len() {
                return None;
            }
            Some(Row {
                row_id: catalog_row.row_id,
                cells: table
                    .columns
                    .iter()
                    .zip(catalog_row.values.iter())
                    .map(|(column, value)| catalog_value_to_cell(column, value, &mut payloads))
                    .collect(),
            })
        })
        .collect();
    Some(ScanState {
        rows,
        payloads,
        next_index: 0,
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_scan_begin(relid: u32) -> u64 {
    let virtual_scan =
        virtual_catalog_by_relation_oid(Oid(relid)).and_then(|_| catalog_scan_state(Oid(relid)));

    with_storage(|state, session| {
        let scan = virtual_scan.unwrap_or_else(|| {
            state.relations.entry(relid).or_default();
            ScanState {
                rows: state.visible_rows(session, relid),
                payloads: Vec::new(),
                next_index: 0,
            }
        });
        let handle = session.allocate_scan_handle();
        session.scans.insert(handle, scan);
        handle
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_scan_reset(scan_handle: u64) {
    with_storage(|_state, session| {
        if let Some(scan) = session.scans.get_mut(&scan_handle) {
            scan.next_index = 0;
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_scan_end(scan_handle: u64) {
    with_storage(|_state, session| {
        session.scans.remove(&scan_handle);
    });
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_scan_next(
    scan_handle: u64,
    forward: u8,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
    row_id_out: *mut u64,
) -> bool {
    let row = with_storage(|_state, session| {
        let scan = match session.scans.get_mut(&scan_handle) {
            Some(scan) => scan,
            None => return None,
        };

        let row_count = scan.rows.len();
        let row_index = if forward != 0 {
            if scan.next_index >= row_count {
                return None;
            }
            let row_index = scan.next_index;
            scan.next_index += 1;
            row_index
        } else {
            if scan.next_index == 0 {
                scan.next_index = row_count;
            }
            if scan.next_index == 0 {
                return None;
            }
            scan.next_index -= 1;
            scan.next_index
        };

        scan.rows.get(row_index).cloned()
    });

    match row {
        Some(row) => unsafe {
            copy_row_to_outputs(&row, values_out, is_null_out, natts, row_id_out)
        },
        None => false,
    }
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_fetch_row(
    relid: u32,
    row_id: u64,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
) -> bool {
    let row = with_storage(|state, session| state.find_visible_row(session, relid, row_id));

    match row {
        Some(row) => unsafe {
            copy_row_to_outputs(&row, values_out, is_null_out, natts, std::ptr::null_mut())
        },
        None => false,
    }
}

unsafe fn copy_row_to_outputs(
    row: &Row,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
    row_id_out: *mut u64,
) -> bool {
    if row.cells.len() != natts {
        return false;
    }
    if natts > 0 && (values_out.is_null() || is_null_out.is_null()) {
        return false;
    }

    let values_out = if natts == 0 {
        &mut []
    } else {
        unsafe { slice::from_raw_parts_mut(values_out, natts) }
    };
    let is_null_out = if natts == 0 {
        &mut []
    } else {
        unsafe { slice::from_raw_parts_mut(is_null_out, natts) }
    };
    for (index, cell) in row.cells.iter().enumerate() {
        values_out[index] = cell.value;
        is_null_out[index] = u8::from(cell.is_null);
    }

    if !row_id_out.is_null() {
        unsafe {
            *row_id_out = row.row_id;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastpg_catalog::builtin_namespaces;
    use std::ptr;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Mutex as StdMutex, MutexGuard};

    static NEXT_RELID: AtomicU32 = AtomicU32::new(10_000);
    static TEST_LOCK: StdMutex<()> = StdMutex::new(());

    struct TestGuard {
        _guard: MutexGuard<'static, ()>,
    }

    impl Drop for TestGuard {
        fn drop(&mut self) {
            fastpg_rust_xact_abort();
        }
    }

    fn test_guard() -> TestGuard {
        let guard = TEST_LOCK.lock().expect("test lock poisoned");
        fastpg_rust_xact_abort();
        clear_primary_key_index_info_cache();
        TestGuard { _guard: guard }
    }

    fn next_relid() -> u32 {
        NEXT_RELID.fetch_add(1, Ordering::Relaxed)
    }

    unsafe fn insert_byval(relid: u32, values: &[usize], is_null: &[u8], row_id: &mut u64) -> bool {
        let byval = vec![1u8; values.len()];
        let value_lens = vec![0usize; values.len()];
        unsafe {
            fastpg_rust_relation_insert(
                relid,
                values.as_ptr(),
                is_null.as_ptr(),
                byval.as_ptr(),
                value_lens.as_ptr(),
                values.len(),
                row_id,
            )
        }
    }

    unsafe fn update_byval(relid: u32, row_id: u64, values: &[usize], is_null: &[u8]) -> bool {
        let byval = vec![1u8; values.len()];
        let value_lens = vec![0usize; values.len()];
        unsafe {
            fastpg_rust_relation_update(
                relid,
                row_id,
                values.as_ptr(),
                is_null.as_ptr(),
                byval.as_ptr(),
                value_lens.as_ptr(),
                values.len(),
            )
        }
    }

    unsafe fn fetch_byval(relid: u32, row_id: u64, natts: usize) -> Option<(Vec<usize>, Vec<u8>)> {
        let mut values = vec![0usize; natts];
        let mut nulls = vec![0u8; natts];
        let found = unsafe {
            fastpg_rust_fetch_row(
                relid,
                row_id,
                values.as_mut_ptr(),
                nulls.as_mut_ptr(),
                natts,
            )
        };
        found.then_some((values, nulls))
    }

    #[test]
    fn inserts_fetches_and_scans_rows() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut first_row_id = 0;
        let mut second_row_id = 0;
        let first_values = [11usize, 0];
        let first_nulls = [0u8, 1];
        let second_values = [22usize, 33];
        let second_nulls = [0u8, 0];

        unsafe {
            assert!(insert_byval(
                relid,
                &first_values,
                &first_nulls,
                &mut first_row_id,
            ));
            assert!(insert_byval(
                relid,
                &second_values,
                &second_nulls,
                &mut second_row_id,
            ));
        }

        assert_eq!(first_row_id, 1);
        assert_eq!(second_row_id, 2);
        assert_eq!(fastpg_rust_relation_row_count(relid), 2);

        let mut values = [0usize; 2];
        let mut nulls = [0u8; 2];
        unsafe {
            assert!(fastpg_rust_fetch_row(
                relid,
                second_row_id,
                values.as_mut_ptr(),
                nulls.as_mut_ptr(),
                values.len(),
            ));
        }
        assert_eq!(values, second_values);
        assert_eq!(nulls, second_nulls);

        let scan = fastpg_rust_scan_begin(relid);
        let mut row_id = 0;
        unsafe {
            assert!(fastpg_rust_scan_next(
                scan,
                1,
                values.as_mut_ptr(),
                nulls.as_mut_ptr(),
                values.len(),
                &mut row_id,
            ));
        }
        assert_eq!(row_id, first_row_id);
        assert_eq!(values, first_values);
        assert_eq!(nulls, first_nulls);
        fastpg_rust_scan_end(scan);

        let backward_scan = fastpg_rust_scan_begin(relid);
        unsafe {
            assert!(fastpg_rust_scan_next(
                backward_scan,
                0,
                values.as_mut_ptr(),
                nulls.as_mut_ptr(),
                values.len(),
                &mut row_id,
            ));
        }
        assert_eq!(row_id, second_row_id);
        assert_eq!(values, second_values);
        assert_eq!(nulls, second_nulls);
        fastpg_rust_scan_end(backward_scan);

        fastpg_rust_relation_clear(relid);
        assert_eq!(fastpg_rust_relation_row_count(relid), 0);
    }

    #[test]
    fn zero_column_rows_accept_null_buffers() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut row_id = 0;

        unsafe {
            assert!(fastpg_rust_relation_insert(
                relid,
                ptr::null(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                0,
                &mut row_id,
            ));
        }
        assert_eq!(row_id, 1);

        let scan = fastpg_rust_scan_begin(relid);
        let mut scanned_row_id = 0;
        unsafe {
            assert!(fastpg_rust_scan_next(
                scan,
                1,
                ptr::null_mut(),
                ptr::null_mut(),
                0,
                &mut scanned_row_id,
            ));
        }
        assert_eq!(scanned_row_id, row_id);
        fastpg_rust_scan_end(scan);

        unsafe {
            assert!(fastpg_rust_fetch_row(
                relid,
                row_id,
                ptr::null_mut(),
                ptr::null_mut(),
                0,
            ));
        }
    }

    #[test]
    fn pg_namespace_scan_exposes_builtin_namespaces() {
        const PG_NAMESPACE_RELATION_ID: u32 = 2615;
        const PG_NAMESPACE_ATTRIBUTE_COUNT: usize = 4;

        let _guard = test_guard();
        let scan = fastpg_rust_scan_begin(PG_NAMESPACE_RELATION_ID);
        let mut rows = Vec::new();

        loop {
            let mut values = [0usize; PG_NAMESPACE_ATTRIBUTE_COUNT];
            let mut nulls = [0u8; PG_NAMESPACE_ATTRIBUTE_COUNT];
            let mut row_id = 0;
            let found = unsafe {
                fastpg_rust_scan_next(
                    scan,
                    1,
                    values.as_mut_ptr(),
                    nulls.as_mut_ptr(),
                    values.len(),
                    &mut row_id,
                )
            };
            if !found {
                break;
            }

            let name = unsafe { CStr::from_ptr(values[1] as *const c_char) }
                .to_string_lossy()
                .into_owned();
            rows.push((row_id, values[0] as u32, name, values[2] as u32, nulls[3]));
        }
        fastpg_rust_scan_end(scan);

        let expected = builtin_namespaces()
            .iter()
            .map(|record| {
                (
                    record.oid.0 as u64,
                    record.oid.0,
                    record.name.to_owned(),
                    record.owner.0,
                    1,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(rows, expected);
    }

    #[test]
    fn committed_rows_survive_top_level_commit() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut row_id = 0;

        fastpg_rust_xact_begin();
        unsafe {
            assert!(insert_byval(relid, &[42], &[0], &mut row_id));
        }
        fastpg_rust_xact_commit();

        assert_eq!(fastpg_rust_relation_row_count(relid), 1);
        assert!(fastpg_rust_relation_contains_row(relid, row_id));
    }

    #[test]
    fn implicit_rows_are_committed_before_explicit_rollback() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut row_id = 0;

        unsafe {
            assert!(insert_byval(relid, &[10], &[0], &mut row_id));
        }

        fastpg_rust_xact_begin();
        unsafe {
            assert!(update_byval(relid, row_id, &[20], &[0]));
        }
        fastpg_rust_xact_abort();

        assert_eq!(fastpg_rust_relation_row_count(relid), 1);
        assert_eq!(
            unsafe { fetch_byval(relid, row_id, 1) },
            Some((vec![10], vec![0]))
        );
    }

    #[test]
    fn transaction_overlays_are_session_owned() {
        let _guard = test_guard();
        let relid = next_relid();
        let session_a = new_session_storage();
        let session_b = new_session_storage();
        let mut row_id = 0;

        {
            let _session_guard = enter_session_storage(session_a.clone());
            fastpg_rust_xact_begin();
            unsafe {
                assert!(insert_byval(relid, &[42], &[0], &mut row_id));
            }
            assert!(fastpg_rust_xact_is_explicit());
            assert_eq!(fastpg_rust_relation_row_count(relid), 1);
        }

        {
            let _session_guard = enter_session_storage(session_b.clone());
            assert!(!fastpg_rust_xact_is_explicit());
            assert_eq!(fastpg_rust_relation_row_count(relid), 0);
        }

        {
            let _session_guard = enter_session_storage(session_a);
            assert!(fastpg_rust_xact_is_explicit());
            fastpg_rust_xact_commit();
        }

        {
            let _session_guard = enter_session_storage(session_b);
            assert_eq!(fastpg_rust_relation_row_count(relid), 1);
            assert!(fastpg_rust_relation_contains_row(relid, row_id));
        }
    }

    #[test]
    fn aborted_rows_are_dropped() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut row_id = 0;
        let values = [7usize];
        let nulls = [0u8];

        fastpg_rust_xact_begin();
        unsafe {
            assert!(insert_byval(relid, &values, &nulls, &mut row_id));
        }
        fastpg_rust_xact_abort();

        assert_eq!(fastpg_rust_relation_row_count(relid), 0);
        assert!(!fastpg_rust_relation_contains_row(relid, row_id));
    }

    #[test]
    fn subxact_abort_drops_only_nested_rows() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut parent_row_id = 0;
        let mut nested_row_id = 0;

        fastpg_rust_xact_begin();
        unsafe {
            assert!(insert_byval(relid, &[1], &[0], &mut parent_row_id));
        }
        fastpg_rust_subxact_begin();
        unsafe {
            assert!(insert_byval(relid, &[2], &[0], &mut nested_row_id));
        }
        fastpg_rust_subxact_abort();
        fastpg_rust_xact_commit();

        assert_eq!(fastpg_rust_relation_row_count(relid), 1);
        assert!(fastpg_rust_relation_contains_row(relid, parent_row_id));
        assert!(!fastpg_rust_relation_contains_row(relid, nested_row_id));
    }

    #[test]
    fn updates_shadow_committed_rows_and_rollback_cleanly() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut row_id = 0;

        fastpg_rust_xact_begin();
        unsafe {
            assert!(insert_byval(relid, &[10, 20], &[0, 0], &mut row_id));
        }
        fastpg_rust_xact_commit();

        fastpg_rust_xact_begin();
        unsafe {
            assert!(update_byval(relid, row_id, &[30, 40], &[0, 0]));
        }
        assert_eq!(fastpg_rust_relation_row_count(relid), 1);
        assert_eq!(
            unsafe { fetch_byval(relid, row_id, 2) },
            Some((vec![30, 40], vec![0, 0]))
        );
        fastpg_rust_xact_abort();

        assert_eq!(
            unsafe { fetch_byval(relid, row_id, 2) },
            Some((vec![10, 20], vec![0, 0]))
        );

        fastpg_rust_xact_begin();
        unsafe {
            assert!(update_byval(relid, row_id, &[50, 60], &[0, 0]));
        }
        fastpg_rust_xact_commit();

        assert_eq!(fastpg_rust_relation_row_count(relid), 1);
        assert_eq!(
            unsafe { fetch_byval(relid, row_id, 2) },
            Some((vec![50, 60], vec![0, 0]))
        );
    }

    #[test]
    fn deletes_hide_rows_and_rollback_cleanly() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut row_id = 0;

        fastpg_rust_xact_begin();
        unsafe {
            assert!(insert_byval(relid, &[1], &[0], &mut row_id));
        }
        fastpg_rust_xact_commit();

        fastpg_rust_xact_begin();
        assert!(fastpg_rust_relation_delete(relid, row_id));
        assert_eq!(fastpg_rust_relation_row_count(relid), 0);
        assert!(!fastpg_rust_relation_contains_row(relid, row_id));
        fastpg_rust_xact_abort();

        assert_eq!(fastpg_rust_relation_row_count(relid), 1);
        assert!(fastpg_rust_relation_contains_row(relid, row_id));

        fastpg_rust_xact_begin();
        assert!(fastpg_rust_relation_delete(relid, row_id));
        fastpg_rust_xact_commit();

        assert_eq!(fastpg_rust_relation_row_count(relid), 0);
        assert!(!fastpg_rust_relation_contains_row(relid, row_id));
    }

    #[test]
    fn inserted_then_deleted_overlay_rows_do_not_underflow_counts() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut row_id = 0;

        fastpg_rust_xact_begin();
        unsafe {
            assert!(insert_byval(relid, &[1], &[0], &mut row_id));
        }
        assert_eq!(fastpg_rust_relation_row_count(relid), 1);
        assert!(fastpg_rust_relation_delete(relid, row_id));
        assert_eq!(fastpg_rust_relation_row_count(relid), 0);
        fastpg_rust_xact_commit();

        assert_eq!(fastpg_rust_relation_row_count(relid), 0);
        assert!(!fastpg_rust_relation_contains_row(relid, row_id));
    }

    #[test]
    fn committed_subxact_updates_merge_into_parent_overlay() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut row_id = 0;

        fastpg_rust_xact_begin();
        unsafe {
            assert!(insert_byval(relid, &[1], &[0], &mut row_id));
        }
        fastpg_rust_subxact_begin();
        unsafe {
            assert!(update_byval(relid, row_id, &[2], &[0]));
        }
        fastpg_rust_subxact_commit();
        assert_eq!(
            unsafe { fetch_byval(relid, row_id, 1) },
            Some((vec![2], vec![0]))
        );
        fastpg_rust_xact_commit();

        assert_eq!(fastpg_rust_relation_row_count(relid), 1);
        assert_eq!(
            unsafe { fetch_byval(relid, row_id, 1) },
            Some((vec![2], vec![0]))
        );
    }

    #[test]
    fn byref_values_are_copied_into_rust_storage() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut row_id = 0;
        let mut bytes = b"hello".to_vec();
        let values = [bytes.as_ptr() as usize];
        let nulls = [0u8];
        let byval = [0u8];
        let value_lens = [bytes.len()];

        fastpg_rust_xact_begin();
        unsafe {
            assert!(fastpg_rust_relation_insert(
                relid,
                values.as_ptr(),
                nulls.as_ptr(),
                byval.as_ptr(),
                value_lens.as_ptr(),
                values.len(),
                &mut row_id,
            ));
        }
        fastpg_rust_xact_commit();

        bytes.fill(b'X');

        let mut values_out = [0usize; 1];
        let mut nulls_out = [0u8; 1];
        unsafe {
            assert!(fastpg_rust_fetch_row(
                relid,
                row_id,
                values_out.as_mut_ptr(),
                nulls_out.as_mut_ptr(),
                1,
            ));
            let copied = slice::from_raw_parts(values_out[0] as *const u8, value_lens[0]);
            assert_eq!(copied, b"hello");
        }
        assert_eq!(nulls_out, [0]);
    }

    #[test]
    fn logical_row_ids_exceed_u16_capacity() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut row_id = 0;

        fastpg_rust_xact_begin();
        for value in 1..=70_000usize {
            unsafe {
                assert!(insert_byval(relid, &[value], &[0], &mut row_id));
            }
        }
        assert_eq!(row_id, 70_000);
        assert_eq!(fastpg_rust_relation_row_count(relid), 70_000);

        let mut values = [0usize; 1];
        let mut nulls = [0u8; 1];
        unsafe {
            assert!(fastpg_rust_fetch_row(
                relid,
                row_id,
                values.as_mut_ptr(),
                nulls.as_mut_ptr(),
                1,
            ));
        }
        assert_eq!(values, [70_000]);
        assert_eq!(nulls, [0]);
    }

    #[test]
    fn primary_key_index_enforces_uniqueness_and_tracks_updates() {
        let _guard = test_guard();
        create_relation(
            "pk_storage",
            vec![
                ColumnRecord::new("id", INT4_OID, -1, true),
                ColumnRecord::new("value", INT4_OID, -1, false),
            ],
            false,
        )
        .unwrap()
        .unwrap();
        add_primary_key("pk_storage", vec!["id".to_owned()]).unwrap();
        let relation = relation_by_name("pk_storage").unwrap();
        let index_oid = primary_key_index_oid(&relation).unwrap();
        let relid = relation.oid.0;
        let mut first_row_id = 0;
        let mut second_row_id = 0;
        let mut index_relation = FastPgRustCatalogRelation {
            oid: 0,
            namespace_oid: 0,
            name: [0; NAMEDATALEN],
            column_count: 0,
            relkind: 0,
            has_primary_key: 0,
        };
        let mut index_info = FastPgRustPrimaryKeyIndexInfo {
            index_oid: 0,
            heap_oid: 0,
            key_count: 0,
            _padding: [0; 2],
            attnums: [0; FASTPG_MAX_INDEX_KEYS],
            type_oids: [0; FASTPG_MAX_INDEX_KEYS],
            collation_oids: [0; FASTPG_MAX_INDEX_KEYS],
        };

        unsafe {
            assert!(fastpg_rust_catalog_relation_by_oid(
                index_oid.0,
                &mut index_relation,
            ));
            assert!(fastpg_rust_catalog_primary_key_index_info(
                index_oid.0,
                &mut index_info,
            ));
        }
        assert_eq!(index_relation.relkind, b'i');
        assert_eq!(index_relation.column_count, 1);
        assert_eq!(index_info.heap_oid, relid);
        assert_eq!(index_info.key_count, 1);
        assert_eq!(index_info.attnums[0], 1);
        assert_eq!(index_info.type_oids[0], INT4_OID.0);

        fastpg_rust_xact_begin();
        unsafe {
            assert!(insert_byval(relid, &[1, 10], &[0, 0], &mut first_row_id));
            assert!(insert_byval(relid, &[2, 20], &[0, 0], &mut second_row_id));
            assert!(!insert_byval(relid, &[1, 30], &[0, 0], &mut 0));
        }
        fastpg_rust_xact_commit();

        let mut found_row_id = 0;
        unsafe {
            assert!(fastpg_rust_primary_key_index_lookup(
                index_oid.0,
                [2usize].as_ptr(),
                [0u8].as_ptr(),
                1,
                &mut found_row_id,
            ));
        }
        assert_eq!(found_row_id, second_row_id);

        fastpg_rust_xact_begin();
        unsafe {
            assert!(update_byval(relid, second_row_id, &[3, 40], &[0, 0]));
            assert!(!update_byval(relid, second_row_id, &[1, 50], &[0, 0]));
        }
        fastpg_rust_xact_commit();

        unsafe {
            assert!(!fastpg_rust_primary_key_index_lookup(
                index_oid.0,
                [2usize].as_ptr(),
                [0u8].as_ptr(),
                1,
                &mut found_row_id,
            ));
            assert!(fastpg_rust_primary_key_index_lookup(
                index_oid.0,
                [3usize].as_ptr(),
                [0u8].as_ptr(),
                1,
                &mut found_row_id,
            ));
        }
        assert_eq!(found_row_id, second_row_id);
        assert_eq!(
            unsafe { fetch_byval(relid, second_row_id, 2) },
            Some((vec![3, 40], vec![0, 0]))
        );
    }
}
