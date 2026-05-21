use fastpg_types::Oid;

pub const BOOL_OID: Oid = Oid(16);
pub const BYTEA_OID: Oid = Oid(17);
pub const CHAR_OID: Oid = Oid(18);
pub const NAME_OID: Oid = Oid(19);
pub const INT8_OID: Oid = Oid(20);
pub const INT2_OID: Oid = Oid(21);
pub const INT2VECTOR_OID: Oid = Oid(22);
pub const INT4_OID: Oid = Oid(23);
pub const TEXT_OID: Oid = Oid(25);
pub const OID_OID: Oid = Oid(26);
pub const TID_OID: Oid = Oid(27);
pub const XID_OID: Oid = Oid(28);
pub const CID_OID: Oid = Oid(29);
pub const OIDVECTOR_OID: Oid = Oid(30);
pub const PG_NODE_TREE_OID: Oid = Oid(194);
pub const FLOAT4_OID: Oid = Oid(700);
pub const FLOAT8_OID: Oid = Oid(701);
pub const UNKNOWN_OID: Oid = Oid(705);
pub const CHAR_ARRAY_OID: Oid = Oid(1002);
pub const INT2_ARRAY_OID: Oid = Oid(1005);
pub const INT4_ARRAY_OID: Oid = Oid(1007);
pub const TEXT_ARRAY_OID: Oid = Oid(1009);
pub const OID_ARRAY_OID: Oid = Oid(1028);
pub const ACLITEM_OID: Oid = Oid(1033);
pub const ACLITEM_ARRAY_OID: Oid = Oid(1034);
pub const BPCHAR_OID: Oid = Oid(1042);
pub const VARCHAR_OID: Oid = Oid(1043);
pub const TIMESTAMP_OID: Oid = Oid(1114);
pub const TIMESTAMPTZ_OID: Oid = Oid(1184);
pub const REGCLASS_OID: Oid = Oid(2205);
pub const ANY_OID: Oid = Oid(2276);
pub const ANYARRAY_OID: Oid = Oid(2277);
pub const INTERNAL_OID: Oid = Oid(2281);
pub const RECORD_OID: Oid = Oid(2249);
pub const LSN_OID: Oid = Oid(3220);
pub const ANYENUM_OID: Oid = Oid(3500);
pub const ANYRANGE_OID: Oid = Oid(3831);
pub const ANYMULTIRANGE_OID: Oid = Oid(4537);
pub const ARRAY_SUBSCRIPT_HANDLER_OID: Oid = Oid(6179);

pub const DEFAULT_COLLATION_OID: Oid = Oid(100);
pub const C_COLLATION_OID: Oid = Oid(950);

