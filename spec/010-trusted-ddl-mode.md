# Trusted DDL Mode

## Summary

Add a test-only DDL mode for fast-fork clusters where unit tests are trusted,
short-lived, and generally run schema setup as the test owner. In that world,
PostgreSQL's full DDL policy and extensibility machinery is mostly overhead:
ACL/default-ACL checks, role membership checks, event triggers, security labels,
object-access hooks, extension membership wiring, and broad dependency recording
create catalog traffic and syscache churn that do not help ordinary application
unit tests.

This mode keeps the parts of DDL that make objects usable:

- table, index, sequence, type, constraint, and toast catalog rows
- command-counter visibility
- relcache/syscache invalidation needed for later queries
- MVCC and rollback semantics
- index and constraint behavior used by query execution

It strips or short-circuits policy, auditing, extension, and long-tail
dependency machinery that only matters when PostgreSQL is acting as a general
multi-user database.

## Goals

- Speed up common fixture DDL: `CREATE TABLE`, `CREATE INDEX`, `CREATE
  SEQUENCE`, `ALTER TABLE ADD CONSTRAINT`, and simple `DROP`.
- Avoid permission/ownership/default-ACL syscache lookups in trusted test
  schemas.
- Compile out event-trigger and object-access-hook work in the fast fork.
- Avoid security-label/comment/default-ACL catalog writes unless explicitly
  needed.
- Reduce `pg_depend` churn for trusted objects while preserving the minimum
  dependencies needed for supported drop/rollback behavior.
- Preserve ordinary query behavior against objects created in trusted DDL mode.
- Preserve transaction rollback of DDL.
- Keep stock PostgreSQL behavior as the default.

## Non-Goals

- Correct permission enforcement.
- Security-label, row-security policy, or default-privilege test coverage.
- Extension install/update/drop support.
- Event trigger behavior.
- Logical replication, publication/subscription, or DDL replication behavior.
- Perfect `pg_depend` introspection for all object types.
- Supporting arbitrary object types in the first pass.
- Making this mode safe for production.

## Build Flag

Add a new opt-in build option:

- Meson: `-Dtest_trusted_ddl=true`
- Autoconf: `--enable-test-trusted-ddl`
- C define: `USE_TEST_TRUSTED_DDL`

The option defaults to `false`. When disabled, DDL behavior must match upstream
PostgreSQL.

This mode should compose with the existing fast-fork options:

```sh
meson setup build-fastfork \
  -Dtest_fake_wal=true \
  -Dtest_mem_smgr=true \
  -Dtest_mem_slru=true \
  -Dtest_no_wal_assembly=true \
  -Dtest_ephemeral_catalog=true \
  -Dtest_trusted_ddl=true
```

If we need a compatibility escape hatch inside a trusted build, add a developer
GUC such as `fastfork.trusted_ddl = on`. The first implementation can keep this
compile-time only if that is simpler.

## Trust Contract

When `USE_TEST_TRUSTED_DDL` is enabled:

- The database is a disposable unit-test database.
- Tests do not assert permission-denied behavior.
- Tests do not depend on event triggers or object-access hooks.
- Tests do not install or introspect extensions as part of the fast path.
- Tests do not rely on exact `pg_depend`, ACL, security label, or default ACL
  contents except for dependencies needed to make ordinary `DROP` work.
- DDL is expected to run as the effective owner of the test schema, or the mode
  may treat supported DDL as owner-authorized.

Unsupported feature tests should run against stock PostgreSQL or with this flag
disabled.

## Primary Targets

### Permission and Ownership Checks

Main files:

- `src/backend/catalog/aclchk.c`
- `src/backend/commands/tablecmds.c`
- `src/backend/commands/indexcmds.c`
- `src/backend/commands/sequence.c`
- `src/backend/commands/schemacmds.c`
- `src/backend/commands/typecmds.c`
- `src/backend/catalog/namespace.c`

Examples of work to bypass for supported trusted DDL:

- schema `CREATE` permission checks
- relation owner checks
- type/schema owner checks
- role membership checks used only to decide whether DDL is allowed
- default privilege lookup/application
- ACL catalog writes for default privileges

The first pass can implement helper wrappers like:

```c
#ifdef USE_TEST_TRUSTED_DDL
bool FastForkTrustedDDLActive(void);
bool FastForkTrustsDDLPermissions(void);
#endif
```

Then gate high-traffic checks with a readable condition rather than scattering
raw preprocessor blocks through every command.

### Event Triggers

Main files:

- `src/backend/commands/event_trigger.c`
- `src/backend/tcop/utility.c`
- `src/include/commands/event_trigger.h`

Important paths:

- `EventTriggerDDLCommandStart`
- `EventTriggerDDLCommandEnd`
- `EventTriggerSQLDrop`
- `EventTriggerTableRewrite`
- `EventTriggerAlterTableStart`
- `EventTriggerAlterTableEnd`
- event-trigger command collection

Under trusted DDL mode, these should be cheap no-ops. Catalog support for event
triggers can remain in the seed cluster, but the DDL execution path should not
collect command objects, scan for triggers, build event payloads, or invoke
callbacks.

### Object Access Hooks

Main files:

- `src/backend/catalog/objectaccess.c`
- `src/include/catalog/objectaccess.h`
- call sites for `InvokeObject*Hook`

Important paths:

- object post-create hooks
- object drop hooks
- object alter hooks
- namespace search hooks

In trusted DDL mode, object-access hooks should compile to no-ops or return
immediately. This is a fork for application test speed, not extension auditing.

### Security Labels, Comments, and Default ACLs

Main files:

- `src/backend/commands/seclabel.c`
- `src/backend/commands/comment.c`
- `src/backend/catalog/aclchk.c`
- `src/backend/catalog/pg_shdepend.c`

