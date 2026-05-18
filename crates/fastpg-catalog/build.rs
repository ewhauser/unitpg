use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug)]
struct BkiColumn {
    name: String,
    type_name: String,
}

#[derive(Clone, Debug)]
struct BkiTable {
    name: String,
    oid: u32,
    rowtype_oid: u32,
    columns: Vec<BkiColumn>,
    rows: Vec<Vec<Option<String>>>,
}

#[derive(Clone, Debug)]
struct SchemaColumn {
    name: String,
    type_oid: u32,
    attlen: i16,
    attnum: i16,
    attndims: i32,
    attbyval: bool,
    attalign: u8,
    attstorage: u8,
    attnotnull: bool,
    attcollation: u32,
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("fastpg-catalog should live under crates/")
        .to_path_buf();
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let generated_dir = out_dir.join("postgres-catalog");
    fs::create_dir_all(&generated_dir).expect("create generated catalog directory");

    let catalog_meson = repo_root.join("src/include/catalog/meson.build");
    let meson_root = repo_root.join("meson.build");
    let catalog_meson_contents =
        fs::read_to_string(&catalog_meson).expect("read src/include/catalog/meson.build");
    let headers = parse_meson_string_list(&catalog_meson_contents, "catalog_headers");
    let bki_data = parse_meson_string_list(&catalog_meson_contents, "bki_data");
    let major_version =
        parse_major_version(&fs::read_to_string(&meson_root).expect("read meson.build"));

    println!("cargo:rerun-if-changed={}", catalog_meson.display());
    println!("cargo:rerun-if-changed={}", meson_root.display());
    println!(
        "cargo:rerun-if-changed={}",
        repo_root.join("src/backend/catalog/genbki.pl").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        repo_root.join("src/backend/catalog/Catalog.pm").display()
    );
    for header in &headers {
        println!(
            "cargo:rerun-if-changed={}",
            repo_root.join("src/include/catalog").join(header).display()
        );
    }
    for dat in &bki_data {
        println!(
            "cargo:rerun-if-changed={}",
            repo_root.join("src/include/catalog").join(dat).display()
        );
    }

    let genbki_status = Command::new("perl")
        .arg(repo_root.join("src/backend/catalog/genbki.pl"))
        .arg(format!(
            "--include-path={}",
            repo_root.join("src/include").display()
        ))
        .arg(format!("--set-version={major_version}"))
        .arg(format!("--output={}", generated_dir.display()))
        .args(
            headers
                .iter()
                .map(|header| repo_root.join("src/include/catalog").join(header)),
        )
        .status()
        .expect("run PostgreSQL genbki.pl");
    assert!(genbki_status.success(), "PostgreSQL genbki.pl failed");

    let bki = fs::read_to_string(generated_dir.join("postgres.bki")).expect("read postgres.bki");
    let schema =
        fs::read_to_string(generated_dir.join("schemapg.h")).expect("read generated schemapg.h");
    let tables = parse_bki(&bki);
    let schema_columns = parse_schema(&schema);
    let rust = emit_static_catalog(&tables, &schema_columns);
    fs::write(out_dir.join("generated_static_catalog.rs"), rust)
        .expect("write generated static catalog Rust source");
}

fn parse_meson_string_list(contents: &str, name: &str) -> Vec<String> {
    let needle = format!("{name} = [");
    let start = contents
        .find(&needle)
        .unwrap_or_else(|| panic!("could not find {name} in catalog meson file"));
    let mut values = Vec::new();
    for line in contents[start + needle.len()..].lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(']') {
            break;
        }
        if let Some(value) = quoted_value(trimmed) {
            values.push(value.to_owned());
        }
    }
    values
}

fn quoted_value(value: &str) -> Option<&str> {
    let start = value.find('\'')?;
    let rest = &value[start + 1..];
    let end = rest.find('\'')?;
    Some(&rest[..end])
}

fn parse_major_version(contents: &str) -> String {
    let version_marker = "version: '";
    let start = contents
        .find(version_marker)
        .expect("root meson.build should declare a project version")
        + version_marker.len();
    let version = &contents[start..];
    let major = version
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    assert!(
        !major.is_empty(),
        "PostgreSQL major version should start with digits"
    );
    major
}

