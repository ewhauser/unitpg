# Seed-Only Startup

## Summary

Treat the data directory as an immutable seed image in fast-fork mode. Each
postmaster start initializes an in-memory database from the seed catalog state
and ignores transient state left by previous runs. This builds on conservative
fast startup: after proving we can skip redo safely, remove more dependence on
durable shutdown/control-file state.

The seed image is the durable part of the test install. Runtime DDL, DML,
transaction status, relation storage changes, SLRUs, and fake WAL state are
ephemeral and must not be trusted after postmaster exit.

## Goals

- Make postmaster startup deterministic from a known seed image.
- Stop trusting prior shutdown state in `pg_control`, `pg_wal`, SLRU
  directories, and transient storage directories.
- Keep seed catalog relation files readable by `memsmgr`.
- Reset transient in-memory state on every start.
- Make dirty previous fast-fork exits irrelevant.
- Measure startup improvement with the startup benchmark.

## Non-Goals

- Removing the seed data directory.
- Loading all catalog pages into memory at executable start.
- Supporting crash recovery or restart persistence.
- Supporting standby/recovery/replication modes.
- Preserving user-created objects across postmaster restart.

## Build Flag

Add a new opt-in build option:

- Meson: `-Dtest_seed_only_startup=true`
- Autoconf: `--enable-test-seed-only-startup`
- C define: `USE_TEST_SEED_ONLY_STARTUP`

This option should require:

- `test_no_recovery_startup=true`
- `test_mem_smgr=true`
- `test_mem_slru=true`

## Seed Contract

The data directory has two categories of state.

Seed state:

- bootstrap catalog relation files
- relation map files needed for bootstrap catalogs
- control metadata needed to locate and identify the seed cluster
- configuration files

Transient state:

- `pg_wal`
- transaction-status SLRU contents
- multixact state
- commit timestamp state
- relation files created or modified after postmaster start
- statistics, replication slots, logical state, temporary files
- control-file fields describing the previous postmaster run

Seed-only startup may read seed state. It should ignore, reset, or recreate
transient state.

## Design

### Seed Metadata

Record a small seed manifest during `initdb` or first fast-fork setup:

```text
PG_VERSION
catalog_version_no
system_identifier
seed_next_oid
seed_next_xid
seed_next_multixact
seed_database_oid(s)
seed_tablespace mapping
```

The manifest can initially live as a small file in `global/` or be represented
by carefully chosen control-file fields. Prefer a separate test-only manifest if
it avoids overloading durable recovery metadata.

### Startup Initialization

At postmaster start:

1. Read seed manifest.
2. Validate catalog version and system identifier.
3. Initialize transaction counters from seed values.
4. Initialize in-memory SLRUs to the seed horizon.
5. Initialize `memsmgr` with no runtime pages.
6. Allow `memsmgr` to lazily read seed catalog pages from disk.
7. Ignore previous `pg_wal` and SLRU contents.
8. Start accepting connections.

This should not require a clean shutdown marker.

### Transient Directory Handling

Do not spend startup time cleaning every transient directory unless validation
shows stale files confuse existing code.

Prefer "ignore by construction":

- fake WAL code never reads previous WAL files
- in-memory SLRU code never reads previous SLRU files
- `memsmgr` uses runtime hash state for modified pages
- seed relation reads come only from known seed relation files

If a directory must exist for compatibility, create it cheaply without scanning
or validating old contents.

### Control File Writes

In seed-only mode, control-file writes should be minimized or eliminated during
normal start/stop. Fields that would normally describe the last durable
checkpoint are not meaningful.

If SQL functions expose those values, return stable seed/fake values or mark the
functions unsupported in fast-fork mode.

### Relation Storage

`memsmgr` already has the right broad shape:

- seed pages can be read lazily from existing relation files
- modified/new pages live in memory
- runtime state disappears on restart

Seed-only startup should make this contract explicit. Runtime relation files
created by code paths that still hit disk should either be avoided by
`memsmgr`, ignored on next start, or placed under a transient directory not
considered part of the seed.

## Correctness Requirements

- A seed cluster starts even if the previous fast-fork postmaster was stopped
  with fast/immediate mode.
- Runtime-created objects from the previous postmaster do not appear after
  restart.
- Seed catalogs and template databases remain readable.
- `CREATE DATABASE` either works through seed-compatible rules or is clearly
  unsupported in fast-fork validation.
- Fixture snapshot/restore works after seed-only startup.
- Existing fast-fork validation passes.

## Validation

Add targeted startup validation:

1. Start a seed-only cluster.
2. Create a table and insert rows.
3. Stop with `pg_ctl -m immediate stop` or kill the postmaster in a controlled
   test harness.
4. Start again.
5. Verify the runtime-created table is gone.
6. Verify seed catalogs and `SELECT 1` still work.

Run:

```sh
./test-fastfork.sh core --no-reconfigure
```

Run:

```sh
python3 bench/compare_startup.py \
  --rounds 10 \
  --mode reuse \
  --reuse-builds \
  --output-dir bench/results/seed-only-startup
```

Also run copy mode:

```sh
python3 bench/compare_startup.py \
  --rounds 10 \
  --mode copy \
  --reuse-builds \
  --output-dir bench/results/seed-only-startup-copy
```

The implementation is successful if:

- validation passes
- startup does not depend on previous shutdown cleanliness
- median startup improves over conservative fast startup or removes recovery
  variability

## Risks

- Existing PostgreSQL code may assume control-file checkpoint fields are
  meaningful. Replace those reads deliberately.
- Some seed relation files may be modified by paths that still use `md.c`.
  Audit all storage-manager routing under fast-fork flags.
- `CREATE DATABASE` copies template storage in normal Postgres. It may need a
  fast-fork-specific in-memory/template strategy or exclusion.
- Ignoring transient directories can hide bugs where code accidentally writes
  runtime state to the seed area. Add assertions when practical.
