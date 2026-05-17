/*-------------------------------------------------------------------------
 *
 * fastpg_mem_tableam.c
 *	  Tiny in-memory table access method for fastpg storage-boundary probes.
 *
 * IDENTIFICATION
 *	  src/backend/access/fastpg/fastpg_mem_tableam.c
 *
 *-------------------------------------------------------------------------
 */
#include "postgres.h"

#ifdef USE_FASTPG

#include "access/fastpg_tableam.h"
#include "access/multixact.h"
#include "access/tableam.h"
#include "access/xact.h"
#include "executor/tuptable.h"
#include "fmgr.h"
#include "utils/datum.h"
#include "utils/elog.h"
#include "utils/errcodes.h"
#include "utils/memutils.h"
#include "utils/rel.h"
#include "utils/snapmgr.h"

#include <stdint.h>

typedef struct FastPgMemScanDesc
{
	TableScanDescData base;
	uint64_t	scan_handle;
} FastPgMemScanDesc;

typedef struct FastPgMemIndexFetch
{
	IndexFetchTableData base;
} FastPgMemIndexFetch;

extern void fastpg_rust_relation_clear(uint32_t relid);
extern size_t fastpg_rust_relation_row_count(uint32_t relid);
extern bool fastpg_rust_relation_insert(uint32_t relid,
										const uintptr_t *values,
										const uint8_t *isnull,
										size_t natts,
										uint16_t *tid_offset);
extern uint64_t fastpg_rust_scan_begin(uint32_t relid);
extern void fastpg_rust_scan_reset(uint64_t scan_handle);
extern void fastpg_rust_scan_end(uint64_t scan_handle);
extern bool fastpg_rust_scan_next(uint64_t scan_handle,
								  uint8_t forward,
								  uintptr_t *values,
								  uint8_t *isnull,
								  size_t natts,
								  uint16_t *tid_offset);
extern bool fastpg_rust_fetch_row(uint32_t relid,
								  uint16_t tid_offset,
								  uintptr_t *values,
								  uint8_t *isnull,
								  size_t natts);

static MemoryContext fastpg_mem_context = NULL;

static const TableAmRoutine fastpg_mem_methods;

static void
fastpg_mem_unsupported(const char *operation)
{
	ereport(ERROR,
			(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
			 errmsg("fastpg_mem table access method does not support %s",
					operation)));
}

static MemoryContext
fastpg_mem_get_context(void)
{
	if (fastpg_mem_context == NULL)
		fastpg_mem_context =
			AllocSetContextCreate(TopMemoryContext,
								  "fastpg_mem tableam storage",
								  ALLOCSET_DEFAULT_SIZES);

	return fastpg_mem_context;
}

static void
fastpg_mem_copy_slot_values(Relation rel,
							TupleTableSlot *slot,
							uintptr_t **values_out,
							uint8_t **isnull_out)
{
	TupleDesc	tupdesc;
	uintptr_t  *values;
	uint8_t    *isnull;
	MemoryContext oldcontext;

	slot_getallattrs(slot);
	tupdesc = RelationGetDescr(rel);
	values = palloc0_array(uintptr_t, tupdesc->natts);
	isnull = palloc0_array(uint8_t, tupdesc->natts);

	oldcontext = MemoryContextSwitchTo(fastpg_mem_get_context());
	for (int index = 0; index < tupdesc->natts; index++)
	{
		Form_pg_attribute attr = TupleDescAttr(tupdesc, index);

		isnull[index] = slot->tts_isnull[index] ? 1 : 0;
		if (isnull[index] == 0)
			values[index] = (uintptr_t) datumCopy(slot->tts_values[index],
												  attr->attbyval,
												  attr->attlen);
	}
	MemoryContextSwitchTo(oldcontext);

	*values_out = values;
	*isnull_out = isnull;
}

static void
fastpg_mem_store_virtual_tuple(Relation rel,
							   TupleTableSlot *slot,
							   const uintptr_t *values,
							   const uint8_t *isnull,
							   uint16_t tid_offset)
{
	int			natts = slot->tts_tupleDescriptor->natts;

	for (int index = 0; index < natts; index++)
	{
		slot->tts_values[index] = (Datum) values[index];
		slot->tts_isnull[index] = isnull[index] != 0;
	}

	ItemPointerSet(&slot->tts_tid, 0, (OffsetNumber) tid_offset);
	slot->tts_tableOid = RelationGetRelid(rel);
	ExecStoreVirtualTuple(slot);
}

