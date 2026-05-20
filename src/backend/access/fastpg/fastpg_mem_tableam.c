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

#include "access/amapi.h"
#include "access/fastpg_catalog.h"
#include "access/fastpg_tableam.h"
#include "access/genam.h"
#include "access/multixact.h"
#include "access/nbtree.h"
#include "access/relscan.h"
#include "access/skey.h"
#include "access/tableam.h"
#include "access/xact.h"
#include "executor/tuptable.h"
#include "fmgr.h"
#include "nodes/pathnodes.h"
#include "storage/off.h"
#include "utils/elog.h"
#include "utils/errcodes.h"
#include "utils/rel.h"
#include "utils/snapmgr.h"

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#define FASTPG_MEM_STACK_NATTS 64

typedef struct FastPgMemScanDesc
{
	TableScanDescData base;
	uint64_t	scan_handle;
	bool		storage2;
} FastPgMemScanDesc;

typedef struct FastPgMemIndexFetch
{
	IndexFetchTableData base;
} FastPgMemIndexFetch;

extern void fastpg_rust_relation_clear(uint32_t relid);
extern size_t fastpg_rust_relation_row_count(uint32_t relid);
extern size_t fastpg_rust_catalog_row_count(uint32_t relid);
extern bool fastpg_rust_relation_insert(uint32_t relid,
										const uintptr_t *values,
										const uint8_t *isnull,
										const uint8_t *byval,
										const size_t *value_lens,
										size_t natts,
										uint64_t *row_id);
extern bool fastpg_rust_relation_insert_unchecked(uint32_t relid,
												  const uintptr_t *values,
												  const uint8_t *isnull,
												  const uint8_t *byval,
												  const size_t *value_lens,
												  size_t natts,
												  uint64_t *row_id);
extern bool fastpg_rust_relation_update(uint32_t relid,
										uint64_t row_id,
										const uintptr_t *values,
										const uint8_t *isnull,
										const uint8_t *byval,
										const size_t *value_lens,
										size_t natts);
extern bool fastpg_rust_relation_update_unchecked(uint32_t relid,
												  uint64_t row_id,
												  const uintptr_t *values,
												  const uint8_t *isnull,
												  const uint8_t *byval,
												  const size_t *value_lens,
												  size_t natts);
extern bool fastpg_rust_relation_delete(uint32_t relid, uint64_t row_id);
extern bool fastpg_rust_relation_contains_row(uint32_t relid,
											  uint64_t row_id);
extern uint64_t fastpg_rust_scan_begin(uint32_t relid);
extern void fastpg_rust_scan_reset(uint64_t scan_handle);
extern void fastpg_rust_scan_end(uint64_t scan_handle);
extern bool fastpg_rust_scan_next(uint64_t scan_handle,
								  uint8_t forward,
								  uintptr_t *values,
								  uint8_t *isnull,
								  size_t natts,
								  uint64_t *row_id);
extern bool fastpg_rust_fetch_row(uint32_t relid,
								  uint64_t row_id,
								  uintptr_t *values,
								  uint8_t *isnull,
								  size_t natts);
extern bool fastpg_rust_primary_key_index_lookup(uint32_t index_relid,
												 const uintptr_t *values,
												 const uint8_t *isnull,
												 size_t nkeys,
												 uint64_t *row_id);
extern bool fastpg_rust_unique_index_conflict(uint32_t index_relid,
											  const uintptr_t *values,
											  const uint8_t *isnull,
											  size_t nkeys,
											  uint64_t replacing_row_id,
											  uint64_t *row_id);
extern void fastpg_rust_xact_begin(void);
extern void fastpg_rust_xact_commit(void);
extern void fastpg_rust_xact_abort(void);
extern void fastpg_rust_subxact_begin(void);
extern void fastpg_rust_subxact_commit(void);
extern void fastpg_rust_subxact_abort(void);
extern bool fastpg_rust_storage_last_error(char *sqlstate_out,
										   size_t sqlstate_len,
										   char *message_out,
										   size_t message_len);

extern void fastpg_storage2_xact_begin(void);
extern void fastpg_storage2_xact_begin_implicit(void);
extern void fastpg_storage2_xact_commit(void);
extern void fastpg_storage2_xact_abort(void);
extern void fastpg_storage2_subxact_begin(void);
extern void fastpg_storage2_subxact_commit(void);
extern void fastpg_storage2_subxact_abort(void);
extern void fastpg_storage2_relation_clear(uint32_t relid);
extern size_t fastpg_storage2_relation_row_count(uint32_t relid);
extern bool fastpg_storage2_relation_contains_tid(uint32_t relid,
												  uint64_t tid);
extern bool fastpg_storage2_relation_insert(uint32_t relid,
											const uintptr_t *values,
											const uint8_t *isnull,
											const uint8_t *byval,
											const size_t *value_lens,
											size_t natts,
											uint64_t *tid);
extern bool fastpg_storage2_relation_insert_unchecked(uint32_t relid,
													  const uintptr_t *values,
													  const uint8_t *isnull,
													  const uint8_t *byval,
													  const size_t *value_lens,
													  size_t natts,
													  uint64_t *tid);
extern bool fastpg_storage2_relation_update(uint32_t relid,
											uint64_t tid,
											const uintptr_t *values,
											const uint8_t *isnull,
											const uint8_t *byval,
											const size_t *value_lens,
											size_t natts,
											uint64_t *new_tid);
extern bool fastpg_storage2_relation_update_unchecked(uint32_t relid,
													  uint64_t tid,
													  const uintptr_t *values,
													  const uint8_t *isnull,
													  const uint8_t *byval,
													  const size_t *value_lens,
													  size_t natts,
													  uint64_t *new_tid);
