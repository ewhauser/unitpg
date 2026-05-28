use crate::*;
use smallvec::SmallVec;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct DeleteMetadata {
    pub(crate) xid: u32,
    pub(crate) cid: u32,
}

pub(crate) type TidList = SmallVec<[Tid; 4]>;
pub(crate) type TidCidList = SmallVec<[(Tid, u32); 4]>;
pub(crate) type TidMetadataList = SmallVec<[(Tid, DeleteMetadata); 4]>;
pub(crate) type RedirectList = SmallVec<[(Tid, Tid); 4]>;
pub(crate) type BlockList = SmallVec<[u32; 4]>;
pub(crate) type PageCheckpointList = SmallVec<[(u32, PageCheckpoint); 4]>;
pub(crate) type PendingPageList = SmallVec<[Page; 4]>;
pub(crate) type IndexInsertMap = HashMap<u32, BTreeMap<IndexKey, TidList>>;

#[derive(Debug, Default)]
pub(crate) struct RelidMap<V> {
    entries: SmallVec<[(u32, V); 4]>,
}

impl<V> RelidMap<V> {
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn get(&self, relid: &u32) -> Option<&V> {
        self.entries
            .iter()
            .find(|(entry_relid, _)| entry_relid == relid)
            .map(|(_, value)| value)
    }

    pub(crate) fn get_mut(&mut self, relid: &u32) -> Option<&mut V> {
        self.entries
            .iter_mut()
            .find(|(entry_relid, _)| entry_relid == relid)
            .map(|(_, value)| value)
    }

    pub(crate) fn contains_key(&self, relid: &u32) -> bool {
        self.get(relid).is_some()
    }

    pub(crate) fn insert(&mut self, relid: u32, value: V) -> Option<V> {
        if let Some((_, existing)) = self
            .entries
            .iter_mut()
            .find(|(entry_relid, _)| *entry_relid == relid)
        {
            return Some(std::mem::replace(existing, value));
        }
        self.entries.push((relid, value));
        None
    }

    pub(crate) fn remove(&mut self, relid: &u32) -> Option<V> {
        let index = self
            .entries
            .iter()
            .position(|(entry_relid, _)| entry_relid == relid)?;
        Some(self.entries.swap_remove(index).1)
    }

    pub(crate) fn entry(&mut self, relid: u32) -> RelidEntry<'_, V> {
        if let Some(index) = self
            .entries
            .iter()
            .position(|(entry_relid, _)| *entry_relid == relid)
        {
            RelidEntry::Occupied(&mut self.entries[index].1)
        } else {
            RelidEntry::Vacant { map: self, relid }
        }
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = &V> {
        self.entries.iter().map(|(_, value)| value)
    }

    pub(crate) fn keys(&self) -> impl Iterator<Item = &u32> {
        self.entries.iter().map(|(relid, _)| relid)
    }

    pub(crate) fn drain(&mut self) -> impl Iterator<Item = (u32, V)> + '_ {
        self.entries.drain(..)
    }
}

pub(crate) enum RelidEntry<'a, V> {
    Occupied(&'a mut V),
    Vacant {
        map: &'a mut RelidMap<V>,
        relid: u32,
    },
}

impl<'a, V> RelidEntry<'a, V> {
    pub(crate) fn or_insert(self, value: V) -> &'a mut V {
        match self {
            Self::Occupied(existing) => existing,
            Self::Vacant { map, relid } => {
                map.entries.push((relid, value));
                &mut map
                    .entries
                    .last_mut()
                    .expect("just pushed relid map entry")
                    .1
            }
        }
    }

    #[allow(dead_code)]
    pub(crate) fn or_insert_with(self, value: impl FnOnce() -> V) -> &'a mut V {
        match self {
            Self::Occupied(existing) => existing,
            Self::Vacant { map, relid } => {
                map.entries.push((relid, value()));
                &mut map
                    .entries
                    .last_mut()
                    .expect("just pushed relid map entry")
                    .1
            }
        }
    }
}

impl<'a, V: Default> RelidEntry<'a, V> {
    pub(crate) fn or_default(self) -> &'a mut V {
        self.or_insert(V::default())
    }
}

pub(crate) struct RelidMapIter<'a, V> {
    inner: std::slice::Iter<'a, (u32, V)>,
}

impl<'a, V> Iterator for RelidMapIter<'a, V> {
    type Item = (&'a u32, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(relid, value)| (relid, value))
    }
}

