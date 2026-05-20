use std::collections::BTreeMap;
use std::sync::Mutex;
#[cfg(test)]
use std::sync::atomic::Ordering;

use fastpg_types::Oid;

use crate::generated_catalog;
use crate::model::*;
use crate::rows::{
    catalog_row_identifier_matches, catalog_row_value, catalog_rows, catalog_rows_matching_static,
    catalog_value_bool, catalog_value_f32, catalog_value_i16, catalog_value_i32, catalog_value_oid,
    catalog_value_string, catalog_value_u8, parse_int2_vector, static_catalog_by_name,
    static_catalog_by_relation_oid, static_row_column_bool, static_row_column_i16,
    static_row_column_identifier_matches, static_row_column_oid, static_row_to_catalog_row,
};
#[cfg(test)]
use crate::state::{CATALOG_GENERATION, CatalogState, with_catalog};
use crate::state::{
    CATALOG_LOOKUP_CACHE, CatalogLookupCache, CatalogSnapshot, PRIMARY_KEY_INDEX_OID_CACHE,
    PrimaryKeyIndexOidCache, RelationMeta, current_generation, has_uncommitted_catalog_changes,
    with_visible_catalog_snapshot,
};

pub fn virtual_catalogs() -> &'static [VirtualCatalogRecord] {
    generated_catalog::STATIC_VIRTUAL_CATALOGS
}

pub fn virtual_catalog_by_relation_oid(relation_oid: Oid) -> Option<VirtualCatalogRecord> {
    generated_catalog::STATIC_VIRTUAL_CATALOGS
        .iter()
        .copied()
        .find(|record| record.relation_oid == relation_oid)
}

pub fn virtual_catalog_by_name(name: &str, namespace: Oid) -> Option<VirtualCatalogRecord> {
    if namespace != PG_CATALOG_NAMESPACE_OID {
        return None;
    }
    let name = normalize_identifier(name);
    generated_catalog::STATIC_VIRTUAL_CATALOGS
        .iter()
        .copied()
        .find(|record| record.name == name.as_str())
}

pub fn builtin_operator_by_oid(oid: Oid) -> Option<&'static PgOperatorRecord> {
    generated_catalog::STATIC_OPERATORS
        .iter()
        .find(|record| record.oid == oid)
}

pub fn builtin_operator_by_signature(
    name: &str,
    left_type: Oid,
    right_type: Oid,
    namespace: Oid,
) -> Option<&'static PgOperatorRecord> {
    let name = normalize_identifier(name);
    generated_catalog::STATIC_OPERATORS.iter().find(|record| {
        record.name == name.as_str()
            && record.left_type == left_type
            && record.right_type == right_type
            && record.namespace == namespace
    })
}

pub fn builtin_operators_by_name(name: &str) -> impl Iterator<Item = &'static PgOperatorRecord> {
    let name = normalize_identifier(name);
    generated_catalog::STATIC_OPERATORS
        .iter()
        .filter(move |record| record.name == name.as_str())
}

pub fn builtin_cast_by_source_target(
    source_type: Oid,
    target_type: Oid,
) -> Option<&'static PgCastRecord> {
    generated_catalog::STATIC_CASTS
        .iter()
        .find(|record| record.source_type == source_type && record.target_type == target_type)
}

pub fn builtin_namespaces() -> &'static [PgNamespaceRecord] {
    generated_catalog::STATIC_NAMESPACES
}

pub fn builtin_namespace_by_oid(oid: Oid) -> Option<&'static PgNamespaceRecord> {
    if oid == INFORMATION_SCHEMA_NAMESPACE.oid {
        return Some(&INFORMATION_SCHEMA_NAMESPACE);
    }
    generated_catalog::STATIC_NAMESPACES
        .iter()
        .find(|record| record.oid == oid)
}

pub fn builtin_namespace_by_name(name: &str) -> Option<&'static PgNamespaceRecord> {
    let name = normalize_identifier(name);
    if name == INFORMATION_SCHEMA_NAMESPACE.name {
        return Some(&INFORMATION_SCHEMA_NAMESPACE);
    }
    generated_catalog::STATIC_NAMESPACES
        .iter()
        .find(|record| record.name == name.as_str())
}

pub fn btree_opclass_for_type(type_oid: Oid) -> Option<Oid> {
    static_btree_opclass_for_type(type_oid).or_else(|| {
        let record = lookup_type(type_oid)?;
        (record.typtype == b'e').then(|| static_btree_opclass_for_type(ANYENUM_OID))?
    })
}

fn static_btree_opclass_for_type(type_oid: Oid) -> Option<Oid> {
    let table = static_catalog_by_name("pg_opclass")?;
    table.rows.iter().find_map(|static_row| {
        let row = static_row_to_catalog_row(table, static_row);
        let opcmethod = catalog_row_value(table, &row, "opcmethod").and_then(catalog_value_oid)?;
        let opcintype = catalog_row_value(table, &row, "opcintype").and_then(catalog_value_oid)?;
        let opcdefault =
            catalog_row_value(table, &row, "opcdefault").and_then(catalog_value_bool)?;
        let oid = catalog_row_value(table, &row, "oid").and_then(catalog_value_oid)?;
        (opcmethod == BTREE_INDEX_AM_OID && opcintype == type_oid && opcdefault).then_some(oid)
    })
}

fn primary_key_index_oid_cache() -> &'static Mutex<PrimaryKeyIndexOidCache> {
    PRIMARY_KEY_INDEX_OID_CACHE.get_or_init(|| Mutex::new(PrimaryKeyIndexOidCache::default()))
}

fn primary_key_index_oid_cache_lookup(relation_oid: Oid) -> Option<Option<Oid>> {
    if has_uncommitted_catalog_changes() {
        return None;
    }
    let generation = current_generation();
    let mut cache = primary_key_index_oid_cache()
        .lock()
        .expect("primary key index OID cache mutex poisoned");
    if cache.generation != generation {
        cache.generation = generation;
        cache.entries.clear();
    }
    cache.entries.get(&relation_oid.0).copied()
}

fn primary_key_index_oid_cache_store(relation_oid: Oid, index_oid: Option<Oid>) {
    if has_uncommitted_catalog_changes() {
        return;
    }
    let generation = current_generation();
    let mut cache = primary_key_index_oid_cache()
        .lock()
        .expect("primary key index OID cache mutex poisoned");
    if cache.generation != generation {
        cache.generation = generation;
        cache.entries.clear();
    }
    cache.entries.insert(relation_oid.0, index_oid);
}

fn catalog_lookup_cache() -> &'static Mutex<CatalogLookupCache> {
    CATALOG_LOOKUP_CACHE.get_or_init(|| Mutex::new(CatalogLookupCache::default()))
}

