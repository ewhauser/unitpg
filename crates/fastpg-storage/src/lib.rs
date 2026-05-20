#![deny(unsafe_op_in_unsafe_fn)]

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::CStr;
use std::os::raw::c_char;
use std::slice;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use fastpg_catalog::{
    ACLITEM_ARRAY_OID, ACLITEM_OID, ANYARRAY_OID, BOOL_OID, BPCHAR_OID, CHAR_ARRAY_OID, CHAR_OID,
    CID_OID, CatalogError, CatalogFilterValue, CatalogRow, CatalogRowFilter, CatalogValue,
    ColumnRecord, FLOAT8_OID, INT2_ARRAY_OID, INT2_OID, INT2VECTOR_OID, INT4_ARRAY_OID, INT4_OID,
    INT8_OID, LSN_OID, NAME_OID, OID_ARRAY_OID, OID_OID, OIDVECTOR_OID, PG_CATALOG_NAMESPACE_OID,
    PG_NODE_TREE_OID, PhysicalColumnRecord, REGCLASS_OID, TEXT_ARRAY_OID, TEXT_OID, TID_OID,
    TIMESTAMP_OID, VARCHAR_OID, XID_OID, btree_opclass_for_type as catalog_btree_opclass_for_type,
    builtin_aggregate_by_proc_oid, builtin_cast_by_source_target, builtin_namespace_by_name,
    builtin_namespace_by_oid, builtin_operator_by_oid, builtin_operator_by_signature,
    builtin_operators_by_name, catalog_row_count, catalog_row_value, catalog_rows,
    catalog_rows_matching_filters, current_generation, delete_catalog_row, enum_oids_by_sort_order,
    has_uncommitted_catalog_changes, index_record_by_index_oid, index_records_for_relation_oid,
    lookup_type, primary_key_index_oid_for_relation_oid, primary_key_relation_oid_for_index_oid,
    relation_by_name, relation_by_name_in_namespace, relation_by_oid, relation_column_by_attnum,
    relation_column_count, relation_oid_by_name_in_namespace, relation_oid_exists,
    relation_oid_for_index_oid, relation_physical_column_by_attnum, relation_planner_stats_by_oid,
    relation_rowtype_oid_by_oid, relation_summary_by_name_in_namespace, relation_summary_by_oid,
    static_catalog_by_name, static_catalog_by_relation_oid, type_by_name,
    unique_index_oids_for_relation_oid, unique_index_records_for_relation_oid, upsert_catalog_row,
    virtual_catalog_by_name, virtual_catalog_by_relation_oid,
};
use fastpg_types::Oid;

const NAMEDATALEN: usize = 64;
const FASTPG_PROC_MAX_ARGS: usize = 8;
const FASTPG_PROC_SOURCE_LEN: usize = 64;
const FASTPG_MAX_INDEX_KEYS: usize = 32;
const ROW_ARENA_DEFAULT_CHUNK_SIZE: usize = 4096;

