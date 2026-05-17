#![forbid(unsafe_code)]

use fastpg_types::{Column, PgType, Value};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryDescription {
    pub parameter_types: Vec<PgType>,
    pub fields: Vec<Column>,
}

impl QueryDescription {
    pub fn new(parameter_types: Vec<PgType>, fields: Vec<Column>) -> Self {
        Self {
            parameter_types,
            fields,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryResult {
    pub fields: Vec<Column>,
    pub rows: Vec<Vec<Value>>,
}

impl QueryResult {
    pub fn new(fields: Vec<Column>, rows: Vec<Vec<Value>>) -> Self {
        Self { fields, rows }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueryExecution {
    Rows(QueryResult),
    Unsupported { query: String },
    InvalidParameters { message: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryExecutor {
    server_version: String,
}

impl QueryExecutor {
    pub fn new(server_version: impl Into<String>) -> Self {
        Self {
            server_version: server_version.into(),
        }
    }

    pub fn describe(&self, sql: &str) -> Option<QueryDescription> {
        SupportedQuery::from_sql(sql)
            .map(|query| QueryDescription::new(query.parameter_types(), query.result_fields()))
    }

    pub fn execute(&self, sql: &str, parameters: &[Value]) -> QueryExecution {
        let Some(query) = SupportedQuery::from_sql(sql) else {
            return QueryExecution::Unsupported {
                query: sql.to_owned(),
            };
        };

        match query {
            SupportedQuery::SelectOne => QueryExecution::Rows(QueryResult::new(
                query.result_fields(),
                vec![vec![Value::Int4(1)]],
            )),
            SupportedQuery::ShowServerVersion => QueryExecution::Rows(QueryResult::new(
                query.result_fields(),
                vec![vec![Value::Text(self.server_version.clone())]],
            )),
            SupportedQuery::SelectInt4Parameter => match parameters.first() {
                Some(Value::Int4(value)) => QueryExecution::Rows(QueryResult::new(
                    query.result_fields(),
                    vec![vec![Value::Int4(*value)]],
                )),
                Some(Value::Null) => QueryExecution::Rows(QueryResult::new(
                    query.result_fields(),
                    vec![vec![Value::Null]],
                )),
                Some(other) => QueryExecution::InvalidParameters {
                    message: format!("expected int4 parameter, got {other:?}"),
                },
                None => QueryExecution::InvalidParameters {
                    message: "missing int4 parameter".to_owned(),
                },
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SupportedQuery {
    SelectOne,
    ShowServerVersion,
    SelectInt4Parameter,
}

impl SupportedQuery {
    fn from_sql(sql: &str) -> Option<Self> {
        match normalize_sql(sql).as_str() {
            "select 1" => Some(Self::SelectOne),
            "show server_version" => Some(Self::ShowServerVersion),
            "select $1::int4" => Some(Self::SelectInt4Parameter),
            _ => None,
        }
    }

    fn result_fields(&self) -> Vec<Column> {
        match self {
            Self::SelectOne | Self::SelectInt4Parameter => {
                vec![Column::new("?column?", PgType::Int4)]
            }
            Self::ShowServerVersion => vec![Column::new("server_version", PgType::Varchar)],
        }
    }

    fn parameter_types(&self) -> Vec<PgType> {
        match self {
            Self::SelectInt4Parameter => vec![PgType::Int4],
            Self::SelectOne | Self::ShowServerVersion => vec![],
        }
    }
}

fn normalize_sql(sql: &str) -> String {
    sql.trim().trim_end_matches(';').trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_parameterized_int4_query() {
        let executor = QueryExecutor::new("17.0-fastpg");

        let description = executor.describe("SELECT $1::int4").unwrap();

        assert_eq!(description.parameter_types, vec![PgType::Int4]);
        assert_eq!(
            description.fields,
            vec![Column::new("?column?", PgType::Int4)]
        );
    }

    #[test]
    fn executes_server_version_query() {
        let executor = QueryExecutor::new("17.0-fastpg");

        let result = executor.execute("SHOW server_version", &[]);

        assert_eq!(
            result,
            QueryExecution::Rows(QueryResult::new(
                vec![Column::new("server_version", PgType::Varchar)],
                vec![vec![Value::Text("17.0-fastpg".to_owned())]]
            ))
        );
    }
}