impl<'a, V> IntoIterator for &'a RelidMap<V> {
    type Item = (&'a u32, &'a V);
    type IntoIter = RelidMapIter<'a, V>;

    fn into_iter(self) -> Self::IntoIter {
        RelidMapIter {
            inner: self.entries.iter(),
        }
    }
}

impl<V> IntoIterator for RelidMap<V> {
    type Item = (u32, V);
    type IntoIter = smallvec::IntoIter<[(u32, V); 4]>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

#[derive(Debug, Default)]
pub(crate) struct TransactionOverlay {
    pub(crate) pending_pages: RelidMap<PendingPageList>,
    pub(crate) relation_checkpoints: RelidMap<RelationCheckpoint>,
    pub(crate) page_checkpoints: RelidMap<PageCheckpointList>,
    pub(crate) new_pages: RelidMap<BlockList>,
    pub(crate) inserted_tids: RelidMap<TidList>,
    pub(crate) inserted_cids: RelidMap<TidCidList>,
    pub(crate) invalidated_tids: RelidMap<TidList>,
    pub(crate) invalidated_metadata: RelidMap<TidMetadataList>,
    pub(crate) hot_redirect_inserts: RelidMap<RedirectList>,
    pub(crate) update_redirect_inserts: RelidMap<RedirectList>,
    pub(crate) primary_key_inserts: RelidMap<HashMap<IndexKey, Tid>>,
    pub(crate) primary_key_deletes: RelidMap<HashSet<IndexKey>>,
    pub(crate) index_inserts: RelidMap<IndexInsertMap>,
    pub(crate) cleared_relations: RelidMap<RelationStorage>,
}

fn push_tid_unique(tids: &mut TidList, tid: Tid) {
    if !tids.contains(&tid) {
        tids.push(tid);
    }
}

fn push_block_unique(blocks: &mut BlockList, block: u32) {
    if !blocks.contains(&block) {
        blocks.push(block);
    }
}

fn upsert_page_checkpoint(
    entries: &mut PageCheckpointList,
    block: u32,
    checkpoint: PageCheckpoint,
) {
    if entries.iter().any(|(entry_block, _)| *entry_block == block) {
        return;
    }
    entries.push((block, checkpoint));
}

fn upsert_tid_cid(entries: &mut TidCidList, tid: Tid, cid: u32) {
    if let Some((_, existing)) = entries.iter_mut().find(|(entry_tid, _)| *entry_tid == tid) {
        *existing = cid;
        return;
    }
    entries.push((tid, cid));
}

fn upsert_tid_metadata(entries: &mut TidMetadataList, tid: Tid, metadata: DeleteMetadata) {
    if let Some((_, existing)) = entries.iter_mut().find(|(entry_tid, _)| *entry_tid == tid) {
        *existing = metadata;
        return;
    }
    entries.push((tid, metadata));
}

fn upsert_redirect(entries: &mut RedirectList, old_tid: Tid, new_tid: Tid) {
    if let Some((_, existing)) = entries
        .iter_mut()
        .find(|(entry_tid, _)| *entry_tid == old_tid)
    {
        *existing = new_tid;
        return;
    }
    entries.push((old_tid, new_tid));
}

fn remap_tid(tid: Tid, remaps: &[(Tid, Tid)]) -> Tid {
    remaps
        .iter()
        .find(|(old_tid, _)| *old_tid == tid)
        .map(|(_, new_tid)| *new_tid)
        .unwrap_or(tid)
}

impl TransactionOverlay {
    pub(crate) fn is_empty(&self) -> bool {
        self.pending_pages.is_empty()
            && self.relation_checkpoints.is_empty()
            && self.page_checkpoints.is_empty()
            && self.new_pages.is_empty()
            && self.inserted_tids.is_empty()
            && self.inserted_cids.is_empty()
            && self.invalidated_tids.is_empty()
            && self.invalidated_metadata.is_empty()
            && self.hot_redirect_inserts.is_empty()
            && self.update_redirect_inserts.is_empty()
            && self.primary_key_inserts.is_empty()
            && self.primary_key_deletes.is_empty()
            && self.index_inserts.is_empty()
            && self.cleared_relations.is_empty()
    }

    #[allow(dead_code)]
    pub(crate) fn checkpoint_relation(&mut self, relid: u32, relation: &RelationStorage) {
        self.relation_checkpoints
            .entry(relid)
            .or_insert_with(|| relation.checkpoint());
    }

