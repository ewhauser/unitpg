use crate::*;

#[derive(Debug, Default)]
pub(crate) struct TransactionOverlay {
    pub(crate) relation_checkpoints: HashMap<u32, RelationCheckpoint>,
    pub(crate) page_checkpoints: HashMap<u32, BTreeMap<u32, PageCheckpoint>>,
    pub(crate) new_pages: HashMap<u32, BTreeSet<u32>>,
    pub(crate) inserted_tids: HashMap<u32, BTreeSet<Tid>>,
    pub(crate) inserted_xids: HashMap<u32, BTreeMap<Tid, u32>>,
    pub(crate) inserted_cids: HashMap<u32, BTreeMap<Tid, u32>>,
    pub(crate) invalidated_tids: HashMap<u32, BTreeSet<Tid>>,
    pub(crate) invalidated_xids: HashMap<u32, BTreeMap<Tid, u32>>,
    pub(crate) invalidated_cids: HashMap<u32, BTreeMap<Tid, u32>>,
    pub(crate) row_xmaxs: HashMap<u32, BTreeMap<Tid, u32>>,
    pub(crate) hot_redirect_inserts: HashMap<u32, BTreeMap<Tid, Tid>>,
    pub(crate) update_redirect_inserts: HashMap<u32, BTreeMap<Tid, Tid>>,
    pub(crate) primary_key_inserts: HashMap<u32, BTreeMap<IndexKey, Tid>>,
    pub(crate) primary_key_deletes: HashMap<u32, BTreeSet<IndexKey>>,
    pub(crate) cleared_relations: HashMap<u32, RelationStorage>,
}

