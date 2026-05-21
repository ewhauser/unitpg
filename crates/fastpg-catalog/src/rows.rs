use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;

use fastpg_types::Oid;

use crate::generated_catalog;
use crate::lookups::{lookup_builtin_type, relation_by_oid};
use crate::model::*;
use crate::state::{
    CatalogDraft, CatalogState, commit_catalog_draft, ensure_catalog_transaction,
    invalidate_visible_catalog_snapshot, with_catalog, with_catalog_session,
    with_visible_catalog_snapshot,
};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum CatalogFilterValue {
    Bool(bool),
    Char(u8),
    Int16(i16),
    Int32(i32),
    Oid(Oid),
    Name(String),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CatalogRowFilter {
    pub attnum: i16,
    pub value: CatalogFilterValue,
}

pub fn static_catalogs() -> &'static [StaticCatalogTable] {
    generated_catalog::STATIC_CATALOG_TABLES
}

pub fn static_catalog_by_relation_oid(relation_oid: Oid) -> Option<&'static StaticCatalogTable> {
    generated_catalog::STATIC_CATALOG_TABLES
        .iter()
        .find(|table| table.oid == relation_oid)
}

struct CachedStaticCatalogRows {
    rows: Vec<CatalogRow>,
    row_indexes: HashMap<u64, usize>,
}

fn static_catalog_row_cache() -> &'static HashMap<u32, CachedStaticCatalogRows> {
    static STATIC_CATALOG_ROW_CACHE: OnceLock<HashMap<u32, CachedStaticCatalogRows>> =
        OnceLock::new();
    STATIC_CATALOG_ROW_CACHE.get_or_init(|| {
        static_catalogs()
            .iter()
            .map(|table| {
                let rows = table
                    .rows
                    .iter()
                    .map(|row| static_row_to_catalog_row_uncached(table, row))
                    .collect::<Vec<_>>();
                let row_indexes = rows
                    .iter()
                    .enumerate()
                    .map(|(index, row)| (row.row_id, index))
                    .collect();
                (table.oid.0, CachedStaticCatalogRows { rows, row_indexes })
            })
            .collect()
    })
}

fn static_catalog_cached_row(
    table: &StaticCatalogTable,
    row: &StaticCatalogRow,
) -> Option<&'static CatalogRow> {
    let cached = static_catalog_row_cache().get(&table.oid.0)?;
    let index = cached.row_indexes.get(&row.row_id)?;
    cached.rows.get(*index)
}

pub fn static_catalog_by_name(name: &str) -> Option<&'static StaticCatalogTable> {
    let name = normalize_identifier(name);
    generated_catalog::STATIC_CATALOG_TABLES
        .iter()
        .find(|table| table.name == name.as_str())
}

fn synthetic_catalog_rowtype_oid(relation_oid: Oid) -> Oid {
    Oid(SYNTHETIC_CATALOG_ROWTYPE_OID_BASE | relation_oid.0)
}

fn static_catalog_table_rowtype_oid(table: &StaticCatalogTable) -> Oid {
    if table.rowtype_oid != INVALID_OID {
        table.rowtype_oid
    } else {
        synthetic_catalog_rowtype_oid(table.oid)
    }
}

pub fn static_catalog_rowtype_oid(relation_oid: Oid) -> Option<Oid> {
    static_catalog_by_relation_oid(relation_oid).map(static_catalog_table_rowtype_oid)
}

pub(crate) fn static_value_as_u32(value: StaticCatalogValue) -> Option<u32> {
    match value {
        StaticCatalogValue::Raw("-") => Some(0),
        StaticCatalogValue::Raw("NAMEDATALEN") => Some(64),
        StaticCatalogValue::Raw(value) => value.parse::<u32>().ok(),
        StaticCatalogValue::Null => None,
    }
}

pub(crate) fn static_value_as_i32(value: StaticCatalogValue) -> Option<i32> {
    match value {
        StaticCatalogValue::Raw("-") => Some(0),
        StaticCatalogValue::Raw("NAMEDATALEN") => Some(64),
        StaticCatalogValue::Raw(value) => value.parse::<i32>().ok(),
        StaticCatalogValue::Null => None,
    }
}

pub(crate) fn static_value_as_f32(value: StaticCatalogValue) -> Option<f32> {
    match value {
        StaticCatalogValue::Raw(value) => value.parse::<f32>().ok(),
        StaticCatalogValue::Null => None,
    }
}

pub(crate) fn static_value_as_bool(value: StaticCatalogValue) -> Option<bool> {
    match value {
        StaticCatalogValue::Raw("t") => Some(true),
        StaticCatalogValue::Raw("f") => Some(false),
        StaticCatalogValue::Raw("true") => Some(true),
        StaticCatalogValue::Raw("false") => Some(false),
        StaticCatalogValue::Null => None,
        StaticCatalogValue::Raw(_) => None,
    }
}

pub(crate) fn static_value_as_char(value: StaticCatalogValue) -> Option<u8> {
    match value {
        StaticCatalogValue::Raw("\\0") => Some(0),
        StaticCatalogValue::Raw(value) => value.as_bytes().first().copied().or(Some(0)),
        StaticCatalogValue::Null => None,
    }
}

pub(crate) fn parse_oid_vector(value: &str) -> Vec<Oid> {
    value
        .split_whitespace()
        .filter_map(|part| part.parse::<u32>().ok())
        .map(Oid)
        .collect()
}

pub(crate) fn parse_int2_vector(value: &str) -> Vec<i16> {
    value
        .split_whitespace()
        .filter_map(|part| part.parse::<i16>().ok())
        .collect()
}

fn parse_pg_array_elements(value: &str) -> Option<Vec<Option<String>>> {
    let inner = value.strip_prefix('{')?.strip_suffix('}')?;
    let mut elements = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut saw_quote = false;
    for ch in inner.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quoted => escaped = true,
            '"' => {
                quoted = !quoted;
                saw_quote = true;
            }
            ',' if !quoted => {
                if !saw_quote && current == "NULL" {
                    elements.push(None);
                } else {
                    elements.push(Some(std::mem::take(&mut current)));
                }
                saw_quote = false;
            }
            _ => current.push(ch),
        }
    }
    if !saw_quote && current == "NULL" {
        elements.push(None);
    } else if inner.is_empty() {
        return Some(Vec::new());
    } else {
        elements.push(Some(current));
    }
    Some(elements)
}

pub(crate) fn static_value_to_catalog_value(
    column: &StaticCatalogColumn,
    value: StaticCatalogValue,
) -> CatalogValue {
    match value {
        StaticCatalogValue::Null => CatalogValue::Null,
        StaticCatalogValue::Raw(raw) => match column.type_oid {
            BOOL_OID => CatalogValue::Bool(static_value_as_bool(value).unwrap_or(false)),
            CHAR_OID => CatalogValue::Char(static_value_as_char(value).unwrap_or(0)),
            INT2_OID => CatalogValue::Int16(static_value_as_i32(value).unwrap_or(0) as i16),
            INT4_OID => CatalogValue::Int32(static_value_as_i32(value).unwrap_or(0)),
            FLOAT4_OID => CatalogValue::Float32(static_value_as_f32(value).unwrap_or(0.0)),
            NAME_OID => CatalogValue::Name(raw.to_owned()),
            TEXT_OID | PG_NODE_TREE_OID => CatalogValue::Text(raw.to_owned()),
            OIDVECTOR_OID => CatalogValue::OidVector(parse_oid_vector(raw)),
            INT2VECTOR_OID => CatalogValue::Int2Vector(parse_int2_vector(raw)),
            OID_OID | REGCLASS_OID => {
                CatalogValue::Oid(Oid(static_value_as_u32(value).unwrap_or(0)))
            }
            _ if column.type_name.starts_with("reg") || column.type_name == "oid" => {
                CatalogValue::Oid(Oid(static_value_as_u32(value).unwrap_or(0)))
            }
            _ if column.type_name.starts_with('_') => CatalogValue::Raw(raw.to_owned()),
            _ => CatalogValue::Raw(raw.to_owned()),
        },
    }
}

fn oid_vector_value(value: &CatalogValue) -> Option<Vec<Oid>> {
    match value {
        CatalogValue::OidVector(values) => Some(values.clone()),
        CatalogValue::Raw(value) | CatalogValue::Text(value) | CatalogValue::Name(value) => {
            Some(parse_oid_vector(value))
        }
        _ => None,
    }
}

fn constvalue_bytes_for_default(type_oid: Oid, default: &str) -> Option<Vec<u8>> {
    match type_oid {
        BOOL_OID => Some(vec![u8::from(matches!(default, "t" | "true" | "1"))]),
        INT2_OID => default
            .parse::<i16>()
            .ok()
            .map(|value| value.to_ne_bytes().to_vec()),
        INT4_OID => default
            .parse::<i32>()
            .ok()
            .map(|value| value.to_ne_bytes().to_vec()),
        INT8_OID => default
            .parse::<i64>()
            .ok()
            .map(|value| value.to_ne_bytes().to_vec()),
        OID_OID | REGCLASS_OID => default
            .parse::<u32>()
            .ok()
            .map(|value| value.to_ne_bytes().to_vec()),
        FLOAT4_OID => default
            .parse::<f32>()
            .ok()
            .map(|value| value.to_ne_bytes().to_vec()),
        FLOAT8_OID => default
            .parse::<f64>()
            .ok()
            .map(|value| value.to_ne_bytes().to_vec()),
        TEXT_OID | VARCHAR_OID | BPCHAR_OID | NAME_OID => {
            let mut bytes = Vec::with_capacity(4 + default.len());
            let total_len = 4 + default.len();
            bytes.extend_from_slice(&((total_len as u32) << 2).to_ne_bytes());
            bytes.extend_from_slice(default.as_bytes());
            Some(bytes)
        }
        _ => None,
    }
}