pub(crate) const INVALID_OID: Oid = Oid(0);
pub(crate) const POSTGRES_ROLE_OID: Oid = Oid(10);
pub const PG_CATALOG_NAMESPACE_OID: Oid = Oid(11);
pub const INFORMATION_SCHEMA_NAMESPACE_OID: Oid = Oid(14_000);
pub const PUBLIC_NAMESPACE_OID: Oid = Oid(2200);
pub(crate) const PG_CLASS_RELATION_OID: Oid = Oid(1259);
pub(crate) const PG_ATTRIBUTE_RELATION_OID: Oid = Oid(1249);
pub(crate) const PG_TYPE_RELATION_OID: Oid = Oid(1247);
pub(crate) const PG_PROC_RELATION_OID: Oid = Oid(1255);
pub(crate) const PG_AGGREGATE_RELATION_OID: Oid = Oid(2600);
pub(crate) const PG_OPERATOR_RELATION_OID: Oid = Oid(2617);
pub(crate) const PG_DATABASE_RELATION_OID: Oid = Oid(1262);
pub(crate) const PG_NAMESPACE_RELATION_OID: Oid = Oid(2615);
pub(crate) const PG_INDEX_RELATION_OID: Oid = Oid(2610);
pub(crate) const PG_INHERITS_RELATION_OID: Oid = Oid(2611);
pub(crate) const PG_CONSTRAINT_RELATION_OID: Oid = Oid(2606);
pub(crate) const PG_DESCRIPTION_RELATION_OID: Oid = Oid(2609);
pub(crate) const PG_SEQUENCE_RELATION_OID: Oid = Oid(2224);
pub(crate) const PG_INIT_PRIVS_RELATION_OID: Oid = Oid(3394);
pub(crate) const PG_ENUM_RELATION_OID: Oid = Oid(3501);
pub(crate) const PG_TS_DICT_RELATION_OID: Oid = Oid(3600);
pub(crate) const PG_TS_CONFIG_RELATION_OID: Oid = Oid(3602);
pub(crate) const PG_TS_CONFIG_MAP_RELATION_OID: Oid = Oid(3603);
pub(crate) const PG_TS_TEMPLATE_RELATION_OID: Oid = Oid(3764);
pub(crate) const BTREE_INDEX_AM_OID: Oid = Oid(403);
pub(crate) const C_LANGUAGE_OID: Oid = Oid(13);
pub(crate) const PG_AGGREGATE_FNOID_INDEX_OID: Oid = Oid(2650);
pub(crate) const PG_ATTRIBUTE_RELID_NAME_INDEX_OID: Oid = Oid(2658);
pub(crate) const PG_ATTRIBUTE_RELID_NUM_INDEX_OID: Oid = Oid(2659);
pub(crate) const PG_CLASS_OID_INDEX_OID: Oid = Oid(2662);
pub(crate) const PG_CLASS_NAME_NSP_INDEX_OID: Oid = Oid(2663);
pub(crate) const PG_NAMESPACE_NAME_INDEX_OID: Oid = Oid(2684);
pub(crate) const PG_NAMESPACE_OID_INDEX_OID: Oid = Oid(2685);
pub(crate) const PG_TS_CONFIG_MAP_INDEX_OID: Oid = Oid(3609);
pub(crate) const PG_SEQUENCE_RELID_INDEX_OID: Oid = Oid(5002);
pub(crate) const OID_BTREE_OPCLASS_OID: Oid = Oid(1981);
pub(crate) const DEFAULT_TS_PARSER_OID: Oid = Oid(3722);
pub(crate) const SIMPLE_TS_DICT_OID: Oid = Oid(3765);
pub(crate) const SNOWBALL_TS_TEMPLATE_OID: Oid = Oid(100_100);
pub(crate) const ENGLISH_TS_CONFIG_OID: Oid = Oid(100_101);
pub(crate) const ENGLISH_TS_DICT_OID: Oid = Oid(100_102);
pub(crate) const DSNOWBALL_INIT_PROC_OID: Oid = Oid(100_103);
pub(crate) const DSNOWBALL_LEXIZE_PROC_OID: Oid = Oid(100_104);
pub(crate) const TEMPLATE1_DATABASE_OID: Oid = Oid(1);
pub(crate) const TEMPLATE0_DATABASE_OID: Oid = Oid(4);
pub(crate) const POSTGRES_DATABASE_OID: Oid = Oid(5);
pub(crate) const SYNTHETIC_DATABASE_OID_BASE: u32 = 0xE100_0000;
pub(crate) const SYNTHETIC_CATALOG_ROWTYPE_OID_BASE: u32 = 0xF000_0000;
pub(crate) const SYNTHETIC_DESCRIPTION_ROW_ID_BASE: u64 = 0xD200_0000;
pub(crate) const SYNTHETIC_INIT_PRIVS_ROW_ID_BASE: u64 = 0xD400_0000;
pub(crate) const SYNTHETIC_TS_CONFIG_MAP_ROW_ID_BASE: u64 = 0xD500_0000;
pub(crate) const SYNTHETIC_STATIC_ATTRIBUTE_ROW_ID_FLAG: u64 = 0x8000;

