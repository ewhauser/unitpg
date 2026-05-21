# `spec/041-multithreaded-pgcore-state.md`

# Multithreaded pgcore State Checklist

## Summary

Make reused PostgreSQL C frontend, catalog, planner, and executor code safe to
enter concurrently from multiple Rust server client sessions.

fastpg does not use PostgreSQL heap storage, shared buffers, WAL, checkpointer,
or physical catalog persistence in the Rust-server path. User data and mutable
catalog data are owned by the Rust storage/catalog layers. That narrows the
threading work: we do not need to make the full PostgreSQL storage engine
thread-safe.

We do need to recreate PostgreSQL's backend-local process illusion inside one
Rust process. Upstream backend globals that are normally private because each
connection is a separate process must become per-session, per-backend,
per-execution, or explicitly synchronized.

This spec is the checklist for graduating from
`spec/029-single-pgcore-execution-lane.md`.

## Current Implementation Status

As of 2026-05-21, the Rust server path has moved off the single global pgcore
lane. Each pgwire session owns a dedicated backend execution thread, so one
session is serialized against itself while different sessions can overlap in
pgcore-backed execution.

The first implementation pass covers the crash-producing state classes seen in
the upstream regression inventory and concurrent client torture test:

- [x] Default Rust-server catalog mode is `postgres`.
- [x] pgwire execution enters pgcore through per-session backend execution.
- [x] Session cleanup for active COPY/executor state runs on the owning backend
      thread.
- [x] Memory-context roots and current memory context state used by pgcore are
      backend-local.
- [x] Error capture state used by pgcore is backend-local enough for concurrent
      errors and notices.
- [x] Resource owner and snapshot shell state is backend-local.
- [x] Transaction shell state used by the supported execution path is
      backend-local.
- [x] Namespace/search-path and database identity state used by the regression
      path is backend-local.
- [x] libpq destination method state and output routing are backend-local.
- [x] PostgreSQL exit callback stacks used in the embedded backend are
      backend-local.
- [x] Relcache/catcache rebuild and invalidation paths reached by the supported
      path are synchronized.
- [x] Dynahash active scan bookkeeping reached by relcache/syscache scans is
      protected from cross-thread scan corruption.
- [x] The regression harness builds the upstream `regress` dynamic library used
      by `test_setup`.
- [x] 2026-05-21 validation passed the Rust-server concurrency torture test for
      300 repeated runs with real overlapping pgcore execution.
- [x] 2026-05-21 upstream regression inventory completed all 245 scheduled
      cases with no FastPG failures, timeouts, server crashes, or harness
      failures. The only remaining stdout mismatch was `event_trigger_login`,
      which is a login event-trigger semantic gap.

The full upstream inventory still reports one output mismatch. That is tracked
as a semantic/unsupported-feature gap until proven otherwise; the threading
acceptance bar here is no FastPG failures, timeouts, server crashes, or harness
failures while preserving real concurrent execution.

## Goals

```text
allow overlapping pgcore-backed execution from multiple Rust client sessions
keep Rust storage and Rust catalog as the authoritative mutable state
isolate PostgreSQL backend-local state per logical backend/session
protect only truly shared state with synchronization
make every remaining shared PostgreSQL global intentional and documented
keep the concurrent harness as the correctness gate
avoid reintroducing one global pgcore execution lane as the final architecture
```

## Non-Goals

```text
do not make PostgreSQL heap/tableam storage thread-safe
do not make PostgreSQL shared buffers thread-safe for fastpg user data
do not restore WAL, CLOG, ProcArray MVCC, checkpointer, bgwriter, or vacuum
do not use PostgreSQL shared-memory IPC as the invalidation model
do not make arbitrary C extensions thread-safe
do not serialize all pgcore execution as the final fix
```

## Classification Rules

Every PostgreSQL global touched by the Rust-server pgcore path must be assigned
one of these dispositions:

```text
immutable_after_init
per_backend_state
per_session_state
per_execution_state
shared_synchronized
rust_owned_replacement
unused_guarded_out
```

Do not leave a global in an implicit "probably fine" state. If a value can be
written after startup, it is unsafe until classified.