extern bool fastpg_storage2_relation_delete(uint32_t relid, uint64_t tid);
extern uint64_t fastpg_storage2_scan_begin(uint32_t relid);
extern void fastpg_storage2_scan_reset(uint64_t scan_handle);
extern void fastpg_storage2_scan_end(uint64_t scan_handle);
extern bool fastpg_storage2_scan_next(uint64_t scan_handle,
									  uint8_t forward,
									  uintptr_t *values,
									  uint8_t *isnull,
									  size_t natts,
									  uint64_t *tid);
extern bool fastpg_storage2_fetch_tid(uint32_t relid,
									  uint64_t tid,
									  uintptr_t *values,
									  uint8_t *isnull,
									  size_t natts);
extern bool fastpg_storage2_primary_key_index_lookup(uint32_t index_relid,
													 const uintptr_t *values,
													 const uint8_t *isnull,
													 size_t nkeys,
													 uint64_t *tid);
extern bool fastpg_storage2_unique_index_conflict(uint32_t index_relid,
												  const uintptr_t *values,
												  const uint8_t *isnull,
												  size_t nkeys,
												  uint64_t replacing_tid,
												  uint64_t *tid);
extern bool fastpg_storage2_last_error(char *sqlstate_out,
									   size_t sqlstate_len,
									   char *message_out,
									   size_t message_len);

static const TableAmRoutine fastpg_mem_methods;
static const IndexAmRoutine fastpg_mem_index_methods;
static bool fastpg_mem_xact_callbacks_registered = false;

typedef struct FastPgMemIndexScan
{
	bool		done;
	bool		unsupported;
	uintptr_t	values[FASTPG_MAX_INDEX_KEYS];
	uint8_t		isnull[FASTPG_MAX_INDEX_KEYS];
	uint8_t		key_seen[FASTPG_MAX_INDEX_KEYS];
	size_t		nkeys;
} FastPgMemIndexScan;

static void
fastpg_mem_unsupported(const char *operation)
{
	ereport(ERROR,
			(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
			 errmsg("fastpg_mem table access method does not support %s",
					operation)));
}

static void
fastpg_mem_index_unsupported(const char *operation)
{
	ereport(ERROR,
			(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
			 errmsg("fastpg_mem primary-key index does not support %s",
					operation)));
}

static bool
fastpg_mem_storage2_enabled(void)
{
	static int	cached = -1;
	const char *engine;

	if (cached >= 0)
		return cached == 1;

	engine = getenv("FASTPG_STORAGE_ENGINE");
	cached = (engine != NULL && strcmp(engine, "storage2") == 0) ? 1 : 0;
	return cached == 1;
}

static bool
fastpg_mem_use_storage2_for_relid(uint32_t relid)
{
	return fastpg_mem_storage2_enabled() &&
		fastpg_rust_catalog_policy_by_relation_oid(relid) == 0;
}

static int
fastpg_mem_sqlstate_to_errcode(const char sqlstate[6])
{
	if (sqlstate == NULL ||
		sqlstate[0] == '\0' ||
		sqlstate[1] == '\0' ||
		sqlstate[2] == '\0' ||
		sqlstate[3] == '\0' ||
		sqlstate[4] == '\0')
		return ERRCODE_INTERNAL_ERROR;

	return MAKE_SQLSTATE(sqlstate[0],
						 sqlstate[1],
						 sqlstate[2],
						 sqlstate[3],
						 sqlstate[4]);
}

static bool
fastpg_mem_get_storage_error(char sqlstate[6], char message[256])
{
	memset(sqlstate, 0, 6);
	memset(message, 0, 256);
	if (fastpg_mem_storage2_enabled() &&
		fastpg_storage2_last_error(sqlstate, 6, message, 256))
		return true;
	return fastpg_rust_storage_last_error(sqlstate, 6, message, 256);
}

static void
fastpg_mem_raise_storage_error(const char *fallback_message)
{
	char		sqlstate[6];
	char		message[256];

	if (fastpg_mem_get_storage_error(sqlstate, message))
		ereport(ERROR,
				(errcode(fastpg_mem_sqlstate_to_errcode(sqlstate)),
				 errmsg("%s", message)));

	elog(ERROR, "%s", fallback_message);
}

static bool
fastpg_mem_has_storage_error(void)
{
	char		sqlstate[6];
	char		message[256];

	return fastpg_mem_get_storage_error(sqlstate, message);
}

static void
fastpg_mem_xact_callback(XactEvent event, void *arg)
{
	switch (event)
	{
		case XACT_EVENT_COMMIT:
		case XACT_EVENT_PARALLEL_COMMIT:
		case XACT_EVENT_PREPARE:
			fastpg_rust_xact_commit();
			if (fastpg_mem_storage2_enabled())
				fastpg_storage2_xact_commit();
			break;
		case XACT_EVENT_ABORT:
		case XACT_EVENT_PARALLEL_ABORT:
			fastpg_rust_xact_abort();
			if (fastpg_mem_storage2_enabled())
				fastpg_storage2_xact_abort();
			break;
		default:
			break;
	}
}

static void
fastpg_mem_subxact_callback(SubXactEvent event, SubTransactionId mySubid,
							SubTransactionId parentSubid, void *arg)
{
	switch (event)
	{
		case SUBXACT_EVENT_START_SUB:
			fastpg_rust_subxact_begin();
			if (fastpg_mem_storage2_enabled())
				fastpg_storage2_subxact_begin();
			break;
		case SUBXACT_EVENT_COMMIT_SUB:
			fastpg_rust_subxact_commit();
			if (fastpg_mem_storage2_enabled())
				fastpg_storage2_subxact_commit();
			break;
		case SUBXACT_EVENT_ABORT_SUB:
			fastpg_rust_subxact_abort();
			if (fastpg_mem_storage2_enabled())
				fastpg_storage2_subxact_abort();
			break;
		default:
			break;
	}
}