pub const VIRTUAL_CATALOG_STATIC: u8 = 1;
pub const VIRTUAL_CATALOG_DYNAMIC: u8 = 2;
pub const VIRTUAL_CATALOG_EMPTY: u8 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StaticCatalogValue {
    Null,
    Raw(&'static str),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StaticCatalogColumn {
    pub name: &'static str,
    pub type_name: &'static str,
    pub type_oid: Oid,
    pub attlen: i16,
    pub attnum: i16,
    pub attndims: i32,
    pub attbyval: bool,
    pub attalign: u8,
    pub attstorage: u8,
    pub attnotnull: bool,
    pub attcollation: Oid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StaticCatalogRow {
    pub row_id: u64,
    pub values: &'static [StaticCatalogValue],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StaticCatalogTable {
    pub oid: Oid,
    pub name: &'static str,
    pub rowtype_oid: Oid,
    pub columns: &'static [StaticCatalogColumn],
    pub rows: &'static [StaticCatalogRow],
}

#[derive(Clone, Debug, PartialEq)]
pub enum CatalogValue {
    Null,
    Bool(bool),
    Char(u8),
    Int16(i16),
    Int32(i32),
    Float32(f32),
    Oid(Oid),
    Name(String),
    Text(String),
    OidVector(Vec<Oid>),
    Int2Vector(Vec<i16>),
    Raw(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct CatalogRow {
    pub relation_oid: Oid,
    pub row_id: u64,
    pub values: Vec<CatalogValue>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VirtualCatalogPolicy {
    Static,
    Dynamic,
    Empty,
}

impl VirtualCatalogPolicy {
    pub const fn code(self) -> u8 {
        match self {
            Self::Static => VIRTUAL_CATALOG_STATIC,
            Self::Dynamic => VIRTUAL_CATALOG_DYNAMIC,
            Self::Empty => VIRTUAL_CATALOG_EMPTY,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VirtualCatalogRecord {
    pub relation_oid: Oid,
    pub name: &'static str,
    pub policy: VirtualCatalogPolicy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PgNamespaceRecord {
    pub oid: Oid,
    pub name: &'static str,
    pub owner: Oid,
}

pub(crate) const INFORMATION_SCHEMA_NAMESPACE: PgNamespaceRecord = PgNamespaceRecord {
    oid: INFORMATION_SCHEMA_NAMESPACE_OID,
    name: "information_schema",
    owner: POSTGRES_ROLE_OID,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PgOperatorRecord {
    pub oid: Oid,
    pub name: &'static str,
    pub namespace: Oid,
    pub owner: Oid,
    pub kind: u8,
    pub can_merge: bool,
    pub can_hash: bool,
    pub left_type: Oid,
    pub right_type: Oid,
    pub result_type: Oid,
    pub commutator: Oid,
    pub negator: Oid,
    pub code: Oid,
    pub rest: Oid,
    pub join: Oid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PgCastRecord {
    pub oid: Oid,
    pub source_type: Oid,
    pub target_type: Oid,
    pub function: Oid,
    pub context: u8,
    pub method: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelationName {
    pub namespace: Oid,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColumnRecord {
    pub row_id: u64,
    pub name: String,
    pub type_oid: Oid,
    pub type_mod: i32,
    pub is_not_null: bool,
    pub has_default: bool,
    pub generated: u8,
}

impl ColumnRecord {
    pub fn new(name: impl Into<String>, type_oid: Oid, type_mod: i32, is_not_null: bool) -> Self {
        Self {
            row_id: 0,
            name: normalize_identifier(&name.into()),
            type_oid,
            type_mod,
            is_not_null,
            has_default: false,
            generated: 0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhysicalColumnRecord {
    pub row_id: u64,
    pub name: String,
    pub type_oid: Oid,
    pub type_mod: i32,
    pub is_not_null: bool,
    pub has_default: bool,
    pub generated: u8,
    pub is_dropped: bool,
    pub attlen: i16,
    pub attbyval: bool,
    pub attalign: u8,
    pub attstorage: u8,
    pub attcollation: Oid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelationRecord {
    pub row_id: u64,
    pub oid: Oid,
    pub type_oid: Oid,
    pub namespace: Oid,
    pub owner: Oid,
    pub name: String,
    pub relkind: u8,
    pub columns: Vec<ColumnRecord>,
    pub primary_key: Vec<String>,
    pub primary_key_constraint_oid: Option<Oid>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelationSummaryRecord {
    pub row_id: u64,
    pub oid: Oid,
    pub type_oid: Oid,
    pub namespace: Oid,
    pub owner: Oid,
    pub name: String,
    pub relkind: u8,
    pub column_count: u16,
    pub has_primary_key: bool,
    pub has_indexes: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RelationPlannerStats {
    pub relpages: i32,
    pub reltuples: f32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexRecord {
    pub row_id: u64,
    pub index_oid: Oid,
    pub relation_oid: Oid,
    pub key_attnums: Vec<i16>,
    pub is_unique: bool,
    pub nulls_not_distinct: bool,
    pub is_primary: bool,
    pub is_exclusion: bool,
    pub is_immediate: bool,
    pub is_valid: bool,
    pub is_ready: bool,
    pub is_live: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogError {
    pub sqlstate: String,
    pub message: String,
}

impl CatalogError {
    pub fn new(sqlstate: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            sqlstate: sqlstate.into(),
            message: message.into(),
        }
    }
}

pub(crate) fn normalize_identifier(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PgTypeRecord {
    pub oid: Oid,
    pub name: &'static str,
    pub namespace: Oid,
    pub owner: Oid,
    pub typlen: i16,
    pub typbyval: bool,
    pub typalign: u8,
    pub typdelim: u8,
    pub typinput: Oid,
    pub typoutput: Oid,
    pub typreceive: Oid,
    pub typsend: Oid,
    pub typmodin: Oid,
    pub typmodout: Oid,
    pub typisdefined: bool,
    pub typtype: u8,
    pub typcategory: u8,
    pub typispreferred: bool,
    pub typrelid: Oid,
    pub typelem: Oid,
    pub typarray: Oid,
    pub typbasetype: Oid,
    pub typtypmod: i32,
    pub typcollation: Oid,
    pub typsubscript: Oid,
    pub typstorage: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogTypeRecord {
    pub row_id: u64,
    pub oid: Oid,
    pub name: String,
    pub namespace: Oid,
    pub owner: Oid,
    pub typlen: i16,
    pub typbyval: bool,
    pub typalign: u8,
    pub typdelim: u8,
    pub typinput: Oid,
    pub typoutput: Oid,
    pub typreceive: Oid,
    pub typsend: Oid,
    pub typmodin: Oid,
    pub typmodout: Oid,
    pub typisdefined: bool,
    pub typtype: u8,
    pub typcategory: u8,
    pub typispreferred: bool,
    pub typrelid: Oid,
    pub typelem: Oid,
    pub typarray: Oid,
    pub typbasetype: Oid,
    pub typtypmod: i32,
    pub typcollation: Oid,
    pub typsubscript: Oid,
    pub typstorage: u8,
}

impl From<PgTypeRecord> for CatalogTypeRecord {
    fn from(record: PgTypeRecord) -> Self {
        Self {
            row_id: 0,
            oid: record.oid,
            name: record.name.to_owned(),
            namespace: record.namespace,
            owner: record.owner,
            typlen: record.typlen,
            typbyval: record.typbyval,
            typalign: record.typalign,
            typdelim: record.typdelim,
            typinput: record.typinput,
            typoutput: record.typoutput,
            typreceive: record.typreceive,
            typsend: record.typsend,
            typmodin: record.typmodin,
            typmodout: record.typmodout,
            typisdefined: record.typisdefined,
            typtype: record.typtype,
            typcategory: record.typcategory,
            typispreferred: record.typispreferred,
            typrelid: record.typrelid,
            typelem: record.typelem,
            typarray: record.typarray,
            typbasetype: record.typbasetype,
            typtypmod: record.typtypmod,
            typcollation: record.typcollation,
            typsubscript: record.typsubscript,
            typstorage: record.typstorage,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PgProcRecord {
    pub oid: Oid,
    pub name: &'static str,
    pub namespace: Oid,
    pub owner: Oid,
    pub language: Oid,
    pub cost: u32,
    pub rows: u32,
    pub variadic: Oid,
    pub support: Oid,
    pub kind: u8,
    pub security_definer: bool,
    pub leakproof: bool,
    pub strict: bool,
    pub returns_set: bool,
    pub volatility: u8,
    pub parallel: u8,
    pub return_type: Oid,
    pub arg_types: &'static [Oid],
    pub arg_defaults: u16,
    pub source: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PgAggregateRecord {
    pub function_oid: Oid,
    pub kind: u8,
    pub direct_arg_count: u16,
    pub transition_fn: Oid,
    pub final_fn: Oid,
    pub combine_fn: Oid,
    pub serial_fn: Oid,
    pub deserial_fn: Oid,
    pub moving_transition_fn: Oid,
    pub moving_inverse_fn: Oid,
    pub moving_final_fn: Oid,
    pub final_extra: bool,
    pub moving_final_extra: bool,
    pub final_modify: u8,
    pub moving_final_modify: u8,
    pub sort_operator: Oid,
    pub transition_type: Oid,
    pub transition_space: i32,
    pub moving_transition_type: Oid,
    pub moving_transition_space: i32,
    pub init_value: Option<&'static str>,
    pub moving_init_value: Option<&'static str>,
}