fn const_node_for_default(type_oid: Oid, default: Option<&str>) -> Option<String> {
    let type_record = lookup_builtin_type(type_oid)?;
    let is_null = default.is_none();
    let constvalue = if let Some(default) = default {
        let mut bytes = constvalue_bytes_for_default(type_oid, default)?;
        if type_record.typbyval {
            bytes.resize(8, 0);
        }
        let bytes = bytes
            .iter()
            .map(|byte| (*byte as i8).to_string())
            .collect::<Vec<_>>()
            .join(" ");
        format!("{} [ {bytes} ]", type_record.typlen)
    } else {
        "<>".to_owned()
    };
    Some(format!(
        "{{CONST :consttype {} :consttypmod -1 :constcollid {} :constlen {} :constbyval {} :constisnull {} :location -1 :constvalue {}}}",
        type_oid.0,
        type_record.typcollation.0,
        type_record.typlen,
        if type_record.typbyval {
            "true"
        } else {
            "false"
        },
        if is_null { "true" } else { "false" },
        constvalue
    ))
}

fn normalize_pg_proc_bootstrap_defaults(table: &StaticCatalogTable, values: &mut [CatalogValue]) {
    if table.oid != PG_PROC_RELATION_OID {
        return;
    }
    normalize_pg_proc_system_function_body(table, values);

    let Some(pronargs_index) = table
        .columns
        .iter()
        .position(|column| column.name == "pronargs")
    else {
        return;
    };
    let Some(pronargdefaults_index) = table
        .columns
        .iter()
        .position(|column| column.name == "pronargdefaults")
    else {
        return;
    };
    let Some(proargtypes_index) = table
        .columns
        .iter()
        .position(|column| column.name == "proargtypes")
    else {
        return;
    };
    let Some(proargdefaults_index) = table
        .columns
        .iter()
        .position(|column| column.name == "proargdefaults")
    else {
        return;
    };
    let Some(defaults_text) = values
        .get(proargdefaults_index)
        .and_then(catalog_value_string)
    else {
        return;
    };
    if !defaults_text.starts_with('{') {
        return;
    }
    let Some(defaults) = parse_pg_array_elements(&defaults_text) else {
        return;
    };
    let Some(pronargs) = values.get(pronargs_index).and_then(catalog_value_i16) else {
        return;
    };
    let Some(arg_types) = values.get(proargtypes_index).and_then(oid_vector_value) else {
        return;
    };
    let pronargs = usize::from(u16::try_from(pronargs).unwrap_or(0));
    if defaults.len() > pronargs || arg_types.len() < pronargs {
        return;
    }
    values[pronargdefaults_index] =
        CatalogValue::Int16(defaults.len().min(i16::MAX as usize) as i16);

    let default_arg_types = &arg_types[pronargs - defaults.len()..pronargs];
    let Some(nodes) = default_arg_types
        .iter()
        .zip(defaults.iter())
        .map(|(type_oid, default)| const_node_for_default(*type_oid, default.as_deref()))
        .collect::<Option<Vec<_>>>()
    else {
        return;
    };
    values[proargdefaults_index] = CatalogValue::Text(format!("({})", nodes.join(" ")));
}

fn normalize_pg_proc_system_function_body(table: &StaticCatalogTable, values: &mut [CatalogValue]) {
    let Some(oid_index) = table.columns.iter().position(|column| column.name == "oid") else {
        return;
    };
    let Some(prosrc_index) = table
        .columns
        .iter()
        .position(|column| column.name == "prosrc")
    else {
        return;
    };
    if !matches!(
        values.get(prosrc_index),
        Some(CatalogValue::Text(value)) if value == "see system_functions.sql"
    ) {
        return;
    }
    let Some(CatalogValue::Oid(oid)) = values.get(oid_index) else {
        return;
    };
    let body = match oid.0 {
        879 => "select lpad($1, $2, ' ')",
        880 => "select rpad($1, $2, ' ')",
        1176 => "select cast(($1 + $2) as timestamptz)",
        1215 => {
            "select description from pg_description where objoid = $1 and classoid = (select oid from pg_class where relname = $2 and relnamespace = 'pg_catalog'::regnamespace) and objsubid = 0"
        }
        1216 => {
            "select description from pg_description where objoid = $1 and classoid = 'pg_class'::regclass and objsubid = $2"
        }
        1296 | 1298 | 1848 | 2546 | 2547 | 2548 | 2549 | 2550 | 2631 | 5023 => "select $2 + $1",
        1305 | 1309 | 2042 => "select ($1, ($1 + $2)) overlaps ($3, ($3 + $4))",
        1306 | 1310 | 2043 => "select ($1, $2) overlaps ($3, ($3 + $4))",
        1307 | 1311 | 2044 => "select ($1, ($1 + $2)) overlaps ($3, $4)",
        1345 => "select description from pg_description where objoid = $1 and objsubid = 0",
        1384 => "select date_part($1, cast($2 as timestamp))",
        1386 => "select age(cast(current_date as timestamptz), $1)",
        1426 => "select on_ppath($2, $1)",
        1708 => "select round($1, 0)",
        1710 => "select trunc($1, 0)",
        1741 | 1481 => "select log(10, $1)",
        1810 | 1811 => "select octet_length($1) * 8",
        1812 => "select length($1)",
        1993 => {
            "select description from pg_shdescription where objoid = $1 and classoid = (select oid from pg_class where relname = $2 and relnamespace = 'pg_catalog'::regnamespace)"
        }
        2059 => "select age(cast(current_date as timestamp), $1)",
        2074 => "select substring($1, similar_to_escape($2, $3))",
        2325 => "select pg_relation_size($1, 'main')",
        2932 => "select xpath($1, $2, '{}'::text[])",
        3050 => "select xpath_exists($1, $2, '{}'::text[])",
        3935 => {
            "select pg_sleep(extract(epoch from clock_timestamp() + $1) - extract(epoch from clock_timestamp()))"
        }
        3936 => "select pg_sleep(extract(epoch from $1) - extract(epoch from clock_timestamp()))",
        _ => return,
    };
    values[prosrc_index] = CatalogValue::Text(body.to_owned());
}

fn static_row_to_catalog_row_uncached(
    table: &StaticCatalogTable,
    row: &StaticCatalogRow,
) -> CatalogRow {
    let mut values = row
        .values
        .iter()
        .copied()
        .zip(table.columns.iter())
        .map(|(value, column)| static_value_to_catalog_value(column, value))
        .collect::<Vec<_>>();
    normalize_pg_proc_bootstrap_defaults(table, &mut values);
    CatalogRow {
        relation_oid: table.oid,
        row_id: row.row_id,
        values,
    }
}

pub(crate) fn static_row_to_catalog_row(
    table: &StaticCatalogTable,
    row: &StaticCatalogRow,
) -> CatalogRow {
    static_catalog_cached_row(table, row)
        .cloned()
        .unwrap_or_else(|| static_row_to_catalog_row_uncached(table, row))
}

fn synthetic_static_relation_rowtype_row(
    pg_type_table: &StaticCatalogTable,
    relation_table: &StaticCatalogTable,
) -> Option<CatalogRow> {
    let rowtype_oid = synthetic_static_relation_rowtype_oid(relation_table);
    let template = pg_type_table
        .rows
        .iter()
        .find(|row| static_row_column_identifier_matches(pg_type_table, row, "typname", "pg_proc"))
        .or_else(|| {
            pg_type_table.rows.iter().find(|row| {
                static_row_column_oid(pg_type_table, row, "typrelid")
                    .is_some_and(|typrelid| typrelid != INVALID_OID)
            })
        })?;
    let mut row = static_row_to_catalog_row(pg_type_table, template);
    row.row_id = u64::from(rowtype_oid.0);
    set_catalog_row_value(
        pg_type_table,
        &mut row,
        "oid",
        CatalogValue::Oid(rowtype_oid),
    );
    set_catalog_row_value(
        pg_type_table,
        &mut row,
        "typname",
        CatalogValue::Name(relation_table.name.to_owned()),
    );
    set_catalog_row_value(
        pg_type_table,
        &mut row,
        "typnamespace",
        CatalogValue::Oid(PG_CATALOG_NAMESPACE_OID),
    );
    set_catalog_row_value(
        pg_type_table,
        &mut row,
        "typowner",
        CatalogValue::Oid(POSTGRES_ROLE_OID),
    );
    set_catalog_row_value(
        pg_type_table,
        &mut row,
        "typrelid",
        CatalogValue::Oid(relation_table.oid),
    );
    set_catalog_row_value(
        pg_type_table,
        &mut row,
        "typelem",
        CatalogValue::Oid(INVALID_OID),
    );
    set_catalog_row_value(
        pg_type_table,
        &mut row,
        "typarray",
        CatalogValue::Oid(INVALID_OID),
    );
    Some(row)
}

fn synthetic_information_schema_namespace_row(
    pg_namespace_table: &StaticCatalogTable,
) -> Option<CatalogRow> {
    let template = pg_namespace_table.rows.first()?;
    let mut row = static_row_to_catalog_row(pg_namespace_table, template);
    row.row_id = u64::from(INFORMATION_SCHEMA_NAMESPACE.oid.0);
    set_catalog_row_value(
        pg_namespace_table,
        &mut row,
        "oid",
        CatalogValue::Oid(INFORMATION_SCHEMA_NAMESPACE.oid),
    );
    set_catalog_row_value(
        pg_namespace_table,
        &mut row,
        "nspname",
        CatalogValue::Name(INFORMATION_SCHEMA_NAMESPACE.name.to_owned()),
    );
    set_catalog_row_value(
        pg_namespace_table,
        &mut row,
        "nspowner",
        CatalogValue::Oid(INFORMATION_SCHEMA_NAMESPACE.owner),
    );
    Some(row)
}