static void
fastpg_mem_ensure_xact_callbacks(void)
{
	if (!fastpg_mem_xact_callbacks_registered)
	{
		RegisterXactCallback(fastpg_mem_xact_callback, NULL);
		RegisterSubXactCallback(fastpg_mem_subxact_callback, NULL);
		fastpg_mem_xact_callbacks_registered = true;
	}
}

static void
fastpg_mem_ensure_write_xact(void)
{
	fastpg_mem_ensure_xact_callbacks();
	fastpg_rust_xact_begin_implicit();
	if (fastpg_mem_storage2_enabled())
		fastpg_storage2_xact_begin_implicit();
}

static bool
fastpg_mem_row_id_to_tid(uint64_t row_id, ItemPointer tid)
{
	uint64_t	zero_index;
	uint64_t	block;
	OffsetNumber offset;

	if (row_id == 0)
		return false;

	zero_index = row_id - 1;
	block = zero_index / (uint64_t) MaxOffsetNumber;
	if (block > UINT32_MAX)
		return false;

	offset = (OffsetNumber) (zero_index % (uint64_t) MaxOffsetNumber) +
		FirstOffsetNumber;
	ItemPointerSet(tid, (BlockNumber) block, offset);
	return true;
}

static bool
fastpg_mem_tid_to_row_id(ItemPointer tid, uint64_t *row_id)
{
	BlockNumber block = ItemPointerGetBlockNumber(tid);
	OffsetNumber offset = ItemPointerGetOffsetNumber(tid);

	if (!OffsetNumberIsValid(offset))
		return false;

	*row_id = ((uint64_t) block * (uint64_t) MaxOffsetNumber) +
		(uint64_t) offset;
	return true;
}

static uint64_t
fastpg_mem_tid_to_storage2_tid(ItemPointer tid)
{
	BlockNumber block = ItemPointerGetBlockNumber(tid);
	OffsetNumber offset = ItemPointerGetOffsetNumber(tid);

	if (!OffsetNumberIsValid(offset))
		return 0;
	return (((uint64_t) block) << 16) | (uint64_t) offset;
}

static bool
fastpg_mem_storage2_tid_to_tid(uint64_t storage2_tid, ItemPointer tid)
{
	uint64_t	block = storage2_tid >> 16;
	OffsetNumber offset = (OffsetNumber) (storage2_tid & 0xffff);

	if (storage2_tid == 0 || block > UINT32_MAX || !OffsetNumberIsValid(offset))
		return false;
	ItemPointerSet(tid, (BlockNumber) block, offset);
	return true;
}

static size_t
fastpg_mem_datum_size(Datum value, Form_pg_attribute attr)
{
	if (attr->attbyval)
		return 0;
	if (attr->attlen > 0)
		return attr->attlen;
	if (attr->attlen == -1)
		return VARSIZE_ANY(DatumGetPointer(value));
	if (attr->attlen == -2)
		return strlen((const char *) DatumGetPointer(value)) + 1;

	elog(ERROR, "fastpg_mem found unsupported attribute length %d",
		 attr->attlen);
	return 0;
}

static void
fastpg_mem_prepare_slot_values(Relation rel,
							   TupleTableSlot *slot,
							   uintptr_t **values_out,
							   uint8_t **isnull_out,
							   uint8_t **byval_out,
							   size_t **value_lens_out)
{
	TupleDesc	tupdesc;
	uintptr_t  *values;
	uint8_t    *isnull;
	uint8_t    *byval;
	size_t	   *value_lens;

	slot_getallattrs(slot);
	tupdesc = RelationGetDescr(rel);
	values = palloc0_array(uintptr_t, tupdesc->natts);
	isnull = palloc0_array(uint8_t, tupdesc->natts);
	byval = palloc0_array(uint8_t, tupdesc->natts);
	value_lens = palloc0_array(size_t, tupdesc->natts);

	for (int index = 0; index < tupdesc->natts; index++)
	{
		Form_pg_attribute attr = TupleDescAttr(tupdesc, index);

		isnull[index] = slot->tts_isnull[index] ? 1 : 0;
		byval[index] = attr->attbyval ? 1 : 0;
		if (isnull[index] == 0)
		{
			values[index] = (uintptr_t) slot->tts_values[index];
			value_lens[index] =
				fastpg_mem_datum_size(slot->tts_values[index], attr);
		}
	}

	*values_out = values;
	*isnull_out = isnull;
	*byval_out = byval;
	*value_lens_out = value_lens;
}

static void
fastpg_mem_store_virtual_tuple(Relation rel,
							   TupleTableSlot *slot,
							   const uintptr_t *values,
							   const uint8_t *isnull,
							   uint64_t row_id)
{
	int			natts = slot->tts_tupleDescriptor->natts;

	for (int index = 0; index < natts; index++)
	{
		slot->tts_values[index] = (Datum) values[index];
		slot->tts_isnull[index] = isnull[index] != 0;
	}

	if (!fastpg_mem_row_id_to_tid(row_id, &slot->tts_tid))
		elog(ERROR, "fastpg_mem row id %llu cannot be represented as a CTID",
			 (unsigned long long) row_id);
	slot->tts_tableOid = RelationGetRelid(rel);
	ExecStoreVirtualTuple(slot);
}

static bool
fastpg_mem_slot_key_test(TupleTableSlot *slot, int nkeys, ScanKey keys)
{
	ScanKey		cur_key = keys;

	for (int cur_nkeys = nkeys; cur_nkeys--; cur_key++)
	{
		Datum		value;
		Datum		test;
		bool		isnull;

		if (cur_key->sk_flags & SK_ISNULL)
			return false;
		if (cur_key->sk_attno <= 0)
			fastpg_mem_unsupported("system-column scan keys");

		value = slot_getattr(slot, cur_key->sk_attno, &isnull);
		if (isnull)
			return false;

		test = FunctionCall2Coll(&cur_key->sk_func,
								 cur_key->sk_collation,
								 value,
								 cur_key->sk_argument);
		if (!DatumGetBool(test))
			return false;
	}

	return true;
}