    #[allow(dead_code)]
    pub(crate) fn checkpoint_page(&mut self, relid: u32, page: &Page) {
        if self
            .new_pages
            .get(&relid)
            .is_some_and(|blocks| blocks.contains(&page.block))
        {
            return;
        }
        upsert_page_checkpoint(
            self.page_checkpoints.entry(relid).or_default(),
            page.block,
            page.checkpoint(),
        );
    }

    #[allow(dead_code)]
    pub(crate) fn record_new_page(&mut self, relid: u32, block: u32) {
        push_block_unique(self.new_pages.entry(relid).or_default(), block);
    }

    pub(crate) fn insert_tid(&mut self, relid: u32, tid: Tid) {
        push_tid_unique(self.inserted_tids.entry(relid).or_default(), tid);
    }

    pub(crate) fn append_pending_tuple_to_existing_page(
        &mut self,
        relid: u32,
        tuple: &[u8],
        max_tuples_per_block: Option<u16>,
    ) -> Option<Tid> {
        let pages = self.pending_pages.get_mut(&relid)?;
        let page = pages.iter_mut().rev().find(|page| {
            page.can_fit(tuple.len())
                && max_tuples_per_block
                    .is_none_or(|max| page.line_pointers.len() < usize::from(max))
        })?;
        page.append_tuple_with_state(tuple, LinePointerState::Pending)
    }

    pub(crate) fn append_pending_tuple_to_new_page(
        &mut self,
        relid: u32,
        mut page: Page,
        tuple: &[u8],
    ) -> Option<Tid> {
        let tid = page.append_tuple_with_state(tuple, LinePointerState::Pending)?;
        self.pending_pages.entry(relid).or_default().push(page);
        Some(tid)
    }

    pub(crate) fn append_pending_input_tuple_to_existing_page(
        &mut self,
        relid: u32,
        input: &RowInput<'_>,
        tuple_len: usize,
        max_tuples_per_block: Option<u16>,
    ) -> Result<Option<Tid>, CatalogError> {
        let Some(pages) = self.pending_pages.get_mut(&relid) else {
            return Ok(None);
        };
        let Some(page) = pages.iter_mut().rev().find(|page| {
            page.can_fit(tuple_len)
                && max_tuples_per_block
                    .is_none_or(|max| page.line_pointers.len() < usize::from(max))
        }) else {
            return Ok(None);
        };
        page.append_input_tuple_with_state(input, tuple_len, LinePointerState::Pending)
    }

    pub(crate) fn append_pending_input_tuple_to_new_page(
        &mut self,
        relid: u32,
        mut page: Page,
        input: &RowInput<'_>,
        tuple_len: usize,
    ) -> Result<Option<Tid>, CatalogError> {
        let tid = page
            .append_input_tuple_with_state(input, tuple_len, LinePointerState::Pending)?
            .ok_or_else(|| storage_limit_error("storage2 could not allocate tuple page"))?;
        self.pending_pages.entry(relid).or_default().push(page);
        Ok(Some(tid))
    }

    fn pending_page(&self, relid: u32, block: u32) -> Option<&Page> {
        self.pending_pages
            .get(&relid)?
            .iter()
            .rev()
            .find(|page| page.block == block)
    }

    fn pending_page_mut(&mut self, relid: u32, block: u32) -> Option<&mut Page> {
        self.pending_pages
            .get_mut(&relid)?
            .iter_mut()
            .rev()
            .find(|page| page.block == block)
    }

    pub(crate) fn pending_tuple_slice(&self, relid: u32, tid: Tid) -> Option<&[u8]> {
        self.pending_page(relid, tid.block)?
            .tuple_slice(tid.offset, true)
    }

    pub(crate) fn pending_line_pointer_state(
        &self,
        relid: u32,
        tid: Tid,
    ) -> Option<LinePointerState> {
        let index = usize::from(tid.offset.checked_sub(1)?);
        Some(
            self.pending_page(relid, tid.block)?
                .line_pointers
                .get(index)?
                .state,
        )
    }

    pub(crate) fn pending_page_needs_stable_tid(&self, relid: u32, block: u32) -> bool {
        self.hot_redirect_inserts
            .get(&relid)
            .into_iter()
            .flat_map(|entries| entries.iter())
            .chain(
                self.update_redirect_inserts
                    .get(&relid)
                    .into_iter()
                    .flat_map(|entries| entries.iter()),
            )
            .any(|(_, new_tid)| new_tid.block == block)
    }

