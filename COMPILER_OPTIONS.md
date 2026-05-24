# Fork Compiler Options

This file tracks compiler options added by this fork. It intentionally does
not document upstream PostgreSQL build options.

| Meson option | Type / values | Default | Generated macro(s) | What it does |
| --- | --- | --- | --- | --- |
| `fastpg` | boolean | `true` | `USE_FASTPG`, `FASTPG_NOOP_PGSTAT` | Enables the FastPG Rust-backed integration hooks in the PostgreSQL backend build. Also disables pgstat work that the Rust-server path does not use. |
| `fastpg_catalog_mode` | `postgres`, `rust` | `postgres` | `FASTPG_USE_RUST_CATALOG` when `fastpg=true` and value is `rust` | Selects the catalog implementation used by FastPG builds. `postgres` uses the embedded PostgreSQL catalog path; `rust` uses the Rust catalog path. |
| `fastpg_postgres_smgr` | `md`, `mem` | `mem` | `FASTPG_USE_MEM_SMGR` when `fastpg=true` and value is `mem` | Selects the PostgreSQL storage-manager entry used by FastPG builds. `md` uses PostgreSQL's normal disk-backed smgr; `mem` uses FastPG's seed-backed in-memory overlay smgr. |
| `fastpg_use_mem_index_am` | boolean | `false` | `FASTPG_USE_MEM_INDEX_AM` when `fastpg=true` | Uses the FastPG in-memory index access method for eligible PostgreSQL-catalog indexes. |
| `fastpg_skip_recovery_startup` | boolean | `false` | `FASTPG_SKIP_RECOVERY_STARTUP` when `fastpg=true` | Skips WAL recovery during startup for disposable Rust-server catalog runs. This is for benchmark/test PGDATA only, not durable PostgreSQL clusters. |

Runtime selectors such as `FASTPG_STORAGE_ENGINE`, `FASTPG_PGDATA`, and
`FASTPG_PGDATA_SEED` are intentionally not listed here because they are not
PostgreSQL compiler options.