static void
fastpg_mem_fill_deleted_tmfd(ItemPointer tid, TM_FailureData *tmfd)
{
	if (tmfd == NULL)
		return;

	tmfd->ctid = *tid;
	tmfd->xmax = InvalidTransactionId;
	tmfd->cmax = InvalidCommandId;
	tmfd->traversed = false;
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

	if (pscan != NULL)
		fastpg_mem_unsupported("parallel scans");

	scan = palloc0_object(FastPgMemScanDesc);
	if (nkeys != 0)
	{
		scan->base.rs_key = palloc_array(ScanKeyData, nkeys);
		memcpy(scan->base.rs_key, key, nkeys * sizeof(ScanKeyData));
	}
	else
		scan->base.rs_key = NULL;

	scan->base.rs_rd = rel;
	scan->base.rs_snapshot = snapshot;
	scan->base.rs_nkeys = nkeys;
	scan->base.rs_flags = flags;
	scan->storage2 = fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(rel));
	scan->scan_handle = scan->storage2 ?
		fastpg_storage2_scan_begin(RelationGetRelid(rel)) :
		fastpg_rust_scan_begin(RelationGetRelid(rel));
	if (scan->scan_handle == 0)
		fastpg_mem_raise_storage_error("fastpg_mem failed to create Rust scan handle");

	RelationIncrementReferenceCount(rel);

	return (TableScanDesc) scan;
}

static void
fastpg_mem_scan_end(TableScanDesc sscan)
{
	FastPgMemScanDesc *scan = (FastPgMemScanDesc *) sscan;

	RelationDecrementReferenceCount(scan->base.rs_rd);
	if (scan->storage2)
		fastpg_storage2_scan_end(scan->scan_handle);
	else
		fastpg_rust_scan_end(scan->scan_handle);
	if (scan->base.rs_flags & SO_TEMP_SNAPSHOT)
		UnregisterSnapshot(scan->base.rs_snapshot);
	if (scan->base.rs_key != NULL)
		pfree(scan->base.rs_key);
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

	if (key != NULL && scan->base.rs_nkeys > 0)
		memcpy(scan->base.rs_key, key, scan->base.rs_nkeys * sizeof(ScanKeyData));
	if (scan->storage2)
		fastpg_storage2_scan_reset(scan->scan_handle);
	else
		fastpg_rust_scan_reset(scan->scan_handle);
}

static bool
fastpg_mem_scan_getnextslot(TableScanDesc sscan,
							ScanDirection direction,
							TupleTableSlot *slot)
{
	FastPgMemScanDesc *scan = (FastPgMemScanDesc *) sscan;
	int			natts = slot->tts_tupleDescriptor->natts;
	uintptr_t	stack_values[FASTPG_MEM_STACK_NATTS];
	uint8_t		stack_isnull[FASTPG_MEM_STACK_NATTS];
	uintptr_t  *values;
	uint8_t    *isnull;
	uint64_t	row_id = 0;
	bool		found;
	bool		heap_buffers = natts > FASTPG_MEM_STACK_NATTS;

	ExecClearTuple(slot);

	values = heap_buffers ? palloc0_array(uintptr_t, natts) : stack_values;
	isnull = heap_buffers ? palloc0_array(uint8_t, natts) : stack_isnull;

	while ((found = scan->storage2 ?
			fastpg_storage2_scan_next(scan->scan_handle,
									  ScanDirectionIsBackward(direction) ? 0 : 1,
									  values,
									  isnull,
									  natts,
									  &row_id) :
			fastpg_rust_scan_next(scan->scan_handle,
								  ScanDirectionIsBackward(direction) ? 0 : 1,
								  values,
								  isnull,
								  natts,
								  &row_id)))
	{
		if (scan->storage2)
		{
			for (int index = 0; index < natts; index++)
			{
				slot->tts_values[index] = (Datum) values[index];
				slot->tts_isnull[index] = isnull[index] != 0;
			}
			if (!fastpg_mem_storage2_tid_to_tid(row_id, &slot->tts_tid))
				elog(ERROR, "fastpg_mem storage2 TID %llu cannot be represented as a CTID",
					 (unsigned long long) row_id);
			slot->tts_tableOid = RelationGetRelid(scan->base.rs_rd);
			ExecStoreVirtualTuple(slot);
		}
		else
			fastpg_mem_store_virtual_tuple(scan->base.rs_rd,
										   slot,
										   values,
										   isnull,
										   row_id);
		if (scan->base.rs_key == NULL ||
			fastpg_mem_slot_key_test(slot, scan->base.rs_nkeys, scan->base.rs_key))
			break;

		ExecClearTuple(slot);
	}

	if (heap_buffers)
	{
		pfree(values);
		pfree(isnull);
	}

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
	int			natts = slot->tts_tupleDescriptor->natts;
	uintptr_t	stack_values[FASTPG_MEM_STACK_NATTS];
	uint8_t		stack_isnull[FASTPG_MEM_STACK_NATTS];
	uintptr_t  *values;
	uint8_t    *isnull;
	uint64_t	row_id;
	bool		found;
	bool		heap_buffers = natts > FASTPG_MEM_STACK_NATTS;
	bool		storage2 = fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(rel));

	if (storage2)
	{
		row_id = fastpg_mem_tid_to_storage2_tid(tid);
		if (row_id == 0)
			return false;
	}
	else if (!fastpg_mem_tid_to_row_id(tid, &row_id))
		return false;

	ExecClearTuple(slot);
	values = heap_buffers ? palloc0_array(uintptr_t, natts) : stack_values;
	isnull = heap_buffers ? palloc0_array(uint8_t, natts) : stack_isnull;
	found = storage2 ?
		fastpg_storage2_fetch_tid(RelationGetRelid(rel),
								  row_id,
								  values,
								  isnull,
								  natts) :
		fastpg_rust_fetch_row(RelationGetRelid(rel),
							  row_id,
							  values,
							  isnull,
							  natts);
	if (found && storage2)
	{
		for (int index = 0; index < natts; index++)
		{
			slot->tts_values[index] = (Datum) values[index];
			slot->tts_isnull[index] = isnull[index] != 0;
		}
		slot->tts_tid = *tid;
		slot->tts_tableOid = RelationGetRelid(rel);
		ExecStoreVirtualTuple(slot);
	}
	else if (found)
		fastpg_mem_store_virtual_tuple(rel, slot, values, isnull, row_id);
	if (heap_buffers)
	{
		pfree(values);
		pfree(isnull);
	}

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
	uint64_t	row_id;

	if (fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(scan->rs_rd)))
	{
		uint64_t	storage2_tid = fastpg_mem_tid_to_storage2_tid(tid);

		if (storage2_tid == 0)
			return false;
		return fastpg_storage2_relation_contains_tid(RelationGetRelid(scan->rs_rd),
													 storage2_tid);
	}

	if (!fastpg_mem_tid_to_row_id(tid, &row_id))
		return false;

	return fastpg_rust_relation_contains_row(RelationGetRelid(scan->rs_rd),
											 row_id);
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