#[derive(Clone, Copy)]
struct InformationSchemaDomainSpec {
    oid: Oid,
    name: &'static str,
    base_type: Oid,
    type_mod: i32,
    collation: Oid,
}

const INFORMATION_SCHEMA_DOMAIN_SPECS: &[InformationSchemaDomainSpec] = &[
    InformationSchemaDomainSpec {
        oid: Oid(14_001),
        name: "cardinal_number",
        base_type: INT4_OID,
        type_mod: -1,
        collation: INVALID_OID,
    },
    InformationSchemaDomainSpec {
        oid: Oid(14_002),
        name: "character_data",
        base_type: VARCHAR_OID,
        type_mod: -1,
        collation: C_COLLATION_OID,
    },
    InformationSchemaDomainSpec {
        oid: Oid(14_003),
        name: "sql_identifier",
        base_type: NAME_OID,
        type_mod: -1,
        collation: C_COLLATION_OID,
    },
    InformationSchemaDomainSpec {
        oid: Oid(14_004),
        name: "time_stamp",
        base_type: TIMESTAMPTZ_OID,
        type_mod: 2,
        collation: INVALID_OID,
    },
    InformationSchemaDomainSpec {
        oid: Oid(14_005),
        name: "yes_or_no",
        base_type: VARCHAR_OID,
        type_mod: 7,
        collation: C_COLLATION_OID,
    },
];

fn synthetic_information_schema_domain_row(
    pg_type_table: &StaticCatalogTable,
    spec: InformationSchemaDomainSpec,
) -> Option<CatalogRow> {
    let template = pg_type_table
        .rows
        .iter()
        .find(|row| static_row_column_oid(pg_type_table, row, "oid") == Some(spec.base_type))?;
    let base_type = lookup_builtin_type(spec.base_type)?;
    let mut row = static_row_to_catalog_row(pg_type_table, template);
    row.row_id = u64::from(spec.oid.0);
    set_catalog_row_value(pg_type_table, &mut row, "oid", CatalogValue::Oid(spec.oid));
    set_catalog_row_value(
        pg_type_table,
        &mut row,
        "typname",
        CatalogValue::Name(spec.name.to_owned()),
    );
    set_catalog_row_value(
        pg_type_table,
        &mut row,
        "typnamespace",
        CatalogValue::Oid(INFORMATION_SCHEMA_NAMESPACE_OID),
    );
    set_catalog_row_value(
        pg_type_table,
        &mut row,
        "typowner",
        CatalogValue::Oid(POSTGRES_ROLE_OID),
    );
    set_catalog_row_value(pg_type_table, &mut row, "typtype", CatalogValue::Char(b'd'));
    set_catalog_row_value(
        pg_type_table,
        &mut row,
        "typispreferred",
        CatalogValue::Bool(false),
    );
    set_catalog_row_value(
        pg_type_table,
        &mut row,
        "typinput",
        CatalogValue::Oid(Oid(2597)),
    );
    set_catalog_row_value(
        pg_type_table,
        &mut row,
        "typoutput",
        CatalogValue::Oid(base_type.typoutput),
    );
    set_catalog_row_value(
        pg_type_table,
        &mut row,
        "typreceive",
        CatalogValue::Oid(Oid(2598)),
    );
    set_catalog_row_value(
        pg_type_table,
        &mut row,
        "typsend",
        CatalogValue::Oid(base_type.typsend),
    );
    set_catalog_row_value(
        pg_type_table,
        &mut row,
        "typmodin",
        CatalogValue::Oid(INVALID_OID),
    );
    set_catalog_row_value(
        pg_type_table,
        &mut row,
        "typmodout",
        CatalogValue::Oid(INVALID_OID),
    );
    set_catalog_row_value(
        pg_type_table,
        &mut row,
        "typrelid",
        CatalogValue::Oid(INVALID_OID),
    );
    set_catalog_row_value(
        pg_type_table,
        &mut row,
        "typelem",
        CatalogValue::Oid(INVALID_OID),
    );
    set_catalog_row_value(
        pg_type_table,
        &mut row,
        "typarray",
        CatalogValue::Oid(INVALID_OID),
    );
    set_catalog_row_value(
        pg_type_table,
        &mut row,
        "typbasetype",
        CatalogValue::Oid(spec.base_type),
    );
    set_catalog_row_value(
        pg_type_table,
        &mut row,
        "typtypmod",
        CatalogValue::Int32(spec.type_mod),
    );
    set_catalog_row_value(
        pg_type_table,
        &mut row,
        "typcollation",
        CatalogValue::Oid(spec.collation),
    );
    set_catalog_row_value(pg_type_table, &mut row, "typdefaultbin", CatalogValue::Null);
    set_catalog_row_value(pg_type_table, &mut row, "typdefault", CatalogValue::Null);
    set_catalog_row_value(pg_type_table, &mut row, "typacl", CatalogValue::Null);
    Some(row)
}

fn empty_catalog_row(table: &StaticCatalogTable, row_id: u64) -> CatalogRow {
    CatalogRow {
        relation_oid: table.oid,
        row_id,
        values: vec![CatalogValue::Null; table.columns.len()],
    }
}

fn synthetic_pg_aggregate_fnoid_index_class_row(pg_class_table: &StaticCatalogTable) -> CatalogRow {
    let mut row = empty_catalog_row(pg_class_table, u64::from(PG_AGGREGATE_FNOID_INDEX_OID.0));
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "oid",
        CatalogValue::Oid(PG_AGGREGATE_FNOID_INDEX_OID),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "relname",
        CatalogValue::Name("pg_aggregate_fnoid_index".to_owned()),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "relnamespace",
        CatalogValue::Oid(PG_CATALOG_NAMESPACE_OID),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "reltype",
        CatalogValue::Oid(INVALID_OID),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "reloftype",
        CatalogValue::Oid(INVALID_OID),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "relowner",
        CatalogValue::Oid(POSTGRES_ROLE_OID),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "relam",
        CatalogValue::Oid(BTREE_INDEX_AM_OID),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "relfilenode",
        CatalogValue::Oid(PG_AGGREGATE_FNOID_INDEX_OID),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "reltablespace",
        CatalogValue::Oid(INVALID_OID),
    );
    set_catalog_row_value(pg_class_table, &mut row, "relpages", CatalogValue::Int32(1));
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "reltuples",
        CatalogValue::Float32(1.0),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "relallvisible",
        CatalogValue::Int32(0),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "relallfrozen",
        CatalogValue::Int32(0),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "reltoastrelid",
        CatalogValue::Oid(INVALID_OID),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "relhasindex",
        CatalogValue::Bool(false),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "relisshared",
        CatalogValue::Bool(false),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "relpersistence",
        CatalogValue::Char(b'p'),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "relkind",
        CatalogValue::Char(b'i'),
    );
    set_catalog_row_value(pg_class_table, &mut row, "relnatts", CatalogValue::Int16(1));
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "relchecks",
        CatalogValue::Int16(0),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "relhasrules",
        CatalogValue::Bool(false),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "relhastriggers",
        CatalogValue::Bool(false),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "relhassubclass",
        CatalogValue::Bool(false),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "relrowsecurity",
        CatalogValue::Bool(false),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "relforcerowsecurity",
        CatalogValue::Bool(false),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "relispopulated",
        CatalogValue::Bool(true),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "relreplident",
        CatalogValue::Char(b'n'),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "relispartition",
        CatalogValue::Bool(false),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "relrewrite",
        CatalogValue::Oid(INVALID_OID),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "relfrozenxid",
        CatalogValue::Raw("3".to_owned()),
    );
    set_catalog_row_value(
        pg_class_table,
        &mut row,
        "relminmxid",
        CatalogValue::Raw("1".to_owned()),
    );
    set_catalog_row_value(pg_class_table, &mut row, "relacl", CatalogValue::Null);
    set_catalog_row_value(pg_class_table, &mut row, "reloptions", CatalogValue::Null);
    set_catalog_row_value(pg_class_table, &mut row, "relpartbound", CatalogValue::Null);
    row
}

fn synthetic_pg_aggregate_fnoid_index_row(pg_index_table: &StaticCatalogTable) -> CatalogRow {
    let mut row = empty_catalog_row(pg_index_table, u64::from(PG_AGGREGATE_FNOID_INDEX_OID.0));
    set_catalog_row_value(
        pg_index_table,
        &mut row,
        "indexrelid",
        CatalogValue::Oid(PG_AGGREGATE_FNOID_INDEX_OID),
    );
    set_catalog_row_value(
        pg_index_table,
        &mut row,
        "indrelid",
        CatalogValue::Oid(PG_AGGREGATE_RELATION_OID),
    );
    set_catalog_row_value(pg_index_table, &mut row, "indnatts", CatalogValue::Int16(1));
    set_catalog_row_value(
        pg_index_table,
        &mut row,
        "indnkeyatts",
        CatalogValue::Int16(1),
    );
    set_catalog_row_value(
        pg_index_table,
        &mut row,
        "indisunique",
        CatalogValue::Bool(true),
    );
    set_catalog_row_value(
        pg_index_table,
        &mut row,
        "indnullsnotdistinct",
        CatalogValue::Bool(false),
    );
    set_catalog_row_value(
        pg_index_table,
        &mut row,
        "indisprimary",
        CatalogValue::Bool(true),
    );
    set_catalog_row_value(
        pg_index_table,
        &mut row,
        "indisexclusion",
        CatalogValue::Bool(false),
    );
    set_catalog_row_value(
        pg_index_table,
        &mut row,
        "indimmediate",
        CatalogValue::Bool(true),
    );
    set_catalog_row_value(
        pg_index_table,
        &mut row,
        "indisclustered",
        CatalogValue::Bool(false),
    );
    set_catalog_row_value(
        pg_index_table,
        &mut row,
        "indisvalid",
        CatalogValue::Bool(true),
    );
    set_catalog_row_value(
        pg_index_table,
        &mut row,
        "indcheckxmin",
        CatalogValue::Bool(false),
    );
    set_catalog_row_value(
        pg_index_table,
        &mut row,
        "indisready",
        CatalogValue::Bool(true),
    );
    set_catalog_row_value(
        pg_index_table,
        &mut row,
        "indislive",
        CatalogValue::Bool(true),
    );
    set_catalog_row_value(
        pg_index_table,
        &mut row,
        "indisreplident",
        CatalogValue::Bool(false),
    );
    set_catalog_row_value(
        pg_index_table,
        &mut row,
        "indkey",
        CatalogValue::Int2Vector(vec![1]),
    );
    set_catalog_row_value(
        pg_index_table,
        &mut row,
        "indcollation",
        CatalogValue::OidVector(vec![INVALID_OID]),
    );
    set_catalog_row_value(
        pg_index_table,
        &mut row,
        "indclass",
        CatalogValue::OidVector(vec![OID_BTREE_OPCLASS_OID]),
    );
    set_catalog_row_value(
        pg_index_table,
        &mut row,
        "indoption",
        CatalogValue::Int2Vector(vec![0]),
    );
    set_catalog_row_value(pg_index_table, &mut row, "indexprs", CatalogValue::Null);
    set_catalog_row_value(pg_index_table, &mut row, "indpred", CatalogValue::Null);
    row
}

