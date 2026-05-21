use super::*;
use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

use fastpg_types::Oid;

use crate::lookups::catalog_value_int2_vector;
use crate::rows::{
    catalog_value_i16, catalog_value_i32, catalog_value_oid, catalog_value_string, catalog_value_u8,
};

static CATALOG_TEST_LOCK: Mutex<()> = Mutex::new(());

fn catalog_test_lock() -> MutexGuard<'static, ()> {
    CATALOG_TEST_LOCK
        .lock()
        .expect("catalog test mutex poisoned")
}

#[test]
fn has_pgbench_scalar_types() {
    for oid in [
        INT4_OID,
        INT8_OID,
        TEXT_OID,
        TID_OID,
        XID_OID,
        CID_OID,
        BPCHAR_OID,
        VARCHAR_OID,
        TIMESTAMP_OID,
        TIMESTAMPTZ_OID,
    ] {
        let record = lookup_builtin_type(oid).expect("builtin type");
        assert_eq!(record.oid, oid);
        assert!(record.typisdefined);
        assert_ne!(record.typinput, INVALID_OID);
        assert_ne!(record.typoutput, INVALID_OID);
    }
}

#[test]
fn resolves_builtin_types_by_catalog_name() {
    assert_eq!(
        builtin_type_by_name("int4", PG_CATALOG_NAMESPACE_OID)
            .expect("int4")
            .oid,
        INT4_OID
    );
    assert_eq!(
        builtin_type_by_name("integer", PG_CATALOG_NAMESPACE_OID)
            .expect("integer alias")
            .oid,
        INT4_OID
    );
    assert_eq!(
        builtin_type_by_name("timestamp with time zone", PG_CATALOG_NAMESPACE_OID)
            .expect("timestamptz alias")
            .oid,
        TIMESTAMPTZ_OID
    );
    assert!(builtin_type_by_name("int4", PUBLIC_NAMESPACE_OID).is_none());
}

#[test]
fn has_timestamp_assignment_casts() {
    let cast = builtin_cast_by_source_target(TIMESTAMPTZ_OID, TIMESTAMP_OID)
        .expect("timestamptz to timestamp cast");
    assert_eq!(cast.function, Oid(2027));
    assert_eq!(cast.context, b'a');
    assert_eq!(cast.method, b'f');

    let proc = builtin_proc_by_oid(cast.function).expect("cast function proc");
    assert_eq!(proc.return_type, TIMESTAMP_OID);
    assert_eq!(proc.arg_types, [TIMESTAMPTZ_OID]);
    assert_eq!(proc.volatility, b's');
}

fn row_value<'a>(table_name: &str, row: &'a CatalogRow, column_name: &str) -> &'a CatalogValue {
    let table = static_catalog_by_name(table_name).expect("catalog table");
    catalog_row_value(table, row, column_name).expect("catalog column")
}

fn value_name(value: &CatalogValue) -> Option<&str> {
    match value {
        CatalogValue::Name(value) => Some(value),
        _ => None,
    }
}

fn value_oid(value: &CatalogValue) -> Option<Oid> {
    match value {
        CatalogValue::Oid(value) => Some(*value),
        _ => None,
    }
}

fn value_i16(value: &CatalogValue) -> Option<i16> {
    match value {
        CatalogValue::Int16(value) => Some(*value),
        _ => None,
    }
}

fn upsert_named_catalog_row(table_name: &str, row_id: u64, values: &[(&str, &str)]) -> u64 {
    let table = static_catalog_by_name(table_name).expect("catalog table");
    let values = values
        .iter()
        .map(|(column, value)| (*column, (*value).to_owned()))
        .collect::<BTreeMap<_, _>>();
    let row = table
        .columns
        .iter()
        .map(|column| values.get(column.name).cloned())
        .collect::<Vec<_>>();
    upsert_catalog_row(table.oid, row_id, row).expect("upsert catalog row")
}

#[test]
fn generated_static_catalog_has_core_rows() {
    assert!(static_catalog_by_name("pg_type").is_some());
    assert!(static_catalog_by_name("pg_proc").is_some());
    assert!(static_catalog_by_name("pg_am").is_some());
    assert!(static_catalog_by_name("pg_opclass").is_some());

    assert_eq!(
        builtin_type_by_name("float8", PG_CATALOG_NAMESPACE_OID)
            .expect("float8")
            .oid,
        FLOAT8_OID
    );
    assert!(builtin_procs_by_name("generate_series").count() > 0);
    assert!(
        catalog_rows(Oid(2601))
            .iter()
            .any(|row| { value_name(row_value("pg_am", row, "amname")) == Some("btree") })
    );
    assert!(
        catalog_rows(Oid(2601))
            .iter()
            .any(|row| { value_name(row_value("pg_am", row, "amname")) == Some("hash") })
    );
    assert!(btree_opclass_for_type(INT4_OID).is_some());
    assert!(default_opclass_for_type(Oid(403), INT4_OID).is_some());
    assert!(default_opclass_for_type(Oid(405), INT4_OID).is_some());
    assert!(default_opclass_for_type(Oid(403), VARCHAR_OID).is_some());
    assert!(default_opclass_for_type(Oid(405), VARCHAR_OID).is_some());
    assert!(default_opclass_for_type(Oid(403), INT4_ARRAY_OID).is_some());
    assert!(default_opclass_for_type(Oid(403), Oid(3906)).is_some());
    assert!(default_opclass_for_type(Oid(405), Oid(3906)).is_some());
    assert!(default_opclass_for_type(Oid(783), Oid(3904)).is_some());
    assert!(default_opclass_for_type(Oid(4000), Oid(3904)).is_some());
    assert!(default_opclass_for_type(Oid(783), Oid(4451)).is_some());
    assert!(default_opclass_for_type(Oid(403), Oid(600)).is_none());
    assert!(builtin_cast_by_source_target(INT4_OID, OID_OID).is_some());
}

#[test]
fn catalog_rowtype_array_columns_use_record_array_type() {
    let table = static_catalog_by_name("pg_statistic_ext_data").expect("pg_statistic_ext_data");
    let column = table
        .columns
        .iter()
        .find(|column| column.name == "stxdexpr")
        .expect("stxdexpr column");

    assert_eq!(column.type_name, "_pg_statistic");
    assert_eq!(column.type_oid, RECORD_ARRAY_OID);
}

