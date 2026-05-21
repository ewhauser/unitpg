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

#include <math.h>

#include "access/amapi.h"
#include "access/fastpg_catalog.h"
#include "access/fastpg_tableam.h"
#include "access/genam.h"
#include "access/htup_details.h"
#include "access/heaptoast.h"
#include "access/multixact.h"
#include "access/nbtree.h"
#include "access/reloptions.h"
#include "access/relscan.h"
#include "access/skey.h"
#include "access/tableam.h"
#include "access/xact.h"
#include "catalog/index.h"
#include "catalog/pg_attribute.h"
#include "catalog/pg_type.h"
#include "commands/vacuum.h"
#include "executor/executor.h"
#include "executor/tuptable.h"
#include "fmgr.h"
#include "miscadmin.h"
#include "nodes/pathnodes.h"
#include "nodes/primnodes.h"
#include "nodes/tidbitmap.h"
#include "optimizer/cost.h"
#include "optimizer/plancat.h"
#include "storage/bufpage.h"
#include "storage/off.h"
#include "storage/read_stream.h"
#include "utils/builtins.h"
#include "utils/elog.h"
#include "utils/errcodes.h"
#include "utils/index_selfuncs.h"
#include "utils/lsyscache.h"
#include "utils/memutils.h"
#include "utils/rel.h"
#include "utils/snapmgr.h"
#include "utils/tuplesort.h"

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#define FASTPG_MEM_STACK_NATTS 64
#define FASTPG_MEM_ROWS_PER_BLOCK ((uint64_t) TBM_MAX_TUPLES_PER_PAGE)
#define FASTPG_MEM_HEAP_OVERHEAD_BYTES_PER_TUPLE \
	(MAXALIGN(SizeofHeapTupleHeader) + sizeof(ItemIdData))
#define FASTPG_MEM_HEAP_USABLE_BYTES_PER_PAGE \
	(BLCKSZ - SizeOfPageHeaderData)

typedef struct FastPgMemScanDesc
{
	TableScanDescData base;
	uint64_t	scan_handle;
	bool		storage2;
	bool		analyze;
	size_t		analyze_row_count;
	size_t		analyze_rows_per_block;
	size_t		analyze_rows_returned;
	size_t		analyze_current_block_end;
	BlockNumber analyze_total_blocks;
	BlockNumber analyze_blocks_started;
	TBMIterateResult bitmap_result;
	OffsetNumber bitmap_offsets[TBM_MAX_TUPLES_PER_PAGE];
	int			bitmap_noffsets;
	int			bitmap_index;
	bool		bitmap_recheck;
} FastPgMemScanDesc;

typedef struct FastPgMemIndexFetch
{
	IndexFetchTableData base;
} FastPgMemIndexFetch;

typedef struct FastPgMemTouchedRow
{
	struct FastPgMemTouchedRow *next;
	uint32_t	relid;
	uint64_t	row_id;
	CommandId	cid;
	TransactionId xid;
} FastPgMemTouchedRow;

typedef struct FastPgMemVisibilityState
{
	struct FastPgMemVisibilityState *next;
	uint32_t	relid;
	bool		all_visible;
} FastPgMemVisibilityState;

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
extern uint64_t fastpg_rust_scan_begin_filtered(uint32_t relid,
												const int16_t *attnums,
												const uintptr_t *values,
												size_t nkeys);
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
extern bool fastpg_rust_fetch_row_any(uint32_t relid,
									  uint64_t row_id,
									  uintptr_t *values,
									  uint8_t *isnull,
									  size_t natts);
extern bool fastpg_rust_primary_key_index_lookup(uint32_t index_relid,
												 const uintptr_t *values,
												 const uint8_t *isnull,
												 size_t nkeys,
												 uint64_t *row_id);