fn synthetic_static_attribute_row(
    pg_attribute_table: &StaticCatalogTable,
    relation_table: &StaticCatalogTable,
    column: &StaticCatalogColumn,
    attnum: i16,
) -> Option<CatalogRow> {
    let template = pg_attribute_table.rows.first()?;
    let type_record = lookup_builtin_type(column.type_oid)?;
    let mut row = static_row_to_catalog_row(pg_attribute_table, template);
    row.row_id = (u64::from(relation_table.oid.0) << 16)
        | SYNTHETIC_STATIC_ATTRIBUTE_ROW_ID_FLAG
        | u64::from(attnum as u16);
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attrelid",
        CatalogValue::Oid(relation_table.oid),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attname",
        CatalogValue::Name(column.name.to_owned()),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "atttypid",
        CatalogValue::Oid(column.type_oid),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attstattarget",
        CatalogValue::Int16(-1),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attlen",
        CatalogValue::Int16(type_record.typlen),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attnum",
        CatalogValue::Int16(attnum),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attndims",
        CatalogValue::Int32(column.attndims),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attcacheoff",
        CatalogValue::Int32(-1),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "atttypmod",
        CatalogValue::Int32(-1),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attbyval",
        CatalogValue::Bool(type_record.typbyval),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attalign",
        CatalogValue::Char(type_record.typalign),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attstorage",
        CatalogValue::Char(type_record.typstorage),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attcompression",
        CatalogValue::Char(0),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attnotnull",
        CatalogValue::Bool(column.attnotnull),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "atthasdef",
        CatalogValue::Bool(false),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "atthasmissing",
        CatalogValue::Bool(false),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attidentity",
        CatalogValue::Char(0),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attgenerated",
        CatalogValue::Char(0),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attisdropped",
        CatalogValue::Bool(false),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attislocal",
        CatalogValue::Bool(true),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attinhcount",
        CatalogValue::Int16(0),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attcollation",
        CatalogValue::Oid(type_record.typcollation),
    );
    set_catalog_row_value(pg_attribute_table, &mut row, "attacl", CatalogValue::Null);
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attoptions",
        CatalogValue::Null,
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attfdwoptions",
        CatalogValue::Null,
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attmissingval",
        CatalogValue::Null,
    );
    Some(row)
}

#[derive(Clone, Copy)]
struct SystemAttributeSpec {
    name: &'static str,
    attnum: i16,
    type_oid: Oid,
}

const SYSTEM_ATTRIBUTE_SPECS: &[SystemAttributeSpec] = &[
    SystemAttributeSpec {
        name: "ctid",
        attnum: -1,
        type_oid: TID_OID,
    },
    SystemAttributeSpec {
        name: "xmin",
        attnum: -2,
        type_oid: XID_OID,
    },
    SystemAttributeSpec {
        name: "cmin",
        attnum: -3,
        type_oid: CID_OID,
    },
    SystemAttributeSpec {
        name: "xmax",
        attnum: -4,
        type_oid: XID_OID,
    },
    SystemAttributeSpec {
        name: "cmax",
        attnum: -5,
        type_oid: CID_OID,
    },
    SystemAttributeSpec {
        name: "tableoid",
        attnum: -6,
        type_oid: OID_OID,
    },
];

fn synthetic_system_attribute_row(
    pg_attribute_table: &StaticCatalogTable,
    relation_table: &StaticCatalogTable,
    spec: SystemAttributeSpec,
) -> Option<CatalogRow> {
    let template = pg_attribute_table.rows.first()?;
    let type_record = lookup_builtin_type(spec.type_oid)?;
    let mut row = static_row_to_catalog_row(pg_attribute_table, template);
    row.row_id = (u64::from(relation_table.oid.0) << 16) | u64::from(spec.attnum.unsigned_abs());
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attrelid",
        CatalogValue::Oid(relation_table.oid),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attname",
        CatalogValue::Name(spec.name.to_owned()),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "atttypid",
        CatalogValue::Oid(spec.type_oid),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attstattarget",
        CatalogValue::Int16(0),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attlen",
        CatalogValue::Int16(type_record.typlen),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attnum",
        CatalogValue::Int16(spec.attnum),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attndims",
        CatalogValue::Int32(0),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attcacheoff",
        CatalogValue::Int32(-1),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "atttypmod",
        CatalogValue::Int32(-1),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attbyval",
        CatalogValue::Bool(type_record.typbyval),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attalign",
        CatalogValue::Char(type_record.typalign),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attstorage",
        CatalogValue::Char(type_record.typstorage),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attcompression",
        CatalogValue::Char(0),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attnotnull",
        CatalogValue::Bool(true),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "atthasdef",
        CatalogValue::Bool(false),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "atthasmissing",
        CatalogValue::Bool(false),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attidentity",
        CatalogValue::Char(0),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attgenerated",
        CatalogValue::Char(0),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attisdropped",
        CatalogValue::Bool(false),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attislocal",
        CatalogValue::Bool(true),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attinhcount",
        CatalogValue::Int16(0),
    );
    set_catalog_row_value(
        pg_attribute_table,
        &mut row,
        "attcollation",
        CatalogValue::Oid(INVALID_OID),
    );
    Some(row)
}

fn catalog_row_attribute_key(table: &StaticCatalogTable, row: &CatalogRow) -> Option<(Oid, i16)> {
    let relation_oid = catalog_row_value(table, row, "attrelid").and_then(catalog_value_oid)?;
    let attnum = catalog_row_value(table, row, "attnum").and_then(catalog_value_i16)?;
    Some((relation_oid, attnum))
}

fn catalog_row_description_key(
    table: &StaticCatalogTable,
    row: &CatalogRow,
) -> Option<(Oid, Oid, i32)> {
    let class_oid = catalog_row_value(table, row, "classoid").and_then(catalog_value_oid)?;
    let object_oid = catalog_row_value(table, row, "objoid").and_then(catalog_value_oid)?;
    let object_subid = catalog_row_value(table, row, "objsubid").and_then(catalog_value_i32)?;
    Some((class_oid, object_oid, object_subid))
}

fn catalog_value_str(value: &CatalogValue) -> Option<&str> {
    match value {
        CatalogValue::Name(value) | CatalogValue::Text(value) | CatalogValue::Raw(value) => {
            Some(value)
        }
        _ => None,
    }
}

fn catalog_description_text_index<'a>(
    table: &'a StaticCatalogTable,
    rows: &'a BTreeMap<u64, CatalogRow>,
) -> HashMap<(Oid, Oid, i32), &'a str> {
    rows.values()
        .filter_map(|row| {
            let key = catalog_row_description_key(table, row)?;
            let description =
                catalog_row_value(table, row, "description").and_then(catalog_value_str)?;
            Some((key, description))
        })
        .collect()
}

fn synthetic_operator_proc_description_row(
    pg_description_table: &StaticCatalogTable,
    proc_oid: Oid,
    description: String,
) -> Option<CatalogRow> {
    if proc_oid == INVALID_OID {
        return None;
    }
    let template = pg_description_table.rows.first()?;
    let mut row = static_row_to_catalog_row(pg_description_table, template);
    row.row_id = SYNTHETIC_DESCRIPTION_ROW_ID_BASE | u64::from(proc_oid.0);
    set_catalog_row_value(
        pg_description_table,
        &mut row,
        "objoid",
        CatalogValue::Oid(proc_oid),
    );
    set_catalog_row_value(
        pg_description_table,
        &mut row,
        "classoid",
        CatalogValue::Oid(PG_PROC_RELATION_OID),
    );
    set_catalog_row_value(
        pg_description_table,
        &mut row,
        "objsubid",
        CatalogValue::Int32(0),
    );
    set_catalog_row_value(
        pg_description_table,
        &mut row,
        "description",
        CatalogValue::Text(description),
    );
    Some(row)
}

fn synthetic_pg_catalog_init_privs_row(table: &StaticCatalogTable) -> CatalogRow {
    let mut row = empty_catalog_row(
        table,
        SYNTHETIC_INIT_PRIVS_ROW_ID_BASE | u64::from(PG_CATALOG_NAMESPACE_OID.0),
    );
    set_catalog_row_value(
        table,
        &mut row,
        "objoid",
        CatalogValue::Oid(PG_CATALOG_NAMESPACE_OID),
    );
    set_catalog_row_value(
        table,
        &mut row,
        "classoid",
        CatalogValue::Oid(PG_NAMESPACE_RELATION_OID),
    );
    set_catalog_row_value(table, &mut row, "objsubid", CatalogValue::Int32(0));
    set_catalog_row_value(table, &mut row, "privtype", CatalogValue::Char(b'i'));
    set_catalog_row_value(
        table,
        &mut row,
        "initprivs",
        CatalogValue::Raw("{=U/postgres,postgres=UC/postgres}".to_owned()),
    );
    row
}

