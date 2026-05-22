use crate::*;

#[derive(Clone, Debug, Default)]
pub(crate) struct RelationStorage {
    pub(crate) pages: Vec<Option<Page>>,
    pub(crate) primary_key_index: BTreeMap<IndexKey, Tid>,
    pub(crate) hot_redirects: BTreeMap<Tid, Tid>,
    pub(crate) update_redirects: BTreeMap<Tid, Tid>,
    pub(crate) row_xmins: BTreeMap<Tid, u32>,
    pub(crate) row_cmins: BTreeMap<Tid, u32>,
    pub(crate) row_xmaxs: BTreeMap<Tid, u32>,
    pub(crate) row_delete_xids: BTreeMap<Tid, u32>,
    pub(crate) row_delete_cids: BTreeMap<Tid, u32>,
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
        self.append_hint = checkpoint.append_hint.filter(|block| {
            self.page(*block)
                .is_some_and(|page| self.page_can_accept_tuple(page, 1))
        });
        self.max_tuples_per_block = checkpoint.max_tuples_per_block;
        self.recompute_tuple_stats();
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
        if self.page_can_accept_tuple(&page, 1) {
            self.append_hint = Some(page.block);
        }
        self.pages[block] = Some(page);
    }

    pub(crate) fn mark_page_dead(&mut self, block: u32) {
        if let Some(page) = self.page_mut(block) {
            page.mark_all_dead();
        }
        if self.append_hint == Some(block) {
            self.append_hint = None;
        }
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

        if let Some((block, _)) = self
            .pages
            .iter()
            .enumerate()
            .rev()
            .filter_map(|(block, page)| Some((u32::try_from(block).ok()?, page.as_ref()?)))
            .find(|(_, page)| self.page_can_accept_tuple(page, tuple_len))
        {
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
        self.append_hint = if can_fit_more { Some(block) } else { None };
        Some(tid)
    }

    pub(crate) fn live_tids(&self) -> impl Iterator<Item = Tid> + '_ {
        self.pages
            .iter()
            .filter_map(Option::as_ref)
            .flat_map(Page::live_tids)
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
        self.pages.iter().filter(|page| page.is_some()).count()
    }

    pub(crate) fn block_count(&self) -> usize {
        self.next_block as usize
    }

    pub(crate) fn set_max_tuples_per_block(&mut self, max_tuples: u16) {
        let max_tuples = max_tuples.clamp(1, MAX_CTID_OFFSET as u16);
        self.max_tuples_per_block = Some(max_tuples);
        self.append_hint = self.append_hint.filter(|block| {
            self.page(*block)
                .is_some_and(|page| self.page_can_accept_tuple(page, 1))
        });
    }

    fn page_can_accept_tuple(&self, page: &Page, tuple_len: usize) -> bool {
        page.can_fit(tuple_len)
            && self
                .max_tuples_per_block
                .is_none_or(|max| page.line_pointers.len() < usize::from(max))
    }

    fn recompute_tuple_stats(&mut self) {
        self.live_tuple_count = 0;
        self.pending_tuple_count = 0;
        self.dead_tuple_count = 0;
        self.live_tuple_bytes = 0;
        self.pending_tuple_bytes = 0;
        self.dead_tuple_bytes = 0;

        for page in self.pages.iter().filter_map(Option::as_ref) {
            for line in &page.line_pointers {
                let len = line.len as usize;
                match line.state {
                    LinePointerState::Pending => {
                        self.pending_tuple_count = self.pending_tuple_count.saturating_add(1);
                        self.pending_tuple_bytes = self.pending_tuple_bytes.saturating_add(len);
                    }
                    LinePointerState::Live => {
                        self.live_tuple_count = self.live_tuple_count.saturating_add(1);
                        self.live_tuple_bytes = self.live_tuple_bytes.saturating_add(len);
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
