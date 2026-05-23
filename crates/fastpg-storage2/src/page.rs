use crate::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LinePointerState {
    Pending,
    Live,
    Dead,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LinePointer {
    pub(crate) offset: u32,
    pub(crate) len: u32,
    pub(crate) state: LinePointerState,
    pub(crate) xmin: u32,
    pub(crate) cmin: u32,
    pub(crate) xmax: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct Page {
    pub(crate) block: u32,
    pub(crate) epoch: u64,
    pub(crate) generation: u64,
    pub(crate) bytes: Box<[u8]>,
    pub(crate) used: usize,
    pub(crate) line_pointers: Vec<LinePointer>,
    pub(crate) pending_tuple_bytes: usize,
    pub(crate) live_tuple_bytes: usize,
    pub(crate) dead_tuple_bytes: usize,
}

impl Page {
    pub(crate) fn new(block: u32, epoch: u64, generation: u64, min_capacity: usize) -> Self {
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

    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.aligned_used())
    }

    pub(crate) fn can_fit(&self, tuple_len: usize) -> bool {
        tuple_len <= self.remaining() && self.line_pointers.len() < MAX_CTID_OFFSET
    }

    fn aligned_used(&self) -> usize {
        self.used.next_multiple_of(DATUM_ALIGNMENT)
    }

    pub(crate) fn append_tuple_with_state(
        &mut self,
        tuple: &[u8],
        state: LinePointerState,
    ) -> Option<Tid> {
        if tuple.len() > self.remaining() || self.line_pointers.len() >= MAX_CTID_OFFSET {
            return None;
        }
        let offset = self.aligned_used();
        let end = offset.checked_add(tuple.len())?;
        if end > self.bytes.len() {
            return None;
        }
        self.bytes[offset..end].copy_from_slice(tuple);
        self.used = end;
        self.line_pointers.push(LinePointer {
            offset: offset.try_into().ok()?,
            len: tuple.len().try_into().ok()?,
            state,
            xmin: 0,
            cmin: 0,
            xmax: 0,
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

    pub(crate) fn tuple_slice(&self, offset: u16, include_pending: bool) -> Option<&[u8]> {
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

    pub(crate) fn tuple_slice_any(&self, offset: u16) -> Option<&[u8]> {
        let index = usize::from(offset.checked_sub(1)?);
        let line = self.line_pointers.get(index)?;
        let start = line.offset as usize;
        let end = start.checked_add(line.len as usize)?;
        self.bytes.get(start..end)
    }

    pub(crate) fn tuple_slice_for_line(
        &self,
        line: LinePointer,
        include_pending: bool,
    ) -> Option<&[u8]> {
        if line.state == LinePointerState::Dead
            || (line.state == LinePointerState::Pending && !include_pending)
        {
            return None;
        }
        let start = line.offset as usize;
        let end = start.checked_add(line.len as usize)?;
        self.bytes.get(start..end)
    }

    pub(crate) fn mark_dead(&mut self, offset: u16) -> bool {
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

    pub(crate) fn mark_live(&mut self, offset: u16) -> bool {
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

    pub(crate) fn checkpoint(&self) -> PageCheckpoint {
        PageCheckpoint {
            used: self.used,
            line_count: self.line_pointers.len(),
            line_states: Vec::new(),
            pending_tuple_bytes: self.pending_tuple_bytes,
            live_tuple_bytes: self.live_tuple_bytes,
            dead_tuple_bytes: self.dead_tuple_bytes,
        }
    }

    pub(crate) fn restore_to_preserving_tid_space(&mut self, checkpoint: &PageCheckpoint) {
        let checkpoint_line_count = checkpoint.line_count.min(self.line_pointers.len());
        if !checkpoint.line_states.is_empty() {
            for (line, state) in self
                .line_pointers
                .iter_mut()
                .take(checkpoint_line_count)
                .zip(checkpoint.line_states.iter().copied())
            {
                line.state = state;
            }
        }
        for line in self.line_pointers.iter_mut().skip(checkpoint_line_count) {
            line.state = LinePointerState::Dead;
        }
        self.recompute_tuple_bytes();
    }

    pub(crate) fn mark_all_dead(&mut self) {
        for line in &mut self.line_pointers {
            line.state = LinePointerState::Dead;
        }
        self.recompute_tuple_bytes();
    }

    fn recompute_tuple_bytes(&mut self) {
        self.pending_tuple_bytes = 0;
        self.live_tuple_bytes = 0;
        self.dead_tuple_bytes = 0;
        for line in &self.line_pointers {
            let len = line.len as usize;
            match line.state {
                LinePointerState::Pending => {
                    self.pending_tuple_bytes = self.pending_tuple_bytes.saturating_add(len)
                }
                LinePointerState::Live => {
                    self.live_tuple_bytes = self.live_tuple_bytes.saturating_add(len)
                }
                LinePointerState::Dead => {
                    self.dead_tuple_bytes = self.dead_tuple_bytes.saturating_add(len)
                }
            }
        }
    }

    pub(crate) fn live_tids(&self) -> impl Iterator<Item = Tid> + '_ {
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

    pub(crate) fn accounted_bytes(&self) -> usize {
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
pub(crate) struct PageCheckpoint {
    pub(crate) used: usize,
    pub(crate) line_count: usize,
    pub(crate) line_states: Vec<LinePointerState>,
    pub(crate) pending_tuple_bytes: usize,
    pub(crate) live_tuple_bytes: usize,
    pub(crate) dead_tuple_bytes: usize,
}
