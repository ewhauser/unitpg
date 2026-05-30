#![deny(unsafe_op_in_unsafe_fn)]
#![allow(ambiguous_glob_reexports)]

pub use fastpg_storage::*;
pub use fastpg_storage2::*;

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_pgcore_suspend_lane_for_wait() -> bool {
    false
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_pgcore_resume_lane_after_wait(_suspended: bool) {}
