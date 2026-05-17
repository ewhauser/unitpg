#![deny(unsafe_op_in_unsafe_fn)]

use std::collections::HashMap;
use std::slice;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RowId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Cell {
    value: usize,
    is_null: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Row {
    row_id: u64,
    cells: Vec<Cell>,
}

#[derive(Default, Debug)]
struct RowSegment {
    rows: Vec<Row>,
    payloads: Vec<Box<[u8]>>,
}

#[derive(Debug)]
struct RelationRows {
    committed: Vec<RowSegment>,
    next_row_id: u64,
}

impl Default for RelationRows {
    fn default() -> Self {
        Self {
            committed: Vec::new(),
            next_row_id: 1,
        }
    }
}

impl RelationRows {
    fn allocate_row_id(&mut self) -> Option<u64> {
        let row_id = self.next_row_id;
        if row_id == 0 {
            return None;
        }
        self.next_row_id = self.next_row_id.checked_add(1)?;
        Some(row_id)
    }
}

#[derive(Default, Debug)]
struct TransactionOverlay {
    relations: HashMap<u32, RowSegment>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ScanState {
    rows: Vec<Row>,
    next_index: usize,
}

#[derive(Debug)]
struct StorageState {
    relations: HashMap<u32, RelationRows>,
    transaction_stack: Vec<TransactionOverlay>,
    scans: HashMap<u64, ScanState>,
    next_scan_handle: u64,
}

impl Default for StorageState {
    fn default() -> Self {
        Self {
            relations: HashMap::new(),
            transaction_stack: Vec::new(),
            scans: HashMap::new(),
            next_scan_handle: 1,
        }
    }
}

impl StorageState {
    fn allocate_scan_handle(&mut self) -> u64 {
        let handle = self.next_scan_handle;
        self.next_scan_handle = self.next_scan_handle.checked_add(1).unwrap_or(1);
        if self.next_scan_handle == 0 {
            self.next_scan_handle = 1;
        }
        handle
    }

    fn ensure_transaction(&mut self) {
        if self.transaction_stack.is_empty() {
            self.transaction_stack.push(TransactionOverlay::default());
        }
    }

    fn visible_row_count(&self, relid: u32) -> usize {
        let committed = self
            .relations
            .get(&relid)
            .map(|relation| {
                relation
                    .committed
                    .iter()
                    .map(|segment| segment.rows.len())
                    .sum()
            })
            .unwrap_or(0);
        let in_flight: usize = self
            .transaction_stack
            .iter()
            .filter_map(|overlay| overlay.relations.get(&relid))
            .map(|segment| segment.rows.len())
            .sum();
        committed + in_flight
    }

    fn visible_rows(&self, relid: u32) -> Vec<Row> {
        let mut rows = Vec::with_capacity(self.visible_row_count(relid));
        if let Some(relation) = self.relations.get(&relid) {
            for segment in &relation.committed {
                rows.extend(segment.rows.iter().cloned());
            }
        }
        for overlay in &self.transaction_stack {
            if let Some(segment) = overlay.relations.get(&relid) {
                rows.extend(segment.rows.iter().cloned());
            }
        }
        rows
    }

    fn find_visible_row(&self, relid: u32, row_id: u64) -> Option<Row> {
        if row_id == 0 {
            return None;
        }
        if let Some(relation) = self.relations.get(&relid) {
            for segment in &relation.committed {
                if let Some(row) = segment.rows.iter().find(|row| row.row_id == row_id) {
                    return Some(row.clone());
                }
            }
        }
        for overlay in &self.transaction_stack {
            if let Some(segment) = overlay.relations.get(&relid) {
                if let Some(row) = segment.rows.iter().find(|row| row.row_id == row_id) {
                    return Some(row.clone());
                }
            }
        }
        None
    }

    fn clear_relation(&mut self, relid: u32) {
        self.relations.insert(relid, RelationRows::default());
        for overlay in &mut self.transaction_stack {
            overlay.relations.remove(&relid);
        }
    }

    fn commit_top_overlay(&mut self) {
        let Some(overlay) = self.transaction_stack.pop() else {
            return;
        };

        if let Some(parent) = self.transaction_stack.last_mut() {
            merge_overlay_into_overlay(parent, overlay);
        } else {
            self.commit_overlay_to_relations(overlay);
        }
    }

    fn commit_overlay_to_relations(&mut self, overlay: TransactionOverlay) {
        for (relid, segment) in overlay.relations {
            if segment.rows.is_empty() {
                continue;
            }
            self.relations
                .entry(relid)
                .or_default()
                .committed
                .push(segment);
        }
    }
}

fn merge_overlay_into_overlay(parent: &mut TransactionOverlay, overlay: TransactionOverlay) {
    for (relid, mut segment) in overlay.relations {
        if segment.rows.is_empty() {
            continue;
        }
        let parent_segment = parent.relations.entry(relid).or_default();
        parent_segment.rows.append(&mut segment.rows);
        parent_segment.payloads.append(&mut segment.payloads);
    }
}

static STORAGE: OnceLock<Mutex<StorageState>> = OnceLock::new();

fn storage() -> &'static Mutex<StorageState> {
    STORAGE.get_or_init(|| Mutex::new(StorageState::default()))
}

fn with_storage<R>(f: impl FnOnce(&mut StorageState) -> R) -> R {
    match storage().lock() {
        Ok(mut state) => f(&mut state),
        Err(poisoned) => {
            let mut state = poisoned.into_inner();
            f(&mut state)
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_xact_begin() {
    with_storage(|state| state.ensure_transaction());
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_xact_commit() {
    with_storage(|state| {
        while state.transaction_stack.len() > 1 {
            state.commit_top_overlay();
        }
        state.commit_top_overlay();
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_xact_abort() {
    with_storage(|state| {
        state.transaction_stack.clear();
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_subxact_begin() {
    with_storage(|state| {
        state.ensure_transaction();
        state.transaction_stack.push(TransactionOverlay::default());
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_subxact_commit() {
    with_storage(|state| {
        if state.transaction_stack.len() > 1 {
            state.commit_top_overlay();
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_subxact_abort() {
    with_storage(|state| {
        if state.transaction_stack.len() > 1 {
            state.transaction_stack.pop();
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_relation_clear(relid: u32) {
    with_storage(|state| state.clear_relation(relid));
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_relation_row_count(relid: u32) -> usize {
    with_storage(|state| state.visible_row_count(relid))
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_relation_contains_row(relid: u32, row_id: u64) -> bool {
    with_storage(|state| state.find_visible_row(relid, row_id).is_some())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fastpg_rust_relation_insert(
    relid: u32,
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
    row_id_out: *mut u64,
) -> bool {
    if natts > 0
        && (values.is_null() || is_null.is_null() || byval.is_null() || value_lens.is_null())
    {
        return false;
    }

    let values = if natts == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(values, natts) }
    };
    let is_null = if natts == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(is_null, natts) }
    };
    let byval = if natts == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(byval, natts) }
    };
    let value_lens = if natts == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(value_lens, natts) }
    };

    with_storage(|state| {
        let row_id = match state.relations.entry(relid).or_default().allocate_row_id() {
            Some(row_id) => row_id,
            None => return false,
        };

        state.ensure_transaction();
        let segment = state
            .transaction_stack
            .last_mut()
            .expect("transaction was just ensured")
            .relations
            .entry(relid)
            .or_default();

        let cells = match copy_cells_to_segment(segment, values, is_null, byval, value_lens) {
            Some(cells) => cells,
            None => return false,
        };

        segment.rows.push(Row { row_id, cells });
        if !row_id_out.is_null() {
            unsafe {
                *row_id_out = row_id;
            }
        }
        true
    })
}

fn copy_cells_to_segment(
    segment: &mut RowSegment,
    values: &[usize],
    is_null: &[u8],
    byval: &[u8],
    value_lens: &[usize],
) -> Option<Vec<Cell>> {
    let mut cells = Vec::with_capacity(values.len());
    for index in 0..values.len() {
        if is_null[index] != 0 {
            cells.push(Cell {
                value: 0,
                is_null: true,
            });
            continue;
        }

        if byval[index] != 0 {
            cells.push(Cell {
                value: values[index],
                is_null: false,
            });
            continue;
        }

        let len = value_lens[index];
        if values[index] == 0 && len > 0 {
            return None;
        }
        let bytes = if len == 0 {
            Vec::new().into_boxed_slice()
        } else {
            let source = unsafe { slice::from_raw_parts(values[index] as *const u8, len) };
            source.to_vec().into_boxed_slice()
        };
        let value = bytes.as_ptr() as usize;
        segment.payloads.push(bytes);
        cells.push(Cell {
            value,
            is_null: false,
        });
    }
    Some(cells)
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_scan_begin(relid: u32) -> u64 {
    with_storage(|state| {
        state.relations.entry(relid).or_default();
        let rows = state.visible_rows(relid);
        let handle = state.allocate_scan_handle();
        state.scans.insert(
            handle,
            ScanState {
                rows,
                next_index: 0,
            },
        );
        handle
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_scan_reset(scan_handle: u64) {
    with_storage(|state| {
        if let Some(scan) = state.scans.get_mut(&scan_handle) {
            scan.next_index = 0;
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_scan_end(scan_handle: u64) {
    with_storage(|state| {
        state.scans.remove(&scan_handle);
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fastpg_rust_scan_next(
    scan_handle: u64,
    forward: u8,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
    row_id_out: *mut u64,
) -> bool {
    let row = with_storage(|state| {
        let scan = match state.scans.get_mut(&scan_handle) {
            Some(scan) => scan,
            None => return None,
        };

        let row_count = scan.rows.len();
        let row_index = if forward != 0 {
            if scan.next_index >= row_count {
                return None;
            }
            let row_index = scan.next_index;
            scan.next_index += 1;
            row_index
        } else {
            if scan.next_index == 0 {
                scan.next_index = row_count;
            }
            if scan.next_index == 0 {
                return None;
            }
            scan.next_index -= 1;
            scan.next_index
        };

        scan.rows.get(row_index).cloned()
    });

    match row {
        Some(row) => unsafe {
            copy_row_to_outputs(&row, values_out, is_null_out, natts, row_id_out)
        },
        None => false,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fastpg_rust_fetch_row(
    relid: u32,
    row_id: u64,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
) -> bool {
    let row = with_storage(|state| state.find_visible_row(relid, row_id));

    match row {
        Some(row) => unsafe {
            copy_row_to_outputs(&row, values_out, is_null_out, natts, std::ptr::null_mut())
        },
        None => false,
    }
}

unsafe fn copy_row_to_outputs(
    row: &Row,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
    row_id_out: *mut u64,
) -> bool {
    if row.cells.len() != natts {
        return false;
    }
    if natts > 0 && (values_out.is_null() || is_null_out.is_null()) {
        return false;
    }

    let values_out = if natts == 0 {
        &mut []
    } else {
        unsafe { slice::from_raw_parts_mut(values_out, natts) }
    };
    let is_null_out = if natts == 0 {
        &mut []
    } else {
        unsafe { slice::from_raw_parts_mut(is_null_out, natts) }
    };
    for (index, cell) in row.cells.iter().enumerate() {
        values_out[index] = cell.value;
        is_null_out[index] = u8::from(cell.is_null);
    }

    if !row_id_out.is_null() {
        unsafe {
            *row_id_out = row.row_id;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Mutex as StdMutex, MutexGuard};

    static NEXT_RELID: AtomicU32 = AtomicU32::new(10_000);
    static TEST_LOCK: StdMutex<()> = StdMutex::new(());

    struct TestGuard {
        _guard: MutexGuard<'static, ()>,
    }

    impl Drop for TestGuard {
        fn drop(&mut self) {
            fastpg_rust_xact_abort();
        }
    }

    fn test_guard() -> TestGuard {
        let guard = TEST_LOCK.lock().expect("test lock poisoned");
        fastpg_rust_xact_abort();
        TestGuard { _guard: guard }
    }

    fn next_relid() -> u32 {
        NEXT_RELID.fetch_add(1, Ordering::Relaxed)
    }

    unsafe fn insert_byval(relid: u32, values: &[usize], is_null: &[u8], row_id: &mut u64) -> bool {
        let byval = vec![1u8; values.len()];
        let value_lens = vec![0usize; values.len()];
        unsafe {
            fastpg_rust_relation_insert(
                relid,
                values.as_ptr(),
                is_null.as_ptr(),
                byval.as_ptr(),
                value_lens.as_ptr(),
                values.len(),
                row_id,
            )
        }
    }

    #[test]
    fn inserts_fetches_and_scans_rows() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut first_row_id = 0;
        let mut second_row_id = 0;
        let first_values = [11usize, 0];
        let first_nulls = [0u8, 1];
        let second_values = [22usize, 33];
        let second_nulls = [0u8, 0];

        unsafe {
            assert!(insert_byval(
                relid,
                &first_values,
                &first_nulls,
                &mut first_row_id,
            ));
            assert!(insert_byval(
                relid,
                &second_values,
                &second_nulls,
                &mut second_row_id,
            ));
        }

        assert_eq!(first_row_id, 1);
        assert_eq!(second_row_id, 2);
        assert_eq!(fastpg_rust_relation_row_count(relid), 2);

        let mut values = [0usize; 2];
        let mut nulls = [0u8; 2];
        unsafe {
            assert!(fastpg_rust_fetch_row(
                relid,
                second_row_id,
                values.as_mut_ptr(),
                nulls.as_mut_ptr(),
                values.len(),
            ));
        }
        assert_eq!(values, second_values);
        assert_eq!(nulls, second_nulls);

        let scan = fastpg_rust_scan_begin(relid);
        let mut row_id = 0;
        unsafe {
            assert!(fastpg_rust_scan_next(
                scan,
                1,
                values.as_mut_ptr(),
                nulls.as_mut_ptr(),
                values.len(),
                &mut row_id,
            ));
        }
        assert_eq!(row_id, first_row_id);
        assert_eq!(values, first_values);
        assert_eq!(nulls, first_nulls);
        fastpg_rust_scan_end(scan);

        let backward_scan = fastpg_rust_scan_begin(relid);
        unsafe {
            assert!(fastpg_rust_scan_next(
                backward_scan,
                0,
                values.as_mut_ptr(),
                nulls.as_mut_ptr(),
                values.len(),
                &mut row_id,
            ));
        }
        assert_eq!(row_id, second_row_id);
        assert_eq!(values, second_values);
        assert_eq!(nulls, second_nulls);
        fastpg_rust_scan_end(backward_scan);

        fastpg_rust_relation_clear(relid);
        assert_eq!(fastpg_rust_relation_row_count(relid), 0);
    }

    #[test]
    fn zero_column_rows_accept_null_buffers() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut row_id = 0;

        unsafe {
            assert!(fastpg_rust_relation_insert(
                relid,
                ptr::null(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                0,
                &mut row_id,
            ));
        }
        assert_eq!(row_id, 1);

        let scan = fastpg_rust_scan_begin(relid);
        let mut scanned_row_id = 0;
        unsafe {
            assert!(fastpg_rust_scan_next(
                scan,
                1,
                ptr::null_mut(),
                ptr::null_mut(),
                0,
                &mut scanned_row_id,
            ));
        }
        assert_eq!(scanned_row_id, row_id);
        fastpg_rust_scan_end(scan);

        unsafe {
            assert!(fastpg_rust_fetch_row(
                relid,
                row_id,
                ptr::null_mut(),
                ptr::null_mut(),
                0,
            ));
        }
    }

    #[test]
    fn committed_rows_survive_top_level_commit() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut row_id = 0;

        fastpg_rust_xact_begin();
        unsafe {
            assert!(insert_byval(relid, &[42], &[0], &mut row_id));
        }
        fastpg_rust_xact_commit();

        assert_eq!(fastpg_rust_relation_row_count(relid), 1);
        assert!(fastpg_rust_relation_contains_row(relid, row_id));
    }

    #[test]
    fn aborted_rows_are_dropped() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut row_id = 0;
        let values = [7usize];
        let nulls = [0u8];

        fastpg_rust_xact_begin();
        unsafe {
            assert!(insert_byval(relid, &values, &nulls, &mut row_id));
        }
        fastpg_rust_xact_abort();

        assert_eq!(fastpg_rust_relation_row_count(relid), 0);
        assert!(!fastpg_rust_relation_contains_row(relid, row_id));
    }

    #[test]
    fn subxact_abort_drops_only_nested_rows() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut parent_row_id = 0;
        let mut nested_row_id = 0;

        fastpg_rust_xact_begin();
        unsafe {
            assert!(insert_byval(relid, &[1], &[0], &mut parent_row_id));
        }
        fastpg_rust_subxact_begin();
        unsafe {
            assert!(insert_byval(relid, &[2], &[0], &mut nested_row_id));
        }
        fastpg_rust_subxact_abort();
        fastpg_rust_xact_commit();

        assert_eq!(fastpg_rust_relation_row_count(relid), 1);
        assert!(fastpg_rust_relation_contains_row(relid, parent_row_id));
        assert!(!fastpg_rust_relation_contains_row(relid, nested_row_id));
    }

    #[test]
    fn byref_values_are_copied_into_rust_storage() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut row_id = 0;
        let mut bytes = b"hello".to_vec();
        let values = [bytes.as_ptr() as usize];
        let nulls = [0u8];
        let byval = [0u8];
        let value_lens = [bytes.len()];

        fastpg_rust_xact_begin();
        unsafe {
            assert!(fastpg_rust_relation_insert(
                relid,
                values.as_ptr(),
                nulls.as_ptr(),
                byval.as_ptr(),
                value_lens.as_ptr(),
                values.len(),
                &mut row_id,
            ));
        }
        fastpg_rust_xact_commit();

        bytes.fill(b'X');

        let mut values_out = [0usize; 1];
        let mut nulls_out = [0u8; 1];
        unsafe {
            assert!(fastpg_rust_fetch_row(
                relid,
                row_id,
                values_out.as_mut_ptr(),
                nulls_out.as_mut_ptr(),
                1,
            ));
            let copied = slice::from_raw_parts(values_out[0] as *const u8, value_lens[0]);
            assert_eq!(copied, b"hello");
        }
        assert_eq!(nulls_out, [0]);
    }

    #[test]
    fn logical_row_ids_exceed_u16_capacity() {
        let _guard = test_guard();
        let relid = next_relid();
        let mut row_id = 0;

        fastpg_rust_xact_begin();
        for value in 1..=70_000usize {
            unsafe {
                assert!(insert_byval(relid, &[value], &[0], &mut row_id));
            }
        }
        assert_eq!(row_id, 70_000);
        assert_eq!(fastpg_rust_relation_row_count(relid), 70_000);

        let mut values = [0usize; 1];
        let mut nulls = [0u8; 1];
        unsafe {
            assert!(fastpg_rust_fetch_row(
                relid,
                row_id,
                values.as_mut_ptr(),
                nulls.as_mut_ptr(),
                1,
            ));
        }
        assert_eq!(values, [70_000]);
        assert_eq!(nulls, [0]);
    }
}