fn parse_bki(contents: &str) -> Vec<BkiTable> {
    let mut tables = Vec::new();
    let mut lines = contents.lines().peekable();
    let mut current: Option<BkiTable> = None;

    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.starts_with("create ") {
            if let Some(table) = current.take() {
                tables.push(table);
            }
            let mut pieces = trimmed.split_whitespace();
            let _create = pieces.next();
            let name = pieces.next().expect("catalog create name").to_owned();
            let oid = pieces
                .next()
                .expect("catalog create oid")
                .parse::<u32>()
                .expect("catalog oid should be numeric");
            let rowtype_oid = trimmed
                .split_whitespace()
                .collect::<Vec<_>>()
                .windows(2)
                .find_map(|window| {
                    (window[0] == "rowtype_oid")
                        .then(|| window[1].parse::<u32>().expect("rowtype oid"))
                })
                .unwrap_or(0);
            let mut columns = Vec::new();
            for column_line in lines.by_ref() {
                let column_line = column_line.trim();
                if column_line == "(" {
                    continue;
                }
                if column_line == ")" {
                    break;
                }
                let column_line = column_line.trim_end_matches(',');
                let mut parts = column_line.split_whitespace();
                let Some(column_name) = parts.next() else {
                    continue;
                };
                let Some("=") = parts.next() else {
                    continue;
                };
                let Some(type_name) = parts.next() else {
                    continue;
                };
                columns.push(BkiColumn {
                    name: column_name.to_owned(),
                    type_name: type_name.to_owned(),
                });
            }
            current = Some(BkiTable {
                name,
                oid,
                rowtype_oid,
                columns,
                rows: Vec::new(),
            });
        } else if trimmed.starts_with("insert ") {
            let table = current
                .as_mut()
                .expect("insert should appear inside an open catalog table");
            table.rows.push(parse_insert_values(trimmed));
        } else if trimmed.starts_with("close ")
            && let Some(table) = current.take()
        {
            tables.push(table);
        }
    }

    if let Some(table) = current.take() {
        tables.push(table);
    }
    tables
}

fn parse_insert_values(line: &str) -> Vec<Option<String>> {
    let start = line.find('(').expect("insert should contain '('");
    let end = line.rfind(')').expect("insert should contain ')'");
    tokenize_bki_values(&line[start + 1..end])
        .into_iter()
        .map(|value| (value != "_null_").then_some(value))
        .collect()
}

fn tokenize_bki_values(values: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut chars = values.chars().peekable();
    let mut in_quote = false;

    while let Some(ch) = chars.next() {
        if in_quote {
            match ch {
                '\'' => {
                    if chars.peek() == Some(&'\'') {
                        token.push('\'');
                        chars.next();
                    } else {
                        in_quote = false;
                    }
                }
                _ => token.push(ch),
            }
            continue;
        }

        match ch {
            '\'' => in_quote = true,
            ch if ch.is_whitespace() => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
            }
            _ => token.push(ch),
        }
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    tokens
}

fn parse_schema(contents: &str) -> BTreeMap<String, Vec<SchemaColumn>> {
    let mut schemas = BTreeMap::new();
    let mut current_name: Option<String> = None;

    for line in contents.lines() {
        let trimmed = line.trim();
        if let Some(name) = trimmed.strip_prefix("#define Schema_") {
            current_name = Some(name.trim().to_owned());
            schemas
                .entry(name.trim().to_owned())
                .or_insert_with(Vec::new);
            continue;
        }
        if !trimmed.starts_with('{') {
            continue;
        }
        let Some(table_name) = current_name.as_ref() else {
            continue;
        };
        let Some(column) = parse_schema_column(trimmed) else {
            continue;
        };
        schemas
            .entry(table_name.clone())
            .or_insert_with(Vec::new)
            .push(column);
    }

    schemas
}

fn parse_schema_column(line: &str) -> Option<SchemaColumn> {
    let line = line
        .trim_end_matches('\\')
        .trim_end_matches(',')
        .trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .trim();
    let fields = split_csvish(line);
    if fields.len() < 20 {
        return None;
    }
    Some(SchemaColumn {
        name: fields[1]
            .trim()
            .trim_start_matches('{')
            .trim_end_matches('}')
            .trim_matches('"')
            .to_owned(),
        type_oid: parse_u32(&fields[2]),
        attlen: parse_i16(&fields[3]),
        attnum: parse_i16(&fields[4]),
        attndims: parse_i32(&fields[6]),
        attbyval: parse_bool(&fields[7]),
        attalign: parse_char(&fields[8]),
        attstorage: parse_char(&fields[9]),
        attnotnull: parse_bool(&fields[11]),
        attcollation: parse_u32(&fields[19]),
    })
}

