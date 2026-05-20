#![deny(unsafe_op_in_unsafe_fn)]

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::c_char;
use std::slice;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

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

pub(crate) static STORAGE2_ARENA_REWINDS: AtomicU64 = AtomicU64::new(0);
pub(crate) static STORAGE2_ARENA_DROPS: AtomicU64 = AtomicU64::new(0);
pub(crate) static STORAGE2_METADATA_CACHE: OnceLock<Mutex<Storage2MetadataCache>> = OnceLock::new();

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
    SessionStorageGuard, SessionStorageHandle, enter_session_storage, new_session_storage,
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
    with_storage(|state, session| state.begin_explicit_transaction(session));
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_xact_begin_implicit() {
    with_storage(|_state, session| session.ensure_transaction());
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
    with_storage(|state, session| state.commit_implicit_transaction(session));
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_xact_abort_if_implicit() {
    with_storage(|state, session| state.abort_implicit_transaction(session));
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_subxact_begin() {
    with_storage(|_state, session| {
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
pub extern "C" fn fastpg_storage2_relation_row_count(relid: u32) -> usize {
    with_storage(|state, session| state.visible_row_count(session, relid))
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_relation_contains_tid(relid: u32, packed_tid: u64) -> bool {
    let Some(tid) = Tid::unpack(packed_tid) else {
        return false;
    };
    with_storage(|state, session| state.find_visible_tuple(session, relid, tid).is_some())
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
    relation_insert_impl(relid, input, tid_out, UniqueCheck::Enforce)
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
    relation_insert_impl(relid, input, tid_out, UniqueCheck::Skip)
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
    relation_update_impl(relid, packed_tid, input, new_tid_out, UniqueCheck::Enforce)
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
    relation_update_impl(relid, packed_tid, input, new_tid_out, UniqueCheck::Skip)
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_relation_delete(relid: u32, packed_tid: u64) -> bool {
    clear_last_storage_error();
    let Some(tid) = Tid::unpack(packed_tid) else {
        return false;
    };
    with_storage(|state, session| {
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
            overlay.delete_primary_key(relid, key);
        }
        true
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_scan_begin(relid: u32) -> u64 {
    clear_last_storage_error();
    with_storage(|state, session| {
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
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let handle = session.allocate_scan_handle();
        session.scans.insert(
            handle,
            ScanState {
                relid,
                high_water_offsets,
                forward_cursor: ScanCursor::forward_start(),
                backward_cursor: ScanCursor::backward_start(),
            },
        );
        handle
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_scan_reset(scan_handle: u64) {
    with_storage(|_state, session| {
        if let Some(scan) = session.scans.get_mut(&scan_handle) {
            scan.forward_cursor = ScanCursor::forward_start();
            scan.backward_cursor = ScanCursor::backward_start();
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_scan_end(scan_handle: u64) {
    with_storage(|_state, session| {
        session.scans.remove(&scan_handle);
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
    with_storage(|state, session| {
        let overlays = &session.transaction_stack;
        let Some(scan) = session.scans.get_mut(&scan_handle) else {
            return false;
        };
        let is_forward = forward != 0;
        let cursor = if is_forward {
            scan.forward_cursor
        } else {
            scan.backward_cursor
        };
        let relid = scan.relid;
        let Some((tid, tuple)) = state.next_visible_tuple_slice_in_overlays(
            overlays,
            relid,
            cursor,
            &scan.high_water_offsets,
            is_forward,
        ) else {
            return false;
        };
        if is_forward {
            scan.forward_cursor = ScanCursor::after(tid);
        } else {
            scan.backward_cursor = ScanCursor::before(tid);
        }
        copy_tuple_to_outputs(tid, tuple, values_out, is_null_out, natts, tid_out)
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
    let Some(tid) = Tid::unpack(packed_tid) else {
        return false;
    };
    with_storage(|state, session| {
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
    let conflict = with_storage(|state, session| {
        state.find_visible_by_index_key_excluding(
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
/// C callers must pass a valid output pointer.
pub unsafe extern "C" fn fastpg_storage2_metrics(out: *mut FastPgStorage2Metrics) -> bool {
    if out.is_null() {
        return false;
    }
    let metrics = with_storage(|state, session| state.metrics(session));
    unsafe {
        *out = metrics;
    }
    true
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_committed_page_bytes() -> usize {
    with_storage(|state, session| state.metrics(session).committed_page_bytes)
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_transaction_page_bytes() -> usize {
    with_storage(|state, session| state.metrics(session).transaction_page_bytes)
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_storage2_index_bytes() -> usize {
    with_storage(|state, session| state.metrics(session).index_bytes)
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
