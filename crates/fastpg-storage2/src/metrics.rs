#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FastPgStorage2Metrics {
    pub committed_page_bytes: usize,
    pub transaction_page_bytes: usize,
    pub scan_scratch_bytes: usize,
    pub live_tuple_bytes: usize,
    pub dead_tuple_bytes: usize,
    pub index_bytes: usize,
    pub page_count: usize,
    pub arena_rewinds: u64,
    pub arena_drops: u64,
}