#[test]
fn pg_init_privs_exposes_bootstrap_schema_privileges() {
    let table = static_catalog_by_name("pg_init_privs").expect("pg_init_privs");
    let rows = catalog_rows(table.oid);
    let row = rows
        .iter()
        .find(|row| {
            catalog_row_value(table, row, "objoid").and_then(catalog_value_oid)
                == Some(PG_CATALOG_NAMESPACE_OID)
        })
        .expect("pg_catalog init privs row");

    assert_eq!(
        catalog_row_value(table, row, "classoid").and_then(catalog_value_oid),
        Some(PG_NAMESPACE_RELATION_OID)
    );
    assert_eq!(
        catalog_row_value(table, row, "privtype").and_then(catalog_value_u8),
        Some(b'i')
    );
    assert!(
        catalog_row_value(table, row, "initprivs")
            .and_then(catalog_value_string)
            .is_some_and(|initprivs| initprivs.contains("=U/postgres"))
    );
}

#[test]
fn text_search_catalog_exposes_initdb_english_config() {
    let config_table = static_catalog_by_name("pg_ts_config").expect("pg_ts_config");
    let config_row = catalog_rows(PG_TS_CONFIG_RELATION_OID)
        .into_iter()
        .find(|row| {
            catalog_row_value(config_table, row, "cfgname").and_then(catalog_value_string)
                == Some("english".to_owned())
        })
        .expect("english text search config");

    assert_eq!(
        catalog_row_value(config_table, &config_row, "oid").and_then(catalog_value_oid),
        Some(ENGLISH_TS_CONFIG_OID)
    );
    assert_eq!(
        catalog_row_value(config_table, &config_row, "cfgparser").and_then(catalog_value_oid),
        Some(DEFAULT_TS_PARSER_OID)
    );

    let template_table = static_catalog_by_name("pg_ts_template").expect("pg_ts_template");
    let snowball_template_row = catalog_rows(PG_TS_TEMPLATE_RELATION_OID)
        .into_iter()
        .find(|row| {
            catalog_row_value(template_table, row, "oid").and_then(catalog_value_oid)
                == Some(SNOWBALL_TS_TEMPLATE_OID)
        })
        .expect("snowball text search template");
    assert_eq!(
        catalog_row_value(template_table, &snowball_template_row, "tmplinit")
            .and_then(catalog_value_oid),
        Some(DSNOWBALL_INIT_PROC_OID)
    );
    assert_eq!(
        catalog_row_value(template_table, &snowball_template_row, "tmpllexize")
            .and_then(catalog_value_oid),
        Some(DSNOWBALL_LEXIZE_PROC_OID)
    );

    let proc_table = static_catalog_by_name("pg_proc").expect("pg_proc");
    let proc_rows = catalog_rows(PG_PROC_RELATION_OID);
    assert!(proc_rows.iter().any(|row| {
        catalog_row_value(proc_table, row, "oid").and_then(catalog_value_oid)
            == Some(DSNOWBALL_INIT_PROC_OID)
            && catalog_row_value(proc_table, row, "probin").and_then(catalog_value_string)
                == Some("$libdir/dict_snowball".to_owned())
    }));
    assert!(proc_rows.iter().any(|row| {
        let arg_types = catalog_row_value(proc_table, row, "proargtypes");
        catalog_row_value(proc_table, row, "oid").and_then(catalog_value_oid)
            == Some(DSNOWBALL_LEXIZE_PROC_OID)
            && matches!(
                arg_types,
                Some(CatalogValue::OidVector(values))
                    if values == &vec![INTERNAL_OID, INTERNAL_OID, INTERNAL_OID, INTERNAL_OID]
            )
    }));

    let map_table = static_catalog_by_name("pg_ts_config_map").expect("pg_ts_config_map");
    let english_map_rows = catalog_rows(PG_TS_CONFIG_MAP_RELATION_OID)
        .into_iter()
        .filter(|row| {
            catalog_row_value(map_table, row, "mapcfg").and_then(catalog_value_oid)
                == Some(ENGLISH_TS_CONFIG_OID)
        })
        .collect::<Vec<_>>();

    assert_eq!(english_map_rows.len(), 19);
    let token_types = english_map_rows
        .iter()
        .map(|row| catalog_row_value(map_table, row, "maptokentype").and_then(catalog_value_i32))
        .collect::<Option<Vec<_>>>()
        .expect("map token types");
    let mut sorted_token_types = token_types.clone();
    sorted_token_types.sort();
    assert_eq!(token_types, sorted_token_types);
    assert!(english_map_rows.iter().any(|row| {
        catalog_row_value(map_table, row, "maptokentype").and_then(catalog_value_i32) == Some(1)
            && catalog_row_value(map_table, row, "mapdict").and_then(catalog_value_oid)
                == Some(ENGLISH_TS_DICT_OID)
    }));
    assert!(english_map_rows.iter().any(|row| {
        catalog_row_value(map_table, row, "maptokentype").and_then(catalog_value_i32) == Some(3)
            && catalog_row_value(map_table, row, "mapdict").and_then(catalog_value_oid)
                == Some(SIMPLE_TS_DICT_OID)
    }));
}

#[test]
fn pg_proc_bootstrap_defaults_are_normalized() {
    let table = static_catalog_by_name("pg_proc").expect("pg_proc");
    let row = catalog_rows(PG_PROC_RELATION_OID)
        .into_iter()
        .find(|row| value_name(row_value("pg_proc", row, "proname")) == Some("parse_ident"))
        .expect("parse_ident proc row");
    assert_eq!(
        value_i16(row_value("pg_proc", &row, "pronargdefaults")),
        Some(1)
    );
    let defaults = catalog_row_value(table, &row, "proargdefaults")
        .and_then(catalog_value_string)
        .expect("proargdefaults");
    assert!(defaults.starts_with("({CONST :consttype 16"));
    assert!(defaults.contains(":constvalue 1 [ 1 0 0 0 0 0 0 0 ]"));
}

