use crate::*;

pub(crate) const DATUM_ALIGNMENT: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DecodedDatum<'a> {
    Null,
    ByValue(usize),
    ByRef(&'a [u8]),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DecodedTuple<'a> {
    pub(crate) tid: Tid,
    pub(crate) values: Vec<DecodedDatum<'a>>,
}

#[derive(Clone, Copy)]
pub(crate) struct RowInput<'a> {
    pub(crate) values: &'a [usize],
    pub(crate) is_null: &'a [u8],
    pub(crate) byval: &'a [u8],
    pub(crate) value_lens: &'a [usize],
}

pub(crate) fn input_arrays<'a>(
    values: *const usize,
    is_null: *const u8,
    byval: *const u8,
    value_lens: *const usize,
    natts: usize,
) -> Option<RowInput<'a>> {
    if natts > 0
        && (values.is_null() || is_null.is_null() || byval.is_null() || value_lens.is_null())
    {
        return None;
    }
    Some(RowInput {
        values: if natts == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(values, natts) }
        },
        is_null: if natts == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(is_null, natts) }
        },
        byval: if natts == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(byval, natts) }
        },
        value_lens: if natts == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(value_lens, natts) }
        },
    })
}

pub(crate) fn key_arrays<'a>(
    values: *const usize,
    is_null: *const u8,
    nkeys: usize,
) -> Option<(&'a [usize], &'a [u8])> {
    if nkeys > 0 && (values.is_null() || is_null.is_null()) {
        return None;
    }
    Some((
        if nkeys == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(values, nkeys) }
        },
        if nkeys == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(is_null, nkeys) }
        },
    ))
}

fn validate_input(input: &RowInput<'_>) -> Result<(), CatalogError> {
    if input.values.len() != input.is_null.len()
        || input.values.len() != input.byval.len()
        || input.values.len() != input.value_lens.len()
    {
        return Err(invalid_ffi_argument(
            "row input arrays have mismatched lengths",
        ));
    }

    Ok(())
}

pub(crate) fn tuple_storage_len(input: &RowInput<'_>) -> Result<usize, CatalogError> {
    validate_input(input)?;
    let natts = input.values.len();
    let null_bitmap_len = natts.div_ceil(8);
    let attr_dir_offset = TUPLE_HEADER_LEN + null_bitmap_len;
    let payload_offset = attr_dir_offset.saturating_add(natts.saturating_mul(ATTR_ENTRY_LEN));
    let payload_offset = payload_offset.next_multiple_of(DATUM_ALIGNMENT);
    let _natts: u16 = natts.try_into().map_err(|_| {
        invalid_ffi_argument("row input has too many attributes for storage2 tuple")
    })?;
    let _null_bitmap_len: u16 = null_bitmap_len
        .try_into()
        .map_err(|_| invalid_ffi_argument("null bitmap is too large"))?;
    let _attr_dir_offset: u32 = attr_dir_offset
        .try_into()
        .map_err(|_| invalid_ffi_argument("attribute directory offset is too large"))?;
    let _payload_offset: u32 = payload_offset
        .try_into()
        .map_err(|_| invalid_ffi_argument("tuple payload offset is too large"))?;
    let mut len = payload_offset;

    for index in 0..natts {
        if input.is_null[index] != 0 || input.byval[index] != 0 {
            continue;
        }

        let value_len = input.value_lens[index];
        if input.values[index] == 0 && value_len > 0 {
            return Err(invalid_ffi_argument(
                "non-null by-reference value has null pointer",
            ));
        }
        len = len.next_multiple_of(DATUM_ALIGNMENT);
        len = len
            .checked_add(value_len)
            .ok_or_else(|| storage_limit_error("storage2 tuple is too large"))?;
    }

    if len > u32::MAX as usize {
        return Err(storage_limit_error("storage2 tuple is too large"));
    }
    Ok(len)
}