    pub(crate) fn pending_row_xmin(&self, relid: u32, tid: Tid) -> Option<u32> {
        let index = usize::from(tid.offset.checked_sub(1)?);
        Some(
            self.pending_page(relid, tid.block)?
                .line_pointers
                .get(index)?
                .xmin,
        )
    }

    pub(crate) fn pending_row_xmax(&self, relid: u32, tid: Tid) -> Option<u32> {
        let index = usize::from(tid.offset.checked_sub(1)?);
        Some(
            self.pending_page(relid, tid.block)?
                .line_pointers
                .get(index)?
                .xmax,
        )
    }

    pub(crate) fn set_pending_insert_metadata(
        &mut self,
        relid: u32,
        tid: Tid,
        xmin: u32,
        cmin: u32,
    ) -> bool {
        let Some(page) = self.pending_page_mut(relid, tid.block) else {
            return false;
        };
        let Some(index) = tid.offset.checked_sub(1).map(usize::from) else {
            return false;
        };
        let Some(line) = page.line_pointers.get_mut(index) else {
            return false;
        };
        line.xmin = xmin;
        line.cmin = cmin;
        true
    }

    pub(crate) fn set_pending_row_xmax(&mut self, relid: u32, tid: Tid, xmax: u32) -> bool {
        let Some(page) = self.pending_page_mut(relid, tid.block) else {
            return false;
        };
        let Some(index) = tid.offset.checked_sub(1).map(usize::from) else {
            return false;
        };
        let Some(line) = page.line_pointers.get_mut(index) else {
            return false;
        };
        line.xmax = xmax;
        true
    }

    pub(crate) fn extend_high_water_offsets(&self, relid: u32, offsets: &mut HighWaterOffsets) {
        let Some(pages) = self.pending_pages.get(&relid) else {
            return;
        };
        for page in pages {
            let Ok(block) = usize::try_from(page.block) else {
                continue;
            };
            if offsets.len() <= block {
                offsets.resize(block + 1, 0);
            }
            if let Ok(offset) = u16::try_from(page.line_pointers.len()) {
                offsets[block] = offsets[block].max(offset);
            }
        }
    }

    pub(crate) fn set_insert_cid(&mut self, relid: u32, tid: Tid, cid: u32) {
        upsert_tid_cid(self.inserted_cids.entry(relid).or_default(), tid, cid);
    }

    pub(crate) fn invalidate(&mut self, relid: u32, tid: Tid) {
        push_tid_unique(self.invalidated_tids.entry(relid).or_default(), tid);
    }

    pub(crate) fn set_invalidate_metadata(&mut self, relid: u32, tid: Tid, xid: u32, cid: u32) {
        upsert_tid_metadata(
            self.invalidated_metadata.entry(relid).or_default(),
            tid,
            DeleteMetadata { xid, cid },
        );
    }

    pub(crate) fn insert_hot_redirect(&mut self, relid: u32, old_tid: Tid, new_tid: Tid) {
        if old_tid != new_tid {
            upsert_redirect(
                self.hot_redirect_inserts.entry(relid).or_default(),
                old_tid,
                new_tid,
            );
        }
    }

