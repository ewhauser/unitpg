use crate::*;

#[derive(Clone, Debug, Default)]
pub(crate) struct RelationStorage {
    pub(crate) pages: Vec<Option<Page>>,
    pub(crate) physical_blocks: BTreeSet<u32>,
    pub(crate) primary_key_index: HashMap<IndexKey, Tid>,
    pub(crate) indexes: HashMap<u32, BTreeMap<IndexKey, TidList>>,
    pub(crate) hot_redirects: HashMap<Tid, Tid>,
    pub(crate) hot_redirect_roots: BTreeSet<Tid>,
    pub(crate) hot_redirect_targets: HashSet<Tid>,
    pub(crate) hot_redirect_target_roots: HashMap<Tid, Tid>,
    pub(crate) update_redirects: HashMap<Tid, Tid>,
    pub(crate) row_delete_xids: HashMap<Tid, u32>,
    pub(crate) row_delete_cids: HashMap<Tid, u32>,
    pub(crate) live_tids: BTreeSet<Tid>,
    pub(crate) pending_reserved_tids: BTreeSet<Tid>,
    pub(crate) pending_reserved_blocks: BTreeSet<u32>,
    pub(crate) appendable_blocks: BTreeSet<u32>,
    pub(crate) next_block: u32,
    pub(crate) append_hint: Option<u32>,
    pub(crate) live_tuple_count: usize,
    pub(crate) pending_tuple_count: usize,
    pub(crate) dead_tuple_count: usize,
    pub(crate) live_tuple_bytes: usize,
    pub(crate) pending_tuple_bytes: usize,
    pub(crate) dead_tuple_bytes: usize,
    pub(crate) max_tuples_per_block: Option<u16>,
}