fn with_catalog_lookup_cache<R>(f: impl FnOnce(&mut CatalogLookupCache) -> R) -> R {
    let generation = current_generation();
    let mut cache = catalog_lookup_cache()
        .lock()
        .expect("catalog lookup cache mutex poisoned");
    if cache.generation != generation {
        cache.generation = generation;
        cache.relation_columns.clear();
        cache.index_records_by_oid.clear();
        cache.index_records_by_relation.clear();
        cache.unique_index_records_by_relation.clear();
    }
    f(&mut cache)
}

fn relation_column_cache_lookup(relation_oid: Oid, attnum: i16) -> Option<Option<ColumnRecord>> {
    if has_uncommitted_catalog_changes() {
        return None;
    }
    with_catalog_lookup_cache(|cache| {
        cache
            .relation_columns
            .get(&(relation_oid.0, attnum))
            .cloned()
    })
}

fn relation_column_cache_store(relation_oid: Oid, attnum: i16, column: Option<ColumnRecord>) {
    if has_uncommitted_catalog_changes() {
        return;
    }
    with_catalog_lookup_cache(|cache| {
        cache
            .relation_columns
            .insert((relation_oid.0, attnum), column);
    });
}

fn index_record_by_oid_cache_lookup(index_oid: Oid) -> Option<Option<IndexRecord>> {
    if has_uncommitted_catalog_changes() {
        return None;
    }
    with_catalog_lookup_cache(|cache| cache.index_records_by_oid.get(&index_oid.0).cloned())
}

fn index_record_by_oid_cache_store(index_oid: Oid, record: Option<IndexRecord>) {
    if has_uncommitted_catalog_changes() {
        return;
    }
    with_catalog_lookup_cache(|cache| {
        cache.index_records_by_oid.insert(index_oid.0, record);
    });
}

fn index_records_by_relation_cache_lookup(relation_oid: Oid) -> Option<Vec<IndexRecord>> {
    if has_uncommitted_catalog_changes() {
        return None;
    }
    with_catalog_lookup_cache(|cache| {
        cache
            .index_records_by_relation
            .get(&relation_oid.0)
            .cloned()
    })
}

fn index_records_by_relation_cache_store(relation_oid: Oid, records: Vec<IndexRecord>) {
    if has_uncommitted_catalog_changes() {
        return;
    }
    with_catalog_lookup_cache(|cache| {
        cache
            .index_records_by_relation
            .insert(relation_oid.0, records);
    });
}

fn unique_index_records_by_relation_cache_lookup(relation_oid: Oid) -> Option<Vec<IndexRecord>> {
    if has_uncommitted_catalog_changes() {
        return None;
    }
    with_catalog_lookup_cache(|cache| {
        cache
            .unique_index_records_by_relation
            .get(&relation_oid.0)
            .cloned()
    })
}

fn unique_index_records_by_relation_cache_store(relation_oid: Oid, records: Vec<IndexRecord>) {
    if has_uncommitted_catalog_changes() {
        return;
    }
    with_catalog_lookup_cache(|cache| {
        cache
            .unique_index_records_by_relation
            .insert(relation_oid.0, records);
    });
}

#[cfg(test)]
pub(crate) fn clear_catalog_lookup_caches() {
    if let Some(cache) = PRIMARY_KEY_INDEX_OID_CACHE.get() {
        let mut cache = cache
            .lock()
            .expect("primary key index OID cache mutex poisoned");
        cache.generation = current_generation();
        cache.entries.clear();
    }
    if let Some(cache) = CATALOG_LOOKUP_CACHE.get() {
        let mut cache = cache.lock().expect("catalog lookup cache mutex poisoned");
        cache.generation = current_generation();
        cache.relation_columns.clear();
        cache.index_records_by_oid.clear();
        cache.index_records_by_relation.clear();
        cache.unique_index_records_by_relation.clear();
    }
}

fn relation_pg_class_row_by_oid(oid: Oid) -> Option<CatalogRow> {
    let table = static_catalog_by_relation_oid(PG_CLASS_RELATION_OID)?;
    catalog_rows_matching_static(
        table.oid,
        |table, row| static_row_column_oid(table, row, "oid").is_some_and(|row_oid| row_oid == oid),
        |row| {
            catalog_row_value(table, row, "oid")
                .and_then(catalog_value_oid)
                .is_some_and(|row_oid| row_oid == oid)
        },
    )
    .into_iter()
    .next()
}

fn pg_class_row_oid_by_name_in_namespace(
    table: &StaticCatalogTable,
    row: &CatalogRow,
    name: &str,
    namespace: Oid,
) -> Option<Oid> {
    let row_name_matches = catalog_row_identifier_matches(table, row, "relname", name);
    let row_namespace_matches = catalog_row_value(table, row, "relnamespace")
        .and_then(catalog_value_oid)
        .is_some_and(|row_namespace| row_namespace == namespace);
    if !row_name_matches || !row_namespace_matches {
        return None;
    }
    catalog_row_value(table, row, "oid").and_then(catalog_value_oid)
}

pub(crate) fn catalog_value_int2_vector(value: &CatalogValue) -> Option<Vec<i16>> {
    match value {
        CatalogValue::Int2Vector(values) => Some(values.clone()),
        CatalogValue::Raw(value) | CatalogValue::Text(value) | CatalogValue::Name(value) => {
            Some(parse_int2_vector(value))
        }
        _ => None,
    }
}

fn relation_columns_from_pg_attribute(relation_oid: Oid) -> Vec<ColumnRecord> {
    let Some(table) = static_catalog_by_relation_oid(PG_ATTRIBUTE_RELATION_OID) else {
        return Vec::new();
    };
    let mut columns = catalog_rows_matching_static(
        table.oid,
        |table, row| {
            static_row_column_oid(table, row, "attrelid")
                .is_some_and(|attrelid| attrelid == relation_oid)
        },
        |row| {
            catalog_row_value(table, row, "attrelid")
                .and_then(catalog_value_oid)
                .is_some_and(|attrelid| attrelid == relation_oid)
        },
    )
    .into_iter()
    .filter_map(|row| column_record_from_pg_attribute_row(table, &row, relation_oid))
    .collect::<Vec<_>>();
    columns.sort_by_key(|(attnum, _)| *attnum);
    columns.into_iter().map(|(_, column)| column).collect()
}

