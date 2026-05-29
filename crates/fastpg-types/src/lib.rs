#![forbid(unsafe_code)]

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Oid(pub u32);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Column {
    pub name: String,
    pub data_type: PgType,
    pub type_oid: u32,
    pub type_modifier: i32,
}

impl Column {
    pub fn new(name: impl Into<String>, data_type: PgType) -> Self {
        Self::with_type_oid(name, data_type, data_type.default_type_oid())
    }

    pub fn with_type_oid(name: impl Into<String>, data_type: PgType, type_oid: u32) -> Self {
        Self::with_type_metadata(name, data_type, type_oid, -1)
    }

    pub fn with_type_metadata(
        name: impl Into<String>,
        data_type: PgType,
        type_oid: u32,
        type_modifier: i32,
    ) -> Self {
        Self {
            name: name.into(),
            data_type,
            type_oid,
            type_modifier,
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

impl PgType {
    pub fn default_type_oid(self) -> u32 {
        match self {
            PgType::Int2 => 21,
            PgType::Int4 => 23,
            PgType::Int8 => 20,
            PgType::Varchar => 1043,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Value {
    Int2(i16),
    Int4(i32),
    Int8(i64),
    Text(String),
    RawText(String),
    TextWithBinary { text: String, binary: Vec<u8> },
    Null,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParameterFormat {
    Text,
    Binary,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueryParameter {
    Int2(i16),
    Int4(i32),
    Int8(i64),
    Text(String),
    Raw {
        format: ParameterFormat,
        bytes: Vec<u8>,
    },
    Null,
}

impl From<&Value> for QueryParameter {
    fn from(value: &Value) -> Self {
        match value {
            Value::Int2(value) => QueryParameter::Int2(*value),
            Value::Int4(value) => QueryParameter::Int4(*value),
            Value::Int8(value) => QueryParameter::Int8(*value),
            Value::Text(value) | Value::RawText(value) => QueryParameter::Text(value.clone()),
            Value::TextWithBinary { text, .. } => QueryParameter::Text(text.clone()),
            Value::Null => QueryParameter::Null,
        }
    }
}