pub(crate) fn write_tuple_to_slice_known_len(
    input: &RowInput<'_>,
    bytes: &mut [u8],
    expected_len: usize,
) -> Result<(), CatalogError> {
    if bytes.len() != expected_len {
        return Err(invalid_ffi_argument(
            "tuple output buffer has the wrong length",
        ));
    }

    bytes.fill(0);
    let natts = input.values.len();
    let null_bitmap_len = natts.div_ceil(8);
    let attr_dir_offset = TUPLE_HEADER_LEN + null_bitmap_len;
    let payload_offset = attr_dir_offset.saturating_add(natts.saturating_mul(ATTR_ENTRY_LEN));
    let payload_offset = payload_offset.next_multiple_of(DATUM_ALIGNMENT);
    let mut payload_len: usize = 0;

    bytes[0..4].copy_from_slice(TUPLE_MAGIC);
    write_u16(
        bytes,
        4,
        natts.try_into().map_err(|_| {
            invalid_ffi_argument("row input has too many attributes for storage2 tuple")
        })?,
    );
    write_u16(
        bytes,
        6,
        null_bitmap_len
            .try_into()
            .map_err(|_| invalid_ffi_argument("null bitmap is too large"))?,
    );
    write_u32(
        bytes,
        8,
        attr_dir_offset
            .try_into()
            .map_err(|_| invalid_ffi_argument("attribute directory offset is too large"))?,
    );
    write_u32(
        bytes,
        12,
        payload_offset
            .try_into()
            .map_err(|_| invalid_ffi_argument("tuple payload offset is too large"))?,
    );

    for index in 0..natts {
        let entry = attr_dir_offset + index * ATTR_ENTRY_LEN;
        if input.is_null[index] != 0 {
            bytes[TUPLE_HEADER_LEN + index / 8] |= 1 << (index % 8);
            bytes[entry] = 0;
            continue;
        }

        if input.byval[index] != 0 {
            bytes[entry] = 1;
            write_u64(bytes, entry + 8, input.values[index] as u64);
            continue;
        }

        let len = input.value_lens[index];
        if input.values[index] == 0 && len > 0 {
            return Err(invalid_ffi_argument(
                "non-null by-reference value has null pointer",
            ));
        }
        let source = if len == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(input.values[index] as *const u8, len) }
        };
        payload_len = payload_len.next_multiple_of(DATUM_ALIGNMENT);
        let start = payload_offset + payload_len;
        let end = start + len;
        bytes[start..end].copy_from_slice(source);
        bytes[entry] = 2;
        write_u64(bytes, entry + 8, payload_len as u64);
        write_u64(bytes, entry + 16, len as u64);
        payload_len += len;
    }

    Ok(())
}

pub(crate) fn decode_tuple(tid: Tid, tuple: &[u8]) -> Option<DecodedTuple<'_>> {
    if tuple.len() < TUPLE_HEADER_LEN || tuple.get(0..4)? != TUPLE_MAGIC {
        return None;
    }
    let natts = read_u16(tuple, 4)? as usize;
    let null_bitmap_len = read_u16(tuple, 6)? as usize;
    let attr_dir_offset = read_u32(tuple, 8)? as usize;
    let payload_offset = read_u32(tuple, 12)? as usize;
    if attr_dir_offset != TUPLE_HEADER_LEN + null_bitmap_len {
        return None;
    }
    if payload_offset < attr_dir_offset
        || payload_offset.checked_add(0)? > tuple.len()
        || payload_offset
            != attr_dir_offset
                .checked_add(natts.checked_mul(ATTR_ENTRY_LEN)?)?
                .next_multiple_of(DATUM_ALIGNMENT)
    {
        return None;
    }

    let mut values = Vec::with_capacity(natts);
    for index in 0..natts {
        let null = tuple
            .get(TUPLE_HEADER_LEN + index / 8)
            .is_some_and(|byte| byte & (1 << (index % 8)) != 0);
        let entry = attr_dir_offset + index * ATTR_ENTRY_LEN;
        let tag = *tuple.get(entry)?;
        if null || tag == 0 {
            values.push(DecodedDatum::Null);
            continue;
        }
        match tag {
            1 => values.push(DecodedDatum::ByValue(read_u64(tuple, entry + 8)? as usize)),
            2 => {
                let offset = read_u64(tuple, entry + 8)? as usize;
                let len = read_u64(tuple, entry + 16)? as usize;
                let start = payload_offset.checked_add(offset)?;
                let end = start.checked_add(len)?;
                values.push(DecodedDatum::ByRef(tuple.get(start..end)?));
            }
            _ => return None,
        }
    }
    Some(DecodedTuple { tid, values })
}

