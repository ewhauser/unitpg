#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use fastpg_types::Oid;

pub const BOOL_OID: Oid = Oid(16);
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
pub const FLOAT8_OID: Oid = Oid(701);
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

pub const DEFAULT_COLLATION_OID: Oid = Oid(100);
pub const C_COLLATION_OID: Oid = Oid(950);

const F_CHAROUT: Oid = Oid(33);
const F_NAMEIN: Oid = Oid(34);
const F_NAMEOUT: Oid = Oid(35);
const F_INT2IN: Oid = Oid(38);
const F_INT2OUT: Oid = Oid(39);
const F_INT2VECTORIN: Oid = Oid(40);
const F_INT2VECTOROUT: Oid = Oid(41);
const F_INT4IN: Oid = Oid(42);
const F_INT4OUT: Oid = Oid(43);
const F_TEXTIN: Oid = Oid(46);
const F_TEXTOUT: Oid = Oid(47);
const F_OIDVECTORIN: Oid = Oid(54);
const F_OIDVECTOROUT: Oid = Oid(55);
const F_PG_NODE_TREE_IN: Oid = Oid(195);
const F_PG_NODE_TREE_OUT: Oid = Oid(196);
const F_PG_NODE_TREE_RECV: Oid = Oid(197);
const F_PG_NODE_TREE_SEND: Oid = Oid(198);
const F_INT8IN: Oid = Oid(460);
const F_INT8OUT: Oid = Oid(461);
const F_FLOAT8IN: Oid = Oid(214);
const F_FLOAT8OUT: Oid = Oid(215);
const F_ARRAY_IN: Oid = Oid(750);
const F_ARRAY_OUT: Oid = Oid(751);
const F_ACLITEMIN: Oid = Oid(1031);
const F_ACLITEMOUT: Oid = Oid(1032);
const F_BPCHARIN: Oid = Oid(1044);
const F_BPCHAROUT: Oid = Oid(1045);
const F_VARCHARIN: Oid = Oid(1046);
const F_VARCHAROUT: Oid = Oid(1047);
const F_BOOLIN: Oid = Oid(1242);
const F_BOOLOUT: Oid = Oid(1243);
const F_CHARIN: Oid = Oid(1245);
const F_OIDIN: Oid = Oid(1798);
const F_OIDOUT: Oid = Oid(1799);
const F_TIDIN: Oid = Oid(48);
const F_TIDOUT: Oid = Oid(49);
const F_XIDIN: Oid = Oid(50);
const F_XIDOUT: Oid = Oid(51);
const F_CIDIN: Oid = Oid(52);
const F_CIDOUT: Oid = Oid(53);
const F_OIDEQ: Oid = Oid(184);
const F_INT4EQ: Oid = Oid(65);
const F_INT4LT: Oid = Oid(66);
const F_INT4GT: Oid = Oid(147);
const F_BTINT4CMP: Oid = Oid(351);
const F_EQSEL: Oid = Oid(101);
const F_EQJOINSEL: Oid = Oid(105);
const F_INT4PL: Oid = Oid(177);
const F_INT2RECV: Oid = Oid(2404);
const F_INT2SEND: Oid = Oid(2405);
const F_INT4RECV: Oid = Oid(2406);
const F_INT4SEND: Oid = Oid(2407);
const F_INT8RECV: Oid = Oid(2408);
const F_INT8SEND: Oid = Oid(2409);
const F_TEXTRECV: Oid = Oid(2414);
const F_TEXTSEND: Oid = Oid(2415);
const F_OIDRECV: Oid = Oid(2418);
const F_OIDSEND: Oid = Oid(2419);
const F_NAMERECV: Oid = Oid(2422);
const F_NAMESEND: Oid = Oid(2423);
const F_REGCLASSIN: Oid = Oid(2218);
const F_REGCLASSOUT: Oid = Oid(2219);
const F_ANYARRAYIN: Oid = Oid(2296);
const F_ANYARRAYOUT: Oid = Oid(2297);
const F_ARRAY_RECV: Oid = Oid(2400);
const F_ARRAY_SEND: Oid = Oid(2401);
const F_REGCLASSRECV: Oid = Oid(2452);
const F_REGCLASSSEND: Oid = Oid(2453);
const F_TIDRECV: Oid = Oid(2438);
const F_TIDSEND: Oid = Oid(2439);
const F_XIDRECV: Oid = Oid(2440);
const F_XIDSEND: Oid = Oid(2441);
const F_CIDRECV: Oid = Oid(2442);
const F_CIDSEND: Oid = Oid(2443);
const F_INT2VECTORRECV: Oid = Oid(2410);
const F_INT2VECTORSEND: Oid = Oid(2411);
const F_OIDVECTORRECV: Oid = Oid(2420);
const F_OIDVECTORSEND: Oid = Oid(2421);
const F_FLOAT8RECV: Oid = Oid(2426);
const F_FLOAT8SEND: Oid = Oid(2427);
const F_BPCHARRECV: Oid = Oid(2430);
const F_BPCHARSEND: Oid = Oid(2431);
const F_VARCHARRECV: Oid = Oid(2432);
const F_VARCHARSEND: Oid = Oid(2433);
const F_BOOLRECV: Oid = Oid(2436);
const F_BOOLSEND: Oid = Oid(2437);
const F_TIMESTAMPRECV: Oid = Oid(2474);
const F_TIMESTAMPSEND: Oid = Oid(2475);
const F_TIMESTAMPTZIN: Oid = Oid(1150);
const F_TIMESTAMPTZOUT: Oid = Oid(1151);
const F_TIMESTAMPTZRECV: Oid = Oid(2476);
const F_TIMESTAMPTZSEND: Oid = Oid(2477);
const F_BPCHARTYPMODIN: Oid = Oid(2913);
const F_BPCHARTYPMODOUT: Oid = Oid(2914);
const F_VARCHARTYPMODIN: Oid = Oid(2915);
const F_VARCHARTYPMODOUT: Oid = Oid(2916);
const F_TIMESTAMPIN: Oid = Oid(1312);
const F_TIMESTAMPOUT: Oid = Oid(1313);
const F_TIMESTAMPTZ_TIMESTAMP: Oid = Oid(2027);
const F_TIMESTAMP_TIMESTAMPTZ: Oid = Oid(2028);
const F_TIMESTAMPTYPMODIN: Oid = Oid(2905);
const F_TIMESTAMPTYPMODOUT: Oid = Oid(2906);
const F_TIMESTAMPTZTYPMODIN: Oid = Oid(2907);
const F_TIMESTAMPTZTYPMODOUT: Oid = Oid(2908);
const F_ANYARRAYRECV: Oid = Oid(2502);
const F_ANYARRAYSEND: Oid = Oid(2503);
const F_ARRAY_SUBSCRIPT_HANDLER: Oid = Oid(6179);
const F_INT8PL: Oid = Oid(463);
const F_INT8EQ: Oid = Oid(467);
const F_INT8INC: Oid = Oid(1219);
const F_INT8DEC: Oid = Oid(3546);
const F_COUNT_ANY: Oid = Oid(2147);
const F_COUNT: Oid = Oid(2803);
const F_INT8INC_ANY: Oid = Oid(2804);
const F_INT8DEC_ANY: Oid = Oid(3547);
const F_INT8INC_SUPPORT: Oid = Oid(6236);