#[test]
fn pg_proc_bootstrap_default_count_is_normalized_for_unhandled_nodes() {
    let row = catalog_rows(PG_PROC_RELATION_OID)
        .into_iter()
        .find(|row| value_name(row_value("pg_proc", row, "proname")) == Some("jsonb_path_exists"))
        .expect("jsonb_path_exists proc row");
    assert_eq!(
        value_i16(row_value("pg_proc", &row, "pronargdefaults")),
        Some(2)
    );
    let defaults = catalog_row_value(
        static_catalog_by_name("pg_proc").expect("pg_proc"),
        &row,
        "proargdefaults",
    )
    .and_then(catalog_value_string)
    .expect("proargdefaults");
    assert!(defaults.starts_with("({CONST :consttype 3802"));
    assert!(defaults.contains(":constvalue 8 [ 32 0 0 0 0 0 0 32 ]"));
    assert!(defaults.contains("{CONST :consttype 16"));
    assert!(defaults.contains(":constvalue 1 [ 0 0 0 0 0 0 0 0 ]"));
}

#[test]
fn ensure_database_adds_synthetic_pg_database_row() {
    let _guard = catalog_test_lock();
    clear_for_tests();
    abort_implicit_transaction();
    let oid = ensure_database("regression");
    let table = static_catalog_by_name("pg_database").expect("pg_database");
    let row = catalog_rows(PG_DATABASE_RELATION_OID)
        .into_iter()
        .find(|row| {
            row_value("pg_database", row, "datname") == &CatalogValue::Name("regression".to_owned())
        })
        .expect("regression database row");

    assert_eq!(
        catalog_row_value(table, &row, "oid").and_then(catalog_value_oid),
        Some(oid)
    );
    assert_eq!(row.row_id, u64::from(oid.0));
}

#[test]
fn classifies_pgbench_critical_virtual_catalogs() {
    let required = [
        ("pg_type", VirtualCatalogPolicy::Dynamic),
        ("pg_proc", VirtualCatalogPolicy::Dynamic),
        ("pg_operator", VirtualCatalogPolicy::Dynamic),
        ("pg_aggregate", VirtualCatalogPolicy::Static),
        ("pg_namespace", VirtualCatalogPolicy::Static),
        ("pg_cast", VirtualCatalogPolicy::Dynamic),
        ("pg_class", VirtualCatalogPolicy::Dynamic),
        ("pg_attribute", VirtualCatalogPolicy::Dynamic),
        ("pg_index", VirtualCatalogPolicy::Dynamic),
        ("pg_constraint", VirtualCatalogPolicy::Dynamic),
        ("pg_am", VirtualCatalogPolicy::Static),
        ("pg_opclass", VirtualCatalogPolicy::Dynamic),
        ("pg_opfamily", VirtualCatalogPolicy::Dynamic),
        ("pg_amop", VirtualCatalogPolicy::Dynamic),
        ("pg_amproc", VirtualCatalogPolicy::Dynamic),
        ("pg_rewrite", VirtualCatalogPolicy::Dynamic),
        ("pg_statistic", VirtualCatalogPolicy::Static),
        ("pg_statistic_ext", VirtualCatalogPolicy::Static),
        ("pg_statistic_ext_data", VirtualCatalogPolicy::Static),
        ("pg_authid", VirtualCatalogPolicy::Static),
        ("pg_roles", VirtualCatalogPolicy::Static),
        ("pg_indexes", VirtualCatalogPolicy::Dynamic),
        ("pg_auth_members", VirtualCatalogPolicy::Static),
        ("pg_parameter_acl", VirtualCatalogPolicy::Static),
    ];

    for (name, policy) in required {
        let record = virtual_catalogs()
            .iter()
            .find(|record| record.name == name)
            .unwrap_or_else(|| panic!("{name} should have virtual catalog policy"));
        assert_eq!(record.policy, policy, "{name}");
    }
}

#[test]
fn virtual_catalog_relation_metadata_includes_rowtype_and_stats() {
    let proc_table = static_catalog_by_name("pg_proc").expect("pg_proc catalog");
    let rowtype_oid = relation_rowtype_oid_by_oid(proc_table.oid).expect("rowtype oid");
    assert_eq!(rowtype_oid, proc_table.rowtype_oid);

    let stats = relation_planner_stats_by_oid(proc_table.oid).expect("planner stats");
    assert_eq!(stats.reltuples, catalog_rows(proc_table.oid).len() as f32);
    assert!(stats.relpages > 0);
}

#[test]
fn virtual_catalog_rowtypes_are_visible_in_pg_type() {
    let operator_table = static_catalog_by_name("pg_operator").expect("pg_operator catalog");
    let pg_type = static_catalog_by_name("pg_type").expect("pg_type catalog");
    let rowtype_oid = static_catalog_rowtype_oid(operator_table.oid).expect("rowtype oid");
    assert_eq!(
        relation_rowtype_oid_by_oid(operator_table.oid),
        Some(rowtype_oid)
    );

    let row = catalog_rows(pg_type.oid)
        .into_iter()
        .find(|row| {
            catalog_row_value(pg_type, row, "oid").and_then(catalog_value_oid) == Some(rowtype_oid)
        })
        .expect("pg_operator rowtype");

    assert_eq!(
        catalog_row_value(pg_type, &row, "typname").and_then(catalog_value_string),
        Some("pg_operator".to_owned())
    );
    assert_eq!(
        catalog_row_value(pg_type, &row, "typrelid").and_then(catalog_value_oid),
        Some(operator_table.oid)
    );
}

#[test]
fn virtual_catalog_system_attributes_are_visible_in_pg_attribute() {
    let operator_table = static_catalog_by_name("pg_operator").expect("pg_operator catalog");
    let pg_attribute = static_catalog_by_name("pg_attribute").expect("pg_attribute catalog");
    let rows = catalog_rows(pg_attribute.oid);

    for (attname, attnum, type_oid) in [("ctid", -1, TID_OID), ("tableoid", -6, OID_OID)] {
        let row = rows
            .iter()
            .find(|row| {
                catalog_row_value(pg_attribute, row, "attrelid").and_then(catalog_value_oid)
                    == Some(operator_table.oid)
                    && catalog_row_value(pg_attribute, row, "attnum").and_then(catalog_value_i16)
                        == Some(attnum)
            })
            .unwrap_or_else(|| panic!("{attname} system attribute"));
        assert_eq!(
            catalog_row_value(pg_attribute, row, "attname").and_then(catalog_value_string),
            Some(attname.to_owned())
        );
        assert_eq!(
            catalog_row_value(pg_attribute, row, "atttypid").and_then(catalog_value_oid),
            Some(type_oid)
        );
    }
}