impl TransactionOverlay {
    pub(crate) fn is_empty(&self) -> bool {
        self.relation_checkpoints.is_empty()
            && self.page_checkpoints.is_empty()
            && self.new_pages.is_empty()
            && self.inserted_tids.is_empty()
            && self.inserted_xids.is_empty()
            && self.inserted_cids.is_empty()
            && self.invalidated_tids.is_empty()
            && self.invalidated_xids.is_empty()
            && self.invalidated_cids.is_empty()
            && self.row_xmaxs.is_empty()
            && self.hot_redirect_inserts.is_empty()
            && self.update_redirect_inserts.is_empty()
            && self.primary_key_inserts.is_empty()
            && self.primary_key_deletes.is_empty()
            && self.cleared_relations.is_empty()
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

    pub(crate) fn set_insert_xid(&mut self, relid: u32, tid: Tid, xid: u32) {
        self.inserted_xids
            .entry(relid)
            .or_default()
            .insert(tid, xid);
    }

    pub(crate) fn set_insert_cid(&mut self, relid: u32, tid: Tid, cid: u32) {
        self.inserted_cids
            .entry(relid)
            .or_default()
            .insert(tid, cid);
    }

    pub(crate) fn invalidate(&mut self, relid: u32, tid: Tid) {
        self.invalidated_tids.entry(relid).or_default().insert(tid);
    }

    pub(crate) fn set_invalidate_xid(&mut self, relid: u32, tid: Tid, xid: u32) {
        self.invalidated_xids
            .entry(relid)
            .or_default()
            .insert(tid, xid);
    }

    pub(crate) fn set_invalidate_cid(&mut self, relid: u32, tid: Tid, cid: u32) {
        self.invalidated_cids
            .entry(relid)
            .or_default()
            .insert(tid, cid);
    }

    pub(crate) fn set_row_xmax(&mut self, relid: u32, tid: Tid, xmax: u32) {
        self.row_xmaxs.entry(relid).or_default().insert(tid, xmax);
    }

    pub(crate) fn insert_hot_redirect(&mut self, relid: u32, old_tid: Tid, new_tid: Tid) {
        if old_tid != new_tid {
            self.hot_redirect_inserts
                .entry(relid)
                .or_default()
                .insert(old_tid, new_tid);
        }
    }

    pub(crate) fn insert_update_redirect(&mut self, relid: u32, old_tid: Tid, new_tid: Tid) {
        if old_tid != new_tid {
            self.update_redirect_inserts
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

    pub(crate) fn remove_primary_key_insert(&mut self, relid: u32, key: &IndexKey) {
        if let Some(entries) = self.primary_key_inserts.get_mut(&relid) {
            entries.remove(key);
            if entries.is_empty() {
                self.primary_key_inserts.remove(&relid);
            }
        }
    }

    pub(crate) fn record_relation_clear(&mut self, relid: u32, mut relation: RelationStorage) {
        if self.cleared_relations.contains_key(&relid) {
            return;
        }
        self.restore_relation_snapshot(relid, &mut relation);
        self.cleared_relations.insert(relid, relation);
    }

    pub(crate) fn clear_insert_shadows_invalidation(&self, relid: u32, tid: Tid) -> bool {
        self.cleared_relations.contains_key(&relid)
            && self.inserted_cid(relid, tid).is_some_and(|insert_cid| {
                insert_shadows_invalidation(insert_cid, self.invalidated_cid(relid, tid))
            })
    }

    pub(crate) fn inserted_cid(&self, relid: u32, tid: Tid) -> Option<u32> {
        if !self
            .inserted_tids
            .get(&relid)
            .is_some_and(|tids| tids.contains(&tid))
        {
            return None;
        }
        Some(
            self.inserted_cids
                .get(&relid)
                .and_then(|cids| cids.get(&tid))
                .copied()
                .unwrap_or_default(),
        )
    }

    pub(crate) fn invalidated_cid(&self, relid: u32, tid: Tid) -> Option<u32> {
        if !self
            .invalidated_tids
            .get(&relid)
            .is_some_and(|tids| tids.contains(&tid))
        {
            return None;
        }
        Some(
            self.invalidated_cids
                .get(&relid)
                .and_then(|cids| cids.get(&tid))
                .copied()
                .unwrap_or_default(),
        )
    }

    pub(crate) fn restore_relation_snapshot(&self, relid: u32, relation: &mut RelationStorage) {
        if let Some(blocks) = self.new_pages.get(&relid) {
            for block in blocks {
                relation.mark_page_dead(*block);
            }
        }
        if let Some(checkpoints) = self.page_checkpoints.get(&relid) {
            for (block, checkpoint) in checkpoints {
                if let Some(page) = relation.page_mut(*block) {
                    page.restore_to_preserving_tid_space(checkpoint);
                }
            }
        }
        if let Some(checkpoint) = self.relation_checkpoints.get(&relid) {
            relation.restore_metadata_preserving_tid_space(*checkpoint);
        }
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

    pub(crate) fn visibility_delta_relids(&self) -> BTreeSet<u32> {
        self.inserted_tids
            .keys()
            .chain(self.invalidated_tids.keys())
            .copied()
            .collect()
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
        for (relid, xids) in other.inserted_xids.drain() {
            self.inserted_xids.entry(relid).or_default().extend(xids);
        }
        for (relid, cids) in other.inserted_cids.drain() {
            self.inserted_cids.entry(relid).or_default().extend(cids);
        }
        for (relid, tids) in other.invalidated_tids.drain() {
            self.invalidated_tids.entry(relid).or_default().extend(tids);
        }
        for (relid, xids) in other.invalidated_xids.drain() {
            self.invalidated_xids.entry(relid).or_default().extend(xids);
        }
        for (relid, cids) in other.invalidated_cids.drain() {
            self.invalidated_cids.entry(relid).or_default().extend(cids);
        }
        for (relid, xmaxs) in other.row_xmaxs.drain() {
            self.row_xmaxs.entry(relid).or_default().extend(xmaxs);
        }
        for (relid, redirects) in other.hot_redirect_inserts.drain() {
            self.hot_redirect_inserts
                .entry(relid)
                .or_default()
                .extend(redirects);
        }
        for (relid, redirects) in other.update_redirect_inserts.drain() {
            self.update_redirect_inserts
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
        for (relid, mut relation) in other.cleared_relations.drain() {
            if !self.cleared_relations.contains_key(&relid) {
                self.restore_relation_snapshot(relid, &mut relation);
                self.cleared_relations.insert(relid, relation);
            }
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
        let update_redirects = self
            .update_redirect_inserts
            .values()
            .map(|entries| {
                entries
                    .len()
                    .saturating_mul(std::mem::size_of::<(Tid, Tid)>())
            })
            .sum::<usize>();
        inserts + deletes + redirects + update_redirects
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

pub(crate) fn overlay_inserted_cid(
    overlays: &[TransactionOverlay],
    relid: u32,
    tid: Tid,
) -> Option<u32> {
    overlays.iter().rev().find_map(|overlay| {
        if !overlay
            .inserted_tids
            .get(&relid)
            .is_some_and(|tids| tids.contains(&tid))
        {
            return None;
        }
        Some(
            overlay
                .inserted_cids
                .get(&relid)
                .and_then(|cids| cids.get(&tid))
                .copied()
                .unwrap_or_default(),
        )
    })
}

pub(crate) fn overlays_own_inserted_tid_before(
    overlays: &[TransactionOverlay],
    relid: u32,
    tid: Tid,
    curcid: u32,
) -> bool {
    overlay_inserted_cid(overlays, relid, tid).is_some_and(|cid| cid < curcid)
}

pub(crate) fn overlays_invalidate_tid(
    overlays: &[TransactionOverlay],
    relid: u32,
    tid: Tid,
) -> bool {
    let mut later_insert_cid = None;
    for overlay in overlays.iter().rev() {
        if let Some(insert_cid) = overlay.inserted_cid(relid, tid) {
            later_insert_cid = Some(insert_cid);
        }
        if let Some(invalidated_cid) = overlay.invalidated_cid(relid, tid) {
            return !(overlay.cleared_relations.contains_key(&relid)
                && later_insert_cid.is_some_and(|insert_cid| {
                    insert_shadows_invalidation(insert_cid, Some(invalidated_cid))
                }));
        }
    }
    false
}

pub(crate) fn overlays_invalidate_tid_before(
    overlays: &[TransactionOverlay],
    relid: u32,
    tid: Tid,
    curcid: u32,
) -> bool {
    let mut later_insert_cid = None;
    for overlay in overlays.iter().rev() {
        if let Some(insert_cid) = overlay.inserted_cid(relid, tid)
            && insert_cid < curcid
        {
            later_insert_cid = Some(insert_cid);
        }
        if let Some(invalidated_cid) = overlay.invalidated_cid(relid, tid)
            && invalidated_cid < curcid
        {
            return !(overlay.cleared_relations.contains_key(&relid)
                && later_insert_cid.is_some_and(|insert_cid| {
                    insert_shadows_invalidation(insert_cid, Some(invalidated_cid))
                }));
        }
    }
    false
}

fn insert_shadows_invalidation(insert_cid: u32, invalidated_cid: Option<u32>) -> bool {
    let invalidated_cid = invalidated_cid.unwrap_or_default();
    insert_cid > invalidated_cid || (insert_cid == 0 && invalidated_cid == 0)
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

pub(crate) fn overlay_update_redirect(
    overlays: &[TransactionOverlay],
    relid: u32,
    tid: Tid,
) -> Option<Tid> {
    overlays.iter().rev().find_map(|overlay| {
        overlay
            .update_redirect_inserts
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
