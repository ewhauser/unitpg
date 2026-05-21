use super::*;
use std::sync::{Mutex as StdMutex, MutexGuard};

static TEST_LOCK: StdMutex<()> = StdMutex::new(());

struct TestGuard {
    _guard: MutexGuard<'static, ()>,
    _session_guard: SessionStorageGuard,
}

impl Drop for TestGuard {
    fn drop(&mut self) {
        fastpg_storage2_xact_abort();
    }
}

fn test_guard() -> TestGuard {
    let guard = TEST_LOCK.lock().expect("test lock poisoned");
    let session = new_session_storage();
    let session_guard = enter_session_storage(session);
    fastpg_storage2_xact_abort();
    TestGuard {
        _guard: guard,
        _session_guard: session_guard,
    }
}

fn insert_i32(relid: u32, value: i32) -> u64 {
    let values = [value as usize];
    let nulls = [0u8];
    let byval = [1u8];
    let lens = [0usize];
    let mut tid = 0;
    assert!(unsafe {
        fastpg_storage2_relation_insert_unchecked(
            relid,
            values.as_ptr(),
            nulls.as_ptr(),
            byval.as_ptr(),
            lens.as_ptr(),
            values.len(),
            &mut tid,
        )
    });
    tid
}

fn fetch_i32(relid: u32, tid: u64) -> Option<i32> {
    let mut values = [0usize];
    let mut nulls = [1u8];
    if unsafe { fastpg_storage2_fetch_tid(relid, tid, values.as_mut_ptr(), nulls.as_mut_ptr(), 1) }
    {
        Some(values[0] as i32)
    } else {
        None
    }
}

#[test]
fn insert_fetch_uses_stable_tid() {
    let _guard = test_guard();
    let relid = 42;
    let tid = insert_i32(relid, 7);
    assert_ne!(tid, 0);
    assert_eq!(fetch_i32(relid, tid), Some(7));
    assert_eq!(fastpg_storage2_relation_row_count(relid), 1);
}

#[test]
fn explicit_abort_drops_transaction_arena() {
    let _guard = test_guard();
    let relid = 43;
    fastpg_storage2_xact_begin();
    let tid = insert_i32(relid, 1);
    assert_eq!(fetch_i32(relid, tid), Some(1));
    assert!(fastpg_storage2_transaction_page_bytes() >= PAGE_SIZE);
    fastpg_storage2_xact_abort();
    assert_eq!(fetch_i32(relid, tid), None);
    assert_eq!(fastpg_storage2_relation_row_count(relid), 0);
    assert_eq!(fastpg_storage2_transaction_page_bytes(), 0);
}

#[test]
fn implicit_statement_abort_preserves_prior_implicit_commit() {
    let _guard = test_guard();
    let relid = 430;

    fastpg_storage2_xact_begin_implicit();
    let committed_tid = insert_i32(relid, 1);
    fastpg_storage2_xact_commit_if_implicit();
    assert_eq!(fetch_i32(relid, committed_tid), Some(1));
    assert_eq!(fastpg_storage2_relation_row_count(relid), 1);

    fastpg_storage2_xact_begin_implicit();
    let aborted_tid = insert_i32(relid, 2);
    assert_eq!(fetch_i32(relid, aborted_tid), Some(2));
    fastpg_storage2_xact_abort_if_implicit();

    assert_eq!(fetch_i32(relid, committed_tid), Some(1));
    assert_eq!(fetch_i32(relid, aborted_tid), None);
    assert_eq!(fastpg_storage2_relation_row_count(relid), 1);
}

#[test]
fn commit_publishes_pages_and_delete_rollback_restores_visibility() {
    let _guard = test_guard();
    let relid = 44;
    fastpg_storage2_xact_begin();
    let tid = insert_i32(relid, 2);
    fastpg_storage2_xact_commit();
    assert_eq!(fetch_i32(relid, tid), Some(2));
    assert!(fastpg_storage2_committed_page_bytes() >= PAGE_SIZE);

    fastpg_storage2_xact_begin();
    assert!(fastpg_storage2_relation_delete(relid, tid));
    assert_eq!(fetch_i32(relid, tid), None);
    fastpg_storage2_xact_abort();
    assert_eq!(fetch_i32(relid, tid), Some(2));
}

#[test]
fn update_appends_new_tid_and_abort_restores_old_tid() {
    let _guard = test_guard();
    let relid = 45;
    fastpg_storage2_xact_begin();
    let old_tid = insert_i32(relid, 3);
    fastpg_storage2_xact_commit();

    let values = [4usize];
    let nulls = [0u8];
    let byval = [1u8];
    let lens = [0usize];
    let mut new_tid = 0;
    fastpg_storage2_xact_begin();
    assert!(unsafe {
        fastpg_storage2_relation_update_unchecked(
            relid,
            old_tid,
            values.as_ptr(),
            nulls.as_ptr(),
            byval.as_ptr(),
            lens.as_ptr(),
            values.len(),
            &mut new_tid,
        )
    });
    assert_ne!(old_tid, new_tid);
    assert_eq!(fetch_i32(relid, old_tid), None);
    assert_eq!(fetch_i32(relid, new_tid), Some(4));
    fastpg_storage2_xact_abort();
    assert_eq!(fetch_i32(relid, old_tid), Some(3));
    assert_eq!(fetch_i32(relid, new_tid), None);
}