pub(crate) fn column_record_from_pg_attribute_row(
    table: &StaticCatalogTable,
    row: &CatalogRow,
    relation_oid: Oid,
) -> Option<(i16, ColumnRecord)> {
    let attrelid = catalog_row_value(table, row, "attrelid").and_then(catalog_value_oid)?;
    if attrelid != relation_oid {
        return None;
    }
    let attnum = catalog_row_value(table, row, "attnum").and_then(catalog_value_i16)?;
    if attnum <= 0 {
        return None;
    }
    let attisdropped = catalog_row_value(table, row, "attisdropped")
        .and_then(catalog_value_bool)
        .unwrap_or(false);
    if attisdropped {
        return None;
    }
    let name = catalog_row_value(table, row, "attname").and_then(catalog_value_string)?;
    let type_oid = catalog_row_value(table, row, "atttypid").and_then(catalog_value_oid)?;
    let type_mod = catalog_row_value(table, row, "atttypmod")
        .and_then(catalog_value_i32)
        .unwrap_or(-1);
    let is_not_null = catalog_row_value(table, row, "attnotnull")
        .and_then(catalog_value_bool)
        .unwrap_or(false);
    let has_default = catalog_row_value(table, row, "atthasdef")
        .and_then(catalog_value_bool)
        .unwrap_or(false);
    let generated = catalog_row_value(table, row, "attgenerated")
        .and_then(catalog_value_u8)
        .unwrap_or(0);
    Some((
        attnum,
        ColumnRecord {
            name: normalize_identifier(&name),
            type_oid,
            type_mod,
            is_not_null,
            has_default,
            generated,
        },
    ))
}

fn physical_column_record_from_pg_attribute_row(
    table: &StaticCatalogTable,
    row: &CatalogRow,
    relation_oid: Oid,
) -> Option<(i16, PhysicalColumnRecord)> {
    let attrelid = catalog_row_value(table, row, "attrelid").and_then(catalog_value_oid)?;
    if attrelid != relation_oid {
        return None;
    }
    let attnum = catalog_row_value(table, row, "attnum").and_then(catalog_value_i16)?;
    if attnum <= 0 {
        return None;
    }
    let is_dropped = catalog_row_value(table, row, "attisdropped")
        .and_then(catalog_value_bool)
        .unwrap_or(false);
    let name = catalog_row_value(table, row, "attname").and_then(catalog_value_string)?;
    let type_oid = catalog_row_value(table, row, "atttypid")
        .and_then(catalog_value_oid)
        .unwrap_or(INVALID_OID);
    let type_mod = catalog_row_value(table, row, "atttypmod")
        .and_then(catalog_value_i32)
        .unwrap_or(-1);
    let is_not_null = !is_dropped
        && catalog_row_value(table, row, "attnotnull")
            .and_then(catalog_value_bool)
            .unwrap_or(false);
    let has_default = !is_dropped
        && catalog_row_value(table, row, "atthasdef")
            .and_then(catalog_value_bool)
            .unwrap_or(false);
    let generated = if is_dropped {
        0
    } else {
        catalog_row_value(table, row, "attgenerated")
            .and_then(catalog_value_u8)
            .unwrap_or(0)
    };
    let attlen = catalog_row_value(table, row, "attlen")
        .and_then(catalog_value_i16)
        .unwrap_or(0);
    let attbyval = catalog_row_value(table, row, "attbyval")
        .and_then(catalog_value_bool)
        .unwrap_or(false);
    let attalign = catalog_row_value(table, row, "attalign")
        .and_then(catalog_value_u8)
        .unwrap_or(b'i');
    let attstorage = catalog_row_value(table, row, "attstorage")
        .and_then(catalog_value_u8)
        .unwrap_or(b'p');
    let attcollation = catalog_row_value(table, row, "attcollation")
        .and_then(catalog_value_oid)
        .unwrap_or(INVALID_OID);

    Some((
        attnum,
        PhysicalColumnRecord {
            name: normalize_identifier(&name),
            type_oid,
            type_mod,
            is_not_null,
            has_default,
            generated,
            is_dropped,
            attlen,
            attbyval,
            attalign,
            attstorage,
            attcollation,
        },
    ))
}

pub fn relation_column_by_attnum(relation_oid: Oid, attnum: i16) -> Option<ColumnRecord> {
    if let Some(cached) = relation_column_cache_lookup(relation_oid, attnum) {
        return cached;
    }
    let result = relation_column_by_attnum_uncached(relation_oid, attnum);
    relation_column_cache_store(relation_oid, attnum, result.clone());
    result
}

fn relation_column_by_attnum_uncached(relation_oid: Oid, attnum: i16) -> Option<ColumnRecord> {
    if attnum <= 0 {
        return None;
    }
    if let Some(column) = with_visible_catalog_snapshot(|snapshot| {
        relation_column_from_snapshot(snapshot, relation_oid, attnum)
    }) {
        return Some(column);
    }
    let table = static_catalog_by_relation_oid(PG_ATTRIBUTE_RELATION_OID)?;
    catalog_rows_matching_static(
        table.oid,
        |table, row| {
            static_row_column_oid(table, row, "attrelid")
                .is_some_and(|attrelid| attrelid == relation_oid)
                && static_row_column_i16(table, row, "attnum")
                    .is_some_and(|row_attnum| row_attnum == attnum)
        },
        |row| {
            let relation_matches = catalog_row_value(table, row, "attrelid")
                .and_then(catalog_value_oid)
                .is_some_and(|attrelid| attrelid == relation_oid);
            let attnum_matches = catalog_row_value(table, row, "attnum")
                .and_then(catalog_value_i16)
                .is_some_and(|row_attnum| row_attnum == attnum);
            relation_matches && attnum_matches
        },
    )
    .into_iter()
    .find_map(|row| {
        let (row_attnum, column) = column_record_from_pg_attribute_row(table, &row, relation_oid)?;
        (row_attnum == attnum).then_some(column)
    })
}

pub fn relation_physical_column_by_attnum(
    relation_oid: Oid,
    attnum: i16,
) -> Option<PhysicalColumnRecord> {
    if attnum <= 0 {
        return None;
    }
    let table = static_catalog_by_relation_oid(PG_ATTRIBUTE_RELATION_OID)?;
    catalog_rows_matching_static(
        table.oid,
        |table, row| {
            static_row_column_oid(table, row, "attrelid")
                .is_some_and(|attrelid| attrelid == relation_oid)
                && static_row_column_i16(table, row, "attnum")
                    .is_some_and(|row_attnum| row_attnum == attnum)
        },
        |row| {
            let relation_matches = catalog_row_value(table, row, "attrelid")
                .and_then(catalog_value_oid)
                .is_some_and(|attrelid| attrelid == relation_oid);
            let attnum_matches = catalog_row_value(table, row, "attnum")
                .and_then(catalog_value_i16)
                .is_some_and(|row_attnum| row_attnum == attnum);
            relation_matches && attnum_matches
        },
    )
    .into_iter()
    .find_map(|row| {
        let (row_attnum, column) =
            physical_column_record_from_pg_attribute_row(table, &row, relation_oid)?;
        (row_attnum == attnum).then_some(column)
    })
}