fn split_csvish(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut brace_depth = 0usize;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    for ch in line.chars() {
        match ch {
            '\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
                field.push(ch);
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
                field.push(ch);
            }
            '{' if !in_single_quote && !in_double_quote => {
                brace_depth += 1;
                field.push(ch);
            }
            '}' if !in_single_quote && !in_double_quote => {
                brace_depth = brace_depth.saturating_sub(1);
                field.push(ch);
            }
            ',' if brace_depth == 0 && !in_single_quote && !in_double_quote => {
                fields.push(field.trim().to_owned());
                field.clear();
            }
            _ => field.push(ch),
        }
    }
    if !field.trim().is_empty() {
        fields.push(field.trim().to_owned());
    }
    fields
}

fn parse_u32(value: &str) -> u32 {
    parse_i64(value).try_into().expect("u32 schema field")
}

fn parse_i16(value: &str) -> i16 {
    parse_i64(value).try_into().expect("i16 schema field")
}

fn parse_i32(value: &str) -> i32 {
    parse_i64(value).try_into().expect("i32 schema field")
}

fn parse_i64(value: &str) -> i64 {
    match value.trim() {
        "NAMEDATALEN" => 64,
        other => other.parse::<i64>().unwrap_or(0),
    }
}

fn parse_bool(value: &str) -> bool {
    match value.trim() {
        "true" => true,
        "false" => false,
        other => panic!("invalid schema bool {other}"),
    }
}

fn parse_char(value: &str) -> u8 {
    let value = value.trim();
    if value == "'\\0'" {
        return 0;
    }
    value
        .trim_matches('\'')
        .as_bytes()
        .first()
        .copied()
        .unwrap_or(0)
}

fn emit_static_catalog(
    tables: &[BkiTable],
    schema_columns: &BTreeMap<String, Vec<SchemaColumn>>,
) -> String {
    let mut out = String::new();
    let type_oid_by_name = type_oid_by_name(tables);

    out.push_str("// @generated by fastpg-catalog/build.rs\n");
    out.push_str("use super::*;\n\n");
    emit_raw_tables(&mut out, tables, schema_columns, &type_oid_by_name);
    emit_typed_types(&mut out, tables);
    emit_typed_procs(&mut out, tables);
    emit_typed_aggregates(&mut out, tables);
    emit_typed_operators(&mut out, tables);
    emit_typed_casts(&mut out, tables);
    emit_typed_namespaces(&mut out, tables);
    out
}

fn type_oid_by_name(tables: &[BkiTable]) -> BTreeMap<String, u32> {
    let mut map = BTreeMap::new();
    let pg_type = table(tables, "pg_type");
    let oid_index = column_index(pg_type, "oid");
    let name_index = column_index(pg_type, "typname");
    for row in &pg_type.rows {
        let Some(Some(name)) = row.get(name_index) else {
            continue;
        };
        let Some(Some(oid)) = row.get(oid_index) else {
            continue;
        };
        map.insert(name.clone(), parse_catalog_oid(oid).unwrap_or(0));
    }
    map
}