impl RelationStorage {
    #[allow(dead_code)]
    pub(crate) fn checkpoint(&self) -> RelationCheckpoint {
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
            max_tuples_per_block: self.max_tuples_per_block,
        }
    }

    pub(crate) fn restore_metadata_preserving_tid_space(&mut self, checkpoint: RelationCheckpoint) {
        self.next_block = self
            .next_block
            .max(checkpoint.next_block)
            .max(u32::try_from(self.pages.len()).unwrap_or(u32::MAX));
        self.max_tuples_per_block = checkpoint.max_tuples_per_block;
        self.recompute_appendable_blocks();
        self.recompute_tuple_stats();
    }

    pub(crate) fn restore_rollback_metadata_preserving_tid_space(
        &mut self,
        checkpoint: RelationCheckpoint,
    ) {
        self.next_block = self
            .next_block
            .max(checkpoint.next_block)
            .max(u32::try_from(self.pages.len()).unwrap_or(u32::MAX));
        if self.max_tuples_per_block != checkpoint.max_tuples_per_block {
            self.max_tuples_per_block = checkpoint.max_tuples_per_block;
            self.recompute_appendable_blocks();
        }
    }

    pub(crate) fn reserve_block(&mut self) -> Option<u32> {
        let block = self.next_block;
        self.next_block = self.next_block.checked_add(1)?;
        Some(block)
    }

    pub(crate) fn insert_page(&mut self, page: Page) {
        let block = page.block as usize;
        if self.pages.len() <= block {
            self.pages.resize_with(block + 1, || None);
        }
        let page_block = page.block;
        self.pages[block] = Some(page);
        self.physical_blocks.insert(page_block);
        self.refresh_appendable_block(page_block);
    }

    pub(crate) fn mark_page_dead(&mut self, block: u32) {
        if let Some(page) = self.page_mut(block) {
            page.mark_all_dead();
        }
        self.refresh_appendable_block(block);
    }

    pub(crate) fn page(&self, block: u32) -> Option<&Page> {
        self.pages.get(block as usize)?.as_ref()
    }

    pub(crate) fn page_mut(&mut self, block: u32) -> Option<&mut Page> {
        self.pages.get_mut(block as usize)?.as_mut()
    }

    pub(crate) fn tuple_slice(&self, tid: Tid, include_pending: bool) -> Option<&[u8]> {
        self.page(tid.block)?
            .tuple_slice(tid.offset, include_pending)
    }

    pub(crate) fn tuple_slice_any(&self, tid: Tid) -> Option<&[u8]> {
        self.page(tid.block)?.tuple_slice_any(tid.offset)
    }

    fn line_pointer(&self, tid: Tid) -> Option<&LinePointer> {
        let index = usize::from(tid.offset.checked_sub(1)?);
        self.page(tid.block)?.line_pointers.get(index)
    }

    pub(crate) fn line_pointer_state(&self, tid: Tid) -> Option<LinePointerState> {
        Some(self.line_pointer(tid)?.state)
    }

    fn line_pointer_mut(&mut self, tid: Tid) -> Option<&mut LinePointer> {
        let index = usize::from(tid.offset.checked_sub(1)?);
        self.page_mut(tid.block)?.line_pointers.get_mut(index)
    }

    pub(crate) fn set_insert_metadata(&mut self, tid: Tid, xmin: u32, cmin: u32) -> bool {
        let Some(line) = self.line_pointer_mut(tid) else {
            return false;
        };
        line.xmin = xmin;
        line.cmin = cmin;
        true
    }

    pub(crate) fn set_row_xmax(&mut self, tid: Tid, xmax: u32) -> bool {
        let Some(line) = self.line_pointer_mut(tid) else {
            return false;
        };
        line.xmax = xmax;
        true
    }

    pub(crate) fn row_xmin(&self, tid: Tid) -> Option<u32> {
        Some(self.line_pointer(tid)?.xmin)
    }

    pub(crate) fn row_cmin(&self, tid: Tid) -> Option<u32> {
        Some(self.line_pointer(tid)?.cmin)
    }

    pub(crate) fn row_xmax(&self, tid: Tid) -> Option<u32> {
        Some(self.line_pointer(tid)?.xmax)
    }

    pub(crate) fn mark_dead(&mut self, tid: Tid) -> bool {
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
                self.live_tids.remove(&tid);
            }
            LinePointerState::Dead => return false,
        }
        self.dead_tuple_count = self.dead_tuple_count.saturating_add(1);
        self.dead_tuple_bytes = self.dead_tuple_bytes.saturating_add(len);
        true
    }

    pub(crate) fn mark_live(&mut self, tid: Tid) -> bool {
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
        self.live_tids.insert(tid);
        true
    }

    pub(crate) fn append_target_block(
        &mut self,
        tuple_len: usize,
        epoch: u64,
        generation: u64,
    ) -> Option<u32> {
        if let Some(block) = self.append_hint
            && self
                .page(block)
                .is_some_and(|page| self.page_can_accept_tuple(page, tuple_len))
        {
            return Some(block);
        }

        if let Some(block) = self.appendable_blocks.iter().rev().copied().find(|block| {
            self.page(*block)
                .is_some_and(|page| self.page_can_accept_tuple(page, tuple_len))
        }) {
            self.append_hint = Some(block);
            return Some(block);
        }

        let block = self.reserve_block()?;
        let page = Page::new(block, epoch, generation, tuple_len);
        self.insert_page(page);
        Some(block)
    }

    pub(crate) fn append_pending_tuple(&mut self, block: u32, tuple: &[u8]) -> Option<Tid> {
        let max_tuples_per_block = self.max_tuples_per_block;
        let (tid, can_fit_more) = {
            let page = self.page_mut(block)?;
            let tid = page.append_tuple_with_state(tuple, LinePointerState::Pending)?;
            let can_fit_more = page.can_fit(1)
                && max_tuples_per_block
                    .is_none_or(|max| page.line_pointers.len() < usize::from(max));
            (tid, can_fit_more)
        };
        self.pending_tuple_count = self.pending_tuple_count.saturating_add(1);
        self.pending_tuple_bytes = self.pending_tuple_bytes.saturating_add(tuple.len());
        if can_fit_more {
            self.appendable_blocks.insert(block);
        } else {
            self.appendable_blocks.remove(&block);
        }
        self.append_hint = self.appendable_blocks.iter().next_back().copied();
        Some(tid)
    }

    #[allow(dead_code)]
    pub(crate) fn append_pending_input_tuple(
        &mut self,
        block: u32,
        input: &RowInput<'_>,
        tuple_len: usize,
    ) -> Result<Option<Tid>, CatalogError> {
        let max_tuples_per_block = self.max_tuples_per_block;
        let (tid, can_fit_more) = {
            let Some(page) = self.page_mut(block) else {
                return Ok(None);
            };
            let Some(tid) =
                page.append_input_tuple_with_state(input, tuple_len, LinePointerState::Pending)?
            else {
                return Ok(None);
            };
            let can_fit_more = page.can_fit(1)
                && max_tuples_per_block
                    .is_none_or(|max| page.line_pointers.len() < usize::from(max));
            (tid, can_fit_more)
        };
        self.pending_tuple_count = self.pending_tuple_count.saturating_add(1);
        self.pending_tuple_bytes = self.pending_tuple_bytes.saturating_add(tuple_len);
        if can_fit_more {
            self.appendable_blocks.insert(block);
        } else {
            self.appendable_blocks.remove(&block);
        }
        self.append_hint = self.appendable_blocks.iter().next_back().copied();
        Ok(Some(tid))
    }

    pub(crate) fn live_tids(&self) -> impl Iterator<Item = Tid> + '_ {
        self.live_tids.iter().copied()
    }

    pub(crate) fn insert_index_entry(&mut self, index_relid: u32, key: IndexKey, tid: Tid) {
        let tids = self
            .indexes
            .entry(index_relid)
            .or_default()
            .entry(key)
            .or_default();
        if !tids.contains(&tid) {
            tids.push(tid);
        }
    }

    pub(crate) fn insert_hot_redirect(&mut self, old_tid: Tid, new_tid: Tid) {
        let root_tid = self
            .hot_redirect_target_roots
            .get(&old_tid)
            .copied()
            .unwrap_or(old_tid);
        if let Some(previous_tid) = self.hot_redirects.insert(old_tid, new_tid)
            && previous_tid != new_tid
            && !self
                .hot_redirects
                .values()
                .any(|target_tid| *target_tid == previous_tid)
        {
            self.hot_redirect_targets.remove(&previous_tid);
            self.hot_redirect_target_roots.remove(&previous_tid);
        }
        self.hot_redirect_roots.insert(root_tid);
        self.hot_redirect_targets.insert(new_tid);
        self.hot_redirect_target_roots.insert(new_tid, root_tid);
    }

    pub(crate) fn is_hot_redirect_root(&self, tid: Tid) -> bool {
        self.hot_redirect_roots.contains(&tid)
    }

    pub(crate) fn is_hot_redirect_target(&self, tid: Tid) -> bool {
        self.hot_redirect_targets.contains(&tid)
    }

    pub(crate) fn hot_redirect_root_for_target(&self, tid: Tid) -> Option<Tid> {
        self.hot_redirect_target_roots.get(&tid).copied()
    }

    pub(crate) fn accounted_bytes(&self) -> usize {
        self.pages
            .iter()
            .filter_map(Option::as_ref)
            .map(Page::accounted_bytes)
            .sum()
    }

    pub(crate) fn live_tuple_bytes(&self) -> usize {
        self.live_tuple_bytes + self.pending_tuple_bytes
    }

    pub(crate) fn dead_tuple_bytes(&self) -> usize {
        self.dead_tuple_bytes
    }

    pub(crate) fn page_count(&self) -> usize {
        self.physical_blocks.len()
    }

    pub(crate) fn block_count(&self) -> usize {
        self.next_block as usize
    }

    pub(crate) fn set_max_tuples_per_block(&mut self, max_tuples: u16) {
        let max_tuples = max_tuples.clamp(1, MAX_CTID_OFFSET as u16);
        self.max_tuples_per_block = Some(max_tuples);
        self.recompute_appendable_blocks();
    }

    fn page_can_accept_tuple(&self, page: &Page, tuple_len: usize) -> bool {
        page.can_fit(tuple_len)
            && self
                .max_tuples_per_block
                .is_none_or(|max| page.line_pointers.len() < usize::from(max))
    }

    fn refresh_appendable_block(&mut self, block: u32) {
        if self
            .page(block)
            .is_some_and(|page| self.page_can_accept_tuple(page, 1))
        {
            self.appendable_blocks.insert(block);
        } else {
            self.appendable_blocks.remove(&block);
        }
        self.append_hint = self.appendable_blocks.iter().next_back().copied();
    }

    fn recompute_appendable_blocks(&mut self) {
        self.appendable_blocks.clear();
        for block in self.physical_blocks.iter().copied() {
            if self
                .page(block)
                .is_some_and(|page| self.page_can_accept_tuple(page, 1))
            {
                self.appendable_blocks.insert(block);
            }
        }
        self.append_hint = self.appendable_blocks.iter().next_back().copied();
    }

    fn recompute_tuple_stats(&mut self) {
        self.live_tuple_count = 0;
        self.pending_tuple_count = 0;
        self.dead_tuple_count = 0;
        self.live_tuple_bytes = 0;
        self.pending_tuple_bytes = 0;
        self.dead_tuple_bytes = 0;
        self.live_tids.clear();
        self.physical_blocks.clear();

        for page in self.pages.iter().filter_map(Option::as_ref) {
            self.physical_blocks.insert(page.block);
            for (index, line) in page.line_pointers.iter().enumerate() {
                let len = line.len as usize;
                match line.state {
                    LinePointerState::Pending => {
                        self.pending_tuple_count = self.pending_tuple_count.saturating_add(1);
                        self.pending_tuple_bytes = self.pending_tuple_bytes.saturating_add(len);
                    }
                    LinePointerState::Live => {
                        self.live_tuple_count = self.live_tuple_count.saturating_add(1);
                        self.live_tuple_bytes = self.live_tuple_bytes.saturating_add(len);
                        if let Ok(offset) = u16::try_from(index + 1) {
                            self.live_tids.insert(Tid {
                                block: page.block,
                                offset,
                            });
                        }
                    }
                    LinePointerState::Dead => {
                        self.dead_tuple_count = self.dead_tuple_count.saturating_add(1);
                        self.dead_tuple_bytes = self.dead_tuple_bytes.saturating_add(len);
                    }
                }
            }
        }
    }

    pub(crate) fn index_bytes(&self) -> usize {
        self.primary_key_index
            .keys()
            .map(|key| key.accounted_bytes() + std::mem::size_of::<Tid>())
            .sum::<usize>()
            + self
                .hot_redirects
                .len()
                .saturating_mul(std::mem::size_of::<(Tid, Tid)>())
            + self
                .hot_redirect_roots
                .len()
                .saturating_mul(std::mem::size_of::<Tid>())
            + self
                .hot_redirect_targets
                .len()
                .saturating_mul(std::mem::size_of::<Tid>())
            + self
                .hot_redirect_target_roots
                .len()
                .saturating_mul(std::mem::size_of::<(Tid, Tid)>())
            + self
                .update_redirects
                .len()
                .saturating_mul(std::mem::size_of::<(Tid, Tid)>())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RelationCheckpoint {
    pub(crate) pages_len: usize,
    pub(crate) next_block: u32,
    pub(crate) append_hint: Option<u32>,
    pub(crate) live_tuple_count: usize,
    pub(crate) pending_tuple_count: usize,
    pub(crate) dead_tuple_count: usize,
    pub(crate) live_tuple_bytes: usize,
    pub(crate) pending_tuple_bytes: usize,
    pub(crate) dead_tuple_bytes: usize,
    pub(crate) max_tuples_per_block: Option<u16>,
}
