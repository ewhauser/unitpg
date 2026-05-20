use crate::*;

#[derive(Debug, Default)]
pub(crate) struct RelationStorage {
    pub(crate) pages: Vec<Option<Page>>,
    pub(crate) primary_key_index: BTreeMap<IndexKey, Tid>,
    pub(crate) next_block: u32,
    pub(crate) append_hint: Option<u32>,
    pub(crate) live_tuple_count: usize,
    pub(crate) pending_tuple_count: usize,
    pub(crate) dead_tuple_count: usize,
    pub(crate) live_tuple_bytes: usize,
    pub(crate) pending_tuple_bytes: usize,
    pub(crate) dead_tuple_bytes: usize,
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
        }
    }

    pub(crate) fn restore_metadata(&mut self, checkpoint: RelationCheckpoint) {
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
        if page.can_fit(1) {
            self.append_hint = Some(page.block);
        }
        self.pages[block] = Some(page);
    }

    pub(crate) fn remove_page(&mut self, block: u32) {
        if let Some(slot) = self.pages.get_mut(block as usize) {
            *slot = None;
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

    pub(crate) fn append_pending_tuple(&mut self, block: u32, tuple: &[u8]) -> Option<Tid> {
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

    pub(crate) fn index_bytes(&self) -> usize {
        self.primary_key_index
            .keys()
            .map(|key| key.accounted_bytes() + std::mem::size_of::<Tid>())
            .sum()
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
}
