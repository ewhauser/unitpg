#![deny(unsafe_op_in_unsafe_fn)]

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::c_char;
use std::slice;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use fastpg_catalog::{
    BPCHAR_OID, CatalogError, INT2_OID, INT4_OID, INT8_OID, IndexRecord, OID_OID, TEXT_OID,
    TIMESTAMP_OID, VARCHAR_OID, current_generation, has_uncommitted_catalog_changes, lookup_type,
    primary_key_index_oid_for_relation_oid, relation_by_name, relation_column_by_attnum,
    relation_oid_for_index_oid, unique_index_records_for_relation_oid,
};
use fastpg_types::Oid;

const PAGE_SIZE: usize = 8192;
const MAX_CTID_OFFSET: usize = 2047;
const TUPLE_MAGIC: &[u8; 4] = b"FP2T";
const TUPLE_HEADER_LEN: usize = 16;
const ATTR_ENTRY_LEN: usize = 24;
const SQLSTATE_PROGRAM_LIMIT_EXCEEDED: &str = "54000";

static STORAGE2_ARENA_REWINDS: AtomicU64 = AtomicU64::new(0);
static STORAGE2_ARENA_DROPS: AtomicU64 = AtomicU64::new(0);
static STORAGE2_METADATA_CACHE: OnceLock<Mutex<Storage2MetadataCache>> = OnceLock::new();

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Tid {
    pub block: u32,
    pub offset: u16,
}

impl Tid {
    fn pack(self) -> u64 {
        ((self.block as u64) << 16) | u64::from(self.offset)
    }