## Out Of Scope Because Rust Storage Owns It

These areas should be guarded away, stubbed, or kept unused in the Rust-server
path instead of being made fully threaded:

- [ ] PostgreSQL heap page storage and heapam tuple persistence.
- [ ] PostgreSQL index page storage and physical btree/gin/gist/spgist/brin
      persistence.
- [ ] Shared buffers, buffer replacement, dirty page tracking, and local buffer
      physical storage.
- [ ] WAL record generation, WAL insertion, WAL replay, checkpoints, and crash
      recovery.
- [ ] CLOG/multixact/subtrans storage as durable PostgreSQL transaction logs.
- [ ] smgr/md file descriptor caches and physical relfilenode lifecycle.
- [ ] Autovacuum, vacuum storage cleanup, background writer, checkpointer, and
      production durability background workers.
- [ ] PostgreSQL ProcArray-backed MVCC visibility for user table tuples.

Required guardrail:

- [ ] Add assertions or no-internal-IPC/no-physical-storage guards for any
      Rust-server path that unexpectedly reaches these systems.
- [ ] Keep regression failures in this bucket separate from pgcore
      backend-state threading failures.

## P0: pgcore Entry And Backend State Model

- [ ] Introduce an explicit `PgBackendState` or equivalent logical backend
      object.
- [ ] Assign one logical backend to each pgwire client session.
- [ ] Add a C-accessible current-backend pointer, preferably TLS, installed by
      a Rust guard before every pgcore FFI call.
- [ ] Make the guard restore the previous backend pointer on normal return,
      PostgreSQL `ERROR`, Rust panic, and early abort.
- [ ] Make every pgcore C entry point assert that a current backend is
      installed.
- [ ] Forbid calls into pgcore C code from Rust threads without an active
      backend state.
- [ ] Decide whether one Rust client may run two simultaneous pgcore operations;
      if not, enforce one in-flight pgcore operation per session.
- [ ] Keep process-wide one-time PostgreSQL initialization under `pthread_once`.
- [ ] Ensure one-time initialization does not write mutable per-backend defaults
      that later sessions share accidentally.

Acceptance:

- [ ] Concurrent test proves `max_active > 1`.
- [ ] Concurrent test also proves each operation sees its own backend/session
      identity.
- [ ] Failure in one concurrent operation does not clear another operation's
      active backend.

## P0: fastpg pgcore Shim Globals

Current high-risk examples live in
`crates/fastpg-pgcore/c/pgcore_raw_parser.c`.

- [ ] Move active notice result/source text out of process globals and into
      per-execution state.
- [ ] Move active COPY OUT statement/context/buffer out of process globals and
      into per-execution state.
- [ ] Move notice capture state out of process globals and into per-backend or
      per-execution state.
- [ ] Remove global install/restore races around `emit_log_hook`.
- [ ] Remove global install/restore races around `PqCommMethods`.
- [ ] Ensure client-message hooks route through the current backend instead of
      a single active global result.
- [ ] Ensure parse, prepare, execute, COPY IN, COPY OUT, and result-free paths
      never share mutable scratch state across sessions.
- [ ] Audit every `static` variable in `pgcore_raw_parser.c` and classify it
      using this spec's classification rules.

Acceptance:

- [ ] Two concurrent errors produce two independent SQLSTATE/message/detail
      results.
- [ ] Concurrent COPY OUT streams cannot append chunks to the wrong statement.
- [ ] Concurrent NOTICE-producing statements cannot see each other's notices.

## P0: Memory Contexts And Error State

PostgreSQL memory allocation depends heavily on process globals such as
`CurrentMemoryContext`, `TopMemoryContext`, `ErrorContext`, `CacheMemoryContext`,
`TopTransactionContext`, `CurTransactionContext`, and `PortalContext`.

- [ ] Decide which memory context roots are per-process immutable and which are
      per-backend.
- [ ] Make `CurrentMemoryContext` backend-local or guarded by an active backend
      switch.
- [ ] Make `ErrorContext` backend-local enough that simultaneous errors cannot
      overwrite each other.
