# `spec/031-fastpg-planner-overhead-pruning.md`

# Fastpg Planner Overhead Pruning

## Summary

Keep PostgreSQL's parser, analyzer, rewriter, planner, optimizer, expression
evaluation, and executor infrastructure, but stop asking those layers to do
catalog and statistics work for features fastpg intentionally does not use.

The first target is planner-time ambient work: relation size estimation,
planner statistics lookup, secondary index metadata construction, foreign-key
proof collection, partition metadata, foreign-table metadata, and parallel
planning metadata. These features are useful in production PostgreSQL, but they
are not part of fastpg's test-optimized in-memory runtime contract today.

This is not a storage-layer feature spec. The goal is to prune higher-level
PostgreSQL work before it reaches storage, while preserving supported SQL
semantics and primary-key fast paths.

## Goals

```text
reuse the PostgreSQL SQL pipeline for supported statements
avoid planner/catalog work for unsupported production features
keep ordinary DDL/DML/query behavior compatible with current fastpg coverage
preserve Rust-owned catalog, storage, session, and transaction state
keep primary-key lookup planning and execution working
make skipped planner work measurable
fail clearly when a query requires an unsupported physical feature
distinguish pg_stat cumulative stats from pg_statistic planner stats
```

## Non-Goals

```text
do not replace PostgreSQL's planner with a Rust planner
do not implement secondary index storage or maintenance
do not implement PostgreSQL ANALYZE statistics collection
do not implement production pg_stat cumulative statistics
do not implement foreign tables, partitions, or inheritance as part of this spec
do not add parallel query support
do not change normal PostgreSQL builds when USE_FASTPG is disabled
```

## Current Problem

The Rust server owns long-lived database state, but the prepare path still runs
through PostgreSQL planning:

```text
raw_parser
parse_analyze_varparams
pg_rewrite_query
pg_plan_queries
```

During planning, PostgreSQL's `get_relation_info()` builds a general-purpose
`RelOptInfo` for every base relation. In the upstream path this can include:

```text
table_open()
NOT NULL metadata lookup
relation size estimation
parallel worker reloption lookup
RelationGetIndexList()
index_open() for every visible index
IndexOptInfo construction
index opfamily, collation, predicate, expression, and cost metadata
RelationGetStatExtList()
extended planner statistics construction
foreign-table metadata
foreign-key proof collection
partition metadata
table-AM capability flags
```

Most of that is production planner machinery. fastpg currently needs only a
small subset for supported test-server behavior:

```text
relation shape and column metadata
basic relation cardinality estimates
primary-key index metadata for intentional primary-key lookup paths
table-AM flags for supported scan nodes
```

## Workload Cardinality Assumption

fastpg should optimize for unit-test schemas where most user tables are tiny:

```text
common table size: fewer than 20-30 rows
larger test fixture table size: roughly 100-500 rows
outliers above that range: possible but not the default design center
```

This matters for planning policy. For these table sizes, exact cost modeling is
usually less valuable than avoiding planner overhead. A sequential scan over a
20-row table is fine. A sequential scan over a few hundred rows is often still
fine for test workloads. The main exception is primary-key equality lookup,
which is common enough and semantically important enough to keep as an
intentional fast path.

The planner should therefore be biased toward:

```text
cheap fixed estimates
simple supported plan shapes
primary-key equality lookup when available
clear rejection of unsupported physical paths
```

It should not spend time maintaining PostgreSQL-style statistics just to choose
between sophisticated plans for tiny relations.

The most confusing naming boundary is statistics:

```text
pg_stat_* cumulative stats:
  runtime counters and activity views such as pg_stat_all_tables

pg_statistic / pg_statistic_ext planner stats:
  selectivity and extended-statistics metadata used during planning
```

The ordinary fastpg execution path does not appear to pay much for cumulative
`pg_stat` accounting: it bypasses the main `postgres.c` query loop, calls
`ExecutorStart(..., 0)`, and the fastpg table AM does not call heap
`pgstat_count_heap_*` counters. The planner-statistics side is different:
planner code can still probe `pg_statistic` and `pg_statistic_ext` metadata even
when fastpg has no mutable ANALYZE-produced statistics to use.

## Target Architecture

Introduce an explicit fastpg planner policy for relations backed by the fastpg
table AM:

