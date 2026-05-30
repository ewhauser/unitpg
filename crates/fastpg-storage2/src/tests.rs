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
    reset_storage_for_tests();
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

fn update_i32(relid: u32, tid: u64, value: i32) -> u64 {
    let values = [value as usize];
    let nulls = [0u8];
    let byval = [1u8];
    let lens = [0usize];
    let mut new_tid = 0;
    assert!(unsafe {
        fastpg_storage2_relation_update_unchecked(
            relid,
            tid,
            values.as_ptr(),
            nulls.as_ptr(),
            byval.as_ptr(),
            lens.as_ptr(),
            values.len(),
            &mut new_tid,
        )
    });
    new_tid
}

fn update_i32_hot(relid: u32, tid: u64, value: i32) -> u64 {
    let values = [value as usize];
    let nulls = [0u8];
    let byval = [1u8];
    let lens = [0usize];
    let mut new_tid = 0;
    assert!(unsafe {
        fastpg_storage2_relation_update_hot_unchecked(
            relid,
            tid,
            values.as_ptr(),
            nulls.as_ptr(),
            byval.as_ptr(),
            lens.as_ptr(),
            values.len(),
            &mut new_tid,
        )
    });
    new_tid
}

fn scan_i32_values(relid: u32) -> Vec<i32> {
    let scan = fastpg_storage2_scan_begin(relid);
    assert_ne!(scan, 0);
    let mut values = Vec::new();
    loop {
        let mut raw_values = [0usize];
        let mut nulls = [1u8];
        let mut tid = 0u64;
        if !unsafe {
            fastpg_storage2_scan_next(
                scan,
                1,
                raw_values.as_mut_ptr(),
                nulls.as_mut_ptr(),
                1,
                &mut tid,
            )
        } {
            break;
        }
        assert_eq!(nulls[0], 0);
        values.push(raw_values[0] as i32);
    }
    fastpg_storage2_scan_end(scan);
    values
}

fn scan_i32_values_at_cid(relid: u32, curcid: u32) -> Vec<i32> {
    let scan = fastpg_storage2_scan_begin_with_snapshot(relid, curcid);
    assert_ne!(scan, 0);
    let mut values = Vec::new();
    loop {
        let mut raw_values = [0usize];
        let mut nulls = [1u8];
        let mut tid = 0u64;
        if !unsafe {
            fastpg_storage2_scan_next(
                scan,
                1,
                raw_values.as_mut_ptr(),
                nulls.as_mut_ptr(),
                1,
                &mut tid,
            )
        } {
            break;
        }
        assert_eq!(nulls[0], 0);
        values.push(raw_values[0] as i32);
    }
    fastpg_storage2_scan_end(scan);
    values
}

fn scan_i32_values_batch(relid: u32, forward: bool, batch_size: usize) -> Vec<i32> {
    let scan = fastpg_storage2_scan_begin(relid);
    assert_ne!(scan, 0);
    let mut values = Vec::new();
    let mut raw_values = vec![0usize; batch_size * 2];
    let mut nulls = vec![1u8; batch_size * 2];
    let mut tids = vec![0u64; batch_size];
    let mut stored_natts = vec![0usize; batch_size];

    loop {
        let count = unsafe {
            fastpg_storage2_scan_next_batch_with_stored_natts(
                scan,
                u8::from(forward),
                raw_values.as_mut_ptr(),
                nulls.as_mut_ptr(),
                2,
                batch_size,
                tids.as_mut_ptr(),
                stored_natts.as_mut_ptr(),
            )
        };
        if count == 0 {
            break;
        }
        assert!(count <= batch_size);
        for index in 0..count {
            let attr_offset = index * 2;
            assert_ne!(tids[index], 0);
            assert_eq!(stored_natts[index], 1);
            assert_eq!(nulls[attr_offset], 0);
            assert_eq!(nulls[attr_offset + 1], 1);
            values.push(raw_values[attr_offset] as i32);
        }
    }

    fastpg_storage2_scan_end(scan);
    values
}

fn next_scan_block(relid: u32, start_block: u32) -> Option<(u32, u16)> {
    let mut block = 0;
    let mut max_offset = 0;
    if unsafe {
        fastpg_storage2_relation_next_scan_block(relid, start_block, 1, &mut block, &mut max_offset)
    } {
        Some((block, max_offset))
    } else {
        None
    }
}

fn hot_redirect_target(relid: u32, tid: u64) -> Option<u64> {
    let tid = Tid::unpack(tid)?;
    with_storage(|state, _session| {
        state
            .relations
            .get(&relid)?
            .hot_redirects
            .get(&tid)
            .copied()
            .map(Tid::pack)
    })
}

fn update_redirect_target(relid: u32, tid: u64) -> Option<u64> {
    let tid = Tid::unpack(tid)?;
    with_storage(|state, _session| {
        state
            .relations
            .get(&relid)?
            .update_redirects
            .get(&tid)
            .copied()
            .map(Tid::pack)
    })
}

fn byval_key(value: i32) -> IndexKey {
    IndexKey::single(IndexKeyPart::ByValue(value as usize))
}

fn single_column_index_spec(typbyval: bool, typlen: i16) -> UniqueIndexSpec {
    UniqueIndexSpec {
        index_oid: Oid(1),
        relation_oid: Oid(2),
        is_primary: true,
        nulls_not_distinct: false,
        columns: vec![IndexColumnSpec {
            column_index: 0,
            type_oid: TEXT_OID,
            typbyval,
            typlen,
        }],
    }
}