extern bool fastpg_rust_primary_key_index_lookup_with_spec(uint32_t index_relid,
														   uint32_t heap_relid,
														   const int16_t *attnums,
														   const uint8_t *typbyval,
														   const int16_t *typlen,
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
extern bool fastpg_rust_unique_index_conflict_with_spec(uint32_t index_relid,
														uint32_t heap_relid,
														const int16_t *attnums,
														const uint8_t *typbyval,
														const int16_t *typlen,
														const uintptr_t *values,
														const uint8_t *isnull,
														size_t nkeys,
														uint8_t nulls_not_distinct,
														uint64_t replacing_row_id,
														uint64_t *row_id);
extern bool fastpg_rust_unique_index_validate_with_spec(uint32_t index_relid,
														uint32_t heap_relid,
														const int16_t *attnums,
														const uint8_t *typbyval,
														const int16_t *typlen,
														size_t nkeys,
														uint8_t nulls_not_distinct,
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
extern bool fastpg_storage2_rebuild_primary_key_index(uint32_t index_relid);
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
static MemoryContext fastpg_mem_touched_context = NULL;
static FastPgMemTouchedRow *fastpg_mem_touched_rows = NULL;
static MemoryContext fastpg_mem_visibility_context = NULL;
static FastPgMemVisibilityState *fastpg_mem_visibility_states = NULL;

typedef struct FastPgMemIndexScan
{
	bool		done;
	bool		unsupported;
	uintptr_t	values[FASTPG_MAX_INDEX_KEYS];
	uint8_t		isnull[FASTPG_MAX_INDEX_KEYS];
	uint8_t		key_seen[FASTPG_MAX_INDEX_KEYS];
	size_t		nkeys;
} FastPgMemIndexScan;

typedef struct FastPgMemIndexBuildState
{
	Relation	heap_relation;
	IndexInfo  *index_info;
	double		index_tuples;
	bool		validate_unique_once;
} FastPgMemIndexBuildState;

static bool fastpg_mem_index_insert(Relation indexRelation,
									Datum *values,
									bool *isnull,
									ItemPointer heap_tid,
									Relation heapRelation,
									IndexUniqueCheck checkUnique,
									bool indexUnchanged,
									IndexInfo *indexInfo);
static bool fastpg_mem_index_path_is_unique_equality(IndexPath *path);

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

static bytea *
fastpg_mem_index_options(Datum reloptions, bool validate)
{
	if (DatumGetPointer(reloptions) != NULL && validate)
		ereport(ERROR,
				(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
				 errmsg("fastpg_mem primary-key index does not support reloptions")));

	return NULL;
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
		!fastpg_catalog_mode_uses_postgres() &&
		fastpg_rust_catalog_policy_by_relation_oid(relid) == 0;
}

static bool
fastpg_mem_index_spec(Relation indexRelation,
					  Relation heapRelation,
					  int key_count,
					  int16_t *attnums,
					  uint8_t *typbyval,
					  int16_t *typlen)
{
	TupleDesc	tupdesc;

	if (indexRelation == NULL ||
		heapRelation == NULL ||
		indexRelation->rd_index == NULL ||
		key_count <= 0 ||
		key_count > FASTPG_MAX_INDEX_KEYS)
		return false;

	tupdesc = RelationGetDescr(heapRelation);
	for (int index = 0; index < key_count; index++)
	{
		AttrNumber	attnum = indexRelation->rd_index->indkey.values[index];
		Form_pg_attribute attr;

		if (attnum <= 0 || attnum > tupdesc->natts)
			return false;
		attr = TupleDescAttr(tupdesc, attnum - 1);
		if (attr->attisdropped)
			return false;
		attnums[index] = attnum;
		typbyval[index] = attr->attbyval ? 1 : 0;
		typlen[index] = attr->attlen;
	}
	return true;
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
fastpg_mem_reset_touched_rows(void)
{
	if (fastpg_mem_touched_context != NULL)
		MemoryContextReset(fastpg_mem_touched_context);
	fastpg_mem_touched_rows = NULL;
}

static FastPgMemVisibilityState *
fastpg_mem_relation_visibility_state(uint32_t relid, bool create)
{
	FastPgMemVisibilityState *entry;
	MemoryContext oldcontext;

	for (entry = fastpg_mem_visibility_states; entry != NULL;
		 entry = entry->next)
	{
		if (entry->relid == relid)
			return entry;
	}

	if (!create)
		return NULL;

	if (fastpg_mem_visibility_context == NULL)
		fastpg_mem_visibility_context =
			AllocSetContextCreate(TopMemoryContext,
								  "fastpg relation visibility state",
								  ALLOCSET_SMALL_SIZES);

	oldcontext = MemoryContextSwitchTo(fastpg_mem_visibility_context);
	entry = palloc0_object(FastPgMemVisibilityState);
	entry->relid = relid;
	entry->next = fastpg_mem_visibility_states;
	fastpg_mem_visibility_states = entry;
	MemoryContextSwitchTo(oldcontext);

	return entry;
}

static void
fastpg_mem_set_relation_all_visible(uint32_t relid, bool all_visible)
{
	FastPgMemVisibilityState *entry;

	if (!fastpg_catalog_mode_uses_postgres())
		return;

	entry = fastpg_mem_relation_visibility_state(relid, true);
	entry->all_visible = all_visible;
}

static bool
fastpg_mem_relation_is_all_visible(uint32_t relid)
{
	FastPgMemVisibilityState *entry;

	if (!fastpg_catalog_mode_uses_postgres())
		return false;

	entry = fastpg_mem_relation_visibility_state(relid, false);
	return entry != NULL && entry->all_visible;
}

static void
fastpg_mem_note_relation_changed(uint32_t relid)
{
	fastpg_mem_set_relation_all_visible(relid, false);
}

static bool
fastpg_mem_row_touched(uint32_t relid, uint64_t row_id, CommandId cid,
					   CommandId *touched_cid)
{
	FastPgMemTouchedRow *entry;
	TransactionId xid = GetCurrentTransactionIdIfAny();

	for (entry = fastpg_mem_touched_rows; entry != NULL; entry = entry->next)
	{
		if (entry->relid == relid &&
			entry->row_id == row_id &&
			entry->cid == cid &&
			entry->xid == xid)
		{
			if (touched_cid != NULL)
				*touched_cid = entry->cid;
			return true;
		}
	}

	return false;
}

void
FastPgMemResetCommandTouchedRows(void)
{
	fastpg_mem_reset_touched_rows();
}

static void
fastpg_mem_mark_row_touched(uint32_t relid, uint64_t row_id, CommandId cid)
{
	MemoryContext oldcontext;
	FastPgMemTouchedRow *entry;

	if (row_id == 0)
		return;
	if (fastpg_mem_row_touched(relid, row_id, cid, NULL))
		return;

	if (fastpg_mem_touched_context == NULL)
		fastpg_mem_touched_context =
			AllocSetContextCreate(TopMemoryContext,
								  "fastpg touched rows",
								  ALLOCSET_SMALL_SIZES);

	oldcontext = MemoryContextSwitchTo(fastpg_mem_touched_context);
	entry = palloc0_object(FastPgMemTouchedRow);
	entry->relid = relid;
	entry->row_id = row_id;
	entry->cid = cid;
	entry->xid = GetCurrentTransactionIdIfAny();
	entry->next = fastpg_mem_touched_rows;
	fastpg_mem_touched_rows = entry;
	MemoryContextSwitchTo(oldcontext);
}

static void
fastpg_mem_fill_self_modified_tmfd(ItemPointer tid, CommandId cmax,
								   TM_FailureData *tmfd)
{
	if (tmfd == NULL)
		return;

	tmfd->ctid = *tid;
	tmfd->xmax = GetCurrentTransactionIdIfAny();
	tmfd->cmax = cmax;
	tmfd->traversed = false;
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
			fastpg_mem_reset_touched_rows();
			break;
		case XACT_EVENT_ABORT:
		case XACT_EVENT_PARALLEL_ABORT:
			fastpg_rust_xact_abort();
			if (fastpg_mem_storage2_enabled())
				fastpg_storage2_xact_abort();
			fastpg_mem_reset_touched_rows();
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
	block = zero_index / FASTPG_MEM_ROWS_PER_BLOCK;
	if (block > UINT32_MAX)
		return false;

	offset = (OffsetNumber) (zero_index % FASTPG_MEM_ROWS_PER_BLOCK) +
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
	if (offset > (OffsetNumber) TBM_MAX_TUPLES_PER_PAGE)
		return false;

	*row_id = ((uint64_t) block * FASTPG_MEM_ROWS_PER_BLOCK) +
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

static double
fastpg_mem_heap_tuple_density(Relation rel, int32 *attr_widths)
{
	int32		tuple_width;
	int			fillfactor;
	double		density;

	fillfactor = RelationGetFillFactor(rel, HEAP_DEFAULT_FILLFACTOR);
	tuple_width = get_rel_data_width(rel, attr_widths);
	tuple_width += FASTPG_MEM_HEAP_OVERHEAD_BYTES_PER_TUPLE;
	if (tuple_width <= 0)
		tuple_width = 1;

	density = (FASTPG_MEM_HEAP_USABLE_BYTES_PER_PAGE * fillfactor / 100) /
		(double) tuple_width;
	if (density < 1.0)
		density = 1.0;
	return density;
}

static BlockNumber
fastpg_mem_heap_pages_for_row_count(Relation rel, int32 *attr_widths,
									size_t row_count,
									bool apply_never_vacuumed_minimum)
{
	double		density = fastpg_mem_heap_tuple_density(rel, attr_widths);
	BlockNumber pages = row_count == 0 ? 0 :
		(BlockNumber) ceil((double) row_count / density);

	if (pages == 0 && row_count > 0)
		pages = 1;

	if (apply_never_vacuumed_minimum &&
		pages < 10 &&
		rel->rd_rel->reltuples < 0 &&
		!rel->rd_rel->relhassubclass)
		pages = 10;

	return pages;
}

BlockNumber
FastPgMemRelationPages(Relation rel)
{
	uint32_t	relid = (uint32_t) RelationGetRelid(rel);
	size_t		row_count;

	row_count = fastpg_mem_use_storage2_for_relid(relid) ?
		fastpg_storage2_relation_row_count(RelationGetRelid(rel)) :
		fastpg_rust_relation_row_count(RelationGetRelid(rel));

	return fastpg_mem_heap_pages_for_row_count(rel, NULL, row_count, false);
}

BlockNumber
FastPgMemRelationAllVisiblePages(Relation rel)
{
	uint32_t	relid = (uint32_t) RelationGetRelid(rel);

	if (!fastpg_mem_relation_is_all_visible(relid))
		return 0;

	return FastPgMemRelationPages(rel);
}

static void
fastpg_mem_estimate_heap_size(Relation rel, int32 *attr_widths,
							  size_t row_count,
							  BlockNumber *pages,
							  double *tuples,
							  double *allvisfrac)
{
	BlockNumber curpages =
		fastpg_mem_heap_pages_for_row_count(rel, attr_widths, row_count, true);
	BlockNumber relpages = (BlockNumber) rel->rd_rel->relpages;
	double		reltuples = (double) rel->rd_rel->reltuples;
	BlockNumber relallvisible = (BlockNumber) rel->rd_rel->relallvisible;
	double		density;

	*pages = curpages;

	if (curpages == 0)
	{
		*tuples = 0;
		*allvisfrac = 0;
		return;
	}

	if (reltuples >= 0 && relpages > 0)
		density = reltuples / (double) relpages;
	else
		density = fastpg_mem_heap_tuple_density(rel, attr_widths);

	*tuples = rint(density * (double) curpages);

	if (relallvisible == 0 || curpages <= 0)
		*allvisfrac = 0;
	else if ((double) relallvisible >= curpages)
		*allvisfrac = 1;
	else
		*allvisfrac = (double) relallvisible / (double) curpages;
}

static void
fastpg_mem_free_slot_value_payloads(Relation rel,
									const uintptr_t *values,
									const uint8_t *isnull)
{
	TupleDesc	tupdesc = RelationGetDescr(rel);

	for (int index = 0; index < tupdesc->natts; index++)
	{
		Form_pg_attribute attr = TupleDescAttr(tupdesc, index);

		if (isnull[index] == 0 && attr->attlen == -1 && values[index] != 0)
			pfree((void *) values[index]);
	}
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
			if (attr->attlen == -1)
			{
				struct varlena *flat =
					(struct varlena *) PG_DETOAST_DATUM_COPY(slot->tts_values[index]);

				values[index] = (uintptr_t) flat;
				value_lens[index] = VARSIZE_ANY(flat);
			}
			else
			{
				values[index] = (uintptr_t) slot->tts_values[index];
				value_lens[index] =
					fastpg_mem_datum_size(slot->tts_values[index], attr);
			}
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

static uint64_t
fastpg_mem_scan_begin_storage1(Relation rel, int nkeys, ScanKeyData *key)
{
	uint32_t	relid = (uint32_t) RelationGetRelid(rel);
	int16_t		stack_attnums[FASTPG_MEM_STACK_NATTS];
	uintptr_t	stack_values[FASTPG_MEM_STACK_NATTS];
	int16_t    *attnums = stack_attnums;
	uintptr_t  *values = stack_values;
	size_t		filter_count = 0;
	uint64_t	scan_handle;
	bool		heap_buffers;

	if (nkeys <= 0 || key == NULL ||
		fastpg_rust_catalog_policy_by_relation_oid(relid) == 0)
		return fastpg_rust_scan_begin(relid);

	heap_buffers = nkeys > FASTPG_MEM_STACK_NATTS;
	if (heap_buffers)
	{
		attnums = palloc_array(int16_t, nkeys);
		values = palloc_array(uintptr_t, nkeys);
	}

	for (int index = 0; index < nkeys; index++)
	{
		if (key[index].sk_attno <= 0 ||
			key[index].sk_strategy != BTEqualStrategyNumber ||
			(key[index].sk_flags & SK_ISNULL) != 0)
			continue;

		attnums[filter_count] = (int16_t) key[index].sk_attno;
		values[filter_count] = (uintptr_t) key[index].sk_argument;
		filter_count++;
	}

	scan_handle = filter_count == 0 ?
		fastpg_rust_scan_begin(relid) :
		fastpg_rust_scan_begin_filtered(relid, attnums, values, filter_count);

	if (heap_buffers)
	{
		pfree(attnums);
		pfree(values);
	}
	return scan_handle;
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
	scan->analyze = (flags & SO_TYPE_ANALYZE) != 0;
	if (scan->analyze)
	{
		scan->analyze_row_count = scan->storage2 ?
			fastpg_storage2_relation_row_count(RelationGetRelid(rel)) :
			fastpg_rust_relation_row_count(RelationGetRelid(rel));
		scan->analyze_total_blocks =
			fastpg_mem_heap_pages_for_row_count(rel,
												NULL,
												scan->analyze_row_count,
												false);
		scan->analyze_rows_per_block =
			scan->analyze_total_blocks == 0 ? 1 :
			((scan->analyze_row_count + scan->analyze_total_blocks - 1) /
			 scan->analyze_total_blocks);
		if (scan->analyze_rows_per_block == 0)
			scan->analyze_rows_per_block = 1;
	}
	scan->scan_handle = scan->storage2 ?
		fastpg_storage2_scan_begin(RelationGetRelid(rel)) :
		fastpg_mem_scan_begin_storage1(rel, nkeys, key);
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
	scan->bitmap_noffsets = 0;
	scan->bitmap_index = 0;
	scan->bitmap_recheck = false;
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

	if (snapshot != NULL && snapshot->snapshot_type == SNAPSHOT_DIRTY)
	{
		snapshot->xmin = InvalidTransactionId;
		snapshot->xmax = InvalidTransactionId;
		snapshot->speculativeToken = 0;
	}

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
		(snapshot == SnapshotAny ?
		 fastpg_rust_fetch_row_any(RelationGetRelid(rel),
								   row_id,
								   values,
								   isnull,
								   natts) :
		 fastpg_rust_fetch_row(RelationGetRelid(rel),
							   row_id,
							   values,
							   isnull,
							   natts));
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

static void
fastpg_mem_index_build_callback(Relation index,
								ItemPointer tid,
								Datum *values,
								bool *isnull,
								bool tupleIsAlive,
								void *state)
{
	FastPgMemIndexBuildState *buildstate =
		(FastPgMemIndexBuildState *) state;
	IndexUniqueCheck checkUnique =
		(!buildstate->validate_unique_once &&
		 index->rd_index != NULL && index->rd_index->indisunique) ?
		UNIQUE_CHECK_YES : UNIQUE_CHECK_NO;

	if (!tupleIsAlive)
		return;

	fastpg_mem_index_insert(index,
							values,
							isnull,
							tid,
							buildstate->heap_relation,
							checkUnique,
							false,
							buildstate->index_info);
	buildstate->index_tuples += 1.0;
}

static char *
fastpg_mem_index_build_key_description(Relation heapRelation,
									   Relation indexRelation,
									   IndexInfo *indexInfo,
									   uint64_t row_id)
{
	ItemPointerData tid;
	TupleTableSlot *slot;
	EState	   *estate;
	Datum		values[INDEX_MAX_KEYS];
	bool		isnull[INDEX_MAX_KEYS];
	char	   *key_desc = NULL;

	if (!fastpg_mem_row_id_to_tid(row_id, &tid))
		return NULL;

	slot = table_slot_create(heapRelation, NULL);
	if (!table_tuple_fetch_row_version(heapRelation, &tid, SnapshotAny, slot))
	{
		ExecDropSingleTupleTableSlot(slot);
		return NULL;
	}

	estate = CreateExecutorState();
	FormIndexDatum(indexInfo, slot, estate, values, isnull);
	key_desc = BuildIndexValueDescription(indexRelation, values, isnull);
	FreeExecutorState(estate);
	ExecDropSingleTupleTableSlot(slot);
	return key_desc;
}

static IndexBuildResult *
fastpg_mem_index_build(Relation heapRelation, Relation indexRelation,
					   IndexInfo *indexInfo)
{
	IndexBuildResult *result = palloc0_object(IndexBuildResult);
	FastPgMemIndexBuildState buildstate;

	if (fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(heapRelation)))
		(void) fastpg_storage2_rebuild_primary_key_index((uint32_t) RelationGetRelid(indexRelation));

	buildstate.heap_relation = heapRelation;
	buildstate.index_info = indexInfo;
	buildstate.index_tuples = 0.0;
	buildstate.validate_unique_once = false;

	if (fastpg_catalog_mode_uses_postgres() &&
		indexRelation->rd_index != NULL &&
		indexRelation->rd_index->indisunique)
	{
		int			key_count =
			IndexRelationGetNumberOfKeyAttributes(indexRelation);
		int16_t		fastpg_attnums[FASTPG_MAX_INDEX_KEYS];
		uint8_t		fastpg_typbyval[FASTPG_MAX_INDEX_KEYS];
		int16_t		fastpg_typlen[FASTPG_MAX_INDEX_KEYS];
		uint64_t	conflict_row_id = 0;

		if (key_count <= 0 || key_count > FASTPG_MAX_INDEX_KEYS)
			fastpg_mem_index_unsupported("unique indexes with invalid key count");
		if (!fastpg_mem_index_spec(indexRelation,
								   heapRelation,
								   key_count,
								   fastpg_attnums,
								   fastpg_typbyval,
								   fastpg_typlen))
			fastpg_mem_index_unsupported("indexes with unsupported key metadata");

		if (fastpg_rust_unique_index_validate_with_spec((uint32_t) RelationGetRelid(indexRelation),
														(uint32_t) RelationGetRelid(heapRelation),
														fastpg_attnums,
														fastpg_typbyval,
														fastpg_typlen,
														(size_t) key_count,
														indexRelation->rd_index->indnullsnotdistinct ? 1 : 0,
														&conflict_row_id))
		{
			char	   *key_desc =
				fastpg_mem_index_build_key_description(heapRelation,
													   indexRelation,
													   indexInfo,
													   conflict_row_id);

			ereport(ERROR,
					(errcode(ERRCODE_UNIQUE_VIOLATION),
					 errmsg("could not create unique index \"%s\"",
							RelationGetRelationName(indexRelation)),
					 key_desc ? errdetail("Key %s is duplicated.",
										  key_desc) : 0,
					 errtableconstraint(heapRelation,
										RelationGetRelationName(indexRelation))));
		}

		buildstate.validate_unique_once = true;
	}

	result->heap_tuples = table_index_build_scan(heapRelation,
												 indexRelation,
												 indexInfo,
												 true,
												 true,
												 fastpg_mem_index_build_callback,
												 &buildstate,
												 NULL);
	result->index_tuples = buildstate.index_tuples;
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
		int16_t		fastpg_attnums[FASTPG_MAX_INDEX_KEYS];
		uint8_t		fastpg_typbyval[FASTPG_MAX_INDEX_KEYS];
		int16_t		fastpg_typlen[FASTPG_MAX_INDEX_KEYS];
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
		if (fastpg_catalog_mode_uses_postgres() &&
			!fastpg_mem_index_spec(indexRelation,
								   heapRelation,
								   key_count,
								   fastpg_attnums,
								   fastpg_typbyval,
								   fastpg_typlen))
			fastpg_mem_index_unsupported("indexes with unsupported key metadata");

		if ((fastpg_catalog_mode_uses_postgres() ?
			 fastpg_rust_unique_index_conflict_with_spec((uint32_t) RelationGetRelid(indexRelation),
														 (uint32_t) RelationGetRelid(heapRelation),
														 fastpg_attnums,
														 fastpg_typbyval,
														 fastpg_typlen,
														 fastpg_values,
														 fastpg_isnull,
														 (size_t) key_count,
														 indexRelation->rd_index->indnullsnotdistinct ? 1 : 0,
														 self_row_id,
														 &conflict_row_id) :
			 (storage2 ?
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
												&conflict_row_id))))
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

static bool
fastpg_mem_index_path_has_only_equality_keys(IndexPath *path)
{
	IndexOptInfo *index = path->indexinfo;
	ListCell   *lc;

	if (path->indexclauses == NIL || index == NULL)
		return false;

	foreach(lc, path->indexclauses)
	{
		IndexClause *iclause = lfirst_node(IndexClause, lc);
		ListCell   *qual_lc;
		int			indexcol = iclause->indexcol;

		if (indexcol < 0 || indexcol >= index->nkeycolumns)
			return false;

		foreach(qual_lc, iclause->indexquals)
		{
			RestrictInfo *rinfo = lfirst_node(RestrictInfo, qual_lc);
			Node	   *clause = rinfo->clause;
			Oid			clause_op;

			if (!IsA(clause, OpExpr))
				return false;

			clause_op = ((OpExpr *) clause)->opno;
			if (get_op_opfamily_strategy(clause_op,
										 index->opfamily[indexcol]) !=
				BTEqualStrategyNumber)
				return false;
		}
	}

	return true;
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
	if (!fastpg_mem_index_path_is_unique_equality(path))
	{
		*indexStartupCost = disable_cost;
		*indexTotalCost = disable_cost;
		*indexSelectivity = 1.0;
		*indexCorrelation = 0.0;
		*indexPages = 0.0;
		return;
	}

	if (fastpg_catalog_mode_uses_postgres())
	{
		btcostestimate(root, path, loop_count,
					   indexStartupCost,
					   indexTotalCost,
					   indexSelectivity,
					   indexCorrelation,
					   indexPages);
		return;
	}

	*indexStartupCost = 0.0;
	*indexTotalCost = 0.01;
	*indexSelectivity = 0.00001;
	*indexCorrelation = 1.0;
	*indexPages = 1.0;
}

static bool
fastpg_mem_index_validate(Oid opclassoid)
{
	return true;
}

static bool
fastpg_mem_index_path_is_unique_equality(IndexPath *path)
{
	IndexOptInfo *index;
	bool		seen[INDEX_MAX_KEYS] = {false};
	int			seen_count = 0;
	ListCell   *lc;

	if (path == NULL || path->indexinfo == NULL)
		return false;
	index = path->indexinfo;
	if (!index->unique ||
		index->nkeycolumns <= 0 ||
		index->nkeycolumns > INDEX_MAX_KEYS ||
		list_length(path->indexclauses) != index->nkeycolumns)
		return false;

	foreach(lc, path->indexclauses)
	{
		IndexClause *iclause = lfirst_node(IndexClause, lc);
		RestrictInfo *rinfo;
		OpExpr	   *op;

		if (iclause->indexcol < 0 ||
			iclause->indexcol >= index->nkeycolumns ||
			seen[iclause->indexcol] ||
			list_length(iclause->indexquals) != 1)
			return false;

		rinfo = linitial_node(RestrictInfo, iclause->indexquals);
		if (rinfo == NULL || rinfo->clause == NULL ||
			!IsA(rinfo->clause, OpExpr))
			return false;
		op = (OpExpr *) rinfo->clause;
		if (get_op_opfamily_strategy(op->opno,
									  index->opfamily[iclause->indexcol]) !=
			BTEqualStrategyNumber)
			return false;

		seen[iclause->indexcol] = true;
		seen_count++;
	}

	return seen_count == index->nkeycolumns;
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
	int16_t		fastpg_attnums[FASTPG_MAX_INDEX_KEYS];
	uint8_t		fastpg_typbyval[FASTPG_MAX_INDEX_KEYS];
	int16_t		fastpg_typlen[FASTPG_MAX_INDEX_KEYS];
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
	if (fastpg_catalog_mode_uses_postgres() &&
		!fastpg_mem_index_spec(indexRelation,
							   heapRelation,
							   key_count,
							   fastpg_attnums,
							   fastpg_typbyval,
							   fastpg_typlen))
		return false;

	conflict = fastpg_catalog_mode_uses_postgres() ?
		fastpg_rust_unique_index_conflict_with_spec((uint32_t) RelationGetRelid(indexRelation),
													(uint32_t) RelationGetRelid(heapRelation),
													fastpg_attnums,
													fastpg_typbyval,
													fastpg_typlen,
													fastpg_values,
													fastpg_isnull,
													(size_t) key_count,
													indexRelation->rd_index->indnullsnotdistinct ? 1 : 0,
													self_row_id,
													&conflict_row_id) :
		(storage2 ?
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
										   &conflict_row_id));

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
	int16_t		fastpg_attnums[FASTPG_MAX_INDEX_KEYS];
	uint8_t		fastpg_typbyval[FASTPG_MAX_INDEX_KEYS];
	int16_t		fastpg_typlen[FASTPG_MAX_INDEX_KEYS];
	bool		storage2 =
		fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(scan->indexRelation));

	if (ScanDirectionIsBackward(direction))
		fastpg_mem_index_unsupported("backward scans");
	if (opaque->unsupported || opaque->done)
		return false;
	opaque->done = true;

	if (fastpg_catalog_mode_uses_postgres() &&
		!fastpg_mem_index_spec(scan->indexRelation,
							   scan->heapRelation,
							   (int) opaque->nkeys,
							   fastpg_attnums,
							   fastpg_typbyval,
							   fastpg_typlen))
		fastpg_mem_index_unsupported("indexes with unsupported key metadata");

	if (!(fastpg_catalog_mode_uses_postgres() ?
		  fastpg_rust_primary_key_index_lookup_with_spec((uint32_t) RelationGetRelid(scan->indexRelation),
														 (uint32_t) RelationGetRelid(scan->heapRelation),
														 fastpg_attnums,
														 fastpg_typbyval,
														 fastpg_typlen,
														 opaque->values,
														 opaque->isnull,
														 opaque->nkeys,
														 &row_id) :
		  (storage2 ?
		   fastpg_storage2_primary_key_index_lookup((uint32_t) RelationGetRelid(scan->indexRelation),
													opaque->values,
													opaque->isnull,
													opaque->nkeys,
													&row_id) :
		   fastpg_rust_primary_key_index_lookup((uint32_t) RelationGetRelid(scan->indexRelation),
												opaque->values,
												opaque->isnull,
												opaque->nkeys,
												&row_id))))
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
		fastpg_mem_free_slot_value_payloads(rel, values, isnull);
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
	fastpg_mem_mark_row_touched((uint32_t) RelationGetRelid(rel), row_id, cid);
	fastpg_mem_note_relation_changed((uint32_t) RelationGetRelid(rel));
	fastpg_mem_free_slot_value_payloads(rel, values, isnull);
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
	uint64_t	row_id;
	bool		storage2;

	if (!succeeded)
	{
		storage2 =
			fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(rel));
		if (storage2)
		{
			row_id = fastpg_mem_tid_to_storage2_tid(&slot->tts_tid);
			if (row_id == 0)
				return;
			(void) fastpg_storage2_relation_delete(RelationGetRelid(rel),
												   row_id);
		}
		else
		{
			if (!fastpg_mem_tid_to_row_id(&slot->tts_tid, &row_id))
				return;
			(void) fastpg_rust_relation_delete(RelationGetRelid(rel),
											   row_id);
		}
		fastpg_mem_note_relation_changed((uint32_t) RelationGetRelid(rel));
		ItemPointerSetInvalid(&slot->tts_tid);
	}
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

	{
		CommandId	touched_cid;

		if (fastpg_mem_row_touched((uint32_t) RelationGetRelid(rel),
								   row_id,
								   cid,
								   &touched_cid))
		{
			fastpg_mem_fill_self_modified_tmfd(tid, touched_cid, tmfd);
			return TM_SelfModified;
		}
	}

	fastpg_mem_ensure_write_xact();
	if (!(storage2 ?
		  fastpg_storage2_relation_delete(RelationGetRelid(rel), row_id) :
		  fastpg_rust_relation_delete(RelationGetRelid(rel), row_id)))
	{
		fastpg_mem_fill_deleted_tmfd(tid, tmfd);
		return TM_Deleted;
	}

	fastpg_mem_mark_row_touched((uint32_t) RelationGetRelid(rel), row_id, cid);
	fastpg_mem_note_relation_changed((uint32_t) RelationGetRelid(rel));
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
		*update_indexes = (storage2 || fastpg_catalog_mode_uses_postgres()) ?
			TU_All : TU_None;
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

	{
		CommandId	touched_cid;

		if (fastpg_mem_row_touched((uint32_t) RelationGetRelid(rel),
								   row_id,
								   cid,
								   &touched_cid))
		{
			fastpg_mem_fill_self_modified_tmfd(otid, touched_cid, tmfd);
			return TM_SelfModified;
		}
	}

	fastpg_mem_ensure_write_xact();
	fastpg_mem_prepare_slot_values(rel, slot, &values, &isnull, &byval,
								   &value_lens);
	if (fastpg_catalog_mode_uses_postgres() && !storage2)
	{
		uint64_t	new_row_id = 0;

		if (!fastpg_rust_relation_delete(RelationGetRelid(rel), row_id) ||
			!fastpg_rust_relation_insert_unchecked(RelationGetRelid(rel),
												   values,
												   isnull,
												   byval,
												   value_lens,
												   tupdesc->natts,
												   &new_row_id))
		{
			fastpg_mem_free_slot_value_payloads(rel, values, isnull);
			pfree(values);
			pfree(isnull);
			pfree(byval);
			pfree(value_lens);
			if (fastpg_mem_has_storage_error())
				fastpg_mem_raise_storage_error("fastpg_mem failed to update row in Rust storage");
			fastpg_mem_fill_deleted_tmfd(otid, tmfd);
			return TM_Deleted;
		}
		row_id = new_row_id;
	}
	else if (!(storage2 ?
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
		fastpg_mem_free_slot_value_payloads(rel, values, isnull);
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
	fastpg_mem_free_slot_value_payloads(rel, values, isnull);
	pfree(values);
	pfree(isnull);
	pfree(byval);
	pfree(value_lens);

	fastpg_mem_mark_row_touched((uint32_t) RelationGetRelid(rel), row_id, cid);
	fastpg_mem_note_relation_changed((uint32_t) RelationGetRelid(rel));
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
	uint64_t	row_id;
	bool		storage2 = fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(rel));

	if (tmfd != NULL)
	{
		tmfd->ctid = *tid;
		tmfd->xmax = InvalidTransactionId;
		tmfd->cmax = InvalidCommandId;
		tmfd->traversed = false;
	}

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

	{
		CommandId	touched_cid;

		if (fastpg_mem_row_touched((uint32_t) RelationGetRelid(rel),
								   row_id,
								   cid,
								   &touched_cid))
		{
			fastpg_mem_fill_self_modified_tmfd(tid, touched_cid, tmfd);
			return TM_SelfModified;
		}
	}

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
	fastpg_mem_note_relation_changed((uint32_t) RelationGetRelid(rel));
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
	fastpg_mem_note_relation_changed((uint32_t) RelationGetRelid(rel));
}

static void
fastpg_mem_relation_copy_data(Relation rel, const RelFileLocator *newrlocator)
{
	/*
	 * FastPG user table storage is keyed by relation OID rather than
	 * relfilenode, so tablespace/relfilenode rewrites do not need a physical
	 * byte-for-byte copy here.
	 */
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
	TableScanDesc table_scan = NULL;
	IndexScanDesc index_scan = NULL;
	TupleTableSlot *old_slot;
	TupleTableSlot *new_slot;
	Snapshot	scan_snapshot = snapshot != NULL ? snapshot : SnapshotAny;
	CommandId	cid = GetCurrentCommandId(true);
	double		copied = 0.0;

	old_slot = table_slot_create(OldTable, NULL);
	new_slot = table_slot_create(NewTable, NULL);

	if (OldIndex != NULL && !use_sort)
	{
		index_scan = index_beginscan(OldTable,
									 OldIndex,
									 scan_snapshot,
									 NULL,
									 0,
									 0,
									 SO_NONE);
		index_rescan(index_scan, NULL, 0, NULL, 0);
		while (index_getnext_slot(index_scan, ForwardScanDirection, old_slot))
		{
			ExecCopySlot(new_slot, old_slot);
			table_tuple_insert(NewTable, new_slot, cid, 0, NULL);
			ExecClearTuple(old_slot);
			ExecClearTuple(new_slot);
			copied += 1.0;
		}
		index_endscan(index_scan);
	}
	else
	{
		table_scan = table_beginscan(OldTable,
									 scan_snapshot,
									 0,
									 (ScanKey) NULL,
									 SO_NONE);
		while (table_scan_getnextslot(table_scan, ForwardScanDirection, old_slot))
		{
			ExecCopySlot(new_slot, old_slot);
			table_tuple_insert(NewTable, new_slot, cid, 0, NULL);
			ExecClearTuple(old_slot);
			ExecClearTuple(new_slot);
			copied += 1.0;
		}
		table_endscan(table_scan);
	}

	ExecDropSingleTupleTableSlot(old_slot);
	ExecDropSingleTupleTableSlot(new_slot);

	if (num_tuples != NULL)
		*num_tuples = copied;
	if (tups_vacuumed != NULL)
		*tups_vacuumed = 0.0;
	if (tups_recently_dead != NULL)
		*tups_recently_dead = 0.0;
	if (xid_cutoff != NULL)
		*xid_cutoff = InvalidTransactionId;
	if (multi_cutoff != NULL)
		*multi_cutoff = InvalidMultiXactId;
}

static void
fastpg_mem_relation_vacuum(Relation rel,
						   const VacuumParams *params,
						   BufferAccessStrategy bstrategy)
{
	uint32_t	relid = (uint32_t) RelationGetRelid(rel);
	size_t		row_count;
	BlockNumber pages;

	if (!fastpg_catalog_mode_uses_postgres())
		return;

	row_count = fastpg_mem_use_storage2_for_relid(relid) ?
		fastpg_storage2_relation_row_count(RelationGetRelid(rel)) :
		fastpg_rust_relation_row_count(RelationGetRelid(rel));
	pages = fastpg_mem_heap_pages_for_row_count(rel, NULL, row_count, false);

	fastpg_mem_set_relation_all_visible(relid, pages > 0);
	rel->rd_rel->relpages = (int32) pages;
	rel->rd_rel->reltuples = (float4) row_count;
	rel->rd_rel->relallvisible = (int32) pages;

	vac_update_relstats(rel,
						pages,
						(double) row_count,
						pages,
						0,
						rel->rd_rel->relhasindex,
						InvalidTransactionId,
						InvalidMultiXactId,
						NULL,
						NULL,
						false);
}

static bool
fastpg_mem_scan_analyze_next_block(TableScanDesc scan, ReadStream *stream)
{
	FastPgMemScanDesc *fscan = (FastPgMemScanDesc *) scan;
	BlockNumber blockno;
	uint64_t	block_end;

	if (!fscan->analyze ||
		fscan->analyze_blocks_started >= fscan->analyze_total_blocks)
		return false;

	if (stream != NULL)
	{
		BufferAccessStrategy strategy;

		blockno = read_stream_next_block(stream, &strategy);
		if (blockno == InvalidBlockNumber)
			return false;
	}
	else
		blockno = fscan->analyze_blocks_started;

	fscan->analyze_blocks_started++;
	block_end = ((uint64_t) blockno + 1) *
		(uint64_t) fscan->analyze_rows_per_block;
	fscan->analyze_current_block_end =
		block_end > fscan->analyze_row_count ?
		fscan->analyze_row_count : (size_t) block_end;
	return fscan->analyze_rows_returned < fscan->analyze_current_block_end;
}

static bool
fastpg_mem_scan_analyze_next_tuple(TableScanDesc scan,
								   double *liverows,
								   double *deadrows,
								   TupleTableSlot *slot)
{
	FastPgMemScanDesc *fscan = (FastPgMemScanDesc *) scan;
	bool		found;

	if (!fscan->analyze ||
		fscan->analyze_rows_returned >= fscan->analyze_current_block_end)
		return false;

	found = fastpg_mem_scan_getnextslot(scan, ForwardScanDirection, slot);
	if (!found)
	{
		fscan->analyze_rows_returned = fscan->analyze_current_block_end;
		return false;
	}

	fscan->analyze_rows_returned++;
	*liverows += 1;
	return true;
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
	double		reltuples = 0;
	bool		need_endscan = false;
	Datum		values[INDEX_MAX_KEYS];
	bool		isnull[INDEX_MAX_KEYS];
	EState	   *estate;
	ExprContext *econtext;
	ExprState  *predicate;
	TupleTableSlot *slot;

	if (scan != NULL || start_blockno != 0 || numblocks != InvalidBlockNumber)
		fastpg_mem_unsupported("parallel or partial index builds");

	estate = CreateExecutorState();
	econtext = GetPerTupleExprContext(estate);
	slot = table_slot_create(table_rel, NULL);
	econtext->ecxt_scantuple = slot;
	predicate = ExecPrepareQual(index_info->ii_Predicate, estate);

	scan = table_beginscan_strat(table_rel,
								 GetTransactionSnapshot(),
								 0,
								 NULL,
								 true,
								 allow_sync);
	need_endscan = true;

	while (table_scan_getnextslot(scan, ForwardScanDirection, slot))
	{
		CHECK_FOR_INTERRUPTS();
		MemoryContextReset(econtext->ecxt_per_tuple_memory);
		reltuples += 1;

		if (predicate != NULL && !ExecQual(predicate, econtext))
		{
			ExecClearTuple(slot);
			continue;
		}

		FormIndexDatum(index_info, slot, estate, values, isnull);
		callback(index_rel, &slot->tts_tid, values, isnull, true,
				 callback_state);
		ExecClearTuple(slot);
	}

	if (need_endscan)
		table_endscan(scan);

	ExecDropSingleTupleTableSlot(slot);
	FreeExecutorState(estate);

	index_info->ii_ExpressionsState = NIL;
	index_info->ii_PredicateState = NULL;

	return reltuples;
}

static void
fastpg_mem_index_validate_scan(Relation table_rel,
							   Relation index_rel,
							   IndexInfo *index_info,
							   Snapshot snapshot,
							   ValidateIndexState *state)
{
	TableScanDesc scan;
	Datum		values[INDEX_MAX_KEYS];
	bool		isnull[INDEX_MAX_KEYS];
	ExprState  *predicate;
	TupleTableSlot *slot;
	EState	   *estate;
	ExprContext *econtext;
	ItemPointer indexcursor = NULL;
	ItemPointerData decoded;
	bool		tuplesort_empty = false;

	Assert(OidIsValid(index_rel->rd_rel->relam));

	estate = CreateExecutorState();
	econtext = GetPerTupleExprContext(estate);
	slot = table_slot_create(table_rel, NULL);
	econtext->ecxt_scantuple = slot;
	predicate = ExecPrepareQual(index_info->ii_Predicate, estate);

	tuplesort_empty = !tuplesort_getdatum(state->tuplesort, true,
										  false, &values[0], &isnull[0],
										  NULL);
	Assert(tuplesort_empty || !isnull[0]);
	if (!tuplesort_empty)
	{
		itemptr_decode(&decoded, DatumGetInt64(values[0]));
		indexcursor = &decoded;
	}

	scan = table_beginscan_strat(table_rel,
								 snapshot,
								 0,
								 NULL,
								 true,
								 false);

	while (table_scan_getnextslot(scan, ForwardScanDirection, slot))
	{
		ItemPointer heapcursor = &slot->tts_tid;

		CHECK_FOR_INTERRUPTS();
		state->htups += 1;

		while (!tuplesort_empty &&
			   indexcursor != NULL &&
			   ItemPointerCompare(indexcursor, heapcursor) < 0)
		{
			Datum		ts_val;
			bool		ts_isnull;

			tuplesort_empty = !tuplesort_getdatum(state->tuplesort, true,
												  false, &ts_val, &ts_isnull,
												  NULL);
			Assert(tuplesort_empty || !ts_isnull);
			if (!tuplesort_empty)
			{
				itemptr_decode(&decoded, DatumGetInt64(ts_val));
				indexcursor = &decoded;
			}
			else
				indexcursor = NULL;
		}

		if (tuplesort_empty ||
			indexcursor == NULL ||
			ItemPointerCompare(indexcursor, heapcursor) > 0)
		{
			MemoryContextReset(econtext->ecxt_per_tuple_memory);

			if (predicate != NULL && !ExecQual(predicate, econtext))
			{
				ExecClearTuple(slot);
				continue;
			}

			FormIndexDatum(index_info, slot, estate, values, isnull);
			index_insert(index_rel,
						 values,
						 isnull,
						 heapcursor,
						 table_rel,
						 index_info->ii_Unique ?
						 UNIQUE_CHECK_YES : UNIQUE_CHECK_NO,
						 false,
						 index_info);
			state->tups_inserted += 1;
		}

		ExecClearTuple(slot);
	}

	table_endscan(scan);
	ExecDropSingleTupleTableSlot(slot);
	FreeExecutorState(estate);

	index_info->ii_ExpressionsState = NIL;
	index_info->ii_PredicateState = NULL;
}

static uint64
fastpg_mem_relation_size(Relation rel, ForkNumber forkNumber)
{
	int32_t		relpages = 0;
	float4		reltuples = 0;
	uint32_t	relid = (uint32_t) RelationGetRelid(rel);
	size_t		row_count;
	BlockNumber pages;

	if (forkNumber == MAIN_FORKNUM &&
		fastpg_catalog_mode_uses_postgres() &&
		rel->rd_rel->reltuples >= 0 &&
		rel->rd_rel->relpages > 0)
		return (uint64) rel->rd_rel->relpages * BLCKSZ;

	if (forkNumber == MAIN_FORKNUM &&
		fastpg_use_rust_catalog() &&
		fastpg_rust_catalog_relation_planner_stats_by_oid(relid,
														  &relpages,
														  &reltuples))
		return (uint64) relpages * BLCKSZ;

	if (forkNumber != MAIN_FORKNUM)
		return 0;

	if (fastpg_use_rust_catalog() &&
		fastpg_rust_catalog_policy_by_relation_oid(relid) != 0)
		row_count = fastpg_rust_catalog_row_count(relid);
	else
		row_count = fastpg_mem_use_storage2_for_relid(relid) ?
			fastpg_storage2_relation_row_count(RelationGetRelid(rel)) :
			fastpg_rust_relation_row_count(RelationGetRelid(rel));

	pages = fastpg_mem_heap_pages_for_row_count(rel, NULL, row_count, false);

	return (uint64) pages * BLCKSZ;
}

static bool
fastpg_mem_relation_needs_toast_table(Relation rel)
{
	int32		data_length = 0;
	bool		maxlength_unknown = false;
	bool		has_toastable_attrs = false;
	TupleDesc	tupdesc = rel->rd_att;
	int32		tuple_length;
	int			i;

	if (!fastpg_catalog_mode_uses_postgres())
		return false;

	for (i = 0; i < tupdesc->natts; i++)
	{
		Form_pg_attribute att = TupleDescAttr(tupdesc, i);

		if (att->attisdropped)
			continue;
		if (att->attgenerated == ATTRIBUTE_GENERATED_VIRTUAL)
			continue;
		data_length = att_align_nominal(data_length, att->attalign);
		if (att->attlen > 0)
			data_length += att->attlen;
		else
		{
			int32		maxlen = type_maximum_size(att->atttypid,
												   att->atttypmod);

			if (maxlen < 0)
				maxlength_unknown = true;
			else
				data_length += maxlen;
			if (att->attstorage != TYPSTORAGE_PLAIN)
				has_toastable_attrs = true;
		}
	}

	if (!has_toastable_attrs)
		return false;
	if (maxlength_unknown)
		return true;
	tuple_length = MAXALIGN(SizeofHeapTupleHeader +
							BITMAPLEN(tupdesc->natts)) +
		MAXALIGN(data_length);
	return tuple_length > TOAST_TUPLE_THRESHOLD;
}

static Oid
fastpg_mem_relation_toast_am(Relation rel)
{
	if (!fastpg_catalog_mode_uses_postgres())
		return InvalidOid;
	return rel->rd_rel->relam;
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
	int32_t		relpages = 0;
	float4		reltuples = 0;
	uint32_t	relid = (uint32_t) RelationGetRelid(rel);
	size_t		row_count;

	if (fastpg_catalog_mode_uses_postgres() &&
		rel->rd_rel->reltuples >= 0 &&
		rel->rd_rel->relpages > 0)
	{
		BlockNumber catalog_relpages = (BlockNumber) rel->rd_rel->relpages;

		*pages = catalog_relpages;
		*tuples = (double) rel->rd_rel->reltuples;
		*allvisfrac = (double) FastPgMemRelationAllVisiblePages(rel) /
			(double) catalog_relpages;
		return;
	}

	if (fastpg_use_rust_catalog() &&
		fastpg_rust_catalog_relation_planner_stats_by_oid(relid,
														  &relpages,
														  &reltuples))
	{
		*tuples = reltuples;
		*pages = relpages;
		*allvisfrac = 1.0;
		return;
	}

	if (fastpg_use_rust_catalog() &&
		fastpg_rust_catalog_policy_by_relation_oid(relid) != 0)
		row_count = fastpg_rust_catalog_row_count(relid);
	else
		row_count = fastpg_mem_use_storage2_for_relid(relid) ?
			fastpg_storage2_relation_row_count(RelationGetRelid(rel)) :
			fastpg_rust_relation_row_count(RelationGetRelid(rel));

	fastpg_mem_estimate_heap_size(rel, attr_widths, row_count,
								  pages, tuples, allvisfrac);
	if (row_count > 0 && fastpg_mem_use_storage2_for_relid(relid))
		*pages = Max(*pages, 8);
}

static bool
fastpg_mem_scan_bitmap_next_tuple(TableScanDesc scan,
								  TupleTableSlot *slot,
								  bool *recheck,
								  uint64 *lossy_pages,
								  uint64 *exact_pages)
{
	FastPgMemScanDesc *fscan = (FastPgMemScanDesc *) scan;

	for (;;)
	{
		while (fscan->bitmap_index >= fscan->bitmap_noffsets)
		{
			if (!tbm_iterate(&scan->st.rs_tbmiterator,
							 &fscan->bitmap_result))
				return false;

			fscan->bitmap_index = 0;
			fscan->bitmap_recheck =
				fscan->bitmap_result.recheck || fscan->bitmap_result.lossy;
			if (fscan->bitmap_result.lossy)
			{
				fscan->bitmap_noffsets = TBM_MAX_TUPLES_PER_PAGE;
				if (lossy_pages != NULL)
					(*lossy_pages)++;
			}
			else
			{
				fscan->bitmap_noffsets =
					tbm_extract_page_tuple(&fscan->bitmap_result,
										   fscan->bitmap_offsets,
										   TBM_MAX_TUPLES_PER_PAGE);
				if (exact_pages != NULL)
					(*exact_pages)++;
			}
		}

		while (fscan->bitmap_index < fscan->bitmap_noffsets)
		{
			ItemPointerData tid;
			OffsetNumber offset;

			if (fscan->bitmap_result.lossy)
				offset = (OffsetNumber) (fscan->bitmap_index + FirstOffsetNumber);
			else
				offset = fscan->bitmap_offsets[fscan->bitmap_index];
			fscan->bitmap_index++;

			ItemPointerSet(&tid, fscan->bitmap_result.blockno, offset);
			if (fastpg_mem_tuple_fetch_row_version(scan->rs_rd,
												   &tid,
												   scan->rs_snapshot,
												   slot))
			{
				if (recheck != NULL)
					*recheck = fscan->bitmap_recheck;
				return true;
			}
		}
	}

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
	/*
	 * Fastpg indexes reuse normal btree operator classes.  Keep the relcache
	 * support-proc shape identical to btree so the shared opclass cache is not
	 * populated with a zero-support variant of the same opclass.
	 */
	.amsupport = BTNProcs,
	.amoptsprocnum = BTOPTIONS_PROC,
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
	.amoptions = fastpg_mem_index_options,
	.amproperty = NULL,
	.ambuildphasename = NULL,
	.amvalidate = fastpg_mem_index_validate,
	.amadjustmembers = NULL,
	.ambeginscan = fastpg_mem_index_begin_scan,
	.amrescan = fastpg_mem_index_rescan,
	.amgettuple = fastpg_mem_index_get_tuple,
	.amgetbitmap = NULL,
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