static const TupleTableSlotOps *
fastpg_mem_slot_callbacks(Relation rel)
{
	return &TTSOpsVirtual;
}

static TableScanDesc
fastpg_mem_scan_begin(Relation rel,
					  Snapshot snapshot,
					  int nkeys,
					  ScanKeyData *key,
					  ParallelTableScanDesc pscan,
					  uint32 flags)
{
	FastPgMemScanDesc *scan;

	if (nkeys != 0)
		fastpg_mem_unsupported("scan keys");
	if (pscan != NULL)
		fastpg_mem_unsupported("parallel scans");

	scan = palloc0_object(FastPgMemScanDesc);
	scan->base.rs_rd = rel;
	scan->base.rs_snapshot = snapshot;
	scan->base.rs_nkeys = nkeys;
	scan->base.rs_key = NULL;
	scan->base.rs_flags = flags;
	scan->scan_handle = fastpg_rust_scan_begin(RelationGetRelid(rel));
	if (scan->scan_handle == 0)
		elog(ERROR, "fastpg_mem failed to create Rust scan handle");

	RelationIncrementReferenceCount(rel);

	return (TableScanDesc) scan;
}

static void
fastpg_mem_scan_end(TableScanDesc sscan)
{
	FastPgMemScanDesc *scan = (FastPgMemScanDesc *) sscan;

	RelationDecrementReferenceCount(scan->base.rs_rd);
	fastpg_rust_scan_end(scan->scan_handle);
	if (scan->base.rs_flags & SO_TEMP_SNAPSHOT)
		UnregisterSnapshot(scan->base.rs_snapshot);
	pfree(scan);
}

static void
fastpg_mem_scan_rescan(TableScanDesc sscan,
					   ScanKeyData *key,
					   bool set_params,
					   bool allow_strat,
					   bool allow_sync,
					   bool allow_pagemode)
{
	FastPgMemScanDesc *scan = (FastPgMemScanDesc *) sscan;

	if (key != NULL)
		fastpg_mem_unsupported("scan keys");
	fastpg_rust_scan_reset(scan->scan_handle);
}

static bool
fastpg_mem_scan_getnextslot(TableScanDesc sscan,
							ScanDirection direction,
							TupleTableSlot *slot)
{
	FastPgMemScanDesc *scan = (FastPgMemScanDesc *) sscan;
	int			natts = slot->tts_tupleDescriptor->natts;
	uintptr_t  *values;
	uint8_t    *isnull;
	uint16_t	tid_offset = 0;
	bool		found;

	ExecClearTuple(slot);

	values = palloc0_array(uintptr_t, natts);
	isnull = palloc0_array(uint8_t, natts);
	found = fastpg_rust_scan_next(scan->scan_handle,
								  ScanDirectionIsBackward(direction) ? 0 : 1,
								  values,
								  isnull,
								  natts,
								  &tid_offset);
	if (found)
		fastpg_mem_store_virtual_tuple(scan->base.rs_rd,
									   slot,
									   values,
									   isnull,
									   tid_offset);

	pfree(values);
	pfree(isnull);

	return found;
}

static Size
fastpg_mem_parallelscan_estimate(Relation rel)
{
	return sizeof(ParallelTableScanDescData);
}

static Size
fastpg_mem_parallelscan_initialize(Relation rel, ParallelTableScanDesc pscan)
{
	memset(pscan, 0, sizeof(ParallelTableScanDescData));
	return sizeof(ParallelTableScanDescData);
}

static void
fastpg_mem_parallelscan_reinitialize(Relation rel, ParallelTableScanDesc pscan)
{
}

static IndexFetchTableData *
fastpg_mem_index_fetch_begin(Relation rel, uint32 flags)
{
	FastPgMemIndexFetch *fetch = palloc0_object(FastPgMemIndexFetch);

	fetch->base.rel = rel;
	fetch->base.flags = flags;
	return (IndexFetchTableData *) fetch;
}

static void
fastpg_mem_index_fetch_reset(IndexFetchTableData *data)
{
}

static void
fastpg_mem_index_fetch_end(IndexFetchTableData *data)
{
	pfree(data);
}