fn emit_raw_tables(
    out: &mut String,
    tables: &[BkiTable],
    schema_columns: &BTreeMap<String, Vec<SchemaColumn>>,
    type_oid_by_name: &BTreeMap<String, u32>,
) {
    out.push_str("pub const STATIC_CATALOG_TABLES: &[StaticCatalogTable] = &[\n");
    for table in tables {
        out.push_str("    StaticCatalogTable {\n");
        out.push_str(&format!("        oid: Oid({}),\n", table.oid));
        out.push_str(&format!(
            "        name: \"{}\",\n",
            rust_escape(&table.name)
        ));
        out.push_str(&format!(
            "        rowtype_oid: Oid({}),\n",
            table.rowtype_oid
        ));
        out.push_str("        columns: &[\n");
        let schema = schema_columns.get(&table.name);
        for column in &table.columns {
            let schema_column = schema.and_then(|columns| {
                columns
                    .iter()
                    .find(|schema_column| schema_column.name == column.name)
            });
            let type_oid = schema_column
                .map(|column| column.type_oid)
                .or_else(|| type_oid_by_name.get(&column.type_name).copied())
                .unwrap_or(0);
            let attlen = schema_column.map(|column| column.attlen).unwrap_or(0);
            let attnum = schema_column.map(|column| column.attnum).unwrap_or(0);
            let attndims = schema_column.map(|column| column.attndims).unwrap_or(0);
            let attbyval = schema_column
                .map(|column| column.attbyval)
                .unwrap_or(type_oid != 19 && type_oid != 25);
            let attalign = schema_column.map(|column| column.attalign).unwrap_or(b'i');
            let attstorage = schema_column
                .map(|column| column.attstorage)
                .unwrap_or(b'p');
            let attnotnull = schema_column
                .map(|column| column.attnotnull)
                .unwrap_or(false);
            let attcollation = schema_column.map(|column| column.attcollation).unwrap_or(0);
            out.push_str("            StaticCatalogColumn {\n");
            out.push_str(&format!(
                "                name: \"{}\",\n",
                rust_escape(&column.name)
            ));
            out.push_str(&format!(
                "                type_name: \"{}\",\n",
                rust_escape(&column.type_name)
            ));
            out.push_str(&format!("                type_oid: Oid({type_oid}),\n"));
            out.push_str(&format!("                attlen: {attlen},\n"));
            out.push_str(&format!("                attnum: {attnum},\n"));
            out.push_str(&format!("                attndims: {attndims},\n"));
            out.push_str(&format!("                attbyval: {attbyval},\n"));
            out.push_str(&format!(
                "                attalign: {},\n",
                byte_literal(attalign)
            ));
            out.push_str(&format!(
                "                attstorage: {},\n",
                byte_literal(attstorage)
            ));
            out.push_str(&format!("                attnotnull: {attnotnull},\n"));
            out.push_str(&format!(
                "                attcollation: Oid({attcollation}),\n"
            ));
            out.push_str("            },\n");
        }
        out.push_str("        ],\n");
        out.push_str("        rows: &[\n");
        let oid_index = table.columns.iter().position(|column| column.name == "oid");
        for (index, row) in table.rows.iter().enumerate() {
            let row_id = oid_index
                .and_then(|oid_index| row.get(oid_index))
                .and_then(|value| value.as_ref())
                .and_then(|value| parse_catalog_oid(value))
                .map(u64::from)
                .unwrap_or_else(|| ((table.oid as u64) << 32) | (index as u64 + 1));
            out.push_str("            StaticCatalogRow {\n");
            out.push_str(&format!("                row_id: {row_id},\n"));
            out.push_str("                values: &[\n");
            for value in row {
                match value {
                    Some(value) => out.push_str(&format!(
                        "                    StaticCatalogValue::Raw(\"{}\"),\n",
                        rust_escape(value)
                    )),
                    None => out.push_str("                    StaticCatalogValue::Null,\n"),
                }
            }
            out.push_str("                ],\n");
            out.push_str("            },\n");
        }
        out.push_str("        ],\n");
        out.push_str("    },\n");
    }
    out.push_str("];\n\n");

    out.push_str("pub const STATIC_VIRTUAL_CATALOGS: &[VirtualCatalogRecord] = &[\n");
    for table in tables {
        let policy = if matches!(
            table.name.as_str(),
            "pg_class" | "pg_attribute" | "pg_index" | "pg_constraint" | "pg_type"
        ) {
            "VirtualCatalogPolicy::Dynamic"
        } else if matches!(
            table.name.as_str(),
            "pg_proc" | "pg_aggregate" | "pg_namespace" | "pg_operator" | "pg_cast" | "pg_opclass"
        ) {
            "VirtualCatalogPolicy::Static"
        } else {
            "VirtualCatalogPolicy::Empty"
        };
        out.push_str(&format!(
            "    VirtualCatalogRecord {{ relation_oid: Oid({}), name: \"{}\", policy: {policy} }},\n",
            table.oid,
            rust_escape(&table.name)
        ));
    }
    out.push_str("];\n\n");
}