pub(crate) fn tuple_byval_attr_equals(
    tuple: &[u8],
    attnum: usize,
    value: usize,
    is_null: u8,
) -> Option<bool> {
    if attnum == 0 || tuple.len() < TUPLE_HEADER_LEN || tuple.get(0..4)? != TUPLE_MAGIC {
        return None;
    }
    let index = attnum - 1;
    let base = tuple.as_ptr();
    let natts = unsafe { std::ptr::read_unaligned(base.add(4) as *const u16) } as usize;
    if index >= natts {
        return Some(is_null != 0);
    }

    let null_bitmap_len = natts.div_ceil(8);
    let attr_dir_offset = TUPLE_HEADER_LEN + null_bitmap_len;
    let payload_offset = attr_dir_offset
        .checked_add(natts.checked_mul(ATTR_ENTRY_LEN)?)?
        .next_multiple_of(DATUM_ALIGNMENT);
    if payload_offset > tuple.len() {
        return None;
    }

    let old_null = unsafe { *base.add(TUPLE_HEADER_LEN + index / 8) } & (1 << (index % 8)) != 0;
    let new_null = is_null != 0;
    if old_null || new_null {
        return Some(old_null == new_null);
    }

    let entry = attr_dir_offset + index * ATTR_ENTRY_LEN;
    if entry.checked_add(ATTR_ENTRY_LEN)? > tuple.len() {
        return None;
    }
    let entry_ptr = unsafe { base.add(entry) };
    if unsafe { *entry_ptr } != 1 {
        return Some(false);
    }
    let old_value = unsafe { std::ptr::read_unaligned(entry_ptr.add(8) as *const u64) };
    Some(old_value as usize == value)
}

pub(crate) fn byref_len(typlen: i16, value: usize, fallback_len: Option<usize>) -> Option<usize> {
    if typlen > 0 {
        return Some(typlen as usize);
    }
    if let Some(len) = fallback_len
        && len > 0
    {
        return Some(len);
    }
    match typlen {
        -1 => varlena_payload_len(value),
        -2 => c_string_payload_len(value),
        _ => None,
    }
}

pub(crate) fn byref_len_from_bytes(typlen: i16, bytes: &[u8]) -> Option<usize> {
    if typlen > 0 {
        return Some((typlen as usize).min(bytes.len()));
    }
    match typlen {
        -1 => varlena_payload_len(bytes.as_ptr() as usize).filter(|len| *len <= bytes.len()),
        -2 => bytes
            .iter()
            .position(|byte| *byte == 0)
            .map(|index| index + 1),
        _ => Some(bytes.len()),
    }
}

pub(crate) fn varlena_payload_len(value: usize) -> Option<usize> {
    if value == 0 {
        return None;
    }
    let raw = unsafe { std::ptr::read_unaligned(value as *const u32) };
    let len = if cfg!(target_endian = "little") {
        (raw >> 2) as usize
    } else {
        raw as usize
    };
    (len >= 4).then_some(len)
}

pub(crate) fn c_string_payload_len(value: usize) -> Option<usize> {
    if value == 0 {
        return None;
    }
    let mut len = 0usize;
    loop {
        let byte = unsafe { std::ptr::read((value as *const u8).add(len)) };
        len = len.checked_add(1)?;
        if byte == 0 {
            return Some(len);
        }
    }
}

pub(crate) fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_ne_bytes());
}

pub(crate) fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
}