pub(crate) fn pg_index_record_from_row(row: &CatalogRow) -> Option<IndexRecord> {
    let table = static_catalog_by_relation_oid(PG_INDEX_RELATION_OID)?;
    let index_oid = catalog_row_value(table, row, "indexrelid").and_then(catalog_value_oid)?;
    let relation_oid = catalog_row_value(table, row, "indrelid").and_then(catalog_value_oid)?;
    let indnkeyatts = catalog_row_value(table, row, "indnkeyatts")
        .and_then(catalog_value_i16)
        .unwrap_or(0)
        .max(0) as usize;
    let mut key_attnums = catalog_row_value(table, row, "indkey")
        .and_then(catalog_value_int2_vector)
        .unwrap_or_default();
    if indnkeyatts > 0 && key_attnums.len() > indnkeyatts {
        key_attnums.truncate(indnkeyatts);
    }

    Some(IndexRecord {
        index_oid,
        relation_oid,
        key_attnums,
        is_unique: catalog_row_value(table, row, "indisunique")
            .and_then(catalog_value_bool)
            .unwrap_or(false),
        nulls_not_distinct: catalog_row_value(table, row, "indnullsnotdistinct")
            .and_then(catalog_value_bool)
            .unwrap_or(false),
        is_primary: catalog_row_value(table, row, "indisprimary")
            .and_then(catalog_value_bool)
            .unwrap_or(false),
        is_immediate: catalog_row_value(table, row, "indimmediate")
            .and_then(catalog_value_bool)
            .unwrap_or(true),
        is_valid: catalog_row_value(table, row, "indisvalid")
            .and_then(catalog_value_bool)
            .unwrap_or(true),
        is_ready: catalog_row_value(table, row, "indisready")
            .and_then(catalog_value_bool)
            .unwrap_or(true),
        is_live: catalog_row_value(table, row, "indislive")
            .and_then(catalog_value_bool)
            .unwrap_or(true),
    })
}

pub fn index_record_by_index_oid(index_oid: Oid) -> Option<IndexRecord> {
    if let Some(index) = with_visible_catalog_snapshot(|snapshot| {
        snapshot
            .index_meta_by_oid(index_oid)
            .map(|index| index.record.clone())
    }) {
        return Some(index);
    }
    if let Some(cached) = index_record_by_oid_cache_lookup(index_oid) {
        return cached;
    }
    let result = index_record_by_index_oid_uncached(index_oid);
    index_record_by_oid_cache_store(index_oid, result.clone());
    result
}

fn index_record_by_index_oid_uncached(index_oid: Oid) -> Option<IndexRecord> {
    let table = static_catalog_by_relation_oid(PG_INDEX_RELATION_OID)?;
    catalog_rows_matching_static(
        table.oid,
        |table, row| {
            static_row_column_oid(table, row, "indexrelid")
                .is_some_and(|row_index_oid| row_index_oid == index_oid)
        },
        |row| {
            catalog_row_value(table, row, "indexrelid")
                .and_then(catalog_value_oid)
                .is_some_and(|row_index_oid| row_index_oid == index_oid)
        },
    )
    .into_iter()
    .find_map(|row| {
        let record = pg_index_record_from_row(&row)?;
        (record.index_oid == index_oid).then_some(record)
    })
}

pub fn index_records_for_relation_oid(relation_oid: Oid) -> Vec<IndexRecord> {
    let typed_indexes = with_visible_catalog_snapshot(|snapshot| {
        snapshot
            .index_metas_for_relation(relation_oid)
            .into_iter()
            .map(|index| index.record.clone())
            .collect::<Vec<_>>()
    });
    if !typed_indexes.is_empty() {
        return typed_indexes;
    }
    if let Some(cached) = index_records_by_relation_cache_lookup(relation_oid) {
        return cached;
    }
    let result = index_records_for_relation_oid_uncached(relation_oid);
    index_records_by_relation_cache_store(relation_oid, result.clone());
    result
}

fn index_records_for_relation_oid_uncached(relation_oid: Oid) -> Vec<IndexRecord> {
    let Some(table) = static_catalog_by_relation_oid(PG_INDEX_RELATION_OID) else {
        return Vec::new();
    };
    catalog_rows_matching_static(
        table.oid,
        |table, row| {
            static_row_column_oid(table, row, "indrelid")
                .is_some_and(|indrelid| indrelid == relation_oid)
        },
        |row| {
            catalog_row_value(table, row, "indrelid")
                .and_then(catalog_value_oid)
                .is_some_and(|indrelid| indrelid == relation_oid)
        },
    )
    .into_iter()
    .filter_map(|row| {
        let record = pg_index_record_from_row(&row)?;
        (record.relation_oid == relation_oid).then_some(record)
    })
    .collect()
}

pub fn unique_index_records_for_relation_oid(relation_oid: Oid) -> Vec<IndexRecord> {
    if let Some(cached) = unique_index_records_by_relation_cache_lookup(relation_oid) {
        return cached;
    }
    let result: Vec<_> = index_records_for_relation_oid(relation_oid)
        .into_iter()
        .filter(|record| record.is_unique && record.is_valid && record.is_ready && record.is_live)
        .collect();
    unique_index_records_by_relation_cache_store(relation_oid, result.clone());
    result
}

pub fn unique_index_oids_for_relation_oid(relation_oid: Oid) -> Vec<Oid> {
    let typed_oids = with_visible_catalog_snapshot(|snapshot| {
        snapshot
            .index_metas_for_relation(relation_oid)
            .into_iter()
            .filter(|index| {
                index.record.is_unique
                    && index.record.is_valid
                    && index.record.is_ready
                    && index.record.is_live
            })
            .map(|index| index.record.index_oid)
            .collect::<Vec<_>>()
    });
    if !typed_oids.is_empty() {
        return typed_oids;
    }
    let Some(table) = static_catalog_by_relation_oid(PG_INDEX_RELATION_OID) else {
        return Vec::new();
    };
    catalog_rows_matching_static(
        table.oid,
        |table, row| {
            static_row_column_oid(table, row, "indrelid")
                .is_some_and(|indrelid| indrelid == relation_oid)
                && static_row_column_bool(table, row, "indisunique").unwrap_or(false)
                && static_row_column_bool(table, row, "indisvalid").unwrap_or(true)
                && static_row_column_bool(table, row, "indisready").unwrap_or(true)
                && static_row_column_bool(table, row, "indislive").unwrap_or(true)
        },
        |row| {
            let relation_matches = catalog_row_value(table, row, "indrelid")
                .and_then(catalog_value_oid)
                .is_some_and(|indrelid| indrelid == relation_oid);
            let is_unique = catalog_row_value(table, row, "indisunique")
                .and_then(catalog_value_bool)
                .unwrap_or(false);
            let is_valid = catalog_row_value(table, row, "indisvalid")
                .and_then(catalog_value_bool)
                .unwrap_or(true);
            let is_ready = catalog_row_value(table, row, "indisready")
                .and_then(catalog_value_bool)
                .unwrap_or(true);
            let is_live = catalog_row_value(table, row, "indislive")
                .and_then(catalog_value_bool)
                .unwrap_or(true);
            relation_matches && is_unique && is_valid && is_ready && is_live
        },
    )
    .into_iter()
    .filter_map(|row| catalog_row_value(table, &row, "indexrelid").and_then(catalog_value_oid))
    .collect()
}