#[test]
fn static_catalog_attributes_are_visible_in_pg_attribute() {
    let pg_attribute = static_catalog_by_name("pg_attribute").expect("pg_attribute catalog");
    let aggregate_table = static_catalog_by_name("pg_aggregate").expect("pg_aggregate catalog");
    let rows = catalog_rows(pg_attribute.oid);
    let row = rows
        .iter()
        .find(|row| {
            catalog_row_value(pg_attribute, row, "attrelid").and_then(catalog_value_oid)
                == Some(aggregate_table.oid)
                && catalog_row_value(pg_attribute, row, "attname").and_then(catalog_value_string)
                    == Some("aggfnoid".to_owned())
        })
        .expect("pg_aggregate.aggfnoid attribute");

    assert_eq!(
        catalog_row_value(pg_attribute, row, "atttypid").and_then(catalog_value_oid),
        Some(Oid(24))
    );
}

#[test]
fn static_catalog_physical_columns_resolve_from_typed_schema() {
    let aggregate_table = static_catalog_by_name("pg_aggregate").expect("pg_aggregate catalog");
    let column =
        relation_physical_column_by_attnum(aggregate_table.oid, 1).expect("pg_aggregate.aggfnoid");

    assert_eq!(column.name, "aggfnoid");
    assert_eq!(column.type_oid, Oid(24));
    assert_eq!(column.type_mod, -1);
    assert!(!column.is_dropped);
    assert_eq!(column.is_not_null, aggregate_table.columns[0].attnotnull);
    assert_eq!(column.attlen, 4);
    assert!(column.attbyval);
    assert_eq!(column.attalign, b'i');
    assert_eq!(column.attstorage, b'p');
}

#[test]
fn relation_oid_exists_uses_typed_membership() {
    let pg_class = static_catalog_by_name("pg_class").expect("pg_class catalog");

    assert!(relation_oid_exists(pg_class.oid));
    assert!(relation_oid_exists(PG_AGGREGATE_FNOID_INDEX_OID));
    assert!(!relation_oid_exists(Oid(0xEE00_0001)));
}

#[test]
fn information_schema_domains_mark_domain_input() {
    let pg_namespace = static_catalog_by_name("pg_namespace").expect("pg_namespace catalog");
    let namespace_rows = catalog_rows(pg_namespace.oid);
    assert!(namespace_rows.iter().any(|row| {
        catalog_row_value(pg_namespace, row, "oid").and_then(catalog_value_oid)
            == Some(INFORMATION_SCHEMA_NAMESPACE_OID)
            && catalog_row_value(pg_namespace, row, "nspname").and_then(catalog_value_string)
                == Some("information_schema".to_owned())
    }));
    assert_eq!(
        builtin_namespace_by_name("information_schema").map(|record| record.oid),
        Some(INFORMATION_SCHEMA_NAMESPACE_OID)
    );

    let pg_type = static_catalog_by_name("pg_type").expect("pg_type catalog");
    let type_rows = catalog_rows(pg_type.oid);
    let row = type_rows
        .iter()
        .find(|row| {
            catalog_row_value(pg_type, row, "typnamespace").and_then(catalog_value_oid)
                == Some(INFORMATION_SCHEMA_NAMESPACE_OID)
                && catalog_row_value(pg_type, row, "typname").and_then(catalog_value_string)
                    == Some("cardinal_number".to_owned())
        })
        .expect("information_schema.cardinal_number type");

    assert_eq!(
        catalog_row_value(pg_type, row, "typtype").and_then(catalog_value_u8),
        Some(b'd')
    );
    assert_eq!(
        catalog_row_value(pg_type, row, "typinput").and_then(catalog_value_oid),
        Some(Oid(2597))
    );
    assert_eq!(
        catalog_row_value(pg_type, row, "typbasetype").and_then(catalog_value_oid),
        Some(INT4_OID)
    );
}

fn assert_catalog_index_metadata(
    pg_class: &StaticCatalogTable,
    pg_index: &StaticCatalogTable,
    index_oid: Oid,
    index_name: &str,
    relation_oid: Oid,
    key_attnums: Vec<i16>,
) {
    let class_rows = catalog_rows(pg_class.oid);
    assert!(class_rows.iter().any(|row| {
        catalog_row_value(pg_class, row, "oid").and_then(catalog_value_oid) == Some(index_oid)
            && catalog_row_value(pg_class, row, "relname").and_then(catalog_value_string)
                == Some(index_name.to_owned())
            && catalog_row_value(pg_class, row, "relkind").and_then(catalog_value_u8) == Some(b'i')
    }));

    let index_rows = catalog_rows(pg_index.oid);
    let row = index_rows
        .iter()
        .find(|row| {
            catalog_row_value(pg_index, row, "indexrelid").and_then(catalog_value_oid)
                == Some(index_oid)
        })
        .unwrap_or_else(|| panic!("{index_name} row"));

    assert_eq!(
        catalog_row_value(pg_index, row, "indrelid").and_then(catalog_value_oid),
        Some(relation_oid)
    );
    assert_eq!(
        catalog_row_value(pg_index, row, "indkey").and_then(catalog_value_int2_vector),
        Some(key_attnums)
    );
}