const O_INT4EQ: Oid = Oid(96);
const O_INT4LT: Oid = Oid(97);
const O_OIDEQ: Oid = Oid(607);
const O_INT8EQ: Oid = Oid(410);
const O_INT4GT: Oid = Oid(521);
const O_INT4PL: Oid = Oid(551);
const O_INT8PL: Oid = Oid(684);
const O_OID_REGCLASS_EQ: Oid = Oid(10184);
const O_REGCLASS_OID_EQ: Oid = Oid(10185);

const C_TIMESTAMP_TIMESTAMPTZ: Oid = Oid(10178);
const C_TIMESTAMPTZ_TIMESTAMP: Oid = Oid(10181);
const C_OID_REGCLASS: Oid = Oid(10182);
const C_REGCLASS_OID: Oid = Oid(10183);

const INVALID_OID: Oid = Oid(0);
const BOOTSTRAP_SUPERUSER_OID: Oid = Oid(10);
const INTERNAL_LANGUAGE_OID: Oid = Oid(12);

const TYPALIGN_CHAR: u8 = b'c';
const TYPALIGN_SHORT: u8 = b's';
const TYPALIGN_INT: u8 = b'i';
const TYPALIGN_DOUBLE: u8 = b'd';

const TYPDELIM_COMMA: u8 = b',';
const TYPSTORAGE_PLAIN: u8 = b'p';
const TYPSTORAGE_EXTENDED: u8 = b'x';
const TYPTYPE_BASE: u8 = b'b';
const TYPTYPE_PSEUDO: u8 = b'p';

const TYPCATEGORY_ARRAY: u8 = b'A';
const TYPCATEGORY_BOOLEAN: u8 = b'B';
const TYPCATEGORY_DATETIME: u8 = b'D';
const TYPCATEGORY_NUMERIC: u8 = b'N';
const TYPCATEGORY_PSEUDO: u8 = b'P';
const TYPCATEGORY_STRING: u8 = b'S';
const TYPCATEGORY_INTERNAL: u8 = b'Z';
const TYPCATEGORY_USER_DEFINED: u8 = b'U';

const PROKIND_FUNCTION: u8 = b'f';
const PROKIND_AGGREGATE: u8 = b'a';
const PROVOLATILE_IMMUTABLE: u8 = b'i';
const PROVOLATILE_STABLE: u8 = b's';
const PROPARALLEL_SAFE: u8 = b's';
const AGGKIND_NORMAL: u8 = b'n';
const AGGMODIFY_READ_ONLY: u8 = b'r';

const COERCION_CODE_ASSIGNMENT: u8 = b'a';
const COERCION_CODE_IMPLICIT: u8 = b'i';
const COERCION_METHOD_BINARY: u8 = b'b';
const COERCION_METHOD_FUNCTION: u8 = b'f';

pub const PG_CATALOG_NAMESPACE_OID: Oid = Oid(11);
pub const PUBLIC_NAMESPACE_OID: Oid = Oid(2200);
const FIRST_DYNAMIC_RELATION_OID: u32 = 16_384;

pub const VIRTUAL_CATALOG_STATIC: u8 = 1;
pub const VIRTUAL_CATALOG_DYNAMIC: u8 = 2;
pub const VIRTUAL_CATALOG_EMPTY: u8 = 3;

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

const VIRTUAL_CATALOGS: &[VirtualCatalogRecord] = &[
    VirtualCatalogRecord {
        relation_oid: Oid(1247),
        name: "pg_type",
        policy: VirtualCatalogPolicy::Static,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(1255),
        name: "pg_proc",
        policy: VirtualCatalogPolicy::Static,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(2600),
        name: "pg_aggregate",
        policy: VirtualCatalogPolicy::Static,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(2615),
        name: "pg_namespace",
        policy: VirtualCatalogPolicy::Static,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(2617),
        name: "pg_operator",
        policy: VirtualCatalogPolicy::Static,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(1259),
        name: "pg_class",
        policy: VirtualCatalogPolicy::Dynamic,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(1249),
        name: "pg_attribute",
        policy: VirtualCatalogPolicy::Dynamic,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(2610),
        name: "pg_index",
        policy: VirtualCatalogPolicy::Dynamic,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(2606),
        name: "pg_constraint",
        policy: VirtualCatalogPolicy::Dynamic,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(1260),
        name: "pg_authid",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(1261),
        name: "pg_auth_members",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(2619),
        name: "pg_statistic",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(3381),
        name: "pg_statistic_ext",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(3429),
        name: "pg_statistic_ext_data",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(2605),
        name: "pg_cast",
        policy: VirtualCatalogPolicy::Static,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(3456),
        name: "pg_collation",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(2601),
        name: "pg_am",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(2602),
        name: "pg_amop",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(2603),
        name: "pg_amproc",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(2616),
        name: "pg_opclass",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(2753),
        name: "pg_opfamily",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(2611),
        name: "pg_inherits",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(2618),
        name: "pg_rewrite",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(3256),
        name: "pg_policy",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(2224),
        name: "pg_sequence",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(3350),
        name: "pg_partitioned_table",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(3541),
        name: "pg_range",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(2612),
        name: "pg_language",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(6243),
        name: "pg_parameter_acl",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(1213),
        name: "pg_tablespace",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(1262),
        name: "pg_database",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(826),
        name: "pg_default_acl",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(1417),
        name: "pg_foreign_server",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(1418),
        name: "pg_user_mapping",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(2328),
        name: "pg_foreign_data_wrapper",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(2607),
        name: "pg_conversion",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(3079),
        name: "pg_extension",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(3118),
        name: "pg_foreign_table",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(3466),
        name: "pg_event_trigger",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(3501),
        name: "pg_enum",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(3576),
        name: "pg_transform",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(3592),
        name: "pg_shseclabel",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(3596),
        name: "pg_seclabel",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(3600),
        name: "pg_ts_dict",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(3601),
        name: "pg_ts_parser",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(3602),
        name: "pg_ts_config",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(3603),
        name: "pg_ts_config_map",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(3764),
        name: "pg_ts_template",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(6000),
        name: "pg_replication_origin",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(6100),
        name: "pg_subscription",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(6102),
        name: "pg_subscription_rel",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(6104),
        name: "pg_publication",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(6106),
        name: "pg_publication_rel",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(6237),
        name: "pg_publication_namespace",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(6466),
        name: "pg_propgraph_element",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(6470),
        name: "pg_propgraph_label",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(6472),
        name: "pg_propgraph_element_label",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(6473),
        name: "pg_propgraph_property",
        policy: VirtualCatalogPolicy::Empty,
    },
    VirtualCatalogRecord {
        relation_oid: Oid(6482),
        name: "pg_propgraph_label_property",
        policy: VirtualCatalogPolicy::Empty,
    },
];

pub fn virtual_catalogs() -> &'static [VirtualCatalogRecord] {
    VIRTUAL_CATALOGS
}