static bool
fastpg_mem_tuple_fetch_row_version(Relation rel,
								   ItemPointer tid,
								   Snapshot snapshot,
								   TupleTableSlot *slot)
{
	OffsetNumber offset = ItemPointerGetOffsetNumber(tid);
	int			natts = slot->tts_tupleDescriptor->natts;
	uintptr_t  *values;
	uint8_t    *isnull;
	bool		found;

	if (ItemPointerGetBlockNumber(tid) != 0 ||
		offset == InvalidOffsetNumber)
		return false;

	ExecClearTuple(slot);
	values = palloc0_array(uintptr_t, natts);
	isnull = palloc0_array(uint8_t, natts);
	found = fastpg_rust_fetch_row(RelationGetRelid(rel),
								  offset,
								  values,
								  isnull,
								  natts);
	if (found)
		fastpg_mem_store_virtual_tuple(rel, slot, values, isnull, offset);
	pfree(values);
	pfree(isnull);

	return found;
}

static bool
fastpg_mem_index_fetch_tuple(IndexFetchTableData *scan,
							 ItemPointer tid,
							 Snapshot snapshot,
							 TupleTableSlot *slot,
							 bool *call_again,
							 bool *all_dead)
{
	*call_again = false;
	if (all_dead != NULL)
		*all_dead = false;
	return fastpg_mem_tuple_fetch_row_version(scan->rel, tid, snapshot, slot);
}

static bool
fastpg_mem_tuple_tid_valid(TableScanDesc scan, ItemPointer tid)
{
	OffsetNumber offset = ItemPointerGetOffsetNumber(tid);
	size_t		row_count =
		fastpg_rust_relation_row_count(RelationGetRelid(scan->rs_rd));

	return ItemPointerGetBlockNumber(tid) == 0 &&
		offset != InvalidOffsetNumber &&
		offset <= row_count;
}

static void
fastpg_mem_tuple_get_latest_tid(TableScanDesc scan, ItemPointer tid)
{
}

static bool
fastpg_mem_tuple_satisfies_snapshot(Relation rel,
									TupleTableSlot *slot,
									Snapshot snapshot)
{
	return true;
}

static TransactionId
fastpg_mem_index_delete_tuples(Relation rel, TM_IndexDeleteOp *delstate)
{
	return InvalidTransactionId;
}

static void
fastpg_mem_tuple_insert(Relation rel,
						TupleTableSlot *slot,
						CommandId cid,
						uint32 options,
						BulkInsertStateData *bistate)
{
	TupleDesc	tupdesc = RelationGetDescr(rel);
	uintptr_t  *values;
	uint8_t    *isnull;
	uint16_t	tid_offset = 0;

	fastpg_mem_copy_slot_values(rel, slot, &values, &isnull);
	if (!fastpg_rust_relation_insert(RelationGetRelid(rel),
									 values,
									 isnull,
									 tupdesc->natts,
									 &tid_offset))
		elog(ERROR, "fastpg_mem failed to insert row into Rust storage");

	ItemPointerSet(&slot->tts_tid, 0, (OffsetNumber) tid_offset);
	slot->tts_tableOid = RelationGetRelid(rel);
	pfree(values);
	pfree(isnull);
}

static void
fastpg_mem_tuple_insert_speculative(Relation rel,
									TupleTableSlot *slot,
									CommandId cid,
									uint32 options,
									BulkInsertStateData *bistate,
									uint32 specToken)
{
	fastpg_mem_tuple_insert(rel, slot, cid, options, bistate);
}

static void
fastpg_mem_tuple_complete_speculative(Relation rel,
									  TupleTableSlot *slot,
									  uint32 specToken,
									  bool succeeded)
{
	if (!succeeded)
		fastpg_mem_unsupported("aborting speculative insertions");
}

static void
fastpg_mem_multi_insert(Relation rel,
						TupleTableSlot **slots,
						int nslots,
						CommandId cid,
						uint32 options,
						BulkInsertStateData *bistate)
{
	for (int index = 0; index < nslots; index++)
		fastpg_mem_tuple_insert(rel, slots[index], cid, options, bistate);
}

static TM_Result
fastpg_mem_tuple_delete(Relation rel,
						ItemPointer tid,
						CommandId cid,
						uint32 options,
						Snapshot snapshot,
						Snapshot crosscheck,
						bool wait,
						TM_FailureData *tmfd)
{
	fastpg_mem_unsupported("DELETE");
	return TM_Ok;
}