pub fn relation_oid_for_index_oid(index_oid: Oid) -> Option<Oid> {
    index_record_by_index_oid(index_oid).map(|record| record.relation_oid)
}

fn primary_key_pg_index_row(relation_oid: Oid) -> Option<CatalogRow> {
    let table = static_catalog_by_relation_oid(PG_INDEX_RELATION_OID)?;
    catalog_rows_matching_static(
        table.oid,
        |table, row| {
            static_row_column_oid(table, row, "indrelid")
                .is_some_and(|indrelid| indrelid == relation_oid)
                && static_row_column_bool(table, row, "indisprimary").unwrap_or(false)
        },
        |row| {
            pg_index_record_from_row(row)
                .is_some_and(|record| record.relation_oid == relation_oid && record.is_primary)
        },
    )
    .into_iter()
    .next()
}

pub fn primary_key_index_oid_for_relation_oid(relation_oid: Oid) -> Option<Oid> {
    if let Some(cached) = primary_key_index_oid_cache_lookup(relation_oid) {
        return cached;
    }
    let result = primary_key_index_oid_for_relation_oid_uncached(relation_oid);
    primary_key_index_oid_cache_store(relation_oid, result);
    result
}

fn primary_key_index_oid_for_relation_oid_uncached(relation_oid: Oid) -> Option<Oid> {
    if let Some(index) = with_visible_catalog_snapshot(|snapshot| {
        primary_key_index_record_from_snapshot(snapshot, relation_oid)
    }) {
        return Some(index.index_oid);
    }
    primary_key_pg_index_row(relation_oid)
        .and_then(|row| pg_index_record_from_row(&row))
        .map(|record| record.index_oid)
}

pub fn primary_key_index_record_for_relation_oid(relation_oid: Oid) -> Option<IndexRecord> {
    if let Some(index) = with_visible_catalog_snapshot(|snapshot| {
        primary_key_index_record_from_snapshot(snapshot, relation_oid)
    }) {
        return Some(index);
    }
    let row = primary_key_pg_index_row(relation_oid)?;
    let record = pg_index_record_from_row(&row)?;
    (record.relation_oid == relation_oid && record.is_primary).then_some(record)
}

pub fn primary_key_relation_oid_for_index_oid(index_oid: Oid) -> Option<Oid> {
    let record = index_record_by_index_oid(index_oid)?;
    record.is_primary.then_some(record.relation_oid)
}

fn primary_key_constraint_oid_for_relation(
    relation_oid: Oid,
    index_oid: Option<Oid>,
) -> Option<Oid> {
    if let Some(oid) = with_visible_catalog_snapshot(|snapshot| {
        primary_key_constraint_oid_from_snapshot(snapshot, relation_oid, index_oid)
    }) {
        return Some(oid);
    }
    let table = static_catalog_by_relation_oid(PG_CONSTRAINT_RELATION_OID)?;
    catalog_rows_matching_static(
        table.oid,
        |table, row| {
            static_row_column_oid(table, row, "conrelid")
                .is_some_and(|conrelid| conrelid == relation_oid)
        },
        |row| {
            catalog_row_value(table, row, "conrelid")
                .and_then(catalog_value_oid)
                .is_some_and(|conrelid| conrelid == relation_oid)
        },
    )
    .into_iter()
    .find_map(|row| {
        let conrelid = catalog_row_value(table, &row, "conrelid").and_then(catalog_value_oid)?;
        if conrelid != relation_oid {
            return None;
        }
        let contype = catalog_row_value(table, &row, "contype").and_then(catalog_value_u8)?;
        if contype != b'p' {
            return None;
        }
        if let Some(index_oid) = index_oid {
            let conindid = catalog_row_value(table, &row, "conindid").and_then(catalog_value_oid);
            if conindid.is_some_and(|conindid| conindid != index_oid) {
                return None;
            }
        }
        catalog_row_value(table, &row, "oid").and_then(catalog_value_oid)
    })
}

fn relation_primary_key_from_pg_index(
    relation_oid: Oid,
    _columns: &[ColumnRecord],
) -> (Vec<String>, Option<Oid>) {
    let Some(table) = static_catalog_by_relation_oid(PG_INDEX_RELATION_OID) else {
        return (Vec::new(), None);
    };
    let Some(row) = primary_key_pg_index_row(relation_oid) else {
        return (
            Vec::new(),
            primary_key_constraint_oid_for_relation(relation_oid, None),
        );
    };
    let index_oid = catalog_row_value(table, &row, "indexrelid").and_then(catalog_value_oid);
    let indkey = catalog_row_value(table, &row, "indkey")
        .and_then(catalog_value_int2_vector)
        .unwrap_or_default();
    let primary_key = indkey
        .into_iter()
        .filter_map(|attnum| {
            relation_column_by_attnum(relation_oid, attnum).map(|column| column.name)
        })
        .collect::<Vec<_>>();
    let constraint_oid = primary_key_constraint_oid_for_relation(relation_oid, index_oid);
    (primary_key, constraint_oid)
}

fn relation_columns_from_snapshot(
    snapshot: &CatalogSnapshot,
    relation_oid: Oid,
) -> Vec<ColumnRecord> {
    snapshot
        .columns_by_relation
        .get(&relation_oid.0)
        .into_iter()
        .flat_map(|columns| columns.values())
        .filter_map(|row_id| snapshot.columns.get(row_id))
        .map(|column| column.record.clone())
        .collect()
}

fn relation_column_from_snapshot(
    snapshot: &CatalogSnapshot,
    relation_oid: Oid,
    attnum: i16,
) -> Option<ColumnRecord> {
    snapshot
        .columns_by_relation
        .get(&relation_oid.0)?
        .get(&attnum)
        .and_then(|row_id| snapshot.columns.get(row_id))
        .map(|column| column.record.clone())
}

fn primary_key_index_record_from_snapshot(
    snapshot: &CatalogSnapshot,
    relation_oid: Oid,
) -> Option<IndexRecord> {
    snapshot
        .index_metas_for_relation(relation_oid)
        .into_iter()
        .find(|index| index.record.is_primary)
        .map(|index| index.record.clone())
}

fn primary_key_constraint_oid_from_snapshot(
    snapshot: &CatalogSnapshot,
    relation_oid: Oid,
    index_oid: Option<Oid>,
) -> Option<Oid> {
    snapshot
        .constraint_metas_for_relation(relation_oid)
        .into_iter()
        .find_map(|constraint| {
            if constraint.kind != b'p' {
                return None;
            }
            if let Some(index_oid) = index_oid
                && constraint.index_oid != INVALID_OID
                && constraint.index_oid != index_oid
            {
                return None;
            }
            Some(constraint.oid)
        })
}