Trusted mode should skip work that creates or updates:

- `pg_seclabel`
- shared security label rows
- default ACL rows
- owner dependency rows needed only for ownership bookkeeping

Comments can either remain supported through the normal path or become no-ops
for trusted objects. If application tests introspect comments, keep comments
normal and focus first on labels/default ACLs.

### Extension and Current-Extension Dependencies

Main files:

- `src/backend/commands/extension.c`
- `src/backend/catalog/dependency.c`

Important paths:

- `recordDependencyOnCurrentExtension`
- extension membership checks
- extension script execution paths

Trusted mode should not optimize extension creation in the first pass. If DDL is
running inside an extension script, either fall back to normal behavior or fail
clearly with an unsupported-feature error. For ordinary unit-test DDL outside an
extension script, skip current-extension dependency checks and writes.

### Dependency Recording

Main files:

- `src/backend/catalog/dependency.c`
- `src/backend/catalog/pg_depend.c`
- `src/backend/catalog/heap.c`
- `src/backend/catalog/index.c`
- `src/backend/commands/tablecmds.c`
- `src/backend/commands/indexcmds.c`
- `src/backend/commands/sequence.c`

Do not remove dependencies blindly. Split them into two buckets.

Keep required structural dependencies:

- table-to-toast relation dependencies
- index-to-table dependencies
- constraint-to-table/index dependencies
- owned sequence dependencies for `serial`/identity columns
- dependencies needed for `DROP TABLE` to remove its indexes, constraints, toast
  relation, and owned sequences

Skip or minimize nonessential dependencies:

- owner dependencies
- extension dependencies outside extension scripts
- expression dependencies on functions/operators/types for trusted defaults and
  check constraints
- dependencies used only by broad catalog introspection tests
- dependencies for unsupported object types

If full `performDeletion` remains expensive, add a trusted fast path for
supported relation drops:

1. Detect ordinary test-created relation objects.
2. Collect known structural children from fast in-memory indexes or direct
   catalog lookups.
3. Drop heap, indexes, toast, constraints, and owned sequences.
4. Skip broad dependency graph traversal.

That drop fast path should be separate from the first permission/hook bypass if
it proves risky.

## Supported First Pass

Optimize this set first:

- `CREATE SCHEMA`
- `CREATE TABLE`
- `CREATE INDEX`
- `CREATE SEQUENCE`
- `ALTER TABLE ADD PRIMARY KEY`
- `ALTER TABLE ADD UNIQUE`
- `ALTER TABLE ADD CHECK`
- simple foreign keys if validation tests need them
- `DROP TABLE`
- transaction rollback of all the above

Explicitly leave these unsupported or normal-path initially:

- `CREATE EXTENSION`
- event triggers
- foreign tables and FDWs
- publications/subscriptions
- RLS policies
- grants/revokes/default privileges
- security labels
- concurrent index builds
- views/rules/materialized views unless needed by tests
- partitioning edge cases beyond simple table/index metadata

## Correctness Requirements

- Objects created through trusted DDL are queryable through ordinary parser,
  planner, executor, relcache, and syscache paths.
- Indexes created through trusted DDL are usable and enforce uniqueness.
- Constraints created through trusted DDL enforce normal DML behavior.
- Rolling back a transaction containing trusted DDL removes the created objects.
- Dropping a trusted table removes its supported structural children.
- Unsupported DDL either uses the normal path or fails clearly; it must not
  silently create half-supported catalog state.
- Stock builds have identical behavior to upstream PostgreSQL.

## Validation

Run the fast-fork validation script:

```sh
./test-fastfork.sh core --no-reconfigure
```

Add targeted SQL validation for trusted DDL:

- create schema, table, index, sequence, primary key, unique constraint, check
  constraint, and optional foreign key
- insert rows that prove indexes and constraints work
- roll back and verify the objects disappear
- create/drop the same supported objects and verify structural children are
  cleaned up
- verify event trigger creation or invocation is unsupported/no-op according to
  the chosen first-pass behavior
- verify permission-specific tests are excluded from the fast-fork validation
  set or run only with `test_trusted_ddl=false`

Run the permanent-table rollback benchmark:

```sh
python3 bench/compare_pgbench.py \
  --rounds 3 \
  --transactions 200 \
  --rows 200 \
  --reuse-builds \
  --output-dir bench/results/trusted-ddl
```

Run the fixture snapshot benchmark:

```sh
python3 bench/compare_pgbench.py \
  --fakewal-workload snapshot \
  --rounds 3 \
  --transactions 200 \
  --rows 200 \
  --reuse-builds \
  --output-dir bench/results/trusted-ddl-snapshot
```

The implementation is successful if:

- validation passes
- the permanent-table rollback workload improves over the previous fast fork
- snapshot restore still passes and remains faster than replaying fixture setup
- disabling `test_trusted_ddl` shows the trusted DDL path contributes measurable
  speedup

## Risks

- Permission semantics are intentionally wrong in this mode. Keep it clearly
  test-only and opt-in.
- Skipping dependency rows can break `DROP`, `ALTER`, or catalog introspection.
  Keep structural dependencies until a measured drop fast path replaces them.
- Event trigger and object-access hook bypass can hide extension behavior. Make
  extension/event-trigger suites incompatible with trusted mode.
- Some default/check expression dependency scans may protect later DDL like
  `DROP FUNCTION`. If those tests matter, use the normal path for expression
  dependencies or mark that DDL unsupported.
- The fastest version will likely compose with the future ephemeral catalog
  overlay. Keep helper boundaries narrow so this mode can later skip physical
  catalog writes rather than just shortening the normal writes.