    pub(crate) fn insert_update_redirect(&mut self, relid: u32, old_tid: Tid, new_tid: Tid) {
        if old_tid != new_tid {
            upsert_redirect(
                self.update_redirect_inserts.entry(relid).or_default(),
                old_tid,
                new_tid,
            );
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

    pub(crate) fn insert_index_entry(
        &mut self,
        relid: u32,
        index_relid: u32,
        key: IndexKey,
        tid: Tid,
    ) {
        let tids = self
            .index_inserts
            .entry(relid)
            .or_default()
            .entry(index_relid)
            .or_default()
            .entry(key)
            .or_default();
        push_tid_unique(tids, tid);
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
        self.pending_pages.remove(&relid);
        self.inserted_tids.remove(&relid);
        self.inserted_cids.remove(&relid);
        self.primary_key_inserts.remove(&relid);
        self.index_inserts.remove(&relid);
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

    pub(crate) fn contains_inserted_tid(&self, relid: u32, tid: Tid) -> bool {
        self.inserted_tids
            .get(&relid)
            .is_some_and(|tids| tids.contains(&tid))
    }

    pub(crate) fn inserted_cid(&self, relid: u32, tid: Tid) -> Option<u32> {
        if !self.contains_inserted_tid(relid, tid) {
            return None;
        }
        Some(
            self.inserted_cids
                .get(&relid)
                .and_then(|cids| {
                    cids.iter()
                        .find(|(entry_tid, _)| *entry_tid == tid)
                        .map(|(_, cid)| *cid)
                })
                .unwrap_or_default(),
        )
    }

    pub(crate) fn contains_invalidated_tid(&self, relid: u32, tid: Tid) -> bool {
        self.invalidated_tids
            .get(&relid)
            .is_some_and(|tids| tids.contains(&tid))
    }

    pub(crate) fn invalidated_cid(&self, relid: u32, tid: Tid) -> Option<u32> {
        if !self.contains_invalidated_tid(relid, tid) {
            return None;
        }
        Some(
            self.invalidated_metadata
                .get(&relid)
                .and_then(|entries| {
                    entries
                        .iter()
                        .find(|(entry_tid, _)| *entry_tid == tid)
                        .map(|(_, metadata)| metadata.cid)
                })
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

    pub(crate) fn append_from(&mut self, other: &mut Self) {
        for (relid, mut relation) in other.cleared_relations.drain() {
            self.pending_pages.remove(&relid);
            self.inserted_tids.remove(&relid);
            self.inserted_cids.remove(&relid);
            self.primary_key_inserts.remove(&relid);
            self.index_inserts.remove(&relid);
            if !self.cleared_relations.contains_key(&relid) {
                self.restore_relation_snapshot(relid, &mut relation);
                self.cleared_relations.insert(relid, relation);
            }
        }
        for (relid, pages) in other.pending_pages.drain() {
            self.pending_pages.entry(relid).or_default().extend(pages);
        }
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
                    upsert_page_checkpoint(target, block, checkpoint);
                }
            }
        }
        for (relid, blocks) in other.new_pages.drain() {
            let target = self.new_pages.entry(relid).or_default();
            for block in blocks {
                push_block_unique(target, block);
            }
        }
        for (relid, tids) in other.inserted_tids.drain() {
            let target = self.inserted_tids.entry(relid).or_default();
            for tid in tids {
                push_tid_unique(target, tid);
            }
        }
        for (relid, tids) in other.invalidated_tids.drain() {
            let target = self.invalidated_tids.entry(relid).or_default();
            for tid in tids {
                push_tid_unique(target, tid);
            }
        }
        for (relid, cids) in other.inserted_cids.drain() {
            let target = self.inserted_cids.entry(relid).or_default();
            for (tid, cid) in cids {
                upsert_tid_cid(target, tid, cid);
            }
        }
        for (relid, entries) in other.invalidated_metadata.drain() {
            let target = self.invalidated_metadata.entry(relid).or_default();
            for (tid, metadata) in entries {
                upsert_tid_metadata(target, tid, metadata);
            }
        }
        for (relid, redirects) in other.hot_redirect_inserts.drain() {
            let target = self.hot_redirect_inserts.entry(relid).or_default();
            for (old_tid, new_tid) in redirects {
                upsert_redirect(target, old_tid, new_tid);
            }
        }
        for (relid, redirects) in other.update_redirect_inserts.drain() {
            let target = self.update_redirect_inserts.entry(relid).or_default();
            for (old_tid, new_tid) in redirects {
                upsert_redirect(target, old_tid, new_tid);
            }
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
        for (relid, index_maps) in other.index_inserts.drain() {
            let target = self.index_inserts.entry(relid).or_default();
            for (index_relid, entries) in index_maps {
                let target_entries = target.entry(index_relid).or_default();
                for (key, tids) in entries {
                    let target_tids = target_entries.entry(key).or_default();
                    for tid in tids {
                        push_tid_unique(target_tids, tid);
                    }
                }
            }
        }
    }

    pub(crate) fn remap_tids(&mut self, relid: u32, remaps: &[(Tid, Tid)]) {
        if remaps.is_empty() {
            return;
        }

        if let Some(tids) = self.inserted_tids.get_mut(&relid) {
            for tid in tids {
                *tid = remap_tid(*tid, remaps);
            }
        }
        if let Some(entries) = self.inserted_cids.get_mut(&relid) {
            for (tid, _) in entries {
                *tid = remap_tid(*tid, remaps);
            }
        }
        if let Some(tids) = self.invalidated_tids.get_mut(&relid) {
            for tid in tids {
                *tid = remap_tid(*tid, remaps);
            }
        }
        if let Some(entries) = self.invalidated_metadata.get_mut(&relid) {
            for (tid, _) in entries {
                *tid = remap_tid(*tid, remaps);
            }
        }
        if let Some(entries) = self.hot_redirect_inserts.get_mut(&relid) {
            for (old_tid, new_tid) in entries {
                *old_tid = remap_tid(*old_tid, remaps);
                *new_tid = remap_tid(*new_tid, remaps);
            }
        }
        if let Some(entries) = self.update_redirect_inserts.get_mut(&relid) {
            for (old_tid, new_tid) in entries {
                *old_tid = remap_tid(*old_tid, remaps);
                *new_tid = remap_tid(*new_tid, remaps);
            }
        }
        if let Some(entries) = self.primary_key_inserts.get_mut(&relid) {
            for tid in entries.values_mut() {
                *tid = remap_tid(*tid, remaps);
            }
        }
        if let Some(index_maps) = self.index_inserts.get_mut(&relid) {
            for entries in index_maps.values_mut() {
                for tids in entries.values_mut() {
                    for tid in tids {
                        *tid = remap_tid(*tid, remaps);
                    }
                }
            }
        }
    }

    pub(crate) fn accounted_bytes(&self) -> usize {
        let pending_pages = self
            .pending_pages
            .values()
            .map(|pages| pages.iter().map(|page| page.bytes.len()).sum::<usize>())
            .sum::<usize>();
        let new_relation_pages = (&self.page_checkpoints)
            .into_iter()
            .map(|(relid, checkpoints)| {
                let prior_pages = self
                    .relation_checkpoints
                    .get(relid)
                    .map(|checkpoint| checkpoint.pages_len)
                    .unwrap_or_default();
                checkpoints
                    .iter()
                    .filter(|(block, _)| (*block as usize) >= prior_pages)
                    .count()
                    .saturating_mul(PAGE_SIZE)
            })
            .sum::<usize>();
        pending_pages
            + new_relation_pages
            + self
                .new_pages
                .values()
                .map(|blocks| blocks.len().saturating_mul(PAGE_SIZE))
                .sum::<usize>()
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
        let index_inserts = self
            .index_inserts
            .values()
            .map(|index_maps| {
                index_maps
                    .values()
                    .map(|entries| {
                        entries
                            .iter()
                            .map(|(key, tids)| {
                                key.accounted_bytes()
                                    + tids.len().saturating_mul(std::mem::size_of::<Tid>())
                            })
                            .sum::<usize>()
                    })
                    .sum::<usize>()
            })
            .sum::<usize>();
        inserts + deletes + redirects + update_redirects + index_inserts
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
            return true;
        }
        if self.transaction_stack.is_empty() {
            return true;
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
        if self.explicit_transaction {
            return true;
        }
        if self
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
        let mut tids = SmallVec::<[Tid; 4]>::new();
        for overlay in &self.transaction_stack {
            if let Some(inserted) = overlay.inserted_tids.get(&relid) {
                for tid in inserted {
                    if !tids.contains(tid) {
                        tids.push(*tid);
                    }
                }
            }
            if let Some(invalidated) = overlay.invalidated_tids.get(&relid) {
                for tid in invalidated {
                    if let Some(index) = tids.iter().position(|entry| entry == tid) {
                        tids.swap_remove(index);
                    }
                }
            }
        }
        tids.len()
    }

    pub(crate) fn transaction_invalidated_live_count(&self, relid: u32) -> usize {
        let mut tids = SmallVec::<[Tid; 4]>::new();
        for overlay in &self.transaction_stack {
            if let Some(invalidated) = overlay.invalidated_tids.get(&relid) {
                for tid in invalidated {
                    if !tids.contains(tid) {
                        tids.push(*tid);
                    }
                }
            }
        }
        tids.iter()
            .copied()
            .filter(|tid| !self.owns_inserted_tid(relid, *tid))
            .count()
    }

    pub(crate) fn single_overlay_row_count_delta(&self, relid: u32) -> Option<isize> {
        if self.transaction_stack.len() != 1 {
            return None;
        }

        let overlay = &self.transaction_stack[0];
        let inserted = overlay
            .inserted_tids
            .get(&relid)
            .map(|tids| tids.len())
            .unwrap_or_default();
        let invalidated = overlay
            .invalidated_tids
            .get(&relid)
            .map(|tids| tids.len())
            .unwrap_or_default();

        Some(inserted as isize - invalidated as isize)
    }
}

pub(crate) fn overlays_own_inserted_tid(
    overlays: &[TransactionOverlay],
    relid: u32,
    tid: Tid,
) -> bool {
    for overlay in overlays.iter().rev() {
        if overlay
            .inserted_tids
            .get(&relid)
            .is_some_and(|tids| tids.contains(&tid))
        {
            return true;
        }
        if overlay.cleared_relations.contains_key(&relid) {
            return false;
        }
    }
    false
}

pub(crate) fn overlays_clear_tid(overlays: &[TransactionOverlay], relid: u32, tid: Tid) -> bool {
    for overlay in overlays.iter().rev() {
        if overlay
            .inserted_tids
            .get(&relid)
            .is_some_and(|tids| tids.contains(&tid))
        {
            return false;
        }
        if overlay.cleared_relations.contains_key(&relid) {
            return true;
        }
    }
    false
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
                .and_then(|cids| {
                    cids.iter()
                        .find(|(entry_tid, _)| *entry_tid == tid)
                        .map(|(_, cid)| *cid)
                })
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

pub(crate) fn insert_shadows_invalidation(insert_cid: u32, invalidated_cid: Option<u32>) -> bool {
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
            .and_then(|redirects| {
                redirects
                    .iter()
                    .find(|(entry_tid, _)| *entry_tid == tid)
                    .map(|(_, next_tid)| *next_tid)
            })
    })
}

pub(crate) fn overlay_tid_redirect_target(
    overlays: &[TransactionOverlay],
    relid: u32,
    tid: Tid,
) -> bool {
    overlays.iter().rev().any(|overlay| {
        overlay
            .hot_redirect_inserts
            .get(&relid)
            .is_some_and(|redirects| redirects.iter().any(|(_, next_tid)| *next_tid == tid))
    })
}

pub(crate) fn overlay_tid_redirect_root_for_target(
    overlays: &[TransactionOverlay],
    relid: u32,
    tid: Tid,
) -> Option<Tid> {
    if !overlay_tid_redirect_target(overlays, relid, tid) {
        return None;
    }

    let mut root = tid;
    for _ in 0..1_000_000 {
        let Some(previous) = overlays.iter().rev().find_map(|overlay| {
            overlay
                .hot_redirect_inserts
                .get(&relid)
                .and_then(|redirects| {
                    redirects
                        .iter()
                        .find(|(_, next_tid)| *next_tid == root)
                        .map(|(old_tid, _)| *old_tid)
                })
        }) else {
            break;
        };
        root = previous;
    }

    (root != tid).then_some(root)
}

pub(crate) fn overlay_pending_tuple_slice(
    overlays: &[TransactionOverlay],
    relid: u32,
    tid: Tid,
) -> Option<&[u8]> {
    for overlay in overlays.iter().rev() {
        if let Some(tuple) = overlay.pending_tuple_slice(relid, tid) {
            return Some(tuple);
        }
        if overlay.cleared_relations.contains_key(&relid) {
            return None;
        }
    }
    None
}

pub(crate) fn single_overlay_tid_visibility(
    overlay: &TransactionOverlay,
    relid: u32,
    tid: Tid,
    curcid: Option<u32>,
) -> Option<bool> {
    let relation_cleared = overlay.cleared_relations.contains_key(&relid);
    let inserted = overlay.contains_inserted_tid(relid, tid);
    let invalidated = overlay.contains_invalidated_tid(relid, tid);

    if curcid.is_none() && !relation_cleared {
        if invalidated {
            return None;
        }
        return Some(inserted);
    }

    let inserted_cid = inserted.then(|| overlay.inserted_cid(relid, tid).unwrap_or_default());
    let invalidated_cid =
        invalidated.then(|| overlay.invalidated_cid(relid, tid).unwrap_or_default());

    if let Some(curcid) = curcid {
        if invalidated_cid.is_some_and(|cid| cid < curcid)
            && !(relation_cleared
                && inserted_cid
                    .filter(|cid| *cid < curcid)
                    .is_some_and(|cid| insert_shadows_invalidation(cid, invalidated_cid)))
        {
            return None;
        }

        let include_pending = inserted_cid.is_some_and(|cid| cid < curcid);
        if inserted_cid.is_some() && !include_pending {
            return None;
        }
        if !include_pending && relation_cleared {
            return None;
        }
        return Some(include_pending);
    }

    if invalidated_cid.is_some()
        && !(relation_cleared
            && inserted_cid.is_some_and(|cid| insert_shadows_invalidation(cid, invalidated_cid)))
    {
        return None;
    }
    if inserted_cid.is_none() && relation_cleared {
        return None;
    }
    Some(inserted_cid.is_some())
}

pub(crate) fn single_overlay_visible_pending_tuple_slice(
    overlay: &TransactionOverlay,
    relid: u32,
    tid: Tid,
    curcid: Option<u32>,
) -> Option<&[u8]> {
    if single_overlay_tid_visibility(overlay, relid, tid, curcid)? {
        return overlay.pending_tuple_slice(relid, tid);
    }
    None
}

pub(crate) fn overlay_pending_line_pointer_state(
    overlays: &[TransactionOverlay],
    relid: u32,
    tid: Tid,
) -> Option<LinePointerState> {
    overlays
        .iter()
        .rev()
        .find_map(|overlay| overlay.pending_line_pointer_state(relid, tid))
}

pub(crate) fn overlay_pending_row_xmin(
    overlays: &[TransactionOverlay],
    relid: u32,
    tid: Tid,
) -> Option<u32> {
    overlays
        .iter()
        .rev()
        .find_map(|overlay| overlay.pending_row_xmin(relid, tid))
}

pub(crate) fn overlay_pending_row_xmax(
    overlays: &[TransactionOverlay],
    relid: u32,
    tid: Tid,
) -> Option<u32> {
    overlays
        .iter()
        .rev()
        .find_map(|overlay| overlay.pending_row_xmax(relid, tid))
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
            .and_then(|redirects| {
                redirects
                    .iter()
                    .find(|(entry_tid, _)| *entry_tid == tid)
                    .map(|(_, next_tid)| *next_tid)
            })
    })
}

pub type SessionStorageHandle = Arc<Mutex<SessionStorage>>;

pub fn new_session_storage() -> SessionStorageHandle {
    Arc::new(Mutex::new(SessionStorage::default()))
}

static DEFAULT_SESSION_STORAGE: OnceLock<SessionStorageHandle> = OnceLock::new();

thread_local! {
    static CURRENT_SESSION_STORAGE: RefCell<Option<SessionStorageHandle>> = const { RefCell::new(None) };
    static BORROWED_SESSION_STORAGE: Cell<Option<NonNull<SessionStorage>>> = const { Cell::new(None) };
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

#[derive(Debug)]
pub struct LockedSessionStorageGuard<'a> {
    _borrowed: BorrowedSessionStorageGuard,
    _lock: MutexGuard<'a, SessionStorage>,
}

pub fn enter_locked_session_storage(
    handle: &SessionStorageHandle,
) -> LockedSessionStorageGuard<'_> {
    let mut lock = handle.lock();
    let borrowed = enter_borrowed_session_storage(&mut lock);
    LockedSessionStorageGuard {
        _borrowed: borrowed,
        _lock: lock,
    }
}