fn emit_typed_types(out: &mut String, tables: &[BkiTable]) {
    let pg_type = table(tables, "pg_type");
    out.push_str("pub const STATIC_TYPES: &[PgTypeRecord] = &[\n");
    for row in &pg_type.rows {
        out.push_str("    PgTypeRecord {\n");
        out.push_str(&format!(
            "        oid: Oid({}),\n",
            u32_value(pg_type, row, "oid")
        ));
        out.push_str(&format!(
            "        name: \"{}\",\n",
            rust_escape(str_value(pg_type, row, "typname"))
        ));
        out.push_str(&format!(
            "        namespace: Oid({}),\n",
            u32_value(pg_type, row, "typnamespace")
        ));
        out.push_str(&format!(
            "        owner: Oid({}),\n",
            u32_value(pg_type, row, "typowner")
        ));
        out.push_str(&format!(
            "        typlen: {},\n",
            i16_value(pg_type, row, "typlen")
        ));
        out.push_str(&format!(
            "        typbyval: {},\n",
            bool_value(pg_type, row, "typbyval")
        ));
        out.push_str(&format!(
            "        typalign: {},\n",
            byte_literal(char_value(pg_type, row, "typalign"))
        ));
        out.push_str(&format!(
            "        typdelim: {},\n",
            byte_literal(char_value(pg_type, row, "typdelim"))
        ));
        out.push_str(&format!(
            "        typinput: Oid({}),\n",
            u32_value(pg_type, row, "typinput")
        ));
        out.push_str(&format!(
            "        typoutput: Oid({}),\n",
            u32_value(pg_type, row, "typoutput")
        ));
        out.push_str(&format!(
            "        typreceive: Oid({}),\n",
            u32_value(pg_type, row, "typreceive")
        ));
        out.push_str(&format!(
            "        typsend: Oid({}),\n",
            u32_value(pg_type, row, "typsend")
        ));
        out.push_str(&format!(
            "        typmodin: Oid({}),\n",
            u32_value(pg_type, row, "typmodin")
        ));
        out.push_str(&format!(
            "        typmodout: Oid({}),\n",
            u32_value(pg_type, row, "typmodout")
        ));
        out.push_str(&format!(
            "        typisdefined: {},\n",
            bool_value(pg_type, row, "typisdefined")
        ));
        out.push_str(&format!(
            "        typtype: {},\n",
            byte_literal(char_value(pg_type, row, "typtype"))
        ));
        out.push_str(&format!(
            "        typcategory: {},\n",
            byte_literal(char_value(pg_type, row, "typcategory"))
        ));
        out.push_str(&format!(
            "        typispreferred: {},\n",
            bool_value(pg_type, row, "typispreferred")
        ));
        out.push_str(&format!(
            "        typrelid: Oid({}),\n",
            u32_value(pg_type, row, "typrelid")
        ));
        out.push_str(&format!(
            "        typelem: Oid({}),\n",
            u32_value(pg_type, row, "typelem")
        ));
        out.push_str(&format!(
            "        typarray: Oid({}),\n",
            u32_value(pg_type, row, "typarray")
        ));
        out.push_str(&format!(
            "        typbasetype: Oid({}),\n",
            u32_value(pg_type, row, "typbasetype")
        ));
        out.push_str(&format!(
            "        typtypmod: {},\n",
            i32_value(pg_type, row, "typtypmod")
        ));
        out.push_str(&format!(
            "        typcollation: Oid({}),\n",
            u32_value(pg_type, row, "typcollation")
        ));
        out.push_str(&format!(
            "        typsubscript: Oid({}),\n",
            u32_value(pg_type, row, "typsubscript")
        ));
        out.push_str(&format!(
            "        typstorage: {},\n",
            byte_literal(char_value(pg_type, row, "typstorage"))
        ));
        out.push_str("    },\n");
    }
    out.push_str("];\n\n");
}

