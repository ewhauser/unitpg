#![forbid(unsafe_code)]

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Oid(pub u32);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Column {
    pub name: String,
    pub data_type: PgType,
}

impl Column {
    pub fn new(name: impl Into<String>, data_type: PgType) -> Self {
        Self {
            name: name.into(),
            data_type,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PgType {
    Int2,
    Int4,
    Int8,
    Varchar,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Value {
    Int2(i16),
    Int4(i32),
    Int8(i64),
    Text(String),
    Null,
}
