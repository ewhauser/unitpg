use crate::*;

pub(crate) fn clear_last_storage_error() {
    LAST_STORAGE_ERROR.with(|slot| {
        slot.replace(None);
    });
}

pub(crate) fn set_last_storage_error(error: CatalogError) {
    LAST_STORAGE_ERROR.with(|slot| {
        slot.replace(Some(error));
    });
}

pub(crate) fn last_storage_error() -> Option<CatalogError> {
    LAST_STORAGE_ERROR.with(|slot| slot.borrow().clone())
}

pub(crate) fn invalid_ffi_argument(message: impl Into<String>) -> CatalogError {
    CatalogError::new("22023", message)
}

pub(crate) fn storage_limit_error(message: impl Into<String>) -> CatalogError {
    CatalogError::new(SQLSTATE_PROGRAM_LIMIT_EXCEEDED, message)
}

pub(crate) fn write_storage_error(out: *mut c_char, out_len: usize, value: &str) {
    if out.is_null() || out_len == 0 {
        return;
    }
    let bytes = value.as_bytes();
    let copy_len = bytes.len().min(out_len.saturating_sub(1));
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out as *mut u8, copy_len);
        *out.add(copy_len) = 0;
    }
}