static TM_Result
fastpg_mem_tuple_update(Relation rel,
						ItemPointer otid,
						TupleTableSlot *slot,
						CommandId cid,
						uint32 options,
						Snapshot snapshot,
						Snapshot crosscheck,
						bool wait,
						TM_FailureData *tmfd,
						LockTupleMode *lockmode,
						TU_UpdateIndexes *update_indexes)
{
	fastpg_mem_unsupported("UPDATE");
	return TM_Ok;
}

static TM_Result
fastpg_mem_tuple_lock(Relation rel,
					  ItemPointer tid,
					  Snapshot snapshot,
					  TupleTableSlot *slot,
					  CommandId cid,
					  LockTupleMode mode,
					  LockWaitPolicy wait_policy,
					  uint8 flags,
					  TM_FailureData *tmfd)
{
	fastpg_mem_unsupported("tuple locking");
	return TM_Ok;
}

static void
fastpg_mem_relation_set_new_filelocator(Relation rel,
										const RelFileLocator *newrlocator,
										char persistence,
										TransactionId *freezeXid,
										MultiXactId *minmulti)
{
	fastpg_rust_relation_clear(RelationGetRelid(rel));
	*freezeXid = InvalidTransactionId;
	*minmulti = InvalidMultiXactId;
}

static void
fastpg_mem_relation_nontransactional_truncate(Relation rel)
{
	fastpg_rust_relation_clear(RelationGetRelid(rel));
}

static void
fastpg_mem_relation_copy_data(Relation rel, const RelFileLocator *newrlocator)
{
	fastpg_mem_unsupported("relation copy");
}

static void
fastpg_mem_relation_copy_for_cluster(Relation OldTable,
									 Relation NewTable,
									 Relation OldIndex,
									 bool use_sort,
									 TransactionId OldestXmin,
									 Snapshot snapshot,
									 TransactionId *xid_cutoff,
									 MultiXactId *multi_cutoff,
									 double *num_tuples,
									 double *tups_vacuumed,
									 double *tups_recently_dead)
{
	fastpg_mem_unsupported("CLUSTER/VACUUM FULL");
}

static void
fastpg_mem_relation_vacuum(Relation rel,
						   const VacuumParams *params,
						   BufferAccessStrategy bstrategy)
{
}

static bool
fastpg_mem_scan_analyze_next_block(TableScanDesc scan, ReadStream *stream)
{
	return false;
}

static bool
fastpg_mem_scan_analyze_next_tuple(TableScanDesc scan,
								   double *liverows,
								   double *deadrows,
								   TupleTableSlot *slot)
{
	return false;
}

static double
fastpg_mem_index_build_range_scan(Relation table_rel,
								  Relation index_rel,
								  IndexInfo *index_info,
								  bool allow_sync,
								  bool anyvisible,
								  bool progress,
								  BlockNumber start_blockno,
								  BlockNumber numblocks,
								  IndexBuildCallback callback,
								  void *callback_state,
								  TableScanDesc scan)
{
	fastpg_mem_unsupported("index builds");
	return 0;
}

static void
fastpg_mem_index_validate_scan(Relation table_rel,
							   Relation index_rel,
							   IndexInfo *index_info,
							   Snapshot snapshot,
							   ValidateIndexState *state)
{
	fastpg_mem_unsupported("index validation");
}

static uint64
fastpg_mem_relation_size(Relation rel, ForkNumber forkNumber)
{
	return 0;
}

static bool
fastpg_mem_relation_needs_toast_table(Relation rel)
{
	return false;
}

static Oid
fastpg_mem_relation_toast_am(Relation rel)
{
	return InvalidOid;
}

static void
fastpg_mem_relation_fetch_toast_slice(Relation toastrel,
									  Oid valueid,
									  int32 attrsize,
									  int32 sliceoffset,
									  int32 slicelength,
									  varlena *result)
{
	fastpg_mem_unsupported("TOAST fetch");
}

static void
fastpg_mem_relation_estimate_size(Relation rel,
								  int32 *attr_widths,
								  BlockNumber *pages,
								  double *tuples,
								  double *allvisfrac)
{
	size_t		row_count =
		fastpg_rust_relation_row_count(RelationGetRelid(rel));

	*tuples = row_count;
	*pages = *tuples == 0 ? 0 : 1;
	*allvisfrac = 1.0;
}

