use std::collections::BTreeMap;
use std::hint::black_box;
use std::sync::atomic::{AtomicU32, Ordering};

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use fastpg_catalog::{relation_by_name, static_catalog_by_name, upsert_catalog_row};
use fastpg_storage::{
    fastpg_rust_catalog_primary_key_index_oid, fastpg_rust_fetch_row,
    fastpg_rust_primary_key_index_lookup, fastpg_rust_relation_clear, fastpg_rust_relation_insert,
    fastpg_rust_relation_row_count, fastpg_rust_scan_begin, fastpg_rust_scan_end,
    fastpg_rust_scan_next, fastpg_rust_storage_reset_limits, fastpg_rust_subxact_abort,
    fastpg_rust_subxact_begin, fastpg_rust_xact_abort, fastpg_rust_xact_begin,
    fastpg_rust_xact_commit, fastpg_rust_xact_commit_if_implicit,
};

static NEXT_RELID: AtomicU32 = AtomicU32::new(1_000_000);
static NEXT_CATALOG_RELATION: AtomicU32 = AtomicU32::new(60_000);

fn next_relid() -> u32 {
    NEXT_RELID.fetch_add(1, Ordering::Relaxed)
}

fn reset_storage(relid: u32) {
    fastpg_rust_storage_reset_limits();
    fastpg_rust_xact_abort();
    fastpg_rust_relation_clear(relid);
}

fn insert_byval_row(relid: u32, value: usize) -> u64 {
    let values = [value];
    let nulls = [0u8];
    let byval = [1u8];
    let value_lens = [0usize];
    let mut row_id = 0;
    let inserted = unsafe {
        fastpg_rust_relation_insert(
            relid,
            values.as_ptr(),
            nulls.as_ptr(),
            byval.as_ptr(),
            value_lens.as_ptr(),
            values.len(),
            &mut row_id,
        )
    };
    assert!(inserted);
    row_id
}

fn insert_byref_row(relid: u32, payload: &[u8]) -> u64 {
    let values = [payload.as_ptr() as usize];
    let nulls = [0u8];
    let byval = [0u8];
    let value_lens = [payload.len()];
    let mut row_id = 0;
    let inserted = unsafe {
        fastpg_rust_relation_insert(
            relid,
            values.as_ptr(),
            nulls.as_ptr(),
            byval.as_ptr(),
            value_lens.as_ptr(),
            values.len(),
            &mut row_id,
        )
    };
    assert!(inserted);
    row_id
}

fn upsert_named_catalog_row(table_name: &str, row_id: u64, values: &[(&str, String)]) -> u64 {
    let table = static_catalog_by_name(table_name).expect("catalog table");
    let values = values
        .iter()
        .map(|(column, value)| (*column, value.clone()))
        .collect::<BTreeMap<_, _>>();
    let row = table
        .columns
        .iter()
        .map(|column| values.get(column.name).cloned())
        .collect::<Vec<_>>();
    upsert_catalog_row(table.oid, row_id, row).expect("upsert catalog row")
}