#[test]
fn virtual_catalog_index_metadata_is_visible() {
    let pg_class = static_catalog_by_name("pg_class").expect("pg_class catalog");
    let pg_index = static_catalog_by_name("pg_index").expect("pg_index catalog");
    for (index_oid, index_name, relation_oid, key_attnums) in [
        (
            PG_AGGREGATE_FNOID_INDEX_OID,
            "pg_aggregate_fnoid_index",
            PG_AGGREGATE_RELATION_OID,
            vec![1],
        ),
        (
            PG_CLASS_OID_INDEX_OID,
            "pg_class_oid_index",
            PG_CLASS_RELATION_OID,
            vec![1],
        ),
        (
            PG_ATTRIBUTE_RELID_NUM_INDEX_OID,
            "pg_attribute_relid_attnum_index",
            PG_ATTRIBUTE_RELATION_OID,
            vec![1, 6],
        ),
        (
            PG_NAMESPACE_OID_INDEX_OID,
            "pg_namespace_oid_index",
            PG_NAMESPACE_RELATION_OID,
            vec![1],
        ),
        (
            PG_TS_CONFIG_MAP_INDEX_OID,
            "pg_ts_config_map_index",
            PG_TS_CONFIG_MAP_RELATION_OID,
            vec![1, 2, 3],
        ),
    ] {
        assert_catalog_index_metadata(
            pg_class,
            pg_index,
            index_oid,
            index_name,
            relation_oid,
            key_attnums,
        );
    }
}

#[test]
fn virtual_catalog_index_attributes_are_visible() {
    let pg_attribute = static_catalog_by_name("pg_attribute").expect("pg_attribute catalog");
    let rows = catalog_rows(pg_attribute.oid);
    let relname = rows.iter().find(|row| {
        catalog_row_value(pg_attribute, row, "attrelid").and_then(catalog_value_oid)
            == Some(PG_CLASS_NAME_NSP_INDEX_OID)
            && catalog_row_value(pg_attribute, row, "attnum").and_then(catalog_value_i16) == Some(1)
    });
    assert_eq!(
        relname
            .and_then(|row| catalog_row_value(pg_attribute, row, "attname"))
            .and_then(catalog_value_string),
        Some("relname".to_owned())
    );

    let attrelid_attnum = pg_attribute
        .columns
        .iter()
        .position(|column| column.name == "attrelid")
        .and_then(|index| i16::try_from(index + 1).ok())
        .expect("pg_attribute.attrelid attnum");
    let attnum_attnum = pg_attribute
        .columns
        .iter()
        .position(|column| column.name == "attnum")
        .and_then(|index| i16::try_from(index + 1).ok())
        .expect("pg_attribute.attnum attnum");
    let filtered = catalog_rows_matching_filters(
        pg_attribute.oid,
        &[
            CatalogRowFilter {
                attnum: attrelid_attnum,
                value: CatalogFilterValue::Oid(PG_CLASS_NAME_NSP_INDEX_OID),
            },
            CatalogRowFilter {
                attnum: attnum_attnum,
                value: CatalogFilterValue::Int16(2),
            },
        ],
    );
    assert_eq!(filtered.len(), 1);
    assert_eq!(
        catalog_row_value(pg_attribute, &filtered[0], "attname").and_then(catalog_value_string),
        Some("relnamespace".to_owned())
    );

    let physical_column = relation_physical_column_by_attnum(PG_CLASS_NAME_NSP_INDEX_OID, 1)
        .expect("index physical column");
    assert_eq!(physical_column.name, "relname");
}

#[test]
fn filtered_pg_index_rows_use_dynamic_catalog_indexes() {
    let _guard = catalog_test_lock();
    clear_for_tests();
    abort_implicit_transaction();

    upsert_named_catalog_row(
        "pg_index",
        61_001,
        &[
            ("indexrelid", "61001"),
            ("indrelid", "61000"),
            ("indnatts", "1"),
            ("indnkeyatts", "1"),
            ("indisunique", "f"),
            ("indisprimary", "f"),
            ("indisvalid", "t"),
            ("indisready", "t"),
            ("indislive", "t"),
            ("indkey", "1"),
        ],
    );
    commit_implicit_transaction();

    let pg_index = static_catalog_by_name("pg_index").expect("pg_index catalog");
    let indrelid_attnum = pg_index
        .columns
        .iter()
        .position(|column| column.name == "indrelid")
        .and_then(|index| i16::try_from(index + 1).ok())
        .expect("pg_index.indrelid attnum");
    let filtered = catalog_rows_matching_filters(
        pg_index.oid,
        &[CatalogRowFilter {
            attnum: indrelid_attnum,
            value: CatalogFilterValue::Oid(Oid(61_000)),
        }],
    );

    assert_eq!(filtered.len(), 1);
    assert_eq!(
        catalog_row_value(pg_index, &filtered[0], "indexrelid").and_then(catalog_value_oid),
        Some(Oid(61_001))
    );

    clear_for_tests();
    abort_implicit_transaction();
}

#[test]
fn filtered_pg_class_rows_use_dynamic_catalog_indexes() {
    let _guard = catalog_test_lock();
    clear_for_tests();
    abort_implicit_transaction();

    upsert_named_catalog_row(
        "pg_class",
        63_000,
        &[
            ("oid", "63000"),
            ("relname", "filtered_relation"),
            ("relnamespace", "2200"),
            ("reltype", "63001"),
            ("relowner", "10"),
            ("relam", "2"),
            ("relfilenode", "63000"),
            ("relhasindex", "f"),
            ("relpersistence", "p"),
            ("relkind", "r"),
            ("relnatts", "0"),
        ],
    );
    commit_implicit_transaction();

    let pg_class = static_catalog_by_name("pg_class").expect("pg_class catalog");
    let oid_attnum = pg_class
        .columns
        .iter()
        .position(|column| column.name == "oid")
        .and_then(|index| i16::try_from(index + 1).ok())
        .expect("pg_class.oid attnum");
    let filtered = catalog_rows_matching_filters(
        pg_class.oid,
        &[CatalogRowFilter {
            attnum: oid_attnum,
            value: CatalogFilterValue::Oid(Oid(63_000)),
        }],
    );

    assert_eq!(filtered.len(), 1);
    assert_eq!(
        catalog_row_value(pg_class, &filtered[0], "relname").and_then(catalog_value_string),
        Some("filtered_relation".to_owned())
    );

    clear_for_tests();
    abort_implicit_transaction();
}

