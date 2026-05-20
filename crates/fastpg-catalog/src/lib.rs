#![forbid(unsafe_code)]

mod generated_catalog;
mod lookups;
mod model;
mod rows;
mod state;

pub use lookups::{
    btree_opclass_for_type, builtin_aggregate_by_proc_oid, builtin_cast_by_source_target,
    builtin_namespace_by_name, builtin_namespace_by_oid, builtin_namespaces,
    builtin_operator_by_oid, builtin_operator_by_signature, builtin_operators_by_name,
    builtin_proc_by_oid, builtin_procs_by_name, builtin_type_by_name, enum_oids_by_sort_order,
    index_record_by_index_oid, index_records_for_relation_oid, lookup_builtin_type, lookup_type,
    primary_key_index_oid_for_relation_oid, primary_key_index_record_for_relation_oid,
    primary_key_relation_oid_for_index_oid, relation_by_name, relation_by_name_in_namespace,
    relation_by_oid, relation_column_by_attnum, relation_column_count,
    relation_oid_by_name_in_namespace, relation_oid_exists, relation_oid_for_index_oid,
    relation_physical_column_by_attnum, relation_summary_by_name_in_namespace,
    relation_summary_by_oid, relations, type_by_name, unique_index_oids_for_relation_oid,
    unique_index_records_for_relation_oid, virtual_catalog_by_name,
    virtual_catalog_by_relation_oid, virtual_catalogs,
};
pub use model::*;
pub use rows::{
    catalog_row_value, catalog_rows, delete_catalog_row, ensure_database,
    relation_planner_stats_by_oid, relation_rowtype_oid_by_oid, resolve_generated_catalog_oid_name,
    static_catalog_by_name, static_catalog_by_relation_oid, static_catalog_rowtype_oid,
    static_catalogs, upsert_catalog_row,
};
pub use state::{
    CatalogSession, CatalogSessionGuard, CatalogSessionHandle, abort_explicit_transaction,
    abort_implicit_transaction, abort_subtransaction, begin_explicit_transaction,
    begin_implicit_transaction, begin_subtransaction, commit_explicit_transaction,
    commit_implicit_transaction, commit_subtransaction, current_generation, enter_catalog_session,
    has_uncommitted_catalog_changes, new_catalog_session,
};

#[cfg(test)]
pub use lookups::clear_for_tests;

#[cfg(test)]
mod tests;