fn install_primary_key_catalog(name: &str) -> u32 {
    let relid = NEXT_CATALOG_RELATION.fetch_add(10, Ordering::Relaxed);
    let type_oid = relid + 1;
    let index_oid = relid + 2;
    let constraint_oid = relid + 3;
    let index_name = format!("{name}_pkey");

    upsert_named_catalog_row(
        "pg_class",
        relid as u64,
        &[
            ("oid", relid.to_string()),
            ("relname", name.to_owned()),
            ("relnamespace", "2200".to_owned()),
            ("reltype", type_oid.to_string()),
            ("relowner", "10".to_owned()),
            ("relam", "2".to_owned()),
            ("relfilenode", relid.to_string()),
            ("relhasindex", "t".to_owned()),
            ("relpersistence", "p".to_owned()),
            ("relkind", "r".to_owned()),
            ("relnatts", "2".to_owned()),
        ],
    );
    upsert_named_catalog_row(
        "pg_type",
        type_oid as u64,
        &[
            ("oid", type_oid.to_string()),
            ("typname", name.to_owned()),
            ("typnamespace", "2200".to_owned()),
            ("typowner", "10".to_owned()),
            ("typlen", "-1".to_owned()),
            ("typbyval", "f".to_owned()),
            ("typtype", "c".to_owned()),
            ("typcategory", "C".to_owned()),
            ("typisdefined", "t".to_owned()),
            ("typdelim", ",".to_owned()),
            ("typrelid", relid.to_string()),
            ("typalign", "d".to_owned()),
            ("typstorage", "x".to_owned()),
        ],
    );
    for (attnum, attname, attnotnull) in [(1, "id", "t"), (2, "value", "f")] {
        upsert_named_catalog_row(
            "pg_attribute",
            0,
            &[
                ("attrelid", relid.to_string()),
                ("attname", attname.to_owned()),
                ("atttypid", "23".to_owned()),
                ("attlen", "4".to_owned()),
                ("attnum", attnum.to_string()),
                ("atttypmod", "-1".to_owned()),
                ("attbyval", "t".to_owned()),
                ("attalign", "i".to_owned()),
                ("attstorage", "p".to_owned()),
                ("attnotnull", attnotnull.to_owned()),
                ("attisdropped", "f".to_owned()),
            ],
        );
    }
    upsert_named_catalog_row(
        "pg_class",
        index_oid as u64,
        &[
            ("oid", index_oid.to_string()),
            ("relname", index_name.clone()),
            ("relnamespace", "2200".to_owned()),
            ("reltype", "0".to_owned()),
            ("relowner", "10".to_owned()),
            ("relam", "403".to_owned()),
            ("relfilenode", index_oid.to_string()),
            ("relhasindex", "f".to_owned()),
            ("relpersistence", "p".to_owned()),
            ("relkind", "i".to_owned()),
            ("relnatts", "1".to_owned()),
        ],
    );
    upsert_named_catalog_row(
        "pg_attribute",
        0,
        &[
            ("attrelid", index_oid.to_string()),
            ("attname", "id".to_owned()),
            ("atttypid", "23".to_owned()),
            ("attlen", "4".to_owned()),
            ("attnum", "1".to_owned()),
            ("atttypmod", "-1".to_owned()),
            ("attbyval", "t".to_owned()),
            ("attalign", "i".to_owned()),
            ("attstorage", "p".to_owned()),
            ("attnotnull", "t".to_owned()),
            ("attisdropped", "f".to_owned()),
        ],
    );
    upsert_named_catalog_row(
        "pg_index",
        index_oid as u64,
        &[
            ("indexrelid", index_oid.to_string()),
            ("indrelid", relid.to_string()),
            ("indnatts", "1".to_owned()),
            ("indnkeyatts", "1".to_owned()),
            ("indisunique", "t".to_owned()),
            ("indisprimary", "t".to_owned()),
            ("indisvalid", "t".to_owned()),
            ("indisready", "t".to_owned()),
            ("indislive", "t".to_owned()),
            ("indkey", "1".to_owned()),
        ],
    );
    upsert_named_catalog_row(
        "pg_constraint",
        constraint_oid as u64,
        &[
            ("oid", constraint_oid.to_string()),
            ("conname", index_name),
            ("connamespace", "2200".to_owned()),
            ("contype", "p".to_owned()),
            ("conrelid", relid.to_string()),
            ("conindid", index_oid.to_string()),
            ("conkey", "1".to_owned()),
        ],
    );
    fastpg_rust_xact_commit_if_implicit();
    relid
}

fn insert_byval_rows(relid: u32, rows: usize) {
    for value in 0..rows {
        black_box(insert_byval_row(relid, value));
    }
}

fn insert_byref_rows(relid: u32, rows: usize) {
    for value in 0..rows {
        let payload = format!("criterion-storage-row-{value:08}");
        black_box(insert_byref_row(relid, payload.as_bytes()));
    }
}

fn seed_byval_relation(relid: u32, rows: usize) {
    reset_storage(relid);
    fastpg_rust_xact_begin();
    insert_byval_rows(relid, rows);
    fastpg_rust_xact_commit();
}

fn scan_all_rows(relid: u32, natts: usize) -> usize {
    let scan = fastpg_rust_scan_begin(relid);
    assert_ne!(scan, 0);

    let mut values = vec![0usize; natts];
    let mut nulls = vec![0u8; natts];
    let mut row_id = 0u64;
    let mut rows = 0usize;
    loop {
        let found = unsafe {
            fastpg_rust_scan_next(
                scan,
                1,
                values.as_mut_ptr(),
                nulls.as_mut_ptr(),
                natts,
                &mut row_id,
            )
        };
        if !found {
            break;
        }
        rows += 1;
        black_box(row_id);
        black_box(&values);
        black_box(&nulls);
    }
    fastpg_rust_scan_end(scan);
    rows
}