#[test]
fn filtered_pg_constraint_rows_use_dynamic_catalog_indexes() {
    let _guard = catalog_test_lock();
    clear_for_tests();
    abort_implicit_transaction();

    upsert_named_catalog_row(
        "pg_constraint",
        64_002,
        &[
            ("oid", "64002"),
            ("conname", "filtered_constraint"),
            ("connamespace", "2200"),
            ("contype", "p"),
            ("conrelid", "64000"),
            ("conindid", "64001"),
            ("conkey", "1"),
        ],
    );
    commit_implicit_transaction();

    let pg_constraint = static_catalog_by_name("pg_constraint").expect("pg_constraint catalog");
    let conrelid_attnum = pg_constraint
        .columns
        .iter()
        .position(|column| column.name == "conrelid")
        .and_then(|index| i16::try_from(index + 1).ok())
        .expect("pg_constraint.conrelid attnum");
    let filtered = catalog_rows_matching_filters(
        pg_constraint.oid,
        &[CatalogRowFilter {
            attnum: conrelid_attnum,
            value: CatalogFilterValue::Oid(Oid(64_000)),
        }],
    );

    assert_eq!(filtered.len(), 1);
    assert_eq!(
        catalog_row_value(pg_constraint, &filtered[0], "conindid").and_then(catalog_value_oid),
        Some(Oid(64_001))
    );

    clear_for_tests();
    abort_implicit_transaction();
}

#[test]
fn filtered_pg_inherits_rows_use_compat_catalog_indexes() {
    let _guard = catalog_test_lock();
    clear_for_tests();
    abort_implicit_transaction();

    upsert_named_catalog_row(
        "pg_inherits",
        62_001,
        &[
            ("inhrelid", "62001"),
            ("inhparent", "62000"),
            ("inhseqno", "1"),
            ("inhdetachpending", "f"),
        ],
    );
    commit_implicit_transaction();

    let pg_inherits = static_catalog_by_name("pg_inherits").expect("pg_inherits catalog");
    let inhrelid_attnum = pg_inherits
        .columns
        .iter()
        .position(|column| column.name == "inhrelid")
        .and_then(|index| i16::try_from(index + 1).ok())
        .expect("pg_inherits.inhrelid attnum");
    let filtered = catalog_rows_matching_filters(
        pg_inherits.oid,
        &[CatalogRowFilter {
            attnum: inhrelid_attnum,
            value: CatalogFilterValue::Oid(Oid(62_001)),
        }],
    );

    assert_eq!(filtered.len(), 1);
    assert_eq!(
        catalog_row_value(pg_inherits, &filtered[0], "inhparent").and_then(catalog_value_oid),
        Some(Oid(62_000))
    );

    clear_for_tests();
    abort_implicit_transaction();
}

#[test]
fn operator_implementation_proc_descriptions_are_synthesized() {
    let pg_description = static_catalog_by_name("pg_description").expect("pg_description catalog");
    let rows = catalog_rows(pg_description.oid);
    let synthesized_description = |oid| {
        rows.iter()
            .find_map(|row| {
                let matches = catalog_row_value(pg_description, row, "classoid")
                    .and_then(catalog_value_oid)
                    == Some(PG_PROC_RELATION_OID)
                    && catalog_row_value(pg_description, row, "objoid").and_then(catalog_value_oid)
                        == Some(oid);
                matches.then(|| {
                    catalog_row_value(pg_description, row, "description")
                        .and_then(catalog_value_string)
                })?
            })
            .unwrap_or_else(|| panic!("description for proc {oid:?}"))
    };

    assert_eq!(
        synthesized_description(Oid(56)),
        "implementation of < operator"
    );
    assert_eq!(
        synthesized_description(Oid(125)),
        "implementation of && operator"
    );
    assert_eq!(
        synthesized_description(Oid(131)),
        "implementation of |>> operator"
    );
    assert_eq!(
        synthesized_description(Oid(3634)),
        "implementation of @@ operator"
    );
}

#[test]
fn visible_catalog_snapshot_cache_updates_after_overlay_changes() {
    let _guard = catalog_test_lock();
    clear_for_tests();
    abort_implicit_transaction();
    let relation_oid = Oid(50_010);

    upsert_named_catalog_row(
        "pg_class",
        relation_oid.0 as u64,
        &[
            ("oid", "50010"),
            ("relname", "cached_snapshot_before_change"),
            ("relnamespace", "2200"),
            ("reltype", "50011"),
            ("relowner", "10"),
            ("relam", "2"),
            ("relfilenode", "50010"),
            ("relhasindex", "f"),
            ("relpersistence", "p"),
            ("relkind", "r"),
            ("relnatts", "0"),
        ],
    );

    crate::state::reset_visible_catalog_snapshot_materializations_for_tests();
    assert!(relation_oid_exists(relation_oid));
    assert!(relation_oid_exists(relation_oid));
    assert_eq!(
        crate::state::visible_catalog_snapshot_materializations_for_tests(),
        1
    );

    upsert_named_catalog_row(
        "pg_class",
        relation_oid.0 as u64,
        &[
            ("oid", "50010"),
            ("relname", "cached_snapshot_after_change"),
            ("relnamespace", "2200"),
            ("reltype", "50011"),
            ("relowner", "10"),
            ("relam", "2"),
            ("relfilenode", "50010"),
            ("relhasindex", "f"),
            ("relpersistence", "p"),
            ("relkind", "r"),
            ("relnatts", "0"),
        ],
    );
    assert!(relation_by_name("cached_snapshot_after_change").is_some());
    assert!(relation_oid_exists(relation_oid));
    assert_eq!(
        crate::state::visible_catalog_snapshot_materializations_for_tests(),
        1
    );

    abort_implicit_transaction();
    assert!(!relation_oid_exists(relation_oid));
}