#[derive(Debug)]
struct BorrowedSessionStorageGuard {
    previous: Option<NonNull<SessionStorage>>,
}

fn enter_borrowed_session_storage(session: &mut SessionStorage) -> BorrowedSessionStorageGuard {
    let previous = BORROWED_SESSION_STORAGE.with(|slot| {
        let previous = slot.get();
        slot.set(Some(NonNull::from(session)));
        previous
    });
    BorrowedSessionStorageGuard { previous }
}

impl Drop for BorrowedSessionStorageGuard {
    fn drop(&mut self) {
        BORROWED_SESSION_STORAGE.with(|slot| {
            slot.set(self.previous.take());
        });
    }
}

pub(crate) fn with_current_session_storage<R>(f: impl FnOnce(&mut SessionStorage) -> R) -> R {
    if let Some(mut borrowed) = BORROWED_SESSION_STORAGE.with(Cell::get) {
        // SAFETY: enter_locked_session_storage installs this pointer only while
        // holding the owning session mutex and removes it before that lock is
        // released. PgCore execution re-enters storage2 callbacks on the same
        // thread, so each callback observes the active exclusive session borrow.
        let session = unsafe { borrowed.as_mut() };
        return f(session);
    }

    let session = current_session_storage();
    let mut session = session.lock();
    f(&mut session)
}