fn emit_typed_procs(out: &mut String, tables: &[BkiTable]) {
    let pg_proc = table(tables, "pg_proc");
    let mut seen_arg_slices = BTreeSet::new();
    let mut arg_slice_names = Vec::new();
    for row in &pg_proc.rows {
        let arg_types = oid_vec_value(pg_proc, row, "proargtypes");
        if seen_arg_slices.insert(arg_types.clone()) {
            let name = format!("STATIC_PROC_ARGS_{}", arg_slice_names.len());
            out.push_str(&format!("const {name}: &[Oid] = &["));
            for oid in &arg_types {
                out.push_str(&format!("Oid({oid}), "));
            }
            out.push_str("];\n");
            arg_slice_names.push((arg_types, name));
        }
    }
    out.push('\n');
    out.push_str("pub const STATIC_PROCS: &[PgProcRecord] = &[\n");
    for row in &pg_proc.rows {
        let arg_types = oid_vec_value(pg_proc, row, "proargtypes");
        let arg_name = arg_slice_names
            .iter()
            .find(|(args, _)| args == &arg_types)
            .map(|(_, name)| name.as_str())
            .expect("proc arg slice should exist");
        out.push_str("    PgProcRecord {\n");
        out.push_str(&format!(
            "        oid: Oid({}),\n",
            u32_value(pg_proc, row, "oid")
        ));
        out.push_str(&format!(
            "        name: \"{}\",\n",
            rust_escape(str_value(pg_proc, row, "proname"))
        ));
        out.push_str(&format!(
            "        namespace: Oid({}),\n",
            u32_value(pg_proc, row, "pronamespace")
        ));
        out.push_str(&format!(
            "        owner: Oid({}),\n",
            u32_value(pg_proc, row, "proowner")
        ));
        out.push_str(&format!(
            "        language: Oid({}),\n",
            u32_value(pg_proc, row, "prolang")
        ));
        out.push_str(&format!(
            "        cost: {},\n",
            f32_value(pg_proc, row, "procost") as u32
        ));
        out.push_str(&format!(
            "        rows: {},\n",
            f32_value(pg_proc, row, "prorows") as u32
        ));
        out.push_str(&format!(
            "        variadic: Oid({}),\n",
            u32_value(pg_proc, row, "provariadic")
        ));
        out.push_str(&format!(
            "        support: Oid({}),\n",
            u32_value(pg_proc, row, "prosupport")
        ));
        out.push_str(&format!(
            "        kind: {},\n",
            byte_literal(char_value(pg_proc, row, "prokind"))
        ));
        out.push_str(&format!(
            "        security_definer: {},\n",
            bool_value(pg_proc, row, "prosecdef")
        ));
        out.push_str(&format!(
            "        leakproof: {},\n",
            bool_value(pg_proc, row, "proleakproof")
        ));
        out.push_str(&format!(
            "        strict: {},\n",
            bool_value(pg_proc, row, "proisstrict")
        ));
        out.push_str(&format!(
            "        returns_set: {},\n",
            bool_value(pg_proc, row, "proretset")
        ));
        out.push_str(&format!(
            "        volatility: {},\n",
            byte_literal(char_value(pg_proc, row, "provolatile"))
        ));
        out.push_str(&format!(
            "        parallel: {},\n",
            byte_literal(char_value(pg_proc, row, "proparallel"))
        ));
        out.push_str(&format!(
            "        return_type: Oid({}),\n",
            u32_value(pg_proc, row, "prorettype")
        ));
        out.push_str(&format!("        arg_types: {arg_name},\n"));
        out.push_str(&format!(
            "        arg_defaults: {},\n",
            u16_value(pg_proc, row, "pronargdefaults")
        ));
        out.push_str(&format!(
            "        source: \"{}\",\n",
            rust_escape(str_value(pg_proc, row, "prosrc"))
        ));
        out.push_str("    },\n");
    }
    out.push_str("];\n\n");
}

fn emit_typed_aggregates(out: &mut String, tables: &[BkiTable]) {
    let pg_aggregate = table(tables, "pg_aggregate");
    out.push_str("pub const STATIC_AGGREGATES: &[PgAggregateRecord] = &[\n");
    for row in &pg_aggregate.rows {
        out.push_str("    PgAggregateRecord {\n");
        out.push_str(&format!(
            "        function_oid: Oid({}),\n",
            u32_value(pg_aggregate, row, "aggfnoid")
        ));
        out.push_str(&format!(
            "        kind: {},\n",
            byte_literal(char_value(pg_aggregate, row, "aggkind"))
        ));
        out.push_str(&format!(
            "        direct_arg_count: {},\n",
            u16_value(pg_aggregate, row, "aggnumdirectargs")
        ));
        for (field, rust_field) in [
            ("aggtransfn", "transition_fn"),
            ("aggfinalfn", "final_fn"),
            ("aggcombinefn", "combine_fn"),
            ("aggserialfn", "serial_fn"),
            ("aggdeserialfn", "deserial_fn"),
            ("aggmtransfn", "moving_transition_fn"),
            ("aggminvtransfn", "moving_inverse_fn"),
            ("aggmfinalfn", "moving_final_fn"),
        ] {
            out.push_str(&format!(
                "        {rust_field}: Oid({}),\n",
                u32_value(pg_aggregate, row, field)
            ));
        }
        out.push_str(&format!(
            "        final_extra: {},\n",
            bool_value(pg_aggregate, row, "aggfinalextra")
        ));
        out.push_str(&format!(
            "        moving_final_extra: {},\n",
            bool_value(pg_aggregate, row, "aggmfinalextra")
        ));
        out.push_str(&format!(
            "        final_modify: {},\n",
            byte_literal(char_value(pg_aggregate, row, "aggfinalmodify"))
        ));
        out.push_str(&format!(
            "        moving_final_modify: {},\n",
            byte_literal(char_value(pg_aggregate, row, "aggmfinalmodify"))
        ));
        out.push_str(&format!(
            "        sort_operator: Oid({}),\n",
            u32_value(pg_aggregate, row, "aggsortop")
        ));
        out.push_str(&format!(
            "        transition_type: Oid({}),\n",
            u32_value(pg_aggregate, row, "aggtranstype")
        ));
        out.push_str(&format!(
            "        transition_space: {},\n",
            i32_value(pg_aggregate, row, "aggtransspace")
        ));
        out.push_str(&format!(
            "        moving_transition_type: Oid({}),\n",
            u32_value(pg_aggregate, row, "aggmtranstype")
        ));
        out.push_str(&format!(
            "        moving_transition_space: {},\n",
            i32_value(pg_aggregate, row, "aggmtransspace")
        ));
        out.push_str(&format!(
            "        init_value: {},\n",
            option_str(row_value(pg_aggregate, row, "agginitval"))
        ));
        out.push_str(&format!(
            "        moving_init_value: {},\n",
            option_str(row_value(pg_aggregate, row, "aggminitval"))
        ));
        out.push_str("    },\n");
    }
    out.push_str("];\n\n");
}