#[test]
fn generic_overlay_rows_drive_relation_views() {
    let _guard = catalog_test_lock();
    clear_for_tests();
    abort_implicit_transaction();
    let relation_oid = Oid(50_000);
    let type_oid = Oid(50_001);
    let index_oid = Oid(50_002);
    let constraint_oid = Oid(50_003);
    let initial_generation = current_generation();

    upsert_named_catalog_row(
        "pg_class",
        relation_oid.0 as u64,
        &[
            ("oid", "50000"),
            ("relname", "pgbench_accounts"),
            ("relnamespace", "2200"),
            ("reltype", "50001"),
            ("relowner", "10"),
            ("relam", "2"),
            ("relfilenode", "50000"),
            ("relhasindex", "f"),
            ("relpersistence", "p"),
            ("relkind", "r"),
            ("relnatts", "2"),
        ],
    );
    upsert_named_catalog_row(
        "pg_type",
        type_oid.0 as u64,
        &[
            ("oid", "50001"),
            ("typname", "pgbench_accounts"),
            ("typnamespace", "2200"),
            ("typowner", "10"),
            ("typlen", "-1"),
            ("typbyval", "f"),
            ("typtype", "c"),
            ("typcategory", "C"),
            ("typisdefined", "t"),
            ("typdelim", ","),
            ("typrelid", "50000"),
            ("typalign", "d"),
            ("typstorage", "x"),
        ],
    );
    upsert_named_catalog_row(
        "pg_attribute",
        0,
        &[
            ("attrelid", "50000"),
            ("attname", "aid"),
            ("atttypid", "23"),
            ("attlen", "4"),
            ("attnum", "1"),
            ("atttypmod", "-1"),
            ("attbyval", "t"),
            ("attalign", "i"),
            ("attstorage", "p"),
            ("attnotnull", "t"),
            ("attisdropped", "f"),
        ],
    );
    upsert_named_catalog_row(
        "pg_attribute",
        0,
        &[
            ("attrelid", "50000"),
            ("attname", "filler"),
            ("atttypid", "1042"),
            ("attlen", "-1"),
            ("attnum", "2"),
            ("atttypmod", "-1"),
            ("attbyval", "f"),
            ("attalign", "i"),
            ("attstorage", "x"),
            ("attnotnull", "f"),
            ("attisdropped", "f"),
        ],
    );
    commit_implicit_transaction();
    assert!(current_generation() > initial_generation);
    let after_create_generation = current_generation();
    let relation = relation_by_name("PgBench_Accounts").expect("relation");
    assert_eq!(relation.name, "pgbench_accounts");
    assert_eq!(relation.columns[0].name, "aid");
    assert_eq!(
        relation_by_name("pgbench_accounts").unwrap().oid,
        relation.oid
    );
    assert!(relation_oid_exists(relation.oid));
    assert_eq!(
        relation_oid_by_name_in_namespace("pgbench_accounts", PUBLIC_NAMESPACE_OID),
        Some(relation.oid)
    );
    assert_eq!(
        relation_by_oid(relation.oid).unwrap().name,
        "pgbench_accounts"
    );
    let relation_summary = relation_summary_by_oid(relation.oid).unwrap();
    assert_eq!(relation_summary.name, "pgbench_accounts");
    assert_eq!(relation_summary.column_count, 2);
    assert!(!relation_summary.has_indexes);
    assert!(!relation_summary.has_primary_key);
    let relation_summary =
        relation_summary_by_name_in_namespace("PgBench_Accounts", PUBLIC_NAMESPACE_OID).unwrap();
    assert_eq!(relation_summary.name, "pgbench_accounts");
    assert_eq!(relation_summary.column_count, 2);
    assert!(!relation_summary.has_indexes);
    assert!(!relation_summary.has_primary_key);
    assert_eq!(
        relation_column_by_attnum(relation.oid, 1).unwrap().name,
        "aid"
    );
    upsert_named_catalog_row(
        "pg_class",
        relation_oid.0 as u64,
        &[
            ("oid", "50000"),
            ("relname", "pgbench_accounts"),
            ("relnamespace", "2200"),
            ("reltype", "50001"),
            ("relowner", "10"),
            ("relam", "2"),
            ("relfilenode", "50000"),
            ("relhasindex", "t"),
            ("relpersistence", "p"),
            ("relkind", "r"),
            ("relnatts", "2"),
        ],
    );
    upsert_named_catalog_row(
        "pg_class",
        index_oid.0 as u64,
        &[
            ("oid", "50002"),
            ("relname", "pgbench_accounts_pkey"),
            ("relnamespace", "2200"),
            ("reltype", "0"),
            ("relowner", "10"),
            ("relam", "403"),
            ("relfilenode", "50002"),
            ("relhasindex", "f"),
            ("relpersistence", "p"),
            ("relkind", "i"),
            ("relnatts", "1"),
        ],
    );
    upsert_named_catalog_row(
        "pg_attribute",
        0,
        &[
            ("attrelid", "50002"),
            ("attname", "aid"),
            ("atttypid", "23"),
            ("attlen", "4"),
            ("attnum", "1"),
            ("atttypmod", "-1"),
            ("attbyval", "t"),
            ("attalign", "i"),
            ("attstorage", "p"),
            ("attnotnull", "t"),
            ("attisdropped", "f"),
        ],
    );
    upsert_named_catalog_row(
        "pg_index",
        index_oid.0 as u64,
        &[
            ("indexrelid", "50002"),
            ("indrelid", "50000"),
            ("indnatts", "1"),
            ("indnkeyatts", "1"),
            ("indisunique", "t"),
            ("indisprimary", "t"),
            ("indisvalid", "t"),
            ("indisready", "t"),
            ("indislive", "t"),
            ("indkey", "1"),
        ],
    );
    upsert_named_catalog_row(
        "pg_constraint",
        constraint_oid.0 as u64,
        &[
            ("oid", "50003"),
            ("conname", "pgbench_accounts_pkey"),
            ("connamespace", "2200"),
            ("contype", "p"),
            ("conrelid", "50000"),
            ("conindid", "50002"),
            ("conkey", "1"),
        ],
    );
    commit_implicit_transaction();
    let after_primary_key_generation = current_generation();
    assert!(after_primary_key_generation > after_create_generation);
    assert_eq!(
        relation_by_name("pgbench_accounts").unwrap().primary_key,
        vec!["aid"]
    );
    assert_eq!(
        unique_index_oids_for_relation_oid(relation.oid),
        vec![index_oid]
    );
    assert_eq!(
        primary_key_index_oid_for_relation_oid(relation.oid),
        Some(index_oid)
    );
    assert_eq!(
        primary_key_index_record_for_relation_oid(relation.oid)
            .unwrap()
            .index_oid,
        index_oid
    );
    let relation_summary = relation_summary_by_oid(relation.oid).unwrap();
    assert_eq!(relation_summary.column_count, 2);
    assert!(relation_summary.has_indexes);
    assert!(relation_summary.has_primary_key);
    let relation_summary =
        relation_summary_by_name_in_namespace("pgbench_accounts", PUBLIC_NAMESPACE_OID).unwrap();
    assert_eq!(relation_summary.column_count, 2);
    assert!(relation_summary.has_indexes);
    assert!(relation_summary.has_primary_key);
    assert!(catalog_rows(PG_CLASS_RELATION_OID).iter().any(|row| {
        value_name(row_value("pg_class", row, "relname")) == Some("pgbench_accounts")
    }));
    assert!(catalog_rows(PG_TYPE_RELATION_OID).iter().any(|row| {
        value_name(row_value("pg_type", row, "typname")) == Some("pgbench_accounts")
    }));
    assert!(catalog_rows(PG_ATTRIBUTE_RELATION_OID).iter().any(|row| {
        value_oid(row_value("pg_attribute", row, "attrelid")) == Some(relation.oid)
            && value_name(row_value("pg_attribute", row, "attname")) == Some("aid")
    }));
    assert!(
        catalog_rows(PG_INDEX_RELATION_OID)
            .iter()
            .any(|row| { value_oid(row_value("pg_index", row, "indrelid")) == Some(relation.oid) })
    );
    assert!(catalog_rows(PG_INDEXES_RELATION_OID).iter().any(|row| {
        value_name(row_value("pg_indexes", row, "schemaname")) == Some("public")
            && value_name(row_value("pg_indexes", row, "tablename")) == Some("pgbench_accounts")
            && value_name(row_value("pg_indexes", row, "indexname"))
                == Some("pgbench_accounts_pkey")
            && row_value("pg_indexes", row, "indexdef")
                == &CatalogValue::Text(
                    "CREATE UNIQUE INDEX pgbench_accounts_pkey ON public.pgbench_accounts USING btree (aid)"
                        .to_owned(),
                )
    }));
    assert!(catalog_rows(PG_CONSTRAINT_RELATION_OID).iter().any(|row| {
        value_oid(row_value("pg_constraint", row, "conrelid")) == Some(relation.oid)
            && value_name(row_value("pg_constraint", row, "conname"))
                == Some("pgbench_accounts_pkey")
    }));
    delete_catalog_row(PG_CLASS_RELATION_OID, relation.oid.0 as u64).unwrap();
    commit_implicit_transaction();
    assert!(current_generation() > after_primary_key_generation);
    assert!(relation_by_name("pgbench_accounts").is_none());
    assert!(!relation_oid_exists(relation.oid));
    assert!(!catalog_rows(PG_CLASS_RELATION_OID).iter().any(|row| {
        value_name(row_value("pg_class", row, "relname")) == Some("pgbench_accounts")
    }));
    assert!(!catalog_rows(PG_INDEXES_RELATION_OID).iter().any(|row| {
        value_name(row_value("pg_indexes", row, "tablename")) == Some("pgbench_accounts")
    }));
}