```c
typedef struct FastPgPlannerRelationPolicy
{
	bool is_fastpg_relation;
	bool use_exact_row_count;
	bool expose_primary_key_index;
	bool expose_secondary_indexes;
	bool expose_extended_statistics;
	bool expose_foreign_keys;
	bool expose_partition_info;
	bool expose_foreign_table_info;
	bool expose_parallel_workers;
} FastPgPlannerRelationPolicy;
```

The first implementation should be conservative:

```text
is_fastpg_relation = true for fastpg_mem table-AM relations
use_exact_row_count = false by default
expose_primary_key_index = true
expose_secondary_indexes = false
expose_extended_statistics = false
expose_foreign_keys = false
expose_partition_info = false
expose_foreign_table_info = false
expose_parallel_workers = false
```

Normal PostgreSQL builds and non-fastpg relations keep the upstream behavior.

## Relation Size Policy

Today the fastpg table AM estimates size by asking Rust storage for the current
row count and deriving fake pages from that count.

That is more exact than fastpg usually needs. It can also make every plan pay
for storage synchronization, even for queries where exact cardinality does not
matter.

Add a configurable relation estimate mode:

```text
fixed:
  use small default pages/tuples values for user tables

catalog_cached:
  use row counts cached in Rust catalog/stat metadata and generationed with it

exact:
  call Rust storage for the current row count
```

Default to `fixed`, not `exact`. The first fixed estimate should assume a tiny
test table:

```text
pages = 1
tuples = 32
allvisfrac = 1.0
```

This estimate deliberately matches the common case of fewer than 20-30 rows
without treating an empty table as a special planning problem. Execution still
returns exact results.

`catalog_cached` is a reasonable follow-up if Rust already maintains cheap
approximate row counts as part of insert/delete/update bookkeeping. It should
not require a planner-time storage scan or synchronization point.

`exact` should be a debug/profiling option or a future targeted optimization,
not the normal path.

The planner may still use PostgreSQL default selectivity estimates. Incorrect
cost estimates are acceptable as long as the chosen plan is supported and
correct.

For outlier test fixtures in the 100-500 row range, fastpg should still prefer
planning simplicity. If those workloads need speed, the first optimization
should be supported primary-key lookup, not broad PostgreSQL statistics.

## Planner Statistics Policy

For fastpg user relations:

```text
set rel->statlist = NIL
skip RelationGetStatExtList()
skip pg_statistic_ext and pg_statistic_ext_data reads
do not publish ANALYZE-produced statistics
do not add a statistics generation counter yet
```

Column selectivity should fall back to PostgreSQL's default estimates. That is
good enough for the current unit-test server scope.

If planner-visible statistics are added later, they should be Rust-owned and
generationed. That future work should compose with
`030-catalog-generation-invalidation.md` rather than reintroducing PostgreSQL
ANALYZE storage.

## Index Planning Policy

fastpg accepts secondary index DDL so schema setup succeeds, but secondary
indexes are not used for accelerated execution today. The planner should not
build general `IndexOptInfo` objects for secondary indexes unless the storage
layer can actually execute and maintain them.

For fastpg user relations:

```text
expose primary-key index metadata when Rust primary-key lookup supports it
hide secondary indexes from path generation
keep secondary index rows visible through pg_catalog introspection
do not open every secondary index relation during get_relation_info()
do not read secondary index predicates or expressions for planning
do not call secondary index AM cost or tree-height callbacks
```

Primary-key index exposure should be narrow. It should only produce planner
metadata for shapes that the fastpg index AM can execute, such as equality
lookup on the complete primary key.

Unsupported index plan nodes must fail before executor startup with a clear
fastpg error. They must not fall through to table-AM callbacks that report
"unsupported" after the planner has already chosen the path.

## Constraint, FK, Partition, And Foreign-Table Policy

These features are not storage requirements for fastpg's current project goal.
They are planner optimizations or production compatibility surfaces.

For fastpg user relations:

```text
skip foreign-key proof collection
skip partition metadata and partition pruning
reject partitioned-table execution unless a separate spec enables it
reject foreign-table execution unless a separate spec enables it
do not use NOT NULL metadata for planner-only optimizations unless measured useful
```

DDL compatibility may still accept selected declarations as catalog-visible
metadata when that helps migrations run. Accepting metadata is not the same as
paying planner or executor costs for the feature.

## Parallel Query Policy

The existing fastpg prepare path clears `CURSOR_OPT_PARALLEL_OK`. Keep that
policy and make it explicit:

