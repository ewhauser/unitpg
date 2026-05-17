#![forbid(unsafe_code)]

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlText(pub String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseError {
    pub query: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParsedStatement {
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
        columns: Vec<ColumnDef>,
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
        values: Vec<Literal>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: SqlTypeName,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SqlTypeName {
    Int4,
    Int8,
    Text,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Literal {
    Int(i64),
    Text(String),
    Null,
    CurrentTimestamp,
}

pub fn parse(sql: &str) -> Result<ParsedStatement, ParseError> {
    let query = trim_sql(sql);
    if query.is_empty() {
        return Err(ParseError {
            query: sql.to_owned(),
        });
    }

    parse_exact_query(query)
        .or_else(|| parse_transaction(query))
        .or_else(|| parse_drop_tables(query))
        .or_else(|| parse_create_table(query))
        .or_else(|| parse_truncate_tables(query))
        .or_else(|| parse_copy_from_stdin(query))
        .or_else(|| parse_select_count(query))
        .or_else(|| parse_select_column_where_int(query))
        .or_else(|| parse_update_add_int(query))
        .or_else(|| parse_insert(query))
        .ok_or_else(|| ParseError {
            query: sql.to_owned(),
        })
}

fn parse_exact_query(query: &str) -> Option<ParsedStatement> {
    match normalize_query(query).as_str() {
        "select 1" => Some(ParsedStatement::SelectOne),
        "show server_version" => Some(ParsedStatement::ShowServerVersion),
        "select $1::int4" => Some(ParsedStatement::SelectInt4Parameter),
        "select relkind from pg_catalog.pg_class where oid=$1::pg_catalog.regclass" => {
            Some(ParsedStatement::SelectRelkindByRegclassParameter)
        }
        normalized
            if normalized.starts_with(
                "select o.n, p.partstrat, pg_catalog.count(i.inhparent) from pg_catalog.pg_class as c",
            ) =>
        {
            Some(ParsedStatement::SelectPgbenchPartitionInfo)
        }
        _ => None,
    }
}

fn parse_transaction(query: &str) -> Option<ParsedStatement> {
    match normalize_query(query).as_str() {
        "begin" => Some(ParsedStatement::Begin),
        "commit" | "end" => Some(ParsedStatement::Commit),
        "rollback" => Some(ParsedStatement::Rollback),
        _ => None,
    }
}

fn parse_drop_tables(query: &str) -> Option<ParsedStatement> {
    let (if_exists, rest) = strip_prefix_ci(query, "drop table if exists ")
        .map(|rest| (true, rest))
        .or_else(|| strip_prefix_ci(query, "drop table ").map(|rest| (false, rest)))?;
    let names = split_top_level_commas(rest)
        .into_iter()
        .map(strip_identifier)
        .collect::<Vec<_>>();
    if names.is_empty() {
        return None;
    }
    Some(ParsedStatement::DropTables { if_exists, names })
}

fn parse_create_table(query: &str) -> Option<ParsedStatement> {
    let rest = strip_prefix_ci(query, "create table ")
        .or_else(|| strip_prefix_ci(query, "create unlogged table "))?;
    let open = rest.find('(')?;
    let name = strip_identifier(&rest[..open]);
    let close = find_matching_paren(rest, open)?;
    let column_text = &rest[(open + 1)..close];
    let columns = split_top_level_commas(column_text)
        .into_iter()
        .map(parse_column_def)
        .collect::<Option<Vec<_>>>()?;
    Some(ParsedStatement::CreateTable { name, columns })
}

fn parse_column_def(definition: &str) -> Option<ColumnDef> {
    let definition = definition.trim();
    let name_end = definition
        .char_indices()
        .find_map(|(idx, ch)| ch.is_whitespace().then_some(idx))?;
    let name = strip_identifier(&definition[..name_end]);
    let type_text = definition[name_end..].trim();
    let data_type = parse_type_name(type_text)?;
    Some(ColumnDef { name, data_type })
}

fn parse_type_name(type_text: &str) -> Option<SqlTypeName> {
    let normalized = normalize_query(type_text);
    if normalized.starts_with("bigint") {
        Some(SqlTypeName::Int8)
    } else if normalized.starts_with("int") || normalized.starts_with("integer") {
        Some(SqlTypeName::Int4)
    } else if normalized.starts_with("char")
        || normalized.starts_with("varchar")
        || normalized.starts_with("text")
        || normalized.starts_with("timestamp")
    {
        Some(SqlTypeName::Text)
    } else {
        None
    }
}

fn parse_truncate_tables(query: &str) -> Option<ParsedStatement> {
    let rest = strip_prefix_ci(query, "truncate table ")
        .or_else(|| strip_prefix_ci(query, "truncate "))?;
    let names = split_top_level_commas(rest)
        .into_iter()
        .map(strip_identifier)
        .collect::<Vec<_>>();
    if names.is_empty() {
        return None;
    }
    Some(ParsedStatement::TruncateTables { names })
}

fn parse_copy_from_stdin(query: &str) -> Option<ParsedStatement> {
    let rest = strip_prefix_ci(query, "copy ")?;
    let from_idx = find_keyword_ci(rest, " from stdin")?;
    let table = strip_identifier(&rest[..from_idx]);
    Some(ParsedStatement::CopyFromStdin { table })
}

fn parse_select_count(query: &str) -> Option<ParsedStatement> {
    let rest = strip_prefix_ci(query, "select count(*) from ")?;
    let table = strip_identifier(rest);
    Some(ParsedStatement::SelectCount { table })
}

fn parse_select_column_where_int(query: &str) -> Option<ParsedStatement> {
    let rest = strip_prefix_ci(query, "select ")?;
    let from_idx = find_keyword_ci(rest, " from ")?;
    let column = strip_identifier(&rest[..from_idx]);
    let after_from = &rest[(from_idx + " from ".len())..];
    let where_idx = find_keyword_ci(after_from, " where ")?;
    let table = strip_identifier(&after_from[..where_idx]);
    let condition = &after_from[(where_idx + " where ".len())..];
    let (key_column, key_value) = parse_int_equality(condition)?;
    Some(ParsedStatement::SelectColumnWhereInt {
        table,
        column,
        key_column,
        key_value,
    })
}

fn parse_update_add_int(query: &str) -> Option<ParsedStatement> {
    let rest = strip_prefix_ci(query, "update ")?;
    let set_idx = find_keyword_ci(rest, " set ")?;
    let table = strip_identifier(&rest[..set_idx]);
    let after_set = &rest[(set_idx + " set ".len())..];
    let where_idx = find_keyword_ci(after_set, " where ")?;
    let assignment = &after_set[..where_idx];
    let condition = &after_set[(where_idx + " where ".len())..];
    let (column_text, expression) = assignment.split_once('=')?;
    let column = strip_identifier(column_text);
    let add_idx = expression.find('+')?;
    let addend = expression[(add_idx + 1)..].trim().parse::<i64>().ok()?;
    let (key_column, key_value) = parse_int_equality(condition)?;
    Some(ParsedStatement::UpdateAddInt {
        table,
        column,
        addend,
        key_column,
        key_value,
    })
}

fn parse_insert(query: &str) -> Option<ParsedStatement> {
    let rest = strip_prefix_ci(query, "insert into ")?;
    let values_idx = find_keyword_ci(rest, " values ")?;
    let target = rest[..values_idx].trim();
    let values = rest[(values_idx + " values ".len())..].trim();
    let (table, columns) = parse_insert_target(target)?;
    let open = values.find('(')?;
    let close = find_matching_paren(values, open)?;
    let values = split_top_level_commas(&values[(open + 1)..close])
        .into_iter()
        .map(parse_literal)
        .collect::<Option<Vec<_>>>()?;
    Some(ParsedStatement::Insert {
        table,
        columns,
        values,
    })
}

fn parse_insert_target(target: &str) -> Option<(String, Vec<String>)> {
    let Some(open) = target.find('(') else {
        return Some((strip_identifier(target), Vec::new()));
    };
    let close = find_matching_paren(target, open)?;
    let table = strip_identifier(&target[..open]);
    let columns = split_top_level_commas(&target[(open + 1)..close])
        .into_iter()
        .map(strip_identifier)
        .collect();
    Some((table, columns))
}

fn parse_literal(value: &str) -> Option<Literal> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("null") || value.eq_ignore_ascii_case("\\N") {
        Some(Literal::Null)
    } else if value.eq_ignore_ascii_case("current_timestamp") {
        Some(Literal::CurrentTimestamp)
    } else if let Some(unquoted) = unquote_string(value) {
        Some(Literal::Text(unquoted))
    } else {
        value.parse::<i64>().ok().map(Literal::Int)
    }
}

fn parse_int_equality(condition: &str) -> Option<(String, i64)> {
    let (key_column, key_value) = condition.split_once('=')?;
    Some((
        strip_identifier(key_column),
        key_value.trim().parse::<i64>().ok()?,
    ))
}

fn trim_sql(sql: &str) -> &str {
    sql.trim().trim_end_matches(';').trim()
}

fn normalize_query(sql: &str) -> String {
    sql.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn strip_prefix_ci<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        .then(|| &value[prefix.len()..])
}

fn find_keyword_ci(value: &str, keyword: &str) -> Option<usize> {
    value
        .to_ascii_lowercase()
        .find(&keyword.to_ascii_lowercase())
}

fn strip_identifier(value: &str) -> String {
    let value = value.trim();
    if let Some(unquoted) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        unquoted.replace("\"\"", "\"")
    } else {
        value.to_ascii_lowercase()
    }
}

fn unquote_string(value: &str) -> Option<String> {
    value
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
        .map(|value| value.replace("''", "'"))
}

fn find_matching_paren(value: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut chars = value.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        if idx < open {
            continue;
        }
        if in_string {
            if ch == '\'' {
                if chars.peek().is_some_and(|(_, next)| *next == '\'') {
                    chars.next();
                } else {
                    in_string = false;
                }
            }
            continue;
        }
        match ch {
            '\'' => in_string = true,
            '(' => depth += 1,
            ')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_top_level_commas(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut chars = value.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        if in_string {
            if ch == '\'' {
                if chars.peek().is_some_and(|(_, next)| *next == '\'') {
                    chars.next();
                } else {
                    in_string = false;
                }
            }
            continue;
        }
        match ch {
            '\'' => in_string = true,
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                let part = value[start..idx].trim();
                if !part.is_empty() {
                    parts.push(part);
                }
                start = idx + ch.len_utf8();
            }
            _ => {}
        }
    }
    let tail = value[start..].trim();
    if !tail.is_empty() {
        parts.push(tail);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pgbench_create_table() {
        let statement = parse(
            "create table pgbench_accounts(aid int not null,bid int,abalance int,filler char(84)) with (fillfactor=100)",
        )
        .unwrap();

        assert_eq!(
            statement,
            ParsedStatement::CreateTable {
                name: "pgbench_accounts".to_owned(),
                columns: vec![
                    ColumnDef {
                        name: "aid".to_owned(),
                        data_type: SqlTypeName::Int4,
                    },
                    ColumnDef {
                        name: "bid".to_owned(),
                        data_type: SqlTypeName::Int4,
                    },
                    ColumnDef {
                        name: "abalance".to_owned(),
                        data_type: SqlTypeName::Int4,
                    },
                    ColumnDef {
                        name: "filler".to_owned(),
                        data_type: SqlTypeName::Text,
                    },
                ],
            }
        );
    }

    #[test]
    fn parses_simple_update_statement() {
        let statement =
            parse("UPDATE pgbench_accounts SET abalance = abalance + -42 WHERE aid = 7;").unwrap();

        assert_eq!(
            statement,
            ParsedStatement::UpdateAddInt {
                table: "pgbench_accounts".to_owned(),
                column: "abalance".to_owned(),
                addend: -42,
                key_column: "aid".to_owned(),
                key_value: 7,
            }
        );
    }
}
