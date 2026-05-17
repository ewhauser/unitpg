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
    tid_offset: u16,
    cells: Vec<Cell>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScanState {
    relid: u32,
    next_index: usize,
}

#[derive(Default, Debug)]
struct RelationRows {
    rows: Vec<Row>,
}

#[derive(Debug)]
struct StorageState {
    relations: HashMap<u32, RelationRows>,
    scans: HashMap<u64, ScanState>,
    next_scan_handle: u64,
}

impl Default for StorageState {
    fn default() -> Self {
        Self {
            relations: HashMap::new(),
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
pub extern "C" fn fastpg_rust_relation_clear(relid: u32) {
    with_storage(|state| {
        state.relations.entry(relid).or_default().rows.clear();
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_relation_row_count(relid: u32) -> usize {
    with_storage(|state| {
        state
            .relations
            .get(&relid)
            .map(|relation| relation.rows.len())
            .unwrap_or(0)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fastpg_rust_relation_insert(
    relid: u32,
    values: *const usize,
    is_null: *const u8,
    natts: usize,
    tid_offset_out: *mut u16,
) -> bool {
    if natts > 0 && (values.is_null() || is_null.is_null()) {
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

    with_storage(|state| {
        let relation = state.relations.entry(relid).or_default();
        if relation.rows.len() >= u16::MAX as usize {
            return false;
        }

        let tid_offset = (relation.rows.len() + 1) as u16;
        let cells = values
            .iter()
            .zip(is_null.iter())
            .map(|(&value, &is_null)| Cell {
                value,
                is_null: is_null != 0,
            })
            .collect();

        relation.rows.push(Row { tid_offset, cells });
        if !tid_offset_out.is_null() {
            unsafe {
                *tid_offset_out = tid_offset;
            }
        }
        true
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fastpg_rust_scan_begin(relid: u32) -> u64 {
    with_storage(|state| {
        state.relations.entry(relid).or_default();
        let handle = state.allocate_scan_handle();
        state.scans.insert(
            handle,
            ScanState {
                relid,
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
    tid_offset_out: *mut u16,
) -> bool {
    let row = with_storage(|state| {
        let scan = match state.scans.get_mut(&scan_handle) {
            Some(scan) => scan,
            None => return None,
        };
        let relation = match state.relations.get(&scan.relid) {
            Some(relation) => relation,
            None => return None,
        };

        let row_count = relation.rows.len();
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

        relation.rows.get(row_index).cloned()
    });

    match row {
        Some(row) => unsafe {
            copy_row_to_outputs(&row, values_out, is_null_out, natts, tid_offset_out)
        },
        None => false,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fastpg_rust_fetch_row(
    relid: u32,
    tid_offset: u16,
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
) -> bool {
    if tid_offset == 0 {
        return false;
    }

    let row = with_storage(|state| {
        state
            .relations
            .get(&relid)
            .and_then(|relation| relation.rows.get(usize::from(tid_offset - 1)))
            .cloned()
    });

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
    tid_offset_out: *mut u16,
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

    if !tid_offset_out.is_null() {
        unsafe {
            *tid_offset_out = row.tid_offset;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT_RELID: AtomicU32 = AtomicU32::new(10_000);

    fn next_relid() -> u32 {
        NEXT_RELID.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn inserts_fetches_and_scans_rows() {
        let relid = next_relid();
        let mut first_tid = 0;
        let mut second_tid = 0;
        let first_values = [11usize, 0];
        let first_nulls = [0u8, 1];
        let second_values = [22usize, 33];
        let second_nulls = [0u8, 0];

        unsafe {
            assert!(fastpg_rust_relation_insert(
                relid,
                first_values.as_ptr(),
                first_nulls.as_ptr(),
                first_values.len(),
                &mut first_tid,
            ));
            assert!(fastpg_rust_relation_insert(
                relid,
                second_values.as_ptr(),
                second_nulls.as_ptr(),
                second_values.len(),
                &mut second_tid,
            ));
        }

        assert_eq!(first_tid, 1);
        assert_eq!(second_tid, 2);
        assert_eq!(fastpg_rust_relation_row_count(relid), 2);

        let mut values = [0usize; 2];
        let mut nulls = [0u8; 2];
        unsafe {
            assert!(fastpg_rust_fetch_row(
                relid,
                second_tid,
                values.as_mut_ptr(),
                nulls.as_mut_ptr(),
                values.len(),
            ));
        }
        assert_eq!(values, second_values);
        assert_eq!(nulls, second_nulls);

        let scan = fastpg_rust_scan_begin(relid);
        let mut tid = 0;
        unsafe {
            assert!(fastpg_rust_scan_next(
                scan,
                1,
                values.as_mut_ptr(),
                nulls.as_mut_ptr(),
                values.len(),
                &mut tid,
            ));
        }
        assert_eq!(tid, first_tid);
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
                &mut tid,
            ));
        }
        assert_eq!(tid, second_tid);
        assert_eq!(values, second_values);
        assert_eq!(nulls, second_nulls);
        fastpg_rust_scan_end(backward_scan);

        fastpg_rust_relation_clear(relid);
        assert_eq!(fastpg_rust_relation_row_count(relid), 0);
    }

    #[test]
    fn zero_column_rows_accept_null_buffers() {
        let relid = next_relid();
        let mut tid = 0;

        unsafe {
            assert!(fastpg_rust_relation_insert(
                relid,
                ptr::null(),
                ptr::null(),
                0,
                &mut tid,
            ));
        }
        assert_eq!(tid, 1);

        let scan = fastpg_rust_scan_begin(relid);
        let mut scanned_tid = 0;
        unsafe {
            assert!(fastpg_rust_scan_next(
                scan,
                1,
                ptr::null_mut(),
                ptr::null_mut(),
                0,
                &mut scanned_tid,
            ));
        }
        assert_eq!(scanned_tid, tid);
        fastpg_rust_scan_end(scan);

        unsafe {
            assert!(fastpg_rust_fetch_row(
                relid,
                tid,
                ptr::null_mut(),
                ptr::null_mut(),
                0,
            ));
        }
    }
}
