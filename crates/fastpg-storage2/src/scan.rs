use crate::*;

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
                block: tid.block - 1,
                offset: u16::MAX,
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScanState {
    pub(crate) relid: u32,
    pub(crate) high_water_offsets: Vec<u16>,
    pub(crate) forward_cursor: ScanCursor,
    pub(crate) backward_cursor: ScanCursor,
    pub(crate) has_visibility_deltas: bool,
}