static IndexBuildResult *
fastpg_mem_index_build(Relation heapRelation, Relation indexRelation,
					   IndexInfo *indexInfo)
{
	IndexBuildResult *result = palloc0_object(IndexBuildResult);

	result->heap_tuples = 0.0;
	result->index_tuples = 0.0;
	return result;
}

static void
fastpg_mem_index_build_empty(Relation indexRelation)
{
}

static bool
fastpg_mem_index_insert(Relation indexRelation,
						Datum *values,
						bool *isnull,
						ItemPointer heap_tid,
						Relation heapRelation,
						IndexUniqueCheck checkUnique,
						bool indexUnchanged,
						IndexInfo *indexInfo)
{
	if (checkUnique != UNIQUE_CHECK_NO &&
		indexRelation->rd_index != NULL &&
		indexRelation->rd_index->indisunique)
	{
		int			key_count =
			IndexRelationGetNumberOfKeyAttributes(indexRelation);
		uintptr_t	fastpg_values[FASTPG_MAX_INDEX_KEYS];
		uint8_t		fastpg_isnull[FASTPG_MAX_INDEX_KEYS];
		uint64_t	self_row_id = 0;
		uint64_t	conflict_row_id = 0;
		bool		storage2 =
			fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(indexRelation));

		if (key_count <= 0 || key_count > FASTPG_MAX_INDEX_KEYS)
			fastpg_mem_index_unsupported("unique indexes with invalid key count");
		if (storage2)
		{
			self_row_id = fastpg_mem_tid_to_storage2_tid(heap_tid);
			if (self_row_id == 0)
				elog(ERROR, "fastpg_mem heap TID cannot be represented as a storage2 TID");
		}
		else if (!fastpg_mem_tid_to_row_id(heap_tid, &self_row_id))
			elog(ERROR, "fastpg_mem heap TID cannot be represented as a row id");
		for (int index = 0; index < key_count; index++)
		{
			fastpg_values[index] = (uintptr_t) values[index];
			fastpg_isnull[index] = isnull[index] ? 1 : 0;
		}

		if ((storage2 ?
			 fastpg_storage2_unique_index_conflict((uint32_t) RelationGetRelid(indexRelation),
												   fastpg_values,
												   fastpg_isnull,
												   (size_t) key_count,
												   self_row_id,
												   &conflict_row_id) :
			 fastpg_rust_unique_index_conflict((uint32_t) RelationGetRelid(indexRelation),
											   fastpg_values,
											   fastpg_isnull,
											   (size_t) key_count,
											   self_row_id,
											   &conflict_row_id)))
		{
			if (checkUnique == UNIQUE_CHECK_PARTIAL)
				return false;
			else
			{
				char	   *key_desc;

				key_desc = BuildIndexValueDescription(indexRelation, values,
													  isnull);
				ereport(ERROR,
						(errcode(ERRCODE_UNIQUE_VIOLATION),
						 errmsg("duplicate key value violates unique constraint \"%s\"",
								RelationGetRelationName(indexRelation)),
						 key_desc ? errdetail("Key %s already exists.",
											  key_desc) : 0,
						 errtableconstraint(heapRelation,
											RelationGetRelationName(indexRelation))));
			}
		}
	}

	return true;
}

static IndexBulkDeleteResult *
fastpg_mem_index_bulk_delete(IndexVacuumInfo *info,
							 IndexBulkDeleteResult *stats,
							 IndexBulkDeleteCallback callback,
							 void *callback_state)
{
	return stats;
}

static IndexBulkDeleteResult *
fastpg_mem_index_vacuum_cleanup(IndexVacuumInfo *info,
								IndexBulkDeleteResult *stats)
{
	return stats;
}

static void
fastpg_mem_index_cost_estimate(PlannerInfo *root,
							   IndexPath *path,
							   double loop_count,
							   Cost *indexStartupCost,
							   Cost *indexTotalCost,
							   Selectivity *indexSelectivity,
							   double *indexCorrelation,
							   double *indexPages)
{
	*indexStartupCost = 0.0;
	*indexTotalCost = 1.0;
	*indexSelectivity = 0.00001;
	*indexCorrelation = 1.0;
	*indexPages = 1.0;
}

