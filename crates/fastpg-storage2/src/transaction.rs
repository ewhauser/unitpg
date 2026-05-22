use crate::*;

#[derive(Debug, Default)]
pub(crate) struct TransactionOverlay {
    pub(crate) relation_checkpoints: HashMap<u32, RelationCheckpoint>,
    pub(crate) page_checkpoints: HashMap<u32, BTreeMap<u32, PageCheckpoint>>,
    pub(crate) new_pages: HashMap<u32, BTreeSet<u32>>,
    pub(crate) inserted_tids: HashMap<u32, BTreeSet<Tid>>,
    pub(crate) invalidated_tids: HashMap<u32, BTreeSet<Tid>>,
    pub(crate) hot_redirect_inserts: HashMap<u32, BTreeMap<Tid, Tid>>,
    pub(crate) primary_key_inserts: HashMap<u32, BTreeMap<IndexKey, Tid>>,
    pub(crate) primary_key_deletes: HashMap<u32, BTreeSet<IndexKey>>,
}

impl TransactionOverlay {
    pub(crate) fn is_empty(&self) -> bool {
        self.relation_checkpoints.is_empty()
            && self.page_checkpoints.is_empty()
            && self.new_pages.is_empty()
            && self.inserted_tids.is_empty()
            && self.invalidated_tids.is_empty()
            && self.hot_redirect_inserts.is_empty()
            && self.primary_key_inserts.is_empty()
            && self.primary_key_deletes.is_empty()
    }

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

    pub(crate) fn insert_hot_redirect(&mut self, relid: u32, old_tid: Tid, new_tid: Tid) {
        if old_tid != new_tid {
            self.hot_redirect_inserts
                .entry(relid)
                .or_default()
                .insert(old_tid, new_tid);
        }
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

    pub(crate) fn has_visibility_deltas(&self, relid: u32) -> bool {
        self.inserted_tids
            .get(&relid)
            .is_some_and(|tids| !tids.is_empty())
            || self
                .invalidated_tids
                .get(&relid)
                .is_some_and(|tids| !tids.is_empty())
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
        for (relid, redirects) in other.hot_redirect_inserts.drain() {
            self.hot_redirect_inserts
                .entry(relid)
                .or_default()
                .extend(redirects);
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
        let redirects = self
            .hot_redirect_inserts
            .values()
            .map(|entries| {
                entries
                    .len()
                    .saturating_mul(std::mem::size_of::<(Tid, Tid)>())
            })
            .sum::<usize>();
        inserts + deletes + redirects
    }
}

#[derive(Debug, Default)]
pub struct SessionStorage {
    pub(crate) transaction_stack: Vec<TransactionOverlay>,
    pub(crate) explicit_transaction: bool,
    pub(crate) scans: Vec<Option<ScanState>>,
}

impl SessionStorage {
    pub(crate) fn ensure_transaction(&mut self) {
        if self.transaction_stack.is_empty() {
            self.transaction_stack.push(TransactionOverlay::default());
        }
    }

    pub(crate) fn commit_empty_implicit_transaction(&mut self) -> bool {
        if self.explicit_transaction {
            return false;
        }
        if self
            .transaction_stack
            .last()
            .is_some_and(TransactionOverlay::is_empty)
        {
            self.transaction_stack.pop();
            return true;
        }
        false
    }

    pub(crate) fn abort_empty_implicit_transaction(&mut self) -> bool {
        if self.explicit_transaction
            || self
                .transaction_stack
                .iter()
                .any(|overlay| !overlay.is_empty())
        {
            return false;
        }
        self.transaction_stack.clear();
        true
    }

    pub(crate) fn allocate_scan_handle(&mut self) -> u64 {
        if let Some(index) = self.scans.iter().position(Option::is_none) {
            return u64::try_from(index + 1).unwrap_or(u64::MAX);
        }
        self.scans.push(None);
        self.scans.len().try_into().unwrap_or(u64::MAX)
    }

    pub(crate) fn scan_slot(&self, handle: u64) -> Option<&ScanState> {
        let index = usize::try_from(handle.checked_sub(1)?).ok()?;
        self.scans.get(index)?.as_ref()
    }

    pub(crate) fn scan_slot_mut(&mut self, handle: u64) -> Option<&mut ScanState> {
        let index = usize::try_from(handle.checked_sub(1)?).ok()?;
        self.scans.get_mut(index)?.as_mut()
    }

    pub(crate) fn insert_scan(&mut self, handle: u64, scan: ScanState) -> bool {
        let Some(index) = handle
            .checked_sub(1)
            .and_then(|handle| usize::try_from(handle).ok())
        else {
            return false;
        };
        let Some(slot) = self.scans.get_mut(index) else {
            return false;
        };
        *slot = Some(scan);
        true
    }

    pub(crate) fn remove_scan(&mut self, handle: u64) {
        if let Some(index) = handle
            .checked_sub(1)
            .and_then(|handle| usize::try_from(handle).ok())
            && let Some(slot) = self.scans.get_mut(index)
        {
            *slot = None;
        }
    }

    pub(crate) fn mark_scans_visibility_delta(&mut self, relid: u32) {
        for scan in self.scans.iter_mut().filter_map(Option::as_mut) {
            if scan.relid == relid {
                scan.has_visibility_deltas = true;
            }
        }
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
            .iter()
            .filter_map(Option::as_ref)
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

    pub(crate) fn transaction_has_visibility_deltas(&self, relid: u32) -> bool {
        self.transaction_stack
            .iter()
            .any(|overlay| overlay.has_visibility_deltas(relid))
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

pub(crate) fn overlay_tid_redirect(
    overlays: &[TransactionOverlay],
    relid: u32,
    tid: Tid,
) -> Option<Tid> {
    overlays.iter().rev().find_map(|overlay| {
        overlay
            .hot_redirect_inserts
            .get(&relid)
            .and_then(|redirects| redirects.get(&tid))
            .copied()
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
