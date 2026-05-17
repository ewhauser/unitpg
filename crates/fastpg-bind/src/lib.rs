#![forbid(unsafe_code)]

use fastpg_parser::{ColumnDef, Literal, ParsedStatement, SqlTypeName};
use fastpg_types::{Column, PgType};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BoundStatement {
    SelectOne,
    ShowServerVersion,
    SelectInt4Parameter,
    SelectRelkindByRegclassParameter,
    SelectPgbenchPartitionInfo,
    SelectCount {
        table: String,
    },
    SelectColumnWhereInt {
        table: String,
        column: String,
        key_column: String,
        key_value: i64,
    },
    DropTables {
        if_exists: bool,
        names: Vec<String>,
    },
    CreateTable {
        name: String,
        columns: Vec<Column>,
    },
    TruncateTables {
        names: Vec<String>,
    },
    Begin,
    Commit,
    Rollback,
    CopyFromStdin {
        table: String,
    },
    UpdateAddInt {
        table: String,
        column: String,
        addend: i64,
        key_column: String,
        key_value: i64,
    },
    Insert {
        table: String,
        columns: Vec<String>,
        values: Vec<BoundExpression>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BoundExpression {
    Int(i64),
    Text(String),
    Null,
    CurrentTimestamp,
}

pub fn bind(statement: ParsedStatement) -> BoundStatement {
    match statement {
        ParsedStatement::SelectOne => BoundStatement::SelectOne,
        ParsedStatement::ShowServerVersion => BoundStatement::ShowServerVersion,
        ParsedStatement::SelectInt4Parameter => BoundStatement::SelectInt4Parameter,
        ParsedStatement::SelectRelkindByRegclassParameter => {
            BoundStatement::SelectRelkindByRegclassParameter
        }
        ParsedStatement::SelectPgbenchPartitionInfo => BoundStatement::SelectPgbenchPartitionInfo,
        ParsedStatement::SelectCount { table } => BoundStatement::SelectCount { table },
        ParsedStatement::SelectColumnWhereInt {
            table,
            column,
            key_column,
            key_value,
        } => BoundStatement::SelectColumnWhereInt {
            table,
            column,
            key_column,
            key_value,
        },
        ParsedStatement::DropTables { if_exists, names } => {
            BoundStatement::DropTables { if_exists, names }
        }
        ParsedStatement::CreateTable { name, columns } => BoundStatement::CreateTable {
            name,
            columns: columns.into_iter().map(bind_column).collect(),
        },
        ParsedStatement::TruncateTables { names } => BoundStatement::TruncateTables { names },
        ParsedStatement::Begin => BoundStatement::Begin,
        ParsedStatement::Commit => BoundStatement::Commit,
        ParsedStatement::Rollback => BoundStatement::Rollback,
        ParsedStatement::CopyFromStdin { table } => BoundStatement::CopyFromStdin { table },
        ParsedStatement::UpdateAddInt {
            table,
            column,
            addend,
            key_column,
            key_value,
        } => BoundStatement::UpdateAddInt {
            table,
            column,
            addend,
            key_column,
            key_value,
        },
        ParsedStatement::Insert {
            table,
            columns,
            values,
        } => BoundStatement::Insert {
            table,
            columns,
            values: values.into_iter().map(bind_expression).collect(),
        },
    }
}

fn bind_column(column: ColumnDef) -> Column {
    Column::new(column.name, bind_type(column.data_type))
}

fn bind_type(data_type: SqlTypeName) -> PgType {
    match data_type {
        SqlTypeName::Int4 => PgType::Int4,
        SqlTypeName::Int8 => PgType::Int8,
        SqlTypeName::Text => PgType::Varchar,
    }
}

fn bind_expression(expression: Literal) -> BoundExpression {
    match expression {
        Literal::Int(value) => BoundExpression::Int(value),
        Literal::Text(value) => BoundExpression::Text(value),
        Literal::Null => BoundExpression::Null,
        Literal::CurrentTimestamp => BoundExpression::CurrentTimestamp,
    }
}