fn synthetic_english_ts_dict_row(table: &StaticCatalogTable) -> CatalogRow {
    let mut row = empty_catalog_row(table, u64::from(ENGLISH_TS_DICT_OID.0));
    set_catalog_row_value(
        table,
        &mut row,
        "oid",
        CatalogValue::Oid(ENGLISH_TS_DICT_OID),
    );
    set_catalog_row_value(
        table,
        &mut row,
        "dictname",
        CatalogValue::Name("english_stem".to_owned()),
    );
    set_catalog_row_value(
        table,
        &mut row,
        "dictnamespace",
        CatalogValue::Oid(PG_CATALOG_NAMESPACE_OID),
    );
    set_catalog_row_value(
        table,
        &mut row,
        "dictowner",
        CatalogValue::Oid(POSTGRES_ROLE_OID),
    );
    set_catalog_row_value(
        table,
        &mut row,
        "dicttemplate",
        CatalogValue::Oid(SNOWBALL_TS_TEMPLATE_OID),
    );
    set_catalog_row_value(
        table,
        &mut row,
        "dictinitoption",
        CatalogValue::Text("language = 'english', stopwords = 'english'".to_owned()),
    );
    row
}

fn synthetic_english_ts_config_row(table: &StaticCatalogTable) -> CatalogRow {
    let mut row = empty_catalog_row(table, u64::from(ENGLISH_TS_CONFIG_OID.0));
    set_catalog_row_value(
        table,
        &mut row,
        "oid",
        CatalogValue::Oid(ENGLISH_TS_CONFIG_OID),
    );
    set_catalog_row_value(
        table,
        &mut row,
        "cfgname",
        CatalogValue::Name("english".to_owned()),
    );
    set_catalog_row_value(
        table,
        &mut row,
        "cfgnamespace",
        CatalogValue::Oid(PG_CATALOG_NAMESPACE_OID),
    );
    set_catalog_row_value(
        table,
        &mut row,
        "cfgowner",
        CatalogValue::Oid(POSTGRES_ROLE_OID),
    );
    set_catalog_row_value(
        table,
        &mut row,
        "cfgparser",
        CatalogValue::Oid(DEFAULT_TS_PARSER_OID),
    );
    row
}

fn synthetic_english_ts_config_map_rows(table: &StaticCatalogTable) -> Vec<CatalogRow> {
    const SIMPLE_TOKEN_TYPES: &[i32] = &[3, 4, 5, 6, 7, 8, 9, 15, 18, 19, 20, 21, 22];
    const ENGLISH_TOKEN_TYPES: &[i32] = &[1, 2, 10, 11, 16, 17];

    SIMPLE_TOKEN_TYPES
        .iter()
        .map(|token_type| (*token_type, SIMPLE_TS_DICT_OID))
        .chain(
            ENGLISH_TOKEN_TYPES
                .iter()
                .map(|token_type| (*token_type, ENGLISH_TS_DICT_OID)),
        )
        .enumerate()
        .map(|(index, (token_type, dict_oid))| {
            let mut row = empty_catalog_row(
                table,
                SYNTHETIC_TS_CONFIG_MAP_ROW_ID_BASE | u64::try_from(index).unwrap_or(0),
            );
            set_catalog_row_value(
                table,
                &mut row,
                "mapcfg",
                CatalogValue::Oid(ENGLISH_TS_CONFIG_OID),
            );
            set_catalog_row_value(
                table,
                &mut row,
                "maptokentype",
                CatalogValue::Int32(token_type),
            );
            set_catalog_row_value(table, &mut row, "mapseqno", CatalogValue::Int32(1));
            set_catalog_row_value(table, &mut row, "mapdict", CatalogValue::Oid(dict_oid));
            row
        })
        .collect()
}

pub(crate) fn static_row_column_value(
    table: &StaticCatalogTable,
    row: &StaticCatalogRow,
    column_name: &str,
) -> Option<CatalogValue> {
    let column_index = table
        .columns
        .iter()
        .position(|column| column.name == column_name)?;
    let column = table.columns.get(column_index)?;
    let value = row.values.get(column_index).copied()?;
    Some(static_value_to_catalog_value(column, value))
}

pub(crate) fn static_row_column_oid(
    table: &StaticCatalogTable,
    row: &StaticCatalogRow,
    column_name: &str,
) -> Option<Oid> {
    static_row_column_value(table, row, column_name).and_then(|value| catalog_value_oid(&value))
}

pub(crate) fn static_row_column_bool(
    table: &StaticCatalogTable,
    row: &StaticCatalogRow,
    column_name: &str,
) -> Option<bool> {
    static_row_column_value(table, row, column_name).and_then(|value| catalog_value_bool(&value))
}

pub(crate) fn static_row_column_i16(
    table: &StaticCatalogTable,
    row: &StaticCatalogRow,
    column_name: &str,
) -> Option<i16> {
    static_row_column_value(table, row, column_name).and_then(|value| catalog_value_i16(&value))
}

pub(crate) fn static_row_column_identifier_matches(
    table: &StaticCatalogTable,
    row: &StaticCatalogRow,
    column_name: &str,
    normalized: &str,
) -> bool {
    let Some(column_index) = table
        .columns
        .iter()
        .position(|column| column.name == column_name)
    else {
        return false;
    };
    match row.values.get(column_index).copied() {
        Some(StaticCatalogValue::Raw(candidate)) => {
            candidate == normalized || normalize_identifier(candidate) == normalized
        }
        Some(value) => table
            .columns
            .get(column_index)
            .map(|column| static_value_to_catalog_value(column, value))
            .is_some_and(|value| catalog_value_identifier_matches(&value, normalized)),
        None => false,
    }
}

pub(crate) fn catalog_row_identifier_matches(
    table: &StaticCatalogTable,
    row: &CatalogRow,
    column_name: &str,
    normalized: &str,
) -> bool {
    catalog_row_value(table, row, column_name)
        .is_some_and(|value| catalog_value_identifier_matches(value, normalized))
}

pub(crate) fn catalog_rows_matching_static<StaticMatches, RowMatches>(
    relation_oid: Oid,
    static_matches: StaticMatches,
    row_matches: RowMatches,
) -> Vec<CatalogRow>
where
    StaticMatches: Fn(&StaticCatalogTable, &StaticCatalogRow) -> bool,
    RowMatches: Fn(&CatalogRow) -> bool,
{
    let Some(table) = static_catalog_by_relation_oid(relation_oid) else {
        return Vec::new();
    };
    with_visible_catalog_snapshot(|snapshot| {
        let mut rows = BTreeMap::<u64, CatalogRow>::new();
        for static_row in table.rows.iter().filter(|row| static_matches(table, row)) {
            if let Some(row) = static_catalog_cached_row(table, static_row) {
                rows.insert(row.row_id, row.clone());
            }
        }
        match relation_oid {
            PG_NAMESPACE_RELATION_OID => {
                insert_matching_row(
                    &mut rows,
                    synthetic_information_schema_namespace_row(table),
                    &row_matches,
                );
                for namespace in snapshot.namespaces.values() {
                    insert_matching_row(&mut rows, Some(namespace.row.clone()), &row_matches);
                }
            }
            PG_CLASS_RELATION_OID => {
                insert_matching_row(
                    &mut rows,
                    Some(synthetic_pg_aggregate_fnoid_index_class_row(table)),
                    &row_matches,
                );
                for relation in snapshot.relations.values() {
                    insert_matching_row(&mut rows, Some(relation.row.clone()), &row_matches);
                }
            }
            PG_ATTRIBUTE_RELATION_OID => {
                for column in snapshot.columns.values() {
                    insert_matching_row(&mut rows, Some(column.row.clone()), &row_matches);
                }
                let mut existing_attributes: HashSet<(Oid, i16)> = rows
                    .values()
                    .filter_map(|row| catalog_row_attribute_key(table, row))
                    .collect();
                for relation_table in static_catalogs() {
                    for (index, column) in relation_table.columns.iter().enumerate() {
                        let attnum = i16::try_from(index + 1).ok();
                        let Some(attnum) = attnum else {
                            continue;
                        };
                        if existing_attributes.contains(&(relation_table.oid, attnum)) {
                            continue;
                        }
                        if let Some(row) =
                            synthetic_static_attribute_row(table, relation_table, column, attnum)
                        {
                            existing_attributes.insert((relation_table.oid, attnum));
                            insert_matching_row(&mut rows, Some(row), &row_matches);
                        }
                    }
                    for spec in SYSTEM_ATTRIBUTE_SPECS {
                        if existing_attributes.contains(&(relation_table.oid, spec.attnum)) {
                            continue;
                        }
                        if let Some(row) =
                            synthetic_system_attribute_row(table, relation_table, *spec)
                        {
                            existing_attributes.insert((relation_table.oid, spec.attnum));
                            insert_matching_row(&mut rows, Some(row), &row_matches);
                        }
                    }
                }
            }
            PG_TYPE_RELATION_OID => {
                for pg_type in snapshot.types.values() {
                    insert_matching_row(&mut rows, Some(pg_type.row.clone()), &row_matches);
                }
                for spec in INFORMATION_SCHEMA_DOMAIN_SPECS {
                    insert_matching_row(
                        &mut rows,
                        synthetic_information_schema_domain_row(table, *spec),
                        &row_matches,
                    );
                }
                for relation_table in static_catalogs() {
                    insert_matching_row(
                        &mut rows,
                        synthetic_static_relation_rowtype_row(table, relation_table),
                        &row_matches,
                    );
                }
            }
            PG_INDEX_RELATION_OID => {
                insert_matching_row(
                    &mut rows,
                    Some(synthetic_pg_aggregate_fnoid_index_row(table)),
                    &row_matches,
                );
                for index in snapshot.indexes.values() {
                    insert_matching_row(&mut rows, Some(index.row.clone()), &row_matches);
                }
            }
            PG_CONSTRAINT_RELATION_OID => {
                for constraint in snapshot.constraints.values() {
                    insert_matching_row(&mut rows, Some(constraint.row.clone()), &row_matches);
                }
            }
            PG_DESCRIPTION_RELATION_OID => {
                let description_texts = catalog_description_text_index(table, &rows);
                let mut proc_descriptions = BTreeMap::<u32, String>::new();
                for operator in generated_catalog::STATIC_OPERATORS {
                    if operator.code == INVALID_OID {
                        continue;
                    }
                    if description_texts
                        .get(&(PG_OPERATOR_RELATION_OID, operator.oid, 0))
                        .is_some_and(|description| description.starts_with("deprecated"))
                    {
                        continue;
                    }
                    proc_descriptions
                        .entry(operator.code.0)
                        .or_insert_with(|| format!("implementation of {} operator", operator.name));
                }
                let mut existing_descriptions: HashSet<(Oid, Oid, i32)> =
                    description_texts.keys().copied().collect();
                for (proc_oid, description) in proc_descriptions {
                    let proc_oid = Oid(proc_oid);
                    if existing_descriptions.contains(&(PG_PROC_RELATION_OID, proc_oid, 0)) {
                        continue;
                    }
                    if let Some(row) =
                        synthetic_operator_proc_description_row(table, proc_oid, description)
                    {
                        existing_descriptions.insert((PG_PROC_RELATION_OID, proc_oid, 0));
                        insert_matching_row(&mut rows, Some(row), &row_matches);
                    }
                }
            }
            PG_INIT_PRIVS_RELATION_OID => {
                insert_matching_row(
                    &mut rows,
                    Some(synthetic_pg_catalog_init_privs_row(table)),
                    &row_matches,
                );
            }
            PG_TS_DICT_RELATION_OID => {
                insert_matching_row(
                    &mut rows,
                    Some(synthetic_english_ts_dict_row(table)),
                    &row_matches,
                );
            }
            PG_TS_CONFIG_RELATION_OID => {
                insert_matching_row(
                    &mut rows,
                    Some(synthetic_english_ts_config_row(table)),
                    &row_matches,
                );
            }
            PG_TS_CONFIG_MAP_RELATION_OID => {
                for row in synthetic_english_ts_config_map_rows(table) {
                    insert_matching_row(&mut rows, Some(row), &row_matches);
                }
            }
            _ => {}
        }
        snapshot.compat_rows.apply_to_rows(relation_oid, &mut rows);
        rows.into_values().filter(|row| row_matches(row)).collect()
    })
}