static bool
fastpg_mem_index_validate(Oid opclassoid)
{
	return true;
}

bool
FastPgMemIndexCheckUniqueConflict(Relation heapRelation,
								  Relation indexRelation,
								  const Datum *values,
								  const bool *isnull,
								  const ItemPointerData *tupleid,
								  bool *satisfies,
								  ItemPointer conflictTid)
{
	int			key_count;
	uintptr_t	fastpg_values[FASTPG_MAX_INDEX_KEYS];
	uint8_t		fastpg_isnull[FASTPG_MAX_INDEX_KEYS];
	uint64_t	self_row_id = 0;
	uint64_t	conflict_row_id = 0;
	bool		storage2;
	bool		conflict;

	if (satisfies == NULL ||
		indexRelation->rd_indam != GetFastPgMemIndexAmRoutine() ||
		indexRelation->rd_index == NULL ||
		!indexRelation->rd_index->indisunique)
		return false;

	key_count = IndexRelationGetNumberOfKeyAttributes(indexRelation);
	if (key_count <= 0 || key_count > FASTPG_MAX_INDEX_KEYS)
		return false;

	storage2 = fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(indexRelation));
	if (tupleid != NULL && ItemPointerIsValid(tupleid))
	{
		if (storage2)
		{
			self_row_id = fastpg_mem_tid_to_storage2_tid((ItemPointer) tupleid);
			if (self_row_id == 0)
				return false;
		}
		else if (!fastpg_mem_tid_to_row_id((ItemPointer) tupleid, &self_row_id))
			return false;
	}

	for (int index = 0; index < key_count; index++)
	{
		fastpg_values[index] = (uintptr_t) values[index];
		fastpg_isnull[index] = isnull[index] ? 1 : 0;
	}

	conflict = storage2 ?
		fastpg_storage2_unique_index_conflict((uint32_t) RelationGetRelid(indexRelation),
											  fastpg_values,
											  fastpg_isnull,
											  (size_t) key_count,
											  self_row_id,
											  &conflict_row_id) :
		fastpg_rust_unique_index_conflict((uint32_t) RelationGetRelid(indexRelation),
										  fastpg_values,
										  fastpg_isnull,
										  (size_t) key_count,
										  self_row_id,
										  &conflict_row_id);

	if (!conflict)
	{
		*satisfies = true;
		if (conflictTid != NULL)
			ItemPointerSetInvalid(conflictTid);
		return true;
	}

	if (conflictTid != NULL)
	{
		if (storage2)
		{
			if (!fastpg_mem_storage2_tid_to_tid(conflict_row_id, conflictTid))
				elog(ERROR, "fastpg_mem storage2 TID %llu cannot be represented as a CTID",
					 (unsigned long long) conflict_row_id);
		}
		else if (!fastpg_mem_row_id_to_tid(conflict_row_id, conflictTid))
			elog(ERROR, "fastpg_mem row id %llu cannot be represented as a CTID",
				 (unsigned long long) conflict_row_id);
	}

	*satisfies = false;
	return true;
}

static IndexScanDesc
fastpg_mem_index_begin_scan(Relation indexRelation, int nkeys, int norderbys)
{
	IndexScanDesc scan;
	FastPgMemIndexScan *opaque;
	int			expected_keys;

	if (norderbys != 0)
		fastpg_mem_index_unsupported("ordered scans");
	if (indexRelation->rd_index == NULL)
		fastpg_mem_index_unsupported("indexes without pg_index metadata");

	expected_keys = IndexRelationGetNumberOfKeyAttributes(indexRelation);
	if (expected_keys <= 0 || expected_keys > FASTPG_MAX_INDEX_KEYS)
		fastpg_mem_index_unsupported("indexes with invalid key count");

	scan = RelationGetIndexScan(indexRelation, nkeys, norderbys);
	opaque = palloc0_object(FastPgMemIndexScan);
	opaque->nkeys = (size_t) expected_keys;
	scan->opaque = opaque;
	return scan;
}

static void
fastpg_mem_index_rescan(IndexScanDesc scan,
						ScanKey keys,
						int nkeys,
						ScanKey orderbys,
						int norderbys)
{
	FastPgMemIndexScan *opaque = (FastPgMemIndexScan *) scan->opaque;

	opaque->done = false;
	opaque->unsupported = false;
	memset(opaque->values, 0, sizeof(opaque->values));
	memset(opaque->isnull, 1, sizeof(opaque->isnull));
	memset(opaque->key_seen, 0, sizeof(opaque->key_seen));

	if (norderbys != 0)
		fastpg_mem_index_unsupported("ordered rescans");
	if (nkeys != (int) opaque->nkeys)
		fastpg_mem_index_unsupported("partial primary-key probes");
	if (nkeys > 0 && keys == NULL)
		fastpg_mem_index_unsupported("rescans without scan keys");

	for (int index = 0; index < nkeys; index++)
	{
		ScanKey		key = &keys[index];
		int			key_index = key->sk_attno - 1;

		if (key->sk_flags & (SK_SEARCHARRAY | SK_SEARCHNULL |
							 SK_SEARCHNOTNULL | SK_ORDER_BY |
							 SK_ROW_HEADER | SK_ROW_MEMBER))
			fastpg_mem_index_unsupported("non-scalar equality scan keys");
		if (key->sk_strategy != BTEqualStrategyNumber)
			fastpg_mem_index_unsupported("non-equality scan keys");
		if (key_index < 0 || key_index >= (int) opaque->nkeys)
			fastpg_mem_index_unsupported("scan keys outside the primary-key prefix");

		opaque->values[key_index] = (uintptr_t) key->sk_argument;
		opaque->isnull[key_index] =
			(key->sk_flags & SK_ISNULL) ? 1 : 0;
		opaque->key_seen[key_index] = 1;
	}

	for (size_t index = 0; index < opaque->nkeys; index++)
	{
		if (opaque->key_seen[index] == 0)
			fastpg_mem_index_unsupported("sparse primary-key probes");
	}
}