pub(crate) fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_ne_bytes());
}

pub(crate) fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let mut value = [0u8; 2];
    value.copy_from_slice(bytes.get(offset..offset + 2)?);
    Some(u16::from_ne_bytes(value))
}

pub(crate) fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let mut value = [0u8; 4];
    value.copy_from_slice(bytes.get(offset..offset + 4)?);
    Some(u32::from_ne_bytes(value))
}

pub(crate) fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let mut value = [0u8; 8];
    value.copy_from_slice(bytes.get(offset..offset + 8)?);
    Some(u64::from_ne_bytes(value))
}

pub(crate) fn copy_tuple_to_outputs(
    tid: Tid,
    tuple: &[u8],
    values_out: *mut usize,
    is_null_out: *mut u8,
    natts: usize,
    tid_out: *mut u64,
    stored_natts_out: *mut usize,
) -> bool {
    if tuple.len() < TUPLE_HEADER_LEN || tuple.get(0..4) != Some(TUPLE_MAGIC) {
        return false;
    }
    let base = tuple.as_ptr();
    let tuple_natts = unsafe { std::ptr::read_unaligned(base.add(4) as *const u16) } as usize;
    if natts > 0 && (values_out.is_null() || is_null_out.is_null()) {
        return false;
    }
    let null_bitmap_len = tuple_natts.div_ceil(8);
    let attr_dir_offset = TUPLE_HEADER_LEN + null_bitmap_len;
    let payload_offset = attr_dir_offset
        .saturating_add(tuple_natts.saturating_mul(ATTR_ENTRY_LEN))
        .next_multiple_of(DATUM_ALIGNMENT);
    if payload_offset > tuple.len() {
        return false;
    }
    debug_assert_eq!(
        unsafe { std::ptr::read_unaligned(base.add(6) as *const u16) } as usize,
        null_bitmap_len
    );
    debug_assert_eq!(
        unsafe { std::ptr::read_unaligned(base.add(8) as *const u32) } as usize,
        attr_dir_offset
    );
    debug_assert_eq!(
        unsafe { std::ptr::read_unaligned(base.add(12) as *const u32) } as usize,
        payload_offset
    );

    let output_natts = tuple_natts.min(natts);
    for index in 0..output_natts {
        let null_byte = unsafe { *base.add(TUPLE_HEADER_LEN + index / 8) };
        let null = null_byte & (1 << (index % 8)) != 0;
        let entry = attr_dir_offset + index * ATTR_ENTRY_LEN;
        let entry_ptr = unsafe { base.add(entry) };
        let tag = unsafe { *entry_ptr };
        if null || tag == 0 {
            unsafe {
                values_out.add(index).write(0);
                is_null_out.add(index).write(1);
            }
            continue;
        }
        match tag {
            1 => {
                let value = unsafe { std::ptr::read_unaligned(entry_ptr.add(8) as *const u64) };
                unsafe {
                    values_out.add(index).write(value as usize);
                    is_null_out.add(index).write(0);
                }
            }
            2 => {
                let offset =
                    unsafe { std::ptr::read_unaligned(entry_ptr.add(8) as *const u64) } as usize;
                let len =
                    unsafe { std::ptr::read_unaligned(entry_ptr.add(16) as *const u64) } as usize;
                let Some(start) = payload_offset.checked_add(offset) else {
                    return false;
                };
                let Some(end) = start.checked_add(len) else {
                    return false;
                };
                if end > tuple.len() {
                    return false;
                }
                unsafe {
                    values_out.add(index).write(base.add(start) as usize);
                    is_null_out.add(index).write(0);
                }
            }
            _ => return false,
        }
    }
    for index in output_natts..natts {
        unsafe {
            values_out.add(index).write(0);
            is_null_out.add(index).write(1);
        }
    }
    if !tid_out.is_null() {
        unsafe {
            *tid_out = tid.pack();
        }
    }
    if !stored_natts_out.is_null() {
        unsafe {
            *stored_natts_out = tuple_natts;
        }
    }
    true
}