```text
do not expose relation parallel worker settings
do not generate Gather/Gather Merge paths
validate planned statements contain no parallel nodes
keep PostgreSQL internal IPC disabled in the Rust-server path
```

Parallel query is a production execution feature and is not aligned with the
single-process test-server goal today.

## pg_stat Policy

`pg_stat_*` cumulative statistics are not a performance target for this spec
because ordinary fastpg DML should not be incrementing PostgreSQL heap stats
counters in the current path.

Keep the policy explicit anyway:

```text
ordinary DML does not update PostgreSQL cumulative relation stats
ordinary query execution does not report postgres.c activity state
pg_stat views may return empty, zero, or compatibility-shim data
pg_stat reset and flush functions are unsupported or no-op compatibility shims
```

If a profile shows `pgstat_*` functions on the hot path, treat that as a bug in
the fastpg execution boundary and prune it separately from planner statistics.

## Plan Validation

After `pg_plan_queries()` and before `ExecutorStart()`, validate planned
statements against the fastpg-supported physical surface.

The validator should reject:

```text
unsupported secondary index scans
bitmap heap/index scans
index-only scans unless explicitly supported
tid scans and tid-range scans
sample scans
lock rows / SELECT FOR UPDATE
parallel plan nodes
foreign scans
custom scans
merge paths that require unsupported storage behavior
```

This validator is a safety net. The planner policy should prevent these paths
from being generated for ordinary fastpg relations, but validation gives clear
errors when a path slips through.

## Observability

Add counters around the fastpg planner policy:

```text
fastpg planner relations seen
fastpg relation size estimate mode used
exact Rust row-count calls
primary-key IndexOptInfo objects built
secondary indexes hidden from planner
extended-statistics lookups skipped
foreign-key lookups skipped
partition metadata lookups skipped
foreign-table metadata lookups skipped
parallel plans rejected
unsupported plan nodes rejected
```

Expose these through the benchmark harness output or a debug trace flag such as:

```text
FASTPG_PLANNER_TRACE=1
```

The trace should be quiet by default.

## Acceptance Tests

```text
simple SELECT over a fastpg table does not call exact Rust row count by default
simple SELECT over a fastpg table does not read extended planner statistics
ANALYZE remains no-op and does not publish planner stats
secondary CREATE INDEX remains visible in pg_catalog
secondary CREATE INDEX does not produce a secondary IndexOptInfo
primary-key equality lookup still uses the fastpg primary-key path
unsupported bitmap/index-only/tid/sample scan plans fail before ExecutorStart
planned statements contain no Gather or Gather Merge nodes
ordinary INSERT/UPDATE/DELETE do not increment PostgreSQL heap pgstat counters
```

## Performance Tests

Add benchmark variants that stress planner overhead rather than storage:

```text
many prepares of SELECT * FROM t WHERE id = $1
many prepares against a table with many secondary indexes
many prepares against a schema with many accepted-but-unused constraints
pgbench simple-update with prepared statements enabled
regression comparison for catalog introspection after secondary index DDL
```

Profile before and after the planner policy. The expected improvement is lower
prepare/planning CPU and fewer catalog/syscache lookups, not a change in Rust
storage throughput.

## Migration Steps

```text
1. Add a fastpg planner relation policy helper behind USE_FASTPG.
2. Add planner-policy counters and an optional trace flag.
3. Change fastpg relation size estimation to fixed or catalog-cached by default.
4. Set rel->statlist = NIL for fastpg user relations and skip extended stats.
5. Hide secondary indexes from planner path generation while preserving catalogs.
6. Preserve primary-key index metadata for supported equality lookup paths.
7. Skip FK, partition, foreign-table, and parallel metadata for fastpg relations.
8. Add planned-statement validation before ExecutorStart().
9. Add regression tests for planner pruning and unsupported-plan errors.
10. Run pgbench and regression profiles before deciding the next pruning target.
```

## Open Questions

```text
Should fixed relation estimates vary by table kind or always use pages=1/tuples=32?
Should exact row count remain available through a GUC, env var, or debug build?
Should NOT NULL metadata be kept for planner proofs or skipped until measured?
Should pg_stat views return empty rows, zero counters, or unsupported errors?
Should primary-key lookup be represented as a PostgreSQL IndexScan or a custom fastpg plan marker?
```