pub fn virtual_catalog_by_relation_oid(relation_oid: Oid) -> Option<VirtualCatalogRecord> {
    VIRTUAL_CATALOGS
        .iter()
        .copied()
        .find(|record| record.relation_oid == relation_oid)
}

pub fn virtual_catalog_by_name(name: &str, namespace: Oid) -> Option<VirtualCatalogRecord> {
    if namespace != PG_CATALOG_NAMESPACE_OID {
        return None;
    }
    let name = normalize_identifier(name);
    VIRTUAL_CATALOGS
        .iter()
        .copied()
        .find(|record| record.name == name.as_str())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PgNamespaceRecord {
    pub oid: Oid,
    pub name: &'static str,
    pub owner: Oid,
}

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

impl PgOperatorRecord {
    const fn binary(
        oid: Oid,
        name: &'static str,
        left_type: Oid,
        right_type: Oid,
        result_type: Oid,
        code: Oid,
    ) -> Self {
        Self {
            oid,
            name,
            namespace: PG_CATALOG_NAMESPACE_OID,
            owner: BOOTSTRAP_SUPERUSER_OID,
            kind: b'b',
            can_merge: false,
            can_hash: false,
            left_type,
            right_type,
            result_type,
            commutator: oid,
            negator: INVALID_OID,
            code,
            rest: INVALID_OID,
            join: INVALID_OID,
        }
    }

    const fn equality(oid: Oid, left_type: Oid, right_type: Oid, code: Oid) -> Self {
        Self {
            oid,
            name: "=",
            namespace: PG_CATALOG_NAMESPACE_OID,
            owner: BOOTSTRAP_SUPERUSER_OID,
            kind: b'b',
            can_merge: false,
            can_hash: false,
            left_type,
            right_type,
            result_type: BOOL_OID,
            commutator: oid,
            negator: INVALID_OID,
            code,
            rest: F_EQSEL,
            join: F_EQJOINSEL,
        }
    }
}

const BUILTIN_OPERATORS: &[PgOperatorRecord] = &[
    PgOperatorRecord::equality(O_INT4EQ, INT4_OID, INT4_OID, F_INT4EQ),
    PgOperatorRecord::binary(O_INT4LT, "<", INT4_OID, INT4_OID, BOOL_OID, F_INT4LT),
    PgOperatorRecord::binary(O_INT4GT, ">", INT4_OID, INT4_OID, BOOL_OID, F_INT4GT),
    PgOperatorRecord::equality(O_OIDEQ, OID_OID, OID_OID, F_OIDEQ),
    PgOperatorRecord::equality(O_INT8EQ, INT8_OID, INT8_OID, F_INT8EQ),
    PgOperatorRecord::equality(O_OID_REGCLASS_EQ, OID_OID, REGCLASS_OID, F_OIDEQ),
    PgOperatorRecord::equality(O_REGCLASS_OID_EQ, REGCLASS_OID, OID_OID, F_OIDEQ),
    PgOperatorRecord::binary(O_INT4PL, "+", INT4_OID, INT4_OID, INT4_OID, F_INT4PL),
    PgOperatorRecord::binary(O_INT8PL, "+", INT8_OID, INT8_OID, INT8_OID, F_INT8PL),
];

pub fn builtin_operator_by_oid(oid: Oid) -> Option<&'static PgOperatorRecord> {
    BUILTIN_OPERATORS.iter().find(|record| record.oid == oid)
}

pub fn builtin_operator_by_signature(
    name: &str,
    left_type: Oid,
    right_type: Oid,
    namespace: Oid,
) -> Option<&'static PgOperatorRecord> {
    let name = normalize_identifier(name);
    BUILTIN_OPERATORS.iter().find(|record| {
        record.name == name.as_str()
            && record.left_type == left_type
            && record.right_type == right_type
            && record.namespace == namespace
    })
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

const BUILTIN_CASTS: &[PgCastRecord] = &[
    PgCastRecord {
        oid: C_TIMESTAMP_TIMESTAMPTZ,
        source_type: TIMESTAMP_OID,
        target_type: TIMESTAMPTZ_OID,
        function: F_TIMESTAMP_TIMESTAMPTZ,
        context: COERCION_CODE_IMPLICIT,
        method: COERCION_METHOD_FUNCTION,
    },
    PgCastRecord {
        oid: C_TIMESTAMPTZ_TIMESTAMP,
        source_type: TIMESTAMPTZ_OID,
        target_type: TIMESTAMP_OID,
        function: F_TIMESTAMPTZ_TIMESTAMP,
        context: COERCION_CODE_ASSIGNMENT,
        method: COERCION_METHOD_FUNCTION,
    },
    PgCastRecord {
        oid: C_OID_REGCLASS,
        source_type: OID_OID,
        target_type: REGCLASS_OID,
        function: INVALID_OID,
        context: COERCION_CODE_IMPLICIT,
        method: COERCION_METHOD_BINARY,
    },
    PgCastRecord {
        oid: C_REGCLASS_OID,
        source_type: REGCLASS_OID,
        target_type: OID_OID,
        function: INVALID_OID,
        context: COERCION_CODE_IMPLICIT,
        method: COERCION_METHOD_BINARY,
    },
];

pub fn builtin_cast_by_source_target(
    source_type: Oid,
    target_type: Oid,
) -> Option<&'static PgCastRecord> {
    BUILTIN_CASTS
        .iter()
        .find(|record| record.source_type == source_type && record.target_type == target_type)
}

const BUILTIN_NAMESPACES: &[PgNamespaceRecord] = &[
    PgNamespaceRecord {
        oid: PG_CATALOG_NAMESPACE_OID,
        name: "pg_catalog",
        owner: BOOTSTRAP_SUPERUSER_OID,
    },
    PgNamespaceRecord {
        oid: PUBLIC_NAMESPACE_OID,
        name: "public",
        owner: BOOTSTRAP_SUPERUSER_OID,
    },
];

pub fn builtin_namespace_by_oid(oid: Oid) -> Option<&'static PgNamespaceRecord> {
    BUILTIN_NAMESPACES.iter().find(|record| record.oid == oid)
}

