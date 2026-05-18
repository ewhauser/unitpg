#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use fastpg_types::Oid;

pub const BOOL_OID: Oid = Oid(16);
pub const CHAR_OID: Oid = Oid(18);
pub const NAME_OID: Oid = Oid(19);
pub const INT8_OID: Oid = Oid(20);
pub const INT2_OID: Oid = Oid(21);
pub const INT4_OID: Oid = Oid(23);
pub const TEXT_OID: Oid = Oid(25);
pub const OID_OID: Oid = Oid(26);
pub const BPCHAR_OID: Oid = Oid(1042);
pub const VARCHAR_OID: Oid = Oid(1043);
pub const TIMESTAMP_OID: Oid = Oid(1114);

pub const DEFAULT_COLLATION_OID: Oid = Oid(100);
pub const C_COLLATION_OID: Oid = Oid(950);

const F_CHAROUT: Oid = Oid(33);
const F_NAMEIN: Oid = Oid(34);
const F_NAMEOUT: Oid = Oid(35);
const F_INT2IN: Oid = Oid(38);
const F_INT2OUT: Oid = Oid(39);
const F_INT4IN: Oid = Oid(42);
const F_INT4OUT: Oid = Oid(43);
const F_TEXTIN: Oid = Oid(46);
const F_TEXTOUT: Oid = Oid(47);
const F_INT8IN: Oid = Oid(460);
const F_INT8OUT: Oid = Oid(461);
const F_BPCHARIN: Oid = Oid(1044);
const F_BPCHAROUT: Oid = Oid(1045);
const F_VARCHARIN: Oid = Oid(1046);
const F_VARCHAROUT: Oid = Oid(1047);
const F_BOOLIN: Oid = Oid(1242);
const F_BOOLOUT: Oid = Oid(1243);
const F_CHARIN: Oid = Oid(1245);
const F_OIDIN: Oid = Oid(1798);
const F_OIDOUT: Oid = Oid(1799);
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
const F_BPCHARRECV: Oid = Oid(2430);
const F_BPCHARSEND: Oid = Oid(2431);
const F_VARCHARRECV: Oid = Oid(2432);
const F_VARCHARSEND: Oid = Oid(2433);
const F_BOOLRECV: Oid = Oid(2436);
const F_BOOLSEND: Oid = Oid(2437);
const F_TIMESTAMPRECV: Oid = Oid(2474);
const F_TIMESTAMPSEND: Oid = Oid(2475);
const F_BPCHARTYPMODIN: Oid = Oid(2913);
const F_BPCHARTYPMODOUT: Oid = Oid(2914);
const F_VARCHARTYPMODIN: Oid = Oid(2915);
const F_VARCHARTYPMODOUT: Oid = Oid(2916);
const F_TIMESTAMPIN: Oid = Oid(1312);
const F_TIMESTAMPOUT: Oid = Oid(1313);
const F_TIMESTAMPTYPMODIN: Oid = Oid(2905);
const F_TIMESTAMPTYPMODOUT: Oid = Oid(2906);

const INVALID_OID: Oid = Oid(0);

const TYPALIGN_CHAR: u8 = b'c';
const TYPALIGN_SHORT: u8 = b's';
const TYPALIGN_INT: u8 = b'i';
const TYPALIGN_DOUBLE: u8 = b'd';

const TYPDELIM_COMMA: u8 = b',';
const TYPSTORAGE_PLAIN: u8 = b'p';
const TYPSTORAGE_EXTENDED: u8 = b'x';
const TYPTYPE_BASE: u8 = b'b';

const TYPCATEGORY_BOOLEAN: u8 = b'B';
const TYPCATEGORY_DATETIME: u8 = b'D';
const TYPCATEGORY_NUMERIC: u8 = b'N';
const TYPCATEGORY_STRING: u8 = b'S';
const TYPCATEGORY_INTERNAL: u8 = b'Z';

pub const PG_CATALOG_NAMESPACE_OID: Oid = Oid(11);
pub const PUBLIC_NAMESPACE_OID: Oid = Oid(2200);
const FIRST_DYNAMIC_RELATION_OID: u32 = 16_384;

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
}

impl Default for CatalogState {
    fn default() -> Self {
        Self {
            next_relation_oid: FIRST_DYNAMIC_RELATION_OID,
            relations_by_name: BTreeMap::new(),
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
        state.relations_by_name.insert(name, relation.clone());
        Ok(Some(relation))
    })
}

pub fn drop_relation(name: &str, missing_ok: bool) -> Result<Option<RelationRecord>, CatalogError> {
    let name = normalize_identifier(name);
    with_catalog(|state| match state.relations_by_name.remove(&name) {
        Some(relation) => Ok(Some(relation)),
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

    const fn varlena_string(
        oid: Oid,
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
}

pub fn lookup_builtin_type(oid: Oid) -> Option<PgTypeRecord> {
    match oid {
        BOOL_OID => Some(PgTypeRecord::fixed(
            BOOL_OID,
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
        INT4_OID => Some(PgTypeRecord::fixed(
            INT4_OID,
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
        TEXT_OID => Some(PgTypeRecord::varlena_string(
            TEXT_OID,
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
        BPCHAR_OID => Some(PgTypeRecord::varlena_string(
            BPCHAR_OID,
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
        _ => None,
    }
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
            BPCHAR_OID,
            VARCHAR_OID,
            TIMESTAMP_OID,
        ] {
            let record = lookup_builtin_type(oid).expect("builtin type");
            assert_eq!(record.oid, oid);
            assert!(record.typisdefined);
            assert_ne!(record.typinput, INVALID_OID);
            assert_ne!(record.typoutput, INVALID_OID);
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