fn relation_primary_key_from_snapshot(
    snapshot: &CatalogSnapshot,
    relation_oid: Oid,
    columns: &[ColumnRecord],
) -> (Vec<String>, Option<Oid>) {
    let Some(index) = primary_key_index_record_from_snapshot(snapshot, relation_oid) else {
        return (
            Vec::new(),
            primary_key_constraint_oid_from_snapshot(snapshot, relation_oid, None),
        );
    };
    let primary_key = index
        .key_attnums
        .iter()
        .filter_map(|attnum| {
            if *attnum <= 0 {
                return None;
            }
            columns
                .get(usize::try_from(*attnum - 1).ok()?)
                .map(|column| column.name.clone())
        })
        .collect::<Vec<_>>();
    let constraint_oid =
        primary_key_constraint_oid_from_snapshot(snapshot, relation_oid, Some(index.index_oid));
    (primary_key, constraint_oid)
}

fn relation_record_from_meta(
    snapshot: &CatalogSnapshot,
    relation: &RelationMeta,
) -> RelationRecord {
    let columns = relation_columns_from_snapshot(snapshot, relation.oid);
    let (primary_key, primary_key_constraint_oid) =
        relation_primary_key_from_snapshot(snapshot, relation.oid, &columns);
    RelationRecord {
        row_id: relation.row_id,
        oid: relation.oid,
        type_oid: relation.type_oid,
        namespace: relation.namespace,
        owner: relation.owner,
        name: relation.name.clone(),
        relkind: relation.relkind,
        columns,
        primary_key,
        primary_key_constraint_oid,
    }
}

fn relation_summary_from_meta(
    snapshot: &CatalogSnapshot,
    relation: &RelationMeta,
) -> RelationSummaryRecord {
    let typed_columns = relation_columns_from_snapshot(snapshot, relation.oid);
    let primary_key = primary_key_index_record_from_snapshot(snapshot, relation.oid);
    let has_primary_key = primary_key.is_some();
    let has_indexes = relation.relhasindex
        || has_primary_key
        || !snapshot.index_metas_for_relation(relation.oid).is_empty();
    RelationSummaryRecord {
        row_id: relation.row_id,
        oid: relation.oid,
        type_oid: relation.type_oid,
        namespace: relation.namespace,
        owner: relation.owner,
        name: relation.name.clone(),
        relkind: relation.relkind,
        column_count: relation
            .relnatts
            .max(typed_columns.len().min(u16::MAX as usize) as u16),
        has_primary_key,
        has_indexes,
    }
}

fn relation_record_from_pg_class_row(row: &CatalogRow) -> Option<RelationRecord> {
    let table = static_catalog_by_relation_oid(PG_CLASS_RELATION_OID)?;
    let oid = catalog_row_value(table, row, "oid").and_then(catalog_value_oid)?;
    let type_oid = catalog_row_value(table, row, "reltype")
        .and_then(catalog_value_oid)
        .unwrap_or(INVALID_OID);
    let namespace = catalog_row_value(table, row, "relnamespace")
        .and_then(catalog_value_oid)
        .unwrap_or(PUBLIC_NAMESPACE_OID);
    let owner = catalog_row_value(table, row, "relowner")
        .and_then(catalog_value_oid)
        .unwrap_or(POSTGRES_ROLE_OID);
    let name = catalog_row_value(table, row, "relname").and_then(catalog_value_string)?;
    let relkind = catalog_row_value(table, row, "relkind")
        .and_then(catalog_value_u8)
        .unwrap_or(b'r');
    let columns = relation_columns_from_pg_attribute(oid);
    let (primary_key, primary_key_constraint_oid) =
        relation_primary_key_from_pg_index(oid, &columns);
    Some(RelationRecord {
        row_id: row.row_id,
        oid,
        type_oid,
        namespace,
        owner,
        name: normalize_identifier(&name),
        relkind,
        columns,
        primary_key,
        primary_key_constraint_oid,
    })
}

fn relation_summary_from_pg_class_row(row: &CatalogRow) -> Option<RelationSummaryRecord> {
    let table = static_catalog_by_relation_oid(PG_CLASS_RELATION_OID)?;
    let oid = catalog_row_value(table, row, "oid").and_then(catalog_value_oid)?;
    let type_oid = catalog_row_value(table, row, "reltype")
        .and_then(catalog_value_oid)
        .unwrap_or(INVALID_OID);
    let namespace = catalog_row_value(table, row, "relnamespace")
        .and_then(catalog_value_oid)
        .unwrap_or(PUBLIC_NAMESPACE_OID);
    let owner = catalog_row_value(table, row, "relowner")
        .and_then(catalog_value_oid)
        .unwrap_or(POSTGRES_ROLE_OID);
    let name = catalog_row_value(table, row, "relname").and_then(catalog_value_string)?;
    let relkind = catalog_row_value(table, row, "relkind")
        .and_then(catalog_value_u8)
        .unwrap_or(b'r');
    let column_count = catalog_row_value(table, row, "relnatts")
        .and_then(catalog_value_i16)
        .map(|value| value.max(0) as usize)
        .unwrap_or_else(|| relation_columns_from_pg_attribute(oid).len())
        .min(u16::MAX as usize) as u16;
    let has_primary_key = primary_key_index_oid_for_relation_oid(oid).is_some();
    let has_indexes = catalog_row_value(table, row, "relhasindex")
        .and_then(catalog_value_bool)
        .unwrap_or_else(|| !index_records_for_relation_oid(oid).is_empty())
        || has_primary_key;

    Some(RelationSummaryRecord {
        row_id: row.row_id,
        oid,
        type_oid,
        namespace,
        owner,
        name: normalize_identifier(&name),
        relkind,
        column_count,
        has_primary_key,
        has_indexes,
    })
}

pub fn relation_by_name(name: &str) -> Option<RelationRecord> {
    let name = normalize_identifier(name);
    if let Some(relation) = with_visible_catalog_snapshot(|snapshot| {
        let mut matches = snapshot
            .relations
            .values()
            .filter(|relation| relation.name == name)
            .map(|relation| relation_record_from_meta(snapshot, relation))
            .collect::<Vec<_>>();
        matches.sort_by_key(|relation| match relation.namespace {
            PUBLIC_NAMESPACE_OID => 0,
            PG_CATALOG_NAMESPACE_OID => 1,
            _ => 2,
        });
        matches.into_iter().next()
    }) {
        return Some(relation);
    }
    let table = static_catalog_by_relation_oid(PG_CLASS_RELATION_OID)?;
    let mut matches = catalog_rows_matching_static(
        table.oid,
        |table, row| static_row_column_identifier_matches(table, row, "relname", &name),
        |row| catalog_row_identifier_matches(table, row, "relname", &name),
    )
    .into_iter()
    .filter_map(|row| {
        if !catalog_row_identifier_matches(table, &row, "relname", &name) {
            return None;
        }
        relation_record_from_pg_class_row(&row)
    })
    .collect::<Vec<_>>();
    matches.sort_by_key(|relation| match relation.namespace {
        PUBLIC_NAMESPACE_OID => 0,
        PG_CATALOG_NAMESPACE_OID => 1,
        _ => 2,
    });
    matches.into_iter().next()
}