- [ ] Make `error_context_stack` backend-local.
- [ ] Make `TopTransactionContext` and `CurTransactionContext` backend-local.
- [ ] Make `PortalContext` backend-local or portal-owned.
- [ ] Audit palloc sites in the pgcore path that assume the current context is
      stable across nested calls.
- [ ] Ensure Rust-owned result copying does not retain pointers into
      per-execution PostgreSQL contexts after guard exit.
- [ ] Add debug assertions that context switches restore to the same backend's
      previous context.

Acceptance:

- [ ] Concurrent errors do not corrupt `ErrorContext` or error stacks.
- [ ] Concurrent query execution does not allocate into another backend's
      transaction or result context.
- [ ] A failed query resets only its own per-execution/per-transaction contexts.

## P0: Transaction, Resource Owner, And Snapshot Shell

Even though tuple storage is Rust-owned, PostgreSQL parser/planner/executor code
still expects transaction shell state.

- [ ] Make `CurrentTransactionState` backend-local.
- [ ] Make `CurrentResourceOwner`, `CurTransactionResourceOwner`, and
      `TopTransactionResourceOwner` backend-local.
- [ ] Make `ActiveSnapshot`, `RegisteredSnapshots`, `CatalogSnapshot`, and
      `FirstXactSnapshot` backend-local.
- [ ] Make command-counter state backend-local.
- [ ] Make combo-CID and subtransaction bookkeeping backend-local, or guard out
      any unused paths.
- [ ] Ensure `StartTransactionCommand`, `CommitTransactionCommand`, and
      `AbortCurrentTransaction` operate on the current logical backend.
- [ ] Ensure implicit transactions for single statements do not interact with
      another session's explicit transaction.
- [ ] Ensure Rust catalog/storage transaction callbacks are bound to the same
      logical backend as the PostgreSQL transaction shell.

Acceptance:

- [ ] Two clients can run explicit transactions concurrently without snapshot,
      resource-owner, or transaction-state interference.
- [ ] One client's rollback cannot pop another client's active snapshot.
- [ ] Regression cases containing `\c`, `BEGIN`, `COMMIT`, `ROLLBACK`, and
      savepoint-like behavior remain stable under concurrent clients.

## P0: Relcache, Syscache, Catcache, Typcache, And Dynahash

The current crash signature points here: concurrent execution has produced
`no hash_seq_search scan for hash table "Relcache by OID"`.

- [ ] Classify relcache state such as relation OID/name caches as shared,
      per-backend, or generationed read-only.
- [ ] Classify syscache and catcache state as shared synchronized,
      per-backend, or Rust-catalog-backed snapshots.
- [ ] Classify typcache and record-cache state.
- [ ] Remove or protect process-global dynahash active scan bookkeeping.
- [ ] Ensure cache invalidation is driven by Rust catalog generation, not
      PostgreSQL shared invalidation IPC.
- [ ] Ensure cache rebuilds cannot happen concurrently without synchronization.
- [ ] Ensure cache entries cannot hold pointers into another backend's memory
      contexts.
- [ ] Ensure cache callbacks do not mutate shared lists/hash tables without
      protection.
- [ ] Decide whether relcache/syscache/typcache are allowed to be shared
      read-mostly caches with locks or must be per-backend.

Acceptance:

- [ ] `rust_server_concurrency_torture` no longer hits relcache dynahash scan
      corruption.
- [ ] Concurrent DDL/DML smoke tests do not use stale catalog metadata.
- [ ] Catalog generation bump invalidates prepared/planned state deterministically.

## P0: Session Identity, Database, User, GUC, And Search Path

These are backend-local in upstream PostgreSQL.

- [ ] Make `MyDatabaseId`, `DatabasePath`, and database-name state backend-local.
- [ ] Make `MyBackendType`, `MyProcPid`, `MyProc`, and `MyProcNumber` safe for
      the Rust-server backend model.
- [ ] Make `SessionUserId`, `OuterUserId`, `CurrentUserId`, and
      `SecurityRestrictionContext` backend-local.
- [ ] Make role-related GUC state backend-local.
- [ ] Make `namespace_search_path`, `activeSearchPath`, `baseSearchPath`, and
      search-path cache state backend-local or generationed.