    fn unpack(value: u64) -> Option<Self> {
        let offset = (value & 0xffff) as u16;
        if offset == 0 {
            return None;
        }
        Some(Self {
            block: (value >> 16) as u32,
            offset,
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FastPgStorage2Metrics {
    pub committed_page_bytes: usize,
    pub transaction_page_bytes: usize,
    pub scan_scratch_bytes: usize,
    pub live_tuple_bytes: usize,
    pub dead_tuple_bytes: usize,
    pub index_bytes: usize,
    pub page_count: usize,
    pub arena_rewinds: u64,
    pub arena_drops: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LinePointerState {
    Pending,
    Live,
    Dead,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LinePointer {
    offset: u32,
    len: u32,
    state: LinePointerState,
}

#[derive(Debug)]
struct Page {
    block: u32,
    epoch: u64,
    generation: u64,
    bytes: Box<[u8]>,
    used: usize,
    line_pointers: Vec<LinePointer>,
    pending_tuple_bytes: usize,
    live_tuple_bytes: usize,
    dead_tuple_bytes: usize,
}

impl Page {
    fn new(block: u32, epoch: u64, generation: u64, min_capacity: usize) -> Self {
        let capacity = PAGE_SIZE.max(min_capacity.next_power_of_two());
        Self {
            block,
            epoch,
            generation,
            bytes: vec![0; capacity].into_boxed_slice(),
            used: 0,
            line_pointers: Vec::new(),
            pending_tuple_bytes: 0,
            live_tuple_bytes: 0,
            dead_tuple_bytes: 0,
        }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.used)
    }

    fn can_fit(&self, tuple_len: usize) -> bool {
        tuple_len <= self.remaining() && self.line_pointers.len() < MAX_CTID_OFFSET
    }

    fn append_tuple_with_state(&mut self, tuple: &[u8], state: LinePointerState) -> Option<Tid> {
        if tuple.len() > self.remaining() || self.line_pointers.len() >= MAX_CTID_OFFSET {
            return None;
        }
        let offset = self.used;
        let end = offset.checked_add(tuple.len())?;
        self.bytes[offset..end].copy_from_slice(tuple);
        self.used = end;
        self.line_pointers.push(LinePointer {
            offset: offset.try_into().ok()?,
            len: tuple.len().try_into().ok()?,
            state,
        });
        match state {
            LinePointerState::Pending => {
                self.pending_tuple_bytes = self.pending_tuple_bytes.saturating_add(tuple.len())
            }
            LinePointerState::Live => {
                self.live_tuple_bytes = self.live_tuple_bytes.saturating_add(tuple.len())
            }
            LinePointerState::Dead => {
                self.dead_tuple_bytes = self.dead_tuple_bytes.saturating_add(tuple.len())
            }
        }
        Some(Tid {
            block: self.block,
            offset: self.line_pointers.len().try_into().ok()?,
        })
    }

    fn tuple_slice(&self, offset: u16, include_pending: bool) -> Option<&[u8]> {
        let index = usize::from(offset.checked_sub(1)?);
        let line = self.line_pointers.get(index)?;
        if line.state == LinePointerState::Dead
            || (line.state == LinePointerState::Pending && !include_pending)
        {
            return None;
        }
        let start = line.offset as usize;
        let end = start.checked_add(line.len as usize)?;
        self.bytes.get(start..end)
    }

    fn mark_dead(&mut self, offset: u16) -> bool {
        let Some(index) = offset.checked_sub(1).map(usize::from) else {
            return false;
        };
        let Some(line) = self.line_pointers.get_mut(index) else {
            return false;
        };
        let previous = line.state;
        if previous == LinePointerState::Dead {
            return false;
        }
        line.state = LinePointerState::Dead;
        let len = line.len as usize;
        match previous {
            LinePointerState::Pending => {
                self.pending_tuple_bytes = self.pending_tuple_bytes.saturating_sub(len);
            }
            LinePointerState::Live => {
                self.live_tuple_bytes = self.live_tuple_bytes.saturating_sub(len);
            }
            LinePointerState::Dead => {}
        }
        self.dead_tuple_bytes = self.dead_tuple_bytes.saturating_add(len);
        true
    }

    fn mark_live(&mut self, offset: u16) -> bool {
        let Some(index) = offset.checked_sub(1).map(usize::from) else {
            return false;
        };
        let Some(line) = self.line_pointers.get_mut(index) else {
            return false;
        };
        if line.state != LinePointerState::Pending {
            return false;
        }
        line.state = LinePointerState::Live;
        let len = line.len as usize;
        self.pending_tuple_bytes = self.pending_tuple_bytes.saturating_sub(len);
        self.live_tuple_bytes = self.live_tuple_bytes.saturating_add(len);
        true
    }

    fn checkpoint(&self) -> PageCheckpoint {
        PageCheckpoint {
            used: self.used,
            line_count: self.line_pointers.len(),
            line_states: self.line_pointers.iter().map(|line| line.state).collect(),
            pending_tuple_bytes: self.pending_tuple_bytes,
            live_tuple_bytes: self.live_tuple_bytes,
            dead_tuple_bytes: self.dead_tuple_bytes,
        }
    }

    fn restore_to(&mut self, checkpoint: &PageCheckpoint) {
        self.used = checkpoint.used.min(self.bytes.len());
        self.line_pointers.truncate(checkpoint.line_count);
        for (line, state) in self
            .line_pointers
            .iter_mut()
            .zip(checkpoint.line_states.iter().copied())
        {
            line.state = state;
        }
        self.pending_tuple_bytes = checkpoint.pending_tuple_bytes;
        self.live_tuple_bytes = checkpoint.live_tuple_bytes;
        self.dead_tuple_bytes = checkpoint.dead_tuple_bytes;
    }

    fn live_tids(&self) -> impl Iterator<Item = Tid> + '_ {
        self.line_pointers
            .iter()
            .enumerate()
            .filter(|(_, line)| line.state == LinePointerState::Live)
            .filter_map(|(index, _)| {
                let offset = u16::try_from(index + 1).ok()?;
                Some(Tid {
                    block: self.block,
                    offset,
                })
            })
    }

    fn accounted_bytes(&self) -> usize {
        self.bytes.len()
            + self
                .line_pointers
                .capacity()
                .saturating_mul(std::mem::size_of::<LinePointer>())
            + std::mem::size_of_val(&self.epoch)
            + std::mem::size_of_val(&self.generation)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct PageCheckpoint {
    used: usize,
    line_count: usize,
    line_states: Vec<LinePointerState>,
    pending_tuple_bytes: usize,
    live_tuple_bytes: usize,
    dead_tuple_bytes: usize,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum IndexKeyPart {
    Null,
    ByValue(usize),
    Bytes(Vec<u8>),
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct IndexKey {
    parts: Vec<IndexKeyPart>,
}

impl IndexKey {
    fn accounted_bytes(&self) -> usize {
        self.parts
            .iter()
            .map(|part| match part {
                IndexKeyPart::Null => 1,
                IndexKeyPart::ByValue(_) => std::mem::size_of::<usize>(),
                IndexKeyPart::Bytes(bytes) => bytes.len(),
            })
            .sum()
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

#[derive(Debug)]
struct Storage2MetadataCache {
    generation: u64,
    unique_specs_by_relation: HashMap<u32, Vec<UniqueIndexSpec>>,
    primary_specs_by_index: HashMap<u32, Option<UniqueIndexSpec>>,
}

impl Default for Storage2MetadataCache {
    fn default() -> Self {
        Self {
            generation: current_generation(),
            unique_specs_by_relation: HashMap::new(),
            primary_specs_by_index: HashMap::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DecodedDatum<'a> {
    Null,
    ByValue(usize),
    ByRef(&'a [u8]),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DecodedTuple<'a> {
    tid: Tid,
    values: Vec<DecodedDatum<'a>>,
}

#[derive(Debug, Default)]
struct RelationStorage {
    pages: Vec<Option<Page>>,
    primary_key_index: BTreeMap<IndexKey, Tid>,
    next_block: u32,
    append_hint: Option<u32>,
    live_tuple_count: usize,
    pending_tuple_count: usize,
    dead_tuple_count: usize,
    live_tuple_bytes: usize,
    pending_tuple_bytes: usize,
    dead_tuple_bytes: usize,
}

impl RelationStorage {
    fn checkpoint(&self) -> RelationCheckpoint {
        RelationCheckpoint {
            pages_len: self.pages.len(),
            next_block: self.next_block,
            append_hint: self.append_hint,
            live_tuple_count: self.live_tuple_count,
            pending_tuple_count: self.pending_tuple_count,
            dead_tuple_count: self.dead_tuple_count,
            live_tuple_bytes: self.live_tuple_bytes,
            pending_tuple_bytes: self.pending_tuple_bytes,
            dead_tuple_bytes: self.dead_tuple_bytes,
        }
    }

    fn restore_metadata(&mut self, checkpoint: RelationCheckpoint) {
        self.pages.truncate(checkpoint.pages_len);
        self.next_block = checkpoint.next_block;
        self.append_hint = checkpoint.append_hint;
        self.live_tuple_count = checkpoint.live_tuple_count;
        self.pending_tuple_count = checkpoint.pending_tuple_count;
        self.dead_tuple_count = checkpoint.dead_tuple_count;
        self.live_tuple_bytes = checkpoint.live_tuple_bytes;
        self.pending_tuple_bytes = checkpoint.pending_tuple_bytes;
        self.dead_tuple_bytes = checkpoint.dead_tuple_bytes;
    }

    fn reserve_block(&mut self) -> Option<u32> {
        let block = self.next_block;
        self.next_block = self.next_block.checked_add(1)?;
        Some(block)
    }

    fn insert_page(&mut self, page: Page) {
        let block = page.block as usize;
        if self.pages.len() <= block {
            self.pages.resize_with(block + 1, || None);
        }
        if page.can_fit(1) {
            self.append_hint = Some(page.block);
        }
        self.pages[block] = Some(page);
    }

    fn remove_page(&mut self, block: u32) {
        if let Some(slot) = self.pages.get_mut(block as usize) {
            *slot = None;
        }
        if self.append_hint == Some(block) {
            self.append_hint = None;
        }
    }

    fn page(&self, block: u32) -> Option<&Page> {
        self.pages.get(block as usize)?.as_ref()
    }

    fn page_mut(&mut self, block: u32) -> Option<&mut Page> {
        self.pages.get_mut(block as usize)?.as_mut()
    }

    fn tuple_slice(&self, tid: Tid, include_pending: bool) -> Option<&[u8]> {
        self.page(tid.block)?
            .tuple_slice(tid.offset, include_pending)
    }

    fn mark_dead(&mut self, tid: Tid) -> bool {
        let Some(page) = self.page_mut(tid.block) else {
            return false;
        };
        let Some(index) = tid.offset.checked_sub(1).map(usize::from) else {
            return false;
        };
        let Some(line) = page.line_pointers.get(index).copied() else {
            return false;
        };
        if !page.mark_dead(tid.offset) {
            return false;
        }
        let len = line.len as usize;
        match line.state {
            LinePointerState::Pending => {
                self.pending_tuple_count = self.pending_tuple_count.saturating_sub(1);
                self.pending_tuple_bytes = self.pending_tuple_bytes.saturating_sub(len);
            }
            LinePointerState::Live => {
                self.live_tuple_count = self.live_tuple_count.saturating_sub(1);
                self.live_tuple_bytes = self.live_tuple_bytes.saturating_sub(len);
            }
            LinePointerState::Dead => return false,
        }
        self.dead_tuple_count = self.dead_tuple_count.saturating_add(1);
        self.dead_tuple_bytes = self.dead_tuple_bytes.saturating_add(len);
        true
    }

    fn mark_live(&mut self, tid: Tid) -> bool {
        let Some(page) = self.page_mut(tid.block) else {
            return false;
        };
        let Some(index) = tid.offset.checked_sub(1).map(usize::from) else {
            return false;
        };
        let Some(line) = page.line_pointers.get(index).copied() else {
            return false;
        };
        if line.state != LinePointerState::Pending || !page.mark_live(tid.offset) {
            return false;
        }
        let len = line.len as usize;
        self.pending_tuple_count = self.pending_tuple_count.saturating_sub(1);
        self.pending_tuple_bytes = self.pending_tuple_bytes.saturating_sub(len);
        self.live_tuple_count = self.live_tuple_count.saturating_add(1);
        self.live_tuple_bytes = self.live_tuple_bytes.saturating_add(len);
        true
    }

    fn append_target_block(
        &mut self,
        tuple_len: usize,
        epoch: u64,
        generation: u64,
    ) -> Option<u32> {
        if let Some(block) = self.append_hint
            && self.page(block).is_some_and(|page| page.can_fit(tuple_len))
        {
            return Some(block);
        }

        if let Some((block, _)) = self
            .pages
            .iter()
            .enumerate()
            .rev()
            .filter_map(|(block, page)| Some((u32::try_from(block).ok()?, page.as_ref()?)))
            .find(|(_, page)| page.can_fit(tuple_len))
        {
            self.append_hint = Some(block);
            return Some(block);
        }

        let block = self.reserve_block()?;
        let page = Page::new(block, epoch, generation, tuple_len);
        self.insert_page(page);
        Some(block)
    }

    fn append_pending_tuple(&mut self, block: u32, tuple: &[u8]) -> Option<Tid> {
        let (tid, can_fit_more) = {
            let page = self.page_mut(block)?;
            let tid = page.append_tuple_with_state(tuple, LinePointerState::Pending)?;
            (tid, page.can_fit(1))
        };
        self.pending_tuple_count = self.pending_tuple_count.saturating_add(1);
        self.pending_tuple_bytes = self.pending_tuple_bytes.saturating_add(tuple.len());
        self.append_hint = if can_fit_more { Some(block) } else { None };
        Some(tid)
    }

    fn live_tids(&self) -> impl Iterator<Item = Tid> + '_ {
        self.pages
            .iter()
            .filter_map(Option::as_ref)
            .flat_map(Page::live_tids)
    }

    fn accounted_bytes(&self) -> usize {
        self.pages
            .iter()
            .filter_map(Option::as_ref)
            .map(Page::accounted_bytes)
            .sum()
    }

    fn live_tuple_bytes(&self) -> usize {
        self.live_tuple_bytes + self.pending_tuple_bytes
    }

    fn dead_tuple_bytes(&self) -> usize {
        self.dead_tuple_bytes
    }

    fn page_count(&self) -> usize {
        self.pages.iter().filter(|page| page.is_some()).count()
    }

    fn index_bytes(&self) -> usize {
        self.primary_key_index
            .keys()
            .map(|key| key.accounted_bytes() + std::mem::size_of::<Tid>())
            .sum()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RelationCheckpoint {
    pages_len: usize,
    next_block: u32,
    append_hint: Option<u32>,
    live_tuple_count: usize,
    pending_tuple_count: usize,
    dead_tuple_count: usize,
    live_tuple_bytes: usize,
    pending_tuple_bytes: usize,
    dead_tuple_bytes: usize,
}

#[derive(Debug, Default)]
struct TransactionOverlay {
    relation_checkpoints: HashMap<u32, RelationCheckpoint>,
    page_checkpoints: HashMap<u32, BTreeMap<u32, PageCheckpoint>>,
    new_pages: HashMap<u32, BTreeSet<u32>>,
    inserted_tids: HashMap<u32, BTreeSet<Tid>>,
    invalidated_tids: HashMap<u32, BTreeSet<Tid>>,
    primary_key_inserts: HashMap<u32, BTreeMap<IndexKey, Tid>>,
    primary_key_deletes: HashMap<u32, BTreeSet<IndexKey>>,
}

impl TransactionOverlay {
    fn checkpoint_relation(&mut self, relid: u32, relation: &RelationStorage) {
        self.relation_checkpoints
            .entry(relid)
            .or_insert_with(|| relation.checkpoint());
    }

    fn checkpoint_page(&mut self, relid: u32, page: &Page) {
        if self
            .new_pages
            .get(&relid)
            .is_some_and(|blocks| blocks.contains(&page.block))
        {
            return;
        }
        self.page_checkpoints
            .entry(relid)
            .or_default()
            .entry(page.block)
            .or_insert_with(|| page.checkpoint());
    }

    fn record_new_page(&mut self, relid: u32, block: u32) {
        self.new_pages.entry(relid).or_default().insert(block);
    }

    fn insert_tid(&mut self, relid: u32, tid: Tid) {
        self.inserted_tids.entry(relid).or_default().insert(tid);
    }

    fn invalidate(&mut self, relid: u32, tid: Tid) {
        self.invalidated_tids.entry(relid).or_default().insert(tid);
    }

    fn delete_primary_key(&mut self, relid: u32, key: IndexKey) {
        self.primary_key_deletes
            .entry(relid)
            .or_default()
            .insert(key);
    }

    fn insert_primary_key(&mut self, relid: u32, key: IndexKey, tid: Tid) {
        self.primary_key_inserts
            .entry(relid)
            .or_default()
            .insert(key, tid);
    }

    fn append_from(&mut self, other: &mut Self) {
        for (relid, checkpoint) in other.relation_checkpoints.drain() {
            self.relation_checkpoints.entry(relid).or_insert(checkpoint);
        }
        for (relid, checkpoints) in other.page_checkpoints.drain() {
            let target = self.page_checkpoints.entry(relid).or_default();
            for (block, checkpoint) in checkpoints {
                if !self
                    .new_pages
                    .get(&relid)
                    .is_some_and(|blocks| blocks.contains(&block))
                {
                    target.entry(block).or_insert(checkpoint);
                }
            }
        }
        for (relid, blocks) in other.new_pages.drain() {
            self.new_pages.entry(relid).or_default().extend(blocks);
        }
        for (relid, tids) in other.inserted_tids.drain() {
            self.inserted_tids.entry(relid).or_default().extend(tids);
        }
        for (relid, tids) in other.invalidated_tids.drain() {
            self.invalidated_tids.entry(relid).or_default().extend(tids);
        }
        for (relid, keys) in other.primary_key_deletes.drain() {
            self.primary_key_deletes
                .entry(relid)
                .or_default()
                .extend(keys);
        }
        for (relid, entries) in other.primary_key_inserts.drain() {
            self.primary_key_inserts
                .entry(relid)
                .or_default()
                .extend(entries);
        }
    }

    fn accounted_bytes(&self) -> usize {
        self.new_pages
            .values()
            .map(|blocks| blocks.len().saturating_mul(PAGE_SIZE))
            .sum()
    }

    fn live_tuple_bytes(&self) -> usize {
        0
    }

    fn dead_tuple_bytes(&self) -> usize {
        0
    }

    fn index_bytes(&self) -> usize {
        let inserts = self
            .primary_key_inserts
            .values()
            .flat_map(|entries| entries.iter())
            .map(|(key, _)| key.accounted_bytes() + std::mem::size_of::<Tid>())
            .sum::<usize>();
        let deletes = self
            .primary_key_deletes
            .values()
            .flat_map(|keys| keys.iter())
            .map(IndexKey::accounted_bytes)
            .sum::<usize>();
        inserts + deletes
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

impl SessionStorage {
    fn ensure_transaction(&mut self) {
        if self.transaction_stack.is_empty() {
            self.transaction_stack.push(TransactionOverlay::default());
        }
    }

    fn allocate_scan_handle(&mut self) -> u64 {
        let handle = self.next_scan_handle;
        self.next_scan_handle = self.next_scan_handle.checked_add(1).unwrap_or(1);
        if self.next_scan_handle == 0 {
            self.next_scan_handle = 1;
        }
        handle
    }

    fn transaction_bytes(&self) -> usize {
        self.transaction_stack
            .iter()
            .map(TransactionOverlay::accounted_bytes)
            .sum()
    }

    fn transaction_live_tuple_bytes(&self) -> usize {
        self.transaction_stack
            .iter()
            .map(TransactionOverlay::live_tuple_bytes)
            .sum()
    }

    fn transaction_dead_tuple_bytes(&self) -> usize {
        self.transaction_stack
            .iter()
            .map(TransactionOverlay::dead_tuple_bytes)
            .sum()
    }

    fn transaction_index_bytes(&self) -> usize {
        self.transaction_stack
            .iter()
            .map(TransactionOverlay::index_bytes)
            .sum()
    }

    fn scan_bytes(&self) -> usize {
        self.scans
            .values()
            .map(|scan| {
                std::mem::size_of::<ScanState>()
                    + scan
                        .high_water_offsets
                        .capacity()
                        .saturating_mul(std::mem::size_of::<u16>())
            })
            .sum()
    }

    fn owns_inserted_tid(&self, relid: u32, tid: Tid) -> bool {
        self.transaction_stack.iter().rev().any(|overlay| {
            overlay
                .inserted_tids
                .get(&relid)
                .is_some_and(|tids| tids.contains(&tid))
        })
    }

    fn transaction_visible_insert_count(&self, relid: u32) -> usize {
        let mut tids = BTreeSet::new();
        for overlay in &self.transaction_stack {
            if let Some(inserted) = overlay.inserted_tids.get(&relid) {
                tids.extend(inserted.iter().copied());
            }
            if let Some(invalidated) = overlay.invalidated_tids.get(&relid) {
                for tid in invalidated {
                    tids.remove(tid);
                }
            }
        }
        tids.len()
    }

    fn transaction_invalidated_live_count(&self, relid: u32) -> usize {
        let mut tids = BTreeSet::new();
        for overlay in &self.transaction_stack {
            if let Some(invalidated) = overlay.invalidated_tids.get(&relid) {
                tids.extend(invalidated.iter().copied());
            }
        }
        tids.into_iter()
            .filter(|tid| !self.owns_inserted_tid(relid, *tid))
            .count()
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

#[derive(Debug, Default)]
struct StorageState {
    relations: HashMap<u32, RelationStorage>,
    epoch: u64,
    generation: u64,
}

impl StorageState {
    fn relation_mut(&mut self, relid: u32) -> &mut RelationStorage {
        self.relations.entry(relid).or_default()
    }

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
        self.generation = self.generation.saturating_add(1);
    }

    fn abort_explicit_transaction(&mut self, session: &mut SessionStorage) {
        self.rollback_all_overlays(session);
        session.explicit_transaction = false;
        self.epoch = self.epoch.saturating_add(1);
    }

    fn commit_implicit_transaction(&mut self, session: &mut SessionStorage) {
        if session.explicit_transaction {
            return;
        }
        while !session.transaction_stack.is_empty() {
            self.commit_top_overlay(session);
        }
        self.generation = self.generation.saturating_add(1);
    }

    fn abort_implicit_transaction(&mut self, session: &mut SessionStorage) {
        if !session.explicit_transaction {
            self.rollback_all_overlays(session);
            self.epoch = self.epoch.saturating_add(1);
        }
    }

    fn rollback_all_overlays(&mut self, session: &mut SessionStorage) {
        while let Some(overlay) = session.transaction_stack.pop() {
            self.rollback_overlay_from_relations(overlay);
        }
    }

    fn commit_top_overlay(&mut self, session: &mut SessionStorage) {
        let Some(mut overlay) = session.transaction_stack.pop() else {
            return;
        };
        if let Some(parent) = session.transaction_stack.last_mut() {
            parent.append_from(&mut overlay);
            return;
        }
        self.commit_overlay_to_relations(overlay);
    }

    fn commit_overlay_to_relations(&mut self, overlay: TransactionOverlay) {
        for (relid, tids) in &overlay.inserted_tids {
            if let Some(relation) = self.relations.get_mut(relid) {
                for tid in tids {
                    relation.mark_live(*tid);
                }
            }
        }

        for (relid, tids) in &overlay.invalidated_tids {
            if let Some(relation) = self.relations.get_mut(relid) {
                for tid in tids {
                    relation.mark_dead(*tid);
                }
            }
        }

        for (relid, keys) in overlay.primary_key_deletes {
            if let Some(relation) = self.relations.get_mut(&relid) {
                for key in keys {
                    relation.primary_key_index.remove(&key);
                }
            }
        }

        for (relid, entries) in overlay.primary_key_inserts {
            if let Some(relation) = self.relations.get_mut(&relid) {
                for (key, tid) in entries {
                    if relation.tuple_slice(tid, false).is_some() {
                        relation.primary_key_index.insert(key, tid);
                    }
                }
            }
        }
    }

    fn rollback_overlay_from_relations(&mut self, overlay: TransactionOverlay) {
        let has_new_pages = overlay.new_pages.values().any(|blocks| !blocks.is_empty());
        let has_page_rewinds = overlay
            .page_checkpoints
            .values()
            .any(|pages| !pages.is_empty());

        for (relid, blocks) in &overlay.new_pages {
            if let Some(relation) = self.relations.get_mut(relid) {
                for block in blocks {
                    relation.remove_page(*block);
                }
            }
        }

        for (relid, checkpoints) in &overlay.page_checkpoints {
            if let Some(relation) = self.relations.get_mut(relid) {
                for (block, checkpoint) in checkpoints {
                    if let Some(page) = relation.page_mut(*block) {
                        page.restore_to(checkpoint);
                    }
                }
            }
        }

        for (relid, checkpoint) in overlay.relation_checkpoints {
            if let Some(relation) = self.relations.get_mut(&relid) {
                relation.restore_metadata(checkpoint);
            }
        }

        if has_page_rewinds {
            STORAGE2_ARENA_REWINDS.fetch_add(1, Ordering::Relaxed);
        }
        if has_new_pages {
            STORAGE2_ARENA_DROPS.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn clear_relation(&mut self, session: &mut SessionStorage, relid: u32) {
        self.relations.insert(relid, RelationStorage::default());
        for overlay in &mut session.transaction_stack {
            overlay.relation_checkpoints.remove(&relid);
            overlay.page_checkpoints.remove(&relid);
            overlay.new_pages.remove(&relid);
            overlay.inserted_tids.remove(&relid);
            overlay.invalidated_tids.remove(&relid);
            overlay.primary_key_inserts.remove(&relid);
            overlay.primary_key_deletes.remove(&relid);
        }
    }

    fn append_pending_tuple(
        &mut self,
        session: &mut SessionStorage,
        relid: u32,
        tuple: &[u8],
    ) -> Result<Tid, CatalogError> {
        session.ensure_transaction();
        let epoch = self.epoch;
        let generation = self.generation;
        let overlay = session
            .transaction_stack
            .last_mut()
            .expect("transaction was just ensured");
        let relation = self.relation_mut(relid);
        overlay.checkpoint_relation(relid, relation);

        let before_next_block = relation.next_block;
        let block = relation
            .append_target_block(tuple.len(), epoch, generation)
            .ok_or_else(|| storage_limit_error("storage2 could not allocate tuple page"))?;
        if block >= before_next_block {
            overlay.record_new_page(relid, block);
        } else if let Some(page) = relation.page(block) {
            overlay.checkpoint_page(relid, page);
        }

        let tid = relation
            .append_pending_tuple(block, tuple)
            .ok_or_else(|| storage_limit_error("storage2 could not allocate tuple page"))?;
        overlay.insert_tid(relid, tid);
        Ok(tid)
    }

    fn find_visible_tuple<'a>(
        &'a self,
        session: &'a SessionStorage,
        relid: u32,
        tid: Tid,
    ) -> Option<DecodedTuple<'a>> {
        for overlay in session.transaction_stack.iter().rev() {
            if overlay
                .invalidated_tids
                .get(&relid)
                .is_some_and(|tids| tids.contains(&tid))
            {
                return None;
            }
        }

        let tuple = self
            .relations
            .get(&relid)?
            .tuple_slice(tid, session.owns_inserted_tid(relid, tid))?;
        decode_tuple(tid, tuple)
    }

    fn visible_tids(&self, session: &SessionStorage, relid: u32) -> Vec<Tid> {
        let mut tids = Vec::new();
        if let Some(relation) = self.relations.get(&relid) {
            tids.extend(relation.live_tids());
        }
        for overlay in &session.transaction_stack {
            if let Some(inserted) = overlay.inserted_tids.get(&relid) {
                tids.extend(inserted.iter().copied());
            }
        }
        tids.sort_unstable();
        tids.dedup();
        tids.retain(|tid| self.find_visible_tuple(session, relid, *tid).is_some());
        tids
    }

    fn visible_row_count(&self, session: &SessionStorage, relid: u32) -> usize {
        let committed = self
            .relations
            .get(&relid)
            .map(|relation| relation.live_tuple_count)
            .unwrap_or_default();
        committed
            .saturating_add(session.transaction_visible_insert_count(relid))
            .saturating_sub(session.transaction_invalidated_live_count(relid))
    }

    fn next_visible_tid(
        &self,
        session: &SessionStorage,
        relid: u32,
        cursor: ScanCursor,
        high_water_offsets: &[u16],
        forward: bool,
    ) -> Option<Tid> {
        let relation = self.relations.get(&relid)?;
        if forward {
            let mut block = cursor.block;
            while usize::try_from(block).ok()? < high_water_offsets.len() {
                let max_offset = high_water_offsets[block as usize];
                if relation
                    .pages
                    .get(block as usize)
                    .and_then(Option::as_ref)
                    .is_none()
                {
                    block = block.checked_add(1)?;
                    continue;
                }
                let mut offset = if block == cursor.block {
                    cursor.offset
                } else {
                    1
                };
                while offset <= max_offset {
                    let tid = Tid { block, offset };
                    if self.find_visible_tuple(session, relid, tid).is_some() {
                        return Some(tid);
                    }
                    offset = offset.checked_add(1)?;
                }
                block = block.checked_add(1)?;
            }
            return None;
        }

        let mut block = if cursor.block == u32::MAX {
            high_water_offsets.len().checked_sub(1)?.try_into().ok()?
        } else {
            cursor.block
        };
        loop {
            let max_offset = high_water_offsets.get(block as usize).copied()?;
            if relation
                .pages
                .get(block as usize)
                .and_then(Option::as_ref)
                .is_some()
            {
                let mut offset = if block == cursor.block && cursor.offset != u16::MAX {
                    cursor.offset.min(max_offset)
                } else {
                    max_offset
                };
                while offset > 0 {
                    let tid = Tid { block, offset };
                    if self.find_visible_tuple(session, relid, tid).is_some() {
                        return Some(tid);
                    }
                    offset -= 1;
                }
            }
            if block == 0 {
                return None;
            }
            block -= 1;
        }
    }

    fn primary_key_lookup(
        &self,
        session: &SessionStorage,
        relid: u32,
        key: &IndexKey,
    ) -> Option<Tid> {
        for overlay in session.transaction_stack.iter().rev() {
            if let Some(tid) = overlay
                .primary_key_inserts
                .get(&relid)
                .and_then(|entries| entries.get(key))
                .copied()
                && self.find_visible_tuple(session, relid, tid).is_some()
            {
                return Some(tid);
            }
            if overlay
                .primary_key_deletes
                .get(&relid)
                .is_some_and(|keys| keys.contains(key))
            {
                return None;
            }
        }
        let tid = self
            .relations
            .get(&relid)?
            .primary_key_index
            .get(key)
            .copied()?;
        self.find_visible_tuple(session, relid, tid).map(|_| tid)
    }

    fn find_visible_by_index_key_excluding(
        &self,
        session: &SessionStorage,
        relid: u32,
        index_spec: &UniqueIndexSpec,
        key: &IndexKey,
        replacing_tid: Option<Tid>,
    ) -> Option<Tid> {
        if index_spec.is_primary {
            if let Some(tid) = self.primary_key_lookup(session, relid, key)
                && Some(tid) != replacing_tid
            {
                return Some(tid);
            }
            return None;
        }

        self.visible_tids(session, relid).into_iter().find(|tid| {
            Some(*tid) != replacing_tid
                && self
                    .find_visible_tuple(session, relid, *tid)
                    .and_then(|tuple| index_key_for_decoded(index_spec, &tuple.values))
                    .as_ref()
                    == Some(key)
        })
    }

    fn unique_index_conflict_for_input(
        &self,
        session: &SessionStorage,
        relid: u32,
        input: &RowInput<'_>,
        replacing_tid: Option<Tid>,
    ) -> Option<Oid> {
        for index_spec in unique_index_specs_for_relation_oid(Oid(relid)) {
            let Some(key) = index_key_for_input(&index_spec, input) else {
                continue;
            };
            if self
                .find_visible_by_index_key_excluding(
                    session,
                    relid,
                    &index_spec,
                    &key,
                    replacing_tid,
                )
                .is_some()
            {
                return Some(index_spec.index_oid);
            }
        }
        None
    }

    fn metrics(&self, session: &SessionStorage) -> FastPgStorage2Metrics {
        FastPgStorage2Metrics {
            committed_page_bytes: self
                .relations
                .values()
                .map(RelationStorage::accounted_bytes)
                .sum(),
            transaction_page_bytes: session.transaction_bytes(),
            scan_scratch_bytes: session.scan_bytes(),
            live_tuple_bytes: self
                .relations
                .values()
                .map(RelationStorage::live_tuple_bytes)
                .sum::<usize>()
                + session.transaction_live_tuple_bytes(),
            dead_tuple_bytes: self
                .relations
                .values()
                .map(RelationStorage::dead_tuple_bytes)
                .sum::<usize>()
                + session.transaction_dead_tuple_bytes(),
            index_bytes: self
                .relations
                .values()
                .map(RelationStorage::index_bytes)
                .sum::<usize>()
                + session.transaction_index_bytes(),
            page_count: self
                .relations
                .values()
                .map(RelationStorage::page_count)
                .sum::<usize>(),
            arena_rewinds: STORAGE2_ARENA_REWINDS.load(Ordering::Relaxed),
            arena_drops: STORAGE2_ARENA_DROPS.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScanCursor {
    block: u32,
    offset: u16,
}

impl ScanCursor {
    fn forward_start() -> Self {
        Self {
            block: 0,
            offset: 1,
        }
    }

    fn backward_start() -> Self {
        Self {
            block: u32::MAX,
            offset: u16::MAX,
        }
    }

    fn after(tid: Tid) -> Self {
        match tid.offset.checked_add(1) {
            Some(offset) => Self {
                block: tid.block,
                offset,
            },
            None => Self {
                block: tid.block.saturating_add(1),
                offset: 1,
            },
        }
    }

    fn before(tid: Tid) -> Self {
        if tid.offset > 1 {
            Self {
                block: tid.block,
                offset: tid.offset - 1,
            }
        } else {
            Self {
                block: tid.block.saturating_sub(1),
                offset: u16::MAX,
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ScanState {
    relid: u32,
    high_water_offsets: Vec<u16>,
    forward_cursor: ScanCursor,
    backward_cursor: ScanCursor,
}

#[derive(Clone, Copy)]
struct RowInput<'a> {
    values: &'a [usize],
    is_null: &'a [u8],
    byval: &'a [u8],
    value_lens: &'a [usize],
}

#[derive(Clone, Copy)]
enum UniqueCheck {
    Enforce,
    Skip,
}

static STORAGE: OnceLock<Mutex<StorageState>> = OnceLock::new();

fn storage() -> &'static Mutex<StorageState> {
    STORAGE.get_or_init(|| Mutex::new(StorageState::default()))
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

fn invalid_ffi_argument(message: impl Into<String>) -> CatalogError {
    CatalogError::new("22023", message)
}

fn storage_limit_error(message: impl Into<String>) -> CatalogError {
    CatalogError::new(SQLSTATE_PROGRAM_LIMIT_EXCEEDED, message)
}

fn unique_index_spec_for_record(record: &IndexRecord) -> Option<UniqueIndexSpec> {
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

    (!columns.is_empty()).then_some(UniqueIndexSpec {
        index_oid: record.index_oid,
        relation_oid: record.relation_oid,
        is_primary: record.is_primary,
        nulls_not_distinct: record.nulls_not_distinct,
        columns,
    })
}

fn storage2_metadata_cache() -> &'static Mutex<Storage2MetadataCache> {
    STORAGE2_METADATA_CACHE.get_or_init(|| Mutex::new(Storage2MetadataCache::default()))
}

fn with_storage2_metadata_cache<R>(f: impl FnOnce(&mut Storage2MetadataCache) -> R) -> R {
    let generation = current_generation();
    let mut cache = storage2_metadata_cache()
        .lock()
        .expect("storage2 metadata cache mutex poisoned");
    if cache.generation != generation {
        cache.generation = generation;
        cache.unique_specs_by_relation.clear();
        cache.primary_specs_by_index.clear();
    }
    f(&mut cache)
}

fn unique_index_specs_for_relation_oid(relation_oid: Oid) -> Vec<UniqueIndexSpec> {
    if !has_uncommitted_catalog_changes()
        && let Some(cached) = with_storage2_metadata_cache(|cache| {
            cache.unique_specs_by_relation.get(&relation_oid.0).cloned()
        })
    {
        return cached;
    }

    let specs: Vec<_> = unique_index_records_for_relation_oid(relation_oid)
        .iter()
        .filter_map(unique_index_spec_for_record)
        .collect();
    if !has_uncommitted_catalog_changes() {
        with_storage2_metadata_cache(|cache| {
            cache
                .unique_specs_by_relation
                .insert(relation_oid.0, specs.clone());
        });
    }
    specs
}

fn primary_index_spec_for_index_oid(index_oid: Oid) -> Option<UniqueIndexSpec> {
    if !has_uncommitted_catalog_changes()
        && let Some(cached) = with_storage2_metadata_cache(|cache| {
            cache.primary_specs_by_index.get(&index_oid.0).cloned()
        })
    {
        return cached;
    }

    let relid = relation_oid_for_index_oid(index_oid)?;
    let primary_index_oid = primary_key_index_oid_for_relation_oid(relid)?;
    let spec = if primary_index_oid == index_oid {
        unique_index_specs_for_relation_oid(relid)
            .into_iter()
            .find(|spec| spec.index_oid == index_oid && spec.is_primary)
    } else {
        None
    };
    if !has_uncommitted_catalog_changes() {
        with_storage2_metadata_cache(|cache| {
            cache
                .primary_specs_by_index
                .insert(index_oid.0, spec.clone());
        });
    }
    spec
}

fn primary_index_spec_for_relation_oid(relation_oid: Oid) -> Option<UniqueIndexSpec> {
    let primary_index_oid = primary_key_index_oid_for_relation_oid(relation_oid)?;
    primary_index_spec_for_index_oid(primary_index_oid)
}

fn input_arrays<'a>(
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
) -> Option<RowInput<'a>> {
    if natts > 0
        && (values.is_null() || is_null.is_null() || byval.is_null() || value_lens.is_null())
    {
        return None;
    }
    Some(RowInput {
        values: if natts == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(values, natts) }
        },
        is_null: if natts == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(is_null, natts) }
        },
        byval: if natts == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(byval, natts) }
        },
        value_lens: if natts == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(value_lens, natts) }
        },
    })
}

fn key_arrays<'a>(
    values: *const usize,
    is_null: *const u8,
    nkeys: usize,
) -> Option<(&'a [usize], &'a [u8])> {
    if nkeys > 0 && (values.is_null() || is_null.is_null()) {
        return None;
    }
    Some((
        if nkeys == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(values, nkeys) }
        },
        if nkeys == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(is_null, nkeys) }
        },
    ))
}

fn build_tuple(input: &RowInput<'_>) -> Result<Vec<u8>, CatalogError> {
    if input.values.len() != input.is_null.len()
        || input.values.len() != input.byval.len()
        || input.values.len() != input.value_lens.len()
    {
        return Err(invalid_ffi_argument(
            "row input arrays have mismatched lengths",
        ));
    }

    let natts = input.values.len();
    let null_bitmap_len = natts.div_ceil(8);
    let attr_dir_offset = TUPLE_HEADER_LEN + null_bitmap_len;
    let payload_offset = attr_dir_offset + natts.saturating_mul(ATTR_ENTRY_LEN);
    let mut bytes = vec![0; payload_offset];
    bytes[0..4].copy_from_slice(TUPLE_MAGIC);
    write_u16(
        &mut bytes,
        4,
        natts.try_into().map_err(|_| {
            invalid_ffi_argument("row input has too many attributes for storage2 tuple")
        })?,
    );
    write_u16(
        &mut bytes,
        6,
        null_bitmap_len
            .try_into()
            .map_err(|_| invalid_ffi_argument("null bitmap is too large"))?,
    );
    write_u32(
        &mut bytes,
        8,
        attr_dir_offset
            .try_into()
            .map_err(|_| invalid_ffi_argument("attribute directory offset is too large"))?,
    );
    write_u32(
        &mut bytes,
        12,
        payload_offset
            .try_into()
            .map_err(|_| invalid_ffi_argument("tuple payload offset is too large"))?,
    );

    for index in 0..natts {
        let entry = attr_dir_offset + index * ATTR_ENTRY_LEN;
        if input.is_null[index] != 0 {
            bytes[TUPLE_HEADER_LEN + index / 8] |= 1 << (index % 8);
            bytes[entry] = 0;
            continue;
        }

        if input.byval[index] != 0 {
            bytes[entry] = 1;
            write_u64(&mut bytes, entry + 8, input.values[index] as u64);
            continue;
        }

        let len = input.value_lens[index];
        if input.values[index] == 0 && len > 0 {
            return Err(invalid_ffi_argument(
                "non-null by-reference value has null pointer",
            ));
        }
        let source = if len == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(input.values[index] as *const u8, len) }
        };
        let offset = bytes.len() - payload_offset;
        bytes.extend_from_slice(source);
        bytes[entry] = 2;
        write_u64(&mut bytes, entry + 8, offset as u64);
        write_u64(&mut bytes, entry + 16, len as u64);
    }

    if bytes.len() > u32::MAX as usize {
        return Err(storage_limit_error("storage2 tuple is too large"));
    }
    Ok(bytes)
}

fn decode_tuple(tid: Tid, tuple: &[u8]) -> Option<DecodedTuple<'_>> {
    if tuple.len() < TUPLE_HEADER_LEN || tuple.get(0..4)? != TUPLE_MAGIC {
        return None;
    }
    let natts = read_u16(tuple, 4)? as usize;
    let null_bitmap_len = read_u16(tuple, 6)? as usize;
    let attr_dir_offset = read_u32(tuple, 8)? as usize;
    let payload_offset = read_u32(tuple, 12)? as usize;
    if attr_dir_offset != TUPLE_HEADER_LEN + null_bitmap_len {
        return None;
    }
    if payload_offset < attr_dir_offset
        || payload_offset.checked_add(0)? > tuple.len()
        || payload_offset != attr_dir_offset.checked_add(natts.checked_mul(ATTR_ENTRY_LEN)?)?
    {
        return None;
    }

    let mut values = Vec::with_capacity(natts);
    for index in 0..natts {
        let null = tuple
            .get(TUPLE_HEADER_LEN + index / 8)
            .is_some_and(|byte| byte & (1 << (index % 8)) != 0);
        let entry = attr_dir_offset + index * ATTR_ENTRY_LEN;
        let tag = *tuple.get(entry)?;
        if null || tag == 0 {
            values.push(DecodedDatum::Null);
            continue;
        }
        match tag {
            1 => values.push(DecodedDatum::ByValue(read_u64(tuple, entry + 8)? as usize)),
            2 => {
                let offset = read_u64(tuple, entry + 8)? as usize;
                let len = read_u64(tuple, entry + 16)? as usize;
                let start = payload_offset.checked_add(offset)?;
                let end = start.checked_add(len)?;
                values.push(DecodedDatum::ByRef(tuple.get(start..end)?));
            }
            _ => return None,
        }
    }
    Some(DecodedTuple { tid, values })
}

fn index_key_for_input(index_spec: &UniqueIndexSpec, input: &RowInput<'_>) -> Option<IndexKey> {
    let mut parts = Vec::with_capacity(index_spec.columns.len());
    for column in &index_spec.columns {
        let index = column.column_index;
        if *input.is_null.get(index)? != 0 {
            if !index_spec.nulls_not_distinct {
                return None;
            }
            parts.push(IndexKeyPart::Null);
            continue;
        }
        if column.typbyval || *input.byval.get(index)? != 0 {
            parts.push(IndexKeyPart::ByValue(*input.values.get(index)?));
            continue;
        }
        let value = *input.values.get(index)?;
        let len = byref_len(column.typlen, value, Some(*input.value_lens.get(index)?))?;
        let bytes = if len == 0 {
            Vec::new()
        } else {
            if value == 0 {
                return None;
            }
            unsafe { slice::from_raw_parts(value as *const u8, len) }.to_vec()
        };
        parts.push(IndexKeyPart::Bytes(bytes));
    }
    Some(IndexKey { parts })
}

fn index_key_for_decoded(
    index_spec: &UniqueIndexSpec,
    values: &[DecodedDatum<'_>],
) -> Option<IndexKey> {
    let mut parts = Vec::with_capacity(index_spec.columns.len());
    for column in &index_spec.columns {
        match values.get(column.column_index)? {
            DecodedDatum::Null => {
                if !index_spec.nulls_not_distinct {
                    return None;
                }
                parts.push(IndexKeyPart::Null);
            }
            DecodedDatum::ByValue(value) => parts.push(IndexKeyPart::ByValue(*value)),
            DecodedDatum::ByRef(bytes) => {
                if column.typbyval {
                    return None;
                }
                let len = byref_len_from_bytes(column.typlen, bytes)?;
                parts.push(IndexKeyPart::Bytes(bytes.get(..len)?.to_vec()));
            }
        }
    }
    Some(IndexKey { parts })
}

fn index_key_for_key_datums(
    index_spec: &UniqueIndexSpec,
    values: &[usize],
    is_null: &[u8],
) -> Option<IndexKey> {
    if values.len() != index_spec.columns.len() || values.len() != is_null.len() {
        return None;
    }
    let mut parts = Vec::with_capacity(values.len());
    for (key_index, column) in index_spec.columns.iter().enumerate() {
        if is_null[key_index] != 0 {
            if !index_spec.nulls_not_distinct {
                return None;
            }
            parts.push(IndexKeyPart::Null);
            continue;
        }
        if column.typbyval {
            parts.push(IndexKeyPart::ByValue(values[key_index]));
            continue;
        }
        let len = byref_len(column.typlen, values[key_index], None)?;
        let bytes = if len == 0 {
            Vec::new()
        } else {
            unsafe { slice::from_raw_parts(values[key_index] as *const u8, len) }.to_vec()
        };
        parts.push(IndexKeyPart::Bytes(bytes));
    }
    Some(IndexKey { parts })
}

fn byref_len(typlen: i16, value: usize, fallback_len: Option<usize>) -> Option<usize> {
    if typlen > 0 {
        return Some(typlen as usize);
    }
    if let Some(len) = fallback_len
        && len > 0
    {
        return Some(len);
    }
    match typlen {
        -1 => varlena_payload_len(value),
        -2 => c_string_payload_len(value),
        _ => None,
    }
}

fn byref_len_from_bytes(typlen: i16, bytes: &[u8]) -> Option<usize> {
    if typlen > 0 {
        return Some((typlen as usize).min(bytes.len()));
    }
    match typlen {
        -1 => varlena_payload_len(bytes.as_ptr() as usize).filter(|len| *len <= bytes.len()),
        -2 => bytes
            .iter()
            .position(|byte| *byte == 0)
            .map(|index| index + 1),
        _ => Some(bytes.len()),
    }
}

fn varlena_payload_len(value: usize) -> Option<usize> {
    if value == 0 {
        return None;
    }
    let raw = unsafe { std::ptr::read_unaligned(value as *const u32) };
    let len = if cfg!(target_endian = "little") {
        (raw >> 2) as usize
    } else {
        raw as usize
    };
    (len >= 4).then_some(len)
}

fn c_string_payload_len(value: usize) -> Option<usize> {
    if value == 0 {
        return None;
    }
    let mut len = 0usize;
    loop {
        let byte = unsafe { std::ptr::read((value as *const u8).add(len)) };
        len = len.checked_add(1)?;
        if byte == 0 {
            return Some(len);
        }
    }
}

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_ne_bytes());
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_ne_bytes());
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let mut value = [0u8; 2];
    value.copy_from_slice(bytes.get(offset..offset + 2)?);
    Some(u16::from_ne_bytes(value))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let mut value = [0u8; 4];
    value.copy_from_slice(bytes.get(offset..offset + 4)?);
    Some(u32::from_ne_bytes(value))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let mut value = [0u8; 8];
    value.copy_from_slice(bytes.get(offset..offset + 8)?);
    Some(u64::from_ne_bytes(value))
}

fn write_storage_error(out: *mut c_char, out_len: usize, value: &str) {
    if out.is_null() || out_len == 0 {
        return;
    }
    let bytes = value.as_bytes();
    let copy_len = bytes.len().min(out_len.saturating_sub(1));
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out as *mut u8, copy_len);
        *out.add(copy_len) = 0;
    }
}

fn copy_decoded_to_outputs(
    tuple: &DecodedTuple<'_>,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
    tid_out: *mut u64,
) -> bool {
    if tuple.values.len() != natts {
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
    for (index, value) in tuple.values.iter().enumerate() {
        match value {
            DecodedDatum::Null => {
                values_out[index] = 0;
                is_null_out[index] = 1;
            }
            DecodedDatum::ByValue(value) => {
                values_out[index] = *value;
                is_null_out[index] = 0;
            }
            DecodedDatum::ByRef(bytes) => {
                values_out[index] = bytes.as_ptr() as usize;
                is_null_out[index] = 0;
            }
        }
    }
    if !tid_out.is_null() {
        unsafe {
            *tid_out = tuple.tid.pack();
        }
    }
    true
}

fn relation_insert_impl(
    relid: u32,
    input: RowInput<'_>,
    tid_out: *mut u64,
    unique_check: UniqueCheck,
) -> bool {
    let result = with_storage(|state, session| -> Result<Option<Tid>, CatalogError> {
        if matches!(unique_check, UniqueCheck::Enforce)
            && state
                .unique_index_conflict_for_input(session, relid, &input, None)
                .is_some()
        {
            return Ok(None);
        }

        let tuple = build_tuple(&input)?;
        let tid = state.append_pending_tuple(session, relid, &tuple)?;

        if let Some(index_spec) = primary_index_spec_for_relation_oid(Oid(relid))
            && let Some(key) = index_key_for_input(&index_spec, &input)
        {
            session
                .transaction_stack
                .last_mut()
                .expect("transaction was just ensured")
                .insert_primary_key(relid, key, tid);
        }
        Ok(Some(tid))
    });

    match result {
        Ok(Some(tid)) => {
            if !tid_out.is_null() {
                unsafe {
                    *tid_out = tid.pack();
                }
            }
            true
        }
        Ok(None) => false,
        Err(error) => {
            set_last_storage_error(error);
            false
        }
    }
}

fn relation_update_impl(
    relid: u32,
    packed_tid: u64,
    input: RowInput<'_>,
    new_tid_out: *mut u64,
    unique_check: UniqueCheck,
) -> bool {
    let Some(old_tid) = Tid::unpack(packed_tid) else {
        return false;
    };
    let result = with_storage(|state, session| -> Result<Option<Tid>, CatalogError> {
        let Some(old_tuple) = state.find_visible_tuple(session, relid, old_tid) else {
            return Ok(None);
        };
        if matches!(unique_check, UniqueCheck::Enforce)
            && state
                .unique_index_conflict_for_input(session, relid, &input, Some(old_tid))
                .is_some()
        {
            return Ok(None);
        }
        let old_primary_key = primary_index_spec_for_relation_oid(Oid(relid))
            .and_then(|index_spec| index_key_for_decoded(&index_spec, &old_tuple.values));
        drop(old_tuple);

        let tuple = build_tuple(&input)?;
        let new_tid = state.append_pending_tuple(session, relid, &tuple)?;

        let overlay = session
            .transaction_stack
            .last_mut()
            .expect("transaction was just ensured");
        overlay.invalidate(relid, old_tid);
        if let Some(key) = old_primary_key {
            overlay.delete_primary_key(relid, key);
        }
        if let Some(index_spec) = primary_index_spec_for_relation_oid(Oid(relid))
            && let Some(key) = index_key_for_input(&index_spec, &input)
        {
            overlay.insert_primary_key(relid, key, new_tid);
        }
        Ok(Some(new_tid))
    });

    match result {
        Ok(Some(tid)) => {
            if !new_tid_out.is_null() {
                unsafe {
                    *new_tid_out = tid.pack();
                }
            }
            true
        }
        Ok(None) => false,
        Err(error) => {
            set_last_storage_error(error);
            false
        }
    }
}

pub struct CopyDatum {
    value: usize,
    byval: bool,
    value_len: usize,
    payload: Option<Box<[u8]>>,
}

impl CopyDatum {
    pub fn by_value(value: usize) -> Self {
        Self {
            value,
            byval: true,
            value_len: 0,
            payload: None,
        }
    }

    pub fn by_reference(payload: Vec<u8>) -> Self {
        let payload = payload.into_boxed_slice();
        Self {
            value: 0,
            byval: false,
            value_len: payload.len(),
            payload: Some(payload),
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

    let datums = fields
        .iter()
        .zip(&relation.columns)
        .map(|(field, column)| {
            if *field == "\\N" {
                Ok(None)
            } else {
                copy_text_field_to_datum(field, column.type_oid).map(Some)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    insert_copy_datums_for_relation(&relation, datums)
}

pub fn insert_copy_datums(table: &str, datums: Vec<Option<CopyDatum>>) -> Result<bool, String> {
    let relation = relation_by_name(table)
        .ok_or_else(|| format!("relation \"{}\" does not exist", table.trim()))?;
    insert_copy_datums_for_relation(&relation, datums)
}

fn insert_copy_datums_for_relation(
    relation: &fastpg_catalog::RelationRecord,
    datums: Vec<Option<CopyDatum>>,
) -> Result<bool, String> {
    if datums.len() != relation.columns.len() {
        return Err(format!(
            "COPY row for relation \"{}\" has {} fields but {} columns",
            relation.name,
            datums.len(),
            relation.columns.len()
        ));
    }

    let mut values = Vec::with_capacity(relation.columns.len());
    let mut is_null = Vec::with_capacity(relation.columns.len());
    let mut byval = Vec::with_capacity(relation.columns.len());
    let mut value_lens = Vec::with_capacity(relation.columns.len());
    let mut byref_payloads = Vec::<Box<[u8]>>::new();

    for datum in datums {
        let Some(copy_value) = datum else {
            values.push(0);
            is_null.push(1);
            byval.push(0);
            value_lens.push(0);
            continue;
        };

        let CopyDatum {
            mut value,
            byval: datum_byval,
            value_len,
            payload,
        } = copy_value;
        if let Some(payload) = payload {
            value = payload.as_ptr() as usize;
            byref_payloads.push(payload);
        }
        values.push(value);
        is_null.push(0);
        byval.push(u8::from(datum_byval));
        value_lens.push(value_len);
    }

    let mut tid = 0u64;
    let inserted = unsafe {
        fastpg_storage2_relation_insert(
            relation.oid.0,
            values.as_ptr(),
            is_null.as_ptr(),
            byval.as_ptr(),
            value_lens.as_ptr(),
            relation.columns.len(),
            &mut tid,
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
pub extern "C" fn fastpg_storage2_xact_begin() {
    with_storage(|state, session| state.begin_explicit_transaction(session));
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_xact_begin_implicit() {
    with_storage(|_state, session| session.ensure_transaction());
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_xact_commit() {
    with_storage(|state, session| state.commit_explicit_transaction(session));
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_xact_abort() {
    with_storage(|state, session| state.abort_explicit_transaction(session));
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_xact_commit_if_implicit() {
    with_storage(|state, session| state.commit_implicit_transaction(session));
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_xact_abort_if_implicit() {
    with_storage(|state, session| state.abort_implicit_transaction(session));
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_subxact_begin() {
    with_storage(|_state, session| {
        session.ensure_transaction();
        session
            .transaction_stack
            .push(TransactionOverlay::default());
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_subxact_commit() {
    with_storage(|state, session| {
        if session.transaction_stack.len() > 1 {
            state.commit_top_overlay(session);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_subxact_abort() {
    with_storage(|state, session| {
        if session.transaction_stack.len() > 1
            && let Some(overlay) = session.transaction_stack.pop()
        {
            state.rollback_overlay_from_relations(overlay);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_relation_clear(relid: u32) {
    with_storage(|state, session| state.clear_relation(session, relid));
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_relation_row_count(relid: u32) -> usize {
    with_storage(|state, session| state.visible_row_count(session, relid))
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_relation_contains_tid(relid: u32, packed_tid: u64) -> bool {
    let Some(tid) = Tid::unpack(packed_tid) else {
        return false;
    };
    with_storage(|state, session| state.find_visible_tuple(session, relid, tid).is_some())
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid row input arrays and an optional valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_relation_insert(
    relid: u32,
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
    tid_out: *mut u64,
) -> bool {
    clear_last_storage_error();
    let Some(input) = input_arrays(values, is_null, byval, value_lens, natts) else {
        set_last_storage_error(invalid_ffi_argument("invalid row input arrays"));
        return false;
    };
    relation_insert_impl(relid, input, tid_out, UniqueCheck::Enforce)
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid row input arrays and an optional valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_relation_insert_unchecked(
    relid: u32,
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
    tid_out: *mut u64,
) -> bool {
    clear_last_storage_error();
    let Some(input) = input_arrays(values, is_null, byval, value_lens, natts) else {
        set_last_storage_error(invalid_ffi_argument("invalid row input arrays"));
        return false;
    };
    relation_insert_impl(relid, input, tid_out, UniqueCheck::Skip)
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid row input arrays and an optional valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_relation_update(
    relid: u32,
    packed_tid: u64,
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
    new_tid_out: *mut u64,
) -> bool {
    clear_last_storage_error();
    let Some(input) = input_arrays(values, is_null, byval, value_lens, natts) else {
        set_last_storage_error(invalid_ffi_argument("invalid row input arrays"));
        return false;
    };
    relation_update_impl(relid, packed_tid, input, new_tid_out, UniqueCheck::Enforce)
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid row input arrays and an optional valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_relation_update_unchecked(
    relid: u32,
    packed_tid: u64,
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
    new_tid_out: *mut u64,
) -> bool {
    clear_last_storage_error();
    let Some(input) = input_arrays(values, is_null, byval, value_lens, natts) else {
        set_last_storage_error(invalid_ffi_argument("invalid row input arrays"));
        return false;
    };
    relation_update_impl(relid, packed_tid, input, new_tid_out, UniqueCheck::Skip)
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_relation_delete(relid: u32, packed_tid: u64) -> bool {
    clear_last_storage_error();
    let Some(tid) = Tid::unpack(packed_tid) else {
        return false;
    };
    with_storage(|state, session| {
        let Some(tuple) = state.find_visible_tuple(session, relid, tid) else {
            return false;
        };
        let old_primary_key = primary_index_spec_for_relation_oid(Oid(relid))
            .and_then(|index_spec| index_key_for_decoded(&index_spec, &tuple.values));
        drop(tuple);
        session.ensure_transaction();
        let overlay = session
            .transaction_stack
            .last_mut()
            .expect("transaction was just ensured");
        overlay.invalidate(relid, tid);
        if let Some(key) = old_primary_key {
            overlay.delete_primary_key(relid, key);
        }
        true
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_scan_begin(relid: u32) -> u64 {
    clear_last_storage_error();
    with_storage(|state, session| {
        let high_water_offsets = state
            .relations
            .get(&relid)
            .map(|relation| {
                relation
                    .pages
                    .iter()
                    .map(|page| {
                        page.as_ref()
                            .and_then(|page| u16::try_from(page.line_pointers.len()).ok())
                            .unwrap_or_default()
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let handle = session.allocate_scan_handle();
        session.scans.insert(
            handle,
            ScanState {
                relid,
                high_water_offsets,
                forward_cursor: ScanCursor::forward_start(),
                backward_cursor: ScanCursor::backward_start(),
            },
        );
        handle
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_scan_reset(scan_handle: u64) {
    with_storage(|_state, session| {
        if let Some(scan) = session.scans.get_mut(&scan_handle) {
            scan.forward_cursor = ScanCursor::forward_start();
            scan.backward_cursor = ScanCursor::backward_start();
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_scan_end(scan_handle: u64) {
    with_storage(|_state, session| {
        session.scans.remove(&scan_handle);
    });
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output buffers for `natts` entries.
pub unsafe extern "C" fn fastpg_storage2_scan_next(
    scan_handle: u64,
    forward: u8,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
    tid_out: *mut u64,
) -> bool {
    with_storage(|state, session| {
        let Some(mut scan) = session.scans.remove(&scan_handle) else {
            return false;
        };
        let cursor = if forward != 0 {
            scan.forward_cursor
        } else {
            scan.backward_cursor
        };
        let relid = scan.relid;
        let Some(tid) = state.next_visible_tid(
            session,
            relid,
            cursor,
            &scan.high_water_offsets,
            forward != 0,
        ) else {
            session.scans.insert(scan_handle, scan);
            return false;
        };
        if forward != 0 {
            scan.forward_cursor = ScanCursor::after(tid);
        } else {
            scan.backward_cursor = ScanCursor::before(tid);
        }
        session.scans.insert(scan_handle, scan);
        let Some(tuple) = state.find_visible_tuple(session, relid, tid) else {
            return false;
        };
        copy_decoded_to_outputs(&tuple, values_out, is_null_out, natts, tid_out)
    })
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output buffers for `natts` entries.
pub unsafe extern "C" fn fastpg_storage2_fetch_tid(
    relid: u32,
    packed_tid: u64,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
) -> bool {
    let Some(tid) = Tid::unpack(packed_tid) else {
        return false;
    };
    with_storage(|state, session| {
        let Some(tuple) = state.find_visible_tuple(session, relid, tid) else {
            return false;
        };
        copy_decoded_to_outputs(&tuple, values_out, is_null_out, natts, std::ptr::null_mut())
    })
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid key input arrays and an optional valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_primary_key_index_lookup(
    index_relid: u32,
    values: *const usize,
    is_null: *const u8,
    nkeys: usize,
    tid_out: *mut u64,
) -> bool {
    clear_last_storage_error();
    let Some((values, is_null)) = key_arrays(values, is_null, nkeys) else {
        return false;
    };
    let Some(index_spec) = primary_index_spec_for_index_oid(Oid(index_relid)) else {
        return false;
    };
    let Some(key) = index_key_for_key_datums(&index_spec, values, is_null) else {
        return false;
    };
    let tid = with_storage(|state, session| {
        state.primary_key_lookup(session, index_spec.relation_oid.0, &key)
    });
    let Some(tid) = tid else {
        return false;
    };
    if !tid_out.is_null() {
        unsafe {
            *tid_out = tid.pack();
        }
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid key input arrays and an optional valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_unique_index_conflict(
    index_relid: u32,
    values: *const usize,
    is_null: *const u8,
    nkeys: usize,
    replacing_tid: u64,
    tid_out: *mut u64,
) -> bool {
    clear_last_storage_error();
    let Some((values, is_null)) = key_arrays(values, is_null, nkeys) else {
        return false;
    };
    let Some(relid) = relation_oid_for_index_oid(Oid(index_relid)) else {
        return false;
    };
    let Some(index_spec) = unique_index_records_for_relation_oid(relid)
        .iter()
        .filter_map(unique_index_spec_for_record)
        .find(|spec| spec.index_oid == Oid(index_relid))
    else {
        return false;
    };
    let Some(key) = index_key_for_key_datums(&index_spec, values, is_null) else {
        return false;
    };
    let replacing_tid = if replacing_tid == 0 {
        None
    } else {
        Tid::unpack(replacing_tid)
    };
    let conflict = with_storage(|state, session| {
        state.find_visible_by_index_key_excluding(
            session,
            relid.0,
            &index_spec,
            &key,
            replacing_tid,
        )
    });
    let Some(tid) = conflict else {
        return false;
    };
    if !tid_out.is_null() {
        unsafe {
            *tid_out = tid.pack();
        }
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass a valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_metrics(out: *mut FastPgStorage2Metrics) -> bool {
    if out.is_null() {
        return false;
    }
    let metrics = with_storage(|state, session| state.metrics(session));
    unsafe {
        *out = metrics;
    }
    true
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_committed_page_bytes() -> usize {
    with_storage(|state, session| state.metrics(session).committed_page_bytes)
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_transaction_page_bytes() -> usize {
    with_storage(|state, session| state.metrics(session).transaction_page_bytes)
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_index_bytes() -> usize {
    with_storage(|state, session| state.metrics(session).index_bytes)
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output buffers when non-null.
pub unsafe extern "C" fn fastpg_storage2_last_error(
    sqlstate_out: *mut c_char,
    sqlstate_len: usize,
    message_out: *mut c_char,
    message_len: usize,
) -> bool {
    let Some(error) = last_storage_error() else {
        return false;
    };
    write_storage_error(sqlstate_out, sqlstate_len, &error.sqlstate);
    write_storage_error(message_out, message_len, &error.message);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex as StdMutex, MutexGuard};

    static TEST_LOCK: StdMutex<()> = StdMutex::new(());

    struct TestGuard {
        _guard: MutexGuard<'static, ()>,
        _session_guard: SessionStorageGuard,
    }

    impl Drop for TestGuard {
        fn drop(&mut self) {
            fastpg_storage2_xact_abort();
        }
    }

    fn test_guard() -> TestGuard {
        let guard = TEST_LOCK.lock().expect("test lock poisoned");
        let session = new_session_storage();
        let session_guard = enter_session_storage(session);
        fastpg_storage2_xact_abort();
        TestGuard {
            _guard: guard,
            _session_guard: session_guard,
        }
    }

    fn insert_i32(relid: u32, value: i32) -> u64 {
        let values = [value as usize];
        let nulls = [0u8];
        let byval = [1u8];
        let lens = [0usize];
        let mut tid = 0;
        assert!(unsafe {
            fastpg_storage2_relation_insert_unchecked(
                relid,
                values.as_ptr(),
                nulls.as_ptr(),
                byval.as_ptr(),
                lens.as_ptr(),
                values.len(),
                &mut tid,
            )
        });
        tid
    }

    fn fetch_i32(relid: u32, tid: u64) -> Option<i32> {
        let mut values = [0usize];
        let mut nulls = [1u8];
        if unsafe {
            fastpg_storage2_fetch_tid(relid, tid, values.as_mut_ptr(), nulls.as_mut_ptr(), 1)
        } {
            Some(values[0] as i32)
        } else {
            None
        }
    }

    #[test]
    fn insert_fetch_uses_stable_tid() {
        let _guard = test_guard();
        let relid = 42;
        let tid = insert_i32(relid, 7);
        assert_ne!(tid, 0);
        assert_eq!(fetch_i32(relid, tid), Some(7));
        assert_eq!(fastpg_storage2_relation_row_count(relid), 1);
    }

    #[test]
    fn explicit_abort_drops_transaction_arena() {
        let _guard = test_guard();
        let relid = 43;
        fastpg_storage2_xact_begin();
        let tid = insert_i32(relid, 1);
        assert_eq!(fetch_i32(relid, tid), Some(1));
        assert!(fastpg_storage2_transaction_page_bytes() >= PAGE_SIZE);
        fastpg_storage2_xact_abort();
        assert_eq!(fetch_i32(relid, tid), None);
        assert_eq!(fastpg_storage2_relation_row_count(relid), 0);
        assert_eq!(fastpg_storage2_transaction_page_bytes(), 0);
    }

    #[test]
    fn implicit_statement_abort_preserves_prior_implicit_commit() {
        let _guard = test_guard();
        let relid = 430;

        fastpg_storage2_xact_begin_implicit();
        let committed_tid = insert_i32(relid, 1);
        fastpg_storage2_xact_commit_if_implicit();
        assert_eq!(fetch_i32(relid, committed_tid), Some(1));
        assert_eq!(fastpg_storage2_relation_row_count(relid), 1);

        fastpg_storage2_xact_begin_implicit();
        let aborted_tid = insert_i32(relid, 2);
        assert_eq!(fetch_i32(relid, aborted_tid), Some(2));
        fastpg_storage2_xact_abort_if_implicit();

        assert_eq!(fetch_i32(relid, committed_tid), Some(1));
        assert_eq!(fetch_i32(relid, aborted_tid), None);
        assert_eq!(fastpg_storage2_relation_row_count(relid), 1);
    }

    #[test]
    fn commit_publishes_pages_and_delete_rollback_restores_visibility() {
        let _guard = test_guard();
        let relid = 44;
        fastpg_storage2_xact_begin();
        let tid = insert_i32(relid, 2);
        fastpg_storage2_xact_commit();
        assert_eq!(fetch_i32(relid, tid), Some(2));
        assert!(fastpg_storage2_committed_page_bytes() >= PAGE_SIZE);

        fastpg_storage2_xact_begin();
        assert!(fastpg_storage2_relation_delete(relid, tid));
        assert_eq!(fetch_i32(relid, tid), None);
        fastpg_storage2_xact_abort();
        assert_eq!(fetch_i32(relid, tid), Some(2));
    }

    #[test]
    fn update_appends_new_tid_and_abort_restores_old_tid() {
        let _guard = test_guard();
        let relid = 45;
        fastpg_storage2_xact_begin();
        let old_tid = insert_i32(relid, 3);
        fastpg_storage2_xact_commit();

        let values = [4usize];
        let nulls = [0u8];
        let byval = [1u8];
        let lens = [0usize];
        let mut new_tid = 0;
        fastpg_storage2_xact_begin();
        assert!(unsafe {
            fastpg_storage2_relation_update_unchecked(
                relid,
                old_tid,
                values.as_ptr(),
                nulls.as_ptr(),
                byval.as_ptr(),
                lens.as_ptr(),
                values.len(),
                &mut new_tid,
            )
        });
        assert_ne!(old_tid, new_tid);
        assert_eq!(fetch_i32(relid, old_tid), None);
        assert_eq!(fetch_i32(relid, new_tid), Some(4));
        fastpg_storage2_xact_abort();
        assert_eq!(fetch_i32(relid, old_tid), Some(3));
        assert_eq!(fetch_i32(relid, new_tid), None);
    }

    #[test]
    fn savepoint_abort_drops_nested_pages() {
        let _guard = test_guard();
        let relid = 46;
        fastpg_storage2_xact_begin();
        let parent_tid = insert_i32(relid, 5);
        let bytes_before = fastpg_storage2_transaction_page_bytes();
        fastpg_storage2_subxact_begin();
        let nested_tid = insert_i32(relid, 6);
        assert_eq!(fetch_i32(relid, nested_tid), Some(6));
        fastpg_storage2_subxact_abort();
        assert_eq!(fetch_i32(relid, parent_tid), Some(5));
        assert_eq!(fetch_i32(relid, nested_tid), None);
        assert!(fastpg_storage2_transaction_page_bytes() <= bytes_before);
        fastpg_storage2_xact_commit();
        assert_eq!(fetch_i32(relid, parent_tid), Some(5));
    }

    #[test]
    fn scan_tracks_tids_not_materialized_rows() {
        let _guard = test_guard();
        let relid = 47;
        fastpg_storage2_xact_begin();
        insert_i32(relid, 10);
        insert_i32(relid, 11);
        let scan = fastpg_storage2_scan_begin(relid);
        assert_ne!(scan, 0);
        assert!(fastpg_storage2_metrics_snapshot().scan_scratch_bytes <= 256);
        let mut values = [0usize];
        let mut nulls = [1u8];
        let mut tid = 0u64;
        assert!(unsafe {
            fastpg_storage2_scan_next(
                scan,
                1,
                values.as_mut_ptr(),
                nulls.as_mut_ptr(),
                1,
                &mut tid,
            )
        });
        assert_eq!(values[0], 10);
        assert!(unsafe {
            fastpg_storage2_scan_next(
                scan,
                1,
                values.as_mut_ptr(),
                nulls.as_mut_ptr(),
                1,
                &mut tid,
            )
        });
        assert_eq!(values[0], 11);
        assert!(!unsafe {
            fastpg_storage2_scan_next(
                scan,
                1,
                values.as_mut_ptr(),
                nulls.as_mut_ptr(),
                1,
                &mut tid,
            )
        });
        fastpg_storage2_scan_end(scan);
    }

    #[test]
    fn committed_small_transactions_pack_into_relation_pages() {
        let _guard = test_guard();
        let relid = 48;
        let before = fastpg_storage2_metrics_snapshot();
        for value in 0..100 {
            fastpg_storage2_xact_begin();
            insert_i32(relid, value);
            fastpg_storage2_xact_commit();
        }

        let after = fastpg_storage2_metrics_snapshot();
        assert_eq!(fastpg_storage2_relation_row_count(relid), 100);
        assert_eq!(after.page_count.saturating_sub(before.page_count), 1);
        assert!(
            after
                .committed_page_bytes
                .saturating_sub(before.committed_page_bytes)
                < PAGE_SIZE * 2
        );
    }

    fn fastpg_storage2_metrics_snapshot() -> FastPgStorage2Metrics {
        let mut metrics = FastPgStorage2Metrics::default();
        assert!(unsafe { fastpg_storage2_metrics(&mut metrics) });
        metrics
    }
}
