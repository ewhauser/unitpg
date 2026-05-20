#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Tid {
    pub block: u32,
    pub offset: u16,
}

impl Tid {
    pub(crate) fn pack(self) -> u64 {
        ((self.block as u64) << 16) | u64::from(self.offset)
    }

    pub(crate) fn unpack(value: u64) -> Option<Self> {
        let offset = (value & 0xffff) as u16;
        if offset == 0 {
            return None;
        }
        Some(Self {
            block: (value >> 16) as u32,
            offset,
        })
    }
}