- [ ] Make temp namespace state such as `myTempNamespace` backend-local.
- [ ] Ensure `SET`, `SET ROLE`, `RESET`, and `\c` update only the current
      session.
- [ ] Ensure temp objects are owned by one logical backend/session.
- [ ] Ensure ParameterStatus reported over pgwire comes from Rust session state
      and matches any installed PostgreSQL GUC state.

Acceptance:

- [ ] Two clients can use different search paths concurrently.
- [ ] Two clients can use different current databases/users where supported.
- [ ] One client's temp table cannot appear in another client's namespace.
- [ ] `\c` in one regression case cannot corrupt another session's database
      identity.

## P1: Portals, Prepared Statements, And Plan Cache

- [ ] Make `PortalHashTable` backend-local or replace it with Rust
      session-owned portal maps.
- [ ] Make `TopPortalContext` backend-local.
- [ ] Tie prepared statement handles to a logical backend/session and catalog
      generation.
- [ ] Ensure prepared handles are not raw cross-thread C pointers unless all
      dereference happens under the owning backend guard.
- [ ] Make saved plans and plan cache entries generation-aware.
- [ ] Ensure extended protocol Parse/Bind/Describe/Execute can run concurrently
      across sessions.
- [ ] Ensure unnamed prepared statement and unnamed portal state is per-session.

Acceptance:

- [ ] Two clients can create the same prepared statement name independently.
- [ ] Concurrent extended protocol workloads do not share unnamed portals.
- [ ] DDL generation changes invalidate or rebuild prepared/planned state.

## P1: Planner, Executor, SPI, And Function Manager Runtime

- [ ] Audit planner and executor globals reached by the supported pgcore path.
- [ ] Make `debug_query_string`, destination receiver state, and executor
      hook-related state backend-local or immutable.
- [ ] Classify `planner_hook`, `ExecutorStart_hook`, `ProcessUtility_hook`,
      `object_access_hook`, `fmgr_hook`, and related hooks.
- [ ] Make SPI globals such as `SPI_processed`, `SPI_tuptable`, and
      `SPI_result` backend-local if SPI-backed paths are supported.
- [ ] Classify fmgr `fn_extra` usage in cached `FmgrInfo` objects.
- [ ] Guard out unsupported procedural language or C-extension entry points.
- [ ] Fix or skip upstream regression C library loading separately from
      threading work.

Acceptance:

- [ ] Concurrent SELECT/INSERT/UPDATE/DDL smoke tests do not corrupt executor
      destinations or function-call state.
- [ ] Unsupported C-extension/procedural-language paths fail clearly instead of
      racing shared process state.

## P1: Locks, Proc State, Interrupts, And Latches

Rust storage removes much of the need for PostgreSQL physical lock/proc
machinery, but parser/planner/DDL/catalog paths can still reach lock manager
APIs.

- [ ] Identify every PostgreSQL lock manager entry point reached by the
      Rust-server path.
- [ ] Decide whether each lock is replaced by Rust catalog/storage locks,
      treated as a no-op, or backed by a logical backend-local lock owner.
- [ ] Make local lock tables and fast-path local lock counters backend-local if
      still used.
- [ ] Make `MyProc`/`MyProcNumber` semantics explicit for the single-process
      Rust-server model.
- [ ] Guard or replace interrupt globals such as cancel/death flags if client
      cancellation is supported.
- [ ] Guard latch/wait-event paths that assume one process per backend.

Acceptance:

- [ ] Concurrent DDL that needs catalog serialization uses Rust-side locking or
      a clearly defined Postgres lock shim.
- [ ] Client cancellation targets only the intended session.

## P1: Statistics And Activity State

The upstream regression suite exercises stats views even though production
PostgreSQL stats infrastructure is not part of fastpg storage.

- [ ] Decide which pg_stat views are supported, stubbed, or skipped.
- [ ] Make pgstat local pending state backend-local where it is still used.
- [ ] Make stats snapshots backend-local.
- [ ] Avoid using PostgreSQL stats shared memory as the Rust-server source of
      truth.
