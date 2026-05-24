#![deny(unsafe_op_in_unsafe_fn)]

use std::cell::{Cell, RefCell};
use std::ffi::c_char;
use std::hash::{BuildHasherDefault, Hasher};
use std::ptr::NonNull;
use std::slice;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::{collections::BTreeMap, collections::BTreeSet};

use parking_lot::{Mutex, MutexGuard, RwLock};

use fastpg_catalog::{
    BPCHAR_OID, CatalogError, INT2_OID, INT4_OID, INT8_OID, IndexRecord, OID_OID, TEXT_OID,
    TIMESTAMP_OID, VARCHAR_OID, current_generation, has_uncommitted_catalog_changes, lookup_type,
    primary_key_index_oid_for_relation_oid, relation_by_name, relation_column_by_attnum,
    relation_oid_for_index_oid, unique_index_records_for_relation_oid,
};
use fastpg_types::Oid;

pub(crate) const PAGE_SIZE: usize = 8192;
pub(crate) const MAX_CTID_OFFSET: usize = 2047;
pub(crate) const TUPLE_MAGIC: &[u8; 4] = b"FP2T";
pub(crate) const TUPLE_HEADER_LEN: usize = 16;
pub(crate) const ATTR_ENTRY_LEN: usize = 24;
pub(crate) const SQLSTATE_PROGRAM_LIMIT_EXCEEDED: &str = "54000";

pub(crate) type HashMap<K, V> =
    std::collections::HashMap<K, V, BuildHasherDefault<FastPgStorageHasher>>;
pub(crate) type HashSet<T> = std::collections::HashSet<T, BuildHasherDefault<FastPgStorageHasher>>;

#[derive(Default)]
pub(crate) struct FastPgStorageHasher {
    state: u64,
}

impl FastPgStorageHasher {
    fn mix(&mut self, mut value: u64) {
        value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^= value >> 31;
        self.state ^= value;
        self.state = self
            .state
            .rotate_left(27)
            .wrapping_mul(0x3c79_ac49_2ba7_b653)
            .wrapping_add(0x1c69_b3f7_4ac4_ae35);
    }
}

impl Hasher for FastPgStorageHasher {
    fn finish(&self) -> u64 {
        self.state
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            self.mix(u64::from_ne_bytes(chunk.try_into().expect("u64 chunk")));
        }
        let mut tail = [0u8; 8];
        let remainder = chunks.remainder();
        tail[..remainder.len()].copy_from_slice(remainder);
        self.mix(u64::from_ne_bytes(tail) ^ ((bytes.len() as u64) << 56));
    }

    fn write_u8(&mut self, i: u8) {
        self.mix(u64::from(i));
    }

    fn write_u16(&mut self, i: u16) {
        self.mix(u64::from(i));
    }

    fn write_u32(&mut self, i: u32) {
        self.mix(u64::from(i));
    }

    fn write_u64(&mut self, i: u64) {
        self.mix(i);
    }

    fn write_usize(&mut self, i: usize) {
        self.mix(i as u64);
    }

    fn write_i8(&mut self, i: i8) {
        self.write_u8(i as u8);
    }

    fn write_i16(&mut self, i: i16) {
        self.write_u16(i as u16);
    }

    fn write_i32(&mut self, i: i32) {
        self.write_u32(i as u32);
    }

    fn write_i64(&mut self, i: i64) {
        self.write_u64(i as u64);
    }

    fn write_isize(&mut self, i: isize) {
        self.write_usize(i as usize);
    }
}

pub(crate) static STORAGE2_ARENA_REWINDS: AtomicU64 = AtomicU64::new(0);
pub(crate) static STORAGE2_ARENA_DROPS: AtomicU64 = AtomicU64::new(0);
pub(crate) static STORAGE2_METADATA_CACHE: OnceLock<Mutex<Storage2MetadataCache>> = OnceLock::new();
pub(crate) static STORAGE2_ROW_COUNTS: OnceLock<Mutex<HashMap<u32, Arc<AtomicUsize>>>> =
    OnceLock::new();

mod copy;
mod error;
mod index;
mod metrics;
mod page;
mod relation;
mod scan;
mod state;
mod tid;
mod transaction;
mod tuple;

#[cfg(test)]
mod tests;

pub use copy::{CopyDatum, copy_text_line, insert_copy_datums};
pub use metrics::FastPgStorage2Metrics;
pub use tid::Tid;
pub use transaction::{
    LockedSessionStorageGuard, SessionStorageGuard, SessionStorageHandle,
    enter_locked_session_storage, enter_session_storage, new_session_storage,
};