static NEXT_STORAGE_REGION_ID: AtomicU64 = AtomicU64::new(1);
static STORAGE_ARENA_REWINDS: AtomicU64 = AtomicU64::new(0);
static STORAGE_MEMORY_LIMIT_REJECTIONS: AtomicU64 = AtomicU64::new(0);
const SQLSTATE_PROGRAM_LIMIT_EXCEEDED: &str = "54000";
const DATUM_ALIGNMENT: usize = 8;

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
    pub row_id: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FastPgRustCatalogRelation {
    pub oid: u32,
    pub type_oid: u32,
    pub namespace_oid: u32,
    pub owner_oid: u32,
    pub name: [c_char; NAMEDATALEN],
    pub column_count: u16,
    pub relkind: u8,
    pub has_primary_key: u8,
    pub has_indexes: u8,
    pub row_id: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FastPgRustCatalogColumn {
    pub name: [c_char; NAMEDATALEN],
    pub type_oid: u32,
    pub type_mod: i32,
    pub attcollation: u32,
    pub attlen: i16,
    pub is_not_null: u8,
    pub has_default: u8,
    pub generated: u8,
    pub is_dropped: u8,
    pub attbyval: u8,
    pub attalign: u8,
    pub attstorage: u8,
    pub _padding: u8,
    pub row_id: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FastPgRustPrimaryKeyIndexInfo {
    pub row_id: u64,
    pub index_oid: u32,
    pub heap_oid: u32,
    pub key_count: u16,
    pub is_unique: u8,
    pub is_primary: u8,
    pub nulls_not_distinct: u8,
    pub is_immediate: u8,
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
    is_null: bool,
    datum: StoredDatum,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StoredDatum {
    ByValue(usize),
    ByRef(ValueRef),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ValueRef {
    region_id: StorageRegionId,
    ptr: usize,
    len: usize,
}

impl Cell {
    fn by_value(value: usize) -> Self {
        Self {
            is_null: false,
            datum: StoredDatum::ByValue(value),
        }
    }

    fn by_ref(value_ref: ValueRef) -> Self {
        Self {
            is_null: false,
            datum: StoredDatum::ByRef(value_ref),
        }
    }

    fn null() -> Self {
        Self {
            is_null: true,
            datum: StoredDatum::ByValue(0),
        }
    }

    fn output_value(&self) -> usize {
        match self.datum {
            StoredDatum::ByValue(value) => value,
            StoredDatum::ByRef(value_ref) => value_ref.ptr,
        }
    }

    fn byref_bytes(&self) -> Option<&[u8]> {
        let StoredDatum::ByRef(value_ref) = self.datum else {
            return None;
        };
        if self.is_null {
            return Some(&[]);
        }
        if value_ref.ptr == 0 && value_ref.len > 0 {
            return None;
        }
        Some(if value_ref.len == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(value_ref.ptr as *const u8, value_ref.len) }
        })
    }

    fn copy_into(&self, region: &mut StorageRegion) -> Option<Self> {
        if let Some(bytes) = self.byref_bytes() {
            return Some(Self {
                is_null: self.is_null,
                datum: StoredDatum::ByRef(region.alloc_bytes(bytes)),
            });
        }
        Some(*self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Row {
    row_id: u64,
    cells: Vec<Cell>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct StorageRegionId(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StorageRegionKind {
    Committed,
    Fixture,
    Epoch,
    Transaction,
    Savepoint,
    Scan,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ArenaCheckpoint {
    chunk_count: usize,
    last_chunk_used: usize,
    bytes_allocated: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RegionCheckpoint {
    arena: ArenaCheckpoint,
    accounting: RegionAccounting,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct RowArena {
    chunks: Vec<ArenaChunk>,
    bytes_allocated: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ArenaChunk {
    bytes: Box<[u8]>,
    used: usize,
}

impl ArenaChunk {
    fn new(capacity: usize) -> Self {
        Self {
            bytes: vec![0; capacity].into_boxed_slice(),
            used: 0,
        }
    }

    fn aligned_used(&self) -> usize {
        align_usize(self.used, DATUM_ALIGNMENT)
    }

    fn can_alloc_bytes(&self, len: usize) -> bool {
        self.aligned_used()
            .checked_add(len)
            .is_some_and(|end| end <= self.bytes.len())
    }

    fn alloc_bytes(&mut self, bytes: &[u8]) -> Option<(usize, usize)> {
        let start = self.aligned_used();
        let end = start.checked_add(bytes.len())?;
        if end > self.bytes.len() {
            return None;
        }
        let consumed = end.saturating_sub(self.used);
        self.bytes[start..end].copy_from_slice(bytes);
        self.used = end;
        Some((self.bytes[start..].as_ptr() as usize, consumed))
    }
}

impl RowArena {
    fn alloc_bytes(&mut self, bytes: &[u8]) -> usize {
        if bytes.is_empty() {
            return std::ptr::NonNull::<u8>::dangling().as_ptr() as usize;
        }
        if self
            .chunks
            .last()
            .is_none_or(|chunk| !chunk.can_alloc_bytes(bytes.len()))
        {
            self.chunks.push(ArenaChunk::new(
                ROW_ARENA_DEFAULT_CHUNK_SIZE.max(bytes.len()),
            ));
        }
        let (ptr, consumed) = self
            .chunks
            .last_mut()
            .and_then(|chunk| chunk.alloc_bytes(bytes))
            .expect("arena chunk was sized for allocation");
        self.bytes_allocated = self.bytes_allocated.saturating_add(consumed);
        ptr
    }

    fn checkpoint(&self) -> ArenaCheckpoint {
        ArenaCheckpoint {
            chunk_count: self.chunks.len(),
            last_chunk_used: self.chunks.last().map_or(0, |chunk| chunk.used),
            bytes_allocated: self.bytes_allocated,
        }
    }

    fn rewind_to(&mut self, checkpoint: ArenaCheckpoint) {
        self.chunks.truncate(checkpoint.chunk_count);
        if let Some(chunk) = self.chunks.last_mut() {
            chunk.used = checkpoint.last_chunk_used.min(chunk.bytes.len());
        }
        self.bytes_allocated = checkpoint.bytes_allocated;
    }

    fn append(&mut self, other: &mut RowArena) {
        self.bytes_allocated = self.bytes_allocated.saturating_add(other.bytes_allocated);
        self.chunks.append(&mut other.chunks);
        other.bytes_allocated = 0;
    }
}

fn align_usize(value: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (value + (align - 1)) & !(align - 1)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct RegionAccounting {
    rows: usize,
    row_bytes: usize,
    byref_bytes: usize,
    index_bytes: usize,
    overhead_bytes: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct StorageLimits {
    max_committed_bytes: Option<usize>,
    max_fixture_bytes: Option<usize>,
    max_epoch_bytes: Option<usize>,
    max_transaction_bytes: Option<usize>,
    max_row_bytes: Option<usize>,
    max_scan_bytes: Option<usize>,
}

impl RegionAccounting {
    fn total_bytes(&self) -> usize {
        self.row_bytes
            .saturating_add(self.byref_bytes)
            .saturating_add(self.index_bytes)
            .saturating_add(self.overhead_bytes)
    }

    fn append(&mut self, other: &mut RegionAccounting) {
        self.rows = self.rows.saturating_add(other.rows);
        self.row_bytes = self.row_bytes.saturating_add(other.row_bytes);
        self.byref_bytes = self.byref_bytes.saturating_add(other.byref_bytes);
        self.index_bytes = self.index_bytes.saturating_add(other.index_bytes);
        self.overhead_bytes = self.overhead_bytes.saturating_add(other.overhead_bytes);
        *other = RegionAccounting::default();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StorageRegion {
    id: StorageRegionId,
    kind: StorageRegionKind,
    arena: RowArena,
    accounting: RegionAccounting,
}

impl StorageRegion {
    fn new(kind: StorageRegionKind) -> Self {
        Self {
            id: next_storage_region_id(),
            kind,
            arena: RowArena::default(),
            accounting: RegionAccounting::default(),
        }
    }

    fn alloc_bytes(&mut self, bytes: &[u8]) -> ValueRef {
        let ptr = self.arena.alloc_bytes(bytes);
        self.accounting.byref_bytes = self.accounting.byref_bytes.saturating_add(bytes.len());
        self.accounting.overhead_bytes = self
            .accounting
            .overhead_bytes
            .saturating_add(std::mem::size_of::<Box<[u8]>>());
        ValueRef {
            region_id: self.id,
            ptr,
            len: bytes.len(),
        }
    }

    fn account_row(&mut self, row: &Row) {
        self.accounting.rows = self.accounting.rows.saturating_add(1);
        self.accounting.row_bytes = self
            .accounting
            .row_bytes
            .saturating_add(estimated_row_bytes(row));
    }

    fn copy_row(&mut self, row: &Row) -> Option<Row> {
        let mut cells = Vec::with_capacity(row.cells.len());
        for cell in &row.cells {
            cells.push(cell.copy_into(self)?);
        }

        let copied = Row {
            row_id: row.row_id,
            cells,
        };
        self.account_row(&copied);
        Some(copied)
    }

    fn unaccount_row(&mut self, row: &Row) {
        self.accounting.rows = self.accounting.rows.saturating_sub(1);
        self.accounting.row_bytes = self
            .accounting
            .row_bytes
            .saturating_sub(estimated_row_bytes(row));
    }

    fn account_index_key(&mut self, key: &IndexKey) {
        self.accounting.index_bytes = self
            .accounting
            .index_bytes
            .saturating_add(estimated_index_key_bytes(key));
    }

    fn unaccount_index_key(&mut self, key: &IndexKey) {
        self.accounting.index_bytes = self
            .accounting
            .index_bytes
            .saturating_sub(estimated_index_key_bytes(key));
    }

    fn checkpoint(&self) -> RegionCheckpoint {
        RegionCheckpoint {
            arena: self.arena.checkpoint(),
            accounting: self.accounting.clone(),
        }
    }

    fn rewind_to(&mut self, checkpoint: RegionCheckpoint) {
        self.arena.rewind_to(checkpoint.arena);
        self.accounting = checkpoint.accounting;
        STORAGE_ARENA_REWINDS.fetch_add(1, Ordering::Relaxed);
    }

    fn append(&mut self, other: &mut StorageRegion) {
        self.arena.append(&mut other.arena);
        self.accounting.append(&mut other.accounting);
    }

    fn bytes(&self) -> usize {
        self.accounting.total_bytes()
    }

    fn reset(&mut self) {
        *self = StorageRegion::new(self.kind);
    }
}

fn next_storage_region_id() -> StorageRegionId {
    let id = NEXT_STORAGE_REGION_ID.fetch_add(1, Ordering::Relaxed);
    StorageRegionId(id.max(1))
}

fn estimated_row_bytes(row: &Row) -> usize {
    std::mem::size_of::<Row>().saturating_add(
        row.cells
            .capacity()
            .saturating_mul(std::mem::size_of::<Cell>()),
    )
}

fn estimated_index_key_bytes(key: &IndexKey) -> usize {
    std::mem::size_of::<IndexKey>().saturating_add(
        key.parts()
            .len()
            .saturating_mul(std::mem::size_of::<IndexKeyPart>()),
    )
}

fn estimated_input_row_bytes(is_null: &[u8], byval: &[u8], value_lens: &[usize]) -> usize {
    let row_bytes = std::mem::size_of::<Row>()
        .saturating_add(value_lens.len().saturating_mul(std::mem::size_of::<Cell>()));
    let byref_bytes = value_lens
        .iter()
        .enumerate()
        .filter(|(index, _)| is_null[*index] == 0 && byval[*index] == 0)
        .map(|(_, len)| len)
        .copied()
        .fold(0usize, usize::saturating_add);
    let byref_overhead = value_lens
        .iter()
        .enumerate()
        .filter(|(index, _)| is_null[*index] == 0 && byval[*index] == 0)
        .count()
        .saturating_mul(std::mem::size_of::<Box<[u8]>>());
    row_bytes
        .saturating_add(byref_bytes)
        .saturating_add(byref_overhead)
}

fn check_limit(
    limit: Option<usize>,
    current_bytes: usize,
    additional_bytes: usize,
    region: &str,
) -> Result<(), CatalogError> {
    let Some(limit) = limit else {
        return Ok(());
    };
    if current_bytes.saturating_add(additional_bytes) <= limit {
        return Ok(());
    }
    STORAGE_MEMORY_LIMIT_REJECTIONS.fetch_add(1, Ordering::Relaxed);
    Err(CatalogError::new(
        SQLSTATE_PROGRAM_LIMIT_EXCEEDED,
        format!("fastpg memory limit exceeded for {region} storage"),
    ))
}

fn limit_from_ffi(value: usize) -> Option<usize> {
    (value != 0).then_some(value)
}

#[derive(Clone, Copy, Debug, Default)]
enum IndexKeyPart {
    #[default]
    Null,
    ByValue(usize),
    Bytes(ValueRef),
}

impl PartialEq for IndexKeyPart {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}

impl Eq for IndexKeyPart {}

impl PartialOrd for IndexKeyPart {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for IndexKeyPart {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (Self::Null, Self::Null) => std::cmp::Ordering::Equal,
            (Self::Null, _) => std::cmp::Ordering::Less,
            (_, Self::Null) => std::cmp::Ordering::Greater,
            (Self::ByValue(left), Self::ByValue(right)) => left.cmp(right),
            (Self::ByValue(_), Self::Bytes(_)) => std::cmp::Ordering::Less,
            (Self::Bytes(_), Self::ByValue(_)) => std::cmp::Ordering::Greater,
            (Self::Bytes(left), Self::Bytes(right)) => {
                value_ref_bytes(left).cmp(value_ref_bytes(right))
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct IndexKey {
    len: u8,
    parts: [IndexKeyPart; FASTPG_MAX_INDEX_KEYS],
}

impl IndexKey {
    fn new() -> Self {
        Self {
            len: 0,
            parts: [IndexKeyPart::Null; FASTPG_MAX_INDEX_KEYS],
        }
    }

    fn push(&mut self, part: IndexKeyPart) -> Option<()> {
        let index = usize::from(self.len);
        if index >= FASTPG_MAX_INDEX_KEYS {
            return None;
        }
        self.parts[index] = part;
        self.len = self.len.checked_add(1)?;
        Some(())
    }

    fn parts(&self) -> &[IndexKeyPart] {
        &self.parts[..usize::from(self.len)]
    }
}

impl PartialEq for IndexKey {
    fn eq(&self, other: &Self) -> bool {
        self.parts() == other.parts()
    }
}

impl Eq for IndexKey {}

impl PartialOrd for IndexKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for IndexKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.parts().cmp(other.parts())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct IndexColumnSpec {
    column_index: usize,
    typbyval: bool,
    typlen: i16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct UniqueIndexSpec {
    index_oid: Oid,
    relation_oid: Oid,
    is_primary: bool,
    nulls_not_distinct: bool,
    columns: Vec<IndexColumnSpec>,
}

#[derive(Clone, Debug)]
struct CachedUniqueIndex {
    info: FastPgRustPrimaryKeyIndexInfo,
    spec: UniqueIndexSpec,
}

#[derive(Debug)]
struct RowSegment {
    rows: Vec<Row>,
    region: StorageRegion,
}

impl RowSegment {
    fn new(kind: StorageRegionKind) -> Self {
        Self {
            rows: Vec::new(),
            region: StorageRegion::new(kind),
        }
    }

    fn alloc_bytes(&mut self, bytes: &[u8]) -> ValueRef {
        self.region.alloc_bytes(bytes)
    }

    fn push_row(&mut self, row: Row) {
        self.region.account_row(&row);
        self.rows.push(row);
    }

    fn remove_row_id(&mut self, row_id: u64) {
        self.remove_row_ids(&BTreeSet::from([row_id]));
    }

    fn remove_row_ids(&mut self, row_ids: &BTreeSet<u64>) {
        if row_ids.is_empty() || self.rows.is_empty() {
            return;
        }

        let mut retained = Vec::with_capacity(self.rows.len());
        for row in self.rows.drain(..) {
            if row_ids.contains(&row.row_id) {
                self.region.unaccount_row(&row);
            } else {
                retained.push(row);
            }
        }
        self.rows = retained;
    }

    fn append_from(&mut self, other: &mut RowSegment) {
        self.rows.append(&mut other.rows);
        self.region.append(&mut other.region);
    }

    fn bytes(&self) -> usize {
        self.region.bytes()
    }

    fn checkpoint(&self) -> RegionCheckpoint {
        self.region.checkpoint()
    }

    fn rewind_to(&mut self, checkpoint: RegionCheckpoint) {
        self.region.rewind_to(checkpoint);
    }
}

impl Default for RowSegment {
    fn default() -> Self {
        Self::new(StorageRegionKind::Transaction)
    }
}

#[derive(Debug)]
struct RelationRows {
    committed_row_ids: BTreeSet<u64>,
    committed_row_index: HashMap<u64, Row>,
    committed_region: StorageRegion,
    primary_key_index: BTreeMap<IndexKey, u64>,
    next_row_id: u64,
}

impl Default for RelationRows {
    fn default() -> Self {
        Self {
            committed_row_ids: BTreeSet::new(),
            committed_row_index: HashMap::new(),
            committed_region: StorageRegion::new(StorageRegionKind::Committed),
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

    fn committed_bytes(&self) -> usize {
        self.committed_region.bytes()
    }

    fn primary_key_index_bytes(&self) -> usize {
        self.committed_region.accounting.index_bytes
    }

    fn insert_primary_key(&mut self, key: IndexKey, row_id: u64) {
        if let Some((old_key, _)) = self.primary_key_index.remove_entry(&key) {
            self.committed_region.unaccount_index_key(&old_key);
        }
        self.committed_region.account_index_key(&key);
        self.primary_key_index.insert(key, row_id);
    }

    fn remove_primary_key(&mut self, key: &IndexKey) {
        if let Some((old_key, _)) = self.primary_key_index.remove_entry(key) {
            self.committed_region.unaccount_index_key(&old_key);
        }
    }
}

#[derive(Debug)]
struct TransactionOverlay {
    region_kind: StorageRegionKind,
    relations: HashMap<u32, RowSegment>,
    deleted_row_ids: HashMap<u32, BTreeSet<u64>>,
    pending_primary_key_rebuilds: BTreeSet<u32>,
}

impl TransactionOverlay {
    fn new(region_kind: StorageRegionKind) -> Self {
        Self {
            region_kind,
            relations: HashMap::new(),
            deleted_row_ids: HashMap::new(),
            pending_primary_key_rebuilds: BTreeSet::new(),
        }
    }

    fn transaction() -> Self {
        Self::new(StorageRegionKind::Transaction)
    }

    fn savepoint() -> Self {
        Self::new(StorageRegionKind::Savepoint)
    }

    fn row_segment_mut(&mut self, relid: u32) -> &mut RowSegment {
        let region_kind = self.region_kind;
        self.relations
            .entry(relid)
            .or_insert_with(|| RowSegment::new(region_kind))
    }

    fn mark_primary_key_rebuild(&mut self, relid: u32) {
        self.pending_primary_key_rebuilds.insert(relid);
    }

    fn bytes(&self) -> usize {
        self.relations.values().map(RowSegment::bytes).sum()
    }
}

impl Default for TransactionOverlay {
    fn default() -> Self {
        Self::transaction()
    }
}

#[derive(Debug)]
struct ScanState {
    rows: Vec<Row>,
    region: StorageRegion,
    shared_scan: Option<Arc<CachedCatalogScan>>,
    next_index: usize,
}

impl ScanState {
    fn materialize<'a>(rows: impl IntoIterator<Item = &'a Row>) -> Result<Self, CatalogError> {
        let mut region = StorageRegion::new(StorageRegionKind::Scan);
        let rows = rows
            .into_iter()
            .map(|row| {
                region
                    .copy_row(row)
                    .ok_or_else(|| invalid_ffi_argument("invalid stored by-reference cell".into()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            rows,
            region,
            shared_scan: None,
            next_index: 0,
        })
    }

    fn rows(&self) -> &[Row] {
        self.shared_scan
            .as_ref()
            .map_or(self.rows.as_slice(), |scan| scan.rows.as_slice())
    }

    fn bytes(&self) -> usize {
        self.region.bytes().saturating_add(
            self.shared_scan
                .as_ref()
                .map_or(0, |scan| scan.region.bytes()),
        )
    }
}

#[derive(Debug)]
struct CachedCatalogScan {
    rows: Vec<Row>,
    region: Arc<StorageRegion>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CatalogScanFilterCacheKey {
    relation_oid: u32,
    filters: Vec<CatalogRowFilter>,
}

#[derive(Debug)]
struct CatalogScanCache {
    generation: u64,
    entries: HashMap<u32, Arc<CachedCatalogScan>>,
    filtered_entries: HashMap<CatalogScanFilterCacheKey, Arc<CachedCatalogScan>>,
}

impl Default for CatalogScanCache {
    fn default() -> Self {
        Self {
            generation: current_generation(),
            entries: HashMap::new(),
            filtered_entries: HashMap::new(),
        }
    }
}

static CATALOG_SCAN_CACHE: OnceLock<Mutex<CatalogScanCache>> = OnceLock::new();

fn catalog_scan_cache() -> &'static Mutex<CatalogScanCache> {
    CATALOG_SCAN_CACHE.get_or_init(|| Mutex::new(CatalogScanCache::default()))
}

fn with_catalog_scan_cache<R>(f: impl FnOnce(&mut CatalogScanCache) -> R) -> R {
    let generation = current_generation();
    let mut cache = catalog_scan_cache()
        .lock()
        .expect("catalog scan cache mutex poisoned");
    if cache.generation != generation {
        cache.generation = generation;
        cache.entries.clear();
        cache.filtered_entries.clear();
    }
    f(&mut cache)
}

fn clear_catalog_scan_cache() {
    if let Some(cache) = CATALOG_SCAN_CACHE.get() {
        let mut cache = cache.lock().expect("catalog scan cache mutex poisoned");
        cache.generation = current_generation();
        cache.entries.clear();
        cache.filtered_entries.clear();
    }
}

fn scan_state_from_cached_catalog_scan(cache: Arc<CachedCatalogScan>) -> ScanState {
    ScanState {
        rows: Vec::new(),
        region: StorageRegion::new(StorageRegionKind::Scan),
        shared_scan: Some(cache),
        next_index: 0,
    }
}

#[derive(Debug)]
struct StorageState {
    relations: HashMap<u32, RelationRows>,
    fixture_region: StorageRegion,
    epoch_region: StorageRegion,
    limits: StorageLimits,
}

impl Default for StorageState {
    fn default() -> Self {
        Self {
            relations: HashMap::new(),
            fixture_region: StorageRegion::new(StorageRegionKind::Fixture),
            epoch_region: StorageRegion::new(StorageRegionKind::Epoch),
            limits: StorageLimits::default(),
        }
    }
}

#[derive(Debug)]
pub struct SessionStorage {
    transaction_stack: Vec<TransactionOverlay>,
    explicit_transaction: bool,
    scans: HashMap<u64, ScanState>,
    next_scan_handle: u64,
    catalog_session: fastpg_catalog::CatalogSessionHandle,
}

impl Default for SessionStorage {
    fn default() -> Self {
        Self {
            transaction_stack: Vec::new(),
            explicit_transaction: false,
            scans: HashMap::new(),
            next_scan_handle: 1,
            catalog_session: fastpg_catalog::new_catalog_session(),
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
    static LAST_STORAGE_ERROR: RefCell<Option<CatalogError>> = const { RefCell::new(None) };
}

#[derive(Debug)]
pub struct SessionStorageGuard {
    previous: Option<SessionStorageHandle>,
    _catalog_guard: fastpg_catalog::CatalogSessionGuard,
}

pub fn enter_session_storage(handle: SessionStorageHandle) -> SessionStorageGuard {
    let catalog_session = match handle.lock() {
        Ok(session) => session.catalog_session.clone(),
        Err(poisoned) => poisoned.into_inner().catalog_session.clone(),
    };
    let catalog_guard = fastpg_catalog::enter_catalog_session(catalog_session);
    let previous = CURRENT_SESSION_STORAGE.with(|slot| slot.replace(Some(handle)));
    SessionStorageGuard {
        previous,
        _catalog_guard: catalog_guard,
    }
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

fn clear_last_storage_error() {
    LAST_STORAGE_ERROR.with(|slot| {
        slot.replace(None);
    });
}

fn set_last_storage_error(error: CatalogError) {
    LAST_STORAGE_ERROR.with(|slot| {
        slot.replace(Some(error));
    });
}

fn last_storage_error() -> Option<CatalogError> {
    LAST_STORAGE_ERROR.with(|slot| slot.borrow().clone())
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
            self.transaction_stack
                .push(TransactionOverlay::transaction());
        }
        fastpg_catalog::begin_implicit_transaction();
    }

    fn mark_primary_key_rebuild(&mut self, relid: u32) {
        self.ensure_transaction();
        self.transaction_stack
            .last_mut()
            .expect("transaction was just ensured")
            .mark_primary_key_rebuild(relid);
    }

    fn transaction_bytes(&self) -> usize {
        self.transaction_stack
            .iter()
            .map(TransactionOverlay::bytes)
            .sum()
    }

    fn scan_bytes(&self) -> usize {
        self.scans.values().map(ScanState::bytes).sum()
    }
}

impl StorageState {
    fn committed_bytes(&self) -> usize {
        self.relations
            .values()
            .map(RelationRows::committed_bytes)
            .sum()
    }

    fn fixture_bytes(&self) -> usize {
        self.fixture_region.bytes()
    }

    fn epoch_bytes(&self) -> usize {
        self.epoch_region.bytes()
    }

    fn index_bytes(&self) -> usize {
        self.relations
            .values()
            .map(RelationRows::primary_key_index_bytes)
            .sum()
    }

    fn set_limits(&mut self, limits: StorageLimits) {
        self.limits = limits;
    }

    fn reset_limits(&mut self) {
        self.limits = StorageLimits::default();
    }

    fn discard_fixture_region(&mut self) {
        self.fixture_region.reset();
    }

    fn discard_epoch_region(&mut self) {
        self.epoch_region.reset();
    }

    fn check_row_limit(&self, bytes: usize) -> Result<(), CatalogError> {
        check_limit(self.limits.max_row_bytes, 0, bytes, "row")
    }

    fn check_transaction_limit(
        &self,
        session: &SessionStorage,
        additional_bytes: usize,
    ) -> Result<(), CatalogError> {
        check_limit(
            self.limits.max_transaction_bytes,
            session.transaction_bytes(),
            additional_bytes,
            "transaction",
        )
    }

    fn check_committed_projection_limit(
        &self,
        session: &SessionStorage,
        additional_bytes: usize,
    ) -> Result<(), CatalogError> {
        check_limit(
            self.limits.max_committed_bytes,
            self.committed_bytes()
                .saturating_add(session.transaction_bytes()),
            additional_bytes,
            "committed",
        )
    }

    fn check_scan_limit(&self, additional_bytes: usize) -> Result<(), CatalogError> {
        check_limit(self.limits.max_scan_bytes, 0, additional_bytes, "scan")
    }

    fn begin_explicit_transaction(&mut self, session: &mut SessionStorage) {
        if !session.explicit_transaction {
            self.commit_implicit_transaction(session);
        }
        session.ensure_transaction();
        fastpg_catalog::begin_explicit_transaction();
        session.explicit_transaction = true;
    }

    fn commit_explicit_transaction(&mut self, session: &mut SessionStorage) {
        let mut pending_primary_key_rebuilds = BTreeSet::new();
        while !session.transaction_stack.is_empty() {
            pending_primary_key_rebuilds.extend(self.commit_top_overlay(session));
        }
        fastpg_catalog::commit_explicit_transaction();
        session.explicit_transaction = false;
        self.rebuild_primary_key_indexes(&pending_primary_key_rebuilds);
    }

    fn abort_explicit_transaction(&mut self, session: &mut SessionStorage) {
        session.transaction_stack.clear();
        fastpg_catalog::abort_explicit_transaction();
        session.explicit_transaction = false;
    }

    fn commit_implicit_transaction(&mut self, session: &mut SessionStorage) {
        if session.explicit_transaction {
            return;
        }
        let mut pending_primary_key_rebuilds = BTreeSet::new();
        while !session.transaction_stack.is_empty() {
            pending_primary_key_rebuilds.extend(self.commit_top_overlay(session));
        }
        fastpg_catalog::commit_implicit_transaction();
        self.rebuild_primary_key_indexes(&pending_primary_key_rebuilds);
    }

    fn abort_implicit_transaction(&mut self, session: &mut SessionStorage) {
        if !session.explicit_transaction {
            session.transaction_stack.clear();
            fastpg_catalog::abort_implicit_transaction();
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

    fn visible_scan_state(
        &self,
        session: &SessionStorage,
        relid: u32,
    ) -> Result<ScanState, CatalogError> {
        let mut rows = BTreeMap::new();
        if let Some(relation) = self.relations.get(&relid) {
            for row_id in &relation.committed_row_ids {
                if let Some(row) = relation.committed_row_index.get(row_id) {
                    rows.insert(*row_id, row);
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
                    rows.insert(row.row_id, row);
                }
            }
        }
        ScanState::materialize(rows.into_values())
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

    fn visible_row_exists(&self, session: &SessionStorage, relid: u32, row_id: u64) -> bool {
        if row_id == 0 {
            return false;
        }

        for overlay in self.visible_overlay_stack(session).iter().rev() {
            if overlay
                .relations
                .get(&relid)
                .is_some_and(|segment| segment.rows.iter().any(|row| row.row_id == row_id))
            {
                return true;
            }
            if overlay
                .deleted_row_ids
                .get(&relid)
                .is_some_and(|deleted| deleted.contains(&row_id))
            {
                return false;
            }
        }

        self.relations
            .get(&relid)
            .is_some_and(|relation| relation.committed_row_index.contains_key(&row_id))
    }

    fn find_committed_row(&self, relid: u32, row_id: u64) -> Option<Row> {
        self.relations
            .get(&relid)
            .and_then(|relation| relation.committed_row_index.get(&row_id).cloned())
    }

    fn find_visible_row_by_index_key(
        &self,
        session: &SessionStorage,
        relid: u32,
        index_spec: &UniqueIndexSpec,
        key: &IndexKey,
    ) -> Option<Row> {
        let mut committed_candidate = if index_spec.is_primary {
            self.relations
                .get(&relid)
                .and_then(|relation| relation.primary_key_index.get(key).copied())
        } else {
            None
        };

        for overlay in self.visible_overlay_stack(session).iter().rev() {
            if let Some(segment) = overlay.relations.get(&relid)
                && let Some(row) = segment
                    .rows
                    .iter()
                    .find(|row| index_key_for_row(index_spec, row).as_ref() == Some(key))
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

        if let Some(row) =
            committed_candidate.and_then(|row_id| self.find_committed_row(relid, row_id))
        {
            return Some(row);
        }

        self.relations.get(&relid).and_then(|relation| {
            relation
                .committed_row_ids
                .iter()
                .filter_map(|row_id| relation.committed_row_index.get(row_id))
                .find(|row| index_key_for_row(index_spec, row).as_ref() == Some(key))
                .cloned()
        })
    }

    fn find_visible_row_by_index_key_excluding(
        &self,
        session: &SessionStorage,
        relid: u32,
        index_spec: &UniqueIndexSpec,
        key: &IndexKey,
        replacing_row_id: Option<u64>,
    ) -> Option<Row> {
        self.visible_rows(session, relid).into_iter().find(|row| {
            Some(row.row_id) != replacing_row_id
                && index_key_for_row(index_spec, row).as_ref() == Some(key)
        })
    }

    fn unique_index_conflict(
        &mut self,
        session: &SessionStorage,
        relid: u32,
        row: &Row,
        replacing_row_id: Option<u64>,
    ) -> Option<Oid> {
        for index_spec in unique_index_specs_for_relation_oid(Oid(relid)) {
            let Some(key) = index_key_for_row(&index_spec, row) else {
                continue;
            };
            if self
                .find_visible_row_by_index_key_excluding(
                    session,
                    relid,
                    &index_spec,
                    &key,
                    replacing_row_id,
                )
                .is_some()
            {
                return Some(index_spec.index_oid);
            }
        }
        None
    }

    fn clear_relation(&mut self, session: &mut SessionStorage, relid: u32) {
        if session.transaction_stack.is_empty() {
            self.relations.insert(relid, RelationRows::default());
            return;
        }

        let visible_row_ids = self
            .visible_rows(session, relid)
            .into_iter()
            .map(|row| row.row_id)
            .collect::<BTreeSet<_>>();
        let overlay = session
            .transaction_stack
            .last_mut()
            .expect("transaction stack was checked");
        if let Some(segment) = overlay.relations.get_mut(&relid) {
            segment.remove_row_ids(&visible_row_ids);
        }
        overlay
            .deleted_row_ids
            .entry(relid)
            .or_default()
            .extend(visible_row_ids);
    }

    fn swap_relation_storage(&mut self, session: &mut SessionStorage, left: u32, right: u32) {
        if left == right {
            return;
        }

        swap_hashmap_entries(&mut self.relations, left, right);
        for overlay in &mut session.transaction_stack {
            swap_hashmap_entries(&mut overlay.relations, left, right);
            swap_hashmap_entries(&mut overlay.deleted_row_ids, left, right);
            swap_btreeset_members(&mut overlay.pending_primary_key_rebuilds, left, right);
        }
    }

    fn commit_top_overlay(&mut self, session: &mut SessionStorage) -> BTreeSet<u32> {
        let Some(overlay) = session.transaction_stack.pop() else {
            return BTreeSet::new();
        };

        if let Some(parent) = session.transaction_stack.last_mut() {
            merge_overlay_into_overlay(parent, overlay);
            BTreeSet::new()
        } else {
            self.commit_overlay_to_relations(overlay)
        }
    }

    fn visible_overlay_stack<'a>(&self, session: &'a SessionStorage) -> &'a [TransactionOverlay] {
        &session.transaction_stack
    }

    fn commit_overlay_to_relations(&mut self, overlay: TransactionOverlay) -> BTreeSet<u32> {
        let TransactionOverlay {
            region_kind: _,
            relations,
            deleted_row_ids,
            pending_primary_key_rebuilds,
        } = overlay;
        let mut unchanged_primary_key_updates: HashMap<u32, BTreeSet<u64>> = HashMap::new();

        for (relid, deleted_row_ids) in &deleted_row_ids {
            if deleted_row_ids.is_empty() {
                continue;
            }
            let primary_key_spec = primary_index_spec_for_relation_oid(Oid(*relid));
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

        for (relid, segment) in relations {
            if segment.rows.is_empty() {
                continue;
            }
            let primary_key_spec = primary_index_spec_for_relation_oid(Oid(relid));
            let unchanged_row_ids = unchanged_primary_key_updates.get(&relid);
            let relation = self.relations.entry(relid).or_default();
            for row in &segment.rows {
                let primary_key_unchanged =
                    unchanged_row_ids.is_some_and(|row_ids| row_ids.contains(&row.row_id));
                let committed_row = relation
                    .committed_region
                    .copy_row(row)
                    .expect("stored by-reference cells must point to owned storage");
                let row_id = committed_row.row_id;
                let primary_key = if primary_key_unchanged {
                    None
                } else {
                    primary_key_spec.as_ref().and_then(|primary_key_spec| {
                        index_key_for_row(primary_key_spec, &committed_row)
                    })
                };
                if !primary_key_unchanged {
                    relation.committed_row_ids.insert(row_id);
                }
                relation.committed_row_index.insert(row_id, committed_row);
                if let Some(key) = primary_key {
                    relation.insert_primary_key(key, row_id);
                }
            }
        }

        pending_primary_key_rebuilds
    }

    fn rebuild_primary_key_indexes(&mut self, relation_oids: &BTreeSet<u32>) {
        for relid in relation_oids {
            self.rebuild_primary_key_index(*relid);
        }
    }

    fn rebuild_primary_key_index(&mut self, relid: u32) {
        let Some(relation) = self.relations.get_mut(&relid) else {
            return;
        };

        let old_keys = std::mem::take(&mut relation.primary_key_index);
        for key in old_keys.into_keys() {
            relation.committed_region.unaccount_index_key(&key);
        }

        let Some(index_spec) = primary_index_spec_for_relation_oid(Oid(relid)) else {
            return;
        };
        let index_entries = relation
            .committed_row_ids
            .iter()
            .filter_map(|row_id| {
                let row = relation.committed_row_index.get(row_id)?;
                index_key_for_row(&index_spec, row).map(|key| (key, *row_id))
            })
            .collect::<Vec<_>>();
        for (key, row_id) in index_entries {
            relation.insert_primary_key(key, row_id);
        }
    }

    fn remove_committed_entries(
        &mut self,
        relid: u32,
        primary_key_spec: Option<&UniqueIndexSpec>,
        deleted_row_ids: &BTreeSet<u64>,
        replacement_segment: Option<&RowSegment>,
    ) -> BTreeSet<u64> {
        let mut unchanged_primary_key_row_ids = BTreeSet::new();
        if deleted_row_ids.is_empty() {
            return unchanged_primary_key_row_ids;
        }

        if let Some(relation) = self.relations.get_mut(&relid) {
            for row_id in deleted_row_ids {
                let Some(row) = relation.committed_row_index.remove(row_id) else {
                    continue;
                };
                relation.committed_region.unaccount_row(&row);
                let Some(primary_key_spec) = primary_key_spec else {
                    relation.committed_row_ids.remove(row_id);
                    continue;
                };
                if let Some(replacement) =
                    replacement_segment.and_then(|segment| find_row_in_segment(segment, *row_id))
                    && primary_key_unchanged(primary_key_spec, &row, replacement)
                {
                    unchanged_primary_key_row_ids.insert(*row_id);
                    continue;
                }
                if let Some(key) = index_key_for_row(primary_key_spec, &row) {
                    relation.remove_primary_key(&key);
                }
                relation.committed_row_ids.remove(row_id);
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
    let TransactionOverlay {
        region_kind: _,
        relations,
        deleted_row_ids,
        pending_primary_key_rebuilds,
    } = overlay;

    for (relid, deleted_row_ids) in deleted_row_ids {
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

    for (relid, mut segment) in relations {
        if segment.rows.is_empty() {
            continue;
        }
        let parent_segment = parent.row_segment_mut(relid);
        parent_segment.append_from(&mut segment);
    }

    parent
        .pending_primary_key_rebuilds
        .extend(pending_primary_key_rebuilds);
}

fn remove_rows_from_segment(segment: &mut RowSegment, deleted_row_ids: &BTreeSet<u64>) {
    segment.remove_row_ids(deleted_row_ids);
}

fn swap_hashmap_entries<V>(map: &mut HashMap<u32, V>, left: u32, right: u32) {
    if left == right {
        return;
    }

    let left_value = map.remove(&left);
    let right_value = map.remove(&right);
    if let Some(value) = left_value {
        map.insert(right, value);
    }
    if let Some(value) = right_value {
        map.insert(left, value);
    }
}

fn swap_btreeset_members(set: &mut BTreeSet<u32>, left: u32, right: u32) {
    if left == right {
        return;
    }

    let had_left = set.remove(&left);
    let had_right = set.remove(&right);
    if had_left {
        set.insert(right);
    }
    if had_right {
        set.insert(left);
    }
}

fn find_row_in_segment(segment: &RowSegment, row_id: u64) -> Option<&Row> {
    segment.rows.iter().find(|row| row.row_id == row_id)
}

fn primary_key_unchanged(primary_key_spec: &UniqueIndexSpec, old_row: &Row, new_row: &Row) -> bool {
    primary_key_spec.columns.iter().all(|column| {
        let Some(old_cell) = old_row.cells.get(column.column_index) else {
            return false;
        };
        let Some(new_cell) = new_row.cells.get(column.column_index) else {
            return false;
        };
        index_key_part(column, old_cell) == index_key_part(column, new_cell)
    })
}

static STORAGE: OnceLock<Mutex<StorageState>> = OnceLock::new();
static PRIMARY_KEY_INDEX_INFO_CACHE: OnceLock<Mutex<HashMap<u32, Option<CachedUniqueIndex>>>> =
    OnceLock::new();
static RELATION_OID_BY_NAME_CACHE: OnceLock<Mutex<RelationOidByNameCache>> = OnceLock::new();
static RELATION_EXISTS_CACHE: OnceLock<Mutex<RelationExistsCache>> = OnceLock::new();

#[derive(Debug)]
struct RelationOidByNameCache {
    generation: u64,
    entries: HashMap<u32, HashMap<String, Option<u32>>>,
}

impl Default for RelationOidByNameCache {
    fn default() -> Self {
        Self {
            generation: current_generation(),
            entries: HashMap::new(),
        }
    }
}

#[derive(Debug)]
struct RelationExistsCache {
    generation: u64,
    entries: HashMap<u32, bool>,
}

impl Default for RelationExistsCache {
    fn default() -> Self {
        Self {
            generation: current_generation(),
            entries: HashMap::new(),
        }
    }
}

fn storage() -> &'static Mutex<StorageState> {
    STORAGE.get_or_init(|| Mutex::new(StorageState::default()))
}

fn primary_key_index_info_cache() -> &'static Mutex<HashMap<u32, Option<CachedUniqueIndex>>> {
    PRIMARY_KEY_INDEX_INFO_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn relation_oid_by_name_cache() -> &'static Mutex<RelationOidByNameCache> {
    RELATION_OID_BY_NAME_CACHE.get_or_init(|| Mutex::new(RelationOidByNameCache::default()))
}

fn relation_exists_cache() -> &'static Mutex<RelationExistsCache> {
    RELATION_EXISTS_CACHE.get_or_init(|| Mutex::new(RelationExistsCache::default()))
}

fn relation_oid_by_name_cache_lookup(name: &str, namespace_oid: u32) -> Option<Option<u32>> {
    if has_uncommitted_catalog_changes() {
        return None;
    }
    let generation = current_generation();
    let mut cache = relation_oid_by_name_cache()
        .lock()
        .expect("relation oid by name cache mutex poisoned");
    if cache.generation != generation {
        cache.generation = generation;
        cache.entries.clear();
    }
    cache
        .entries
        .get(&namespace_oid)
        .and_then(|entries| entries.get(name).copied())
}

fn relation_oid_by_name_cache_store(name: &str, namespace_oid: u32, oid: Option<u32>) {
    if has_uncommitted_catalog_changes() {
        return;
    }
    let generation = current_generation();
    let mut cache = relation_oid_by_name_cache()
        .lock()
        .expect("relation oid by name cache mutex poisoned");
    if cache.generation != generation {
        cache.generation = generation;
        cache.entries.clear();
    }
    cache
        .entries
        .entry(namespace_oid)
        .or_default()
        .insert(name.to_owned(), oid);
}

#[cfg(test)]
fn clear_relation_oid_by_name_cache() {
    if let Some(cache) = RELATION_OID_BY_NAME_CACHE.get() {
        let mut cache = cache
            .lock()
            .expect("relation oid by name cache mutex poisoned");
        cache.generation = current_generation();
        cache.entries.clear();
    }
}

fn relation_exists_cache_lookup(oid: u32) -> Option<bool> {
    if has_uncommitted_catalog_changes() {
        return None;
    }
    let generation = current_generation();
    let mut cache = relation_exists_cache()
        .lock()
        .expect("relation exists cache mutex poisoned");
    if cache.generation != generation {
        cache.generation = generation;
        cache.entries.clear();
    }
    cache.entries.get(&oid).copied()
}

fn relation_exists_cache_store(oid: u32, exists: bool) {
    if has_uncommitted_catalog_changes() {
        return;
    }
    let generation = current_generation();
    let mut cache = relation_exists_cache()
        .lock()
        .expect("relation exists cache mutex poisoned");
    if cache.generation != generation {
        cache.generation = generation;
        cache.entries.clear();
    }
    cache.entries.insert(oid, exists);
}

#[cfg(test)]
fn clear_relation_exists_cache() {
    if let Some(cache) = RELATION_EXISTS_CACHE.get() {
        let mut cache = cache.lock().expect("relation exists cache mutex poisoned");
        cache.generation = current_generation();
        cache.entries.clear();
    }
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

fn catalog_mutation_invalidates_primary_key_cache(relation_oid: u32) -> bool {
    matches!(relation_oid, 1249 | 1259 | 2606 | 2610)
}

fn pg_index_heap_relation_from_upsert(relation_oid: u32, values: &[Option<String>]) -> Option<u32> {
    let table = static_catalog_by_name("pg_index")?;
    if table.oid.0 != relation_oid {
        return None;
    }
    let indrelid_index = table
        .columns
        .iter()
        .position(|column| column.name == "indrelid")?;
    values.get(indrelid_index)?.as_deref()?.parse::<u32>().ok()
}

fn pg_index_heap_relation_from_delete(relation_oid: u32, row_id: u64) -> Option<u32> {
    let table = static_catalog_by_name("pg_index")?;
    if table.oid.0 != relation_oid {
        return None;
    }
    catalog_rows(table.oid).into_iter().find_map(|row| {
        if row.row_id != row_id {
            return None;
        }
        catalog_row_value(table, &row, "indrelid")
            .and_then(catalog_value_oid)
            .map(|oid| oid.0)
    })
}

fn mark_primary_key_rebuild(relid: u32) {
    with_storage(|_state, session| session.mark_primary_key_rebuild(relid));
}

fn cached_primary_key_index(index_oid: Oid) -> Option<CachedUniqueIndex> {
    let cached = match primary_key_index_info_cache().lock() {
        Ok(cache) => cache.get(&index_oid.0).cloned(),
        Err(poisoned) => poisoned.into_inner().get(&index_oid.0).cloned(),
    };
    if let Some(index) = cached {
        return index;
    }

    let index = index_heap_relation(index_oid)
        .and_then(|relation| primary_key_index_cache_entry(&relation, index_oid));
    match primary_key_index_info_cache().lock() {
        Ok(mut cache) => {
            cache.insert(index_oid.0, index.clone());
        }
        Err(poisoned) => {
            poisoned.into_inner().insert(index_oid.0, index.clone());
        }
    }
    index
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

unsafe fn nullable_c_str_array_to_strings(
    values: *const *const c_char,
    is_null: *const u8,
    len: usize,
) -> Result<Vec<Option<String>>, String> {
    if len == 0 {
        return Ok(Vec::new());
    }
    if values.is_null() || is_null.is_null() {
        return Err("null catalog row array pointer".to_owned());
    }
    let values = unsafe { slice::from_raw_parts(values, len) };
    let is_null = unsafe { slice::from_raw_parts(is_null, len) };
    values
        .iter()
        .zip(is_null.iter())
        .map(|(value, is_null)| {
            if *is_null != 0 {
                Ok(None)
            } else {
                unsafe { c_str_to_string(*value) }.map(Some)
            }
        })
        .collect()
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
        type_oid: relation.type_oid.0,
        namespace_oid: relation.namespace.0,
        owner_oid: relation.owner.0,
        name: fixed_c_name(&relation.name),
        column_count: relation.columns.len().min(u16::MAX as usize) as u16,
        relkind: relation.relkind,
        has_primary_key: u8::from(!relation.primary_key.is_empty()),
        has_indexes: u8::from(!index_records_for_relation_oid(relation.oid).is_empty()),
        row_id: relation.row_id,
    }
}

fn relation_summary_to_ffi(
    relation: &fastpg_catalog::RelationSummaryRecord,
) -> FastPgRustCatalogRelation {
    FastPgRustCatalogRelation {
        oid: relation.oid.0,
        type_oid: relation.type_oid.0,
        namespace_oid: relation.namespace.0,
        owner_oid: relation.owner.0,
        name: fixed_c_name(&relation.name),
        column_count: relation.column_count,
        relkind: relation.relkind,
        has_primary_key: u8::from(relation.has_primary_key),
        has_indexes: u8::from(relation.has_indexes),
        row_id: relation.row_id,
    }
}

fn primary_key_index_to_ffi(
    relation: &fastpg_catalog::RelationRecord,
    index_oid: Oid,
) -> FastPgRustCatalogRelation {
    FastPgRustCatalogRelation {
        oid: index_oid.0,
        type_oid: 0,
        namespace_oid: relation.namespace.0,
        owner_oid: relation.owner.0,
        name: fixed_c_name(&primary_key_index_name(relation)),
        column_count: relation.primary_key.len().min(u16::MAX as usize) as u16,
        relkind: b'i',
        has_primary_key: 0,
        has_indexes: 0,
        row_id: 0,
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
        type_oid: fastpg_catalog::static_catalog_rowtype_oid(relation.relation_oid)
            .map_or(0, |oid| oid.0),
        namespace_oid: PG_CATALOG_NAMESPACE_OID.0,
        owner_oid: 10,
        name: fixed_c_name(relation.name),
        column_count: virtual_catalog_column_count(relation.relation_oid),
        relkind: b'r',
        has_primary_key: 0,
        has_indexes: 0,
        row_id: 0,
    }
}

fn column_to_ffi(column: &ColumnRecord) -> FastPgRustCatalogColumn {
    let pg_type = lookup_type(column.type_oid);
    FastPgRustCatalogColumn {
        name: fixed_c_name(&column.name),
        type_oid: column.type_oid.0,
        type_mod: column.type_mod,
        attcollation: pg_type.as_ref().map_or(0, |pg_type| pg_type.typcollation.0),
        attlen: pg_type.as_ref().map_or(0, |pg_type| pg_type.typlen),
        is_not_null: u8::from(column.is_not_null),
        has_default: u8::from(column.has_default),
        generated: column.generated,
        is_dropped: 0,
        attbyval: pg_type
            .as_ref()
            .map_or(0, |pg_type| u8::from(pg_type.typbyval)),
        attalign: pg_type.as_ref().map_or(b'i', |pg_type| pg_type.typalign),
        attstorage: pg_type.as_ref().map_or(b'p', |pg_type| pg_type.typstorage),
        _padding: 0,
        row_id: column.row_id,
    }
}

fn physical_column_to_ffi(column: &PhysicalColumnRecord) -> FastPgRustCatalogColumn {
    FastPgRustCatalogColumn {
        name: fixed_c_name(&column.name),
        type_oid: column.type_oid.0,
        type_mod: column.type_mod,
        attcollation: column.attcollation.0,
        attlen: column.attlen,
        is_not_null: u8::from(column.is_not_null),
        has_default: u8::from(column.has_default),
        generated: column.generated,
        is_dropped: u8::from(column.is_dropped),
        attbyval: u8::from(column.attbyval),
        attalign: column.attalign,
        attstorage: column.attstorage,
        _padding: 0,
        row_id: column.row_id,
    }
}

fn static_catalog_column_to_ffi(
    relation_oid: Oid,
    column: &fastpg_catalog::StaticCatalogColumn,
) -> FastPgRustCatalogColumn {
    FastPgRustCatalogColumn {
        name: fixed_c_name(column.name),
        type_oid: column.type_oid.0,
        type_mod: -1,
        attcollation: column.attcollation.0,
        attlen: column.attlen,
        is_not_null: u8::from(column.attnotnull),
        has_default: 0,
        generated: 0,
        is_dropped: 0,
        attbyval: u8::from(column.attbyval),
        attalign: column.attalign,
        attstorage: column.attstorage,
        _padding: 0,
        row_id: fastpg_catalog::static_attribute_row_id(relation_oid, column.attnum),
    }
}

fn namespace_to_ffi(record: &fastpg_catalog::PgNamespaceRecord) -> FastPgRustCatalogNamespace {
    FastPgRustCatalogNamespace {
        oid: record.oid.0,
        owner_oid: record.owner.0,
        name: fixed_c_name(record.name),
    }
}

fn type_to_ffi(record: &fastpg_catalog::CatalogTypeRecord) -> FastPgRustCatalogType {
    FastPgRustCatalogType {
        oid: record.oid.0,
        namespace_oid: record.namespace.0,
        owner_oid: record.owner.0,
        name: fixed_c_name(&record.name),
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
        row_id: record.row_id,
    }
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

fn operator_row_to_ffi(row: &fastpg_catalog::CatalogRow) -> Option<FastPgRustCatalogOperator> {
    let table = static_catalog_by_name("pg_operator")?;
    let oid = catalog_row_value(table, row, "oid").and_then(catalog_value_oid)?;
    let namespace_oid =
        catalog_row_value(table, row, "oprnamespace").and_then(catalog_value_oid)?;
    let owner_oid = catalog_row_value(table, row, "oprowner")
        .and_then(catalog_value_oid)
        .unwrap_or(Oid(0));
    let name = catalog_row_value(table, row, "oprname").and_then(catalog_value_name)?;
    let kind = catalog_row_value(table, row, "oprkind")
        .and_then(catalog_value_char)
        .unwrap_or(b'b');
    let can_merge = catalog_row_value(table, row, "oprcanmerge")
        .and_then(catalog_value_bool)
        .unwrap_or(false);
    let can_hash = catalog_row_value(table, row, "oprcanhash")
        .and_then(catalog_value_bool)
        .unwrap_or(false);
    let left_type_oid = catalog_row_value(table, row, "oprleft")
        .and_then(catalog_value_oid)
        .unwrap_or(Oid(0));
    let right_type_oid = catalog_row_value(table, row, "oprright")
        .and_then(catalog_value_oid)
        .unwrap_or(Oid(0));
    let result_type_oid = catalog_row_value(table, row, "oprresult")
        .and_then(catalog_value_oid)
        .unwrap_or(Oid(0));
    let commutator_oid = catalog_row_value(table, row, "oprcom")
        .and_then(catalog_value_oid)
        .unwrap_or(Oid(0));
    let negator_oid = catalog_row_value(table, row, "oprnegate")
        .and_then(catalog_value_oid)
        .unwrap_or(Oid(0));
    let code_fn_oid = catalog_row_value(table, row, "oprcode")
        .and_then(catalog_value_oid)
        .unwrap_or(Oid(0));
    let rest_fn_oid = catalog_row_value(table, row, "oprrest")
        .and_then(catalog_value_oid)
        .unwrap_or(Oid(0));
    let join_fn_oid = catalog_row_value(table, row, "oprjoin")
        .and_then(catalog_value_oid)
        .unwrap_or(Oid(0));

    Some(FastPgRustCatalogOperator {
        oid: oid.0,
        namespace_oid: namespace_oid.0,
        owner_oid: owner_oid.0,
        name: fixed_c_name(name),
        kind,
        can_merge: u8::from(can_merge),
        can_hash: u8::from(can_hash),
        _padding: 0,
        left_type_oid: left_type_oid.0,
        right_type_oid: right_type_oid.0,
        result_type_oid: result_type_oid.0,
        commutator_oid: commutator_oid.0,
        negator_oid: negator_oid.0,
        code_fn_oid: code_fn_oid.0,
        rest_fn_oid: rest_fn_oid.0,
        join_fn_oid: join_fn_oid.0,
    })
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

fn cast_row_to_ffi(row: &fastpg_catalog::CatalogRow) -> Option<FastPgRustCatalogCast> {
    let table = static_catalog_by_name("pg_cast")?;
    let oid = catalog_row_value(table, row, "oid").and_then(catalog_value_oid)?;
    let source_type_oid =
        catalog_row_value(table, row, "castsource").and_then(catalog_value_oid)?;
    let target_type_oid =
        catalog_row_value(table, row, "casttarget").and_then(catalog_value_oid)?;
    let function_oid = catalog_row_value(table, row, "castfunc")
        .and_then(catalog_value_oid)
        .unwrap_or(Oid(0));
    let context = catalog_row_value(table, row, "castcontext")
        .and_then(catalog_value_char)
        .unwrap_or(b'e');
    let method = catalog_row_value(table, row, "castmethod")
        .and_then(catalog_value_char)
        .unwrap_or(b'f');

    Some(FastPgRustCatalogCast {
        oid: oid.0,
        source_type_oid: source_type_oid.0,
        target_type_oid: target_type_oid.0,
        function_oid: function_oid.0,
        context,
        method,
        _padding: [0; 2],
    })
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

fn catalog_value_i16(value: &CatalogValue) -> Option<i16> {
    match value {
        CatalogValue::Int16(value) => Some(*value),
        CatalogValue::Int32(value) => i16::try_from(*value).ok(),
        CatalogValue::Raw(value) => value.parse::<i16>().ok(),
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

fn catalog_value_char(value: &CatalogValue) -> Option<u8> {
    match value {
        CatalogValue::Char(value) => Some(*value),
        CatalogValue::Raw(value) => value.as_bytes().first().copied(),
        _ => None,
    }
}

fn catalog_value_oid_vector(value: &CatalogValue) -> Option<Vec<Oid>> {
    match value {
        CatalogValue::OidVector(values) => Some(values.clone()),
        CatalogValue::Raw(value) => Some(
            value
                .split_whitespace()
                .filter_map(|part| part.parse::<u32>().ok())
                .map(Oid)
                .collect(),
        ),
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

fn proc_row_to_ffi(row: &fastpg_catalog::CatalogRow) -> Option<FastPgRustCatalogProc> {
    let table = static_catalog_by_name("pg_proc")?;
    let oid = catalog_row_value(table, row, "oid").and_then(catalog_value_oid)?;
    let namespace = catalog_row_value(table, row, "pronamespace").and_then(catalog_value_oid)?;
    let owner = catalog_row_value(table, row, "proowner").and_then(catalog_value_oid)?;
    let language = catalog_row_value(table, row, "prolang").and_then(catalog_value_oid)?;
    let name = catalog_row_value(table, row, "proname").and_then(catalog_value_name)?;
    let cost = catalog_row_value(table, row, "procost")
        .and_then(catalog_value_f32)
        .unwrap_or(1.0);
    let rows = catalog_row_value(table, row, "prorows")
        .and_then(catalog_value_f32)
        .unwrap_or(0.0);
    let variadic = catalog_row_value(table, row, "provariadic")
        .and_then(catalog_value_oid)
        .unwrap_or(Oid(0));
    let support = catalog_row_value(table, row, "prosupport")
        .and_then(catalog_value_oid)
        .unwrap_or(Oid(0));
    let return_type = catalog_row_value(table, row, "prorettype").and_then(catalog_value_oid)?;
    let arg_types = catalog_row_value(table, row, "proargtypes")
        .and_then(catalog_value_oid_vector)
        .unwrap_or_default();
    if arg_types.len() > FASTPG_PROC_MAX_ARGS {
        return None;
    }
    let mut arg_type_oids = [0; FASTPG_PROC_MAX_ARGS];
    for (index, oid) in arg_types.iter().enumerate() {
        arg_type_oids[index] = oid.0;
    }
    let source = catalog_row_value(table, row, "prosrc")
        .and_then(catalog_value_name)
        .unwrap_or("");

    Some(FastPgRustCatalogProc {
        oid: oid.0,
        namespace_oid: namespace.0,
        owner_oid: owner.0,
        language_oid: language.0,
        name: fixed_c_name(name),
        source: fixed_c_bytes(source),
        cost,
        rows,
        variadic_oid: variadic.0,
        support_oid: support.0,
        return_type_oid: return_type.0,
        arg_count: arg_types.len() as u16,
        arg_default_count: catalog_row_value(table, row, "pronargdefaults")
            .and_then(catalog_value_i16)
            .unwrap_or(0)
            .max(0) as u16,
        kind: catalog_row_value(table, row, "prokind")
            .and_then(catalog_value_char)
            .unwrap_or(b'f'),
        security_definer: catalog_row_value(table, row, "prosecdef")
            .and_then(catalog_value_bool)
            .unwrap_or(false)
            .into(),
        leakproof: catalog_row_value(table, row, "proleakproof")
            .and_then(catalog_value_bool)
            .unwrap_or(false)
            .into(),
        is_strict: catalog_row_value(table, row, "proisstrict")
            .and_then(catalog_value_bool)
            .unwrap_or(false)
            .into(),
        returns_set: catalog_row_value(table, row, "proretset")
            .and_then(catalog_value_bool)
            .unwrap_or(false)
            .into(),
        volatility: catalog_row_value(table, row, "provolatile")
            .and_then(catalog_value_char)
            .unwrap_or(b'v'),
        parallel: catalog_row_value(table, row, "proparallel")
            .and_then(catalog_value_char)
            .unwrap_or(b'u'),
        _padding: 0,
        arg_type_oids,
    })
}

fn proc_row_by_oid(oid: Oid) -> Option<FastPgRustCatalogProc> {
    let table = static_catalog_by_name("pg_proc")?;
    catalog_rows(table.oid).into_iter().find_map(|row| {
        let row_oid = catalog_row_value(table, &row, "oid").and_then(catalog_value_oid)?;
        (row_oid == oid).then(|| proc_row_to_ffi(&row)).flatten()
    })
}

fn proc_rows_by_name(name: &str) -> impl Iterator<Item = FastPgRustCatalogProc> {
    let normalized = name.to_ascii_lowercase();
    let rows = static_catalog_by_name("pg_proc")
        .map(|table| {
            catalog_rows(table.oid)
                .into_iter()
                .filter_map(move |row| {
                    let row_name =
                        catalog_row_value(table, &row, "proname").and_then(catalog_value_name)?;
                    (row_name == normalized)
                        .then(|| proc_row_to_ffi(&row))
                        .flatten()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    rows.into_iter()
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

fn unique_index_spec_for_record(record: &fastpg_catalog::IndexRecord) -> Option<UniqueIndexSpec> {
    if !record.is_unique || !record.is_valid || !record.is_ready || !record.is_live {
        return None;
    }
    let mut columns = Vec::with_capacity(record.key_attnums.len());
    for attnum in &record.key_attnums {
        if *attnum <= 0 {
            return None;
        }
        let column_index = usize::try_from(*attnum - 1).ok()?;
        let column = relation_column_by_attnum(record.relation_oid, *attnum)?;
        let pg_type = lookup_type(column.type_oid)?;
        columns.push(IndexColumnSpec {
            column_index,
            typbyval: pg_type.typbyval,
            typlen: pg_type.typlen,
        });
    }

    if columns.is_empty() {
        return None;
    }

    Some(UniqueIndexSpec {
        index_oid: record.index_oid,
        relation_oid: record.relation_oid,
        is_primary: record.is_primary,
        nulls_not_distinct: record.nulls_not_distinct,
        columns,
    })
}

fn unique_index_specs_for_relation_oid(relation_oid: Oid) -> Vec<UniqueIndexSpec> {
    unique_index_records_for_relation_oid(relation_oid)
        .iter()
        .filter_map(unique_index_spec_for_record)
        .collect()
}

fn primary_index_spec_for_relation_oid(relation_oid: Oid) -> Option<UniqueIndexSpec> {
    let index_oid = primary_key_index_oid_for_relation_oid(relation_oid)?;
    let index_spec = cached_primary_key_index(index_oid)?.spec;
    index_spec.is_primary.then_some(index_spec)
}

fn primary_key_index_oid(relation: &fastpg_catalog::RelationRecord) -> Option<Oid> {
    primary_key_index_oid_for_relation_oid(relation.oid)
}

fn primary_key_index_name(relation: &fastpg_catalog::RelationRecord) -> String {
    if let Some(index_oid) = primary_key_index_oid(relation)
        && let Some(index_relation) = relation_by_oid(index_oid)
    {
        return index_relation.name;
    }
    format!("{}_pkey", relation.name)
}

fn primary_key_index_relation(index_oid: Oid) -> Option<fastpg_catalog::RelationRecord> {
    let relation_oid = primary_key_relation_oid_for_index_oid(index_oid)?;
    relation_by_oid(relation_oid)
}

fn index_heap_relation(index_oid: Oid) -> Option<fastpg_catalog::RelationRecord> {
    let relation_oid = relation_oid_for_index_oid(index_oid)?;
    relation_by_oid(relation_oid)
}

fn primary_key_index_relation_by_name(
    name: &str,
    namespace: Oid,
) -> Option<fastpg_catalog::RelationRecord> {
    let index_relation = relation_by_name_in_namespace(name, namespace)?;
    let relation_oid = primary_key_relation_oid_for_index_oid(index_relation.oid)?;
    relation_by_oid(relation_oid)
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

fn primary_key_index_cache_entry(
    relation: &fastpg_catalog::RelationRecord,
    index_oid: Oid,
) -> Option<CachedUniqueIndex> {
    let record = index_record_by_index_oid(index_oid)?;
    let index_spec = unique_index_spec_for_record(&record)?;
    let key_count = index_spec.columns.len();
    if key_count == 0 || key_count > FASTPG_MAX_INDEX_KEYS {
        return None;
    }

    let mut attnums = [0i16; FASTPG_MAX_INDEX_KEYS];
    let mut type_oids = [0u32; FASTPG_MAX_INDEX_KEYS];
    let mut collation_oids = [0u32; FASTPG_MAX_INDEX_KEYS];
    for (key_index, index_column) in index_spec.columns.iter().enumerate() {
        let column = relation.columns.get(index_column.column_index)?;
        let type_record = lookup_type(column.type_oid)?;
        attnums[key_index] = (index_column.column_index + 1).try_into().ok()?;
        type_oids[key_index] = column.type_oid.0;
        collation_oids[key_index] = type_record.typcollation.0;
    }

    let info = FastPgRustPrimaryKeyIndexInfo {
        row_id: record.row_id,
        index_oid: index_oid.0,
        heap_oid: relation.oid.0,
        key_count: key_count as u16,
        is_unique: u8::from(record.is_unique),
        is_primary: u8::from(record.is_primary),
        nulls_not_distinct: u8::from(record.nulls_not_distinct),
        is_immediate: u8::from(record.is_immediate),
        attnums,
        type_oids,
        collation_oids,
    };

    Some(CachedUniqueIndex {
        info,
        spec: index_spec,
    })
}

fn index_key_for_row(index_spec: &UniqueIndexSpec, row: &Row) -> Option<IndexKey> {
    let mut key = IndexKey::new();

    for column in &index_spec.columns {
        let cell = row.cells.get(column.column_index)?;
        if cell.is_null && !index_spec.nulls_not_distinct {
            return None;
        }
        key.push(index_key_part(column, cell)?)?;
    }

    Some(key)
}

fn index_key_for_datums(
    index_spec: &UniqueIndexSpec,
    values: &[usize],
    is_null: &[u8],
) -> Option<IndexKey> {
    if index_spec.columns.len() != values.len() || values.len() != is_null.len() {
        return None;
    }

    let mut key = IndexKey::new();
    for (key_index, column) in index_spec.columns.iter().enumerate() {
        let cell = if is_null[key_index] != 0 {
            Cell::null()
        } else {
            Cell::by_value(values[key_index])
        };
        if cell.is_null && !index_spec.nulls_not_distinct {
            return None;
        }
        key.push(index_key_part(column, &cell)?)?;
    }

    Some(key)
}

fn index_key_part(column: &IndexColumnSpec, cell: &Cell) -> Option<IndexKeyPart> {
    if cell.is_null {
        return Some(IndexKeyPart::Null);
    }

    if column.typbyval {
        return Some(IndexKeyPart::ByValue(cell.output_value()));
    }

    let value_ref = byref_key_ref_from_cell(cell, column.typlen)?;
    Some(IndexKeyPart::Bytes(value_ref))
}

fn value_ref_bytes(value_ref: &ValueRef) -> &[u8] {
    if value_ref.len == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(value_ref.ptr as *const u8, value_ref.len) }
    }
}

fn byref_key_ref_from_cell(cell: &Cell, typlen: i16) -> Option<ValueRef> {
    let StoredDatum::ByRef(mut value_ref) = cell.datum else {
        return None;
    };
    if value_ref.ptr == 0 && value_ref.len > 0 {
        return None;
    }
    let len = if typlen > 0 {
        typlen as usize
    } else if typlen == -1 {
        varlena_payload_len(cell.output_value())?
    } else if typlen == -2 {
        c_string_payload_len(cell.output_value())?
    } else {
        return None;
    };
    if value_ref.len < len {
        return None;
    }
    value_ref.len = len;
    Some(value_ref)
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
    let Some(record) = lookup_type(Oid(oid)) else {
        return false;
    };

    if out.is_null() {
        return false;
    }

    unsafe {
        *out = type_to_ffi(&record);
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
    let Some(record) = type_by_name(&name, Oid(namespace_oid)) else {
        return false;
    };

    unsafe {
        *out = type_to_ffi(&record);
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
pub extern "C" fn fastpg_rust_catalog_relation_exists_by_oid(oid: u32) -> bool {
    if let Some(exists) = relation_exists_cache_lookup(oid) {
        return exists;
    }
    let exists =
        relation_oid_exists(Oid(oid)) || virtual_catalog_by_relation_oid(Oid(oid)).is_some();
    relation_exists_cache_store(oid, exists);
    exists
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
    if name.is_null() {
        return false;
    }
    let Ok(name) = unsafe { CStr::from_ptr(name) }.to_str() else {
        return false;
    };
    if let Some(cached_oid) = relation_oid_by_name_cache_lookup(name, namespace_oid) {
        if let Some(cached_oid) = cached_oid {
            unsafe {
                *oid_out = cached_oid;
            }
            return true;
        }
        return false;
    }
    let oid = if let Some(relation) = virtual_catalog_by_name(name, Oid(namespace_oid)) {
        Some(relation.relation_oid.0)
    } else {
        let namespace = Oid(namespace_oid);
        relation_oid_by_name_in_namespace(name, namespace)
            .map(|relation_oid| relation_oid.0)
            .or_else(|| {
                let relation = primary_key_index_relation_by_name(name, namespace)?;
                let index_oid = primary_key_index_oid(&relation)?;
                Some(index_oid.0)
            })
    };
    relation_oid_by_name_cache_store(name, namespace_oid, oid);
    if let Some(oid) = oid {
        unsafe {
            *oid_out = oid;
        }
        return true;
    }
    false
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_catalog_relation_by_name(
    name: *const c_char,
    namespace_oid: u32,
    out: *mut FastPgRustCatalogRelation,
) -> bool {
    if out.is_null() {
        return false;
    }
    let Ok(name) = (unsafe { c_str_to_string(name) }) else {
        return false;
    };
    let namespace = Oid(namespace_oid);
    unsafe {
        if let Some(relation) = virtual_catalog_by_name(&name, namespace) {
            *out = virtual_catalog_relation_to_ffi(relation);
            return true;
        }
        if let Some(relation) = relation_summary_by_name_in_namespace(&name, namespace) {
            *out = relation_summary_to_ffi(&relation);
            return true;
        }
        if let Some(relation) = primary_key_index_relation_by_name(&name, namespace)
            && let Some(index_oid) = primary_key_index_oid(&relation)
        {
            *out = primary_key_index_to_ffi(&relation, index_oid);
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
pub unsafe extern "C" fn fastpg_rust_catalog_relation_by_oid(
    oid: u32,
    out: *mut FastPgRustCatalogRelation,
) -> bool {
    if out.is_null() {
        return false;
    }

    unsafe {
        if let Some(relation) = relation_summary_by_oid(Oid(oid)) {
            *out = relation_summary_to_ffi(&relation);
            return true;
        }
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
/// C callers must pass a valid output pointer.
pub unsafe extern "C" fn fastpg_rust_catalog_relation_rowtype_oid_by_oid(
    relation_oid: u32,
    oid_out: *mut u32,
) -> bool {
    if oid_out.is_null() {
        return false;
    }
    let Some(rowtype_oid) = relation_rowtype_oid_by_oid(Oid(relation_oid)) else {
        return false;
    };
    unsafe {
        *oid_out = rowtype_oid.0;
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers.
pub unsafe extern "C" fn fastpg_rust_catalog_relation_planner_stats_by_oid(
    relation_oid: u32,
    relpages_out: *mut i32,
    reltuples_out: *mut f32,
) -> bool {
    if relpages_out.is_null() || reltuples_out.is_null() {
        return false;
    }
    let Some(stats) = relation_planner_stats_by_oid(Oid(relation_oid)) else {
        return false;
    };
    unsafe {
        *relpages_out = stats.relpages;
        *reltuples_out = stats.reltuples;
    }
    true
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
    let Some(attnum) = column_index
        .checked_add(1)
        .and_then(|attnum| i16::try_from(attnum).ok())
    else {
        return false;
    };

    if let Some(column) = relation_physical_column_by_attnum(Oid(relation_oid), attnum) {
        unsafe {
            *out = physical_column_to_ffi(&column);
        }
        true
    } else if let Some(relation) = primary_key_index_relation(Oid(relation_oid)) {
        let Some(column) = primary_key_column(&relation, column_index) else {
            return false;
        };
        unsafe {
            *out = column_to_ffi(column);
        }
        true
    } else if let Some(table) = static_catalog_by_relation_oid(Oid(relation_oid)) {
        let Some(column) = table.columns.get(column_index) else {
            return false;
        };
        unsafe {
            *out = static_catalog_column_to_ffi(Oid(relation_oid), column);
        }
        true
    } else {
        false
    }
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
    let Some(index_info) = cached_primary_key_index(Oid(index_oid)).map(|index| index.info) else {
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
/// C callers must pass a valid output pointer.
pub unsafe extern "C" fn fastpg_rust_catalog_relation_unique_index_oid(
    relation_oid: u32,
    index_position: usize,
    oid_out: *mut u32,
) -> bool {
    if oid_out.is_null() {
        return false;
    }
    let indexes = unique_index_oids_for_relation_oid(Oid(relation_oid));
    let Some(index_oid) = indexes.get(index_position) else {
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
/// C callers must pass a valid output pointer for `oid_out`.
pub unsafe extern "C" fn fastpg_rust_catalog_enum_endpoint(
    enum_type_oid: u32,
    forward: u8,
    oid_out: *mut u32,
) -> bool {
    if oid_out.is_null() {
        return false;
    }
    let mut oids = enum_oids_by_sort_order(Oid(enum_type_oid));
    if oids.is_empty() {
        return false;
    }
    let oid = if forward != 0 {
        oids.remove(0)
    } else {
        oids.pop().expect("non-empty enum oid list")
    };
    unsafe {
        *oid_out = oid.0;
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// If `oids_out` is non-null, it must point to `capacity` writable `uint32_t`
/// slots. `count_out`, when non-null, receives the full number of matching
/// enum labels even when `capacity` is smaller.
pub unsafe extern "C" fn fastpg_rust_catalog_enum_oids_by_sort_order(
    enum_type_oid: u32,
    oids_out: *mut u32,
    capacity: usize,
    count_out: *mut usize,
) -> bool {
    let oids = enum_oids_by_sort_order(Oid(enum_type_oid));
    if !count_out.is_null() {
        unsafe {
            *count_out = oids.len();
        }
    }
    if oids_out.is_null() {
        return true;
    }
    if capacity < oids.len() {
        return false;
    }
    for (index, oid) in oids.into_iter().enumerate() {
        unsafe {
            *oids_out.add(index) = oid.0;
        }
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
    let Some(ffi_record) = proc_row_by_oid(Oid(oid)) else {
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
    proc_rows_by_name(&name).count()
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
    let Some(ffi_record) = proc_rows_by_name(&name).nth(index) else {
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
    let record = static_catalog_by_name("pg_operator")
        .and_then(|table| {
            catalog_rows(table.oid).into_iter().find_map(|row| {
                let row_oid = catalog_row_value(table, &row, "oid").and_then(catalog_value_oid)?;
                (row_oid == Oid(oid))
                    .then(|| operator_row_to_ffi(&row))
                    .flatten()
            })
        })
        .or_else(|| builtin_operator_by_oid(Oid(oid)).map(operator_to_ffi));
    let Some(record) = record else {
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
    let left_type = Oid(left_type_oid);
    let right_type = Oid(right_type_oid);
    let namespace = Oid(namespace_oid);
    let normalized = name.trim().to_ascii_lowercase();
    let record = static_catalog_by_name("pg_operator")
        .and_then(|table| {
            catalog_rows(table.oid).into_iter().find_map(|row| {
                let record = operator_row_to_ffi(&row)?;
                let record_name = c_name_to_string(&record.name);
                (record_name == normalized
                    && record.left_type_oid == left_type.0
                    && record.right_type_oid == right_type.0
                    && record.namespace_oid == namespace.0)
                    .then_some(record)
            })
        })
        .or_else(|| {
            builtin_operator_by_signature(&name, left_type, right_type, namespace)
                .map(operator_to_ffi)
        });
    let Some(record) = record else {
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
/// C callers must pass a valid, null-terminated string pointer for `name`.
pub unsafe extern "C" fn fastpg_rust_catalog_operator_count_by_name(name: *const c_char) -> usize {
    let Ok(name) = (unsafe { c_str_to_string(name) }) else {
        return 0;
    };
    let normalized = name.trim().to_ascii_lowercase();
    static_catalog_by_name("pg_operator")
        .map(|table| {
            catalog_rows(table.oid)
                .into_iter()
                .filter(|row| {
                    catalog_row_value(table, row, "oprname")
                        .and_then(catalog_value_name)
                        .is_some_and(|row_name| row_name == normalized)
                })
                .count()
        })
        .unwrap_or_else(|| builtin_operators_by_name(&normalized).count())
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
    let normalized = name.trim().to_ascii_lowercase();
    let records = static_catalog_by_name("pg_operator")
        .map(|table| {
            catalog_rows(table.oid)
                .into_iter()
                .filter_map(|row| {
                    let record = operator_row_to_ffi(&row)?;
                    let record_name = c_name_to_string(&record.name);
                    (record_name == normalized).then_some(record)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            builtin_operators_by_name(&normalized)
                .map(operator_to_ffi)
                .collect()
        });
    let Some(record) = records.into_iter().nth(index) else {
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
/// C callers must pass a valid output pointer for `out`.
pub unsafe extern "C" fn fastpg_rust_catalog_cast_by_source_target(
    source_type_oid: u32,
    target_type_oid: u32,
    out: *mut FastPgRustCatalogCast,
) -> bool {
    if out.is_null() {
        return false;
    }
    let source_type = Oid(source_type_oid);
    let target_type = Oid(target_type_oid);
    let record = static_catalog_by_name("pg_cast")
        .and_then(|table| {
            catalog_rows(table.oid).into_iter().find_map(|row| {
                let record = cast_row_to_ffi(&row)?;
                (record.source_type_oid == source_type.0 && record.target_type_oid == target_type.0)
                    .then_some(record)
            })
        })
        .or_else(|| builtin_cast_by_source_target(source_type, target_type).map(cast_to_ffi));
    let Some(record) = record else {
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
/// C callers must pass valid row value and null arrays for `natts` entries.
/// Non-null string pointers must contain valid UTF-8.
pub unsafe extern "C" fn fastpg_rust_catalog_upsert_row(
    relation_oid: u32,
    row_id: u64,
    values: *const *const c_char,
    is_null: *const u8,
    natts: usize,
    row_id_out: *mut u64,
) -> bool {
    let result = (|| {
        let values = unsafe { nullable_c_str_array_to_strings(values, is_null, natts) }
            .map_err(invalid_ffi_argument)?;
        let pending_primary_key_rebuild = pg_index_heap_relation_from_upsert(relation_oid, &values);
        let row_id = upsert_catalog_row(Oid(relation_oid), row_id, values)?;
        if catalog_mutation_invalidates_primary_key_cache(relation_oid) {
            clear_primary_key_index_info_cache();
        }
        if let Some(relid) = pending_primary_key_rebuild {
            mark_primary_key_rebuild(relid);
        }
        if !row_id_out.is_null() {
            unsafe {
                *row_id_out = row_id;
            }
        }
        Ok::<(), CatalogError>(())
    })();

    result.is_ok()
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_catalog_delete_row(relation_oid: u32, row_id: u64) -> bool {
    let pending_primary_key_rebuild = pg_index_heap_relation_from_delete(relation_oid, row_id);
    let ok = delete_catalog_row(Oid(relation_oid), row_id).is_ok();
    if ok && catalog_mutation_invalidates_primary_key_cache(relation_oid) {
        clear_primary_key_index_info_cache();
    }
    if ok && let Some(relid) = pending_primary_key_rebuild {
        mark_primary_key_rebuild(relid);
    }
    ok
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_xact_begin() {
    begin_explicit_transaction();
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_xact_begin_implicit() {
    with_storage(|_state, session| session.ensure_transaction());
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_xact_commit() {
    commit_explicit_transaction();
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_xact_abort() {
    abort_explicit_transaction();
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

pub fn begin_explicit_transaction() {
    with_storage(|state, session| state.begin_explicit_transaction(session));
}

pub fn commit_explicit_transaction() {
    with_storage(|state, session| state.commit_explicit_transaction(session));
}

pub fn abort_explicit_transaction() {
    with_storage(|state, session| state.abort_explicit_transaction(session));
}

pub fn is_explicit_transaction() -> bool {
    with_storage(|state, session| state.is_explicit_transaction(session))
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_xact_is_explicit() -> bool {
    is_explicit_transaction()
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_subxact_begin() {
    with_storage(|_state, session| {
        session.ensure_transaction();
        session
            .transaction_stack
            .push(TransactionOverlay::savepoint());
        fastpg_catalog::begin_subtransaction();
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_subxact_commit() {
    with_storage(|state, session| {
        if session.transaction_stack.len() > 1 {
            state.commit_top_overlay(session);
            fastpg_catalog::commit_subtransaction();
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_subxact_abort() {
    with_storage(|_state, session| {
        if session.transaction_stack.len() > 1 {
            session.transaction_stack.pop();
            fastpg_catalog::abort_subtransaction();
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_relation_clear(relid: u32) {
    with_storage(|state, session| state.clear_relation(session, relid));
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_relation_swap(left_relid: u32, right_relid: u32) {
    with_storage(|state, session| {
        state.swap_relation_storage(session, left_relid, right_relid);
    });
    clear_primary_key_index_info_cache();
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_relation_row_count(relid: u32) -> usize {
    with_storage(|state, session| state.visible_row_count(session, relid))
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_catalog_row_count(relid: u32) -> usize {
    catalog_row_count(Oid(relid)).unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_relation_contains_row(relid: u32, row_id: u64) -> bool {
    with_storage(|state, session| state.find_visible_row(session, relid, row_id).is_some())
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_storage_committed_bytes() -> usize {
    with_storage(|state, _session| state.committed_bytes())
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_storage_fixture_bytes() -> usize {
    with_storage(|state, _session| state.fixture_bytes())
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_storage_epoch_bytes() -> usize {
    with_storage(|state, _session| state.epoch_bytes())
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_storage_transaction_bytes() -> usize {
    with_storage(|_state, session| session.transaction_bytes())
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_storage_scan_bytes() -> usize {
    with_storage(|_state, session| session.scan_bytes())
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_storage_index_bytes() -> usize {
    with_storage(|state, _session| state.index_bytes())
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_storage_arena_rewinds() -> u64 {
    STORAGE_ARENA_REWINDS.load(Ordering::Relaxed)
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_storage_memory_limit_rejections() -> u64 {
    STORAGE_MEMORY_LIMIT_REJECTIONS.load(Ordering::Relaxed)
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_storage_set_limits(
    max_committed_bytes: usize,
    max_fixture_bytes: usize,
    max_epoch_bytes: usize,
    max_transaction_bytes: usize,
    max_row_bytes: usize,
    max_scan_bytes: usize,
) {
    with_storage(|state, _session| {
        state.set_limits(StorageLimits {
            max_committed_bytes: limit_from_ffi(max_committed_bytes),
            max_fixture_bytes: limit_from_ffi(max_fixture_bytes),
            max_epoch_bytes: limit_from_ffi(max_epoch_bytes),
            max_transaction_bytes: limit_from_ffi(max_transaction_bytes),
            max_row_bytes: limit_from_ffi(max_row_bytes),
            max_scan_bytes: limit_from_ffi(max_scan_bytes),
        });
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_storage_reset_limits() {
    with_storage(|state, _session| state.reset_limits());
    clear_catalog_scan_cache();
    clear_last_storage_error();
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_storage_discard_fixture_region() {
    with_storage(|state, _session| state.discard_fixture_region());
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_storage_discard_epoch_region() {
    with_storage(|state, _session| state.discard_epoch_region());
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output buffers when they are non-null.
pub unsafe extern "C" fn fastpg_rust_storage_last_error(
    sqlstate_out: *mut c_char,
    sqlstate_len: usize,
    message_out: *mut c_char,
    message_len: usize,
) -> bool {
    let Some(error) = last_storage_error() else {
        return false;
    };
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
    true
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
    let Some(index_spec) = cached_primary_key_index(Oid(index_relid)).map(|index| index.spec)
    else {
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
        let key = index_key_for_datums(&index_spec, values, is_null)?;
        Some(state.find_visible_row_by_index_key(
            session,
            index_spec.relation_oid.0,
            &index_spec,
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
/// C callers must pass valid value/null arrays for `nkeys` entries and a valid
/// output pointer when `row_id_out` is non-null.
pub unsafe extern "C" fn fastpg_rust_unique_index_conflict(
    index_relid: u32,
    values: *const usize,
    is_null: *const u8,
    nkeys: usize,
    replacing_row_id: u64,
    row_id_out: *mut u64,
) -> bool {
    if nkeys > 0 && (values.is_null() || is_null.is_null()) {
        return false;
    }
    let Some(index_spec) = cached_primary_key_index(Oid(index_relid)).map(|index| index.spec)
    else {
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
        let key = index_key_for_datums(&index_spec, values, is_null)?;
        Some(state.find_visible_row_by_index_key_excluding(
            session,
            index_spec.relation_oid.0,
            &index_spec,
            &key,
            Some(replacing_row_id),
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
    clear_last_storage_error();
    unsafe {
        relation_insert_impl(
            RawRowInput {
                relid,
                values,
                is_null,
                byval,
                value_lens,
                natts,
            },
            row_id_out,
            UniqueCheck::Enforce,
        )
    }
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_relation_insert_unchecked(
    relid: u32,
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
    row_id_out: *mut u64,
) -> bool {
    clear_last_storage_error();
    unsafe {
        relation_insert_impl(
            RawRowInput {
                relid,
                values,
                is_null,
                byval,
                value_lens,
                natts,
            },
            row_id_out,
            UniqueCheck::Skip,
        )
    }
}

#[derive(Clone, Copy)]
struct RawRowInput {
    relid: u32,
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
}

#[derive(Clone, Copy)]
enum UniqueCheck {
    Enforce,
    Skip,
}

unsafe fn relation_insert_impl(
    input: RawRowInput,
    row_id_out: *mut u64,
    unique_check: UniqueCheck,
) -> bool {
    let Some((values, is_null, byval, value_lens)) = (unsafe {
        row_input_arrays(
            input.values,
            input.is_null,
            input.byval,
            input.value_lens,
            input.natts,
        )
    }) else {
        set_last_storage_error(invalid_ffi_argument("invalid row input arrays".to_owned()));
        return false;
    };

    let result = with_storage(|state, session| -> Result<bool, CatalogError> {
        let estimated_row_bytes = estimated_input_row_bytes(is_null, byval, value_lens);
        state.check_row_limit(estimated_row_bytes)?;
        state.check_transaction_limit(session, estimated_row_bytes)?;
        state.check_committed_projection_limit(session, estimated_row_bytes)?;

        let row_id = match state
            .relations
            .entry(input.relid)
            .or_default()
            .allocate_row_id()
        {
            Some(row_id) => row_id,
            None => return Ok(false),
        };

        session.ensure_transaction();
        let (cells, checkpoint) = {
            let segment = session
                .transaction_stack
                .last_mut()
                .expect("transaction was just ensured")
                .row_segment_mut(input.relid);
            let checkpoint = segment.checkpoint();

            match copy_cells_to_segment(segment, values, is_null, byval, value_lens) {
                Some(cells) => (cells, checkpoint),
                None => {
                    segment.rewind_to(checkpoint);
                    return Err(invalid_ffi_argument(
                        "invalid by-reference row input".to_owned(),
                    ));
                }
            }
        };

        let row = Row { row_id, cells };
        let index_bytes = primary_index_spec_for_relation_oid(Oid(input.relid))
            .and_then(|index_spec| index_key_for_row(&index_spec, &row))
            .map(|key| estimated_index_key_bytes(&key))
            .unwrap_or(0);
        if let Err(error) = state.check_committed_projection_limit(session, index_bytes) {
            let segment = session
                .transaction_stack
                .last_mut()
                .expect("transaction was just ensured")
                .row_segment_mut(input.relid);
            segment.rewind_to(checkpoint);
            return Err(error);
        }
        if matches!(unique_check, UniqueCheck::Enforce)
            && state
                .unique_index_conflict(session, input.relid, &row, None)
                .is_some()
        {
            let segment = session
                .transaction_stack
                .last_mut()
                .expect("transaction was just ensured")
                .row_segment_mut(input.relid);
            segment.rewind_to(checkpoint);
            return Ok(false);
        }

        let segment = session
            .transaction_stack
            .last_mut()
            .expect("transaction was just ensured")
            .row_segment_mut(input.relid);
        segment.push_row(row);
        if !row_id_out.is_null() {
            unsafe {
                *row_id_out = row_id;
            }
        }
        Ok(true)
    });

    match result {
        Ok(inserted) => inserted,
        Err(error) => {
            set_last_storage_error(error);
            false
        }
    }
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
    clear_last_storage_error();
    unsafe {
        relation_update_impl(
            RawRowInput {
                relid,
                values,
                is_null,
                byval,
                value_lens,
                natts,
            },
            row_id,
            UniqueCheck::Enforce,
        )
    }
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output pointers where required; any C strings or
/// arrays must be valid for reads of the specified length for the call.
pub unsafe extern "C" fn fastpg_rust_relation_update_unchecked(
    relid: u32,
    row_id: u64,
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
) -> bool {
    clear_last_storage_error();
    unsafe {
        relation_update_impl(
            RawRowInput {
                relid,
                values,
                is_null,
                byval,
                value_lens,
                natts,
            },
            row_id,
            UniqueCheck::Skip,
        )
    }
}

unsafe fn relation_update_impl(input: RawRowInput, row_id: u64, unique_check: UniqueCheck) -> bool {
    if row_id == 0 {
        return false;
    }
    let Some((values, is_null, byval, value_lens)) = (unsafe {
        row_input_arrays(
            input.values,
            input.is_null,
            input.byval,
            input.value_lens,
            input.natts,
        )
    }) else {
        set_last_storage_error(invalid_ffi_argument("invalid row input arrays".to_owned()));
        return false;
    };

    let result = with_storage(|state, session| -> Result<bool, CatalogError> {
        if !state.visible_row_exists(session, input.relid, row_id) {
            return Ok(false);
        }

        let estimated_row_bytes = estimated_input_row_bytes(is_null, byval, value_lens);
        state.check_row_limit(estimated_row_bytes)?;
        state.check_transaction_limit(session, estimated_row_bytes)?;
        state.check_committed_projection_limit(session, estimated_row_bytes)?;

        session.ensure_transaction();
        let (cells, checkpoint) = {
            let overlay = session
                .transaction_stack
                .last_mut()
                .expect("transaction was just ensured");
            let segment = overlay.row_segment_mut(input.relid);
            let checkpoint = segment.checkpoint();
            match copy_cells_to_segment(segment, values, is_null, byval, value_lens) {
                Some(cells) => (cells, checkpoint),
                None => {
                    segment.rewind_to(checkpoint);
                    return Err(invalid_ffi_argument(
                        "invalid by-reference row input".to_owned(),
                    ));
                }
            }
        };
        let row = Row { row_id, cells };
        let index_bytes = primary_index_spec_for_relation_oid(Oid(input.relid))
            .and_then(|index_spec| index_key_for_row(&index_spec, &row))
            .map(|key| estimated_index_key_bytes(&key))
            .unwrap_or(0);
        if let Err(error) = state.check_committed_projection_limit(session, index_bytes) {
            let overlay = session
                .transaction_stack
                .last_mut()
                .expect("transaction was just ensured");
            let segment = overlay.row_segment_mut(input.relid);
            segment.rewind_to(checkpoint);
            return Err(error);
        }
        if matches!(unique_check, UniqueCheck::Enforce)
            && state
                .unique_index_conflict(session, input.relid, &row, Some(row_id))
                .is_some()
        {
            let overlay = session
                .transaction_stack
                .last_mut()
                .expect("transaction was just ensured");
            let segment = overlay.row_segment_mut(input.relid);
            segment.rewind_to(checkpoint);
            return Ok(false);
        }

        let overlay = session
            .transaction_stack
            .last_mut()
            .expect("transaction was just ensured");
        overlay
            .deleted_row_ids
            .entry(input.relid)
            .or_default()
            .insert(row_id);
        let segment = overlay.row_segment_mut(input.relid);
        segment.remove_row_id(row_id);
        segment.push_row(row);
        Ok(true)
    });

    match result {
        Ok(updated) => updated,
        Err(error) => {
            set_last_storage_error(error);
            false
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_relation_delete(relid: u32, row_id: u64) -> bool {
    if row_id == 0 {
        return false;
    }

    with_storage(|state, session| {
        if !state.visible_row_exists(session, relid, row_id) {
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
            segment.remove_row_id(row_id);
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
            cells.push(Cell::null());
            continue;
        }

        if byval[index] != 0 {
            cells.push(Cell::by_value(values[index]));
            continue;
        }

        let len = value_lens[index];
        if values[index] == 0 && len > 0 {
            return None;
        }
        let source = if len == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(values[index] as *const u8, len) }
        };
        cells.push(Cell::by_ref(segment.alloc_bytes(source)));
    }
    Some(cells)
}

fn datum(value: usize) -> Cell {
    Cell::by_value(value)
}

fn int2_datum(value: i16) -> Cell {
    datum(value as usize)
}

fn int4_datum(value: i32) -> Cell {
    datum(value as usize)
}

fn int8_datum(value: i64) -> Cell {
    datum(value as usize)
}

fn null_datum() -> Cell {
    Cell::null()
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

fn float8_datum(value: f64) -> Cell {
    datum(value.to_bits() as usize)
}

fn byref_datum(bytes: &[u8], region: &mut StorageRegion) -> Cell {
    Cell::by_ref(region.alloc_bytes(bytes))
}

fn name_datum(value: &str, region: &mut StorageRegion) -> Cell {
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
    byref_datum(&bytes, region)
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

fn push_u64_ne(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_ne_bytes());
}

fn push_i16_ne(bytes: &mut Vec<u8>, value: i16) {
    bytes.extend_from_slice(&value.to_ne_bytes());
}

fn push_i8(bytes: &mut Vec<u8>, value: i8) {
    bytes.push(value as u8);
}

fn push_1d_array_header(
    bytes: &mut Vec<u8>,
    total_len: usize,
    elemtype: Oid,
    element_count: usize,
    lower_bound: i32,
) {
    push_u32_ne(bytes, varlena_4b_header(total_len));
    push_i32_ne(bytes, 1);
    push_i32_ne(bytes, 0);
    push_u32_ne(bytes, elemtype.0);
    push_i32_ne(bytes, element_count.min(i32::MAX as usize) as i32);
    push_i32_ne(bytes, lower_bound);
}

fn parse_pg_array_elements(value: &str) -> Option<Vec<Option<String>>> {
    let body = value.strip_prefix('{')?.strip_suffix('}')?;
    if body.is_empty() {
        return Some(Vec::new());
    }

    let mut elements = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut current_was_quoted = false;

    for ch in body.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if quoted {
            match ch {
                '\\' => escaped = true,
                '"' => quoted = false,
                _ => current.push(ch),
            }
            continue;
        }
        match ch {
            '"' => {
                quoted = true;
                current_was_quoted = true;
            }
            ',' => {
                if !current_was_quoted && current == "NULL" {
                    elements.push(None);
                } else {
                    elements.push(Some(std::mem::take(&mut current)));
                }
                current_was_quoted = false;
            }
            _ => current.push(ch),
        }
    }

    if quoted || escaped {
        return None;
    }
    if !current_was_quoted && current == "NULL" {
        elements.push(None);
    } else {
        elements.push(Some(current));
    }
    Some(elements)
}

fn parse_pg_array_values<T>(value: &str, parse: impl Fn(&str) -> Option<T>) -> Option<Vec<T>> {
    parse_pg_array_elements(value)?
        .into_iter()
        .map(|element| parse(element.as_deref()?))
        .collect()
}

fn int2vector_datum(values: &[i16], region: &mut StorageRegion) -> Cell {
    let total_len = 24 + std::mem::size_of_val(values);
    let mut bytes = Vec::with_capacity(total_len);
    push_1d_array_header(&mut bytes, total_len, INT2_OID, values.len(), 0);
    for value in values {
        push_i16_ne(&mut bytes, *value);
    }
    byref_datum(&bytes, region)
}

fn int2array_datum(values: &[i16], region: &mut StorageRegion) -> Cell {
    let total_len = 24 + std::mem::size_of_val(values);
    let mut bytes = Vec::with_capacity(total_len);
    push_1d_array_header(&mut bytes, total_len, INT2_OID, values.len(), 1);
    for value in values {
        push_i16_ne(&mut bytes, *value);
    }
    byref_datum(&bytes, region)
}

fn oidvector_datum(values: &[u32], region: &mut StorageRegion) -> Cell {
    let total_len = 24 + std::mem::size_of_val(values);
    let mut bytes = Vec::with_capacity(total_len);
    push_1d_array_header(&mut bytes, total_len, OID_OID, values.len(), 0);
    for value in values {
        push_u32_ne(&mut bytes, *value);
    }
    byref_datum(&bytes, region)
}

fn oidarray_datum(values: &[u32], region: &mut StorageRegion) -> Cell {
    let total_len = 24 + std::mem::size_of_val(values);
    let mut bytes = Vec::with_capacity(total_len);
    push_1d_array_header(&mut bytes, total_len, OID_OID, values.len(), 1);
    for value in values {
        push_u32_ne(&mut bytes, *value);
    }
    byref_datum(&bytes, region)
}

fn int4array_datum(values: &[i32], region: &mut StorageRegion) -> Cell {
    let total_len = 24 + std::mem::size_of_val(values);
    let mut bytes = Vec::with_capacity(total_len);
    push_1d_array_header(&mut bytes, total_len, INT4_OID, values.len(), 1);
    for value in values {
        push_i32_ne(&mut bytes, *value);
    }
    byref_datum(&bytes, region)
}

fn chararray_datum(values: &[u8], region: &mut StorageRegion) -> Cell {
    let total_len = 24 + values.len();
    let mut bytes = Vec::with_capacity(total_len);
    push_1d_array_header(
        &mut bytes,
        total_len,
        fastpg_catalog::CHAR_OID,
        values.len(),
        1,
    );
    for value in values {
        push_i8(&mut bytes, *value as i8);
    }
    byref_datum(&bytes, region)
}

fn textarray_datum(values: &[String], region: &mut StorageRegion) -> Cell {
    let mut bytes = Vec::new();
    push_u32_ne(&mut bytes, 0);
    push_i32_ne(&mut bytes, 1);
    push_i32_ne(&mut bytes, 0);
    push_u32_ne(&mut bytes, TEXT_OID.0);
    push_i32_ne(&mut bytes, values.len().min(i32::MAX as usize) as i32);
    push_i32_ne(&mut bytes, 1);
    for value in values {
        while bytes.len() % 4 != 0 {
            bytes.push(0);
        }
        let element_len = 4 + value.len();
        push_u32_ne(&mut bytes, varlena_4b_header(element_len));
        bytes.extend_from_slice(value.as_bytes());
    }
    let total_len = bytes.len();
    bytes[0..4].copy_from_slice(&varlena_4b_header(total_len).to_ne_bytes());
    byref_datum(&bytes, region)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AclItemDatum {
    grantee: Oid,
    grantor: Oid,
    rights: u64,
}

fn acl_privilege_bit(ch: char) -> Option<u64> {
    match ch {
        'a' => Some(1 << 0),
        'r' => Some(1 << 1),
        'w' => Some(1 << 2),
        'd' => Some(1 << 3),
        'D' => Some(1 << 4),
        'x' => Some(1 << 5),
        't' => Some(1 << 6),
        'X' => Some(1 << 7),
        'U' => Some(1 << 8),
        'C' => Some(1 << 9),
        'T' => Some(1 << 10),
        'c' => Some(1 << 11),
        's' => Some(1 << 12),
        'A' => Some(1 << 13),
        'm' => Some(1 << 14),
        _ => None,
    }
}

fn acl_role_oid(name: &str) -> Option<Oid> {
    if name.is_empty() {
        return Some(Oid(0));
    }
    if let Ok(oid) = name.parse::<u32>() {
        return Some(Oid(oid));
    }

    let table = static_catalog_by_name("pg_authid")?;
    catalog_rows(table.oid)
        .into_iter()
        .find(|row| {
            catalog_row_value(table, row, "rolname")
                .and_then(catalog_value_name)
                .is_some_and(|rolname| rolname == name)
        })
        .and_then(|row| catalog_row_value(table, &row, "oid").and_then(catalog_value_oid))
}

fn parse_aclitem(value: &str) -> Option<AclItemDatum> {
    let (grantee, rest) = value.split_once('=')?;
    let (privileges, grantor) = rest.split_once('/').unwrap_or((rest, "10"));
    let grantee = acl_role_oid(grantee)?;
    let grantor = acl_role_oid(grantor)?;
    let mut privs = 0u64;
    let mut goptions = 0u64;
    let mut last_privilege = 0u64;

    for ch in privileges.chars() {
        if ch == '*' {
            if last_privilege == 0 {
                return None;
            }
            goptions |= last_privilege;
            continue;
        }
        let bit = acl_privilege_bit(ch)?;
        privs |= bit;
        last_privilege = bit;
    }

    Some(AclItemDatum {
        grantee,
        grantor,
        rights: privs | (goptions << 32),
    })
}

fn aclitem_array_datum(value: &str, region: &mut StorageRegion) -> Option<Cell> {
    let values = parse_pg_array_values(value, parse_aclitem)?;
    let total_len = 24 + values.len() * 16;
    let mut bytes = Vec::with_capacity(total_len);
    push_1d_array_header(&mut bytes, total_len, ACLITEM_OID, values.len(), 1);
    for value in values {
        push_u32_ne(&mut bytes, value.grantee.0);
        push_u32_ne(&mut bytes, value.grantor.0);
        push_u64_ne(&mut bytes, value.rights);
    }
    Some(byref_datum(&bytes, region))
}

fn parse_lsn(value: &str) -> Option<u64> {
    let (high, low) = value.split_once('/')?;
    let high = u64::from_str_radix(high, 16).ok()?;
    let low = u64::from_str_radix(low, 16).ok()?;
    Some((high << 32) | low)
}

fn text_datum(value: &str, region: &mut StorageRegion) -> Cell {
    byref_datum(&postgres_text_payload(value.as_bytes()), region)
}

fn catalog_value_to_cell(
    column: &fastpg_catalog::StaticCatalogColumn,
    value: &CatalogValue,
    region: &mut StorageRegion,
) -> Cell {
    match value {
        CatalogValue::Null => null_datum(),
        CatalogValue::Bool(value) => bool_datum(*value),
        CatalogValue::Char(value) => char_datum(*value),
        CatalogValue::Int16(value) => int2_datum(*value),
        CatalogValue::Int32(value) => int4_datum(*value),
        CatalogValue::Float32(value) => float4_datum(*value),
        CatalogValue::Oid(value) => datum(value.0 as usize),
        CatalogValue::Name(value) => name_datum(value, region),
        CatalogValue::Text(value) => text_datum(value, region),
        CatalogValue::OidVector(values) => {
            let values = values.iter().map(|oid| oid.0).collect::<Vec<_>>();
            oidvector_datum(&values, region)
        }
        CatalogValue::Int2Vector(values) => int2vector_datum(values, region),
        CatalogValue::Raw(value) => match column.type_oid {
            NAME_OID => name_datum(value, region),
            TEXT_OID | PG_NODE_TREE_OID => text_datum(value, region),
            _ if column.type_name.starts_with("reg") => {
                fastpg_catalog::resolve_generated_catalog_oid_name(value)
                    .map(|value| datum(value.0 as usize))
                    .unwrap_or_else(|| datum(0))
            }
            OID_OID | XID_OID | CID_OID => value
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
            INT8_OID => value
                .parse::<i64>()
                .map(int8_datum)
                .unwrap_or_else(|_| int8_datum(0)),
            FLOAT8_OID => value
                .parse::<f64>()
                .map(float8_datum)
                .unwrap_or_else(|_| float8_datum(0.0)),
            TID_OID => null_datum(),
            LSN_OID => parse_lsn(value)
                .map(|value| datum(value as usize))
                .unwrap_or_else(|| datum(0)),
            OIDVECTOR_OID => {
                let values = value
                    .split_whitespace()
                    .filter_map(|part| part.parse::<u32>().ok())
                    .collect::<Vec<_>>();
                oidvector_datum(&values, region)
            }
            INT2VECTOR_OID => {
                let values = value
                    .split_whitespace()
                    .filter_map(|part| part.parse::<i16>().ok())
                    .collect::<Vec<_>>();
                int2vector_datum(&values, region)
            }
            INT2_ARRAY_OID => parse_pg_array_values(value, |part| part.parse::<i16>().ok())
                .map(|values| int2array_datum(&values, region))
                .unwrap_or_else(null_datum),
            INT4_ARRAY_OID => parse_pg_array_values(value, |part| part.parse::<i32>().ok())
                .map(|values| int4array_datum(&values, region))
                .unwrap_or_else(null_datum),
            OID_ARRAY_OID => parse_pg_array_values(value, |part| part.parse::<u32>().ok())
                .map(|values| oidarray_datum(&values, region))
                .unwrap_or_else(null_datum),
            CHAR_ARRAY_OID => parse_pg_array_values(value, |part| part.as_bytes().first().copied())
                .map(|values| chararray_datum(&values, region))
                .unwrap_or_else(null_datum),
            TEXT_ARRAY_OID => parse_pg_array_values(value, |part| Some(part.to_owned()))
                .map(|values| textarray_datum(&values, region))
                .unwrap_or_else(null_datum),
            ACLITEM_ARRAY_OID => aclitem_array_datum(value, region).unwrap_or_else(null_datum),
            ANYARRAY_OID => null_datum(),
            _ => null_datum(),
        },
    }
}

fn catalog_scan_state_from_rows(
    relation_oid: Oid,
    catalog_rows: impl IntoIterator<Item = CatalogRow>,
) -> Option<ScanState> {
    let table = static_catalog_by_relation_oid(relation_oid)?;
    let mut region = StorageRegion::new(StorageRegionKind::Scan);
    let mut rows = Vec::new();
    for catalog_row in catalog_rows {
        if catalog_row.values.len() != table.columns.len() {
            continue;
        }
        let row = Row {
            row_id: catalog_row.row_id,
            cells: table
                .columns
                .iter()
                .zip(catalog_row.values.iter())
                .map(|(column, value)| catalog_value_to_cell(column, value, &mut region))
                .collect(),
        };
        region.account_row(&row);
        rows.push(row);
    }
    Some(ScanState {
        rows,
        region,
        shared_scan: None,
        next_index: 0,
    })
}

fn catalog_scan_state_uncached(relation_oid: Oid) -> Option<ScanState> {
    catalog_scan_state_from_rows(relation_oid, catalog_rows(relation_oid))
}

fn catalog_scan_state_filtered(
    relation_oid: Oid,
    filters: &[CatalogRowFilter],
) -> Option<ScanState> {
    if filters.is_empty() {
        return catalog_scan_state(relation_oid);
    }
    if has_uncommitted_catalog_changes() {
        return catalog_scan_state_filtered_uncached(relation_oid, filters);
    }
    let cache_key = CatalogScanFilterCacheKey {
        relation_oid: relation_oid.0,
        filters: filters.to_vec(),
    };
    if let Some(cached) =
        with_catalog_scan_cache(|cache| cache.filtered_entries.get(&cache_key).map(Arc::clone))
    {
        return Some(scan_state_from_cached_catalog_scan(cached));
    }
    let scan = catalog_scan_state_filtered_uncached(relation_oid, filters)?;
    let cached = Arc::new(CachedCatalogScan {
        rows: scan.rows,
        region: Arc::new(scan.region),
    });
    with_catalog_scan_cache(|cache| {
        cache
            .filtered_entries
            .insert(cache_key, Arc::clone(&cached));
    });
    Some(scan_state_from_cached_catalog_scan(cached))
}

fn catalog_scan_state_filtered_uncached(
    relation_oid: Oid,
    filters: &[CatalogRowFilter],
) -> Option<ScanState> {
    catalog_scan_state_from_rows(
        relation_oid,
        catalog_rows_matching_filters(relation_oid, filters),
    )
}

fn catalog_scan_state(relation_oid: Oid) -> Option<ScanState> {
    if has_uncommitted_catalog_changes() {
        return catalog_scan_state_uncached(relation_oid);
    }
    if let Some(cached) =
        with_catalog_scan_cache(|cache| cache.entries.get(&relation_oid.0).map(Arc::clone))
    {
        return Some(scan_state_from_cached_catalog_scan(cached));
    }
    let scan = catalog_scan_state_uncached(relation_oid)?;
    let cached = Arc::new(CachedCatalogScan {
        rows: scan.rows,
        region: Arc::new(scan.region),
    });
    with_catalog_scan_cache(|cache| {
        cache.entries.insert(relation_oid.0, Arc::clone(&cached));
    });
    Some(scan_state_from_cached_catalog_scan(cached))
}

unsafe fn catalog_name_filter_value(datum: usize) -> Option<String> {
    if datum == 0 {
        return None;
    }
    let bytes = unsafe { slice::from_raw_parts(datum as *const u8, NAMEDATALEN) };
    let len = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..len]).ok().map(str::to_owned)
}

unsafe fn catalog_scan_filters_from_datums(
    relation_oid: Oid,
    attnums: *const i16,
    values: *const usize,
    nkeys: usize,
) -> Vec<CatalogRowFilter> {
    if nkeys == 0 || attnums.is_null() || values.is_null() {
        return Vec::new();
    }
    let Some(table) = static_catalog_by_relation_oid(relation_oid) else {
        return Vec::new();
    };
    let attnums = unsafe { slice::from_raw_parts(attnums, nkeys) };
    let values = unsafe { slice::from_raw_parts(values, nkeys) };
    let mut filters = Vec::with_capacity(nkeys);
    for (attnum, datum) in attnums.iter().copied().zip(values.iter().copied()) {
        let Some(column_index) = attnum
            .checked_sub(1)
            .and_then(|attnum| usize::try_from(attnum).ok())
        else {
            continue;
        };
        let Some(column) = table.columns.get(column_index) else {
            continue;
        };
        let value = match column.type_oid {
            BOOL_OID => CatalogFilterValue::Bool(datum != 0),
            CHAR_OID => CatalogFilterValue::Char(datum as u8),
            INT2_OID => CatalogFilterValue::Int16(datum as i16),
            INT4_OID => CatalogFilterValue::Int32(datum as i32),
            OID_OID | REGCLASS_OID => CatalogFilterValue::Oid(Oid(datum as u32)),
            NAME_OID => {
                let Some(value) = (unsafe { catalog_name_filter_value(datum) }) else {
                    continue;
                };
                CatalogFilterValue::Name(value)
            }
            _ => continue,
        };
        filters.push(CatalogRowFilter { attnum, value });
    }
    filters
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_scan_begin(relid: u32) -> u64 {
    clear_last_storage_error();
    let virtual_scan =
        virtual_catalog_by_relation_oid(Oid(relid)).and_then(|_| catalog_scan_state(Oid(relid)));

    let result = with_storage(|state, session| -> Result<u64, CatalogError> {
        let scan = match virtual_scan {
            Some(scan) => scan,
            None => {
                state.relations.entry(relid).or_default();
                state.visible_scan_state(session, relid)?
            }
        };
        state.check_scan_limit(scan.bytes())?;
        let handle = session.allocate_scan_handle();
        session.scans.insert(handle, scan);
        Ok(handle)
    });

    match result {
        Ok(handle) => handle,
        Err(error) => {
            set_last_storage_error(error);
            0
        }
    }
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass `attnums` and `values` arrays with at least `nkeys`
/// entries when `nkeys` is non-zero. Datum values are interpreted according to
/// the target catalog column metadata.
pub unsafe extern "C" fn fastpg_rust_scan_begin_filtered(
    relid: u32,
    attnums: *const i16,
    values: *const usize,
    nkeys: usize,
) -> u64 {
    clear_last_storage_error();
    let relation_oid = Oid(relid);
    let filters = unsafe { catalog_scan_filters_from_datums(relation_oid, attnums, values, nkeys) };
    let virtual_scan = virtual_catalog_by_relation_oid(relation_oid)
        .and_then(|_| catalog_scan_state_filtered(relation_oid, &filters));

    let result = with_storage(|state, session| -> Result<u64, CatalogError> {
        let scan = match virtual_scan {
            Some(scan) => scan,
            None => {
                state.relations.entry(relid).or_default();
                state.visible_scan_state(session, relid)?
            }
        };
        state.check_scan_limit(scan.bytes())?;
        let handle = session.allocate_scan_handle();
        session.scans.insert(handle, scan);
        Ok(handle)
    });

    match result {
        Ok(handle) => handle,
        Err(error) => {
            set_last_storage_error(error);
            0
        }
    }
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
    with_storage(|_state, session| {
        let scan = match session.scans.get_mut(&scan_handle) {
            Some(scan) => scan,
            None => return false,
        };

        let row_count = scan.rows().len();
        let row_index = if forward != 0 {
            if scan.next_index >= row_count {
                return false;
            }
            let row_index = scan.next_index;
            scan.next_index += 1;
            row_index
        } else {
            if scan.next_index == 0 {
                scan.next_index = row_count;
            }
            if scan.next_index == 0 {
                return false;
            }
            scan.next_index -= 1;
            scan.next_index
        };

        let Some(row) = scan.rows().get(row_index) else {
            return false;
        };
        unsafe { copy_row_to_outputs(row, values_out, is_null_out, natts, row_id_out) }
    })
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
        values_out[index] = cell.output_value();
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
    use fastpg_catalog::{
        INFORMATION_SCHEMA_NAMESPACE_OID, PUBLIC_NAMESPACE_OID, builtin_namespaces,
        static_catalog_by_name, upsert_catalog_row,
    };
    use std::collections::BTreeMap;
    use std::ffi::CString;
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
            fastpg_rust_storage_reset_limits();
            fastpg_rust_xact_abort();
        }
    }

    fn test_guard() -> TestGuard {
        let guard = TEST_LOCK.lock().expect("test lock poisoned");
        fastpg_rust_storage_reset_limits();
        fastpg_rust_xact_abort();
        clear_primary_key_index_info_cache();
        clear_relation_oid_by_name_cache();
        clear_relation_exists_cache();
        TestGuard { _guard: guard }
    }

    fn next_relid() -> u32 {
        NEXT_RELID.fetch_add(1, Ordering::Relaxed)
    }

    fn i32_from_bytes(bytes: &[u8], offset: usize) -> i32 {
        i32::from_ne_bytes(bytes[offset..offset + 4].try_into().expect("i32 bytes"))
    }

    fn u32_from_bytes(bytes: &[u8], offset: usize) -> u32 {
        u32::from_ne_bytes(bytes[offset..offset + 4].try_into().expect("u32 bytes"))
    }

    fn u64_from_bytes(bytes: &[u8], offset: usize) -> u64 {
        u64::from_ne_bytes(bytes[offset..offset + 8].try_into().expect("u64 bytes"))
    }

    #[test]
    fn catalog_arrays_use_sql_array_bounds_not_vector_bounds() {
        let mut region = StorageRegion::new(StorageRegionKind::Scan);
        let _unaligned_prefix = text_datum("x", &mut region);
        let oid_array = oidarray_datum(&[23, 26], &mut region);
        assert_eq!(oid_array.output_value() % DATUM_ALIGNMENT, 0);
        let oid_array_bytes = oid_array.byref_bytes().expect("oid array bytes");
        assert_eq!(u32_from_bytes(oid_array_bytes, 12), OID_OID.0);
        assert_eq!(i32_from_bytes(oid_array_bytes, 16), 2);
        assert_eq!(i32_from_bytes(oid_array_bytes, 20), 1);
        assert_eq!(u32_from_bytes(oid_array_bytes, 24), 23);
        assert_eq!(u32_from_bytes(oid_array_bytes, 28), 26);

        let oid_vector = oidvector_datum(&[23, 26], &mut region);
        let oid_vector_bytes = oid_vector.byref_bytes().expect("oidvector bytes");
        assert_eq!(i32_from_bytes(oid_vector_bytes, 20), 0);
    }

    #[test]
    fn aclitem_arrays_render_as_postgres_acl_datums() {
        let _guard = test_guard();
        upsert_named_catalog_row(
            "pg_authid",
            60_001,
            &[("oid", "60001"), ("rolname", "regress_acl_user")],
        );

        let mut region = StorageRegion::new(StorageRegionKind::Scan);
        let cell = aclitem_array_datum(
            "{regress_acl_user=a*r/regress_acl_user,=r/regress_acl_user}",
            &mut region,
        )
        .expect("aclitem array");
        let bytes = cell.byref_bytes().expect("aclitem array bytes");
        assert_eq!(u32_from_bytes(bytes, 12), ACLITEM_OID.0);
        assert_eq!(i32_from_bytes(bytes, 16), 2);
        assert_eq!(i32_from_bytes(bytes, 20), 1);
        assert_eq!(u32_from_bytes(bytes, 24), 60_001);
        assert_eq!(u32_from_bytes(bytes, 28), 60_001);
        assert_eq!(u64_from_bytes(bytes, 32), (1 | 2) | (1u64 << 32));
        assert_eq!(u32_from_bytes(bytes, 40), 0);
        assert_eq!(u32_from_bytes(bytes, 44), 60_001);
        assert_eq!(u64_from_bytes(bytes, 48), 2);
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

    fn upsert_named_catalog_row_via_ffi(
        table_name: &str,
        row_id: u64,
        values: &[(&str, &str)],
    ) -> u64 {
        let table = static_catalog_by_name(table_name).expect("catalog table");
        let values = values
            .iter()
            .map(|(column, value)| (*column, (*value).to_owned()))
            .collect::<BTreeMap<_, _>>();
        let c_values = table
            .columns
            .iter()
            .map(|column| {
                values
                    .get(column.name)
                    .map(|value| CString::new(value.as_str()).expect("catalog value has no NUL"))
            })
            .collect::<Vec<_>>();
        let value_ptrs = c_values
            .iter()
            .map(|value| value.as_ref().map_or(ptr::null(), |value| value.as_ptr()))
            .collect::<Vec<_>>();
        let nulls = c_values
            .iter()
            .map(|value| u8::from(value.is_none()))
            .collect::<Vec<_>>();
        let mut row_id_out = 0;
        unsafe {
            assert!(fastpg_rust_catalog_upsert_row(
                table.oid.0,
                row_id,
                value_ptrs.as_ptr(),
                nulls.as_ptr(),
                value_ptrs.len(),
                &mut row_id_out,
            ));
        }
        row_id_out
    }

    fn relation_oid_by_name_via_ffi(name: &str, namespace_oid: u32) -> Option<u32> {
        let name = CString::new(name).expect("relation name has no NUL");
        let mut oid = 0;
        unsafe {
            fastpg_rust_catalog_relation_oid_by_name(name.as_ptr(), namespace_oid, &mut oid)
                .then_some(oid)
        }
    }

    fn install_primary_key_test_catalog() -> (u32, Oid) {
        let relid = 50_100;
        let type_oid = 50_101;
        let index_oid = Oid(50_102);
        let constraint_oid = 50_103;

        upsert_named_catalog_row(
            "pg_class",
            relid as u64,
            &[
                ("oid", "50100"),
                ("relname", "pk_storage"),
                ("relnamespace", "2200"),
                ("reltype", "50101"),
                ("relowner", "10"),
                ("relam", "2"),
                ("relfilenode", "50100"),
                ("relhasindex", "t"),
                ("relpersistence", "p"),
                ("relkind", "r"),
                ("relnatts", "2"),
            ],
        );
        upsert_named_catalog_row(
            "pg_type",
            type_oid as u64,
            &[
                ("oid", "50101"),
                ("typname", "pk_storage"),
                ("typnamespace", "2200"),
                ("typowner", "10"),
                ("typlen", "-1"),
                ("typbyval", "f"),
                ("typtype", "c"),
                ("typcategory", "C"),
                ("typisdefined", "t"),
                ("typdelim", ","),
                ("typrelid", "50100"),
                ("typalign", "d"),
                ("typstorage", "x"),
            ],
        );
        for (attnum, name, type_oid, attlen, attbyval, attnotnull) in [
            (1, "id", "23", "4", "t", "t"),
            (2, "value", "23", "4", "t", "f"),
        ] {
            upsert_named_catalog_row(
                "pg_attribute",
                0,
                &[
                    ("attrelid", "50100"),
                    ("attname", name),
                    ("atttypid", type_oid),
                    ("attlen", attlen),
                    ("attnum", &attnum.to_string()),
                    ("atttypmod", "-1"),
                    ("attbyval", attbyval),
                    ("attalign", "i"),
                    ("attstorage", "p"),
                    ("attnotnull", attnotnull),
                    ("attisdropped", "f"),
                ],
            );
        }
        upsert_named_catalog_row(
            "pg_class",
            index_oid.0 as u64,
            &[
                ("oid", "50102"),
                ("relname", "pk_storage_pkey"),
                ("relnamespace", "2200"),
                ("reltype", "0"),
                ("relowner", "10"),
                ("relam", "403"),
                ("relfilenode", "50102"),
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
                ("attrelid", "50102"),
                ("attname", "id"),
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
                ("indexrelid", "50102"),
                ("indrelid", "50100"),
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
            constraint_oid as u64,
            &[
                ("oid", "50103"),
                ("conname", "pk_storage_pkey"),
                ("connamespace", "2200"),
                ("contype", "p"),
                ("conrelid", "50100"),
                ("conindid", "50102"),
                ("conkey", "1"),
            ],
        );
        fastpg_rust_xact_commit_if_implicit();
        clear_primary_key_index_info_cache();
        (relid, index_oid)
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

    unsafe fn insert_byref(relid: u32, payload: &[u8], row_id: &mut u64) -> bool {
        let values = [payload.as_ptr() as usize];
        let nulls = [0u8];
        let byval = [0u8];
        let value_lens = [payload.len()];
        unsafe {
            fastpg_rust_relation_insert(
                relid,
                values.as_ptr(),
                nulls.as_ptr(),
                byval.as_ptr(),
                value_lens.as_ptr(),
                1,
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

    fn last_storage_error_for_test() -> Option<(String, String)> {
        let mut sqlstate = [0 as c_char; 6];
        let mut message = [0 as c_char; 256];
        let found = unsafe {
            fastpg_rust_storage_last_error(
                sqlstate.as_mut_ptr(),
                sqlstate.len(),
                message.as_mut_ptr(),
                message.len(),
            )
        };
        found.then(|| {
            let sqlstate = unsafe { CStr::from_ptr(sqlstate.as_ptr()) }
                .to_string_lossy()
                .into_owned();
            let message = unsafe { CStr::from_ptr(message.as_ptr()) }
                .to_string_lossy()
                .into_owned();
            (sqlstate, message)
        })
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
            .chain(std::iter::once((
                INFORMATION_SCHEMA_NAMESPACE_OID.0 as u64,
                INFORMATION_SCHEMA_NAMESPACE_OID.0,
                "information_schema".to_owned(),
                10,
                1,
            )))
            .collect::<Vec<_>>();
        assert_eq!(rows, expected);
    }

    #[test]
    fn catalog_scan_filters_simple_oid_keys() {
        const PG_NAMESPACE_RELATION_ID: u32 = 2615;
        const PG_NAMESPACE_ATTRIBUTE_COUNT: usize = 4;

        let _guard = test_guard();
        let attnums = [1i16];
        let values = [PG_CATALOG_NAMESPACE_OID.0 as usize];
        let scan = unsafe {
            fastpg_rust_scan_begin_filtered(
                PG_NAMESPACE_RELATION_ID,
                attnums.as_ptr(),
                values.as_ptr(),
                attnums.len(),
            )
        };
        assert_ne!(scan, 0);

        let mut tuple_values = [0usize; PG_NAMESPACE_ATTRIBUTE_COUNT];
        let mut nulls = [0u8; PG_NAMESPACE_ATTRIBUTE_COUNT];
        let mut row_id = 0;
        unsafe {
            assert!(fastpg_rust_scan_next(
                scan,
                1,
                tuple_values.as_mut_ptr(),
                nulls.as_mut_ptr(),
                tuple_values.len(),
                &mut row_id,
            ));
        }
        assert_eq!(tuple_values[0] as u32, PG_CATALOG_NAMESPACE_OID.0);
        let name = unsafe { CStr::from_ptr(tuple_values[1] as *const c_char) }
            .to_string_lossy()
            .into_owned();
        assert_eq!(name, "pg_catalog");
        unsafe {
            assert!(!fastpg_rust_scan_next(
                scan,
                1,
                tuple_values.as_mut_ptr(),
                nulls.as_mut_ptr(),
                tuple_values.len(),
                &mut row_id,
            ));
        }
        fastpg_rust_scan_end(scan);
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
    fn relation_name_cache_skips_uncommitted_catalog_overlays() {
        let _guard = test_guard();
        let relid = next_relid();
        let relation_name = format!("cache_visible_{relid}");
        let relid_text = relid.to_string();
        let namespace_text = PUBLIC_NAMESPACE_OID.0.to_string();

        assert_eq!(
            relation_oid_by_name_via_ffi(&relation_name, PUBLIC_NAMESPACE_OID.0),
            None
        );

        fastpg_rust_xact_begin();
        upsert_named_catalog_row(
            "pg_class",
            relid as u64,
            &[
                ("oid", &relid_text),
                ("relname", &relation_name),
                ("relnamespace", &namespace_text),
                ("reltype", "0"),
                ("relowner", "10"),
                ("relam", "2"),
                ("relfilenode", &relid_text),
                ("relhasindex", "f"),
                ("relpersistence", "p"),
                ("relkind", "r"),
                ("relnatts", "0"),
            ],
        );
        assert_eq!(
            relation_oid_by_name_via_ffi(&relation_name, PUBLIC_NAMESPACE_OID.0),
            Some(relid)
        );

        fastpg_rust_xact_abort();
        assert_eq!(
            relation_oid_by_name_via_ffi(&relation_name, PUBLIC_NAMESPACE_OID.0),
            None
        );
    }

    #[test]
    fn relation_exists_cache_skips_uncommitted_catalog_overlays() {
        let _guard = test_guard();
        let relid = next_relid();
        let relation_name = format!("exists_cache_visible_{relid}");
        let relid_text = relid.to_string();
        let namespace_text = PUBLIC_NAMESPACE_OID.0.to_string();

        assert!(!fastpg_rust_catalog_relation_exists_by_oid(relid));

        fastpg_rust_xact_begin();
        upsert_named_catalog_row(
            "pg_class",
            relid as u64,
            &[
                ("oid", &relid_text),
                ("relname", &relation_name),
                ("relnamespace", &namespace_text),
                ("reltype", "0"),
                ("relowner", "10"),
                ("relam", "2"),
                ("relfilenode", &relid_text),
                ("relhasindex", "f"),
                ("relpersistence", "p"),
                ("relkind", "r"),
                ("relnatts", "0"),
            ],
        );
        assert!(fastpg_rust_catalog_relation_exists_by_oid(relid));

        fastpg_rust_xact_abort();
        assert!(!fastpg_rust_catalog_relation_exists_by_oid(relid));
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
    fn subxact_abort_restores_parent_relation_clear() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut parent_row_id = 0;

        fastpg_rust_xact_begin();
        unsafe {
            assert!(insert_byval(relid, &[1], &[0], &mut parent_row_id));
        }
        fastpg_rust_subxact_begin();
        fastpg_rust_relation_clear(relid);
        assert_eq!(fastpg_rust_relation_row_count(relid), 0);
        assert!(!fastpg_rust_relation_contains_row(relid, parent_row_id));
        fastpg_rust_subxact_abort();
        fastpg_rust_xact_commit();

        assert_eq!(fastpg_rust_relation_row_count(relid), 1);
        assert!(fastpg_rust_relation_contains_row(relid, parent_row_id));
    }

    #[test]
    fn subxact_commit_preserves_relation_clear() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut parent_row_id = 0;

        fastpg_rust_xact_begin();
        unsafe {
            assert!(insert_byval(relid, &[1], &[0], &mut parent_row_id));
        }
        fastpg_rust_subxact_begin();
        fastpg_rust_relation_clear(relid);
        fastpg_rust_subxact_commit();
        assert_eq!(fastpg_rust_relation_row_count(relid), 0);
        assert!(!fastpg_rust_relation_contains_row(relid, parent_row_id));
        fastpg_rust_xact_commit();

        assert_eq!(fastpg_rust_relation_row_count(relid), 0);
        assert!(!fastpg_rust_relation_contains_row(relid, parent_row_id));
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
    fn byref_bytes_are_accounted_and_promoted_on_commit() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut row_id = 0;
        let payload = b"owned by transaction arena";
        let committed_before = fastpg_rust_storage_committed_bytes();

        assert_eq!(fastpg_rust_storage_transaction_bytes(), 0);
        fastpg_rust_xact_begin();
        unsafe {
            assert!(insert_byref(relid, payload, &mut row_id));
        }
        assert!(fastpg_rust_storage_transaction_bytes() >= payload.len());
        fastpg_rust_xact_commit();

        assert_eq!(fastpg_rust_storage_transaction_bytes(), 0);
        assert!(fastpg_rust_storage_committed_bytes() >= committed_before + payload.len());

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
            assert_eq!(
                slice::from_raw_parts(values_out[0] as *const u8, payload.len()),
                payload
            );
        }
        assert_eq!(nulls_out, [0]);
    }

    #[test]
    fn savepoint_abort_drops_nested_arena_bytes() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut parent_row_id = 0;
        let mut nested_row_id = 0;
        let parent_payload = b"parent arena bytes";
        let nested_payload = b"nested savepoint arena bytes";

        fastpg_rust_xact_begin();
        unsafe {
            assert!(insert_byref(relid, parent_payload, &mut parent_row_id));
        }
        let parent_bytes = fastpg_rust_storage_transaction_bytes();
        assert!(parent_bytes >= parent_payload.len());

        fastpg_rust_subxact_begin();
        unsafe {
            assert!(insert_byref(relid, nested_payload, &mut nested_row_id));
        }
        assert!(fastpg_rust_storage_transaction_bytes() > parent_bytes);
        fastpg_rust_subxact_abort();

        assert_eq!(fastpg_rust_storage_transaction_bytes(), parent_bytes);
        assert!(fastpg_rust_relation_contains_row(relid, parent_row_id));
        assert!(!fastpg_rust_relation_contains_row(relid, nested_row_id));
        fastpg_rust_xact_commit();
        assert_eq!(fastpg_rust_storage_transaction_bytes(), 0);
    }

    #[test]
    fn scan_materialization_bytes_are_released_on_scan_end() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut row_id = 0;
        let payload = b"scan-owned copy";

        fastpg_rust_xact_begin();
        unsafe {
            assert!(insert_byref(relid, payload, &mut row_id));
        }
        fastpg_rust_xact_commit();
        assert_eq!(fastpg_rust_storage_scan_bytes(), 0);

        let scan = fastpg_rust_scan_begin(relid);
        assert!(fastpg_rust_storage_scan_bytes() >= payload.len());
        fastpg_rust_scan_end(scan);
        assert_eq!(fastpg_rust_storage_scan_bytes(), 0);
    }

    #[test]
    fn row_memory_limit_rejects_insert_with_sqlstate() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut row_id = 0;
        let rejections_before = fastpg_rust_storage_memory_limit_rejections();

        fastpg_rust_storage_set_limits(0, 0, 0, 0, 1, 0);
        unsafe {
            assert!(!insert_byref(
                relid,
                b"too large for row limit",
                &mut row_id
            ));
        }

        assert_eq!(row_id, 0);
        assert_eq!(fastpg_rust_storage_transaction_bytes(), 0);
        assert_eq!(
            fastpg_rust_storage_memory_limit_rejections(),
            rejections_before + 1
        );
        assert_eq!(
            last_storage_error_for_test(),
            Some((
                "54000".to_owned(),
                "fastpg memory limit exceeded for row storage".to_owned()
            ))
        );
    }

    #[test]
    fn transaction_memory_limit_rejects_insert_with_sqlstate() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut row_id = 0;

        fastpg_rust_storage_set_limits(0, 0, 0, 1, 0, 0);
        fastpg_rust_xact_begin();
        unsafe {
            assert!(!insert_byref(
                relid,
                b"too large for transaction limit",
                &mut row_id
            ));
        }

        assert_eq!(row_id, 0);
        assert_eq!(fastpg_rust_storage_transaction_bytes(), 0);
        assert_eq!(
            last_storage_error_for_test(),
            Some((
                "54000".to_owned(),
                "fastpg memory limit exceeded for transaction storage".to_owned()
            ))
        );
        fastpg_rust_xact_abort();
    }

    #[test]
    fn committed_memory_limit_rejects_projected_insert_with_sqlstate() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut row_id = 0;

        fastpg_rust_storage_set_limits(1, 0, 0, 0, 0, 0);
        unsafe {
            assert!(!insert_byref(
                relid,
                b"too large for committed limit",
                &mut row_id
            ));
        }

        assert_eq!(row_id, 0);
        assert_eq!(
            last_storage_error_for_test(),
            Some((
                "54000".to_owned(),
                "fastpg memory limit exceeded for committed storage".to_owned()
            ))
        );
    }

    #[test]
    fn scan_memory_limit_rejects_materialization_with_sqlstate() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut row_id = 0;

        unsafe {
            assert!(insert_byref(
                relid,
                b"too large for scan limit",
                &mut row_id
            ));
        }
        fastpg_rust_storage_set_limits(0, 0, 0, 0, 0, 1);

        assert_eq!(fastpg_rust_scan_begin(relid), 0);
        assert_eq!(fastpg_rust_storage_scan_bytes(), 0);
        assert_eq!(
            last_storage_error_for_test(),
            Some((
                "54000".to_owned(),
                "fastpg memory limit exceeded for scan storage".to_owned()
            ))
        );
    }

    #[test]
    fn primary_key_index_bytes_are_accounted() {
        let _guard = test_guard();
        let (relid, _index_oid) = install_primary_key_test_catalog();
        let before_index_bytes = fastpg_rust_storage_index_bytes();
        let mut row_id = 0;

        fastpg_rust_xact_begin();
        unsafe {
            assert!(insert_byval(relid, &[7], &[0], &mut row_id));
        }
        fastpg_rust_xact_commit();

        assert!(fastpg_rust_storage_index_bytes() > before_index_bytes);
    }

    #[test]
    fn primary_key_index_backfills_rows_inserted_before_index_catalog() {
        let _guard = test_guard();
        let relid = next_relid();
        let type_oid = relid + 100_000;
        let index_oid = Oid(relid + 200_000);
        let constraint_oid = relid + 300_000;
        let mut first_row_id = 0;
        let mut second_row_id = 0;

        upsert_named_catalog_row(
            "pg_class",
            relid as u64,
            &[
                ("oid", &relid.to_string()),
                ("relname", "late_pk_storage"),
                ("relnamespace", "2200"),
                ("reltype", &type_oid.to_string()),
                ("relowner", "10"),
                ("relam", "2"),
                ("relfilenode", &relid.to_string()),
                ("relhasindex", "f"),
                ("relpersistence", "p"),
                ("relkind", "r"),
                ("relnatts", "2"),
            ],
        );
        upsert_named_catalog_row(
            "pg_type",
            type_oid as u64,
            &[
                ("oid", &type_oid.to_string()),
                ("typname", "late_pk_storage"),
                ("typnamespace", "2200"),
                ("typowner", "10"),
                ("typlen", "-1"),
                ("typbyval", "f"),
                ("typtype", "c"),
                ("typcategory", "C"),
                ("typisdefined", "t"),
                ("typdelim", ","),
                ("typrelid", &relid.to_string()),
                ("typalign", "d"),
                ("typstorage", "x"),
            ],
        );
        for (attnum, name) in [(1, "id"), (2, "value")] {
            upsert_named_catalog_row(
                "pg_attribute",
                0,
                &[
                    ("attrelid", &relid.to_string()),
                    ("attname", name),
                    ("atttypid", "23"),
                    ("attlen", "4"),
                    ("attnum", &attnum.to_string()),
                    ("atttypmod", "-1"),
                    ("attbyval", "t"),
                    ("attalign", "i"),
                    ("attstorage", "p"),
                    ("attnotnull", if attnum == 1 { "t" } else { "f" }),
                    ("attisdropped", "f"),
                ],
            );
        }
        fastpg_rust_xact_commit_if_implicit();

        fastpg_rust_xact_begin();
        unsafe {
            assert!(insert_byval(relid, &[1, 10], &[0, 0], &mut first_row_id));
            assert!(insert_byval(relid, &[2, 20], &[0, 0], &mut second_row_id));
        }
        fastpg_rust_xact_commit();
        let before_index_bytes = fastpg_rust_storage_index_bytes();

        upsert_named_catalog_row(
            "pg_class",
            relid as u64,
            &[
                ("oid", &relid.to_string()),
                ("relname", "late_pk_storage"),
                ("relnamespace", "2200"),
                ("reltype", &type_oid.to_string()),
                ("relowner", "10"),
                ("relam", "2"),
                ("relfilenode", &relid.to_string()),
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
                ("oid", &index_oid.0.to_string()),
                ("relname", "late_pk_storage_pkey"),
                ("relnamespace", "2200"),
                ("reltype", "0"),
                ("relowner", "10"),
                ("relam", "403"),
                ("relfilenode", &index_oid.0.to_string()),
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
                ("attrelid", &index_oid.0.to_string()),
                ("attname", "id"),
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
        upsert_named_catalog_row_via_ffi(
            "pg_index",
            index_oid.0 as u64,
            &[
                ("indexrelid", &index_oid.0.to_string()),
                ("indrelid", &relid.to_string()),
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
            constraint_oid as u64,
            &[
                ("oid", &constraint_oid.to_string()),
                ("conname", "late_pk_storage_pkey"),
                ("connamespace", "2200"),
                ("contype", "p"),
                ("conrelid", &relid.to_string()),
                ("conindid", &index_oid.0.to_string()),
                ("conkey", "1"),
            ],
        );
        fastpg_rust_xact_commit_if_implicit();
        assert!(fastpg_rust_storage_index_bytes() > before_index_bytes);

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
        assert_ne!(first_row_id, second_row_id);
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
        let (relid, index_oid) = install_primary_key_test_catalog();
        let relation = relation_by_name("pk_storage").unwrap();
        assert_eq!(relation.oid.0, relid);
        assert_eq!(primary_key_index_oid(&relation), Some(index_oid));
        let mut first_row_id = 0;
        let mut second_row_id = 0;
        let mut index_relation = FastPgRustCatalogRelation {
            oid: 0,
            type_oid: 0,
            namespace_oid: 0,
            owner_oid: 0,
            name: [0; NAMEDATALEN],
            column_count: 0,
            relkind: 0,
            has_primary_key: 0,
            has_indexes: 0,
            row_id: 0,
        };
        let mut index_info = FastPgRustPrimaryKeyIndexInfo {
            row_id: 0,
            index_oid: 0,
            heap_oid: 0,
            key_count: 0,
            is_unique: 0,
            is_primary: 0,
            nulls_not_distinct: 0,
            is_immediate: 0,
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
        assert_eq!(index_info.heap_oid, relid);
        assert_eq!(index_info.key_count, 1);
        assert!(index_relation.column_count >= index_info.key_count);
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
