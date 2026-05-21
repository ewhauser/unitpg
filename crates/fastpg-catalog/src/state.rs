use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use fastpg_types::Oid;

use crate::lookups::{
    canonical_catalog_type_name, catalog_value_int2_vector, column_record_from_pg_attribute_row,
    pg_index_record_from_row,
};
use crate::model::*;
use crate::rows::{
    catalog_row_value, catalog_value_bool, catalog_value_i16, catalog_value_i32, catalog_value_oid,
    catalog_value_string, catalog_value_u8,
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RelationMeta {
    pub(crate) row_id: u64,
    pub(crate) oid: Oid,
    pub(crate) type_oid: Oid,
    pub(crate) namespace: Oid,
    pub(crate) owner: Oid,
    pub(crate) name: String,
    pub(crate) relkind: u8,
    pub(crate) relnatts: u16,
    pub(crate) relhasindex: bool,
    pub(crate) row: CatalogRow,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ColumnMeta {
    pub(crate) row_id: u64,
    pub(crate) relation_oid: Oid,
    pub(crate) attnum: i16,
    pub(crate) record: ColumnRecord,
    pub(crate) row: CatalogRow,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TypeMeta {
    pub(crate) row_id: u64,
    pub(crate) record: CatalogTypeRecord,
    pub(crate) row: CatalogRow,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct IndexMeta {
    pub(crate) row_id: u64,
    pub(crate) record: IndexRecord,
    pub(crate) row: CatalogRow,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ConstraintMeta {
    pub(crate) row_id: u64,
    pub(crate) oid: Oid,
    pub(crate) name: String,
    pub(crate) namespace: Oid,
    pub(crate) relation_oid: Oid,
    pub(crate) index_oid: Oid,
    pub(crate) kind: u8,
    pub(crate) key_attnums: Vec<i16>,
    pub(crate) row: CatalogRow,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NamespaceMeta {
    pub(crate) row_id: u64,
    pub(crate) oid: Oid,
    pub(crate) name: String,
    pub(crate) owner: Oid,
    pub(crate) row: CatalogRow,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CompatCatalogRows {
    pub(crate) rows: BTreeMap<u32, BTreeMap<u64, Arc<CatalogRow>>>,
    pub(crate) tombstones: BTreeMap<u32, BTreeSet<u64>>,
}

impl CompatCatalogRows {
    fn is_empty(&self) -> bool {
        self.rows.values().all(BTreeMap::is_empty)
            && self.tombstones.values().all(BTreeSet::is_empty)
    }

    fn insert(&mut self, row: CatalogRow) {
        self.tombstones
            .entry(row.relation_oid.0)
            .or_default()
            .remove(&row.row_id);
        self.rows
            .entry(row.relation_oid.0)
            .or_default()
            .insert(row.row_id, Arc::new(row));
    }

    fn untombstone(&mut self, relation_oid: Oid, row_id: u64) {
        if let Some(tombstones) = self.tombstones.get_mut(&relation_oid.0) {
            tombstones.remove(&row_id);
        }
    }

    fn delete(&mut self, relation_oid: Oid, row_id: u64) {
        self.rows.entry(relation_oid.0).or_default().remove(&row_id);
        self.tombstones
            .entry(relation_oid.0)
            .or_default()
            .insert(row_id);
    }

    fn merge(&mut self, other: CompatCatalogRows) {
        for (relation_oid, tombstones) in other.tombstones {
            for row_id in tombstones {
                self.delete(Oid(relation_oid), row_id);
            }
        }
        for rows in other.rows.into_values() {
            for row in rows.into_values() {
                self.insert(row.as_ref().clone());
            }
        }
    }

    pub(crate) fn apply_to_rows(&self, relation_oid: Oid, rows: &mut BTreeMap<u64, CatalogRow>) {
        if let Some(tombstones) = self.tombstones.get(&relation_oid.0) {
            for row_id in tombstones {
                rows.remove(row_id);
            }
        }
        if let Some(overlay_rows) = self.rows.get(&relation_oid.0) {
            for (row_id, row) in overlay_rows {
                rows.insert(*row_id, row.as_ref().clone());
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CatalogSnapshot {
    pub(crate) generation: u64,
    pub(crate) namespaces: BTreeMap<u64, NamespaceMeta>,
    pub(crate) namespace_oids: BTreeMap<u32, u64>,
    pub(crate) namespace_names: BTreeMap<String, u64>,
    pub(crate) relations: BTreeMap<u64, RelationMeta>,
    pub(crate) relation_oids: BTreeMap<u32, u64>,
    pub(crate) relation_names: BTreeMap<(u32, String), u64>,
    pub(crate) columns: BTreeMap<u64, ColumnMeta>,
    pub(crate) columns_by_relation: BTreeMap<u32, BTreeMap<i16, u64>>,
    pub(crate) types: BTreeMap<u64, TypeMeta>,
    pub(crate) type_oids: BTreeMap<u32, u64>,
    pub(crate) type_names: BTreeMap<(u32, String), u64>,
    pub(crate) indexes: BTreeMap<u64, IndexMeta>,
    pub(crate) index_oids: BTreeMap<u32, u64>,
    pub(crate) indexes_by_relation: BTreeMap<u32, Vec<u64>>,
    pub(crate) pg_inherits_by_child: BTreeMap<u32, Vec<u64>>,
    pub(crate) pg_inherits_by_parent: BTreeMap<u32, Vec<u64>>,
    pub(crate) constraints: BTreeMap<u64, ConstraintMeta>,
    pub(crate) constraint_oids: BTreeMap<u32, u64>,
    pub(crate) constraints_by_relation: BTreeMap<u32, Vec<u64>>,
    pub(crate) constraints_by_index: BTreeMap<u32, Vec<u64>>,
    pub(crate) compat_rows: CompatCatalogRows,
}

impl CatalogSnapshot {
    fn new(generation: u64) -> Self {
        Self {
            generation,
            ..Self::default()
        }
    }

    fn rebuild_indexes(&mut self) {
        self.namespace_oids.clear();
        self.namespace_names.clear();
        for (row_id, namespace) in &self.namespaces {
            self.namespace_oids.insert(namespace.oid.0, *row_id);
            self.namespace_names
                .insert(normalize_identifier(&namespace.name), *row_id);
        }

        self.relation_oids.clear();
        self.relation_names.clear();
        for (row_id, relation) in &self.relations {
            self.relation_oids.insert(relation.oid.0, *row_id);
            self.relation_names.insert(
                (relation.namespace.0, normalize_identifier(&relation.name)),
                *row_id,
            );
        }

        self.columns_by_relation.clear();
        for (row_id, column) in &self.columns {
            self.columns_by_relation
                .entry(column.relation_oid.0)
                .or_default()
                .insert(column.attnum, *row_id);
        }

        self.type_oids.clear();
        self.type_names.clear();
        for (row_id, pg_type) in &self.types {
            self.type_oids.insert(pg_type.record.oid.0, *row_id);
            self.type_names.insert(
                (
                    pg_type.record.namespace.0,
                    normalize_identifier(&pg_type.record.name),
                ),
                *row_id,
            );
        }

        self.index_oids.clear();
        self.indexes_by_relation.clear();
        for (row_id, index) in &self.indexes {
            self.index_oids.insert(index.record.index_oid.0, *row_id);
            self.indexes_by_relation
                .entry(index.record.relation_oid.0)
                .or_default()
                .push(*row_id);
        }

        self.pg_inherits_by_child.clear();
        self.pg_inherits_by_parent.clear();
        if let Some(table) = crate::rows::static_catalog_by_relation_oid(PG_INHERITS_RELATION_OID)
            && let Some(rows) = self.compat_rows.rows.get(&PG_INHERITS_RELATION_OID.0)
        {
            for (row_id, row) in rows {
                let row = row.as_ref();
                if let Some(child) =
                    catalog_row_value(table, row, "inhrelid").and_then(catalog_value_oid)
                {
                    self.pg_inherits_by_child
                        .entry(child.0)
                        .or_default()
                        .push(*row_id);
                }
                if let Some(parent) =
                    catalog_row_value(table, row, "inhparent").and_then(catalog_value_oid)
                {
                    self.pg_inherits_by_parent
                        .entry(parent.0)
                        .or_default()
                        .push(*row_id);
                }
            }
        }

        self.constraint_oids.clear();
        self.constraints_by_relation.clear();
        self.constraints_by_index.clear();
        for (row_id, constraint) in &self.constraints {
            self.constraint_oids.insert(constraint.oid.0, *row_id);
            self.constraints_by_relation
                .entry(constraint.relation_oid.0)
                .or_default()
                .push(*row_id);
            if constraint.index_oid != INVALID_OID {
                self.constraints_by_index
                    .entry(constraint.index_oid.0)
                    .or_default()
                    .push(*row_id);
            }
        }
    }

    pub(crate) fn relation_meta_by_oid(&self, oid: Oid) -> Option<&RelationMeta> {
        self.relation_oids
            .get(&oid.0)
            .and_then(|row_id| self.relations.get(row_id))
    }

    pub(crate) fn relation_meta_by_name(
        &self,
        name: &str,
        namespace: Oid,
    ) -> Option<&RelationMeta> {
        let name = normalize_identifier(name);
        self.relation_names
            .get(&(namespace.0, name))
            .and_then(|row_id| self.relations.get(row_id))
    }

    pub(crate) fn type_meta_by_oid(&self, oid: Oid) -> Option<&TypeMeta> {
        self.type_oids
            .get(&oid.0)
            .and_then(|row_id| self.types.get(row_id))
    }

    pub(crate) fn type_meta_by_name(&self, name: &str, namespace: Oid) -> Option<&TypeMeta> {
        let name = canonical_catalog_type_name(name);
        self.type_names
            .get(&(namespace.0, name))
            .and_then(|row_id| self.types.get(row_id))
    }

    pub(crate) fn index_meta_by_oid(&self, oid: Oid) -> Option<&IndexMeta> {
        self.index_oids
            .get(&oid.0)
            .and_then(|row_id| self.indexes.get(row_id))
    }

    pub(crate) fn index_metas_for_relation(&self, relation_oid: Oid) -> Vec<&IndexMeta> {
        self.indexes_by_relation
            .get(&relation_oid.0)
            .into_iter()
            .flat_map(|row_ids| row_ids.iter())
            .filter_map(|row_id| self.indexes.get(row_id))
            .collect()
    }

    pub(crate) fn constraint_metas_for_relation(&self, relation_oid: Oid) -> Vec<&ConstraintMeta> {
        self.constraints_by_relation
            .get(&relation_oid.0)
            .into_iter()
            .flat_map(|row_ids| row_ids.iter())
            .filter_map(|row_id| self.constraints.get(row_id))
            .collect()
    }

    pub(crate) fn constraint_meta_by_oid(&self, oid: Oid) -> Option<&ConstraintMeta> {
        self.constraint_oids
            .get(&oid.0)
            .and_then(|row_id| self.constraints.get(row_id))
    }

    pub(crate) fn constraint_metas_for_index(&self, index_oid: Oid) -> Vec<&ConstraintMeta> {
        self.constraints_by_index
            .get(&index_oid.0)
            .into_iter()
            .flat_map(|row_ids| row_ids.iter())
            .filter_map(|row_id| self.constraints.get(row_id))
            .collect()
    }

    fn remove_relation(&mut self, row_id: u64) {
        self.relations.remove(&row_id);
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CatalogDraft {
    pub(crate) namespaces: BTreeMap<u64, Option<NamespaceMeta>>,
    pub(crate) relations: BTreeMap<u64, Option<RelationMeta>>,
    pub(crate) columns: BTreeMap<u64, Option<ColumnMeta>>,
    pub(crate) types: BTreeMap<u64, Option<TypeMeta>>,
    pub(crate) indexes: BTreeMap<u64, Option<IndexMeta>>,
    pub(crate) constraints: BTreeMap<u64, Option<ConstraintMeta>>,
    pub(crate) compat_rows: CompatCatalogRows,
}

impl CatalogDraft {
    fn is_empty(&self) -> bool {
        self.namespaces.is_empty()
            && self.relations.is_empty()
            && self.columns.is_empty()
            && self.types.is_empty()
            && self.indexes.is_empty()
            && self.constraints.is_empty()
            && self.compat_rows.is_empty()
    }

    fn merge(&mut self, other: CatalogDraft) {
        for (row_id, mutation) in &other.namespaces {
            if mutation.is_some() {
                self.compat_rows
                    .untombstone(PG_NAMESPACE_RELATION_OID, *row_id);
            }
        }
        for (row_id, mutation) in &other.relations {
            if mutation.is_some() {
                self.compat_rows.untombstone(PG_CLASS_RELATION_OID, *row_id);
            }
        }
        for (row_id, mutation) in &other.columns {
            if mutation.is_some() {
                self.compat_rows
                    .untombstone(PG_ATTRIBUTE_RELATION_OID, *row_id);
            }
        }
        for (row_id, mutation) in &other.types {
            if mutation.is_some() {
                self.compat_rows.untombstone(PG_TYPE_RELATION_OID, *row_id);
            }
        }
        for (row_id, mutation) in &other.indexes {
            if mutation.is_some() {
                self.compat_rows.untombstone(PG_INDEX_RELATION_OID, *row_id);
            }
        }
        for (row_id, mutation) in &other.constraints {
            if mutation.is_some() {
                self.compat_rows
                    .untombstone(PG_CONSTRAINT_RELATION_OID, *row_id);
            }
        }
        self.namespaces.extend(other.namespaces);
        self.relations.extend(other.relations);
        self.columns.extend(other.columns);
        self.types.extend(other.types);
        self.indexes.extend(other.indexes);
        self.constraints.extend(other.constraints);
        self.compat_rows.merge(other.compat_rows);
    }

    fn apply_to_snapshot(&self, snapshot: &mut CatalogSnapshot) {
        for (row_id, mutation) in &self.namespaces {
            match mutation {
                Some(namespace) => {
                    snapshot.namespaces.insert(*row_id, namespace.clone());
                }
                None => {
                    snapshot.namespaces.remove(row_id);
                }
            }
        }

        for (row_id, mutation) in &self.relations {
            match mutation {
                Some(relation) => {
                    snapshot.relations.insert(*row_id, relation.clone());
                }
                None => {
                    snapshot.remove_relation(*row_id);
                }
            }
        }

        for (row_id, mutation) in &self.columns {
            match mutation {
                Some(column) => {
                    snapshot.columns.insert(*row_id, column.clone());
                }
                None => {
                    snapshot.columns.remove(row_id);
                }
            }
        }

        for (row_id, mutation) in &self.types {
            match mutation {
                Some(pg_type) => {
                    snapshot.types.insert(*row_id, pg_type.clone());
                }
                None => {
                    snapshot.types.remove(row_id);
                }
            }
        }

        for (row_id, mutation) in &self.indexes {
            match mutation {
                Some(index) => {
                    snapshot.indexes.insert(*row_id, index.clone());
                }
                None => {
                    snapshot.indexes.remove(row_id);
                }
            }
        }

        for (row_id, mutation) in &self.constraints {
            match mutation {
                Some(constraint) => {
                    snapshot.constraints.insert(*row_id, constraint.clone());
                }
                None => {
                    snapshot.constraints.remove(row_id);
                }
            }
        }

        snapshot.compat_rows.merge(self.compat_rows.clone());
        snapshot.rebuild_indexes();
    }

    pub(crate) fn upsert_catalog_row(&mut self, table: &StaticCatalogTable, row: CatalogRow) {
        let row_id = row.row_id;
        let handled = match table.oid {
            PG_NAMESPACE_RELATION_OID => namespace_meta_from_row(table, &row, row_id)
                .map(|namespace| self.namespaces.insert(row_id, Some(namespace)))
                .is_some(),
            PG_CLASS_RELATION_OID => relation_meta_from_row(table, &row, row_id)
                .map(|relation| self.relations.insert(row_id, Some(relation)))
                .is_some(),
            PG_ATTRIBUTE_RELATION_OID => column_meta_from_row(table, &row, row_id)
                .map(|column| self.columns.insert(row_id, Some(column)))
                .is_some(),
            PG_TYPE_RELATION_OID => type_meta_from_row(table, &row, row_id)
                .map(|pg_type| self.types.insert(row_id, Some(pg_type)))
                .is_some(),
            PG_INDEX_RELATION_OID => index_meta_from_row(table, &row, row_id)
                .map(|index| self.indexes.insert(row_id, Some(index)))
                .is_some(),
            PG_CONSTRAINT_RELATION_OID => constraint_meta_from_row(table, &row, row_id)
                .map(|constraint| self.constraints.insert(row_id, Some(constraint)))
                .is_some(),
            _ => false,
        };
        if handled {
            self.compat_rows.untombstone(table.oid, row_id);
        } else {
            self.compat_rows.insert(row);
        }
    }

    pub(crate) fn delete_catalog_row(&mut self, relation_oid: Oid, row_id: u64) {
        match relation_oid {
            PG_NAMESPACE_RELATION_OID => {
                self.namespaces.insert(row_id, None);
            }
            PG_CLASS_RELATION_OID => {
                self.relations.insert(row_id, None);
            }
            PG_ATTRIBUTE_RELATION_OID => {
                self.columns.insert(row_id, None);
            }
            PG_TYPE_RELATION_OID => {
                self.types.insert(row_id, None);
            }
            PG_INDEX_RELATION_OID => {
                self.indexes.insert(row_id, None);
            }
            PG_CONSTRAINT_RELATION_OID => {
                self.constraints.insert(row_id, None);
            }
            _ => {}
        }
        self.compat_rows.delete(relation_oid, row_id);
    }
}

fn namespace_meta_from_row(
    table: &StaticCatalogTable,
    row: &CatalogRow,
    row_id: u64,
) -> Option<NamespaceMeta> {
    let oid = catalog_row_value(table, row, "oid").and_then(catalog_value_oid)?;
    let name = catalog_row_value(table, row, "nspname").and_then(catalog_value_string)?;
    let owner = catalog_row_value(table, row, "nspowner")
        .and_then(catalog_value_oid)
        .unwrap_or(POSTGRES_ROLE_OID);
    Some(NamespaceMeta {
        row_id,
        oid,
        name: normalize_identifier(&name),
        owner,
        row: row.clone(),
    })
}

fn relation_meta_from_row(
    table: &StaticCatalogTable,
    row: &CatalogRow,
    row_id: u64,
) -> Option<RelationMeta> {
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
    let relnatts = catalog_row_value(table, row, "relnatts")
        .and_then(catalog_value_i16)
        .map(|value| value.max(0) as u16)
        .unwrap_or(0);
    let relhasindex = catalog_row_value(table, row, "relhasindex")
        .and_then(catalog_value_bool)
        .unwrap_or(false);
    Some(RelationMeta {
        row_id,
        oid,
        type_oid,
        namespace,
        owner,
        name: normalize_identifier(&name),
        relkind,
        relnatts,
        relhasindex,
        row: row.clone(),
    })
}

fn column_meta_from_row(
    table: &StaticCatalogTable,
    row: &CatalogRow,
    row_id: u64,
) -> Option<ColumnMeta> {
    let relation_oid = catalog_row_value(table, row, "attrelid").and_then(catalog_value_oid)?;
    let (attnum, record) = column_record_from_pg_attribute_row(table, row, relation_oid)?;
    Some(ColumnMeta {
        row_id,
        relation_oid,
        attnum,
        record,
        row: row.clone(),
    })
}

fn type_meta_from_row(
    table: &StaticCatalogTable,
    row: &CatalogRow,
    row_id: u64,
) -> Option<TypeMeta> {
    let oid = catalog_row_value(table, row, "oid").and_then(catalog_value_oid)?;
    let name = catalog_row_value(table, row, "typname").and_then(catalog_value_string)?;
    let record = CatalogTypeRecord {
        row_id,
        oid,
        name: canonical_catalog_type_name(&name),
        namespace: catalog_row_value(table, row, "typnamespace")
            .and_then(catalog_value_oid)
            .unwrap_or(PG_CATALOG_NAMESPACE_OID),
        owner: catalog_row_value(table, row, "typowner")
            .and_then(catalog_value_oid)
            .unwrap_or(POSTGRES_ROLE_OID),
        typlen: catalog_row_value(table, row, "typlen")
            .and_then(catalog_value_i16)
            .unwrap_or(-1),
        typbyval: catalog_row_value(table, row, "typbyval")
            .and_then(catalog_value_bool)
            .unwrap_or(false),
        typalign: catalog_row_value(table, row, "typalign")
            .and_then(catalog_value_u8)
            .unwrap_or(b'i'),
        typdelim: catalog_row_value(table, row, "typdelim")
            .and_then(catalog_value_u8)
            .unwrap_or(b','),
        typinput: catalog_row_value(table, row, "typinput")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typoutput: catalog_row_value(table, row, "typoutput")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typreceive: catalog_row_value(table, row, "typreceive")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typsend: catalog_row_value(table, row, "typsend")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typmodin: catalog_row_value(table, row, "typmodin")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typmodout: catalog_row_value(table, row, "typmodout")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typisdefined: catalog_row_value(table, row, "typisdefined")
            .and_then(catalog_value_bool)
            .unwrap_or(true),
        typtype: catalog_row_value(table, row, "typtype")
            .and_then(catalog_value_u8)
            .unwrap_or(b'b'),
        typcategory: catalog_row_value(table, row, "typcategory")
            .and_then(catalog_value_u8)
            .unwrap_or(b'U'),
        typispreferred: catalog_row_value(table, row, "typispreferred")
            .and_then(catalog_value_bool)
            .unwrap_or(false),
        typrelid: catalog_row_value(table, row, "typrelid")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typelem: catalog_row_value(table, row, "typelem")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typarray: catalog_row_value(table, row, "typarray")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typbasetype: catalog_row_value(table, row, "typbasetype")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typtypmod: catalog_row_value(table, row, "typtypmod")
            .and_then(catalog_value_i32)
            .unwrap_or(-1),
        typcollation: catalog_row_value(table, row, "typcollation")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typsubscript: catalog_row_value(table, row, "typsubscript")
            .and_then(catalog_value_oid)
            .unwrap_or(INVALID_OID),
        typstorage: catalog_row_value(table, row, "typstorage")
            .and_then(catalog_value_u8)
            .unwrap_or(b'p'),
    };
    Some(TypeMeta {
        row_id,
        record,
        row: row.clone(),
    })
}

fn index_meta_from_row(
    _table: &StaticCatalogTable,
    row: &CatalogRow,
    row_id: u64,
) -> Option<IndexMeta> {
    Some(IndexMeta {
        row_id,
        record: pg_index_record_from_row(row)?,
        row: row.clone(),
    })
}

fn constraint_meta_from_row(
    table: &StaticCatalogTable,
    row: &CatalogRow,
    row_id: u64,
) -> Option<ConstraintMeta> {
    let oid = catalog_row_value(table, row, "oid").and_then(catalog_value_oid)?;
    let name = catalog_row_value(table, row, "conname")
        .and_then(catalog_value_string)
        .unwrap_or_default();
    let namespace = catalog_row_value(table, row, "connamespace")
        .and_then(catalog_value_oid)
        .unwrap_or(PUBLIC_NAMESPACE_OID);
    let relation_oid = catalog_row_value(table, row, "conrelid")
        .and_then(catalog_value_oid)
        .unwrap_or(INVALID_OID);
    let index_oid = catalog_row_value(table, row, "conindid")
        .and_then(catalog_value_oid)
        .unwrap_or(INVALID_OID);
    let kind = catalog_row_value(table, row, "contype")
        .and_then(catalog_value_u8)
        .unwrap_or(0);
    let key_attnums = catalog_row_value(table, row, "conkey")
        .and_then(catalog_value_int2_vector)
        .unwrap_or_default();
    Some(ConstraintMeta {
        row_id,
        oid,
        name: normalize_identifier(&name),
        namespace,
        relation_oid,
        index_oid,
        kind,
        key_attnums,
        row: row.clone(),
    })
}

#[derive(Debug)]
pub(crate) struct CatalogState {
    pub(crate) next_overlay_row_ids: BTreeMap<u32, u64>,
    pub(crate) snapshot: CatalogSnapshot,
}

impl Default for CatalogState {
    fn default() -> Self {
        Self {
            next_overlay_row_ids: BTreeMap::new(),
            snapshot: CatalogSnapshot::new(1),
        }
    }
}

static CATALOG: OnceLock<RwLock<CatalogState>> = OnceLock::new();
pub(crate) static CATALOG_GENERATION: AtomicU64 = AtomicU64::new(1);
pub(crate) static PRIMARY_KEY_INDEX_OID_CACHE: OnceLock<Mutex<PrimaryKeyIndexOidCache>> =
    OnceLock::new();
pub(crate) static CATALOG_LOOKUP_CACHE: OnceLock<Mutex<CatalogLookupCache>> = OnceLock::new();

#[derive(Debug)]
pub(crate) struct PrimaryKeyIndexOidCache {
    pub(crate) generation: u64,
    pub(crate) entries: BTreeMap<u32, Option<Oid>>,
}

impl Default for PrimaryKeyIndexOidCache {
    fn default() -> Self {
        Self {
            generation: current_generation(),
            entries: BTreeMap::new(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct CatalogLookupCache {
    pub(crate) generation: u64,
    pub(crate) relation_columns: BTreeMap<(u32, i16), Option<ColumnRecord>>,
    pub(crate) relation_physical_columns: BTreeMap<(u32, i16), Option<PhysicalColumnRecord>>,
    pub(crate) index_records_by_oid: BTreeMap<u32, Option<IndexRecord>>,
    pub(crate) index_records_by_relation: BTreeMap<u32, Vec<IndexRecord>>,
    pub(crate) unique_index_records_by_relation: BTreeMap<u32, Vec<IndexRecord>>,
}

impl Default for CatalogLookupCache {
    fn default() -> Self {
        Self {
            generation: current_generation(),
            relation_columns: BTreeMap::new(),
            relation_physical_columns: BTreeMap::new(),
            index_records_by_oid: BTreeMap::new(),
            index_records_by_relation: BTreeMap::new(),
            unique_index_records_by_relation: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Default)]
pub struct CatalogSession {
    pub(crate) transaction_stack: Vec<CatalogDraft>,
    pub(crate) explicit_transaction: bool,
    pub(crate) visible_snapshot_revision: u64,
    pub(crate) visible_snapshot_cache: Option<CachedVisibleCatalogSnapshot>,
}

pub type CatalogSessionHandle = Arc<Mutex<CatalogSession>>;

#[derive(Debug)]
pub(crate) struct CachedVisibleCatalogSnapshot {
    base_generation: u64,
    revision: u64,
    snapshot: Arc<CatalogSnapshot>,
}

#[cfg(test)]
pub(crate) static VISIBLE_CATALOG_SNAPSHOT_MATERIALIZATIONS: AtomicU64 = AtomicU64::new(0);

pub fn new_catalog_session() -> CatalogSessionHandle {
    Arc::new(Mutex::new(CatalogSession::default()))
}

static DEFAULT_CATALOG_SESSION: OnceLock<CatalogSessionHandle> = OnceLock::new();

thread_local! {
    static CURRENT_CATALOG_SESSION: RefCell<Option<CatalogSessionHandle>> = const { RefCell::new(None) };
}

#[derive(Debug)]
pub struct CatalogSessionGuard {
    previous: Option<CatalogSessionHandle>,
}

pub fn enter_catalog_session(handle: CatalogSessionHandle) -> CatalogSessionGuard {
    let previous = CURRENT_CATALOG_SESSION.with(|slot| slot.replace(Some(handle)));
    CatalogSessionGuard { previous }
}

impl Drop for CatalogSessionGuard {
    fn drop(&mut self) {
        CURRENT_CATALOG_SESSION.with(|slot| {
            slot.replace(self.previous.take());
        });
    }
}

fn default_catalog_session() -> CatalogSessionHandle {
    DEFAULT_CATALOG_SESSION
        .get_or_init(new_catalog_session)
        .clone()
}

fn current_catalog_session() -> CatalogSessionHandle {
    CURRENT_CATALOG_SESSION
        .with(|slot| slot.borrow().clone())
        .unwrap_or_else(default_catalog_session)
}

pub(crate) fn catalog() -> &'static RwLock<CatalogState> {
    CATALOG.get_or_init(|| RwLock::new(CatalogState::default()))
}

pub(crate) fn with_catalog<R>(f: impl FnOnce(&mut CatalogState) -> R) -> R {
    match catalog().write() {
        Ok(mut state) => f(&mut state),
        Err(poisoned) => {
            let mut state = poisoned.into_inner();
            f(&mut state)
        }
    }
}

pub(crate) fn with_catalog_read<R>(f: impl FnOnce(&CatalogState) -> R) -> R {
    match catalog().read() {
        Ok(state) => f(&state),
        Err(poisoned) => {
            let state = poisoned.into_inner();
            f(&state)
        }
    }
}

pub(crate) fn with_catalog_session<R>(f: impl FnOnce(&mut CatalogSession) -> R) -> R {
    let session = current_catalog_session();
    match session.lock() {
        Ok(mut session) => f(&mut session),
        Err(poisoned) => {
            let mut session = poisoned.into_inner();
            f(&mut session)
        }
    }
}

pub(crate) fn invalidate_visible_catalog_snapshot(session: &mut CatalogSession) {
    let next_revision = session.visible_snapshot_revision.wrapping_add(1);
    session.visible_snapshot_revision = next_revision.max(1);
    session.visible_snapshot_cache = None;
}

fn session_has_visible_drafts(session: &CatalogSession) -> bool {
    session
        .transaction_stack
        .iter()
        .any(|draft| !draft.is_empty())
}

pub(crate) fn with_visible_catalog_snapshot<R>(f: impl FnOnce(&CatalogSnapshot) -> R) -> R {
    let session = current_catalog_session();
    let visible_snapshot = {
        let mut session = match session.lock() {
            Ok(session) => session,
            Err(poisoned) => poisoned.into_inner(),
        };

        if !session_has_visible_drafts(&session) {
            None
        } else {
            let base_generation = current_generation();
            if let Some(cache) = &session.visible_snapshot_cache {
                if cache.base_generation == base_generation
                    && cache.revision == session.visible_snapshot_revision
                {
                    Some(Arc::clone(&cache.snapshot))
                } else {
                    let revision = session.visible_snapshot_revision;
                    let (base_generation, snapshot) =
                        materialize_visible_catalog_snapshot(&session);
                    session.visible_snapshot_cache = Some(CachedVisibleCatalogSnapshot {
                        base_generation,
                        revision,
                        snapshot: Arc::clone(&snapshot),
                    });
                    Some(snapshot)
                }
            } else {
                let revision = session.visible_snapshot_revision;
                let (base_generation, snapshot) = materialize_visible_catalog_snapshot(&session);
                session.visible_snapshot_cache = Some(CachedVisibleCatalogSnapshot {
                    base_generation,
                    revision,
                    snapshot: Arc::clone(&snapshot),
                });
                Some(snapshot)
            }
        }
    };

    if let Some(snapshot) = visible_snapshot {
        f(&snapshot)
    } else {
        with_catalog_read(|state| f(&state.snapshot))
    }
}

fn materialize_visible_catalog_snapshot(session: &CatalogSession) -> (u64, Arc<CatalogSnapshot>) {
    let snapshot = with_catalog_read(|state| {
        let mut visible_snapshot = state.snapshot.clone();
        for draft in session
            .transaction_stack
            .iter()
            .filter(|draft| !draft.is_empty())
        {
            draft.apply_to_snapshot(&mut visible_snapshot);
        }
        (state.snapshot.generation, Arc::new(visible_snapshot))
    });
    #[cfg(test)]
    VISIBLE_CATALOG_SNAPSHOT_MATERIALIZATIONS.fetch_add(1, Ordering::Relaxed);
    snapshot
}

pub fn has_uncommitted_catalog_changes() -> bool {
    let session = current_catalog_session();
    match session.lock() {
        Ok(session) => session
            .transaction_stack
            .iter()
            .any(|overlay| !overlay.is_empty()),
        Err(poisoned) => poisoned
            .into_inner()
            .transaction_stack
            .iter()
            .any(|overlay| !overlay.is_empty()),
    }
}

pub(crate) fn ensure_catalog_transaction(session: &mut CatalogSession) {
    if session.transaction_stack.is_empty() {
        session.transaction_stack.push(CatalogDraft::default());
        invalidate_visible_catalog_snapshot(session);
    }
}

pub(crate) fn commit_catalog_draft(draft: CatalogDraft) {
    if draft.is_empty() {
        return;
    }
    with_catalog(|state| {
        draft.apply_to_snapshot(&mut state.snapshot);
        bump_generation(state);
    });
}

fn commit_top_catalog_draft(session: &mut CatalogSession) {
    let Some(draft) = session.transaction_stack.pop() else {
        return;
    };
    if let Some(parent) = session.transaction_stack.last_mut() {
        parent.merge(draft);
    } else {
        commit_catalog_draft(draft);
    }
    invalidate_visible_catalog_snapshot(session);
}

pub(crate) fn bump_generation(state: &mut CatalogState) {
    state.snapshot.generation = state.snapshot.generation.saturating_add(1).max(1);
    CATALOG_GENERATION.store(state.snapshot.generation, Ordering::Relaxed);
}

pub fn current_generation() -> u64 {
    CATALOG_GENERATION.load(Ordering::Relaxed)
}

pub fn begin_explicit_transaction() {
    with_catalog_session(|session| {
        if !session.explicit_transaction {
            while !session.transaction_stack.is_empty() {
                commit_top_catalog_draft(session);
            }
        }
        ensure_catalog_transaction(session);
        session.explicit_transaction = true;
    });
}

pub fn begin_implicit_transaction() {
    with_catalog_session(ensure_catalog_transaction);
}

pub fn commit_explicit_transaction() {
    with_catalog_session(|session| {
        while !session.transaction_stack.is_empty() {
            commit_top_catalog_draft(session);
        }
        session.explicit_transaction = false;
    });
}

pub fn abort_explicit_transaction() {
    with_catalog_session(|session| {
        session.transaction_stack.clear();
        session.explicit_transaction = false;
        invalidate_visible_catalog_snapshot(session);
    });
}

pub fn commit_implicit_transaction() {
    with_catalog_session(|session| {
        if session.explicit_transaction {
            return;
        }
        while !session.transaction_stack.is_empty() {
            commit_top_catalog_draft(session);
        }
    });
}

pub fn abort_implicit_transaction() {
    with_catalog_session(|session| {
        if !session.explicit_transaction {
            session.transaction_stack.clear();
            invalidate_visible_catalog_snapshot(session);
        }
    });
}

pub fn begin_subtransaction() {
    with_catalog_session(|session| {
        ensure_catalog_transaction(session);
        session.transaction_stack.push(CatalogDraft::default());
        invalidate_visible_catalog_snapshot(session);
    });
}

pub fn commit_subtransaction() {
    with_catalog_session(|session| {
        if session.transaction_stack.len() > 1 {
            commit_top_catalog_draft(session);
        }
    });
}

pub fn abort_subtransaction() {
    with_catalog_session(|session| {
        if session.transaction_stack.len() > 1 {
            session.transaction_stack.pop();
            invalidate_visible_catalog_snapshot(session);
        }
    });
}

#[cfg(test)]
pub(crate) fn clear_current_catalog_session_for_tests() {
    with_catalog_session(|session| {
        session.transaction_stack.clear();
        session.explicit_transaction = false;
        invalidate_visible_catalog_snapshot(session);
    });
}

#[cfg(test)]
pub(crate) fn reset_visible_catalog_snapshot_materializations_for_tests() {
    VISIBLE_CATALOG_SNAPSHOT_MATERIALIZATIONS.store(0, Ordering::Relaxed);
}

#[cfg(test)]
pub(crate) fn visible_catalog_snapshot_materializations_for_tests() -> u64 {
    VISIBLE_CATALOG_SNAPSHOT_MATERIALIZATIONS.load(Ordering::Relaxed)
}
