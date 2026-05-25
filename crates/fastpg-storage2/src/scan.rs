use crate::*;
use smallvec::SmallVec;

pub(crate) type HighWaterOffsets = SmallVec<[u16; 4]>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScanCursor {
    pub(crate) block: u32,
    pub(crate) offset: u16,
}

impl ScanCursor {
    pub(crate) fn forward_start() -> Self {
        Self {
            block: 0,
            offset: 1,
        }
    }

    pub(crate) fn backward_start() -> Self {
        Self {
            block: u32::MAX,
            offset: u16::MAX,
        }
    }

    pub(crate) fn after(tid: Tid) -> Self {
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

    pub(crate) fn before(tid: Tid) -> Self {
        if tid.offset > 1 {
            Self {
                block: tid.block,
                offset: tid.offset - 1,
            }
        } else if tid.block == 0 {
            Self {
                block: 0,
                offset: 0,
            }
        } else {
            Self {
                block: tid.block.saturating_sub(1),
                offset: u16::MAX,
            }
        }
    }

    pub(crate) fn before_cursor(cursor: Self) -> Self {
        if cursor.offset > 1 {
            Self {
                block: cursor.block,
                offset: cursor.offset - 1,
            }
        } else if cursor.block == 0 {
            Self {
                block: 0,
                offset: 0,
            }
        } else {
            Self {
                block: cursor.block - 1,
                offset: u16::MAX,
            }
        }
    }

    pub(crate) fn after_cursor(cursor: Self) -> Self {
        match cursor.offset.checked_add(1) {
            Some(offset) => Self {
                block: cursor.block,
                offset,
            },
            None => Self {
                block: cursor.block.saturating_add(1),
                offset: 1,
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScanState {
    pub(crate) relid: u32,
    pub(crate) high_water_offsets: HighWaterOffsets,
    pub(crate) forward_cursor: ScanCursor,
    pub(crate) backward_cursor: ScanCursor,
    pub(crate) forward_exhausted: bool,
    pub(crate) backward_exhausted: bool,
    pub(crate) has_visibility_deltas: bool,
    pub(crate) overlay_only: bool,
    pub(crate) snapshot_curcid: Option<u32>,
}

pub(crate) fn tid_beyond_high_water(tid: Tid, high_water_offsets: &[u16]) -> bool {
    high_water_offsets
        .get(tid.block as usize)
        .is_none_or(|max_offset| tid.offset > *max_offset)
}

pub(crate) fn scan_backward_end_tid(cursor: ScanCursor, high_water_offsets: &[u16]) -> Option<Tid> {
    if cursor.block == u32::MAX {
        let (block, offset) = high_water_offsets
            .iter()
            .enumerate()
            .rev()
            .find(|(_, offset)| **offset > 0)?;
        return Some(Tid {
            block: block.try_into().ok()?,
            offset: *offset,
        });
    }

    if cursor.offset == 0 || usize::try_from(cursor.block).ok()? >= high_water_offsets.len() {
        return None;
    }

    Some(Tid {
        block: cursor.block,
        offset: cursor.offset,
    })
}