fn transaction_stack_len() -> usize {
    with_session_storage(|session| session.transaction_stack.len())
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
fn fetch_reports_stored_attribute_count_for_missing_attrs() {
    let _guard = test_guard();
    let relid = 464;
    let tid = insert_i32(relid, 7);
    let mut values = [0usize; 2];
    let mut nulls = [0u8; 2];
    let mut stored_natts = 0usize;

    assert!(unsafe {
        fastpg_storage2_fetch_tid_with_stored_natts(
            relid,
            tid,
            values.as_mut_ptr(),
            nulls.as_mut_ptr(),
            values.len(),
            &mut stored_natts,
        )
    });
    assert_eq!(stored_natts, 1);
    assert_eq!(values[0] as i32, 7);
    assert_eq!(nulls, [0, 1]);
}

#[test]
fn fetch_allows_prefix_projection_and_reports_stored_attribute_count() {
    let _guard = test_guard();
    let relid = 467;
    let values = [7usize, 8usize];
    let nulls = [0u8, 0u8];
    let byval = [1u8, 1u8];
    let lens = [0usize, 0usize];
    let mut tid = 0u64;
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

    let mut projected_values = [0usize; 1];
    let mut projected_nulls = [1u8; 1];
    let mut stored_natts = 0usize;
    assert!(unsafe {
        fastpg_storage2_fetch_tid_any_with_stored_natts(
            relid,
            tid,
            projected_values.as_mut_ptr(),
            projected_nulls.as_mut_ptr(),
            projected_values.len(),
            &mut stored_natts,
        )
    });
    assert_eq!(stored_natts, 2);
    assert_eq!(projected_values[0] as i32, 7);
    assert_eq!(projected_nulls[0], 0);
}

#[test]
fn fetch_any_returns_requested_tid_after_hot_update() {
    let _guard = test_guard();
    let relid = 468;
    fastpg_storage2_xact_begin();
    let old_tid = insert_i32(relid, 7);
    let new_tid = update_i32_hot(relid, old_tid, 9);

    let mut values = [0usize; 1];
    let mut nulls = [1u8; 1];
    assert!(unsafe {
        fastpg_storage2_fetch_tid_any(
            relid,
            old_tid,
            values.as_mut_ptr(),
            nulls.as_mut_ptr(),
            values.len(),
        )
    });
    assert_eq!(values[0] as i32, 7);
    assert_eq!(fetch_i32(relid, new_tid), Some(9));
    fastpg_storage2_xact_abort();
}

#[test]
fn index_key_for_short_varlena_datum_uses_one_byte_header_len() {
    let index_spec = single_column_index_spec(false, -1);
    let short_varlena = [5u8, b'1', b'x', b'x'];
    let long_varlena = [20u8, 0, 0, 0, b'1'];
    let values = [short_varlena.as_ptr() as usize];
    let long_values = [long_varlena.as_ptr() as usize];
    let nulls = [0u8];

    let key = index_key_for_key_datums(&index_spec, &values, &nulls);
    let long_key = index_key_for_key_datums(&index_spec, &long_values, &nulls);
    assert_eq!(key, Some(IndexKey::single(IndexKeyPart::Bytes(vec![b'1']))));
    assert_eq!(key, long_key);
}

#[test]
fn bpchar_index_keys_ignore_trailing_spaces() {
    let mut index_spec = single_column_index_spec(false, -1);
    index_spec.columns[0].type_oid = BPCHAR_OID;
    let padded = short_varlena(b"PS.base.a1     ");
    let unpadded = short_varlena(b"PS.base.a1");
    let padded_values = [padded.as_ptr() as usize];
    let unpadded_values = [unpadded.as_ptr() as usize];
    let nulls = [0u8];
    let expected = Some(IndexKey::single(IndexKeyPart::Bytes(
        b"PS.base.a1".to_vec(),
    )));

    assert_eq!(
        index_key_for_key_datums(&index_spec, &padded_values, &nulls),
        expected
    );
    assert_eq!(
        index_key_for_key_datums(&index_spec, &unpadded_values, &nulls),
        expected
    );
    assert_eq!(
        index_key_for_decoded(&index_spec, &[DecodedDatum::ByRef(&padded)]),
        expected
    );
}

fn short_varlena(payload: &[u8]) -> Vec<u8> {
    let total_len = payload.len() + 1;
    assert!(total_len <= 0x7f);
    let mut bytes = Vec::with_capacity(total_len);
    bytes.push(((total_len << 1) | 1) as u8);
    bytes.extend_from_slice(payload);
    bytes
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
fn pending_inserts_respect_max_tuples_per_block() {
    let _guard = test_guard();
    let relid = 456;

    fastpg_storage2_relation_set_max_tuples_per_block(relid, 2);
    fastpg_storage2_xact_begin();
    let first = Tid::unpack(insert_i32(relid, 1)).expect("first tid");
    let second = Tid::unpack(insert_i32(relid, 2)).expect("second tid");
    let third = Tid::unpack(insert_i32(relid, 3)).expect("third tid");

    assert_eq!(
        first,
        Tid {
            block: 0,
            offset: 1,
        }
    );
    assert_eq!(
        second,
        Tid {
            block: 0,
            offset: 2,
        }
    );
    assert_eq!(
        third,
        Tid {
            block: 1,
            offset: 1,
        }
    );
    assert_eq!(scan_i32_values(relid), vec![1, 2, 3]);
    fastpg_storage2_xact_commit();
    assert_eq!(scan_i32_values(relid), vec![1, 2, 3]);
}

#[test]
fn unique_conflict_sees_prior_direct_pending_insert() {
    let _guard = test_guard();
    let relid = 457;
    let index_relid = 4570;
    let attnums = [1i16];
    let typbyval = [1u8];
    let type_oids = [INT4_OID.0];
    let typlen = [4i16];
    let values = [7usize];
    let nulls = [0u8];
    let mut conflict_tid = 0u64;

    fastpg_storage2_relation_set_max_tuples_per_block(relid, 8);
    fastpg_storage2_xact_begin();
    let first_tid = insert_i32(relid, 7);
    let second_tid = insert_i32(relid, 7);

    assert!(unsafe {
        fastpg_storage2_unique_index_conflict_with_spec(
            index_relid,
            relid,
            attnums.as_ptr(),
            type_oids.as_ptr(),
            typbyval.as_ptr(),
            typlen.as_ptr(),
            values.as_ptr(),
            nulls.as_ptr(),
            values.len(),
            0,
            0,
            second_tid,
            &mut conflict_tid,
        )
    });
    assert_eq!(conflict_tid, first_tid);
}

#[test]
fn unique_conflict_ignores_pending_insert_deleted_before_update() {
    let _guard = test_guard();
    let relid = 4571;
    let index_relid = 45710;
    let attnums = [1i16];
    let typbyval = [1u8];
    let type_oids = [INT4_OID.0];
    let typlen = [4i16];
    let values = [7usize];
    let nulls = [0u8];
    let byval = [1u8];
    let lens = [0usize];
    let mut conflict_tid = 0u64;
    let mut updated_tid = 0u64;

    fastpg_storage2_relation_set_max_tuples_per_block(relid, 8);
    fastpg_storage2_xact_begin();
    let first_tid = insert_i32(relid, 7);
    fastpg_storage2_xact_commit();

    fastpg_storage2_xact_begin();
    fastpg_storage2_subxact_begin();
    let speculative_tid = insert_i32(relid, 7);
    assert!(fastpg_storage2_relation_delete_record_primary_key(
        relid,
        speculative_tid,
        0
    ));
    fastpg_storage2_subxact_commit();
    assert!(unsafe {
        fastpg_storage2_relation_update_hot_unchecked_with_metadata(
            relid,
            first_tid,
            1,
            1,
            1,
            1,
            1,
            values.as_ptr(),
            nulls.as_ptr(),
            byval.as_ptr(),
            lens.as_ptr(),
            values.len(),
            &mut updated_tid,
        )
    });

    assert!(!unsafe {
        fastpg_storage2_unique_index_conflict_with_spec(
            index_relid,
            relid,
            attnums.as_ptr(),
            type_oids.as_ptr(),
            typbyval.as_ptr(),
            typlen.as_ptr(),
            values.as_ptr(),
            nulls.as_ptr(),
            values.len(),
            0,
            0,
            updated_tid,
            &mut conflict_tid,
        )
    });
}

#[test]
fn primary_key_conflict_sees_direct_pending_index_entry() {
    let _guard = test_guard();
    let relid = 458;
    let index_relid = 4580;
    let attnums = [1i16];
    let typbyval = [1u8];
    let type_oids = [INT4_OID.0];
    let typlen = [4i16];
    let values = [7usize];
    let nulls = [0u8];
    let mut conflict_tid = 0u64;

    fastpg_storage2_relation_set_max_tuples_per_block(relid, 8);
    fastpg_storage2_xact_begin();
    let first_tid = insert_i32(relid, 7);
    assert!(unsafe {
        fastpg_storage2_primary_key_index_insert_with_spec(
            index_relid,
            relid,
            attnums.as_ptr(),
            type_oids.as_ptr(),
            typbyval.as_ptr(),
            typlen.as_ptr(),
            values.as_ptr(),
            nulls.as_ptr(),
            values.len(),
            first_tid,
        )
    });
    let second_tid = insert_i32(relid, 7);

    assert!(unsafe {
        fastpg_storage2_unique_index_conflict_with_spec(
            index_relid,
            relid,
            attnums.as_ptr(),
            type_oids.as_ptr(),
            typbyval.as_ptr(),
            typlen.as_ptr(),
            values.as_ptr(),
            nulls.as_ptr(),
            values.len(),
            1,
            0,
            second_tid,
            &mut conflict_tid,
        )
    });
    assert_eq!(conflict_tid, first_tid);
}

#[test]
fn generic_index_candidate_lookup_keeps_null_entries() {
    let _guard = test_guard();
    let relid = 459;
    let index_relid = 4590;
    let attnums = [1i16, 2i16];
    let type_oids = [INT4_OID.0, INT4_OID.0];
    let typbyval = [1u8, 1u8];
    let typlen = [4i16, 4i16];
    let values = [500usize, 0usize];
    let nulls = [0u8, 1u8];
    let byval = [1u8, 1u8];
    let lens = [0usize, 0usize];
    let mut tid = 0u64;
    let mut tids = [0u64; 4];
    let mut count = 0usize;

    fastpg_storage2_xact_begin();
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
    assert!(unsafe {
        fastpg_storage2_index_insert_with_spec(
            index_relid,
            relid,
            attnums.as_ptr(),
            type_oids.as_ptr(),
            typbyval.as_ptr(),
            typlen.as_ptr(),
            values.as_ptr(),
            nulls.as_ptr(),
            values.len(),
            tid,
        )
    });
    assert!(unsafe {
        fastpg_storage2_index_candidate_lookup(
            index_relid,
            relid,
            tids.as_mut_ptr(),
            tids.len(),
            &mut count,
        )
    });
    assert_eq!(count, 1);
    assert_eq!(tids[0], tid);
}

#[test]
fn empty_implicit_transactions_stay_session_local() {
    let _guard = test_guard();

    fastpg_storage2_xact_begin_implicit();
    assert_eq!(transaction_stack_len(), 1);
    fastpg_storage2_xact_commit_if_implicit();
    assert_eq!(transaction_stack_len(), 0);

    fastpg_storage2_xact_begin_implicit();
    fastpg_storage2_subxact_begin();
    assert_eq!(transaction_stack_len(), 2);
    fastpg_storage2_xact_abort_if_implicit();
    assert_eq!(transaction_stack_len(), 0);
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
fn aborted_insert_does_not_reuse_ctid() {
    let _guard = test_guard();
    let relid = 431;

    fastpg_storage2_xact_begin_implicit();
    let aborted_tid = insert_i32(relid, 1);
    fastpg_storage2_xact_abort_if_implicit();

    fastpg_storage2_xact_begin_implicit();
    let committed_tid = insert_i32(relid, 2);
    fastpg_storage2_xact_commit_if_implicit();

    assert_ne!(aborted_tid, committed_tid);
    assert_eq!(fetch_i32(relid, aborted_tid), None);
    assert_eq!(fetch_i32(relid, committed_tid), Some(2));
    assert_eq!(fastpg_storage2_relation_row_count(relid), 1);
}

#[test]
fn aborted_append_to_existing_page_does_not_reuse_ctid() {
    let _guard = test_guard();
    let relid = 432;

    fastpg_storage2_xact_begin_implicit();
    let first_tid = insert_i32(relid, 1);
    fastpg_storage2_xact_commit_if_implicit();

    fastpg_storage2_xact_begin_implicit();
    let aborted_tid = insert_i32(relid, 2);
    fastpg_storage2_xact_abort_if_implicit();

    fastpg_storage2_xact_begin_implicit();
    let committed_tid = insert_i32(relid, 3);
    fastpg_storage2_xact_commit_if_implicit();

    assert_ne!(aborted_tid, committed_tid);
    assert_eq!(fetch_i32(relid, first_tid), Some(1));
    assert_eq!(fetch_i32(relid, aborted_tid), None);
    assert_eq!(fetch_i32(relid, committed_tid), Some(3));
    assert_eq!(fastpg_storage2_relation_row_count(relid), 2);
}

#[test]
fn current_session_invalidates_tid_tracks_update_overlay() {
    let _guard = test_guard();
    let relid = 433;

    fastpg_storage2_xact_begin();
    let old_tid = insert_i32(relid, 1);
    fastpg_storage2_xact_commit();

    fastpg_storage2_xact_begin();
    let new_tid = update_i32(relid, old_tid, 2);

    assert!(fastpg_storage2_relation_current_session_invalidates_tid(
        relid, old_tid
    ));
    assert!(!fastpg_storage2_relation_current_session_invalidates_tid(
        relid, new_tid
    ));
}

#[test]
fn next_scan_block_skips_aborted_physical_only_blocks() {
    let _guard = test_guard();
    let relid = 469;

    fastpg_storage2_xact_begin_implicit();
    let aborted_tid = Tid::unpack(insert_i32(relid, 1)).expect("aborted tid");
    fastpg_storage2_xact_abort_if_implicit();
    assert_eq!(fetch_i32(relid, aborted_tid.pack()), None);
    assert_eq!(next_scan_block(relid, 0), None);

    fastpg_storage2_xact_begin_implicit();
    let committed_tid = insert_i32(relid, 2);
    fastpg_storage2_xact_commit_if_implicit();
    let committed_tid = Tid::unpack(committed_tid).expect("committed tid");
    assert_eq!(
        next_scan_block(relid, 0),
        Some((committed_tid.block, committed_tid.offset))
    );

    fastpg_storage2_xact_begin_implicit();
    let pending_tid = Tid::unpack(insert_i32(relid, 3)).expect("pending tid");
    let pending_block = next_scan_block(relid, committed_tid.block).expect("pending block");
    assert_eq!(pending_block.0, pending_tid.block);
    assert!(pending_block.1 >= pending_tid.offset);
    fastpg_storage2_xact_abort_if_implicit();
    assert_eq!(fetch_i32(relid, pending_tid.pack()), None);
    let after_abort = next_scan_block(relid, committed_tid.block).expect("committed block");
    assert_eq!(after_abort.0, committed_tid.block);
    assert!(after_abort.1 >= committed_tid.offset);
}

#[test]
fn abort_drops_only_own_pending_overlay() {
    let _guard = test_guard();
    let relid = 433;
    let session_a = new_session_storage();
    let session_b = new_session_storage();

    let tid_a = {
        let _session_guard = enter_session_storage(session_a.clone());
        fastpg_storage2_xact_begin();
        insert_i32(relid, 1)
    };
    let tid_b = {
        let _session_guard = enter_session_storage(session_b.clone());
        fastpg_storage2_xact_begin();
        insert_i32(relid, 2)
    };

    {
        let _session_guard = enter_session_storage(session_a);
        assert_eq!(fetch_i32(relid, tid_a), Some(1));
        fastpg_storage2_xact_abort();
        assert_eq!(fetch_i32(relid, tid_a), None);
    }

    {
        let _session_guard = enter_session_storage(session_b);
        assert_eq!(fetch_i32(relid, tid_b), Some(2));
        fastpg_storage2_xact_commit();
        assert_eq!(fetch_i32(relid, tid_b), Some(2));
    }

    assert_eq!(fetch_i32(relid, tid_a), None);
    assert_eq!(fetch_i32(relid, tid_b), Some(2));
    assert_eq!(fastpg_storage2_relation_row_count(relid), 1);
}

#[test]
fn index_tid_all_dead_ignores_pending_and_live_tids() {
    let _guard = test_guard();
    let relid = 434;

    fastpg_storage2_xact_begin();
    let pending_tid = insert_i32(relid, 1);
    assert!(!fastpg_storage2_relation_index_tid_all_dead(
        relid,
        pending_tid
    ));

    {
        let other_session = new_session_storage();
        let _other_guard = enter_session_storage(other_session);
        assert!(!fastpg_storage2_relation_index_tid_all_dead(
            relid,
            pending_tid
        ));
    }

    fastpg_storage2_xact_abort();
    assert!(fastpg_storage2_relation_index_tid_all_dead(
        relid,
        pending_tid
    ));

    fastpg_storage2_xact_begin();
    let live_tid = insert_i32(relid, 2);
    fastpg_storage2_xact_commit();
    assert!(!fastpg_storage2_relation_index_tid_all_dead(
        relid, live_tid
    ));
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
fn hot_update_redirects_old_tid_to_committed_new_tid() {
    let _guard = test_guard();
    let relid = 145;
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
        fastpg_storage2_relation_update_hot_unchecked(
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
    assert_eq!(fetch_i32(relid, old_tid), Some(4));
    assert_eq!(fetch_i32(relid, new_tid), Some(4));
    fastpg_storage2_xact_commit();

    assert_eq!(fetch_i32(relid, old_tid), Some(4));
    assert_eq!(fetch_i32(relid, new_tid), Some(4));
}

#[test]
fn committed_hot_update_scan_returns_only_latest_version() {
    let _guard = test_guard();
    let relid = 245;
    fastpg_storage2_xact_begin();
    let old_tid = insert_i32(relid, 3);
    let second_tid = insert_i32(relid, 7);
    fastpg_storage2_xact_commit();

    let values = [4usize];
    let nulls = [0u8];
    let byval = [1u8];
    let lens = [0usize];
    let mut new_tid = 0;
    fastpg_storage2_xact_begin();
    assert!(unsafe {
        fastpg_storage2_relation_update_hot_unchecked(
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
    fastpg_storage2_xact_commit();

    assert_eq!(scan_i32_values(relid), vec![7, 4]);

    let scan = fastpg_storage2_scan_begin(relid);
    assert_ne!(scan, 0);
    let mut raw_values = [0usize];
    let mut nulls = [1u8];
    let mut output_tid = 0u64;
    let mut cursor_tid = 0u64;
    let mut stored_natts = 0usize;
    assert!(unsafe {
        fastpg_storage2_scan_next_with_cursor_tid_and_stored_natts(
            scan,
            1,
            raw_values.as_mut_ptr(),
            nulls.as_mut_ptr(),
            1,
            &mut output_tid,
            &mut cursor_tid,
            &mut stored_natts,
        )
    });
    assert_eq!(raw_values[0] as i32, 7);
    assert_eq!(output_tid, second_tid);
    assert_eq!(cursor_tid, second_tid);
    assert_eq!(stored_natts, 1);
    assert!(unsafe {
        fastpg_storage2_scan_next_with_cursor_tid_and_stored_natts(
            scan,
            1,
            raw_values.as_mut_ptr(),
            nulls.as_mut_ptr(),
            1,
            &mut output_tid,
            &mut cursor_tid,
            &mut stored_natts,
        )
    });
    assert_eq!(raw_values[0] as i32, 4);
    assert_eq!(output_tid, new_tid);
    assert_eq!(cursor_tid, old_tid);
    fastpg_storage2_scan_end(scan);

    fastpg_storage2_xact_begin();
    insert_i32(relid, 9);
    assert_eq!(scan_i32_values(relid), vec![4, 7, 9]);
    fastpg_storage2_xact_abort();
}

#[test]
fn in_transaction_hot_update_scan_keeps_root_order() {
    let _guard = test_guard();
    let relid = 345;
    fastpg_storage2_xact_begin();
    let first_tid = insert_i32(relid, 1);
    let second_tid = insert_i32(relid, 5);
    fastpg_storage2_xact_commit();

    fastpg_storage2_xact_begin();
    let new_tid = update_i32_hot(relid, first_tid, 10);
    assert_ne!(first_tid, new_tid);

    assert_eq!(scan_i32_values(relid), vec![10, 5]);
    assert_eq!(scan_i32_values_batch(relid, true, 8), vec![10, 5]);
    assert_eq!(scan_i32_values_batch(relid, false, 8), vec![5, 10]);

    let scan = fastpg_storage2_scan_begin(relid);
    assert_ne!(scan, 0);

    let mut raw_values = [0usize];
    let mut nulls = [1u8];
    let mut output_tid = 0u64;
    let mut cursor_tid = 0u64;
    let mut stored_natts = 0usize;
    assert!(unsafe {
        fastpg_storage2_scan_next_with_cursor_tid_and_stored_natts(
            scan,
            1,
            raw_values.as_mut_ptr(),
            nulls.as_mut_ptr(),
            1,
            &mut output_tid,
            &mut cursor_tid,
            &mut stored_natts,
        )
    });
    assert_eq!(raw_values[0] as i32, 10);
    assert_eq!(output_tid, new_tid);
    assert_eq!(cursor_tid, first_tid);
    assert_eq!(stored_natts, 1);

    assert!(unsafe {
        fastpg_storage2_scan_next_with_cursor_tid_and_stored_natts(
            scan,
            1,
            raw_values.as_mut_ptr(),
            nulls.as_mut_ptr(),
            1,
            &mut output_tid,
            &mut cursor_tid,
            &mut stored_natts,
        )
    });
    assert_eq!(raw_values[0] as i32, 5);
    assert_eq!(output_tid, second_tid);
    assert_eq!(cursor_tid, second_tid);
    assert_eq!(stored_natts, 1);

    assert!(!unsafe {
        fastpg_storage2_scan_next_with_cursor_tid_and_stored_natts(
            scan,
            1,
            raw_values.as_mut_ptr(),
            nulls.as_mut_ptr(),
            1,
            &mut output_tid,
            &mut cursor_tid,
            &mut stored_natts,
        )
    });
    fastpg_storage2_scan_end(scan);
}

#[test]
fn in_transaction_hot_update_scan_at_later_cid_returns_only_latest_version() {
    let _guard = test_guard();
    let relid = 246;
    fastpg_storage2_xact_begin();
    let old_tid = insert_i32(relid, 3);
    assert!(fastpg_storage2_relation_record_insert_metadata(
        relid, old_tid, 1, 0
    ));

    let values = [4usize];
    let nulls = [0u8];
    let byval = [1u8];
    let lens = [0usize];
    let mut new_tid = 0;
    assert!(unsafe {
        fastpg_storage2_relation_update_hot_unchecked(
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
    assert!(fastpg_storage2_relation_record_invalidate_metadata(
        relid, old_tid, 1, 1
    ));
    assert!(fastpg_storage2_relation_record_insert_metadata(
        relid, new_tid, 1, 1
    ));

    assert_eq!(scan_i32_values_at_cid(relid, 2), vec![4]);
}

#[test]
fn in_transaction_chained_hot_update_uses_latest_source_tuple() {
    let _guard = test_guard();
    let relid = 247;
    fastpg_storage2_xact_begin();
    let first_tid = insert_i32(relid, 3);
    assert!(fastpg_storage2_relation_record_insert_metadata(
        relid, first_tid, 1, 0
    ));

    let nulls = [0u8];
    let byval = [1u8];
    let lens = [0usize];
    let values = [4usize];
    let mut second_tid = 0;
    assert!(unsafe {
        fastpg_storage2_relation_update_hot_unchecked(
            relid,
            first_tid,
            values.as_ptr(),
            nulls.as_ptr(),
            byval.as_ptr(),
            lens.as_ptr(),
            values.len(),
            &mut second_tid,
        )
    });
    assert!(fastpg_storage2_relation_record_invalidate_metadata(
        relid, first_tid, 1, 1
    ));
    assert!(fastpg_storage2_relation_record_insert_metadata(
        relid, second_tid, 1, 1
    ));

    let values = [5usize];
    let mut third_tid = 0;
    assert!(unsafe {
        fastpg_storage2_relation_update_hot_unchecked(
            relid,
            first_tid,
            values.as_ptr(),
            nulls.as_ptr(),
            byval.as_ptr(),
            lens.as_ptr(),
            values.len(),
            &mut third_tid,
        )
    });
    assert!(fastpg_storage2_relation_record_invalidate_metadata(
        relid, second_tid, 1, 2
    ));
    assert!(fastpg_storage2_relation_record_insert_metadata(
        relid, third_tid, 1, 2
    ));

    assert_eq!(fetch_i32(relid, first_tid), Some(5));
    assert_eq!(scan_i32_values_at_cid(relid, 3), vec![5]);
}

#[test]
fn scan_open_before_non_hot_update_can_follow_update_redirect() {
    let _guard = test_guard();
    let relid = 248;
    fastpg_storage2_xact_begin();
    let old_tid = insert_i32(relid, 3);
    assert!(fastpg_storage2_relation_record_insert_metadata(
        relid, old_tid, 1, 0
    ));

    let scan = fastpg_storage2_scan_begin_with_snapshot(relid, 2);
    assert_ne!(scan, 0);

    let values = [4usize];
    let nulls = [0u8];
    let byval = [1u8];
    let lens = [0usize];
    let mut new_tid = 0;
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
    assert!(fastpg_storage2_relation_record_invalidate_metadata(
        relid, old_tid, 1, 1
    ));
    assert!(fastpg_storage2_relation_record_insert_metadata(
        relid, new_tid, 1, 1
    ));

    let mut raw_values = [0usize];
    let mut nulls = [1u8];
    let mut found_tid = 0u64;
    assert!(unsafe {
        fastpg_storage2_scan_next(
            scan,
            1,
            raw_values.as_mut_ptr(),
            nulls.as_mut_ptr(),
            1,
            &mut found_tid,
        )
    });
    assert_eq!(found_tid, new_tid);
    assert_eq!(raw_values[0] as i32, 4);
    assert!(!unsafe {
        fastpg_storage2_scan_next(
            scan,
            1,
            raw_values.as_mut_ptr(),
            nulls.as_mut_ptr(),
            1,
            &mut found_tid,
        )
    });
    fastpg_storage2_scan_end(scan);
}

#[test]
fn scan_open_after_non_hot_update_returns_latest_once() {
    let _guard = test_guard();
    let relid = 249;
    fastpg_storage2_xact_begin();
    let old_tid = insert_i32(relid, 3);
    fastpg_storage2_xact_commit();

    fastpg_storage2_xact_begin();
    let new_tid = update_i32(relid, old_tid, 4);
    assert_ne!(old_tid, new_tid);

    assert_eq!(scan_i32_values(relid), vec![4]);
    assert_eq!(scan_i32_values_batch(relid, true, 8), vec![4]);
}

#[test]
fn hot_update_redirects_follow_long_committed_chains() {
    let _guard = test_guard();
    let relid = 146;
    fastpg_storage2_xact_begin();
    let first_tid = insert_i32(relid, 0);
    fastpg_storage2_xact_commit();

    let nulls = [0u8];
    let byval = [1u8];
    let lens = [0usize];
    let mut current_tid = first_tid;
    for value in 1..=96usize {
        let values = [value];
        let mut new_tid = 0;
        fastpg_storage2_xact_begin();
        assert!(unsafe {
            fastpg_storage2_relation_update_hot_unchecked(
                relid,
                current_tid,
                values.as_ptr(),
                nulls.as_ptr(),
                byval.as_ptr(),
                lens.as_ptr(),
                values.len(),
                &mut new_tid,
            )
        });
        fastpg_storage2_xact_commit();
        current_tid = new_tid;
    }

    let mut resolved_tid = 0u64;
    assert!(unsafe { fastpg_storage2_relation_resolve_tid(relid, first_tid, &mut resolved_tid) });
    assert_eq!(resolved_tid, current_tid);
    assert_eq!(fetch_i32(relid, first_tid), Some(96));
    assert_eq!(fetch_i32(relid, current_tid), Some(96));
    assert_eq!(hot_redirect_target(relid, first_tid), Some(current_tid));
    assert!(!fastpg_storage2_relation_tid_is_hot_redirect_target(
        relid, first_tid
    ));
    assert!(fastpg_storage2_relation_tid_is_hot_redirect_target(
        relid,
        current_tid
    ));
    let mut root_from_target = 0;
    assert!(unsafe {
        fastpg_storage2_relation_hot_root_tid(relid, current_tid, &mut root_from_target)
    });
    assert_eq!(root_from_target, first_tid);
}

#[test]
fn hot_redirect_target_detection_sees_current_overlay() {
    let _guard = test_guard();
    let relid = 152;
    fastpg_storage2_xact_begin();
    let root_tid = insert_i32(relid, 10);
    fastpg_storage2_xact_commit();

    let values = [10usize];
    let nulls = [0u8];
    let byval = [1u8];
    let lens = [0usize];
    let mut hot_tid = 0;
    fastpg_storage2_xact_begin();
    assert!(unsafe {
        fastpg_storage2_relation_update_hot_unchecked(
            relid,
            root_tid,
            values.as_ptr(),
            nulls.as_ptr(),
            byval.as_ptr(),
            lens.as_ptr(),
            values.len(),
            &mut hot_tid,
        )
    });

    assert!(!fastpg_storage2_relation_tid_is_hot_redirect_target(
        relid, root_tid
    ));
    assert!(fastpg_storage2_relation_tid_is_hot_redirect_target(
        relid, hot_tid
    ));
    let mut root_from_target = 0;
    assert!(unsafe {
        fastpg_storage2_relation_hot_root_tid(relid, hot_tid, &mut root_from_target)
    });
    assert_eq!(root_from_target, root_tid);
    fastpg_storage2_xact_abort();
}

#[test]
fn hot_redirect_read_resolution_does_not_compress_committed_chains() {
    let _guard = test_guard();
    let relid = 147;
    fastpg_storage2_xact_begin();
    let first_tid = insert_i32(relid, 0);
    fastpg_storage2_xact_commit();

    let nulls = [0u8];
    let byval = [1u8];
    let lens = [0usize];
    let mut current_tid = first_tid;
    let mut second_tid = None;
    for value in 1..=4usize {
        let values = [value];
        let mut new_tid = 0;
        fastpg_storage2_xact_begin();
        assert!(unsafe {
            fastpg_storage2_relation_update_hot_unchecked(
                relid,
                current_tid,
                values.as_ptr(),
                nulls.as_ptr(),
                byval.as_ptr(),
                lens.as_ptr(),
                values.len(),
                &mut new_tid,
            )
        });
        fastpg_storage2_xact_commit();
        if second_tid.is_none() {
            second_tid = Some(new_tid);
        }
        current_tid = new_tid;
    }

    let mut resolved_tid = 0u64;
    assert!(unsafe {
        fastpg_storage2_relation_resolve_tid_read(relid, first_tid, &mut resolved_tid)
    });
    assert_eq!(resolved_tid, current_tid);
    assert_eq!(hot_redirect_target(relid, first_tid), second_tid);
}

#[test]
fn current_session_visible_tid_fetches_pending_overlay_rows() {
    let _guard = test_guard();
    let relid = 157;
    let values = [7usize];
    let nulls = [0u8];
    let byval = [1u8];
    let lens = [0usize];
    let mut tid = 0u64;
    let mut resolved_tid = 0u64;
    let mut fetched_values = [0usize];
    let mut fetched_nulls = [1u8];
    let mut stored_natts = 0usize;

    fastpg_storage2_xact_begin();
    assert!(unsafe {
        fastpg_storage2_relation_insert_unchecked_with_metadata(
            relid,
            1,
            1,
            values.as_ptr(),
            nulls.as_ptr(),
            byval.as_ptr(),
            lens.as_ptr(),
            values.len(),
            &mut tid,
        )
    });

    assert!(unsafe {
        fastpg_storage2_relation_current_session_visible_tid(relid, tid, 0, 0, &mut resolved_tid)
    });
    assert_eq!(resolved_tid, tid);
    assert!(!unsafe {
        fastpg_storage2_fetch_current_session_tid_with_stored_natts(
            relid,
            tid,
            1,
            1,
            fetched_values.as_mut_ptr(),
            fetched_nulls.as_mut_ptr(),
            fetched_values.len(),
            &mut stored_natts,
            std::ptr::null_mut(),
        )
    });
    assert!(unsafe {
        fastpg_storage2_fetch_current_session_tid_with_stored_natts(
            relid,
            tid,
            1,
            2,
            fetched_values.as_mut_ptr(),
            fetched_nulls.as_mut_ptr(),
            fetched_values.len(),
            &mut stored_natts,
            &mut resolved_tid,
        )
    });
    assert_eq!(resolved_tid, tid);
    assert_eq!(stored_natts, 1);
    assert_eq!(fetched_nulls[0], 0);
    assert_eq!(fetched_values[0], 7);
}

#[test]
fn current_session_hot_visible_tid_only_follows_hot_redirects() {
    let _guard = test_guard();
    let relid = 520;
    let mut resolved_tid = 0u64;

    fastpg_storage2_xact_begin();
    let root_tid = insert_i32(relid, 1);
    fastpg_storage2_xact_commit();

    fastpg_storage2_xact_begin();
    let non_hot_tid = update_i32(relid, root_tid, 2);
    assert!(unsafe {
        fastpg_storage2_relation_current_session_visible_tid(
            relid,
            root_tid,
            0,
            0,
            &mut resolved_tid,
        )
    });
    assert_eq!(resolved_tid, non_hot_tid);
    assert!(!unsafe {
        fastpg_storage2_relation_current_session_hot_visible_tid(
            relid,
            root_tid,
            0,
            0,
            &mut resolved_tid,
        )
    });
    assert!(unsafe {
        fastpg_storage2_relation_current_session_hot_visible_tid(
            relid,
            non_hot_tid,
            0,
            0,
            &mut resolved_tid,
        )
    });
    assert_eq!(resolved_tid, non_hot_tid);
    fastpg_storage2_xact_abort();

    fastpg_storage2_xact_begin();
    let hot_tid = update_i32_hot(relid, root_tid, 3);
    assert!(unsafe {
        fastpg_storage2_relation_current_session_hot_visible_tid(
            relid,
            root_tid,
            0,
            0,
            &mut resolved_tid,
        )
    });
    assert_eq!(resolved_tid, hot_tid);
}

#[test]
fn update_redirect_resolution_compresses_long_committed_chains() {
    let _guard = test_guard();
    let relid = 148;
    fastpg_storage2_xact_begin();
    let first_tid = insert_i32(relid, 0);
    fastpg_storage2_xact_commit();

    let nulls = [0u8];
    let byval = [1u8];
    let lens = [0usize];
    let mut current_tid = first_tid;
    let mut tids = vec![first_tid];
    for value in 1..=96usize {
        let values = [value];
        let mut new_tid = 0;
        fastpg_storage2_xact_begin();
        assert!(unsafe {
            fastpg_storage2_relation_update_unchecked(
                relid,
                current_tid,
                values.as_ptr(),
                nulls.as_ptr(),
                byval.as_ptr(),
                lens.as_ptr(),
                values.len(),
                &mut new_tid,
            )
        });
        fastpg_storage2_xact_commit();
        current_tid = new_tid;
        tids.push(new_tid);
    }

    let mut resolved_tid = 0u64;
    assert!(unsafe {
        fastpg_storage2_relation_resolve_update_tid(relid, first_tid, &mut resolved_tid)
    });
    assert_eq!(resolved_tid, current_tid);
    assert_eq!(update_redirect_target(relid, tids[32]), Some(current_tid));
    assert_eq!(update_redirect_target(relid, tids[64]), Some(current_tid));
    with_storage_read(|state, _session| {
        let relation = state.relations.get(&relid).expect("relation exists");
        let first_tid = Tid::unpack(first_tid).expect("packed first tid");
        let current_tid = Tid::unpack(current_tid).expect("packed current tid");
        assert_eq!(
            relation.update_redirects.get(&first_tid),
            Some(&current_tid)
        );
    });
}

#[test]
fn primary_lookup_with_spec_uses_primary_index_not_full_scan() {
    let _guard = test_guard();
    let relid = 149;
    let index_relid = 1490;
    fastpg_storage2_xact_begin();
    let tid = insert_i32(relid, 20);
    fastpg_storage2_xact_commit();

    with_storage(|state, _session| {
        state
            .relation_mut(relid)
            .primary_key_index
            .insert(byval_key(10), Tid::unpack(tid).expect("packed tid"));
    });

    let attnums = [1i16];
    let typbyval = [1u8];
    let type_oids = [INT4_OID.0];
    let typlen = [4i16];
    let values = [10usize];
    let nulls = [0u8];
    let mut found_tid = 0u64;

    assert!(unsafe {
        fastpg_storage2_primary_key_index_lookup_with_spec(
            index_relid,
            relid,
            attnums.as_ptr(),
            type_oids.as_ptr(),
            typbyval.as_ptr(),
            typlen.as_ptr(),
            values.as_ptr(),
            nulls.as_ptr(),
            values.len(),
            &mut found_tid,
        )
    });
    assert_eq!(found_tid, tid);
}

#[test]
fn hot_primary_lookup_keeps_primary_index_at_root_tid() {
    let _guard = test_guard();
    let relid = 150;
    let index_relid = 1500;
    fastpg_storage2_xact_begin();
    let root_tid = insert_i32(relid, 10);
    fastpg_storage2_xact_commit();

    with_storage(|state, _session| {
        state.relation_mut(relid).primary_key_index.insert(
            byval_key(10),
            Tid::unpack(root_tid).expect("packed root tid"),
        );
    });

    let values = [20usize];
    let nulls = [0u8];
    let byval = [1u8];
    let lens = [0usize];
    let mut hot_tid = 0;
    fastpg_storage2_xact_begin();
    assert!(unsafe {
        fastpg_storage2_relation_update_hot_if_single_byval_preserved_with_metadata(
            relid,
            root_tid,
            1,
            10,
            0,
            1,
            0,
            1,
            1,
            1,
            values.as_ptr(),
            nulls.as_ptr(),
            byval.as_ptr(),
            lens.as_ptr(),
            values.len(),
            &mut hot_tid,
            std::ptr::null_mut(),
        )
    });
    fastpg_storage2_xact_commit();

    with_storage_read(|state, _session| {
        let relation = state.relations.get(&relid).expect("relation");
        assert_eq!(
            relation.primary_key_index.get(&byval_key(10)),
            Some(&Tid::unpack(root_tid).expect("packed root tid"))
        );
    });

    let attnums = [1i16];
    let typbyval = [1u8];
    let type_oids = [INT4_OID.0];
    let typlen = [4i16];
    let lookup_values = [10usize];
    let lookup_nulls = [0u8];
    let mut found_tid = 0u64;

    assert!(unsafe {
        fastpg_storage2_primary_key_index_lookup_with_spec(
            index_relid,
            relid,
            attnums.as_ptr(),
            type_oids.as_ptr(),
            typbyval.as_ptr(),
            typlen.as_ptr(),
            lookup_values.as_ptr(),
            lookup_nulls.as_ptr(),
            lookup_values.len(),
            &mut found_tid,
        )
    });
    assert_eq!(found_tid, hot_tid);
}

#[test]
fn current_transaction_primary_lookup_follows_preserved_key_update() {
    let _guard = test_guard();
    let relid = 152;
    let index_relid = 1520;
    let attnums = [1i16];
    let typbyval = [1u8];
    let type_oids = [INT4_OID.0];
    let typlen = [4i16];
    let key_values = [1usize];
    let nulls = [0u8];
    let byval = [1u8, 1u8];
    let lens = [0usize, 0usize];
    let mut root_tid = 0;
    let mut hot_tid = 0;
    let mut found_tid = 0;

    fastpg_storage2_xact_begin();
    let root_values = [1usize, 10usize];
    assert!(unsafe {
        fastpg_storage2_relation_insert_unchecked(
            relid,
            root_values.as_ptr(),
            nulls.as_ptr(),
            byval.as_ptr(),
            lens.as_ptr(),
            root_values.len(),
            &mut root_tid,
        )
    });
    assert!(unsafe {
        fastpg_storage2_primary_key_index_insert_with_spec(
            index_relid,
            relid,
            attnums.as_ptr(),
            type_oids.as_ptr(),
            typbyval.as_ptr(),
            typlen.as_ptr(),
            key_values.as_ptr(),
            nulls.as_ptr(),
            key_values.len(),
            root_tid,
        )
    });

    let updated_values = [1usize, 20usize];
    assert!(unsafe {
        fastpg_storage2_relation_update_hot_if_single_byval_preserved_with_metadata(
            relid,
            root_tid,
            1,
            1,
            0,
            1,
            0,
            1,
            1,
            1,
            updated_values.as_ptr(),
            nulls.as_ptr(),
            byval.as_ptr(),
            lens.as_ptr(),
            updated_values.len(),
            &mut hot_tid,
            std::ptr::null_mut(),
        )
    });
    assert_ne!(root_tid, hot_tid);
    assert_eq!(fastpg_storage2_relation_row_xmin(relid, hot_tid), 1);
    assert_eq!(fastpg_storage2_relation_row_xmax(relid, hot_tid), 1);

    assert!(unsafe {
        fastpg_storage2_primary_key_index_lookup_with_spec(
            index_relid,
            relid,
            attnums.as_ptr(),
            type_oids.as_ptr(),
            typbyval.as_ptr(),
            typlen.as_ptr(),
            key_values.as_ptr(),
            nulls.as_ptr(),
            key_values.len(),
            &mut found_tid,
        )
    });
    assert_eq!(found_tid, hot_tid);
    found_tid = 0;
    assert!(unsafe {
        fastpg_storage2_primary_key_index_lookup_single_byval_with_spec(
            index_relid,
            relid,
            1,
            0,
            &mut found_tid,
        )
    });
    assert_eq!(found_tid, hot_tid);
    fastpg_storage2_xact_abort();
}

#[test]
fn explicitly_recorded_primary_key_survives_pending_tid_commit_remap() {
    let _guard = test_guard();
    let relid = 153;
    let index_relid = 1530;
    let attnums = [1i16];
    let typbyval = [1u8];
    let type_oids = [INT4_OID.0];
    let typlen = [4i16];
    let key_values = [2usize];
    let nulls = [0u8];
    let byval = [1u8, 1u8];
    let lens = [0usize, 0usize];
    let values = [2usize, 20usize];
    let mut inserted_tid = 0;
    let mut conflict_tid = 0;

    fastpg_storage2_xact_begin();
    let _prior_tid = insert_i32(relid, 1);
    fastpg_storage2_xact_commit();

    fastpg_storage2_xact_begin();
    assert!(unsafe {
        fastpg_storage2_relation_insert_unchecked_with_metadata_record_primary_key(
            relid,
            1,
            1,
            values.as_ptr(),
            nulls.as_ptr(),
            byval.as_ptr(),
            lens.as_ptr(),
            values.len(),
            &mut inserted_tid,
            0,
        )
    });
    assert!(!unsafe {
        fastpg_storage2_unique_index_conflict_with_spec(
            index_relid,
            relid,
            attnums.as_ptr(),
            type_oids.as_ptr(),
            typbyval.as_ptr(),
            typlen.as_ptr(),
            key_values.as_ptr(),
            nulls.as_ptr(),
            key_values.len(),
            1,
            0,
            inserted_tid,
            &mut conflict_tid,
        )
    });
    assert!(unsafe {
        fastpg_storage2_primary_key_index_insert_with_spec(
            index_relid,
            relid,
            attnums.as_ptr(),
            type_oids.as_ptr(),
            typbyval.as_ptr(),
            typlen.as_ptr(),
            key_values.as_ptr(),
            nulls.as_ptr(),
            key_values.len(),
            inserted_tid,
        )
    });
    fastpg_storage2_xact_commit();

    fastpg_storage2_xact_begin();
    let mut second_tid = 0;
    assert!(unsafe {
        fastpg_storage2_relation_insert_unchecked_with_metadata_record_primary_key(
            relid,
            2,
            1,
            values.as_ptr(),
            nulls.as_ptr(),
            byval.as_ptr(),
            lens.as_ptr(),
            values.len(),
            &mut second_tid,
            0,
        )
    });
    assert!(unsafe {
        fastpg_storage2_unique_index_conflict_with_spec(
            index_relid,
            relid,
            attnums.as_ptr(),
            type_oids.as_ptr(),
            typbyval.as_ptr(),
            typlen.as_ptr(),
            key_values.as_ptr(),
            nulls.as_ptr(),
            key_values.len(),
            1,
            0,
            second_tid,
            &mut conflict_tid,
        )
    });
    assert_ne!(conflict_tid, second_tid);
    fastpg_storage2_xact_abort();
}

#[test]
fn primary_lookup_compresses_hot_redirect_chain_from_root_tid() {
    let _guard = test_guard();
    let relid = 151;
    let index_relid = 1510;
    fastpg_storage2_xact_begin();
    let root_tid = insert_i32(relid, 10);
    fastpg_storage2_xact_commit();

    with_storage(|state, _session| {
        state.relation_mut(relid).primary_key_index.insert(
            byval_key(10),
            Tid::unpack(root_tid).expect("packed root tid"),
        );
    });

    let nulls = [0u8];
    let byval = [1u8];
    let lens = [0usize];
    let mut current_tid = root_tid;
    let mut tids = vec![root_tid];
    for _ in 0..96usize {
        let values = [10usize];
        let mut hot_tid = 0;
        fastpg_storage2_xact_begin();
        assert!(unsafe {
            fastpg_storage2_relation_update_hot_if_single_byval_preserved_with_metadata(
                relid,
                current_tid,
                1,
                10,
                0,
                1,
                0,
                1,
                1,
                1,
                values.as_ptr(),
                nulls.as_ptr(),
                byval.as_ptr(),
                lens.as_ptr(),
                values.len(),
                &mut hot_tid,
                std::ptr::null_mut(),
            )
        });
        fastpg_storage2_xact_commit();
        current_tid = hot_tid;
        tids.push(hot_tid);
    }

    let mut found_tid = 0u64;
    assert!(unsafe {
        fastpg_storage2_primary_key_index_lookup_single_byval_with_spec(
            index_relid,
            relid,
            10,
            0,
            &mut found_tid,
        )
    });
    assert_eq!(found_tid, current_tid);
    assert_eq!(hot_redirect_target(relid, root_tid), Some(current_tid));
    assert_eq!(hot_redirect_target(relid, tids[32]), Some(current_tid));
    assert_eq!(hot_redirect_target(relid, tids[64]), Some(current_tid));
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
fn savepoint_commit_preserves_nested_insert_after_relation_clear() {
    let _guard = test_guard();
    let relid = 462;
    fastpg_storage2_xact_begin();

    fastpg_storage2_subxact_begin();
    fastpg_storage2_relation_clear(relid);
    fastpg_storage2_subxact_begin();
    let nested_tid = insert_i32(relid, 7);
    fastpg_storage2_subxact_commit();
    fastpg_storage2_subxact_commit();

    assert_eq!(fetch_i32(relid, nested_tid), Some(7));
    fastpg_storage2_xact_commit();
    assert_eq!(fetch_i32(relid, nested_tid), Some(7));
}

#[test]
fn transactional_clear_restarts_tid_space() {
    let _guard = test_guard();
    let relid = 465;

    fastpg_storage2_xact_begin();
    let old_tid = insert_i32(relid, 1);
    fastpg_storage2_xact_commit();

    fastpg_storage2_xact_begin();
    fastpg_storage2_relation_clear(relid);
    let new_tid = insert_i32(relid, 2);

    assert_eq!(new_tid, old_tid);
    assert_eq!(fetch_i32(relid, old_tid), Some(2));
    assert_eq!(fastpg_storage2_relation_row_count(relid), 1);
    assert_eq!(scan_i32_values(relid), vec![2]);
    fastpg_storage2_xact_commit();

    assert_eq!(fetch_i32(relid, old_tid), Some(2));
    assert_eq!(scan_i32_values(relid), vec![2]);
}

#[test]
fn update_after_transactional_clear_invalidates_reused_tid() {
    let _guard = test_guard();
    let relid = 466;

    fastpg_storage2_xact_begin();
    let old_tid = insert_i32(relid, 1);
    fastpg_storage2_xact_commit();

    fastpg_storage2_xact_begin();
    fastpg_storage2_relation_clear(relid);
    let reused_tid = insert_i32(relid, 2);
    assert_eq!(reused_tid, old_tid);
    assert!(fastpg_storage2_relation_record_insert_metadata(
        relid, reused_tid, 1, 1
    ));

    let updated_tid = update_i32(relid, reused_tid, 3);
    assert_ne!(updated_tid, reused_tid);
    assert!(fastpg_storage2_relation_record_invalidate_metadata(
        relid, reused_tid, 1, 2
    ));
    assert!(fastpg_storage2_relation_record_insert_metadata(
        relid,
        updated_tid,
        1,
        2
    ));

    assert_eq!(fetch_i32(relid, reused_tid), None);
    assert_eq!(fetch_i32(relid, updated_tid), Some(3));
    assert_eq!(scan_i32_values(relid), vec![3]);
    fastpg_storage2_xact_commit();

    assert_eq!(fetch_i32(relid, reused_tid), None);
    assert_eq!(fetch_i32(relid, updated_tid), Some(3));
    assert_eq!(scan_i32_values(relid), vec![3]);
}

#[test]
fn replace_relation_from_is_transactional() {
    let _guard = test_guard();
    let dst_relid = 462;
    let src_relid = 463;

    fastpg_storage2_xact_begin();
    let old_tid = insert_i32(dst_relid, 1);
    fastpg_storage2_xact_commit();
    assert_eq!(scan_i32_values(dst_relid), vec![1]);

    fastpg_storage2_xact_begin();
    insert_i32(src_relid, 2);
    assert!(fastpg_storage2_relation_replace_from(dst_relid, src_relid));
    assert_eq!(fetch_i32(dst_relid, old_tid), None);
    assert_eq!(fastpg_storage2_relation_row_count(dst_relid), 1);
    assert_eq!(scan_i32_values(dst_relid), vec![2]);
    fastpg_storage2_xact_abort();

    assert_eq!(fetch_i32(dst_relid, old_tid), Some(1));
    assert_eq!(scan_i32_values(dst_relid), vec![1]);

    fastpg_storage2_xact_begin();
    insert_i32(src_relid, 3);
    assert!(fastpg_storage2_relation_replace_from(dst_relid, src_relid));
    fastpg_storage2_xact_commit();

    assert_eq!(fetch_i32(dst_relid, old_tid), None);
    assert_eq!(scan_i32_values(dst_relid), vec![3]);
}

#[test]
fn database_storage_is_isolated_and_cloneable() {
    let _guard = test_guard();
    let relid = 464;

    fastpg_storage2_set_database(11);
    fastpg_storage2_xact_begin();
    insert_i32(relid, 1);
    fastpg_storage2_xact_commit();
    assert_eq!(fastpg_storage2_relation_row_count(relid), 1);

    fastpg_storage2_set_database(12);
    assert_eq!(fastpg_storage2_relation_row_count(relid), 0);
    assert_eq!(scan_i32_values(relid), Vec::<i32>::new());

    assert!(fastpg_storage2_clone_database(12, 11));
    assert_eq!(scan_i32_values(relid), vec![1]);

    fastpg_storage2_xact_begin();
    insert_i32(relid, 2);
    fastpg_storage2_xact_commit();
    assert_eq!(scan_i32_values(relid), vec![1, 2]);

    fastpg_storage2_set_database(11);
    assert_eq!(scan_i32_values(relid), vec![1]);
}

#[test]
fn replace_relation_from_hides_old_rows_from_unique_validation() {
    let _guard = test_guard();
    let dst_relid = 465;
    let src_relid = 466;
    let index_relid = 4650;
    let attnums = [1i16];
    let typbyval = [1u8];
    let type_oids = [INT4_OID.0];
    let typlen = [4i16];
    let mut conflict_tid = 0u64;

    fastpg_storage2_xact_begin();
    let old_tid = insert_i32(dst_relid, 6);
    fastpg_storage2_xact_commit();
    assert_eq!(scan_i32_values(dst_relid), vec![6]);

    fastpg_storage2_xact_begin();
    insert_i32(src_relid, 6);
    assert!(fastpg_storage2_relation_replace_from(dst_relid, src_relid));
    assert_eq!(fetch_i32(dst_relid, old_tid), None);
    assert_eq!(scan_i32_values(dst_relid), vec![6]);
    assert!(!unsafe {
        fastpg_storage2_unique_index_validate_with_spec(
            index_relid,
            dst_relid,
            attnums.as_ptr(),
            type_oids.as_ptr(),
            typbyval.as_ptr(),
            typlen.as_ptr(),
            attnums.len(),
            0,
            &mut conflict_tid,
        )
    });
    fastpg_storage2_xact_commit();
}

#[test]
fn replace_relation_from_detects_duplicate_rewritten_rows_in_unique_validation() {
    let _guard = test_guard();
    let dst_relid = 471;
    let src_relid = 472;
    let index_relid = 4710;
    let attnums = [1i16];
    let typbyval = [1u8];
    let type_oids = [INT4_OID.0];
    let typlen = [4i16];
    let mut conflict_tid = 0u64;

    fastpg_storage2_xact_begin();
    insert_i32(src_relid, 6);
    insert_i32(src_relid, 6);
    assert!(fastpg_storage2_relation_replace_from(dst_relid, src_relid));
    assert_eq!(fastpg_storage2_relation_page_count(dst_relid), 0);
    assert_eq!(fastpg_storage2_relation_row_count(dst_relid), 2);
    assert_eq!(scan_i32_values(dst_relid), vec![6, 6]);
    assert!(unsafe {
        fastpg_storage2_unique_index_validate_with_spec(
            index_relid,
            dst_relid,
            attnums.as_ptr(),
            type_oids.as_ptr(),
            typbyval.as_ptr(),
            typlen.as_ptr(),
            attnums.len(),
            0,
            &mut conflict_tid,
        )
    });
    assert_ne!(conflict_tid, 0);
    fastpg_storage2_xact_commit();
}

#[test]
fn replace_relation_from_hides_same_transaction_old_rows_from_unique_validation() {
    let _guard = test_guard();
    let dst_relid = 466;
    let src_relid = 467;
    let index_relid = 4660;
    let attnums = [1i16];
    let typbyval = [1u8];
    let type_oids = [INT4_OID.0];
    let typlen = [4i16];
    let mut conflict_tid = 0u64;

    fastpg_storage2_xact_begin();
    let old_tid = insert_i32(dst_relid, 6);
    assert_eq!(scan_i32_values(dst_relid), vec![6]);
    insert_i32(src_relid, 6);
    assert!(fastpg_storage2_relation_replace_from(dst_relid, src_relid));
    assert_eq!(fetch_i32(dst_relid, old_tid), None);
    assert_eq!(scan_i32_values(dst_relid), vec![6]);
    assert!(!unsafe {
        fastpg_storage2_unique_index_validate_with_spec(
            index_relid,
            dst_relid,
            attnums.as_ptr(),
            type_oids.as_ptr(),
            typbyval.as_ptr(),
            typlen.as_ptr(),
            attnums.len(),
            0,
            &mut conflict_tid,
        )
    });
    fastpg_storage2_xact_commit();
}

#[test]
fn replace_relation_from_hides_prior_same_transaction_rewrite_from_unique_validation() {
    let _guard = test_guard();
    let dst_relid = 468;
    let first_src_relid = 469;
    let second_src_relid = 470;
    let index_relid = 4680;
    let attnums = [1i16];
    let typbyval = [1u8];
    let type_oids = [INT4_OID.0];
    let typlen = [4i16];
    let mut conflict_tid = 0u64;

    fastpg_storage2_xact_begin();
    insert_i32(first_src_relid, 6);
    assert!(fastpg_storage2_relation_replace_from(
        dst_relid,
        first_src_relid
    ));
    assert_eq!(scan_i32_values(dst_relid), vec![6]);
    assert!(!unsafe {
        fastpg_storage2_unique_index_validate_with_spec(
            index_relid,
            dst_relid,
            attnums.as_ptr(),
            type_oids.as_ptr(),
            typbyval.as_ptr(),
            typlen.as_ptr(),
            attnums.len(),
            0,
            &mut conflict_tid,
        )
    });

    insert_i32(second_src_relid, 6);
    assert!(fastpg_storage2_relation_replace_from(
        dst_relid,
        second_src_relid
    ));
    assert_eq!(scan_i32_values(dst_relid), vec![6]);
    assert!(!unsafe {
        fastpg_storage2_unique_index_validate_with_spec(
            index_relid,
            dst_relid,
            attnums.as_ptr(),
            type_oids.as_ptr(),
            typbyval.as_ptr(),
            typlen.as_ptr(),
            attnums.len(),
            0,
            &mut conflict_tid,
        )
    });
    fastpg_storage2_xact_commit();
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
}

#[test]
fn batch_scan_matches_forward_and_backward_single_row_scan() {
    let _guard = test_guard();
    let relid = 470;
    fastpg_storage2_xact_begin();
    for value in 0..5 {
        insert_i32(relid, value);
    }
    fastpg_storage2_xact_commit();

    assert_eq!(scan_i32_values_batch(relid, true, 2), vec![0, 1, 2, 3, 4]);
    assert_eq!(scan_i32_values_batch(relid, false, 2), vec![4, 3, 2, 1, 0]);
}

#[test]
fn batch_scan_open_before_non_hot_update_can_follow_update_redirect() {
    let _guard = test_guard();
    let relid = 471;
    fastpg_storage2_xact_begin();
    let old_tid = insert_i32(relid, 3);
    assert!(fastpg_storage2_relation_record_insert_metadata(
        relid, old_tid, 1, 0
    ));

    let scan = fastpg_storage2_scan_begin_with_snapshot(relid, 2);
    assert_ne!(scan, 0);

    let new_tid = update_i32(relid, old_tid, 4);
    assert!(fastpg_storage2_relation_record_invalidate_metadata(
        relid, old_tid, 1, 1
    ));
    assert!(fastpg_storage2_relation_record_insert_metadata(
        relid, new_tid, 1, 1
    ));

    let mut raw_values = [0usize; 2];
    let mut nulls = [1u8; 2];
    let mut tids = [0u64; 1];
    let mut stored_natts = [0usize; 1];
    let count = unsafe {
        fastpg_storage2_scan_next_batch_with_stored_natts(
            scan,
            1,
            raw_values.as_mut_ptr(),
            nulls.as_mut_ptr(),
            raw_values.len(),
            tids.len(),
            tids.as_mut_ptr(),
            stored_natts.as_mut_ptr(),
        )
    };
    assert_eq!(count, 1);
    assert_eq!(tids[0], new_tid);
    assert_eq!(raw_values[0] as i32, 4);
    assert_eq!(nulls, [0, 1]);
    assert_eq!(stored_natts[0], 1);

    let count = unsafe {
        fastpg_storage2_scan_next_batch_with_stored_natts(
            scan,
            1,
            raw_values.as_mut_ptr(),
            nulls.as_mut_ptr(),
            raw_values.len(),
            tids.len(),
            tids.as_mut_ptr(),
            stored_natts.as_mut_ptr(),
        )
    };
    assert_eq!(count, 0);
    fastpg_storage2_scan_end(scan);
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

#[test]
fn ffi_index_specs_scan_storage_without_rust_catalog_metadata() {
    let _guard = test_guard();
    let relid = 49;
    let index_relid = 490;
    fastpg_storage2_xact_begin();
    let first_tid = insert_i32(relid, 12);
    let second_tid = insert_i32(relid, 13);
    fastpg_storage2_xact_commit();

    let attnums = [1i16];
    let typbyval = [1u8];
    let type_oids = [INT4_OID.0];
    let typlen = [4i16];
    let values = [12usize];
    let nulls = [0u8];
    let mut tid = 0u64;

    assert!(unsafe {
        fastpg_storage2_primary_key_index_lookup_with_spec(
            index_relid,
            relid,
            attnums.as_ptr(),
            type_oids.as_ptr(),
            typbyval.as_ptr(),
            typlen.as_ptr(),
            values.as_ptr(),
            nulls.as_ptr(),
            values.len(),
            &mut tid,
        )
    });
    assert_eq!(tid, first_tid);

    assert!(unsafe {
        fastpg_storage2_unique_index_conflict_with_spec(
            index_relid,
            relid,
            attnums.as_ptr(),
            type_oids.as_ptr(),
            typbyval.as_ptr(),
            typlen.as_ptr(),
            values.as_ptr(),
            nulls.as_ptr(),
            values.len(),
            0,
            0,
            second_tid,
            &mut tid,
        )
    });
    assert_eq!(tid, first_tid);

    fastpg_storage2_xact_begin();
    insert_i32(relid, 12);
    assert!(unsafe {
        fastpg_storage2_unique_index_validate_with_spec(
            index_relid,
            relid,
            attnums.as_ptr(),
            type_oids.as_ptr(),
            typbyval.as_ptr(),
            typlen.as_ptr(),
            values.len(),
            0,
            &mut tid,
        )
    });
    assert_eq!(tid, first_tid);
    fastpg_storage2_xact_abort();
}

#[test]
fn ffi_primary_key_index_insert_records_lookup_entry() {
    let _guard = test_guard();
    let relid = 50;
    let index_relid = 500;
    let attnums = [1i16];
    let typbyval = [1u8];
    let type_oids = [INT4_OID.0];
    let typlen = [4i16];
    let values = [21usize];
    let nulls = [0u8];
    let mut tid = 0u64;

    fastpg_storage2_xact_begin();
    let inserted_tid = insert_i32(relid, 21);
    assert!(unsafe {
        fastpg_storage2_primary_key_index_insert_with_spec(
            index_relid,
            relid,
            attnums.as_ptr(),
            type_oids.as_ptr(),
            typbyval.as_ptr(),
            typlen.as_ptr(),
            values.as_ptr(),
            nulls.as_ptr(),
            values.len(),
            inserted_tid,
        )
    });
    assert!(unsafe {
        fastpg_storage2_primary_key_index_lookup_single_byval_with_spec(
            index_relid,
            relid,
            values[0],
            nulls[0],
            &mut tid,
        )
    });
    assert_eq!(tid, inserted_tid);
    fastpg_storage2_xact_abort();
}

#[test]
fn ffi_primary_key_index_insert_survives_commit_tid_remap() {
    let _guard = test_guard();
    let relid = 51;
    let index_relid = 501;
    let attnums = [1i16];
    let typbyval = [1u8];
    let type_oids = [INT4_OID.0];
    let typlen = [4i16];
    let nulls = [0u8];
    let mut tid = 0u64;

    fastpg_storage2_xact_begin();
    insert_i32(relid, 10);
    fastpg_storage2_xact_commit();

    fastpg_storage2_xact_begin();
    let inserted_tid = insert_i32(relid, 22);
    let values = [22usize];
    assert!(unsafe {
        fastpg_storage2_primary_key_index_insert_with_spec(
            index_relid,
            relid,
            attnums.as_ptr(),
            type_oids.as_ptr(),
            typbyval.as_ptr(),
            typlen.as_ptr(),
            values.as_ptr(),
            nulls.as_ptr(),
            values.len(),
            inserted_tid,
        )
    });
    fastpg_storage2_xact_commit();

    fastpg_storage2_xact_begin();
    assert!(unsafe {
        fastpg_storage2_primary_key_index_lookup_single_byval_with_spec(
            index_relid,
            relid,
            values[0],
            nulls[0],
            &mut tid,
        )
    });
    assert_eq!(fetch_i32(relid, tid), Some(22));
    fastpg_storage2_xact_abort();
}

fn fastpg_storage2_metrics_snapshot() -> FastPgStorage2Metrics {
    let mut metrics = FastPgStorage2Metrics::default();
    assert!(unsafe { fastpg_storage2_metrics(&mut metrics) });
    metrics
}