static bool
fastpg_mem_scan_bitmap_next_tuple(TableScanDesc scan,
								  TupleTableSlot *slot,
								  bool *recheck,
								  uint64 *lossy_pages,
								  uint64 *exact_pages)
{
	fastpg_mem_unsupported("bitmap scans");
	return false;
}

static bool
fastpg_mem_scan_sample_next_block(TableScanDesc scan,
								  SampleScanState *scanstate)
{
	return false;
}

static bool
fastpg_mem_scan_sample_next_tuple(TableScanDesc scan,
								  SampleScanState *scanstate,
								  TupleTableSlot *slot)
{
	return false;
}

static const TableAmRoutine fastpg_mem_methods = {
	.type = T_TableAmRoutine,

	.slot_callbacks = fastpg_mem_slot_callbacks,

	.scan_begin = fastpg_mem_scan_begin,
	.scan_end = fastpg_mem_scan_end,
	.scan_rescan = fastpg_mem_scan_rescan,
	.scan_getnextslot = fastpg_mem_scan_getnextslot,

	.scan_set_tidrange = NULL,
	.scan_getnextslot_tidrange = NULL,

	.parallelscan_estimate = fastpg_mem_parallelscan_estimate,
	.parallelscan_initialize = fastpg_mem_parallelscan_initialize,
	.parallelscan_reinitialize = fastpg_mem_parallelscan_reinitialize,

	.index_fetch_begin = fastpg_mem_index_fetch_begin,
	.index_fetch_reset = fastpg_mem_index_fetch_reset,
	.index_fetch_end = fastpg_mem_index_fetch_end,
	.index_fetch_tuple = fastpg_mem_index_fetch_tuple,

	.tuple_fetch_row_version = fastpg_mem_tuple_fetch_row_version,
	.tuple_get_latest_tid = fastpg_mem_tuple_get_latest_tid,
	.tuple_tid_valid = fastpg_mem_tuple_tid_valid,
	.tuple_satisfies_snapshot = fastpg_mem_tuple_satisfies_snapshot,
	.index_delete_tuples = fastpg_mem_index_delete_tuples,

	.tuple_insert = fastpg_mem_tuple_insert,
	.tuple_insert_speculative = fastpg_mem_tuple_insert_speculative,
	.tuple_complete_speculative = fastpg_mem_tuple_complete_speculative,
	.multi_insert = fastpg_mem_multi_insert,
	.tuple_delete = fastpg_mem_tuple_delete,
	.tuple_update = fastpg_mem_tuple_update,
	.tuple_lock = fastpg_mem_tuple_lock,

	.relation_set_new_filelocator = fastpg_mem_relation_set_new_filelocator,
	.relation_nontransactional_truncate = fastpg_mem_relation_nontransactional_truncate,
	.relation_copy_data = fastpg_mem_relation_copy_data,
	.relation_copy_for_cluster = fastpg_mem_relation_copy_for_cluster,
	.relation_vacuum = fastpg_mem_relation_vacuum,
	.scan_analyze_next_block = fastpg_mem_scan_analyze_next_block,
	.scan_analyze_next_tuple = fastpg_mem_scan_analyze_next_tuple,
	.index_build_range_scan = fastpg_mem_index_build_range_scan,
	.index_validate_scan = fastpg_mem_index_validate_scan,

	.relation_size = fastpg_mem_relation_size,
	.relation_needs_toast_table = fastpg_mem_relation_needs_toast_table,
	.relation_toast_am = fastpg_mem_relation_toast_am,
	.relation_fetch_toast_slice = fastpg_mem_relation_fetch_toast_slice,

	.relation_estimate_size = fastpg_mem_relation_estimate_size,

	.scan_bitmap_next_tuple = fastpg_mem_scan_bitmap_next_tuple,
	.scan_sample_next_block = fastpg_mem_scan_sample_next_block,
	.scan_sample_next_tuple = fastpg_mem_scan_sample_next_tuple,
};

const TableAmRoutine *
GetFastPgMemTableAmRoutine(void)
{
	return &fastpg_mem_methods;
}

#endif							/* USE_FASTPG */