fn emit_typed_operators(out: &mut String, tables: &[BkiTable]) {
    let pg_operator = table(tables, "pg_operator");
    out.push_str("pub const STATIC_OPERATORS: &[PgOperatorRecord] = &[\n");
    for row in &pg_operator.rows {
        out.push_str("    PgOperatorRecord {\n");
        out.push_str(&format!(
            "        oid: Oid({}),\n",
            u32_value(pg_operator, row, "oid")
        ));
        out.push_str(&format!(
            "        name: \"{}\",\n",
            rust_escape(str_value(pg_operator, row, "oprname"))
        ));
        out.push_str(&format!(
            "        namespace: Oid({}),\n",
            u32_value(pg_operator, row, "oprnamespace")
        ));
        out.push_str(&format!(
            "        owner: Oid({}),\n",
            u32_value(pg_operator, row, "oprowner")
        ));
        out.push_str(&format!(
            "        kind: {},\n",
            byte_literal(char_value(pg_operator, row, "oprkind"))
        ));
        out.push_str(&format!(
            "        can_merge: {},\n",
            bool_value(pg_operator, row, "oprcanmerge")
        ));
        out.push_str(&format!(
            "        can_hash: {},\n",
            bool_value(pg_operator, row, "oprcanhash")
        ));
        out.push_str(&format!(
            "        left_type: Oid({}),\n",
            u32_value(pg_operator, row, "oprleft")
        ));
        out.push_str(&format!(
            "        right_type: Oid({}),\n",
            u32_value(pg_operator, row, "oprright")
        ));
        out.push_str(&format!(
            "        result_type: Oid({}),\n",
            u32_value(pg_operator, row, "oprresult")
        ));
        out.push_str(&format!(
            "        commutator: Oid({}),\n",
            u32_value(pg_operator, row, "oprcom")
        ));
        out.push_str(&format!(
            "        negator: Oid({}),\n",
            u32_value(pg_operator, row, "oprnegate")
        ));
        out.push_str(&format!(
            "        code: Oid({}),\n",
            u32_value(pg_operator, row, "oprcode")
        ));
        out.push_str(&format!(
            "        rest: Oid({}),\n",
            u32_value(pg_operator, row, "oprrest")
        ));
        out.push_str(&format!(
            "        join: Oid({}),\n",
            u32_value(pg_operator, row, "oprjoin")
        ));
        out.push_str("    },\n");
    }
    out.push_str("];\n\n");
}

fn emit_typed_casts(out: &mut String, tables: &[BkiTable]) {
    let pg_cast = table(tables, "pg_cast");
    out.push_str("pub const STATIC_CASTS: &[PgCastRecord] = &[\n");
    for row in &pg_cast.rows {
        out.push_str("    PgCastRecord {\n");
        out.push_str(&format!(
            "        oid: Oid({}),\n",
            u32_value(pg_cast, row, "oid")
        ));
        out.push_str(&format!(
            "        source_type: Oid({}),\n",
            u32_value(pg_cast, row, "castsource")
        ));
        out.push_str(&format!(
            "        target_type: Oid({}),\n",
            u32_value(pg_cast, row, "casttarget")
        ));
        out.push_str(&format!(
            "        function: Oid({}),\n",
            u32_value(pg_cast, row, "castfunc")
        ));
        out.push_str(&format!(
            "        context: {},\n",
            byte_literal(char_value(pg_cast, row, "castcontext"))
        ));
        out.push_str(&format!(
            "        method: {},\n",
            byte_literal(char_value(pg_cast, row, "castmethod"))
        ));
        out.push_str("    },\n");
    }
    out.push_str("];\n\n");
}