static bool
fastpg_mem_index_get_tuple(IndexScanDesc scan, ScanDirection direction)
{
	FastPgMemIndexScan *opaque = (FastPgMemIndexScan *) scan->opaque;
	uint64_t	row_id = 0;
	bool		storage2 =
		fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(scan->indexRelation));

	if (ScanDirectionIsBackward(direction))
		fastpg_mem_index_unsupported("backward scans");
	if (opaque->unsupported || opaque->done)
		return false;
	opaque->done = true;

	if (!(storage2 ?
		  fastpg_storage2_primary_key_index_lookup((uint32_t) RelationGetRelid(scan->indexRelation),
												   opaque->values,
												   opaque->isnull,
												   opaque->nkeys,
												   &row_id) :
		  fastpg_rust_primary_key_index_lookup((uint32_t) RelationGetRelid(scan->indexRelation),
											   opaque->values,
											   opaque->isnull,
											   opaque->nkeys,
											   &row_id)))
		return false;

	if (storage2)
	{
		if (!fastpg_mem_storage2_tid_to_tid(row_id, &scan->xs_heaptid))
			elog(ERROR, "fastpg_mem storage2 TID %llu cannot be represented as a CTID",
				 (unsigned long long) row_id);
	}
	else if (!fastpg_mem_row_id_to_tid(row_id, &scan->xs_heaptid))
		elog(ERROR, "fastpg_mem row id %llu cannot be represented as a CTID",
			 (unsigned long long) row_id);
	scan->xs_recheck = false;
	scan->xs_recheckorderby = false;
	return true;
}

static int64
fastpg_mem_index_get_bitmap(IndexScanDesc scan, TIDBitmap *tbm)
{
	fastpg_mem_index_unsupported("bitmap scans");
	return 0;
}

static void
fastpg_mem_index_end_scan(IndexScanDesc scan)
{
	if (scan->opaque != NULL)
	{
		pfree(scan->opaque);
		scan->opaque = NULL;
	}
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
	uint8_t    *byval;
	size_t	   *value_lens;
	uint64_t	row_id = 0;
	bool		storage2 = fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(rel));

	fastpg_mem_ensure_write_xact();
	fastpg_mem_prepare_slot_values(rel, slot, &values, &isnull, &byval,
								   &value_lens);
	if (!(storage2 ?
		  fastpg_storage2_relation_insert_unchecked(RelationGetRelid(rel),
													values,
													isnull,
													byval,
													value_lens,
													tupdesc->natts,
													&row_id) :
		  fastpg_rust_relation_insert_unchecked(RelationGetRelid(rel),
												values,
												isnull,
												byval,
												value_lens,
												tupdesc->natts,
												&row_id)))
	{
		pfree(values);
		pfree(isnull);
		pfree(byval);
		pfree(value_lens);
		fastpg_mem_raise_storage_error("fastpg_mem failed to insert row into Rust storage");
	}

	if (storage2)
	{
		if (!fastpg_mem_storage2_tid_to_tid(row_id, &slot->tts_tid))
			elog(ERROR, "fastpg_mem storage2 TID %llu cannot be represented as a CTID",
				 (unsigned long long) row_id);
	}
	else if (!fastpg_mem_row_id_to_tid(row_id, &slot->tts_tid))
		elog(ERROR, "fastpg_mem row id %llu cannot be represented as a CTID",
			 (unsigned long long) row_id);
	slot->tts_tableOid = RelationGetRelid(rel);
	pfree(values);
	pfree(isnull);
	pfree(byval);
	pfree(value_lens);
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
	uint64_t	row_id;
	bool		storage2 = fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(rel));

	if (storage2)
	{
		row_id = fastpg_mem_tid_to_storage2_tid(tid);
		if (row_id == 0)
		{
			fastpg_mem_fill_deleted_tmfd(tid, tmfd);
			return TM_Deleted;
		}
	}
	else if (!fastpg_mem_tid_to_row_id(tid, &row_id))
	{
		fastpg_mem_fill_deleted_tmfd(tid, tmfd);
		return TM_Deleted;
	}

	fastpg_mem_ensure_write_xact();
	if (!(storage2 ?
		  fastpg_storage2_relation_delete(RelationGetRelid(rel), row_id) :
		  fastpg_rust_relation_delete(RelationGetRelid(rel), row_id)))
	{
		fastpg_mem_fill_deleted_tmfd(tid, tmfd);
		return TM_Deleted;
	}

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
	TupleDesc	tupdesc = RelationGetDescr(rel);
	uintptr_t  *values;
	uint8_t    *isnull;
	uint8_t    *byval;
	size_t	   *value_lens;
	uint64_t	row_id;
	bool		storage2 = fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(rel));

	if (update_indexes != NULL)
		*update_indexes = storage2 ? TU_All : TU_None;
	if (lockmode != NULL)
		*lockmode = LockTupleExclusive;

	if (storage2)
	{
		row_id = fastpg_mem_tid_to_storage2_tid(otid);
		if (row_id == 0)
		{
			fastpg_mem_fill_deleted_tmfd(otid, tmfd);
			return TM_Deleted;
		}
	}
	else if (!fastpg_mem_tid_to_row_id(otid, &row_id))
	{
		fastpg_mem_fill_deleted_tmfd(otid, tmfd);
		return TM_Deleted;
	}

	fastpg_mem_ensure_write_xact();
	fastpg_mem_prepare_slot_values(rel, slot, &values, &isnull, &byval,
								   &value_lens);
	if (!(storage2 ?
		  fastpg_storage2_relation_update_unchecked(RelationGetRelid(rel),
													row_id,
													values,
													isnull,
													byval,
													value_lens,
													tupdesc->natts,
													&row_id) :
		  fastpg_rust_relation_update_unchecked(RelationGetRelid(rel),
												row_id,
												values,
												isnull,
												byval,
												value_lens,
												tupdesc->natts)))
	{
		pfree(values);
		pfree(isnull);
		pfree(byval);
		pfree(value_lens);
		if (fastpg_mem_has_storage_error())
			fastpg_mem_raise_storage_error("fastpg_mem failed to update row in Rust storage");
		fastpg_mem_fill_deleted_tmfd(otid, tmfd);
		return TM_Deleted;
	}

	if (storage2)
	{
		if (!fastpg_mem_storage2_tid_to_tid(row_id, &slot->tts_tid))
			elog(ERROR, "fastpg_mem storage2 TID %llu cannot be represented as a CTID",
				 (unsigned long long) row_id);
	}
	else if (!fastpg_mem_row_id_to_tid(row_id, &slot->tts_tid))
		elog(ERROR, "fastpg_mem row id %llu cannot be represented as a CTID",
			 (unsigned long long) row_id);
	slot->tts_tableOid = RelationGetRelid(rel);
	pfree(values);
	pfree(isnull);
	pfree(byval);
	pfree(value_lens);

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
	if (!fastpg_mem_tuple_fetch_row_version(rel, tid, snapshot, slot))
	{
		fastpg_mem_fill_deleted_tmfd(tid, tmfd);
		return TM_Deleted;
	}
	return TM_Ok;
}