fn catalog_value_matches_filter(value: &CatalogValue, filter: &CatalogFilterValue) -> bool {
    match (value, filter) {
        (CatalogValue::Bool(left), CatalogFilterValue::Bool(right)) => left == right,
        (CatalogValue::Char(left), CatalogFilterValue::Char(right)) => left == right,
        (CatalogValue::Int16(left), CatalogFilterValue::Int16(right)) => left == right,
        (CatalogValue::Int32(left), CatalogFilterValue::Int32(right)) => left == right,
        (CatalogValue::Oid(left), CatalogFilterValue::Oid(right)) => left == right,
        (CatalogValue::Name(left), CatalogFilterValue::Name(right))
        | (CatalogValue::Text(left), CatalogFilterValue::Name(right))
        | (CatalogValue::Raw(left), CatalogFilterValue::Name(right)) => left == right,
        _ => false,
    }
}

fn static_row_matches_filters(
    table: &StaticCatalogTable,
    row: &StaticCatalogRow,
    filters: &[CatalogRowFilter],
) -> bool {
    filters.iter().all(|filter| {
        let Some(column_index) = filter
            .attnum
            .checked_sub(1)
            .and_then(|attnum| usize::try_from(attnum).ok())
        else {
            return false;
        };
        let Some(column) = table.columns.get(column_index) else {
            return false;
        };
        let Some(value) = row.values.get(column_index).copied() else {
            return false;
        };
        if value == StaticCatalogValue::Null {
            return false;
        }
        let value = static_value_to_catalog_value(column, value);
        catalog_value_matches_filter(&value, &filter.value)
    })
}

fn catalog_row_matches_filters(
    table: &StaticCatalogTable,
    row: &CatalogRow,
    filters: &[CatalogRowFilter],
) -> bool {
    filters.iter().all(|filter| {
        let Some(column_index) = filter
            .attnum
            .checked_sub(1)
            .and_then(|attnum| usize::try_from(attnum).ok())
        else {
            return false;
        };
        let Some(value) = row.values.get(column_index) else {
            return false;
        };
        if value == &CatalogValue::Null || column_index >= table.columns.len() {
            return false;
        }
        catalog_value_matches_filter(value, &filter.value)
    })
}

fn filter_attnum_for_column(table: &StaticCatalogTable, column_name: &str) -> Option<i16> {
    table
        .columns
        .iter()
        .position(|column| column.name == column_name)
        .and_then(|index| i16::try_from(index + 1).ok())
}

fn filter_oid(filters: &[CatalogRowFilter], attnum: i16) -> Option<Oid> {
    filters
        .iter()
        .find(|filter| filter.attnum == attnum)
        .and_then(|filter| match filter.value {
            CatalogFilterValue::Oid(oid) => Some(oid),
            _ => None,
        })
}

fn filter_i16(filters: &[CatalogRowFilter], attnum: i16) -> Option<i16> {
    filters
        .iter()
        .find(|filter| filter.attnum == attnum)
        .and_then(|filter| match filter.value {
            CatalogFilterValue::Int16(value) => Some(value),
            _ => None,
        })
}

fn filter_name(filters: &[CatalogRowFilter], attnum: i16) -> Option<String> {
    filters
        .iter()
        .find(|filter| filter.attnum == attnum)
        .and_then(|filter| match &filter.value {
            CatalogFilterValue::Name(value) => Some(value.clone()),
            _ => None,
        })
}

fn catalog_rows_matching_pg_attribute_filters(
    table: &StaticCatalogTable,
    filters: &[CatalogRowFilter],
) -> Option<Vec<CatalogRow>> {
    let relid_attnum = filter_attnum_for_column(table, "attrelid")?;
    let attnum_attnum = filter_attnum_for_column(table, "attnum")?;
    let attname_attnum = filter_attnum_for_column(table, "attname")?;
    let relation_oid = filter_oid(filters, relid_attnum)?;
    let attnum_filter = filter_i16(filters, attnum_attnum);
    let attname_filter = filter_name(filters, attname_attnum);

    with_visible_catalog_snapshot(|snapshot| {
        let row_matches = |row: &CatalogRow| catalog_row_matches_filters(table, row, filters);
        let static_matches =
            |static_row: &StaticCatalogRow| static_row_matches_filters(table, static_row, filters);
        let attnum_matches = |attnum: i16| attnum_filter.is_none_or(|wanted| wanted == attnum);
        let name_matches = |name: &str| attname_filter.as_ref().is_none_or(|wanted| wanted == name);
        let mut rows = BTreeMap::<u64, CatalogRow>::new();

        for static_row in table.rows.iter().filter(|row| static_matches(row)) {
            if let Some(row) = static_catalog_cached_row(table, static_row) {
                rows.insert(row.row_id, row.clone());
            }
        }

        if let Some(column_row_ids) = snapshot.columns_by_relation.get(&relation_oid.0) {
            for (attnum, row_id) in column_row_ids {
                if !attnum_matches(*attnum) {
                    continue;
                }
                if let Some(column) = snapshot.columns.get(row_id) {
                    insert_matching_row(&mut rows, Some(column.row.clone()), &row_matches);
                }
            }
        }

        let mut existing_attributes: HashSet<(Oid, i16)> = rows
            .values()
            .filter_map(|row| catalog_row_attribute_key(table, row))
            .collect();
        if let Some(relation_table) = static_catalog_by_relation_oid(relation_oid) {
            for (index, column) in relation_table.columns.iter().enumerate() {
                let Some(attnum) = i16::try_from(index + 1).ok() else {
                    continue;
                };
                if !attnum_matches(attnum) || !name_matches(column.name) {
                    continue;
                }
                if existing_attributes.contains(&(relation_table.oid, attnum)) {
                    continue;
                }
                if let Some(row) =
                    synthetic_static_attribute_row(table, relation_table, column, attnum)
                {
                    existing_attributes.insert((relation_table.oid, attnum));
                    insert_matching_row(&mut rows, Some(row), &row_matches);
                }
            }
            for spec in SYSTEM_ATTRIBUTE_SPECS {
                if !attnum_matches(spec.attnum) || !name_matches(spec.name) {
                    continue;
                }
                if existing_attributes.contains(&(relation_table.oid, spec.attnum)) {
                    continue;
                }
                if let Some(row) = synthetic_system_attribute_row(table, relation_table, *spec) {
                    existing_attributes.insert((relation_table.oid, spec.attnum));
                    insert_matching_row(&mut rows, Some(row), &row_matches);
                }
            }
        }

        snapshot
            .compat_rows
            .apply_to_rows(PG_ATTRIBUTE_RELATION_OID, &mut rows);
        Some(
            rows.into_values()
                .filter(|row| catalog_row_matches_filters(table, row, filters))
                .collect(),
        )
    })
}