pub fn relation_by_name_in_namespace(name: &str, namespace: Oid) -> Option<RelationRecord> {
    let name = normalize_identifier(name);
    if let Some(relation) = with_visible_catalog_snapshot(|snapshot| {
        snapshot
            .relation_meta_by_name(&name, namespace)
            .map(|relation| relation_record_from_meta(snapshot, relation))
    }) {
        return Some(relation);
    }
    let table = static_catalog_by_relation_oid(PG_CLASS_RELATION_OID)?;
    catalog_rows_matching_static(
        table.oid,
        |table, row| {
            static_row_column_identifier_matches(table, row, "relname", &name)
                && static_row_column_oid(table, row, "relnamespace")
                    .is_some_and(|row_namespace| row_namespace == namespace)
        },
        |row| {
            let row_name = catalog_row_identifier_matches(table, row, "relname", &name);
            let row_namespace = catalog_row_value(table, row, "relnamespace")
                .and_then(catalog_value_oid)
                .is_some_and(|row_namespace| row_namespace == namespace);
            row_name && row_namespace
        },
    )
    .into_iter()
    .find_map(|row| {
        if !catalog_row_identifier_matches(table, &row, "relname", &name) {
            return None;
        }
        let row_namespace =
            catalog_row_value(table, &row, "relnamespace").and_then(catalog_value_oid)?;
        if row_namespace != namespace {
            return None;
        }
        relation_record_from_pg_class_row(&row)
    })
}

pub fn relation_summary_by_name_in_namespace(
    name: &str,
    namespace: Oid,
) -> Option<RelationSummaryRecord> {
    let name = normalize_identifier(name);
    if let Some(relation) = with_visible_catalog_snapshot(|snapshot| {
        snapshot
            .relation_meta_by_name(&name, namespace)
            .map(|relation| relation_summary_from_meta(snapshot, relation))
    }) {
        return Some(relation);
    }
    let table = static_catalog_by_relation_oid(PG_CLASS_RELATION_OID)?;
    catalog_rows_matching_static(
        table.oid,
        |table, row| {
            static_row_column_identifier_matches(table, row, "relname", &name)
                && static_row_column_oid(table, row, "relnamespace")
                    .is_some_and(|row_namespace| row_namespace == namespace)
        },
        |row| {
            let row_name = catalog_row_identifier_matches(table, row, "relname", &name);
            let row_namespace = catalog_row_value(table, row, "relnamespace")
                .and_then(catalog_value_oid)
                .is_some_and(|row_namespace| row_namespace == namespace);
            row_name && row_namespace
        },
    )
    .into_iter()
    .find_map(|row| {
        if !catalog_row_identifier_matches(table, &row, "relname", &name) {
            return None;
        }
        let row_namespace =
            catalog_row_value(table, &row, "relnamespace").and_then(catalog_value_oid)?;
        if row_namespace != namespace {
            return None;
        }
        relation_summary_from_pg_class_row(&row)
    })
}

pub fn relation_oid_by_name_in_namespace(name: &str, namespace: Oid) -> Option<Oid> {
    let name = normalize_identifier(name);
    if let Some(relation_oid) = with_visible_catalog_snapshot(|snapshot| {
        snapshot
            .relation_meta_by_name(&name, namespace)
            .map(|relation| relation.oid)
    }) {
        return Some(relation_oid);
    }
    let table = static_catalog_by_relation_oid(PG_CLASS_RELATION_OID)?;
    catalog_rows(table.oid)
        .into_iter()
        .find_map(|row| pg_class_row_oid_by_name_in_namespace(table, &row, &name, namespace))
}

pub fn relations() -> Vec<RelationRecord> {
    let mut dynamic_relations = with_visible_catalog_snapshot(|snapshot| {
        snapshot
            .relations
            .values()
            .map(|relation| {
                let record = relation_record_from_meta(snapshot, relation);
                (record.oid.0, record)
            })
            .collect::<BTreeMap<_, _>>()
    });
    let Some(table) = static_catalog_by_relation_oid(PG_CLASS_RELATION_OID) else {
        return dynamic_relations.into_values().collect();
    };
    for relation in catalog_rows(table.oid)
        .into_iter()
        .filter_map(|row| relation_record_from_pg_class_row(&row))
    {
        dynamic_relations.insert(relation.oid.0, relation);
    }
    dynamic_relations.into_values().collect()
}

pub fn relation_by_oid(oid: Oid) -> Option<RelationRecord> {
    if let Some(relation) = with_visible_catalog_snapshot(|snapshot| {
        snapshot
            .relation_meta_by_oid(oid)
            .map(|relation| relation_record_from_meta(snapshot, relation))
    }) {
        return Some(relation);
    }
    relation_pg_class_row_by_oid(oid).and_then(|row| relation_record_from_pg_class_row(&row))
}

pub fn relation_oid_exists(oid: Oid) -> bool {
    relation_by_oid(oid).is_some()
}

pub fn relation_summary_by_oid(oid: Oid) -> Option<RelationSummaryRecord> {
    if let Some(relation) = with_visible_catalog_snapshot(|snapshot| {
        snapshot
            .relation_meta_by_oid(oid)
            .map(|relation| relation_summary_from_meta(snapshot, relation))
    }) {
        return Some(relation);
    }
    relation_pg_class_row_by_oid(oid).and_then(|row| relation_summary_from_pg_class_row(&row))
}

pub fn relation_column_count(name: &str) -> Result<usize, CatalogError> {
    relation_by_name(name)
        .map(|relation| relation.columns.len())
        .ok_or_else(|| {
            CatalogError::new(
                "42P01",
                format!("relation \"{}\" does not exist", normalize_identifier(name)),
            )
        })
}

#[cfg(test)]
pub fn clear_for_tests() {
    with_catalog(|state| {
        *state = CatalogState::default();
        CATALOG_GENERATION.store(state.snapshot.generation, Ordering::Relaxed);
    });
    clear_catalog_lookup_caches();
}

