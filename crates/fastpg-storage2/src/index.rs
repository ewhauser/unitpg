use crate::*;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum IndexKeyPart {
    Null,
    ByValue(usize),
    Bytes(Vec<u8>),
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct IndexKey {
    pub(crate) parts: Vec<IndexKeyPart>,
}

impl IndexKey {
    pub(crate) fn accounted_bytes(&self) -> usize {
        self.parts
            .iter()
            .map(|part| match part {
                IndexKeyPart::Null => 1,
                IndexKeyPart::ByValue(_) => std::mem::size_of::<usize>(),
                IndexKeyPart::Bytes(bytes) => bytes.len(),
            })
            .sum()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IndexColumnSpec {
    pub(crate) column_index: usize,
    pub(crate) typbyval: bool,
    pub(crate) typlen: i16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UniqueIndexSpec {
    pub(crate) index_oid: Oid,
    pub(crate) relation_oid: Oid,
    pub(crate) is_primary: bool,
    pub(crate) nulls_not_distinct: bool,
    pub(crate) columns: Vec<IndexColumnSpec>,
}

#[derive(Debug)]
pub(crate) struct Storage2MetadataCache {
    pub(crate) generation: u64,
    pub(crate) unique_specs_by_relation: HashMap<u32, Vec<UniqueIndexSpec>>,
    pub(crate) primary_specs_by_index: HashMap<u32, Option<UniqueIndexSpec>>,
}

impl Default for Storage2MetadataCache {
    fn default() -> Self {
        Self {
            generation: current_generation(),
            unique_specs_by_relation: HashMap::new(),
            primary_specs_by_index: HashMap::new(),
        }
    }
}

pub(crate) fn unique_index_spec_for_record(record: &IndexRecord) -> Option<UniqueIndexSpec> {
    if !record.is_unique || !record.is_valid || !record.is_ready || !record.is_live {
        return None;
    }
    let mut columns = Vec::with_capacity(record.key_attnums.len());
    for attnum in &record.key_attnums {
        if *attnum <= 0 {
            return None;
        }
        let column_index = usize::try_from(*attnum - 1).ok()?;
        let column = relation_column_by_attnum(record.relation_oid, *attnum)?;
        let pg_type = lookup_type(column.type_oid)?;
        columns.push(IndexColumnSpec {
            column_index,
            typbyval: pg_type.typbyval,
            typlen: pg_type.typlen,
        });
    }

    (!columns.is_empty()).then_some(UniqueIndexSpec {
        index_oid: record.index_oid,
        relation_oid: record.relation_oid,
        is_primary: record.is_primary,
        nulls_not_distinct: record.nulls_not_distinct,
        columns,
    })
}

pub(crate) fn storage2_metadata_cache() -> &'static Mutex<Storage2MetadataCache> {
    STORAGE2_METADATA_CACHE.get_or_init(|| Mutex::new(Storage2MetadataCache::default()))
}

pub(crate) fn with_storage2_metadata_cache<R>(
    f: impl FnOnce(&mut Storage2MetadataCache) -> R,
) -> R {
    let generation = current_generation();
    let mut cache = storage2_metadata_cache()
        .lock()
        .expect("storage2 metadata cache mutex poisoned");
    if cache.generation != generation {
        cache.generation = generation;
        cache.unique_specs_by_relation.clear();
        cache.primary_specs_by_index.clear();
    }
    f(&mut cache)
}

pub(crate) fn unique_index_specs_for_relation_oid(relation_oid: Oid) -> Vec<UniqueIndexSpec> {
    if !has_uncommitted_catalog_changes()
        && let Some(cached) = with_storage2_metadata_cache(|cache| {
            cache.unique_specs_by_relation.get(&relation_oid.0).cloned()
        })
    {
        return cached;
    }

    let specs: Vec<_> = unique_index_records_for_relation_oid(relation_oid)
        .iter()
        .filter_map(unique_index_spec_for_record)
        .collect();
    if !has_uncommitted_catalog_changes() {
        with_storage2_metadata_cache(|cache| {
            cache
                .unique_specs_by_relation
                .insert(relation_oid.0, specs.clone());
        });
    }
    specs
}

pub(crate) fn primary_index_spec_for_index_oid(index_oid: Oid) -> Option<UniqueIndexSpec> {
    if !has_uncommitted_catalog_changes()
        && let Some(cached) = with_storage2_metadata_cache(|cache| {
            cache.primary_specs_by_index.get(&index_oid.0).cloned()
        })
    {
        return cached;
    }

    let relid = relation_oid_for_index_oid(index_oid)?;
    let primary_index_oid = primary_key_index_oid_for_relation_oid(relid)?;
    let spec = if primary_index_oid == index_oid {
        unique_index_specs_for_relation_oid(relid)
            .into_iter()
            .find(|spec| spec.index_oid == index_oid && spec.is_primary)
    } else {
        None
    };
    if !has_uncommitted_catalog_changes() {
        with_storage2_metadata_cache(|cache| {
            cache
                .primary_specs_by_index
                .insert(index_oid.0, spec.clone());
        });
    }
    spec
}

pub(crate) fn primary_index_spec_for_relation_oid(relation_oid: Oid) -> Option<UniqueIndexSpec> {
    let primary_index_oid = primary_key_index_oid_for_relation_oid(relation_oid)?;
    primary_index_spec_for_index_oid(primary_index_oid)
}

pub(crate) struct UniqueIndexFfiSpecArgs {
    pub(crate) index_relid: u32,
    pub(crate) heap_relid: u32,
    pub(crate) attnums: *const i16,
    pub(crate) typbyval: *const u8,
    pub(crate) typlen: *const i16,
    pub(crate) nkeys: usize,
    pub(crate) is_primary: bool,
    pub(crate) nulls_not_distinct: bool,
}

pub(crate) unsafe fn unique_index_spec_from_ffi(
    args: UniqueIndexFfiSpecArgs,
) -> Option<UniqueIndexSpec> {
    let UniqueIndexFfiSpecArgs {
        index_relid,
        heap_relid,
        attnums,
        typbyval,
        typlen,
        nkeys,
        is_primary,
        nulls_not_distinct,
    } = args;

    if nkeys == 0 || attnums.is_null() || typbyval.is_null() || typlen.is_null() {
        return None;
    }
    let attnums = unsafe { slice::from_raw_parts(attnums, nkeys) };
    let typbyval = unsafe { slice::from_raw_parts(typbyval, nkeys) };
    let typlen = unsafe { slice::from_raw_parts(typlen, nkeys) };
    let mut columns = Vec::with_capacity(nkeys);
    for index in 0..nkeys {
        let attnum = attnums[index];
        if attnum <= 0 {
            return None;
        }
        columns.push(IndexColumnSpec {
            column_index: usize::try_from(attnum - 1).ok()?,
            typbyval: typbyval[index] != 0,
            typlen: typlen[index],
        });
    }

    Some(UniqueIndexSpec {
        index_oid: Oid(index_relid),
        relation_oid: Oid(heap_relid),
        is_primary,
        nulls_not_distinct,
        columns,
    })
}

pub(crate) fn index_key_for_input(
    index_spec: &UniqueIndexSpec,
    input: &RowInput<'_>,
) -> Option<IndexKey> {
    let mut parts = Vec::with_capacity(index_spec.columns.len());
    for column in &index_spec.columns {
        let index = column.column_index;
        if *input.is_null.get(index)? != 0 {
            if !index_spec.nulls_not_distinct {
                return None;
            }
            parts.push(IndexKeyPart::Null);
            continue;
        }
        if column.typbyval || *input.byval.get(index)? != 0 {
            parts.push(IndexKeyPart::ByValue(*input.values.get(index)?));
            continue;
        }
        let value = *input.values.get(index)?;
        let len = byref_len(column.typlen, value, Some(*input.value_lens.get(index)?))?;
        let bytes = if len == 0 {
            Vec::new()
        } else {
            if value == 0 {
                return None;
            }
            unsafe { slice::from_raw_parts(value as *const u8, len) }.to_vec()
        };
        parts.push(IndexKeyPart::Bytes(bytes));
    }
    Some(IndexKey { parts })
}

pub(crate) fn index_key_for_decoded(
    index_spec: &UniqueIndexSpec,
    values: &[DecodedDatum<'_>],
) -> Option<IndexKey> {
    let mut parts = Vec::with_capacity(index_spec.columns.len());
    for column in &index_spec.columns {
        match values.get(column.column_index)? {
            DecodedDatum::Null => {
                if !index_spec.nulls_not_distinct {
                    return None;
                }
                parts.push(IndexKeyPart::Null);
            }
            DecodedDatum::ByValue(value) => parts.push(IndexKeyPart::ByValue(*value)),
            DecodedDatum::ByRef(bytes) => {
                if column.typbyval {
                    return None;
                }
                let len = byref_len_from_bytes(column.typlen, bytes)?;
                parts.push(IndexKeyPart::Bytes(bytes.get(..len)?.to_vec()));
            }
        }
    }
    Some(IndexKey { parts })
}

pub(crate) fn index_key_for_key_datums(
    index_spec: &UniqueIndexSpec,
    values: &[usize],
    is_null: &[u8],
) -> Option<IndexKey> {
    if values.len() != index_spec.columns.len() || values.len() != is_null.len() {
        return None;
    }
    let mut parts = Vec::with_capacity(values.len());
    for (key_index, column) in index_spec.columns.iter().enumerate() {
        if is_null[key_index] != 0 {
            if !index_spec.nulls_not_distinct {
                return None;
            }
            parts.push(IndexKeyPart::Null);
            continue;
        }
        if column.typbyval {
            parts.push(IndexKeyPart::ByValue(values[key_index]));
            continue;
        }
        let len = byref_len(column.typlen, values[key_index], None)?;
        let bytes = if len == 0 {
            Vec::new()
        } else {
            unsafe { slice::from_raw_parts(values[key_index] as *const u8, len) }.to_vec()
        };
        parts.push(IndexKeyPart::Bytes(bytes));
    }
    Some(IndexKey { parts })
}