fn storage_insert_commit(c: &mut Criterion) {
    let mut group = c.benchmark_group("storage/insert_commit");
    for rows in [1usize, 30, 500] {
        let relid = next_relid();
        group.bench_with_input(BenchmarkId::new("byval", rows), &rows, |b, &rows| {
            b.iter_batched(
                || reset_storage(relid),
                |_| {
                    fastpg_rust_xact_begin();
                    insert_byval_rows(relid, rows);
                    fastpg_rust_xact_commit();
                    black_box(fastpg_rust_relation_row_count(relid));
                },
                BatchSize::SmallInput,
            );
        });

        let relid = next_relid();
        group.bench_with_input(BenchmarkId::new("byref", rows), &rows, |b, &rows| {
            b.iter_batched(
                || reset_storage(relid),
                |_| {
                    fastpg_rust_xact_begin();
                    insert_byref_rows(relid, rows);
                    fastpg_rust_xact_commit();
                    black_box(fastpg_rust_relation_row_count(relid));
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn storage_rollback(c: &mut Criterion) {
    let mut group = c.benchmark_group("storage/rollback");
    for rows in [1usize, 500] {
        let relid = next_relid();
        group.bench_with_input(
            BenchmarkId::new("transaction_byref", rows),
            &rows,
            |b, &rows| {
                b.iter_batched(
                    || reset_storage(relid),
                    |_| {
                        fastpg_rust_xact_begin();
                        insert_byref_rows(relid, rows);
                        fastpg_rust_xact_abort();
                        black_box(fastpg_rust_relation_row_count(relid));
                    },
                    BatchSize::SmallInput,
                );
            },
        );

        let relid = next_relid();
        group.bench_with_input(
            BenchmarkId::new("savepoint_byref", rows),
            &rows,
            |b, &rows| {
                b.iter_batched(
                    || reset_storage(relid),
                    |_| {
                        fastpg_rust_xact_begin();
                        black_box(insert_byref_row(relid, b"parent-row"));
                        fastpg_rust_subxact_begin();
                        insert_byref_rows(relid, rows);
                        fastpg_rust_subxact_abort();
                        fastpg_rust_xact_commit();
                        black_box(fastpg_rust_relation_row_count(relid));
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

fn storage_scan(c: &mut Criterion) {
    let mut group = c.benchmark_group("storage/scan");
    for rows in [30usize, 500] {
        let relid = next_relid();
        seed_byval_relation(relid, rows);
        group.bench_with_input(
            BenchmarkId::new("materialized_byval", rows),
            &rows,
            |b, &rows| {
                b.iter(|| {
                    let scanned = scan_all_rows(relid, 1);
                    assert_eq!(scanned, rows);
                    black_box(scanned);
                });
            },
        );
        fastpg_rust_relation_clear(relid);
    }
    group.finish();
}

fn storage_fetch(c: &mut Criterion) {
    let mut group = c.benchmark_group("storage/fetch");
    for rows in [30usize, 500] {
        let relid = next_relid();
        seed_byval_relation(relid, rows);
        let row_id = (rows / 2 + 1) as u64;
        group.bench_with_input(BenchmarkId::new("by_row_id", rows), &rows, |b, _| {
            b.iter(|| {
                let mut values = [0usize; 1];
                let mut nulls = [0u8; 1];
                let found = unsafe {
                    fastpg_rust_fetch_row(
                        relid,
                        row_id,
                        values.as_mut_ptr(),
                        nulls.as_mut_ptr(),
                        values.len(),
                    )
                };
                assert!(found);
                black_box(values);
                black_box(nulls);
            });
        });
        fastpg_rust_relation_clear(relid);
    }
    group.finish();
}

fn seed_primary_key_relation(rows: usize) -> (u32, u32) {
    let name = format!(
        "criterion_pk_{}",
        NEXT_CATALOG_RELATION.fetch_add(1, Ordering::Relaxed)
    );
    let relid = install_primary_key_catalog(&name);
    let relation = relation_by_name(&name).unwrap();
    assert_eq!(relation.oid.0, relid);
    let mut index_oid = 0u32;
    let found_index = unsafe { fastpg_rust_catalog_primary_key_index_oid(relid, &mut index_oid) };
    assert!(found_index);

    reset_storage(relid);
    fastpg_rust_xact_begin();
    for value in 0..rows {
        let values = [value + 1, (value + 1) * 10];
        let nulls = [0u8, 0];
        let byval = [1u8, 1];
        let value_lens = [0usize, 0];
        let mut row_id = 0;
        let inserted = unsafe {
            fastpg_rust_relation_insert(
                relid,
                values.as_ptr(),
                nulls.as_ptr(),
                byval.as_ptr(),
                value_lens.as_ptr(),
                values.len(),
                &mut row_id,
            )
        };
        assert!(inserted);
    }
    fastpg_rust_xact_commit();
    (relid, index_oid)
}

fn storage_primary_key(c: &mut Criterion) {
    let mut group = c.benchmark_group("storage/primary_key");
    for rows in [30usize, 500] {
        let (relid, index_oid) = seed_primary_key_relation(rows);
        let key = [rows / 2 + 1];
        let nulls = [0u8];
        group.bench_with_input(BenchmarkId::new("lookup", rows), &rows, |b, _| {
            b.iter(|| {
                let mut row_id = 0u64;
                let found = unsafe {
                    fastpg_rust_primary_key_index_lookup(
                        index_oid,
                        key.as_ptr(),
                        nulls.as_ptr(),
                        key.len(),
                        &mut row_id,
                    )
                };
                assert!(found);
                black_box(row_id);
            });
        });
        fastpg_rust_relation_clear(relid);
    }
    group.finish();
}

criterion_group!(
    benches,
    storage_insert_commit,
    storage_rollback,
    storage_scan,
    storage_fetch,
    storage_primary_key
);
criterion_main!(benches);