static void
fastpg_mem_relation_set_new_filelocator(Relation rel,
										const RelFileLocator *newrlocator,
										char persistence,
										TransactionId *freezeXid,
										MultiXactId *minmulti)
{
	if (fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(rel)))
		fastpg_storage2_relation_clear(RelationGetRelid(rel));
	else
		fastpg_rust_relation_clear(RelationGetRelid(rel));
	*freezeXid = InvalidTransactionId;
	*minmulti = InvalidMultiXactId;
}

static void
fastpg_mem_relation_nontransactional_truncate(Relation rel)
{
	if (fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(rel)))
		fastpg_storage2_relation_clear(RelationGetRelid(rel));
	else
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
	size_t		row_count;
	uint32_t	relid = (uint32_t) RelationGetRelid(rel);

	if (fastpg_rust_catalog_policy_by_relation_oid(relid) != 0)
		row_count = fastpg_rust_catalog_row_count(relid);
	else
		row_count = fastpg_mem_use_storage2_for_relid(relid) ?
			fastpg_storage2_relation_row_count(relid) :
			fastpg_rust_relation_row_count(relid);

	*tuples = row_count;
	*pages = row_count == 0 ? 0 :
		(BlockNumber) (((uint64_t) row_count +
						(uint64_t) MaxOffsetNumber - 1) /
					   (uint64_t) MaxOffsetNumber);
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

static const IndexAmRoutine fastpg_mem_index_methods = {
	.type = T_IndexAmRoutine,
	.amstrategies = BTMaxStrategyNumber,
	.amsupport = 0,
	.amoptsprocnum = 0,
	.amcanorder = false,
	.amcanorderbyop = false,
	.amcanhash = false,
	.amconsistentequality = true,
	.amconsistentordering = false,
	.amcanbackward = false,
	.amcanunique = true,
	.amcanmulticol = true,
	.amoptionalkey = false,
	.amsearcharray = false,
	.amsearchnulls = false,
	.amstorage = false,
	.amclusterable = false,
	.ampredlocks = true,
	.amcanparallel = false,
	.amcanbuildparallel = false,
	.amcaninclude = false,
	.amusemaintenanceworkmem = false,
	.amsummarizing = false,
	.amparallelvacuumoptions = 0,
	.amkeytype = InvalidOid,

	.ambuild = fastpg_mem_index_build,
	.ambuildempty = fastpg_mem_index_build_empty,
	.aminsert = fastpg_mem_index_insert,
	.aminsertcleanup = NULL,
	.ambulkdelete = fastpg_mem_index_bulk_delete,
	.amvacuumcleanup = fastpg_mem_index_vacuum_cleanup,
	.amcanreturn = NULL,
	.amcostestimate = fastpg_mem_index_cost_estimate,
	.amgettreeheight = NULL,
	.amoptions = NULL,
	.amproperty = NULL,
	.ambuildphasename = NULL,
	.amvalidate = fastpg_mem_index_validate,
	.amadjustmembers = NULL,
	.ambeginscan = fastpg_mem_index_begin_scan,
	.amrescan = fastpg_mem_index_rescan,
	.amgettuple = fastpg_mem_index_get_tuple,
	.amgetbitmap = fastpg_mem_index_get_bitmap,
	.amendscan = fastpg_mem_index_end_scan,
	.ammarkpos = NULL,
	.amrestrpos = NULL,
	.amestimateparallelscan = NULL,
	.aminitparallelscan = NULL,
	.amparallelrescan = NULL,
	.amtranslatestrategy = NULL,
	.amtranslatecmptype = NULL,
};

const TableAmRoutine *
GetFastPgMemTableAmRoutine(void)
{
	fastpg_mem_ensure_xact_callbacks();
	return &fastpg_mem_methods;
}

const IndexAmRoutine *
GetFastPgMemIndexAmRoutine(void)
{
	fastpg_mem_ensure_xact_callbacks();
	return &fastpg_mem_index_methods;
}

#endif							/* USE_FASTPG */