pub fn catalog_rows_matching_filters(
    relation_oid: Oid,
    filters: &[CatalogRowFilter],
) -> Vec<CatalogRow> {
    if filters.is_empty() {
        return catalog_rows(relation_oid);
    }
    if relation_oid == PG_ATTRIBUTE_RELATION_OID
        && let Some(rows) = static_catalog_by_relation_oid(PG_ATTRIBUTE_RELATION_OID)
            .and_then(|table| catalog_rows_matching_pg_attribute_filters(table, filters))
    {
        return rows;
    }
    catalog_rows_matching_static(
        relation_oid,
        |table, row| static_row_matches_filters(table, row, filters),
        |row| {
            static_catalog_by_relation_oid(relation_oid)
                .is_some_and(|table| catalog_row_matches_filters(table, row, filters))
        },
    )
}

fn static_row_attribute_key_from_values(
    table: &StaticCatalogTable,
    row: &StaticCatalogRow,
) -> Option<(u32, i16)> {
    let relid_index = table
        .columns
        .iter()
        .position(|column| column.name == "attrelid")?;
    let attnum_index = table
        .columns
        .iter()
        .position(|column| column.name == "attnum")?;
    let relid = row
        .values
        .get(relid_index)
        .copied()
        .and_then(static_value_as_u32)?;
    let attnum = row
        .values
        .get(attnum_index)
        .copied()
        .and_then(static_value_as_i32)
        .and_then(|value| i16::try_from(value).ok())?;
    Some((relid, attnum))
}

fn synthetic_static_attribute_row_id(relation_oid: Oid, attnum: i16) -> u64 {
    (u64::from(relation_oid.0) << 16)
        | SYNTHETIC_STATIC_ATTRIBUTE_ROW_ID_FLAG
        | u64::from(attnum as u16)
}

fn synthetic_system_attribute_row_id(relation_oid: Oid, attnum: i16) -> u64 {
    (u64::from(relation_oid.0) << 16) | u64::from(attnum.unsigned_abs())
}

fn insert_compat_row_ids(
    relation_oid: Oid,
    rows: &mut BTreeSet<u64>,
    compat_rows: &crate::state::CompatCatalogRows,
) {
    if let Some(tombstones) = compat_rows.tombstones.get(&relation_oid.0) {
        for row_id in tombstones {
            rows.remove(row_id);
        }
    }
    if let Some(overlay_rows) = compat_rows.rows.get(&relation_oid.0) {
        rows.extend(overlay_rows.keys().copied());
    }
}

fn synthetic_ts_config_map_row_count() -> usize {
    const SIMPLE_TOKEN_TYPE_COUNT: usize = 13;
    const ENGLISH_TOKEN_TYPE_COUNT: usize = 6;
    SIMPLE_TOKEN_TYPE_COUNT + ENGLISH_TOKEN_TYPE_COUNT
}

pub fn catalog_row_count(relation_oid: Oid) -> Option<usize> {
    let table = static_catalog_by_relation_oid(relation_oid)?;
    Some(with_visible_catalog_snapshot(|snapshot| {
        let mut row_ids = table
            .rows
            .iter()
            .map(|row| row.row_id)
            .collect::<BTreeSet<_>>();

        match relation_oid {
            PG_NAMESPACE_RELATION_OID => {
                row_ids.insert(u64::from(INFORMATION_SCHEMA_NAMESPACE_OID.0));
                row_ids.extend(snapshot.namespaces.keys().copied());
            }
            PG_CLASS_RELATION_OID => {
                row_ids.insert(u64::from(PG_AGGREGATE_FNOID_INDEX_OID.0));
                row_ids.extend(snapshot.relations.keys().copied());
            }
            PG_ATTRIBUTE_RELATION_OID => {
                row_ids.extend(snapshot.columns.keys().copied());
                let mut existing_attributes = table
                    .rows
                    .iter()
                    .filter_map(|row| static_row_attribute_key_from_values(table, row))
                    .collect::<HashSet<_>>();
                existing_attributes.extend(
                    snapshot
                        .columns
                        .values()
                        .map(|column| (column.relation_oid.0, column.attnum)),
                );
                for relation_table in static_catalogs() {
                    for (index, _) in relation_table.columns.iter().enumerate() {
                        let Some(attnum) = i16::try_from(index + 1).ok() else {
                            continue;
                        };
                        if existing_attributes.insert((relation_table.oid.0, attnum)) {
                            row_ids.insert(synthetic_static_attribute_row_id(
                                relation_table.oid,
                                attnum,
                            ));
                        }
                    }
                    for spec in SYSTEM_ATTRIBUTE_SPECS {
                        if existing_attributes.insert((relation_table.oid.0, spec.attnum)) {
                            row_ids.insert(synthetic_system_attribute_row_id(
                                relation_table.oid,
                                spec.attnum,
                            ));
                        }
                    }
                }
            }
            PG_TYPE_RELATION_OID => {
                row_ids.extend(snapshot.types.keys().copied());
                row_ids.extend(
                    INFORMATION_SCHEMA_DOMAIN_SPECS
                        .iter()
                        .map(|spec| u64::from(spec.oid.0)),
                );
                row_ids.extend(
                    static_catalogs()
                        .iter()
                        .map(|table| u64::from(synthetic_static_relation_rowtype_oid(table).0)),
                );
            }
            PG_INDEX_RELATION_OID => {
                row_ids.insert(u64::from(PG_AGGREGATE_FNOID_INDEX_OID.0));
                row_ids.extend(snapshot.indexes.keys().copied());
            }
            PG_CONSTRAINT_RELATION_OID => {
                row_ids.extend(snapshot.constraints.keys().copied());
            }
            PG_INIT_PRIVS_RELATION_OID => {
                row_ids.insert(
                    SYNTHETIC_INIT_PRIVS_ROW_ID_BASE | u64::from(PG_CATALOG_NAMESPACE_OID.0),
                );
            }
            PG_TS_DICT_RELATION_OID => {
                row_ids.insert(u64::from(ENGLISH_TS_DICT_OID.0));
            }
            PG_TS_CONFIG_RELATION_OID => {
                row_ids.insert(u64::from(ENGLISH_TS_CONFIG_OID.0));
            }
            PG_TS_CONFIG_MAP_RELATION_OID => {
                row_ids.extend(
                    (0..synthetic_ts_config_map_row_count())
                        .map(|index| SYNTHETIC_TS_CONFIG_MAP_ROW_ID_BASE | index as u64),
                );
            }
            _ => {}
        }

        insert_compat_row_ids(relation_oid, &mut row_ids, &snapshot.compat_rows);
        row_ids.len()
    }))
}

fn insert_matching_row(
    rows: &mut BTreeMap<u64, CatalogRow>,
    row: Option<CatalogRow>,
    row_matches: &impl Fn(&CatalogRow) -> bool,
) {
    if let Some(row) = row
        && row_matches(&row)
    {
        rows.insert(row.row_id, row);
    }
}

pub fn catalog_rows(relation_oid: Oid) -> Vec<CatalogRow> {
    catalog_rows_matching_static(relation_oid, |_, _| true, |_| true)
}

pub fn relation_rowtype_oid_by_oid(relation_oid: Oid) -> Option<Oid> {
    if let Some(table) = static_catalog_by_relation_oid(relation_oid) {
        return Some(synthetic_static_relation_rowtype_oid(table));
    }
    relation_by_oid(relation_oid).map(|relation| relation.type_oid)
}

pub fn relation_planner_stats_by_oid(relation_oid: Oid) -> Option<RelationPlannerStats> {
    let row_count = catalog_row_count(relation_oid)?;
    let relpages = if row_count == 0 {
        0
    } else {
        row_count.div_ceil(32).min(i32::MAX as usize) as i32
    };
    Some(RelationPlannerStats {
        relpages,
        reltuples: row_count as f32,
    })
}

fn canonical_database_name(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        "postgres".to_owned()
    } else {
        name.to_owned()
    }
}

fn synthetic_database_oid(name: &str) -> Oid {
    match name {
        "template1" => TEMPLATE1_DATABASE_OID,
        "template0" => TEMPLATE0_DATABASE_OID,
        "postgres" => POSTGRES_DATABASE_OID,
        _ => {
            let mut hash = 0x811c_9dc5u32;
            for byte in name.as_bytes() {
                hash ^= u32::from(*byte);
                hash = hash.wrapping_mul(0x0100_0193);
            }
            Oid(SYNTHETIC_DATABASE_OID_BASE | (hash & 0x00ff_ffff))
        }
    }
}

fn synthetic_static_relation_rowtype_oid(table: &StaticCatalogTable) -> Oid {
    static_catalog_table_rowtype_oid(table)
}

fn database_oid_by_name(name: &str) -> Option<Oid> {
    let table = static_catalog_by_relation_oid(PG_DATABASE_RELATION_OID)?;
    catalog_rows(PG_DATABASE_RELATION_OID)
        .into_iter()
        .find(|row| catalog_row_identifier_matches(table, row, "datname", name))
        .and_then(|row| catalog_row_value(table, &row, "oid").and_then(catalog_value_oid))
}

fn database_row_by_oid(oid: Oid) -> Option<CatalogRow> {
    let table = static_catalog_by_relation_oid(PG_DATABASE_RELATION_OID)?;
    catalog_rows(PG_DATABASE_RELATION_OID)
        .into_iter()
        .find(|row| {
            catalog_row_value(table, row, "oid")
                .and_then(catalog_value_oid)
                .is_some_and(|row_oid| row_oid == oid)
        })
}

fn synthetic_database_row(table: &StaticCatalogTable, oid: Oid, name: &str) -> Option<CatalogRow> {
    let base = table.rows.iter().find(|row| {
        static_row_column_oid(table, row, "oid")
            .is_some_and(|row_oid| row_oid == TEMPLATE1_DATABASE_OID)
    })?;
    let mut row = static_row_to_catalog_row(table, base);
    row.row_id = u64::from(oid.0);
    set_catalog_row_value(table, &mut row, "oid", CatalogValue::Oid(oid));
    set_catalog_row_value(
        table,
        &mut row,
        "datname",
        CatalogValue::Name(name.to_owned()),
    );
    set_catalog_row_value(table, &mut row, "datistemplate", CatalogValue::Bool(false));
    set_catalog_row_value(table, &mut row, "datallowconn", CatalogValue::Bool(true));
    set_catalog_row_value(table, &mut row, "dathasloginevt", CatalogValue::Bool(false));
    set_catalog_row_value(table, &mut row, "datconnlimit", CatalogValue::Int32(-1));
    Some(row)
}