#[test]
fn savepoint_abort_drops_nested_pages() {
    let _guard = test_guard();
    let relid = 46;
    fastpg_storage2_xact_begin();
    let parent_tid = insert_i32(relid, 5);
    let bytes_before = fastpg_storage2_transaction_page_bytes();
    fastpg_storage2_subxact_begin();
    let nested_tid = insert_i32(relid, 6);
    assert_eq!(fetch_i32(relid, nested_tid), Some(6));
    fastpg_storage2_subxact_abort();
    assert_eq!(fetch_i32(relid, parent_tid), Some(5));
    assert_eq!(fetch_i32(relid, nested_tid), None);
    assert!(fastpg_storage2_transaction_page_bytes() <= bytes_before);
    fastpg_storage2_xact_commit();
    assert_eq!(fetch_i32(relid, parent_tid), Some(5));
}

#[test]
fn savepoint_abort_restores_parent_relation_clear() {
    let _guard = test_guard();
    let relid = 460;
    fastpg_storage2_xact_begin();
    let parent_tid = insert_i32(relid, 5);

    fastpg_storage2_subxact_begin();
    fastpg_storage2_relation_clear(relid);
    assert_eq!(fetch_i32(relid, parent_tid), None);
    assert_eq!(fastpg_storage2_relation_row_count(relid), 0);
    fastpg_storage2_subxact_abort();

    assert_eq!(fetch_i32(relid, parent_tid), Some(5));
    assert_eq!(fastpg_storage2_relation_row_count(relid), 1);
    fastpg_storage2_xact_commit();
    assert_eq!(fetch_i32(relid, parent_tid), Some(5));
}

#[test]
fn savepoint_commit_preserves_relation_clear() {
    let _guard = test_guard();
    let relid = 461;
    fastpg_storage2_xact_begin();
    let parent_tid = insert_i32(relid, 5);

    fastpg_storage2_subxact_begin();
    fastpg_storage2_relation_clear(relid);
    fastpg_storage2_subxact_commit();

    assert_eq!(fetch_i32(relid, parent_tid), None);
    assert_eq!(fastpg_storage2_relation_row_count(relid), 0);
    fastpg_storage2_xact_commit();
    assert_eq!(fetch_i32(relid, parent_tid), None);
}

#[test]
fn scan_tracks_tids_not_materialized_rows() {
    let _guard = test_guard();
    let relid = 47;
    fastpg_storage2_xact_begin();
    insert_i32(relid, 10);
    insert_i32(relid, 11);
    let scan = fastpg_storage2_scan_begin(relid);
    assert_ne!(scan, 0);
    assert!(fastpg_storage2_metrics_snapshot().scan_scratch_bytes <= 256);
    let mut values = [0usize];
    let mut nulls = [1u8];
    let mut tid = 0u64;
    assert!(unsafe {
        fastpg_storage2_scan_next(
            scan,
            1,
            values.as_mut_ptr(),
            nulls.as_mut_ptr(),
            1,
            &mut tid,
        )
    });
    assert_eq!(values[0], 10);
    assert!(unsafe {
        fastpg_storage2_scan_next(
            scan,
            1,
            values.as_mut_ptr(),
            nulls.as_mut_ptr(),
            1,
            &mut tid,
        )
    });
    assert_eq!(values[0], 11);
    assert!(!unsafe {
        fastpg_storage2_scan_next(
            scan,
            1,
            values.as_mut_ptr(),
            nulls.as_mut_ptr(),
            1,
            &mut tid,
        )
    });
    fastpg_storage2_scan_end(scan);

    let backward_scan = fastpg_storage2_scan_begin(relid);
    assert_ne!(backward_scan, 0);
    assert!(unsafe {
        fastpg_storage2_scan_next(
            backward_scan,
            0,
            values.as_mut_ptr(),
            nulls.as_mut_ptr(),
            1,
            &mut tid,
        )
    });
    assert_eq!(values[0], 11);
    assert!(unsafe {
        fastpg_storage2_scan_next(
            backward_scan,
            0,
            values.as_mut_ptr(),
            nulls.as_mut_ptr(),
            1,
            &mut tid,
        )
    });
    assert_eq!(values[0], 10);
    assert!(!unsafe {
        fastpg_storage2_scan_next(
            backward_scan,
            0,
            values.as_mut_ptr(),
            nulls.as_mut_ptr(),
            1,
            &mut tid,
        )
    });
    assert!(!unsafe {
        fastpg_storage2_scan_next(
            backward_scan,
            0,
            values.as_mut_ptr(),
            nulls.as_mut_ptr(),
            1,
            &mut tid,
        )
    });
    fastpg_storage2_scan_end(backward_scan);
}

#[test]
fn committed_small_transactions_pack_into_relation_pages() {
    let _guard = test_guard();
    let relid = 48;
    let before = fastpg_storage2_metrics_snapshot();
    for value in 0..100 {
        fastpg_storage2_xact_begin();
        insert_i32(relid, value);
        fastpg_storage2_xact_commit();
    }

    let after = fastpg_storage2_metrics_snapshot();
    assert_eq!(fastpg_storage2_relation_row_count(relid), 100);
    assert_eq!(after.page_count.saturating_sub(before.page_count), 1);
    assert!(
        after
            .committed_page_bytes
            .saturating_sub(before.committed_page_bytes)
            < PAGE_SIZE * 2
    );
}

fn fastpg_storage2_metrics_snapshot() -> FastPgStorage2Metrics {
    let mut metrics = FastPgStorage2Metrics::default();
    assert!(unsafe { fastpg_storage2_metrics(&mut metrics) });
    metrics
}