fn emit_typed_namespaces(out: &mut String, tables: &[BkiTable]) {
    let pg_namespace = table(tables, "pg_namespace");
    out.push_str("pub const STATIC_NAMESPACES: &[PgNamespaceRecord] = &[\n");
    for row in &pg_namespace.rows {
        out.push_str("    PgNamespaceRecord {\n");
        out.push_str(&format!(
            "        oid: Oid({}),\n",
            u32_value(pg_namespace, row, "oid")
        ));
        out.push_str(&format!(
            "        name: \"{}\",\n",
            rust_escape(str_value(pg_namespace, row, "nspname"))
        ));
        out.push_str(&format!(
            "        owner: Oid({}),\n",
            u32_value(pg_namespace, row, "nspowner")
        ));
        out.push_str("    },\n");
    }
    out.push_str("];\n\n");
}

fn table<'a>(tables: &'a [BkiTable], name: &str) -> &'a BkiTable {
    tables
        .iter()
        .find(|table| table.name == name)
        .unwrap_or_else(|| panic!("missing catalog table {name}"))
}

fn column_index(table: &BkiTable, name: &str) -> usize {
    table
        .columns
        .iter()
        .position(|column| column.name == name)
        .unwrap_or_else(|| panic!("missing column {}.{}", table.name, name))
}

fn row_value<'a>(table: &'a BkiTable, row: &'a [Option<String>], column: &str) -> Option<&'a str> {
    row.get(column_index(table, column))
        .and_then(|value| value.as_deref())
}

fn str_value<'a>(table: &'a BkiTable, row: &'a [Option<String>], column: &str) -> &'a str {
    row_value(table, row, column).unwrap_or("")
}

fn u32_value(table: &BkiTable, row: &[Option<String>], column: &str) -> u32 {
    row_value(table, row, column)
        .and_then(parse_catalog_oid)
        .unwrap_or(0)
}

fn u16_value(table: &BkiTable, row: &[Option<String>], column: &str) -> u16 {
    i64_value(table, row, column)
        .try_into()
        .expect("u16 catalog value")
}

fn i16_value(table: &BkiTable, row: &[Option<String>], column: &str) -> i16 {
    i64_value(table, row, column)
        .try_into()
        .expect("i16 catalog value")
}

fn i32_value(table: &BkiTable, row: &[Option<String>], column: &str) -> i32 {
    i64_value(table, row, column)
        .try_into()
        .expect("i32 catalog value")
}

fn i64_value(table: &BkiTable, row: &[Option<String>], column: &str) -> i64 {
    parse_catalog_i64(str_value(table, row, column)).unwrap_or(0)
}

fn f32_value(table: &BkiTable, row: &[Option<String>], column: &str) -> f32 {
    str_value(table, row, column).parse::<f32>().unwrap_or(0.0)
}

fn bool_value(table: &BkiTable, row: &[Option<String>], column: &str) -> bool {
    match str_value(table, row, column) {
        "t" | "true" => true,
        "f" | "false" | "" => false,
        other => panic!("invalid catalog bool {other} for {}.{}", table.name, column),
    }
}

fn char_value(table: &BkiTable, row: &[Option<String>], column: &str) -> u8 {
    catalog_char(str_value(table, row, column))
}

fn oid_vec_value(table: &BkiTable, row: &[Option<String>], column: &str) -> Vec<u32> {
    str_value(table, row, column)
        .split_whitespace()
        .filter_map(parse_catalog_oid)
        .collect()
}

fn parse_catalog_oid(value: &str) -> Option<u32> {
    parse_catalog_i64(value).and_then(|value| u32::try_from(value).ok())
}

fn parse_catalog_i64(value: &str) -> Option<i64> {
    match value {
        "" | "-" => Some(0),
        "NAMEDATALEN" => Some(64),
        other => other.parse::<i64>().ok(),
    }
}

fn catalog_char(value: &str) -> u8 {
    if value == "\\0" {
        return 0;
    }
    value.as_bytes().first().copied().unwrap_or(0)
}

fn option_str(value: Option<&str>) -> String {
    value
        .map(|value| format!("Some(\"{}\")", rust_escape(value)))
        .unwrap_or_else(|| "None".to_owned())
}

fn rust_escape(value: &str) -> String {
    value.escape_default().to_string()
}

fn byte_literal(value: u8) -> String {
    value.to_string()
}