pub fn ensure_database(name: &str) -> Oid {
    let name = canonical_database_name(name);
    if let Some(oid) = database_oid_by_name(&name) {
        return oid;
    }
    let oid = synthetic_database_oid(&name);
    if database_row_by_oid(oid).is_some() {
        return oid;
    }
    let Some(table) = static_catalog_by_relation_oid(PG_DATABASE_RELATION_OID) else {
        return oid;
    };
    let Some(row) = synthetic_database_row(table, oid, &name) else {
        return oid;
    };
    let mut draft = CatalogDraft::default();
    draft.upsert_catalog_row(table, row);
    commit_catalog_draft(draft);
    oid
}

pub fn catalog_row_value<'a>(
    table: &'a StaticCatalogTable,
    row: &'a CatalogRow,
    column_name: &str,
) -> Option<&'a CatalogValue> {
    table
        .columns
        .iter()
        .position(|column| column.name == column_name)
        .and_then(|index| row.values.get(index))
}

pub(crate) fn set_catalog_row_value(
    table: &StaticCatalogTable,
    row: &mut CatalogRow,
    column_name: &str,
    value: CatalogValue,
) {
    if let Some(index) = table
        .columns
        .iter()
        .position(|column| column.name == column_name)
        && let Some(slot) = row.values.get_mut(index)
    {
        *slot = value;
    }
}

fn catalog_value_from_text(column: &StaticCatalogColumn, value: Option<&str>) -> CatalogValue {
    let Some(raw) = value else {
        return CatalogValue::Null;
    };
    match column.type_oid {
        BOOL_OID => match raw {
            "t" | "true" => CatalogValue::Bool(true),
            "f" | "false" => CatalogValue::Bool(false),
            _ => CatalogValue::Raw(raw.to_owned()),
        },
        CHAR_OID => CatalogValue::Char(match raw {
            "\\0" => 0,
            _ => raw.as_bytes().first().copied().unwrap_or(0),
        }),
        INT2_OID => raw
            .parse::<i16>()
            .map(CatalogValue::Int16)
            .unwrap_or_else(|_| CatalogValue::Raw(raw.to_owned())),
        INT4_OID => raw
            .parse::<i32>()
            .map(CatalogValue::Int32)
            .unwrap_or_else(|_| CatalogValue::Raw(raw.to_owned())),
        FLOAT4_OID => raw
            .parse::<f32>()
            .map(CatalogValue::Float32)
            .unwrap_or_else(|_| CatalogValue::Raw(raw.to_owned())),
        NAME_OID => CatalogValue::Name(raw.to_owned()),
        TEXT_OID | PG_NODE_TREE_OID => CatalogValue::Text(raw.to_owned()),
        OIDVECTOR_OID => CatalogValue::OidVector(parse_oid_vector(raw)),
        INT2VECTOR_OID => CatalogValue::Int2Vector(parse_int2_vector(raw)),
        OID_OID | REGCLASS_OID => raw
            .parse::<u32>()
            .map(|oid| CatalogValue::Oid(Oid(oid)))
            .unwrap_or_else(|_| CatalogValue::Raw(raw.to_owned())),
        _ if column.type_name.starts_with("reg") || column.type_name == "oid" => raw
            .parse::<u32>()
            .map(|oid| CatalogValue::Oid(Oid(oid)))
            .unwrap_or_else(|_| CatalogValue::Raw(raw.to_owned())),
        _ => CatalogValue::Raw(raw.to_owned()),
    }
}

fn row_id_from_catalog_values(table: &StaticCatalogTable, values: &[CatalogValue]) -> Option<u64> {
    table
        .columns
        .iter()
        .position(|column| column.name == "oid")
        .and_then(|index| values.get(index))
        .and_then(catalog_value_oid)
        .map(|oid| oid.0 as u64)
}

fn next_overlay_row_id(state: &mut CatalogState, table: &StaticCatalogTable) -> u64 {
    let first_dynamic_row_id = table
        .rows
        .iter()
        .map(|row| row.row_id)
        .max()
        .unwrap_or(0)
        .saturating_add(1)
        .max(1);
    let next = state
        .next_overlay_row_ids
        .entry(table.oid.0)
        .or_insert(first_dynamic_row_id);
    let row_id = *next;
    *next = next.saturating_add(1).max(1);
    row_id
}

pub fn upsert_catalog_row(
    relation_oid: Oid,
    row_id: u64,
    values: Vec<Option<String>>,
) -> Result<u64, CatalogError> {
    let table = static_catalog_by_relation_oid(relation_oid).ok_or_else(|| {
        CatalogError::new(
            "42P01",
            format!(
                "generated catalog relation {} does not exist",
                relation_oid.0
            ),
        )
    })?;
    if values.len() != table.columns.len() {
        return Err(CatalogError::new(
            "42804",
            format!(
                "catalog row for {} has {} values but {} columns",
                table.name,
                values.len(),
                table.columns.len()
            ),
        ));
    }
    let values = table
        .columns
        .iter()
        .zip(values.iter())
        .map(|(column, value)| catalog_value_from_text(column, value.as_deref()))
        .collect::<Vec<_>>();
    let row_id = if row_id != 0 {
        row_id
    } else if let Some(row_id) = row_id_from_catalog_values(table, &values) {
        row_id
    } else {
        with_catalog(|state| next_overlay_row_id(state, table))
    };
    let row = CatalogRow {
        relation_oid,
        row_id,
        values,
    };
    with_catalog_session(|session| {
        ensure_catalog_transaction(session);
        session
            .transaction_stack
            .last_mut()
            .expect("catalog transaction was just ensured")
            .upsert_catalog_row(table, row);
        invalidate_visible_catalog_snapshot(session);
    });
    Ok(row_id)
}

pub fn delete_catalog_row(relation_oid: Oid, row_id: u64) -> Result<(), CatalogError> {
    if static_catalog_by_relation_oid(relation_oid).is_none() {
        return Err(CatalogError::new(
            "42P01",
            format!(
                "generated catalog relation {} does not exist",
                relation_oid.0
            ),
        ));
    }
    with_catalog_session(|session| {
        ensure_catalog_transaction(session);
        session
            .transaction_stack
            .last_mut()
            .expect("catalog transaction was just ensured")
            .delete_catalog_row(relation_oid, row_id);
        invalidate_visible_catalog_snapshot(session);
    });
    Ok(())
}

pub(crate) fn catalog_value_oid(value: &CatalogValue) -> Option<Oid> {
    match value {
        CatalogValue::Oid(oid) => Some(*oid),
        CatalogValue::Int32(value) => u32::try_from(*value).ok().map(Oid),
        CatalogValue::Int16(value) => u32::try_from(*value).ok().map(Oid),
        CatalogValue::Raw(value) => resolve_generated_catalog_oid_name(value),
        _ => None,
    }
}

pub fn resolve_generated_catalog_oid_name(value: &str) -> Option<Oid> {
    if value == "-" {
        return Some(INVALID_OID);
    }
    if let Ok(oid) = value.parse::<u32>() {
        return Some(Oid(oid));
    }
    let name = normalize_identifier(value);
    generated_catalog::STATIC_PROCS
        .iter()
        .find(|record| record.name == name)
        .map(|record| record.oid)
        .or_else(|| {
            generated_catalog::STATIC_TYPES
                .iter()
                .find(|record| record.name == name)
                .map(|record| record.oid)
        })
        .or_else(|| static_catalog_by_name(&name).map(|table| table.oid))
}

pub(crate) fn catalog_value_bool(value: &CatalogValue) -> Option<bool> {
    match value {
        CatalogValue::Bool(value) => Some(*value),
        CatalogValue::Raw(value) => match value.as_str() {
            "t" | "true" => Some(true),
            "f" | "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn catalog_value_i16(value: &CatalogValue) -> Option<i16> {
    match value {
        CatalogValue::Int16(value) => Some(*value),
        CatalogValue::Int32(value) => i16::try_from(*value).ok(),
        CatalogValue::Raw(value) => value.parse::<i16>().ok(),
        _ => None,
    }
}

pub(crate) fn catalog_value_i32(value: &CatalogValue) -> Option<i32> {
    match value {
        CatalogValue::Int16(value) => Some(i32::from(*value)),
        CatalogValue::Int32(value) => Some(*value),
        CatalogValue::Raw(value) => value.parse::<i32>().ok(),
        _ => None,
    }
}

pub(crate) fn catalog_value_f32(value: &CatalogValue) -> Option<f32> {
    match value {
        CatalogValue::Float32(value) => Some(*value),
        CatalogValue::Raw(value) => value.parse::<f32>().ok(),
        _ => None,
    }
}

pub(crate) fn catalog_value_u8(value: &CatalogValue) -> Option<u8> {
    match value {
        CatalogValue::Char(value) => Some(*value),
        CatalogValue::Raw(value) => value.as_bytes().first().copied(),
        _ => None,
    }
}

pub(crate) fn catalog_value_string(value: &CatalogValue) -> Option<String> {
    match value {
        CatalogValue::Name(value) | CatalogValue::Text(value) | CatalogValue::Raw(value) => {
            Some(value.clone())
        }
        _ => None,
    }
}

pub(crate) fn catalog_value_identifier_matches(value: &CatalogValue, normalized: &str) -> bool {
    let candidate = match value {
        CatalogValue::Name(value) | CatalogValue::Text(value) | CatalogValue::Raw(value) => value,
        _ => return false,
    };
    candidate == normalized || normalize_identifier(candidate) == normalized
}
