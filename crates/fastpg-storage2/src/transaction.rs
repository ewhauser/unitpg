use crate::*;

#[derive(Debug, Default)]
pub(crate) struct TransactionOverlay {
    pub(crate) relation_checkpoints: HashMap<u32, RelationCheckpoint>,
    pub(crate) page_checkpoints: HashMap<u32, BTreeMap<u32, PageCheckpoint>>,
    pub(crate) new_pages: HashMap<u32, BTreeSet<u32>>,
    pub(crate) inserted_tids: HashMap<u32, BTreeSet<Tid>>,
    pub(crate) invalidated_tids: HashMap<u32, BTreeSet<Tid>>,
    pub(crate) primary_key_inserts: HashMap<u32, BTreeMap<IndexKey, Tid>>,
    pub(crate) primary_key_deletes: HashMap<u32, BTreeSet<IndexKey>>,
}

impl TransactionOverlay {
    pub(crate) fn checkpoint_relation(&mut self, relid: u32, relation: &RelationStorage) {
        self.relation_checkpoints
            .entry(relid)
            .or_insert_with(|| relation.checkpoint());
    }

    pub(crate) fn checkpoint_page(&mut self, relid: u32, page: &Page) {
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

    pub(crate) fn record_new_page(&mut self, relid: u32, block: u32) {
        self.new_pages.entry(relid).or_default().insert(block);
    }

    pub(crate) fn insert_tid(&mut self, relid: u32, tid: Tid) {
        self.inserted_tids.entry(relid).or_default().insert(tid);
    }

    pub(crate) fn invalidate(&mut self, relid: u32, tid: Tid) {
        self.invalidated_tids.entry(relid).or_default().insert(tid);
    }

    pub(crate) fn delete_primary_key(&mut self, relid: u32, key: IndexKey) {
        self.primary_key_deletes
            .entry(relid)
            .or_default()
            .insert(key);
    }

    pub(crate) fn insert_primary_key(&mut self, relid: u32, key: IndexKey, tid: Tid) {
        self.primary_key_inserts
            .entry(relid)
            .or_default()
            .insert(key, tid);
    }

    pub(crate) fn append_from(&mut self, other: &mut Self) {
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

    pub(crate) fn accounted_bytes(&self) -> usize {
        self.new_pages
            .values()
            .map(|blocks| blocks.len().saturating_mul(PAGE_SIZE))
            .sum()
    }

    pub(crate) fn live_tuple_bytes(&self) -> usize {
        0
    }

    pub(crate) fn dead_tuple_bytes(&self) -> usize {
        0
    }

    pub(crate) fn index_bytes(&self) -> usize {
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
    pub(crate) transaction_stack: Vec<TransactionOverlay>,
    pub(crate) explicit_transaction: bool,
    pub(crate) scans: HashMap<u64, ScanState>,
    pub(crate) next_scan_handle: u64,
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
    pub(crate) fn ensure_transaction(&mut self) {
        if self.transaction_stack.is_empty() {
            self.transaction_stack.push(TransactionOverlay::default());
        }
    }

    pub(crate) fn allocate_scan_handle(&mut self) -> u64 {
        let handle = self.next_scan_handle;
        self.next_scan_handle = self.next_scan_handle.checked_add(1).unwrap_or(1);
        if self.next_scan_handle == 0 {
            self.next_scan_handle = 1;
        }
        handle
    }

    pub(crate) fn transaction_bytes(&self) -> usize {
        self.transaction_stack
            .iter()
            .map(TransactionOverlay::accounted_bytes)
            .sum()
    }

    pub(crate) fn transaction_live_tuple_bytes(&self) -> usize {
        self.transaction_stack
            .iter()
            .map(TransactionOverlay::live_tuple_bytes)
            .sum()
    }

    pub(crate) fn transaction_dead_tuple_bytes(&self) -> usize {
        self.transaction_stack
            .iter()
            .map(TransactionOverlay::dead_tuple_bytes)
            .sum()
    }

    pub(crate) fn transaction_index_bytes(&self) -> usize {
        self.transaction_stack
            .iter()
            .map(TransactionOverlay::index_bytes)
            .sum()
    }

    pub(crate) fn scan_bytes(&self) -> usize {
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

    pub(crate) fn owns_inserted_tid(&self, relid: u32, tid: Tid) -> bool {
        overlays_own_inserted_tid(&self.transaction_stack, relid, tid)
    }

    pub(crate) fn transaction_visible_insert_count(&self, relid: u32) -> usize {
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

    pub(crate) fn transaction_invalidated_live_count(&self, relid: u32) -> usize {
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

pub(crate) fn overlays_own_inserted_tid(
    overlays: &[TransactionOverlay],
    relid: u32,
    tid: Tid,
) -> bool {
    overlays.iter().rev().any(|overlay| {
        overlay
            .inserted_tids
            .get(&relid)
            .is_some_and(|tids| tids.contains(&tid))
    })
}

pub(crate) fn overlays_invalidate_tid(
    overlays: &[TransactionOverlay],
    relid: u32,
    tid: Tid,
) -> bool {
    overlays.iter().rev().any(|overlay| {
        overlay
            .invalidated_tids
            .get(&relid)
            .is_some_and(|tids| tids.contains(&tid))
    })
}

pub type SessionStorageHandle = Arc<Mutex<SessionStorage>>;

pub fn new_session_storage() -> SessionStorageHandle {
    Arc::new(Mutex::new(SessionStorage::default()))
}

static DEFAULT_SESSION_STORAGE: OnceLock<SessionStorageHandle> = OnceLock::new();

thread_local! {
    static CURRENT_SESSION_STORAGE: RefCell<Option<SessionStorageHandle>> = const { RefCell::new(None) };
    pub(crate) static LAST_STORAGE_ERROR: RefCell<Option<CatalogError>> = const { RefCell::new(None) };
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

pub(crate) fn default_session_storage() -> SessionStorageHandle {
    DEFAULT_SESSION_STORAGE
        .get_or_init(new_session_storage)
        .clone()
}

pub(crate) fn current_session_storage() -> SessionStorageHandle {
    CURRENT_SESSION_STORAGE
        .with(|slot| slot.borrow().clone())
        .unwrap_or_else(default_session_storage)
}
