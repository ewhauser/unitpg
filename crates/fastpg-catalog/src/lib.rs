#![forbid(unsafe_code)]

use fastpg_types::Oid;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelationName {
    pub namespace: Oid,
    pub name: String,
}