pub fn builtin_namespace_by_name(name: &str) -> Option<&'static PgNamespaceRecord> {
    let name = normalize_identifier(name);
    BUILTIN_NAMESPACES
        .iter()
        .find(|record| record.name == name.as_str())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelationName {
    pub namespace: Oid,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColumnRecord {
    pub name: String,
    pub type_oid: Oid,
    pub type_mod: i32,
    pub is_not_null: bool,
}

impl ColumnRecord {
    pub fn new(name: impl Into<String>, type_oid: Oid, type_mod: i32, is_not_null: bool) -> Self {
        Self {
            name: normalize_identifier(&name.into()),
            type_oid,
            type_mod,
            is_not_null,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelationRecord {
    pub oid: Oid,
    pub namespace: Oid,
    pub name: String,
    pub columns: Vec<ColumnRecord>,
    pub primary_key: Vec<String>,
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

#[derive(Debug)]
struct CatalogState {
    next_relation_oid: u32,
    relations_by_name: BTreeMap<String, RelationRecord>,
    relation_names_by_oid: BTreeMap<u32, String>,
}

impl Default for CatalogState {
    fn default() -> Self {
        Self {
            next_relation_oid: FIRST_DYNAMIC_RELATION_OID,
            relations_by_name: BTreeMap::new(),
            relation_names_by_oid: BTreeMap::new(),
        }
    }
}

static CATALOG: OnceLock<Mutex<CatalogState>> = OnceLock::new();

fn catalog() -> &'static Mutex<CatalogState> {
    CATALOG.get_or_init(|| Mutex::new(CatalogState::default()))
}

fn with_catalog<R>(f: impl FnOnce(&mut CatalogState) -> R) -> R {
    match catalog().lock() {
        Ok(mut state) => f(&mut state),
        Err(poisoned) => {
            let mut state = poisoned.into_inner();
            f(&mut state)
        }
    }
}

fn normalize_identifier(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn next_relation_oid(state: &mut CatalogState) -> Result<Oid, CatalogError> {
    let oid = state.next_relation_oid;
    if oid == 0 {
        return Err(CatalogError::new(
            "54000",
            "fastpg relation OID space is exhausted",
        ));
    }
    state.next_relation_oid = state
        .next_relation_oid
        .checked_add(1)
        .ok_or_else(|| CatalogError::new("54000", "fastpg relation OID space is exhausted"))?;
    Ok(Oid(oid))
}

pub fn create_relation(
    name: &str,
    columns: Vec<ColumnRecord>,
    if_not_exists: bool,
) -> Result<Option<RelationRecord>, CatalogError> {
    let name = normalize_identifier(name);
    if name.is_empty() {
        return Err(CatalogError::new("42602", "relation name cannot be empty"));
    }
    if columns.iter().any(|column| column.name.is_empty()) {
        return Err(CatalogError::new("42602", "column name cannot be empty"));
    }

    with_catalog(|state| {
        if state.relations_by_name.contains_key(&name) {
            if if_not_exists {
                return Ok(None);
            }
            return Err(CatalogError::new(
                "42P07",
                format!("relation \"{name}\" already exists"),
            ));
        }

        let relation = RelationRecord {
            oid: next_relation_oid(state)?,
            namespace: PUBLIC_NAMESPACE_OID,
            name: name.clone(),
            columns,
            primary_key: Vec::new(),
        };
        state
            .relation_names_by_oid
            .insert(relation.oid.0, name.clone());
        state.relations_by_name.insert(name, relation.clone());
        Ok(Some(relation))
    })
}

pub fn drop_relation(name: &str, missing_ok: bool) -> Result<Option<RelationRecord>, CatalogError> {
    let name = normalize_identifier(name);
    with_catalog(|state| match state.relations_by_name.remove(&name) {
        Some(relation) => {
            state.relation_names_by_oid.remove(&relation.oid.0);
            Ok(Some(relation))
        }
        None if missing_ok => Ok(None),
        None => Err(CatalogError::new(
            "42P01",
            format!("relation \"{name}\" does not exist"),
        )),
    })
}

pub fn truncate_relation(name: &str) -> Result<RelationRecord, CatalogError> {
    let name = normalize_identifier(name);
    with_catalog(|state| {
        state.relations_by_name.get(&name).cloned().ok_or_else(|| {
            CatalogError::new("42P01", format!("relation \"{name}\" does not exist"))
        })
    })
}

pub fn relation_by_name(name: &str) -> Option<RelationRecord> {
    let name = normalize_identifier(name);
    with_catalog(|state| state.relations_by_name.get(&name).cloned())
}

pub fn relations() -> Vec<RelationRecord> {
    with_catalog(|state| state.relations_by_name.values().cloned().collect())
}

pub fn relation_by_oid(oid: Oid) -> Option<RelationRecord> {
    with_catalog(|state| {
        state
            .relation_names_by_oid
            .get(&oid.0)
            .and_then(|name| state.relations_by_name.get(name))
            .cloned()
    })
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

pub fn add_primary_key(name: &str, columns: Vec<String>) -> Result<(), CatalogError> {
    let name = normalize_identifier(name);
    let columns = columns
        .into_iter()
        .map(|column| normalize_identifier(&column))
        .collect::<Vec<_>>();
    if columns.is_empty() {
        return Err(CatalogError::new(
            "42602",
            "primary key must include at least one column",
        ));
    }

    with_catalog(|state| {
        let relation = state.relations_by_name.get_mut(&name).ok_or_else(|| {
            CatalogError::new("42P01", format!("relation \"{name}\" does not exist"))
        })?;
        for column in &columns {
            if !relation
                .columns
                .iter()
                .any(|existing| &existing.name == column)
            {
                return Err(CatalogError::new(
                    "42703",
                    format!("column \"{column}\" does not exist"),
                ));
            }
        }
        for column in &mut relation.columns {
            if columns
                .iter()
                .any(|primary_key_column| primary_key_column == &column.name)
            {
                column.is_not_null = true;
            }
        }
        relation.primary_key = columns;
        Ok(())
    })
}

#[cfg(test)]
pub fn clear_for_tests() {
    with_catalog(|state| *state = CatalogState::default());
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

impl PgTypeRecord {
    const fn fixed(
        oid: Oid,
        name: &'static str,
        typlen: i16,
        typalign: u8,
        typinput: Oid,
        typoutput: Oid,
        typreceive: Oid,
        typsend: Oid,
        typarray: Oid,
        typcategory: u8,
        typispreferred: bool,
    ) -> Self {
        Self {
            oid,
            name,
            namespace: PG_CATALOG_NAMESPACE_OID,
            owner: BOOTSTRAP_SUPERUSER_OID,
            typlen,
            typbyval: true,
            typalign,
            typdelim: TYPDELIM_COMMA,
            typinput,
            typoutput,
            typreceive,
            typsend,
            typmodin: INVALID_OID,
            typmodout: INVALID_OID,
            typisdefined: true,
            typtype: TYPTYPE_BASE,
            typcategory,
            typispreferred,
            typrelid: INVALID_OID,
            typelem: INVALID_OID,
            typarray,
            typbasetype: INVALID_OID,
            typtypmod: -1,
            typcollation: INVALID_OID,
            typsubscript: INVALID_OID,
            typstorage: TYPSTORAGE_PLAIN,
        }
    }

    const fn fixed_custom(
        oid: Oid,
        name: &'static str,
        typlen: i16,
        typbyval: bool,
        typalign: u8,
        typinput: Oid,
        typoutput: Oid,
        typreceive: Oid,
        typsend: Oid,
        typarray: Oid,
        typcategory: u8,
    ) -> Self {
        Self {
            oid,
            name,
            namespace: PG_CATALOG_NAMESPACE_OID,
            owner: BOOTSTRAP_SUPERUSER_OID,
            typlen,
            typbyval,
            typalign,
            typdelim: TYPDELIM_COMMA,
            typinput,
            typoutput,
            typreceive,
            typsend,
            typmodin: INVALID_OID,
            typmodout: INVALID_OID,
            typisdefined: true,
            typtype: TYPTYPE_BASE,
            typcategory,
            typispreferred: false,
            typrelid: INVALID_OID,
            typelem: INVALID_OID,
            typarray,
            typbasetype: INVALID_OID,
            typtypmod: -1,
            typcollation: INVALID_OID,
            typsubscript: INVALID_OID,
            typstorage: TYPSTORAGE_PLAIN,
        }
    }

    const fn varlena_string(
        oid: Oid,
        name: &'static str,
        typinput: Oid,
        typoutput: Oid,
        typreceive: Oid,
        typsend: Oid,
        typmodin: Oid,
        typmodout: Oid,
        typarray: Oid,
        typispreferred: bool,
    ) -> Self {
        Self {
            oid,
            name,
            namespace: PG_CATALOG_NAMESPACE_OID,
            owner: BOOTSTRAP_SUPERUSER_OID,
            typlen: -1,
            typbyval: false,
            typalign: TYPALIGN_INT,
            typdelim: TYPDELIM_COMMA,
            typinput,
            typoutput,
            typreceive,
            typsend,
            typmodin,
            typmodout,
            typisdefined: true,
            typtype: TYPTYPE_BASE,
            typcategory: TYPCATEGORY_STRING,
            typispreferred,
            typrelid: INVALID_OID,
            typelem: INVALID_OID,
            typarray,
            typbasetype: INVALID_OID,
            typtypmod: -1,
            typcollation: DEFAULT_COLLATION_OID,
            typsubscript: INVALID_OID,
            typstorage: TYPSTORAGE_EXTENDED,
        }
    }

    const fn varlena_catalog(
        oid: Oid,
        name: &'static str,
        typinput: Oid,
        typoutput: Oid,
        typreceive: Oid,
        typsend: Oid,
        typarray: Oid,
        typcategory: u8,
        typelem: Oid,
        typcollation: Oid,
        typstorage: u8,
    ) -> Self {
        Self {
            oid,
            name,
            namespace: PG_CATALOG_NAMESPACE_OID,
            owner: BOOTSTRAP_SUPERUSER_OID,
            typlen: -1,
            typbyval: false,
            typalign: TYPALIGN_INT,
            typdelim: TYPDELIM_COMMA,
            typinput,
            typoutput,
            typreceive,
            typsend,
            typmodin: INVALID_OID,
            typmodout: INVALID_OID,
            typisdefined: true,
            typtype: TYPTYPE_BASE,
            typcategory,
            typispreferred: false,
            typrelid: INVALID_OID,
            typelem,
            typarray,
            typbasetype: INVALID_OID,
            typtypmod: -1,
            typcollation,
            typsubscript: if typelem.0 == 0 {
                INVALID_OID
            } else {
                F_ARRAY_SUBSCRIPT_HANDLER
            },
            typstorage,
        }
    }

    const fn varlena_array(
        oid: Oid,
        name: &'static str,
        typelem: Oid,
        typalign: u8,
        typcollation: Oid,
    ) -> Self {
        Self {
            typalign,
            typstorage: TYPSTORAGE_EXTENDED,
            ..Self::varlena_catalog(
                oid,
                name,
                F_ARRAY_IN,
                F_ARRAY_OUT,
                F_ARRAY_RECV,
                F_ARRAY_SEND,
                INVALID_OID,
                TYPCATEGORY_ARRAY,
                typelem,
                typcollation,
                TYPSTORAGE_EXTENDED,
            )
        }
    }

    const fn pseudo_array(oid: Oid, name: &'static str) -> Self {
        Self {
            oid,
            name,
            namespace: PG_CATALOG_NAMESPACE_OID,
            owner: BOOTSTRAP_SUPERUSER_OID,
            typlen: -1,
            typbyval: false,
            typalign: TYPALIGN_DOUBLE,
            typdelim: TYPDELIM_COMMA,
            typinput: F_ANYARRAYIN,
            typoutput: F_ANYARRAYOUT,
            typreceive: F_ANYARRAYRECV,
            typsend: F_ANYARRAYSEND,
            typmodin: INVALID_OID,
            typmodout: INVALID_OID,
            typisdefined: true,
            typtype: TYPTYPE_PSEUDO,
            typcategory: TYPCATEGORY_PSEUDO,
            typispreferred: false,
            typrelid: INVALID_OID,
            typelem: INVALID_OID,
            typarray: INVALID_OID,
            typbasetype: INVALID_OID,
            typtypmod: -1,
            typcollation: INVALID_OID,
            typsubscript: INVALID_OID,
            typstorage: TYPSTORAGE_EXTENDED,
        }
    }
}

pub fn lookup_builtin_type(oid: Oid) -> Option<PgTypeRecord> {
    match oid {
        BOOL_OID => Some(PgTypeRecord::fixed(
            BOOL_OID,
            "bool",
            1,
            TYPALIGN_CHAR,
            F_BOOLIN,
            F_BOOLOUT,
            F_BOOLRECV,
            F_BOOLSEND,
            Oid(1000),
            TYPCATEGORY_BOOLEAN,
            true,
        )),
        CHAR_OID => Some(PgTypeRecord::fixed(
            CHAR_OID,
            "char",
            1,
            TYPALIGN_CHAR,
            F_CHARIN,
            F_CHAROUT,
            INVALID_OID,
            INVALID_OID,
            Oid(1002),
            TYPCATEGORY_INTERNAL,
            false,
        )),
        NAME_OID => {
            let mut record = PgTypeRecord {
                typbyval: false,
                typelem: CHAR_OID,
                typcollation: C_COLLATION_OID,
                ..PgTypeRecord::fixed(
                    NAME_OID,
                    "name",
                    64,
                    TYPALIGN_CHAR,
                    F_NAMEIN,
                    F_NAMEOUT,
                    F_NAMERECV,
                    F_NAMESEND,
                    Oid(1003),
                    TYPCATEGORY_STRING,
                    false,
                )
            };
            record.typsubscript = INVALID_OID;
            Some(record)
        }
        INT8_OID => Some(PgTypeRecord::fixed(
            INT8_OID,
            "int8",
            8,
            TYPALIGN_DOUBLE,
            F_INT8IN,
            F_INT8OUT,
            F_INT8RECV,
            F_INT8SEND,
            Oid(1016),
            TYPCATEGORY_NUMERIC,
            false,
        )),
        INT2_OID => Some(PgTypeRecord::fixed(
            INT2_OID,
            "int2",
            2,
            TYPALIGN_SHORT,
            F_INT2IN,
            F_INT2OUT,
            F_INT2RECV,
            F_INT2SEND,
            Oid(1005),
            TYPCATEGORY_NUMERIC,
            false,
        )),
        INT2VECTOR_OID => Some(PgTypeRecord::varlena_catalog(
            INT2VECTOR_OID,
            "int2vector",
            F_INT2VECTORIN,
            F_INT2VECTOROUT,
            F_INT2VECTORRECV,
            F_INT2VECTORSEND,
            Oid(1006),
            TYPCATEGORY_ARRAY,
            INT2_OID,
            INVALID_OID,
            TYPSTORAGE_PLAIN,
        )),
        INT4_OID => Some(PgTypeRecord::fixed(
            INT4_OID,
            "int4",
            4,
            TYPALIGN_INT,
            F_INT4IN,
            F_INT4OUT,
            F_INT4RECV,
            F_INT4SEND,
            Oid(1007),
            TYPCATEGORY_NUMERIC,
            false,
        )),
        FLOAT8_OID => Some(PgTypeRecord::fixed(
            FLOAT8_OID,
            "float8",
            8,
            TYPALIGN_DOUBLE,
            F_FLOAT8IN,
            F_FLOAT8OUT,
            F_FLOAT8RECV,
            F_FLOAT8SEND,
            Oid(1022),
            TYPCATEGORY_NUMERIC,
            true,
        )),
        TEXT_OID => Some(PgTypeRecord::varlena_string(
            TEXT_OID,
            "text",
            F_TEXTIN,
            F_TEXTOUT,
            F_TEXTRECV,
            F_TEXTSEND,
            INVALID_OID,
            INVALID_OID,
            Oid(1009),
            true,
        )),
        OID_OID => Some(PgTypeRecord::fixed(
            OID_OID,
            "oid",
            4,
            TYPALIGN_INT,
            F_OIDIN,
            F_OIDOUT,
            F_OIDRECV,
            F_OIDSEND,
            Oid(1028),
            TYPCATEGORY_NUMERIC,
            true,
        )),
        TID_OID => Some(PgTypeRecord::fixed_custom(
            TID_OID,
            "tid",
            6,
            false,
            TYPALIGN_SHORT,
            F_TIDIN,
            F_TIDOUT,
            F_TIDRECV,
            F_TIDSEND,
            Oid(1010),
            TYPCATEGORY_USER_DEFINED,
        )),
        XID_OID => Some(PgTypeRecord::fixed_custom(
            XID_OID,
            "xid",
            4,
            true,
            TYPALIGN_INT,
            F_XIDIN,
            F_XIDOUT,
            F_XIDRECV,
            F_XIDSEND,
            Oid(1011),
            TYPCATEGORY_USER_DEFINED,
        )),
        CID_OID => Some(PgTypeRecord::fixed_custom(
            CID_OID,
            "cid",
            4,
            true,
            TYPALIGN_INT,
            F_CIDIN,
            F_CIDOUT,
            F_CIDRECV,
            F_CIDSEND,
            Oid(1012),
            TYPCATEGORY_USER_DEFINED,
        )),
        OIDVECTOR_OID => Some(PgTypeRecord::varlena_catalog(
            OIDVECTOR_OID,
            "oidvector",
            F_OIDVECTORIN,
            F_OIDVECTOROUT,
            F_OIDVECTORRECV,
            F_OIDVECTORSEND,
            Oid(1013),
            TYPCATEGORY_ARRAY,
            OID_OID,
            INVALID_OID,
            TYPSTORAGE_PLAIN,
        )),
        PG_NODE_TREE_OID => Some(PgTypeRecord::varlena_catalog(
            PG_NODE_TREE_OID,
            "pg_node_tree",
            F_PG_NODE_TREE_IN,
            F_PG_NODE_TREE_OUT,
            F_PG_NODE_TREE_RECV,
            F_PG_NODE_TREE_SEND,
            INVALID_OID,
            TYPCATEGORY_INTERNAL,
            INVALID_OID,
            DEFAULT_COLLATION_OID,
            TYPSTORAGE_EXTENDED,
        )),
        INT2_ARRAY_OID => Some(PgTypeRecord::varlena_array(
            INT2_ARRAY_OID,
            "_int2",
            INT2_OID,
            TYPALIGN_INT,
            INVALID_OID,
        )),
        INT4_ARRAY_OID => Some(PgTypeRecord::varlena_array(
            INT4_ARRAY_OID,
            "_int4",
            INT4_OID,
            TYPALIGN_INT,
            INVALID_OID,
        )),
        TEXT_ARRAY_OID => Some(PgTypeRecord::varlena_array(
            TEXT_ARRAY_OID,
            "_text",
            TEXT_OID,
            TYPALIGN_INT,
            DEFAULT_COLLATION_OID,
        )),
        OID_ARRAY_OID => Some(PgTypeRecord::varlena_array(
            OID_ARRAY_OID,
            "_oid",
            OID_OID,
            TYPALIGN_INT,
            INVALID_OID,
        )),
        ACLITEM_OID => Some(PgTypeRecord::fixed_custom(
            ACLITEM_OID,
            "aclitem",
            16,
            false,
            TYPALIGN_DOUBLE,
            F_ACLITEMIN,
            F_ACLITEMOUT,
            INVALID_OID,
            INVALID_OID,
            ACLITEM_ARRAY_OID,
            TYPCATEGORY_USER_DEFINED,
        )),
        ACLITEM_ARRAY_OID => Some(PgTypeRecord::varlena_array(
            ACLITEM_ARRAY_OID,
            "_aclitem",
            ACLITEM_OID,
            TYPALIGN_DOUBLE,
            INVALID_OID,
        )),
        BPCHAR_OID => Some(PgTypeRecord::varlena_string(
            BPCHAR_OID,
            "bpchar",
            F_BPCHARIN,
            F_BPCHAROUT,
            F_BPCHARRECV,
            F_BPCHARSEND,
            F_BPCHARTYPMODIN,
            F_BPCHARTYPMODOUT,
            Oid(1014),
            false,
        )),
        VARCHAR_OID => Some(PgTypeRecord::varlena_string(
            VARCHAR_OID,
            "varchar",
            F_VARCHARIN,
            F_VARCHAROUT,
            F_VARCHARRECV,
            F_VARCHARSEND,
            F_VARCHARTYPMODIN,
            F_VARCHARTYPMODOUT,
            Oid(1015),
            false,
        )),
        TIMESTAMP_OID => {
            let mut record = PgTypeRecord::fixed(
                TIMESTAMP_OID,
                "timestamp",
                8,
                TYPALIGN_DOUBLE,
                F_TIMESTAMPIN,
                F_TIMESTAMPOUT,
                F_TIMESTAMPRECV,
                F_TIMESTAMPSEND,
                Oid(1115),
                TYPCATEGORY_DATETIME,
                false,
            );
            record.typmodin = F_TIMESTAMPTYPMODIN;
            record.typmodout = F_TIMESTAMPTYPMODOUT;
            Some(record)
        }
        TIMESTAMPTZ_OID => {
            let mut record = PgTypeRecord::fixed(
                TIMESTAMPTZ_OID,
                "timestamptz",
                8,
                TYPALIGN_DOUBLE,
                F_TIMESTAMPTZIN,
                F_TIMESTAMPTZOUT,
                F_TIMESTAMPTZRECV,
                F_TIMESTAMPTZSEND,
                Oid(1185),
                TYPCATEGORY_DATETIME,
                true,
            );
            record.typmodin = F_TIMESTAMPTZTYPMODIN;
            record.typmodout = F_TIMESTAMPTZTYPMODOUT;
            Some(record)
        }
        REGCLASS_OID => Some(PgTypeRecord::fixed(
            REGCLASS_OID,
            "regclass",
            4,
            TYPALIGN_INT,
            F_REGCLASSIN,
            F_REGCLASSOUT,
            F_REGCLASSRECV,
            F_REGCLASSSEND,
            Oid(2210),
            TYPCATEGORY_NUMERIC,
            false,
        )),
        ANYARRAY_OID => Some(PgTypeRecord::pseudo_array(ANYARRAY_OID, "anyarray")),
        _ => None,
    }
}

const BUILTIN_TYPE_OIDS: &[Oid] = &[
    BOOL_OID,
    CHAR_OID,
    NAME_OID,
    INT8_OID,
    INT2_OID,
    INT2VECTOR_OID,
    INT4_OID,
    FLOAT8_OID,
    TEXT_OID,
    OID_OID,
    TID_OID,
    XID_OID,
    CID_OID,
    OIDVECTOR_OID,
    PG_NODE_TREE_OID,
    INT2_ARRAY_OID,
    INT4_ARRAY_OID,
    TEXT_ARRAY_OID,
    OID_ARRAY_OID,
    ACLITEM_OID,
    ACLITEM_ARRAY_OID,
    BPCHAR_OID,
    VARCHAR_OID,
    TIMESTAMP_OID,
    TIMESTAMPTZ_OID,
    REGCLASS_OID,
    ANYARRAY_OID,
];

pub fn builtin_type_by_name(name: &str, namespace: Oid) -> Option<PgTypeRecord> {
    if namespace != PG_CATALOG_NAMESPACE_OID {
        return None;
    }

    let name = normalize_identifier(name);
    let canonical_name = match name.as_str() {
        "boolean" => "bool",
        "character" => "bpchar",
        "character varying" => "varchar",
        "integer" | "int" => "int4",
        "smallint" => "int2",
        "bigint" => "int8",
        "double precision" => "float8",
        "timestamp without time zone" => "timestamp",
        "timestamp with time zone" => "timestamptz",
        other => other,
    };

    BUILTIN_TYPE_OIDS
        .iter()
        .filter_map(|oid| lookup_builtin_type(*oid))
        .find(|record| record.name == canonical_name)
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

impl PgProcRecord {
    const fn internal_function(
        oid: Oid,
        name: &'static str,
        return_type: Oid,
        arg_types: &'static [Oid],
        source: &'static str,
    ) -> Self {
        Self {
            oid,
            name,
            namespace: PG_CATALOG_NAMESPACE_OID,
            owner: BOOTSTRAP_SUPERUSER_OID,
            language: INTERNAL_LANGUAGE_OID,
            cost: 1,
            rows: 0,
            variadic: INVALID_OID,
            support: INVALID_OID,
            kind: PROKIND_FUNCTION,
            security_definer: false,
            leakproof: false,
            strict: true,
            returns_set: false,
            volatility: PROVOLATILE_IMMUTABLE,
            parallel: PROPARALLEL_SAFE,
            return_type,
            arg_types,
            arg_defaults: 0,
            source,
        }
    }

    const fn stable_function(
        oid: Oid,
        name: &'static str,
        return_type: Oid,
        arg_types: &'static [Oid],
        source: &'static str,
    ) -> Self {
        Self {
            oid,
            name,
            namespace: PG_CATALOG_NAMESPACE_OID,
            owner: BOOTSTRAP_SUPERUSER_OID,
            language: INTERNAL_LANGUAGE_OID,
            cost: 1,
            rows: 0,
            variadic: INVALID_OID,
            support: INVALID_OID,
            kind: PROKIND_FUNCTION,
            security_definer: false,
            leakproof: false,
            strict: true,
            returns_set: false,
            volatility: PROVOLATILE_STABLE,
            parallel: PROPARALLEL_SAFE,
            return_type,
            arg_types,
            arg_defaults: 0,
            source,
        }
    }

    const fn aggregate(
        oid: Oid,
        return_type: Oid,
        arg_types: &'static [Oid],
        support: Oid,
    ) -> Self {
        Self {
            oid,
            name: "count",
            namespace: PG_CATALOG_NAMESPACE_OID,
            owner: BOOTSTRAP_SUPERUSER_OID,
            language: INTERNAL_LANGUAGE_OID,
            cost: 1,
            rows: 0,
            variadic: INVALID_OID,
            support,
            kind: PROKIND_AGGREGATE,
            security_definer: false,
            leakproof: false,
            strict: false,
            returns_set: false,
            volatility: PROVOLATILE_IMMUTABLE,
            parallel: PROPARALLEL_SAFE,
            return_type,
            arg_types,
            arg_defaults: 0,
            source: "aggregate_dummy",
        }
    }
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

const COUNT_ANY_ARGS: [Oid; 1] = [ANY_OID];
const INT8_ARGS: [Oid; 1] = [INT8_OID];
const INT8_ANY_ARGS: [Oid; 2] = [INT8_OID, ANY_OID];
const INT8_INT8_ARGS: [Oid; 2] = [INT8_OID, INT8_OID];
const INT4_INT4_ARGS: [Oid; 2] = [INT4_OID, INT4_OID];
const OID_OID_ARGS: [Oid; 2] = [OID_OID, OID_OID];
const TIMESTAMP_ARGS: [Oid; 1] = [TIMESTAMP_OID];
const TIMESTAMPTZ_ARGS: [Oid; 1] = [TIMESTAMPTZ_OID];
const EQSEL_ARGS: [Oid; 4] = [INTERNAL_OID, OID_OID, INTERNAL_OID, INT4_OID];
const EQJOINSEL_ARGS: [Oid; 5] = [INTERNAL_OID, OID_OID, INTERNAL_OID, INT2_OID, INTERNAL_OID];
const INTERNAL_ARGS: [Oid; 1] = [INTERNAL_OID];
const NO_ARGS: [Oid; 0] = [];

const BUILTIN_PROCS: &[PgProcRecord] = &[
    PgProcRecord::internal_function(F_INT4EQ, "int4eq", BOOL_OID, &INT4_INT4_ARGS, "int4eq"),
    PgProcRecord::internal_function(F_INT4LT, "int4lt", BOOL_OID, &INT4_INT4_ARGS, "int4lt"),
    PgProcRecord::internal_function(F_INT4GT, "int4gt", BOOL_OID, &INT4_INT4_ARGS, "int4gt"),
    PgProcRecord::internal_function(
        F_BTINT4CMP,
        "btint4cmp",
        INT4_OID,
        &INT4_INT4_ARGS,
        "btint4cmp",
    ),
    PgProcRecord::internal_function(F_OIDEQ, "oideq", BOOL_OID, &OID_OID_ARGS, "oideq"),
    PgProcRecord::internal_function(F_INT4PL, "int4pl", INT4_OID, &INT4_INT4_ARGS, "int4pl"),
    PgProcRecord::internal_function(F_INT8PL, "int8pl", INT8_OID, &INT8_INT8_ARGS, "int8pl"),
    PgProcRecord::internal_function(F_INT8EQ, "int8eq", BOOL_OID, &INT8_INT8_ARGS, "int8eq"),
    PgProcRecord::internal_function(F_EQSEL, "eqsel", FLOAT8_OID, &EQSEL_ARGS, "eqsel"),
    PgProcRecord::internal_function(
        F_EQJOINSEL,
        "eqjoinsel",
        FLOAT8_OID,
        &EQJOINSEL_ARGS,
        "eqjoinsel",
    ),
    PgProcRecord::internal_function(F_INT8INC, "int8inc", INT8_OID, &INT8_ARGS, "int8inc"),
    PgProcRecord::internal_function(F_INT8DEC, "int8dec", INT8_OID, &INT8_ARGS, "int8dec"),
    PgProcRecord::internal_function(
        F_INT8INC_ANY,
        "int8inc_any",
        INT8_OID,
        &INT8_ANY_ARGS,
        "int8inc_any",
    ),
    PgProcRecord::internal_function(
        F_INT8DEC_ANY,
        "int8dec_any",
        INT8_OID,
        &INT8_ANY_ARGS,
        "int8dec_any",
    ),
    PgProcRecord::aggregate(F_COUNT_ANY, INT8_OID, &COUNT_ANY_ARGS, F_INT8INC_SUPPORT),
    PgProcRecord::aggregate(F_COUNT, INT8_OID, &NO_ARGS, F_INT8INC_SUPPORT),
    PgProcRecord::internal_function(
        F_INT8INC_SUPPORT,
        "int8inc_support",
        INTERNAL_OID,
        &INTERNAL_ARGS,
        "int8inc_support",
    ),
    PgProcRecord::stable_function(
        F_TIMESTAMPTZ_TIMESTAMP,
        "timestamp",
        TIMESTAMP_OID,
        &TIMESTAMPTZ_ARGS,
        "timestamptz_timestamp",
    ),
    PgProcRecord::stable_function(
        F_TIMESTAMP_TIMESTAMPTZ,
        "timestamptz",
        TIMESTAMPTZ_OID,
        &TIMESTAMP_ARGS,
        "timestamp_timestamptz",
    ),
];

const BUILTIN_AGGREGATES: &[PgAggregateRecord] = &[
    PgAggregateRecord {
        function_oid: F_COUNT_ANY,
        kind: AGGKIND_NORMAL,
        direct_arg_count: 0,
        transition_fn: F_INT8INC_ANY,
        final_fn: INVALID_OID,
        combine_fn: F_INT8PL,
        serial_fn: INVALID_OID,
        deserial_fn: INVALID_OID,
        moving_transition_fn: F_INT8INC_ANY,
        moving_inverse_fn: F_INT8DEC_ANY,
        moving_final_fn: INVALID_OID,
        final_extra: false,
        moving_final_extra: false,
        final_modify: AGGMODIFY_READ_ONLY,
        moving_final_modify: AGGMODIFY_READ_ONLY,
        sort_operator: INVALID_OID,
        transition_type: INT8_OID,
        transition_space: 0,
        moving_transition_type: INT8_OID,
        moving_transition_space: 0,
        init_value: Some("0"),
        moving_init_value: Some("0"),
    },
    PgAggregateRecord {
        function_oid: F_COUNT,
        kind: AGGKIND_NORMAL,
        direct_arg_count: 0,
        transition_fn: F_INT8INC,
        final_fn: INVALID_OID,
        combine_fn: F_INT8PL,
        serial_fn: INVALID_OID,
        deserial_fn: INVALID_OID,
        moving_transition_fn: F_INT8INC,
        moving_inverse_fn: F_INT8DEC,
        moving_final_fn: INVALID_OID,
        final_extra: false,
        moving_final_extra: false,
        final_modify: AGGMODIFY_READ_ONLY,
        moving_final_modify: AGGMODIFY_READ_ONLY,
        sort_operator: INVALID_OID,
        transition_type: INT8_OID,
        transition_space: 0,
        moving_transition_type: INT8_OID,
        moving_transition_space: 0,
        init_value: Some("0"),
        moving_init_value: Some("0"),
    },
];

pub fn builtin_proc_by_oid(oid: Oid) -> Option<&'static PgProcRecord> {
    BUILTIN_PROCS.iter().find(|record| record.oid == oid)
}

pub fn builtin_procs_by_name(name: &str) -> impl Iterator<Item = &'static PgProcRecord> {
    let name = normalize_identifier(name);
    BUILTIN_PROCS
        .iter()
        .filter(move |record| record.name == name.as_str())
}

pub fn builtin_aggregate_by_proc_oid(function_oid: Oid) -> Option<&'static PgAggregateRecord> {
    BUILTIN_AGGREGATES
        .iter()
        .find(|record| record.function_oid == function_oid)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(cast.function, F_TIMESTAMPTZ_TIMESTAMP);
        assert_eq!(cast.context, COERCION_CODE_ASSIGNMENT);
        assert_eq!(cast.method, COERCION_METHOD_FUNCTION);

        let proc = builtin_proc_by_oid(cast.function).expect("cast function proc");
        assert_eq!(proc.return_type, TIMESTAMP_OID);
        assert_eq!(proc.arg_types, [TIMESTAMPTZ_OID]);
        assert_eq!(proc.volatility, PROVOLATILE_STABLE);
    }

    #[test]
    fn classifies_pgbench_critical_virtual_catalogs() {
        let required = [
            ("pg_type", VirtualCatalogPolicy::Static),
            ("pg_proc", VirtualCatalogPolicy::Static),
            ("pg_operator", VirtualCatalogPolicy::Static),
            ("pg_aggregate", VirtualCatalogPolicy::Static),
            ("pg_namespace", VirtualCatalogPolicy::Static),
            ("pg_cast", VirtualCatalogPolicy::Static),
            ("pg_class", VirtualCatalogPolicy::Dynamic),
            ("pg_attribute", VirtualCatalogPolicy::Dynamic),
            ("pg_index", VirtualCatalogPolicy::Dynamic),
            ("pg_constraint", VirtualCatalogPolicy::Dynamic),
            ("pg_statistic", VirtualCatalogPolicy::Empty),
            ("pg_statistic_ext", VirtualCatalogPolicy::Empty),
            ("pg_statistic_ext_data", VirtualCatalogPolicy::Empty),
            ("pg_authid", VirtualCatalogPolicy::Empty),
            ("pg_auth_members", VirtualCatalogPolicy::Empty),
            ("pg_parameter_acl", VirtualCatalogPolicy::Empty),
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
    fn creates_drops_and_truncates_relations() {
        clear_for_tests();
        let relation = create_relation(
            "PgBench_Accounts",
            vec![
                ColumnRecord::new("aid", INT4_OID, -1, true),
                ColumnRecord::new("filler", BPCHAR_OID, -1, false),
            ],
            false,
        )
        .unwrap()
        .expect("created");

        assert_eq!(relation.name, "pgbench_accounts");
        assert_eq!(relation.columns[0].name, "aid");
        assert_eq!(
            relation_by_name("pgbench_accounts").unwrap().oid,
            relation.oid
        );
        assert_eq!(
            relation_by_oid(relation.oid).unwrap().name,
            "pgbench_accounts"
        );
        assert_eq!(
            truncate_relation("pgbench_accounts").unwrap().oid,
            relation.oid
        );
        add_primary_key("pgbench_accounts", vec!["aid".to_owned()]).unwrap();
        assert_eq!(
            relation_by_name("pgbench_accounts").unwrap().primary_key,
            vec!["aid"]
        );
        assert_eq!(
            drop_relation("pgbench_accounts", false)
                .unwrap()
                .unwrap()
                .oid,
            relation.oid
        );
        assert!(relation_by_name("pgbench_accounts").is_none());
    }
}