fn catalog_type_from_row(
    table: &StaticCatalogTable,
    row: &CatalogRow,
) -> Option<CatalogTypeRecord> {
    Some(CatalogTypeRecord {
        row_id: row.row_id,
        oid: catalog_row_value(table, row, "oid").and_then(catalog_value_oid)?,
        name: catalog_row_value(table, row, "typname").and_then(catalog_value_string)?,
        namespace: catalog_row_value(table, row, "typnamespace").and_then(catalog_value_oid)?,
        owner: catalog_row_value(table, row, "typowner").and_then(catalog_value_oid)?,
        typlen: catalog_row_value(table, row, "typlen").and_then(catalog_value_i16)?,
        typbyval: catalog_row_value(table, row, "typbyval").and_then(catalog_value_bool)?,
        typalign: catalog_row_value(table, row, "typalign").and_then(catalog_value_u8)?,
        typdelim: catalog_row_value(table, row, "typdelim").and_then(catalog_value_u8)?,
        typinput: catalog_row_value(table, row, "typinput").and_then(catalog_value_oid)?,
        typoutput: catalog_row_value(table, row, "typoutput").and_then(catalog_value_oid)?,
        typreceive: catalog_row_value(table, row, "typreceive").and_then(catalog_value_oid)?,
        typsend: catalog_row_value(table, row, "typsend").and_then(catalog_value_oid)?,
        typmodin: catalog_row_value(table, row, "typmodin").and_then(catalog_value_oid)?,
        typmodout: catalog_row_value(table, row, "typmodout").and_then(catalog_value_oid)?,
        typisdefined: catalog_row_value(table, row, "typisdefined").and_then(catalog_value_bool)?,
        typtype: catalog_row_value(table, row, "typtype").and_then(catalog_value_u8)?,
        typcategory: catalog_row_value(table, row, "typcategory").and_then(catalog_value_u8)?,
        typispreferred: catalog_row_value(table, row, "typispreferred")
            .and_then(catalog_value_bool)?,
        typrelid: catalog_row_value(table, row, "typrelid").and_then(catalog_value_oid)?,
        typelem: catalog_row_value(table, row, "typelem").and_then(catalog_value_oid)?,
        typarray: catalog_row_value(table, row, "typarray").and_then(catalog_value_oid)?,
        typbasetype: catalog_row_value(table, row, "typbasetype").and_then(catalog_value_oid)?,
        typtypmod: catalog_row_value(table, row, "typtypmod").and_then(catalog_value_i32)?,
        typcollation: catalog_row_value(table, row, "typcollation").and_then(catalog_value_oid)?,
        typsubscript: catalog_row_value(table, row, "typsubscript").and_then(catalog_value_oid)?,
        typstorage: catalog_row_value(table, row, "typstorage").and_then(catalog_value_u8)?,
    })
}

pub fn lookup_builtin_type(oid: Oid) -> Option<PgTypeRecord> {
    generated_catalog::STATIC_TYPES
        .iter()
        .find(|record| record.oid == oid)
        .copied()
}

pub(crate) fn canonical_catalog_type_name(name: &str) -> String {
    match normalize_identifier(name).as_str() {
        "boolean" => "bool".to_owned(),
        "character" => "bpchar".to_owned(),
        "character varying" => "varchar".to_owned(),
        "decimal" => "numeric".to_owned(),
        "integer" | "int" => "int4".to_owned(),
        "smallint" => "int2".to_owned(),
        "bigint" => "int8".to_owned(),
        "real" => "float4".to_owned(),
        "double precision" => "float8".to_owned(),
        "time without time zone" => "time".to_owned(),
        "time with time zone" => "timetz".to_owned(),
        "timestamp without time zone" => "timestamp".to_owned(),
        "timestamp with time zone" => "timestamptz".to_owned(),
        "bit varying" => "varbit".to_owned(),
        other => other.to_owned(),
    }
}

pub fn lookup_type(oid: Oid) -> Option<CatalogTypeRecord> {
    if let Some(record) = lookup_builtin_type(oid) {
        return Some(record.into());
    }
    if let Some(pg_type) = with_visible_catalog_snapshot(|snapshot| {
        snapshot
            .type_meta_by_oid(oid)
            .map(|pg_type| pg_type.record.clone())
    }) {
        return Some(pg_type);
    }

    let table = static_catalog_by_relation_oid(PG_TYPE_RELATION_OID)?;
    catalog_rows(PG_TYPE_RELATION_OID)
        .into_iter()
        .find_map(|row| {
            let record = catalog_type_from_row(table, &row)?;
            (record.oid == oid).then_some(record)
        })
}

pub fn type_by_name(name: &str, namespace: Oid) -> Option<CatalogTypeRecord> {
    let canonical_name = canonical_catalog_type_name(name);
    if let Some(pg_type) = with_visible_catalog_snapshot(|snapshot| {
        snapshot
            .type_meta_by_name(&canonical_name, namespace)
            .map(|pg_type| pg_type.record.clone())
    }) {
        return Some(pg_type);
    }
    let table = static_catalog_by_relation_oid(PG_TYPE_RELATION_OID)?;
    catalog_rows(PG_TYPE_RELATION_OID)
        .into_iter()
        .find_map(|row| {
            let record = catalog_type_from_row(table, &row)?;
            (record.namespace == namespace && record.name == canonical_name).then_some(record)
        })
}

pub fn builtin_type_by_name(name: &str, namespace: Oid) -> Option<PgTypeRecord> {
    if namespace != PG_CATALOG_NAMESPACE_OID {
        return None;
    }

    let canonical_name = canonical_catalog_type_name(name);

    generated_catalog::STATIC_TYPES
        .iter()
        .copied()
        .find(|record| record.name == canonical_name.as_str())
}

pub fn enum_oids_by_sort_order(enum_type_oid: Oid) -> Vec<Oid> {
    let Some(table) = static_catalog_by_relation_oid(PG_ENUM_RELATION_OID) else {
        return Vec::new();
    };
    let mut rows = catalog_rows(PG_ENUM_RELATION_OID)
        .into_iter()
        .filter_map(|row| {
            let oid = catalog_row_value(table, &row, "oid").and_then(catalog_value_oid)?;
            let row_type =
                catalog_row_value(table, &row, "enumtypid").and_then(catalog_value_oid)?;
            if row_type != enum_type_oid {
                return None;
            }
            let sort_order =
                catalog_row_value(table, &row, "enumsortorder").and_then(catalog_value_f32)?;
            Some((sort_order, oid))
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.0
            .total_cmp(&right.0)
            .then_with(|| left.1.0.cmp(&right.1.0))
    });
    rows.into_iter().map(|(_, oid)| oid).collect()
}

pub fn builtin_proc_by_oid(oid: Oid) -> Option<&'static PgProcRecord> {
    generated_catalog::STATIC_PROCS
        .iter()
        .find(|record| record.oid == oid)
}

pub fn builtin_procs_by_name(name: &str) -> impl Iterator<Item = &'static PgProcRecord> {
    let name = normalize_identifier(name);
    generated_catalog::STATIC_PROCS
        .iter()
        .filter(move |record| record.name == name.as_str())
}

pub fn builtin_aggregate_by_proc_oid(function_oid: Oid) -> Option<&'static PgAggregateRecord> {
    generated_catalog::STATIC_AGGREGATES
        .iter()
        .find(|record| record.function_oid == function_oid)
}