pub(crate) use error::*;
pub(crate) use index::*;
pub(crate) use page::*;
pub(crate) use relation::*;
pub(crate) use scan::*;
pub(crate) use state::*;
pub(crate) use transaction::*;
pub(crate) use tuple::*;

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_xact_begin() {
    if with_session_storage(|session| {
        if session.explicit_transaction {
            session.ensure_transaction();
            return true;
        }
        if session.commit_empty_implicit_transaction() {
            session.ensure_transaction();
            session.explicit_transaction = true;
            return true;
        }
        false
    }) {
        return;
    }
    with_storage(|state, session| state.begin_explicit_transaction(session));
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_xact_begin_implicit() {
    with_session_storage(SessionStorage::ensure_transaction);
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_xact_commit() {
    with_storage(|state, session| state.commit_explicit_transaction(session));
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_xact_abort() {
    with_storage(|state, session| state.abort_explicit_transaction(session));
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_xact_commit_if_implicit() {
    if with_session_storage(SessionStorage::commit_empty_implicit_transaction) {
        return;
    }
    with_storage(|state, session| state.commit_implicit_transaction(session));
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_xact_abort_if_implicit() {
    if with_session_storage(SessionStorage::abort_empty_implicit_transaction) {
        return;
    }
    with_storage(|state, session| state.abort_implicit_transaction(session));
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_subxact_begin() {
    with_session_storage(|session| {
        session.ensure_transaction();
        session
            .transaction_stack
            .push(TransactionOverlay::default());
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_subxact_commit() {
    with_storage(|state, session| {
        if session.transaction_stack.len() > 1 {
            state.commit_top_overlay(session);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_subxact_abort() {
    with_storage(|state, session| {
        if session.transaction_stack.len() > 1
            && let Some(overlay) = session.transaction_stack.pop()
        {
            state.rollback_overlay_from_relations(overlay);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_relation_clear(relid: u32) {
    with_storage(|state, session| state.clear_relation(session, relid));
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_relation_replace_from(dst_relid: u32, src_relid: u32) -> bool {
    clear_last_storage_error();
    let result =
        with_storage(|state, session| state.replace_relation_from(session, dst_relid, src_relid));
    match result {
        Ok(()) => true,
        Err(error) => {
            set_last_storage_error(error);
            false
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_relation_row_count(relid: u32) -> usize {
    visible_row_count_cached(relid)
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_relation_page_count(relid: u32) -> usize {
    with_storage_read(|state, _session| {
        state
            .relations
            .get(&relid)
            .map(RelationStorage::page_count)
            .unwrap_or_default()
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_relation_block_count(relid: u32) -> usize {
    with_storage_read(|state, _session| {
        state
            .relations
            .get(&relid)
            .map(RelationStorage::block_count)
            .unwrap_or_default()
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_relation_set_max_tuples_per_block(relid: u32, max_tuples: u16) {
    with_storage(|state, _session| {
        state
            .relation_mut(relid)
            .set_max_tuples_per_block(max_tuples);
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_relation_block_max_offset(relid: u32, block: u32) -> u16 {
    with_storage_read(|state, _session| {
        state
            .relations
            .get(&relid)
            .and_then(|relation| relation.page(block))
            .and_then(|page| u16::try_from(page.line_pointers.len()).ok())
            .unwrap_or_default()
    })
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass a valid output pointer when non-null.
pub unsafe extern "C" fn fastpg_storage2_relation_visible_tid_at(
    relid: u32,
    zero_based_index: usize,
    tid_out: *mut u64,
) -> bool {
    let tid = with_storage_read(|state, session| {
        state
            .visible_tids(session, relid)
            .get(zero_based_index)
            .copied()
    });
    let Some(tid) = tid else {
        return false;
    };
    if !tid_out.is_null() {
        unsafe {
            *tid_out = tid.pack();
        }
    }
    true
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_relation_contains_tid(relid: u32, packed_tid: u64) -> bool {
    let Some(tid) = Tid::unpack(packed_tid) else {
        return false;
    };
    with_storage_read(|state, session| state.find_visible_tuple(session, relid, tid).is_some())
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass a valid output pointer when non-null.
pub unsafe extern "C" fn fastpg_storage2_relation_resolve_tid(
    relid: u32,
    packed_tid: u64,
    resolved_tid_out: *mut u64,
) -> bool {
    let Some(tid) = Tid::unpack(packed_tid) else {
        return false;
    };
    let resolved = with_storage(|state, session| {
        state.resolve_tid_redirect_in_overlays_compress(&session.transaction_stack, relid, tid)
    });
    if !resolved_tid_out.is_null() {
        unsafe {
            *resolved_tid_out = resolved.pack();
        }
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass a valid output pointer when non-null.
pub unsafe extern "C" fn fastpg_storage2_relation_resolve_update_tid(
    relid: u32,
    packed_tid: u64,
    resolved_tid_out: *mut u64,
) -> bool {
    let Some(tid) = Tid::unpack(packed_tid) else {
        return false;
    };
    let resolved = with_storage(|state, session| {
        state.resolve_update_redirect_in_overlays_compress(&session.transaction_stack, relid, tid)
    });
    if !resolved_tid_out.is_null() {
        unsafe {
            *resolved_tid_out = resolved.pack();
        }
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass a valid output pointer when non-null.
pub unsafe extern "C" fn fastpg_storage2_relation_resolve_update_tid_read(
    relid: u32,
    packed_tid: u64,
    resolved_tid_out: *mut u64,
) -> bool {
    let Some(tid) = Tid::unpack(packed_tid) else {
        return false;
    };
    let resolved = with_storage_read(|state, session| {
        state.resolve_update_redirect_in_overlays(&session.transaction_stack, relid, tid)
    });
    if !resolved_tid_out.is_null() {
        unsafe {
            *resolved_tid_out = resolved.pack();
        }
    }
    true
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_relation_record_insert_metadata(
    relid: u32,
    packed_tid: u64,
    xid: u32,
    cid: u32,
) -> bool {
    let Some(tid) = Tid::unpack(packed_tid) else {
        return false;
    };
    with_storage(|state, session| {
        state.record_insert_metadata(session, relid, tid, xid, cid);
        true
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_relation_record_invalidate_metadata(
    relid: u32,
    packed_tid: u64,
    xid: u32,
    cid: u32,
) -> bool {
    let Some(tid) = Tid::unpack(packed_tid) else {
        return false;
    };
    with_storage(|state, session| {
        state.record_invalidate_metadata(session, relid, tid, xid, cid);
        true
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_relation_record_row_xmax(
    relid: u32,
    packed_tid: u64,
    xmax: u32,
) -> bool {
    let Some(tid) = Tid::unpack(packed_tid) else {
        return false;
    };
    with_storage(|state, session| {
        state.record_row_xmax(session, relid, tid, xmax);
        true
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_relation_row_xmin(relid: u32, packed_tid: u64) -> u32 {
    let Some(tid) = Tid::unpack(packed_tid) else {
        return 0;
    };
    with_storage_read(|state, session| state.row_xmin(session, relid, tid))
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_relation_row_cmin(relid: u32, packed_tid: u64) -> u32 {
    let Some(tid) = Tid::unpack(packed_tid) else {
        return 0;
    };
    with_storage_read(|state, session| state.row_cmin(session, relid, tid))
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_relation_row_xmax(relid: u32, packed_tid: u64) -> u32 {
    let Some(tid) = Tid::unpack(packed_tid) else {
        return 0;
    };
    with_storage_read(|state, session| state.row_xmax(session, relid, tid))
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_relation_row_delete_xid(relid: u32, packed_tid: u64) -> u32 {
    let Some(tid) = Tid::unpack(packed_tid) else {
        return 0;
    };
    with_storage_read(|state, session| state.row_delete_xid(session, relid, tid))
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_relation_row_delete_cid(relid: u32, packed_tid: u64) -> u32 {
    let Some(tid) = Tid::unpack(packed_tid) else {
        return 0;
    };
    with_storage_read(|state, session| state.row_delete_cid(session, relid, tid))
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid row input arrays and an optional valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_relation_insert(
    relid: u32,
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
    tid_out: *mut u64,
) -> bool {
    clear_last_storage_error();
    let Some(input) = input_arrays(values, is_null, byval, value_lens, natts) else {
        set_last_storage_error(invalid_ffi_argument("invalid row input arrays"));
        return false;
    };
    relation_insert_impl(relid, input, tid_out, UniqueCheck::Enforce, None, true)
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid row input arrays and an optional valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_relation_insert_unchecked(
    relid: u32,
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
    tid_out: *mut u64,
) -> bool {
    clear_last_storage_error();
    let Some(input) = input_arrays(values, is_null, byval, value_lens, natts) else {
        set_last_storage_error(invalid_ffi_argument("invalid row input arrays"));
        return false;
    };
    relation_insert_impl(relid, input, tid_out, UniqueCheck::Skip, None, true)
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid row input arrays and an optional valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_relation_insert_unchecked_with_metadata(
    relid: u32,
    xid: u32,
    cid: u32,
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
    tid_out: *mut u64,
) -> bool {
    clear_last_storage_error();
    let Some(input) = input_arrays(values, is_null, byval, value_lens, natts) else {
        set_last_storage_error(invalid_ffi_argument("invalid row input arrays"));
        return false;
    };
    relation_insert_impl(
        relid,
        input,
        tid_out,
        UniqueCheck::Skip,
        Some(InsertMetadata { xid, cid }),
        true,
    )
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid row input arrays and an optional valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_relation_insert_unchecked_no_index_with_metadata(
    relid: u32,
    xid: u32,
    cid: u32,
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
    tid_out: *mut u64,
) -> bool {
    clear_last_storage_error();
    let Some(input) = input_arrays(values, is_null, byval, value_lens, natts) else {
        set_last_storage_error(invalid_ffi_argument("invalid row input arrays"));
        return false;
    };
    relation_insert_impl(
        relid,
        input,
        tid_out,
        UniqueCheck::Skip,
        Some(InsertMetadata { xid, cid }),
        false,
    )
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid row input arrays and an optional valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_relation_update(
    relid: u32,
    packed_tid: u64,
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
    new_tid_out: *mut u64,
) -> bool {
    clear_last_storage_error();
    let Some(input) = input_arrays(values, is_null, byval, value_lens, natts) else {
        set_last_storage_error(invalid_ffi_argument("invalid row input arrays"));
        return false;
    };
    relation_update_impl(
        relid,
        packed_tid,
        input,
        new_tid_out,
        UniqueCheck::Enforce,
        false,
        None,
    )
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid row input arrays and an optional valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_relation_update_unchecked(
    relid: u32,
    packed_tid: u64,
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
    new_tid_out: *mut u64,
) -> bool {
    clear_last_storage_error();
    let Some(input) = input_arrays(values, is_null, byval, value_lens, natts) else {
        set_last_storage_error(invalid_ffi_argument("invalid row input arrays"));
        return false;
    };
    relation_update_impl(
        relid,
        packed_tid,
        input,
        new_tid_out,
        UniqueCheck::Skip,
        false,
        None,
    )
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid row input arrays and an optional valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_relation_update_unchecked_with_metadata(
    relid: u32,
    packed_tid: u64,
    delete_xid: u32,
    delete_cid: u32,
    insert_xid: u32,
    insert_cid: u32,
    row_xmax: u32,
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
    new_tid_out: *mut u64,
) -> bool {
    clear_last_storage_error();
    let Some(input) = input_arrays(values, is_null, byval, value_lens, natts) else {
        set_last_storage_error(invalid_ffi_argument("invalid row input arrays"));
        return false;
    };
    relation_update_impl(
        relid,
        packed_tid,
        input,
        new_tid_out,
        UniqueCheck::Skip,
        false,
        Some(UpdateMetadata {
            delete_xid,
            delete_cid,
            insert_xid,
            insert_cid,
            row_xmax,
        }),
    )
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid row input arrays and an optional valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_relation_update_redirect_unchecked(
    relid: u32,
    packed_tid: u64,
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
    new_tid_out: *mut u64,
) -> bool {
    clear_last_storage_error();
    let Some(input) = input_arrays(values, is_null, byval, value_lens, natts) else {
        set_last_storage_error(invalid_ffi_argument("invalid row input arrays"));
        return false;
    };
    relation_update_impl(
        relid,
        packed_tid,
        input,
        new_tid_out,
        UniqueCheck::Skip,
        true,
        None,
    )
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid row input arrays and an optional valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_relation_update_hot_unchecked(
    relid: u32,
    packed_tid: u64,
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
    new_tid_out: *mut u64,
) -> bool {
    clear_last_storage_error();
    let Some(input) = input_arrays(values, is_null, byval, value_lens, natts) else {
        set_last_storage_error(invalid_ffi_argument("invalid row input arrays"));
        return false;
    };
    relation_update_impl(
        relid,
        packed_tid,
        input,
        new_tid_out,
        UniqueCheck::Skip,
        true,
        None,
    )
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid row input arrays and an optional valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_relation_update_hot_unchecked_with_metadata(
    relid: u32,
    packed_tid: u64,
    delete_xid: u32,
    delete_cid: u32,
    insert_xid: u32,
    insert_cid: u32,
    row_xmax: u32,
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
    new_tid_out: *mut u64,
) -> bool {
    clear_last_storage_error();
    let Some(input) = input_arrays(values, is_null, byval, value_lens, natts) else {
        set_last_storage_error(invalid_ffi_argument("invalid row input arrays"));
        return false;
    };
    relation_update_impl(
        relid,
        packed_tid,
        input,
        new_tid_out,
        UniqueCheck::Skip,
        true,
        Some(UpdateMetadata {
            delete_xid,
            delete_cid,
            insert_xid,
            insert_cid,
            row_xmax,
        }),
    )
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid row input arrays and optional valid output pointers.
pub unsafe extern "C" fn fastpg_storage2_relation_update_hot_if_single_byval_preserved_with_metadata(
    relid: u32,
    packed_tid: u64,
    key_attnum: usize,
    key_value: usize,
    key_is_null: u8,
    delete_xid: u32,
    delete_cid: u32,
    insert_xid: u32,
    insert_cid: u32,
    row_xmax: u32,
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
    new_tid_out: *mut u64,
    hot_preserved_out: *mut bool,
) -> bool {
    clear_last_storage_error();
    if !hot_preserved_out.is_null() {
        unsafe {
            *hot_preserved_out = false;
        }
    }
    let Some(input) = input_arrays(values, is_null, byval, value_lens, natts) else {
        set_last_storage_error(invalid_ffi_argument("invalid row input arrays"));
        return false;
    };
    relation_update_hot_if_single_byval_preserved_impl(
        relid,
        packed_tid,
        input,
        SingleByvalHotKey {
            attnum: key_attnum,
            value: key_value,
            is_null: key_is_null,
        },
        HotUpdateOutputs {
            new_tid: new_tid_out,
            hot_preserved: hot_preserved_out,
        },
        Some(UpdateMetadata {
            delete_xid,
            delete_cid,
            insert_xid,
            insert_cid,
            row_xmax,
        }),
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_relation_delete(relid: u32, packed_tid: u64) -> bool {
    clear_last_storage_error();
    let Some(tid) = Tid::unpack(packed_tid) else {
        return false;
    };
    with_storage(|state, session| {
        let own_insert = session.owns_inserted_tid(relid, tid);
        let Some(tuple) = state.find_visible_tuple(session, relid, tid) else {
            return false;
        };
        let old_primary_key = primary_index_spec_for_relation_oid(Oid(relid))
            .and_then(|index_spec| index_key_for_decoded(&index_spec, &tuple.values));
        drop(tuple);
        session.ensure_transaction();
        let overlay = session
            .transaction_stack
            .last_mut()
            .expect("transaction was just ensured");
        overlay.invalidate(relid, tid);
        if let Some(key) = old_primary_key {
            if own_insert {
                overlay.remove_primary_key_insert(relid, &key);
            } else {
                overlay.delete_primary_key(relid, key);
            }
        }
        session.mark_scans_visibility_delta(relid);
        true
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_scan_begin(relid: u32) -> u64 {
    scan_begin_impl(relid, None)
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_scan_begin_with_snapshot(relid: u32, curcid: u32) -> u64 {
    scan_begin_impl(relid, Some(curcid))
}

fn scan_begin_impl(relid: u32, snapshot_curcid: Option<u32>) -> u64 {
    clear_last_storage_error();
    with_storage_read(|state, session| {
        let high_water_offsets = state
            .relations
            .get(&relid)
            .map(|relation| {
                relation
                    .pages
                    .iter()
                    .map(|page| {
                        page.as_ref()
                            .and_then(|page| u16::try_from(page.line_pointers.len()).ok())
                            .unwrap_or_default()
                    })
                    .collect::<HighWaterOffsets>()
            })
            .unwrap_or_default();
        let handle = session.allocate_scan_handle();
        let scan = ScanState {
            relid,
            high_water_offsets,
            forward_cursor: ScanCursor::forward_start(),
            backward_cursor: ScanCursor::backward_start(),
            forward_exhausted: false,
            backward_exhausted: false,
            has_visibility_deltas: session.transaction_has_visibility_deltas(relid),
            snapshot_curcid,
        };
        if !session.insert_scan(handle, scan) {
            return 0;
        }
        handle
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_scan_reset(scan_handle: u64) {
    with_session_storage(|session| {
        let has_visibility_deltas = session
            .scan_slot(scan_handle)
            .map(|scan| session.transaction_has_visibility_deltas(scan.relid));
        if let Some(scan) = session.scan_slot_mut(scan_handle) {
            scan.forward_cursor = ScanCursor::forward_start();
            scan.backward_cursor = ScanCursor::backward_start();
            scan.forward_exhausted = false;
            scan.backward_exhausted = false;
            if let Some(has_visibility_deltas) = has_visibility_deltas {
                scan.has_visibility_deltas = has_visibility_deltas;
            }
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_scan_set_position(scan_handle: u64, packed_tid: u64) -> bool {
    clear_last_storage_error();
    let Some(tid) = Tid::unpack(packed_tid) else {
        return false;
    };
    with_session_storage(|session| {
        let Some(scan) = session.scan_slot_mut(scan_handle) else {
            return false;
        };
        scan.forward_cursor = ScanCursor::after(tid);
        scan.backward_cursor = ScanCursor::before(tid);
        scan.forward_exhausted = false;
        scan.backward_exhausted = false;
        true
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_scan_end(scan_handle: u64) {
    with_session_storage(|session| {
        session.remove_scan(scan_handle);
    });
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output buffers for `natts` entries.
pub unsafe extern "C" fn fastpg_storage2_scan_next(
    scan_handle: u64,
    forward: u8,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
    tid_out: *mut u64,
) -> bool {
    scan_next_impl(
        scan_handle,
        forward,
        values_out,
        is_null_out,
        natts,
        tid_out,
        std::ptr::null_mut(),
    )
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output buffers for `natts` entries.
pub unsafe extern "C" fn fastpg_storage2_scan_next_with_stored_natts(
    scan_handle: u64,
    forward: u8,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
    tid_out: *mut u64,
    stored_natts_out: *mut usize,
) -> bool {
    scan_next_impl(
        scan_handle,
        forward,
        values_out,
        is_null_out,
        natts,
        tid_out,
        stored_natts_out,
    )
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output buffers for `natts * max_rows` value/null
/// entries and `max_rows` TID/stored-natts entries.
pub unsafe extern "C" fn fastpg_storage2_scan_next_batch_with_stored_natts(
    scan_handle: u64,
    forward: u8,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
    max_rows: usize,
    tids_out: *mut u64,
    stored_natts_out: *mut usize,
) -> usize {
    if max_rows == 0 {
        return 0;
    }
    if natts > 0 && (values_out.is_null() || is_null_out.is_null()) {
        return 0;
    }
    if tids_out.is_null() || stored_natts_out.is_null() {
        return 0;
    }

    let has_visibility_deltas = with_session_storage(|session| {
        session
            .scan_slot(scan_handle)
            .is_some_and(|scan| scan.has_visibility_deltas)
    });
    if !has_visibility_deltas {
        return with_storage_read(|state, session| {
            let (written, forward_cursor, backward_cursor, exhausted) = {
                let Some(scan) = session.scan_slot(scan_handle) else {
                    return 0;
                };
                let is_forward = forward != 0;
                if (is_forward && scan.forward_exhausted)
                    || (!is_forward && scan.backward_exhausted)
                {
                    return 0;
                }
                let relid = scan.relid;
                let mut forward_cursor = scan.forward_cursor;
                let mut backward_cursor = scan.backward_cursor;
                let mut written = 0;
                let mut exhausted = false;

                while written < max_rows {
                    let cursor = if is_forward {
                        forward_cursor
                    } else {
                        backward_cursor
                    };
                    let Some((tid, tuple)) = state.next_committed_tuple_slice(
                        relid,
                        cursor,
                        &scan.high_water_offsets,
                        is_forward,
                    ) else {
                        if is_forward {
                            backward_cursor = ScanCursor::before_cursor(forward_cursor);
                        } else {
                            forward_cursor = ScanCursor::after_cursor(backward_cursor);
                        }
                        exhausted = true;
                        break;
                    };

                    let row_values_out = if natts == 0 {
                        std::ptr::null_mut()
                    } else {
                        // SAFETY: caller provided `natts * max_rows` entries and
                        // `written < max_rows`, so this row's segment is in bounds.
                        unsafe { values_out.add(written * natts) }
                    };
                    let row_is_null_out = if natts == 0 {
                        std::ptr::null_mut()
                    } else {
                        // SAFETY: caller provided `natts * max_rows` entries and
                        // `written < max_rows`, so this row's segment is in bounds.
                        unsafe { is_null_out.add(written * natts) }
                    };
                    // SAFETY: caller provided `max_rows` TID/stored-natts entries and
                    // `written < max_rows`.
                    let row_tid_out = unsafe { tids_out.add(written) };
                    let row_stored_natts_out = unsafe { stored_natts_out.add(written) };
                    if !copy_tuple_to_outputs(
                        tid,
                        tuple,
                        row_values_out,
                        row_is_null_out,
                        natts,
                        row_tid_out,
                        row_stored_natts_out,
                    ) {
                        break;
                    }

                    if is_forward {
                        forward_cursor = ScanCursor::after(tid);
                        backward_cursor = ScanCursor::before(tid);
                    } else {
                        backward_cursor = ScanCursor::before(tid);
                        forward_cursor = ScanCursor::after(tid);
                    }
                    written += 1;
                }

                (written, forward_cursor, backward_cursor, exhausted)
            };

            if let Some(scan) = session.scan_slot_mut(scan_handle) {
                scan.forward_cursor = forward_cursor;
                scan.backward_cursor = backward_cursor;
                if written > 0 {
                    if forward != 0 {
                        scan.backward_exhausted = false;
                    } else {
                        scan.forward_exhausted = false;
                    }
                }
                if exhausted {
                    if forward != 0 {
                        scan.forward_exhausted = true;
                        scan.backward_exhausted = false;
                    } else {
                        scan.backward_exhausted = true;
                        scan.forward_exhausted = false;
                    }
                }
            }

            written
        });
    }

    with_storage(|state, session| {
        let (written, forward_cursor, backward_cursor, exhausted) = {
            let Some(scan) = session.scan_slot(scan_handle) else {
                return 0;
            };
            let is_forward = forward != 0;
            if (is_forward && scan.forward_exhausted) || (!is_forward && scan.backward_exhausted) {
                return 0;
            }
            let relid = scan.relid;
            let mut forward_cursor = scan.forward_cursor;
            let mut backward_cursor = scan.backward_cursor;
            let mut written = 0;
            let mut exhausted = false;

            while written < max_rows {
                let cursor = if is_forward {
                    forward_cursor
                } else {
                    backward_cursor
                };
                let tuple = if scan.has_visibility_deltas {
                    state.next_visible_tuple_slice_in_overlays(
                        &session.transaction_stack,
                        relid,
                        cursor,
                        &scan.high_water_offsets,
                        is_forward,
                        scan.snapshot_curcid,
                    )
                } else {
                    state.next_committed_tuple_slice(
                        relid,
                        cursor,
                        &scan.high_water_offsets,
                        is_forward,
                    )
                };
                let Some((tid, tuple)) = tuple else {
                    if is_forward {
                        backward_cursor = ScanCursor::before_cursor(forward_cursor);
                    } else {
                        forward_cursor = ScanCursor::after_cursor(backward_cursor);
                    }
                    exhausted = true;
                    break;
                };

                let row_values_out = if natts == 0 {
                    std::ptr::null_mut()
                } else {
                    // SAFETY: caller provided `natts * max_rows` entries and
                    // `written < max_rows`, so this row's segment is in bounds.
                    unsafe { values_out.add(written * natts) }
                };
                let row_is_null_out = if natts == 0 {
                    std::ptr::null_mut()
                } else {
                    // SAFETY: caller provided `natts * max_rows` entries and
                    // `written < max_rows`, so this row's segment is in bounds.
                    unsafe { is_null_out.add(written * natts) }
                };
                // SAFETY: caller provided `max_rows` TID/stored-natts entries and
                // `written < max_rows`.
                let row_tid_out = unsafe { tids_out.add(written) };
                let row_stored_natts_out = unsafe { stored_natts_out.add(written) };
                if !copy_tuple_to_outputs(
                    tid,
                    tuple,
                    row_values_out,
                    row_is_null_out,
                    natts,
                    row_tid_out,
                    row_stored_natts_out,
                ) {
                    break;
                }

                if is_forward {
                    forward_cursor = ScanCursor::after(tid);
                    backward_cursor = ScanCursor::before(tid);
                } else {
                    backward_cursor = ScanCursor::before(tid);
                    forward_cursor = ScanCursor::after(tid);
                }
                written += 1;
            }

            (written, forward_cursor, backward_cursor, exhausted)
        };

        if let Some(scan) = session.scan_slot_mut(scan_handle) {
            scan.forward_cursor = forward_cursor;
            scan.backward_cursor = backward_cursor;
            if written > 0 {
                if forward != 0 {
                    scan.backward_exhausted = false;
                } else {
                    scan.forward_exhausted = false;
                }
            }
            if exhausted {
                if forward != 0 {
                    scan.forward_exhausted = true;
                    scan.backward_exhausted = false;
                } else {
                    scan.backward_exhausted = true;
                    scan.forward_exhausted = false;
                }
            }
        }

        written
    })
}

fn scan_next_impl(
    scan_handle: u64,
    forward: u8,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
    tid_out: *mut u64,
    stored_natts_out: *mut usize,
) -> bool {
    let has_visibility_deltas = with_session_storage(|session| {
        session
            .scan_slot(scan_handle)
            .is_some_and(|scan| scan.has_visibility_deltas)
    });
    if !has_visibility_deltas {
        return with_storage_read(|state, session| {
            let Some(scan) = session.scan_slot(scan_handle) else {
                return false;
            };
            let is_forward = forward != 0;
            if (is_forward && scan.forward_exhausted) || (!is_forward && scan.backward_exhausted) {
                return false;
            }
            let cursor = if is_forward {
                scan.forward_cursor
            } else {
                scan.backward_cursor
            };
            let relid = scan.relid;
            let Some((tid, tuple)) = state.next_committed_tuple_slice(
                relid,
                cursor,
                &scan.high_water_offsets,
                is_forward,
            ) else {
                if let Some(scan) = session.scan_slot_mut(scan_handle) {
                    if is_forward {
                        scan.backward_cursor = ScanCursor::before_cursor(scan.forward_cursor);
                        scan.forward_exhausted = true;
                        scan.backward_exhausted = false;
                    } else {
                        scan.forward_cursor = ScanCursor::after_cursor(scan.backward_cursor);
                        scan.backward_exhausted = true;
                        scan.forward_exhausted = false;
                    }
                }
                return false;
            };
            if let Some(scan) = session.scan_slot_mut(scan_handle) {
                if is_forward {
                    scan.forward_cursor = ScanCursor::after(tid);
                    scan.backward_cursor = ScanCursor::before(tid);
                    scan.backward_exhausted = false;
                } else {
                    scan.backward_cursor = ScanCursor::before(tid);
                    scan.forward_cursor = ScanCursor::after(tid);
                    scan.forward_exhausted = false;
                }
            }
            copy_tuple_to_outputs(
                tid,
                tuple,
                values_out,
                is_null_out,
                natts,
                tid_out,
                stored_natts_out,
            )
        });
    }

    with_storage(|state, session| {
        let Some(scan) = session.scan_slot(scan_handle) else {
            return false;
        };
        let is_forward = forward != 0;
        if (is_forward && scan.forward_exhausted) || (!is_forward && scan.backward_exhausted) {
            return false;
        }
        let cursor = if is_forward {
            scan.forward_cursor
        } else {
            scan.backward_cursor
        };
        let relid = scan.relid;
        let tuple = if scan.has_visibility_deltas {
            state.next_visible_tuple_slice_in_overlays(
                &session.transaction_stack,
                relid,
                cursor,
                &scan.high_water_offsets,
                is_forward,
                scan.snapshot_curcid,
            )
        } else {
            state.next_committed_tuple_slice(relid, cursor, &scan.high_water_offsets, is_forward)
        };
        let Some((tid, tuple)) = tuple else {
            if let Some(scan) = session.scan_slot_mut(scan_handle) {
                if is_forward {
                    scan.backward_cursor = ScanCursor::before_cursor(scan.forward_cursor);
                    scan.forward_exhausted = true;
                    scan.backward_exhausted = false;
                } else {
                    scan.forward_cursor = ScanCursor::after_cursor(scan.backward_cursor);
                    scan.backward_exhausted = true;
                    scan.forward_exhausted = false;
                }
            }
            return false;
        };
        if let Some(scan) = session.scan_slot_mut(scan_handle) {
            if is_forward {
                scan.forward_cursor = ScanCursor::after(tid);
                scan.backward_cursor = ScanCursor::before(tid);
                scan.backward_exhausted = false;
            } else {
                scan.backward_cursor = ScanCursor::before(tid);
                scan.forward_cursor = ScanCursor::after(tid);
                scan.forward_exhausted = false;
            }
        }
        copy_tuple_to_outputs(
            tid,
            tuple,
            values_out,
            is_null_out,
            natts,
            tid_out,
            stored_natts_out,
        )
    })
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output buffers for `natts` entries.
pub unsafe extern "C" fn fastpg_storage2_fetch_tid_snapshot(
    relid: u32,
    packed_tid: u64,
    curcid: u32,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
) -> bool {
    fetch_tid_snapshot_impl(
        relid,
        packed_tid,
        curcid,
        values_out,
        is_null_out,
        natts,
        std::ptr::null_mut(),
    )
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output buffers for `natts` entries.
pub unsafe extern "C" fn fastpg_storage2_fetch_tid_snapshot_with_stored_natts(
    relid: u32,
    packed_tid: u64,
    curcid: u32,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
    stored_natts_out: *mut usize,
) -> bool {
    fetch_tid_snapshot_impl(
        relid,
        packed_tid,
        curcid,
        values_out,
        is_null_out,
        natts,
        stored_natts_out,
    )
}

fn fetch_tid_snapshot_impl(
    relid: u32,
    packed_tid: u64,
    curcid: u32,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
    stored_natts_out: *mut usize,
) -> bool {
    let Some(tid) = Tid::unpack(packed_tid) else {
        return false;
    };
    with_storage_read(|state, session| {
        let Some(tuple) = state.visible_tuple_slice_in_overlays_at_cid(
            &session.transaction_stack,
            relid,
            tid,
            curcid,
        ) else {
            return false;
        };
        copy_tuple_to_outputs(
            tid,
            tuple,
            values_out,
            is_null_out,
            natts,
            std::ptr::null_mut(),
            stored_natts_out,
        )
    })
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output buffers for `natts` entries.
pub unsafe extern "C" fn fastpg_storage2_fetch_tid(
    relid: u32,
    packed_tid: u64,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
) -> bool {
    fetch_tid_impl(
        relid,
        packed_tid,
        values_out,
        is_null_out,
        natts,
        std::ptr::null_mut(),
    )
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output buffers for `natts` entries.
pub unsafe extern "C" fn fastpg_storage2_fetch_tid_with_stored_natts(
    relid: u32,
    packed_tid: u64,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
    stored_natts_out: *mut usize,
) -> bool {
    fetch_tid_impl(
        relid,
        packed_tid,
        values_out,
        is_null_out,
        natts,
        stored_natts_out,
    )
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass a TID that has already been resolved through storage2
/// redirects, plus valid output buffers for `natts` entries.
pub unsafe extern "C" fn fastpg_storage2_fetch_resolved_tid_with_stored_natts(
    relid: u32,
    packed_tid: u64,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
    stored_natts_out: *mut usize,
) -> bool {
    let Some(tid) = Tid::unpack(packed_tid) else {
        return false;
    };
    with_storage_read(|state, session| {
        let Some(tuple) =
            state.visible_tuple_slice_in_overlays(&session.transaction_stack, relid, tid)
        else {
            return false;
        };
        copy_tuple_to_outputs(
            tid,
            tuple,
            values_out,
            is_null_out,
            natts,
            std::ptr::null_mut(),
            stored_natts_out,
        )
    })
}

fn fetch_tid_impl(
    relid: u32,
    packed_tid: u64,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
    stored_natts_out: *mut usize,
) -> bool {
    let Some(tid) = Tid::unpack(packed_tid) else {
        return false;
    };
    with_storage(|state, session| {
        let tid =
            state.resolve_tid_redirect_in_overlays_compress(&session.transaction_stack, relid, tid);
        let Some(tuple) =
            state.visible_tuple_slice_in_overlays(&session.transaction_stack, relid, tid)
        else {
            return false;
        };
        copy_tuple_to_outputs(
            tid,
            tuple,
            values_out,
            is_null_out,
            natts,
            std::ptr::null_mut(),
            stored_natts_out,
        )
    })
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output buffers for `natts` entries.
pub unsafe extern "C" fn fastpg_storage2_fetch_tid_any(
    relid: u32,
    packed_tid: u64,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
) -> bool {
    fetch_tid_any_impl(
        relid,
        packed_tid,
        values_out,
        is_null_out,
        natts,
        std::ptr::null_mut(),
    )
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output buffers for `natts` entries.
pub unsafe extern "C" fn fastpg_storage2_fetch_tid_any_with_stored_natts(
    relid: u32,
    packed_tid: u64,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
    stored_natts_out: *mut usize,
) -> bool {
    fetch_tid_any_impl(
        relid,
        packed_tid,
        values_out,
        is_null_out,
        natts,
        stored_natts_out,
    )
}

fn fetch_tid_any_impl(
    relid: u32,
    packed_tid: u64,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
    stored_natts_out: *mut usize,
) -> bool {
    let Some(tid) = Tid::unpack(packed_tid) else {
        return false;
    };
    with_storage_read(|state, _session| {
        let Some(tuple) = state
            .relations
            .get(&relid)
            .and_then(|relation| relation.tuple_slice_any(tid))
        else {
            return false;
        };
        copy_tuple_to_outputs(
            tid,
            tuple,
            values_out,
            is_null_out,
            natts,
            std::ptr::null_mut(),
            stored_natts_out,
        )
    })
}
#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid key input arrays and an optional valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_primary_key_index_lookup(
    index_relid: u32,
    values: *const usize,
    is_null: *const u8,
    nkeys: usize,
    tid_out: *mut u64,
) -> bool {
    clear_last_storage_error();
    let Some((values, is_null)) = key_arrays(values, is_null, nkeys) else {
        return false;
    };
    let Some(index_spec) = primary_index_spec_for_index_oid(Oid(index_relid)) else {
        return false;
    };
    let Some(key) = index_key_for_key_datums(&index_spec, values, is_null) else {
        return false;
    };
    let tid = with_storage(|state, session| {
        state.primary_key_lookup(session, index_spec.relation_oid.0, &key)
    });
    let Some(tid) = tid else {
        return false;
    };
    if !tid_out.is_null() {
        unsafe {
            *tid_out = tid.pack();
        }
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid index metadata arrays, key input arrays, and an
/// optional valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_primary_key_index_lookup_with_spec(
    index_relid: u32,
    heap_relid: u32,
    attnums: *const i16,
    typbyval: *const u8,
    typlen: *const i16,
    values: *const usize,
    is_null: *const u8,
    nkeys: usize,
    tid_out: *mut u64,
) -> bool {
    clear_last_storage_error();
    let Some((values, is_null)) = key_arrays(values, is_null, nkeys) else {
        return false;
    };
    let Some(index_spec) = (unsafe {
        unique_index_spec_from_ffi(UniqueIndexFfiSpecArgs {
            index_relid,
            heap_relid,
            attnums,
            typbyval,
            typlen,
            nkeys,
            is_primary: true,
            nulls_not_distinct: false,
        })
    }) else {
        return false;
    };
    let Some(key) = index_key_for_key_datums(&index_spec, values, is_null) else {
        return false;
    };
    let tid = with_storage(|state, session| {
        state
            .primary_key_lookup(session, heap_relid, &key)
            .or_else(|| {
                let mut scan_spec = index_spec.clone();
                scan_spec.is_primary = false;
                state.find_visible_by_index_key_excluding(
                    session, heap_relid, &scan_spec, &key, None,
                )
            })
    });
    let Some(tid) = tid else {
        return false;
    };
    if !tid_out.is_null() {
        unsafe {
            *tid_out = tid.pack();
        }
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass an optional valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_primary_key_index_lookup_single_byval_with_spec(
    _index_relid: u32,
    heap_relid: u32,
    value: usize,
    is_null: u8,
    tid_out: *mut u64,
) -> bool {
    clear_last_storage_error();
    if is_null != 0 {
        return false;
    }

    let key = IndexKey::single(IndexKeyPart::ByValue(value));
    let tid = with_storage(|state, session| state.primary_key_lookup(session, heap_relid, &key));
    let Some(tid) = tid else {
        return false;
    };
    if !tid_out.is_null() {
        unsafe {
            *tid_out = tid.pack();
        }
    }
    true
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_rebuild_primary_key_index(index_relid: u32) -> bool {
    clear_last_storage_error();
    let Some(index_spec) = primary_index_spec_for_index_oid(Oid(index_relid)) else {
        return false;
    };
    with_storage_read(|state, session| {
        let relid = index_spec.relation_oid.0;
        let entries = state
            .visible_tids(session, relid)
            .into_iter()
            .filter_map(|tid| {
                state
                    .find_visible_tuple(session, relid, tid)
                    .and_then(|tuple| index_key_for_decoded(&index_spec, &tuple.values))
                    .map(|key| (key, tid))
            })
            .collect::<Vec<_>>();
        session.ensure_transaction();
        let overlay = session
            .transaction_stack
            .last_mut()
            .expect("transaction was just ensured");
        for (key, tid) in entries {
            overlay.insert_primary_key(relid, key, tid);
        }
        true
    })
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid index metadata arrays.
pub unsafe extern "C" fn fastpg_storage2_rebuild_primary_key_index_with_spec(
    index_relid: u32,
    heap_relid: u32,
    attnums: *const i16,
    typbyval: *const u8,
    typlen: *const i16,
    nkeys: usize,
) -> bool {
    clear_last_storage_error();
    let Some(index_spec) = (unsafe {
        unique_index_spec_from_ffi(UniqueIndexFfiSpecArgs {
            index_relid,
            heap_relid,
            attnums,
            typbyval,
            typlen,
            nkeys,
            is_primary: true,
            nulls_not_distinct: false,
        })
    }) else {
        return false;
    };
    with_storage_read(|state, session| {
        let entries = state
            .visible_tids(session, heap_relid)
            .into_iter()
            .filter_map(|tid| {
                state
                    .find_visible_tuple(session, heap_relid, tid)
                    .and_then(|tuple| index_key_for_decoded(&index_spec, &tuple.values))
                    .map(|key| (key, tid))
            })
            .collect::<Vec<_>>();
        session.ensure_transaction();
        let overlay = session
            .transaction_stack
            .last_mut()
            .expect("transaction was just ensured");
        for (key, tid) in entries {
            overlay.insert_primary_key(heap_relid, key, tid);
        }
        true
    })
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid key input arrays and an optional valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_unique_index_conflict(
    index_relid: u32,
    values: *const usize,
    is_null: *const u8,
    nkeys: usize,
    replacing_tid: u64,
    tid_out: *mut u64,
) -> bool {
    clear_last_storage_error();
    let Some((values, is_null)) = key_arrays(values, is_null, nkeys) else {
        return false;
    };
    let Some(relid) = relation_oid_for_index_oid(Oid(index_relid)) else {
        return false;
    };
    let Some(index_spec) = unique_index_records_for_relation_oid(relid)
        .iter()
        .filter_map(unique_index_spec_for_record)
        .find(|spec| spec.index_oid == Oid(index_relid))
    else {
        return false;
    };
    let Some(key) = index_key_for_key_datums(&index_spec, values, is_null) else {
        return false;
    };
    let replacing_tid = if replacing_tid == 0 {
        None
    } else {
        Tid::unpack(replacing_tid)
    };
    let conflict = with_storage_read(|state, session| {
        state.find_visible_by_index_key_excluding_read(
            session,
            relid.0,
            &index_spec,
            &key,
            replacing_tid,
        )
    });
    let Some(tid) = conflict else {
        return false;
    };
    if !tid_out.is_null() {
        unsafe {
            *tid_out = tid.pack();
        }
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid index metadata arrays, key input arrays, and an
/// optional valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_unique_index_conflict_with_spec(
    index_relid: u32,
    heap_relid: u32,
    attnums: *const i16,
    typbyval: *const u8,
    typlen: *const i16,
    values: *const usize,
    is_null: *const u8,
    nkeys: usize,
    nulls_not_distinct: u8,
    replacing_tid: u64,
    tid_out: *mut u64,
) -> bool {
    clear_last_storage_error();
    let Some((values, is_null)) = key_arrays(values, is_null, nkeys) else {
        return false;
    };
    let Some(index_spec) = (unsafe {
        unique_index_spec_from_ffi(UniqueIndexFfiSpecArgs {
            index_relid,
            heap_relid,
            attnums,
            typbyval,
            typlen,
            nkeys,
            is_primary: false,
            nulls_not_distinct: nulls_not_distinct != 0,
        })
    }) else {
        return false;
    };
    let Some(key) = index_key_for_key_datums(&index_spec, values, is_null) else {
        return false;
    };
    let replacing_tid = if replacing_tid == 0 {
        None
    } else {
        Tid::unpack(replacing_tid)
    };
    let conflict = with_storage_read(|state, session| {
        state.find_visible_by_index_key_excluding_read(
            session,
            heap_relid,
            &index_spec,
            &key,
            replacing_tid,
        )
    });
    let Some(tid) = conflict else {
        return false;
    };
    if !tid_out.is_null() {
        unsafe {
            *tid_out = tid.pack();
        }
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid index metadata arrays and an optional valid output
/// pointer.
pub unsafe extern "C" fn fastpg_storage2_unique_index_validate_with_spec(
    index_relid: u32,
    heap_relid: u32,
    attnums: *const i16,
    typbyval: *const u8,
    typlen: *const i16,
    nkeys: usize,
    nulls_not_distinct: u8,
    tid_out: *mut u64,
) -> bool {
    clear_last_storage_error();
    let Some(index_spec) = (unsafe {
        unique_index_spec_from_ffi(UniqueIndexFfiSpecArgs {
            index_relid,
            heap_relid,
            attnums,
            typbyval,
            typlen,
            nkeys,
            is_primary: false,
            nulls_not_distinct: nulls_not_distinct != 0,
        })
    }) else {
        return false;
    };
    let conflict = with_storage_read(|state, session| {
        let mut seen = BTreeMap::new();
        for tid in state.visible_tids(session, heap_relid) {
            let Some(key) = state
                .find_visible_tuple(session, heap_relid, tid)
                .and_then(|tuple| index_key_for_decoded(&index_spec, &tuple.values))
            else {
                continue;
            };
            if let Some(existing_tid) = seen.get(&key).copied() {
                return Some(existing_tid);
            }
            seen.insert(key, tid);
        }
        None
    });
    let Some(tid) = conflict else {
        return false;
    };
    if !tid_out.is_null() {
        unsafe {
            *tid_out = tid.pack();
        }
    }
    true
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass a valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_metrics(out: *mut FastPgStorage2Metrics) -> bool {
    if out.is_null() {
        return false;
    }
    let metrics = with_storage_read(|state, session| state.metrics(session));
    unsafe {
        *out = metrics;
    }
    true
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_committed_page_bytes() -> usize {
    with_storage_read(|state, session| state.metrics(session).committed_page_bytes)
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_transaction_page_bytes() -> usize {
    with_storage_read(|state, session| state.metrics(session).transaction_page_bytes)
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_index_bytes() -> usize {
    with_storage_read(|state, session| state.metrics(session).index_bytes)
}

#[unsafe(no_mangle)]
/// # Safety
///
/// C callers must pass valid output buffers when non-null.
pub unsafe extern "C" fn fastpg_storage2_last_error(
    sqlstate_out: *mut c_char,
    sqlstate_len: usize,
    message_out: *mut c_char,
    message_len: usize,
) -> bool {
    let Some(error) = last_storage_error() else {
        return false;
    };
    write_storage_error(sqlstate_out, sqlstate_len, &error.sqlstate);
    write_storage_error(message_out, message_len, &error.message);
    true
}