#[test]
fn physical_columns_preserve_dropped_attnum_holes() {
    let _guard = catalog_test_lock();
    clear_for_tests();
    abort_implicit_transaction();
    let relation_oid = Oid(50_100);

    upsert_named_catalog_row(
        "pg_class",
        relation_oid.0 as u64,
        &[
            ("oid", "50100"),
            ("relname", "dropped_columns"),
            ("relnamespace", "2200"),
            ("reltype", "50101"),
            ("relowner", "10"),
            ("relam", "2"),
            ("relfilenode", "50100"),
            ("relhasindex", "f"),
            ("relpersistence", "p"),
            ("relkind", "r"),
            ("relnatts", "3"),
        ],
    );
    upsert_named_catalog_row(
        "pg_attribute",
        0,
        &[
            ("attrelid", "50100"),
            ("attname", "........pg.dropped.1........"),
            ("atttypid", "0"),
            ("attlen", "4"),
            ("attnum", "1"),
            ("atttypmod", "-1"),
            ("attbyval", "t"),
            ("attalign", "i"),
            ("attstorage", "p"),
            ("attnotnull", "f"),
            ("attisdropped", "t"),
        ],
    );
    upsert_named_catalog_row(
        "pg_attribute",
        0,
        &[
            ("attrelid", "50100"),
            ("attname", "live_b"),
            ("atttypid", "23"),
            ("attlen", "4"),
            ("attnum", "2"),
            ("atttypmod", "-1"),
            ("attbyval", "t"),
            ("attalign", "i"),
            ("attstorage", "p"),
            ("attnotnull", "f"),
            ("attisdropped", "f"),
        ],
    );
    upsert_named_catalog_row(
        "pg_attribute",
        0,
        &[
            ("attrelid", "50100"),
            ("attname", "live_c"),
            ("atttypid", "25"),
            ("attlen", "-1"),
            ("attnum", "3"),
            ("atttypmod", "-1"),
            ("attbyval", "f"),
            ("attalign", "i"),
            ("attstorage", "x"),
            ("attnotnull", "f"),
            ("attisdropped", "f"),
            ("attcollation", "100"),
        ],
    );
    commit_implicit_transaction();

    let relation = relation_by_oid(relation_oid).expect("relation");
    assert_eq!(
        relation
            .columns
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>(),
        vec!["live_b", "live_c"]
    );
    assert_eq!(
        relation_summary_by_oid(relation_oid)
            .expect("relation summary")
            .column_count,
        3
    );
    assert!(relation_column_by_attnum(relation_oid, 1).is_none());
    assert_eq!(
        relation_column_by_attnum(relation_oid, 2)
            .expect("attnum 2")
            .name,
        "live_b"
    );

    let dropped = relation_physical_column_by_attnum(relation_oid, 1).expect("physical attnum 1");
    assert!(dropped.is_dropped);
    assert_eq!(dropped.type_oid, INVALID_OID);
    assert_eq!(dropped.attlen, 4);

    let live = relation_physical_column_by_attnum(relation_oid, 3).expect("physical attnum 3");
    assert!(!live.is_dropped);
    assert_eq!(live.name, "live_c");
    assert_eq!(live.type_oid, TEXT_OID);
    assert_eq!(live.attcollation, DEFAULT_COLLATION_OID);
}
