use crate::*;

pub struct CopyDatum {
    value: usize,
    byval: bool,
    value_len: usize,
    payload: Option<Box<[u8]>>,
}

impl CopyDatum {
    pub fn by_value(value: usize) -> Self {
        Self {
            value,
            byval: true,
            value_len: 0,
            payload: None,
        }
    }

    pub fn by_reference(payload: Vec<u8>) -> Self {
        let payload = payload.into_boxed_slice();
        Self {
            value: 0,
            byval: false,
            value_len: payload.len(),
            payload: Some(payload),
        }
    }
}

pub fn copy_text_line(table: &str, line: &str) -> Result<bool, String> {
    let line = line.trim_end_matches('\n').trim_end_matches('\r');
    if line == "\\." {
        return Ok(false);
    }

    let relation = relation_by_name(table)
        .ok_or_else(|| format!("relation \"{}\" does not exist", table.trim()))?;
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.len() != relation.columns.len() {
        return Err(format!(
            "COPY row for relation \"{}\" has {} fields but {} columns",
            relation.name,
            fields.len(),
            relation.columns.len()
        ));
    }

    let datums = fields
        .iter()
        .zip(&relation.columns)
        .map(|(field, column)| {
            if *field == "\\N" {
                Ok(None)
            } else {
                copy_text_field_to_datum(field, column.type_oid).map(Some)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    insert_copy_datums_for_relation(&relation, datums)
}

pub fn insert_copy_datums(table: &str, datums: Vec<Option<CopyDatum>>) -> Result<bool, String> {
    let relation = relation_by_name(table)
        .ok_or_else(|| format!("relation \"{}\" does not exist", table.trim()))?;
    insert_copy_datums_for_relation(&relation, datums)
}

pub(crate) fn insert_copy_datums_for_relation(
    relation: &fastpg_catalog::RelationRecord,
    datums: Vec<Option<CopyDatum>>,
) -> Result<bool, String> {
    if datums.len() != relation.columns.len() {
        return Err(format!(
            "COPY row for relation \"{}\" has {} fields but {} columns",
            relation.name,
            datums.len(),
            relation.columns.len()
        ));
    }

    let mut values = Vec::with_capacity(relation.columns.len());
    let mut is_null = Vec::with_capacity(relation.columns.len());
    let mut byval = Vec::with_capacity(relation.columns.len());
    let mut value_lens = Vec::with_capacity(relation.columns.len());
    let mut byref_payloads = Vec::<Box<[u8]>>::new();

    for datum in datums {
        let Some(copy_value) = datum else {
            values.push(0);
            is_null.push(1);
            byval.push(0);
            value_lens.push(0);
            continue;
        };

        let CopyDatum {
            mut value,
            byval: datum_byval,
            value_len,
            payload,
        } = copy_value;
        if let Some(payload) = payload {
            value = payload.as_ptr() as usize;
            byref_payloads.push(payload);
        }
        values.push(value);
        is_null.push(0);
        byval.push(u8::from(datum_byval));
        value_lens.push(value_len);
    }

    let mut tid = 0u64;
    let inserted = unsafe {
        fastpg_storage2_relation_insert(
            relation.oid.0,
            values.as_ptr(),
            is_null.as_ptr(),
            byval.as_ptr(),
            value_lens.as_ptr(),
            relation.columns.len(),
            &mut tid,
        )
    };
    if inserted {
        Ok(true)
    } else {
        Err(format!(
            "failed to insert COPY row into \"{}\"",
            relation.name
        ))
    }
}

pub(crate) fn copy_text_field_to_datum(field: &str, type_oid: Oid) -> Result<CopyDatum, String> {
    match type_oid {
        INT2_OID => field
            .parse::<i16>()
            .map(|value| CopyDatum {
                value: value as usize,
                byval: true,
                value_len: 0,
                payload: None,
            })
            .map_err(|error| format!("invalid int2 literal {field:?}: {error}")),
        INT4_OID => field
            .parse::<i32>()
            .map(|value| CopyDatum {
                value: value as usize,
                byval: true,
                value_len: 0,
                payload: None,
            })
            .map_err(|error| format!("invalid int4 literal {field:?}: {error}")),
        INT8_OID => field
            .parse::<i64>()
            .map(|value| CopyDatum {
                value: value as usize,
                byval: true,
                value_len: 0,
                payload: None,
            })
            .map_err(|error| format!("invalid int8 literal {field:?}: {error}")),
        OID_OID => field
            .parse::<u32>()
            .map(|value| CopyDatum {
                value: value as usize,
                byval: true,
                value_len: 0,
                payload: None,
            })
            .map_err(|error| format!("invalid oid literal {field:?}: {error}")),
        TEXT_OID | BPCHAR_OID | VARCHAR_OID => {
            let decoded = decode_copy_text_field(field);
            let payload = postgres_text_payload(decoded.as_bytes());
            Ok(CopyDatum {
                value: 0,
                byval: false,
                value_len: payload.len(),
                payload: Some(payload),
            })
        }
        TIMESTAMP_OID => Ok(CopyDatum {
            value: 0,
            byval: true,
            value_len: 0,
            payload: None,
        }),
        other => Err(format!("COPY does not support type OID {}", other.0)),
    }
}

pub(crate) fn postgres_text_payload(value: &[u8]) -> Box<[u8]> {
    let len = (value.len() + 4) as u32;
    let header = if cfg!(target_endian = "little") {
        len << 2
    } else {
        len
    };
    let mut payload = Vec::with_capacity(value.len() + 4);
    payload.extend_from_slice(&header.to_ne_bytes());
    payload.extend_from_slice(value);
    payload.into_boxed_slice()
}

pub(crate) fn decode_copy_text_field(field: &str) -> String {
    let mut decoded = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            decoded.push(ch);
            continue;
        }

        match chars.next() {
            Some('b') => decoded.push('\u{0008}'),
            Some('f') => decoded.push('\u{000c}'),
            Some('n') => decoded.push('\n'),
            Some('r') => decoded.push('\r'),
            Some('t') => decoded.push('\t'),
            Some('\\') => decoded.push('\\'),
            Some(other) => decoded.push(other),
            None => decoded.push('\\'),
        }
    }
    decoded
}