- [ ] Ensure `stats` regression coverage cannot crash the server even if values
      are approximate or stubbed.

Acceptance:

- [ ] The `stats` regression case does not crash under full-suite concurrent
      pressure.
- [ ] Stats view mismatches are classified separately from threading crashes.

## P1: Rust Storage And Catalog FFI Binding

The Rust storage layer is the right ownership model, but every callback from C
must be bound to the correct logical backend.

- [ ] Ensure `CURRENT_SESSION_STORAGE` is installed for every C callback that
      can touch Rust storage.
- [ ] Reject fallback to default session storage in server execution paths.
- [ ] Ensure Rust catalog transaction state is installed alongside PostgreSQL
      transaction shell state.
- [ ] Ensure Rust storage/catalog caches are generation-aware and invalidated on
      DDL commit.
- [ ] Ensure FFI callbacks do not return pointers whose lifetime is shorter than
      PostgreSQL expects.
- [ ] Ensure errors raised by Rust callbacks unwind through the owning backend
      only.

Acceptance:

- [ ] Two clients inserting into same/different Rust tables preserve transaction
      isolation.
- [ ] Two clients doing DDL/DML concurrently do not mix catalog generations.
- [ ] No server path reaches default session storage after startup.

## P2: Immutable Shared State Audit

Some process globals are acceptable if initialized once and never mutated.

- [ ] List all PostgreSQL globals the pgcore path reads after `pthread_once`
      initialization.
- [ ] Mark immutable lookup tables, constant function tables, fixed OID maps,
      and static operator/type metadata as `immutable_after_init`.
- [ ] Add debug assertions around any value that must remain unchanged.
- [ ] Separate immutable PostgreSQL metadata from mutable Rust catalog overlays.

Acceptance:

- [ ] The immutable list is documented in this spec or a linked audit file.
- [ ] Runtime assertions catch accidental writes to values treated as immutable.

## P2: Unsupported Feature Guardrails

- [ ] Add clear errors for unsupported storage-dependent features instead of
      letting them reach PostgreSQL physical storage code.
- [ ] Add clear errors for unsupported replication/logical decoding features.
- [ ] Add clear errors for unsupported extension loading in the Rust-server
      path unless explicitly enabled.
- [ ] Add clear errors for unsupported background worker paths.
- [ ] Keep unsupported-feature mismatches separate from threading failures in
      regression summaries.

Acceptance:

- [ ] Unsupported paths fail with stable SQLSTATE/message instead of process
      crashes or hidden shared-state mutation.

## Validation Checklist

- [x] `FASTPG_POSTGRES_BUILD_DIR=benches/.build/pgbench/fastpg cargo test -p fastpg-testkit --features postgres-execution --test rust_server_concurrency_torture`
- [ ] `FASTPG_POSTGRES_BUILD_DIR=benches/.build/pgbench/fastpg cargo test -p fastpg-pgcore --features postgres-execution`
- [x] `make -C benches regression REGRESSION_GLOBAL_TIMEOUT=600`
- [x] `make -C benches regression REGRESSION_GLOBAL_TIMEOUT=600 UPSTREAM_REGRESSION_CASES="stats privileges lock copy2"`
- [ ] `make -C benches pgbench-simple-indexed`
- [ ] `make -C benches pgbench-tpcb`
- [ ] Add a targeted two-client search-path/temp-table test.
- [ ] Add a targeted two-client explicit-transaction rollback/commit test.
- [ ] Add a targeted two-client concurrent error/notice/COPY test.
- [ ] Add a targeted `\c` or database-identity transition test.
- [ ] Add a stress loop that runs the above until it has observed sustained
      pgcore overlap.

## Done Criteria

- [ ] No process-global mutable pgcore state remains unclassified.
- [ ] All backend-local PostgreSQL state in the supported path is per-backend,
      per-session, per-execution, or guarded out.
- [ ] All truly shared mutable caches have synchronization and generation
      semantics.
- [ ] The Rust storage/catalog boundary is explicit and enforced.
- [ ] Regression mismatches are classified as semantic gaps, unsupported
      features, or threading bugs.
- [ ] Concurrent validation passes without reintroducing a global pgcore lane.
