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
#include "access/hash.h"
#include "access/heapam.h"
#include "access/htup_details.h"
#include "access/heaptoast.h"
#include "access/multixact.h"
#include "access/nbtree.h"
#include "access/reloptions.h"
#include "access/relscan.h"
#include "access/skey.h"
#include "access/tableam.h"
#include "access/tsmapi.h"
#include "access/xact.h"
#include "catalog/index.h"
#include "catalog/catalog.h"
#include "catalog/pg_am_d.h"
#include "catalog/pg_attribute.h"
#include "catalog/pg_class.h"
#include "catalog/pg_constraint.h"
#include "catalog/pg_index.h"
#include "catalog/pg_type.h"
#include "commands/vacuum.h"
#include "executor/executor.h"
#include "executor/instrument.h"
#include "executor/tuptable.h"
#include "fmgr.h"
#include "miscadmin.h"
#include "nodes/bitmapset.h"
#include "nodes/pathnodes.h"
#include "nodes/primnodes.h"
#include "nodes/tidbitmap.h"
#include "optimizer/cost.h"
#include "optimizer/optimizer.h"
#include "optimizer/plancat.h"
#include "pgstat.h"
#include "storage/bufpage.h"
#include "storage/off.h"
#include "storage/predicate.h"
#include "storage/read_stream.h"
#include "utils/builtins.h"
#include "utils/catcache.h"
#include "utils/datum.h"
#include "utils/elog.h"
#include "utils/errcodes.h"
#include "utils/fmgroids.h"
#include "utils/hsearch.h"
#include "utils/index_selfuncs.h"
#include "utils/inval.h"
#include "utils/array.h"
#include "utils/lsyscache.h"
#include "utils/memutils.h"
#include "utils/rel.h"
#include "utils/relcache.h"
#include "utils/snapmgr.h"
#include "utils/timestamp.h"
#include "utils/tuplesort.h"

#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>

#define FASTPG_MEM_STACK_NATTS 64
#define FASTPG_MEM_SCAN_BATCH_ROWS 128
#define FASTPG_MEM_INLINE_TOUCHED_ROWS 16
#define FASTPG_MEM_ROW_LOCK_BUCKETS 4096
#define FASTPG_MEM_MAX_ROWS_PER_BLOCK ((uint64_t) TBM_MAX_TUPLES_PER_PAGE)
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
	BlockNumber analyze_total_blocks;
	BlockNumber analyze_blocks_started;
	BlockNumber analyze_current_block;
	OffsetNumber analyze_current_offset;
	OffsetNumber analyze_current_max_offset;
	TBMIterateResult bitmap_result;
	OffsetNumber bitmap_offsets[TBM_MAX_TUPLES_PER_PAGE];
	int			bitmap_noffsets;
	int			bitmap_index;
	bool		bitmap_recheck;
	uintptr_t  *batch_values;
	uint8_t    *batch_isnull;
	uint64_t   *batch_row_ids;
	size_t	   *batch_stored_natts;
	uint32_t   *batch_xmins;
	uint32_t   *batch_cmins;
	MemoryContext batch_context;
	int			batch_natts;
	int			batch_count;
	int			batch_index;
	bool		batch_forward;
	bool		batch_enabled;
	bool		batch_exhausted_forward;
	bool		batch_exhausted_backward;
	bool		cache_single_index_key;
	AttrNumber	single_index_attnum;
	BlockNumber sample_block;
	BlockNumber sample_nblocks;
} FastPgMemScanDesc;

typedef struct FastPgMemIndexFetch
{
	IndexFetchTableData base;
} FastPgMemIndexFetch;

typedef struct FastPgMemLastIndexKey
{
	bool		valid;
	uint32_t	relid;
	uint64_t	row_id;
	TransactionId xid;
	AttrNumber	attnum;
	uintptr_t	value;
	uint8_t		isnull;
} FastPgMemLastIndexKey;

typedef struct FastPgMemTouchedRowKey
{
	uint64_t	row_id;
	uint32_t	relid;
	CommandId	cid;
	TransactionId xid;
} FastPgMemTouchedRowKey;

typedef struct FastPgMemTouchedRowHashEntry
{
	FastPgMemTouchedRowKey key;
	CommandId	cid;
} FastPgMemTouchedRowHashEntry;

typedef struct FastPgMemRowRedirect
{
	struct FastPgMemRowRedirect *next;
	uint32_t	relid;
	uint64_t	old_row_id;
	uint64_t	new_row_id;
	TransactionId xid;
} FastPgMemRowRedirect;

typedef struct FastPgMemStorage2LockRoot
{
	struct FastPgMemStorage2LockRoot *next;
	uint32_t	relid;
	uint64_t	root_row_id;
	uint64_t	resolved_row_id;
} FastPgMemStorage2LockRoot;

typedef struct FastPgMemVisibilityState
{
	struct FastPgMemVisibilityState *next;
	uint32_t	relid;
	bool		all_visible;
	bool		known_empty;
	TransactionId touched_xid;
	CommandId	max_touched_cid;
} FastPgMemVisibilityState;

typedef struct FastPgMemRowLockEntry
{
	struct FastPgMemRowLockEntry *next;
	pthread_mutex_t mutex;
	uint64_t	row_id;
	uint32_t	relid;
} FastPgMemRowLockEntry;

typedef struct FastPgMemBlockLayout
{
	struct FastPgMemBlockLayout *next;
	uint32_t	relid;
	uint64_t	rows_per_block;
	bool		storage2_rows_per_block_applied;
	TupleDesc	fits_without_toast_tupdesc;
	bool		fits_without_toast_valid;
	bool		fits_without_toast;
	TupleDesc	single_update_index_tupdesc;
	Bitmapset  *single_update_hotblockingattr;
	Bitmapset  *single_update_summarizedattr;
	Bitmapset  *single_update_keyattr;
	Bitmapset  *single_update_idattr;
	bool		single_update_index_attr_valid;
	bool		single_update_index_attr_result;
	AttrNumber	single_update_index_attr;
} FastPgMemBlockLayout;

typedef struct FastPgMemToastState
{
	struct FastPgMemToastState *next;
	uint32_t	relid;
	bool		may_have_external;
} FastPgMemToastState;

typedef struct FastPgMemIndexMatch
{
	uint64_t	row_id;
	Datum		values[FASTPG_MAX_INDEX_KEYS];
	bool		isnull[FASTPG_MAX_INDEX_KEYS];
	bool		owned[FASTPG_MAX_INDEX_KEYS];
} FastPgMemIndexMatch;

typedef struct FastPgMemIndexSortContext
{
	Relation	index_relation;
	FmgrInfo   *order_procs[FASTPG_MAX_INDEX_KEYS];
} FastPgMemIndexSortContext;

extern void fastpg_rust_relation_clear(uint32_t relid);
extern void fastpg_rust_relation_clear_transactional(uint32_t relid);
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
extern size_t fastpg_rust_relation_multi_insert_unchecked(uint32_t relid,
														  const uintptr_t *values,
														  const uint8_t *isnull,
														  const uint8_t *byval,
														  const size_t *value_lens,
														  size_t natts,
														  size_t nrows,
														  uint64_t *row_ids);
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
extern bool fastpg_rust_relation_update_with_metadata(uint32_t relid,
													  uint64_t row_id,
													  uint32_t delete_xid,
													  uint32_t delete_cid,
													  const uintptr_t *values,
													  const uint8_t *isnull,
													  const uint8_t *byval,
													  const size_t *value_lens,
													  size_t natts);
extern bool fastpg_rust_relation_delete(uint32_t relid, uint64_t row_id);
extern bool fastpg_rust_relation_delete_with_metadata(uint32_t relid,
													  uint64_t row_id,
													  uint32_t delete_xid,
													  uint32_t delete_cid);
extern bool fastpg_rust_relation_contains_row(uint32_t relid,
											  uint64_t row_id);
extern uint64_t fastpg_rust_scan_begin(uint32_t relid);
extern uint64_t fastpg_rust_scan_begin_filtered(uint32_t relid,
												const int16_t *attnums,
												const uintptr_t *values,
												size_t nkeys);
extern uint64_t fastpg_rust_scan_begin_with_snapshot(uint32_t relid,
													 uint8_t has_snapshot,
													 uint32_t current_xid,
													 uint32_t curcid);
extern void fastpg_rust_scan_reset(uint64_t scan_handle);
extern void fastpg_rust_scan_end(uint64_t scan_handle);
extern bool fastpg_rust_scan_next(uint64_t scan_handle,
								  uint8_t forward,
								  uintptr_t *values,
								  uint8_t *isnull,
								  size_t natts,
								  uint64_t *row_id);
extern bool fastpg_rust_scan_next_with_stored_natts(uint64_t scan_handle,
													uint8_t forward,
													uintptr_t *values,
													uint8_t *isnull,
													size_t natts,
													uint64_t *row_id,
													size_t *stored_natts);
extern bool fastpg_rust_scan_next_with_metadata(uint64_t scan_handle,
												uint8_t forward,
												uintptr_t *values,
												uint8_t *isnull,
												size_t natts,
												uint64_t *row_id,
												size_t *stored_natts,
												uint32_t *xmin,
												uint32_t *cmin);
extern size_t fastpg_rust_scan_next_batch_with_stored_natts(uint64_t scan_handle,
															uint8_t forward,
															uintptr_t *values,
															uint8_t *isnull,
															size_t natts,
															size_t max_rows,
															uint64_t *row_ids,
															size_t *stored_natts);
extern size_t fastpg_rust_scan_next_batch_with_metadata(uint64_t scan_handle,
														uint8_t forward,
														uintptr_t *values,
														uint8_t *isnull,
														size_t natts,
														size_t max_rows,
														uint64_t *row_ids,
														size_t *stored_natts,
														uint32_t *xmins,
														uint32_t *cmins);
extern bool fastpg_rust_fetch_row(uint32_t relid,
								  uint64_t row_id,
								  uintptr_t *values,
								  uint8_t *isnull,
								  size_t natts);
extern bool fastpg_rust_fetch_row_with_stored_natts(uint32_t relid,
													uint64_t row_id,
													uintptr_t *values,
													uint8_t *isnull,
													size_t natts,
													size_t *stored_natts);
extern bool fastpg_rust_fetch_row_with_snapshot_stored_natts(uint32_t relid,
															 uint64_t row_id,
															 uint8_t has_snapshot,
															 uint32_t current_xid,
															 uint32_t curcid,
															 uintptr_t *values,
															 uint8_t *isnull,
															 size_t natts,
															 size_t *stored_natts,
															 uint32_t *xmin,
															 uint32_t *cmin);
extern bool fastpg_rust_fetch_row_any(uint32_t relid,
									  uint64_t row_id,
									  uintptr_t *values,
									  uint8_t *isnull,
									  size_t natts);
extern bool fastpg_rust_fetch_row_any_with_stored_natts(uint32_t relid,
														uint64_t row_id,
														uintptr_t *values,
														uint8_t *isnull,
														size_t natts,
														size_t *stored_natts);
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
extern bool fastpg_rust_relation_set_row_xmin(uint32_t relid,
											  uint64_t row_id,
											  uint32_t xmin,
											  uint32_t cmin);
extern bool fastpg_rust_relation_set_row_xmax(uint32_t relid,
											  uint64_t row_id,
											  uint32_t xmax);
extern uint32_t fastpg_rust_relation_row_xmin(uint32_t relid, uint64_t row_id);
extern uint32_t fastpg_rust_relation_row_cmin(uint32_t relid, uint64_t row_id);
extern uint32_t fastpg_rust_relation_row_delete_xid(uint32_t relid,
													uint64_t row_id);
extern uint32_t fastpg_rust_relation_row_delete_cid(uint32_t relid,
													uint64_t row_id);

extern void fastpg_storage2_xact_begin(void);
extern void fastpg_storage2_xact_begin_implicit(void);
extern void fastpg_storage2_xact_commit(void);
extern void fastpg_storage2_xact_abort(void);
extern void fastpg_storage2_xact_commit_if_implicit(void);
extern void fastpg_storage2_xact_abort_if_implicit(void);
extern void fastpg_storage2_subxact_begin(void);
extern void fastpg_storage2_subxact_commit(void);
extern void fastpg_storage2_subxact_abort(void);
extern void fastpg_storage2_relation_clear(uint32_t relid);
extern size_t fastpg_storage2_relation_row_count(uint32_t relid);
extern bool fastpg_storage2_relation_row_count_if_visibility_deltas(uint32_t relid,
																	size_t *row_count);
extern size_t fastpg_storage2_relation_page_count(uint32_t relid);
extern size_t fastpg_storage2_relation_block_count(uint32_t relid);
extern uint32_t fastpg_storage2_relation_row_xmin(uint32_t relid,
												  uint64_t packed_tid);
extern void fastpg_storage2_relation_set_max_tuples_per_block(uint32_t relid,
															  uint16_t max_tuples);
extern uint16_t fastpg_storage2_relation_block_max_offset(uint32_t relid,
														  uint32_t block);
extern bool fastpg_storage2_relation_visible_tid_at(uint32_t relid,
													size_t zero_based_index,
													uint64_t *tid_out);
extern bool fastpg_storage2_relation_contains_tid(uint32_t relid,
												  uint64_t tid);
extern bool fastpg_storage2_relation_current_session_owns_inserted_tid(uint32_t relid,
																	   uint64_t tid);
extern bool fastpg_storage2_relation_current_session_visible_tid(uint32_t relid,
																 uint64_t tid,
																 uint8_t use_curcid,
																 uint32_t curcid,
																 uint64_t *resolved_tid);
extern bool fastpg_storage2_relation_index_tid_all_dead(uint32_t relid,
														uint64_t tid);
extern bool fastpg_storage2_relation_resolve_tid(uint32_t relid,
												 uint64_t tid,
												 uint64_t *resolved_tid);
extern bool fastpg_storage2_relation_resolve_tid_read(uint32_t relid,
													  uint64_t tid,
													  uint64_t *resolved_tid);
extern bool fastpg_storage2_relation_resolve_update_tid(uint32_t relid,
														uint64_t tid,
														uint64_t *resolved_tid);
extern bool fastpg_storage2_relation_resolve_update_tid_read(uint32_t relid,
															 uint64_t tid,
															 uint64_t *resolved_tid);
extern bool fastpg_storage2_relation_record_insert_metadata(uint32_t relid,
															uint64_t tid,
															uint32_t xid,
															uint32_t cid);
extern bool fastpg_storage2_relation_record_invalidate_metadata(uint32_t relid,
																uint64_t tid,
																uint32_t xid,
																uint32_t cid);
extern bool fastpg_storage2_relation_record_row_xmax(uint32_t relid,
													 uint64_t tid,
													 uint32_t xmax);
extern uint32_t fastpg_storage2_relation_row_delete_xid(uint32_t relid,
														uint64_t tid);
extern uint32_t fastpg_storage2_relation_row_delete_cid(uint32_t relid,
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
extern bool fastpg_storage2_relation_insert_unchecked_with_metadata(uint32_t relid,
																	uint32_t xid,
																	uint32_t cid,
																	const uintptr_t *values,
																	const uint8_t *isnull,
																	const uint8_t *byval,
																	const size_t *value_lens,
																	size_t natts,
																	uint64_t *tid);
extern bool fastpg_storage2_relation_insert_unchecked_no_index_with_metadata(uint32_t relid,
																			 uint32_t xid,
																			 uint32_t cid,
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
extern bool fastpg_storage2_relation_update_unchecked_with_metadata(uint32_t relid,
																	uint64_t tid,
																	uint32_t delete_xid,
																	uint32_t delete_cid,
																	uint32_t insert_xid,
																	uint32_t insert_cid,
																	uint32_t row_xmax,
																	const uintptr_t *values,
																	const uint8_t *isnull,
																	const uint8_t *byval,
																	const size_t *value_lens,
																	size_t natts,
																	uint64_t *new_tid);
extern bool fastpg_storage2_relation_update_redirect_unchecked(uint32_t relid,
															   uint64_t tid,
															   const uintptr_t *values,
															   const uint8_t *isnull,
															   const uint8_t *byval,
															   const size_t *value_lens,
															   size_t natts,
															   uint64_t *new_tid);
extern bool fastpg_storage2_relation_update_hot_unchecked(uint32_t relid,
														  uint64_t tid,
														  const uintptr_t *values,
														  const uint8_t *isnull,
														  const uint8_t *byval,
														  const size_t *value_lens,
														  size_t natts,
														  uint64_t *new_tid);
extern bool fastpg_storage2_relation_update_hot_unchecked_with_metadata(uint32_t relid,
																		uint64_t tid,
																		uint32_t delete_xid,
																		uint32_t delete_cid,
																		uint32_t insert_xid,
																		uint32_t insert_cid,
																		uint32_t row_xmax,
																		const uintptr_t *values,
																		const uint8_t *isnull,
																		const uint8_t *byval,
																		const size_t *value_lens,
																		size_t natts,
																		uint64_t *new_tid);
extern bool fastpg_storage2_relation_update_hot_if_single_byval_preserved_with_metadata(uint32_t relid,
																						uint64_t tid,
																						size_t key_attnum,
																						uintptr_t key_value,
																						uint8_t key_isnull,
																						uint32_t delete_xid,
																						uint32_t delete_cid,
																						uint32_t insert_xid,
																						uint32_t insert_cid,
																						uint32_t row_xmax,
																						const uintptr_t *values,
																						const uint8_t *isnull,
																						const uint8_t *byval,
																						const size_t *value_lens,
																						size_t natts,
																						uint64_t *new_tid,
																						bool *hot_preserved);
extern bool fastpg_storage2_relation_delete(uint32_t relid, uint64_t tid);
extern uint64_t fastpg_storage2_scan_begin(uint32_t relid);
extern uint64_t fastpg_storage2_scan_begin_with_snapshot(uint32_t relid,
														 uint32_t curcid);
extern void fastpg_storage2_scan_reset(uint64_t scan_handle);
extern bool fastpg_storage2_scan_set_position(uint64_t scan_handle,
											  uint64_t packed_tid);
extern void fastpg_storage2_scan_end(uint64_t scan_handle);
extern bool fastpg_storage2_scan_next_with_stored_natts(uint64_t scan_handle,
														uint8_t forward,
														uintptr_t *values,
														uint8_t *isnull,
														size_t natts,
														uint64_t *tid,
														size_t *stored_natts);
extern size_t fastpg_storage2_scan_next_batch_with_stored_natts(uint64_t scan_handle,
																uint8_t forward,
																uintptr_t *values,
																uint8_t *isnull,
																size_t natts,
																size_t max_rows,
																uint64_t *tids,
																size_t *stored_natts);
extern bool fastpg_storage2_fetch_tid_with_stored_natts(uint32_t relid,
														uint64_t tid,
														uintptr_t *values,
														uint8_t *isnull,
														size_t natts,
														size_t *stored_natts);
extern bool fastpg_storage2_fetch_resolved_tid_with_stored_natts(uint32_t relid,
																 uint64_t tid,
																 uintptr_t *values,
																 uint8_t *isnull,
																 size_t natts,
																 size_t *stored_natts);
	extern bool fastpg_storage2_fetch_tid_any_with_stored_natts(uint32_t relid,
																uint64_t tid,
																uintptr_t *values,
																uint8_t *isnull,
																size_t natts,
																size_t *stored_natts);
	extern bool fastpg_storage2_fetch_tid_snapshot_with_stored_natts(uint32_t relid,
																	 uint64_t tid,
																	 uint32_t curcid,
																	 uintptr_t *values,
																	 uint8_t *isnull,
																	 size_t natts,
																	 size_t *stored_natts);
extern bool fastpg_storage2_fetch_current_session_tid_with_stored_natts(uint32_t relid,
																		uint64_t tid,
																		uint8_t use_curcid,
																		uint32_t curcid,
																		uintptr_t *values,
																		uint8_t *isnull,
																		size_t natts,
																		size_t *stored_natts,
																		uint64_t *resolved_tid);
extern bool fastpg_storage2_primary_key_index_lookup(uint32_t index_relid,
													 const uintptr_t *values,
													 const uint8_t *isnull,
													 size_t nkeys,
													 uint64_t *tid);
extern bool fastpg_storage2_primary_key_index_lookup_with_spec(uint32_t index_relid,
															   uint32_t heap_relid,
															   const int16_t *attnums,
															   const uint8_t *typbyval,
															   const int16_t *typlen,
															   const uintptr_t *values,
															   const uint8_t *isnull,
															   size_t nkeys,
															   uint64_t *tid);
extern bool fastpg_storage2_primary_key_index_lookup_single_byval_with_spec(uint32_t index_relid,
																			uint32_t heap_relid,
																			uintptr_t value,
																			uint8_t isnull,
																			uint64_t *tid);
extern bool fastpg_storage2_primary_key_index_insert_with_spec(uint32_t index_relid,
															   uint32_t heap_relid,
															   const int16_t *attnums,
															   const uint8_t *typbyval,
															   const int16_t *typlen,
															   const uintptr_t *values,
															   const uint8_t *isnull,
															   size_t nkeys,
															   uint64_t tid);
extern bool fastpg_storage2_rebuild_primary_key_index(uint32_t index_relid);
extern bool fastpg_storage2_rebuild_primary_key_index_with_spec(uint32_t index_relid,
																uint32_t heap_relid,
																const int16_t *attnums,
																const uint8_t *typbyval,
																const int16_t *typlen,
																size_t nkeys);
extern bool fastpg_storage2_unique_index_conflict(uint32_t index_relid,
												  const uintptr_t *values,
												  const uint8_t *isnull,
												  size_t nkeys,
												  uint64_t replacing_tid,
												  uint64_t *tid);
extern bool fastpg_storage2_unique_index_conflict_with_spec(uint32_t index_relid,
															uint32_t heap_relid,
															const int16_t *attnums,
															const uint8_t *typbyval,
															const int16_t *typlen,
															const uintptr_t *values,
															const uint8_t *isnull,
															size_t nkeys,
															uint8_t is_primary,
															uint8_t nulls_not_distinct,
															uint64_t replacing_tid,
															uint64_t *tid);
extern bool fastpg_storage2_unique_index_validate_with_spec(uint32_t index_relid,
															uint32_t heap_relid,
															const int16_t *attnums,
															const uint8_t *typbyval,
															const int16_t *typlen,
															size_t nkeys,
															uint8_t nulls_not_distinct,
															uint64_t *tid);
extern bool fastpg_storage2_last_error(char *sqlstate_out,
									   size_t sqlstate_len,
									   char *message_out,
									   size_t message_len);

static const TableAmRoutine fastpg_mem_methods;
static const IndexAmRoutine fastpg_mem_index_methods;
#ifdef USE_FASTPG
static _Thread_local bool fastpg_mem_xact_callbacks_registered = false;
static _Thread_local MemoryContext fastpg_mem_touched_context = NULL;
static _Thread_local HTAB *fastpg_mem_touched_hash = NULL;
static _Thread_local FastPgMemTouchedRowHashEntry fastpg_mem_touched_inline[FASTPG_MEM_INLINE_TOUCHED_ROWS];
static _Thread_local int fastpg_mem_touched_inline_count = 0;
static _Thread_local MemoryContext fastpg_mem_redirect_context = NULL;
static _Thread_local FastPgMemRowRedirect *fastpg_mem_row_redirects = NULL;
static _Thread_local MemoryContext fastpg_mem_storage2_lock_root_context = NULL;
static _Thread_local FastPgMemStorage2LockRoot *fastpg_mem_storage2_lock_roots = NULL;
static _Thread_local MemoryContext fastpg_mem_visibility_context = NULL;
static _Thread_local FastPgMemVisibilityState *fastpg_mem_visibility_states = NULL;
static _Thread_local MemoryContext fastpg_mem_block_layout_context = NULL;
static _Thread_local FastPgMemBlockLayout *fastpg_mem_block_layouts = NULL;
static _Thread_local FastPgMemLastIndexKey fastpg_mem_last_index_key = {0};
static _Thread_local FastPgMemRowLockEntry **fastpg_mem_held_row_locks = NULL;
static _Thread_local int fastpg_mem_held_row_lock_count = 0;
static _Thread_local int fastpg_mem_held_row_lock_capacity = 0;
static pthread_mutex_t fastpg_mem_row_lock_table_mutex = PTHREAD_MUTEX_INITIALIZER;
static FastPgMemRowLockEntry *fastpg_mem_row_lock_buckets[FASTPG_MEM_ROW_LOCK_BUCKETS];
static pthread_mutex_t fastpg_mem_toast_state_lock = PTHREAD_MUTEX_INITIALIZER;
static FastPgMemToastState *fastpg_mem_toast_states = NULL;
#else
static bool fastpg_mem_xact_callbacks_registered = false;
static MemoryContext fastpg_mem_touched_context = NULL;
static HTAB *fastpg_mem_touched_hash = NULL;
static FastPgMemTouchedRowHashEntry fastpg_mem_touched_inline[FASTPG_MEM_INLINE_TOUCHED_ROWS];
static int	fastpg_mem_touched_inline_count = 0;
static MemoryContext fastpg_mem_redirect_context = NULL;
static FastPgMemRowRedirect *fastpg_mem_row_redirects = NULL;
static MemoryContext fastpg_mem_storage2_lock_root_context = NULL;
static FastPgMemStorage2LockRoot *fastpg_mem_storage2_lock_roots = NULL;
static MemoryContext fastpg_mem_visibility_context = NULL;
static FastPgMemVisibilityState *fastpg_mem_visibility_states = NULL;
static MemoryContext fastpg_mem_block_layout_context = NULL;
static FastPgMemBlockLayout *fastpg_mem_block_layouts = NULL;
static FastPgMemLastIndexKey fastpg_mem_last_index_key = {0};
static FastPgMemRowLockEntry **fastpg_mem_held_row_locks = NULL;
static int	fastpg_mem_held_row_lock_count = 0;
static int	fastpg_mem_held_row_lock_capacity = 0;
static pthread_mutex_t fastpg_mem_row_lock_table_mutex = PTHREAD_MUTEX_INITIALIZER;
static FastPgMemRowLockEntry *fastpg_mem_row_lock_buckets[FASTPG_MEM_ROW_LOCK_BUCKETS];
#endif

typedef struct FastPgMemIndexScan
{
	bool		done;
	bool		unsupported;
	bool		full_scan;
	bool		counted_scan;
	ScanKeyData *scan_keys;
	int			scan_nkeys;
	uint64_t	scan_handle;
	bool		scan_storage2;
	TupleTableSlot *scan_slot;
	FastPgMemIndexMatch *matched_rows;
	int			matched_count;
	int			matched_capacity;
	int			matched_index;
	bool		matched_ready;
	uintptr_t	values[FASTPG_MAX_INDEX_KEYS];
	uint8_t		isnull[FASTPG_MAX_INDEX_KEYS];
	uint8_t		key_seen[FASTPG_MAX_INDEX_KEYS];
	size_t		nkeys;
	Datum	   *array_values;
	bool	   *array_isnull;
	int			array_nelems;
	int			array_index;
	int			array_key_index;
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
static void fastpg_mem_ensure_index_attr_bitmaps(Relation rel);
static bool fastpg_mem_single_update_index_attr(Relation rel,
												TupleDesc tupdesc,
												AttrNumber *attnum_out);
static void fastpg_mem_remember_single_byval_index_key(uint32_t relid,
													   uint64_t row_id,
													   AttrNumber attnum,
													   uintptr_t value,
													   uint8_t isnull);
static bool fastpg_mem_cached_single_byval_index_lookup(uint32_t relid,
														AttrNumber attnum,
														uintptr_t value,
														uint8_t isnull,
														uint64_t *row_id_out);
static bool fastpg_mem_cached_single_index_key_preserves(Relation rel,
														 uint64_t row_id,
														 TupleTableSlot *new_slot,
														 AttrNumber attnum,
														 bool *preserves_out);
static void fastpg_mem_maybe_remember_scan_single_index_key(FastPgMemScanDesc *scan,
															uint64_t row_id,
															const uintptr_t *values,
															const uint8_t *isnull,
															size_t stored_natts);
static FastPgMemBlockLayout *fastpg_mem_block_layout_entry(uint32_t relid,
														   bool create);
static bool fastpg_mem_heap_pages_from_recorded_layout(Relation rel,
													   size_t row_count,
													   BlockNumber *pages);
static uint64_t fastpg_mem_relation_rows_per_block(Relation rel);
static bool fastpg_mem_slot_needs_heap_tuple(Relation rel,
											 TupleTableSlot *slot);
static bool fastpg_mem_relation_fits_without_toast(Relation rel);
static bool fastpg_mem_relation_may_have_external_toast(uint32_t relid);
static void fastpg_mem_note_relation_external_toast(uint32_t relid);
static void fastpg_mem_clear_relation_external_toast(uint32_t relid);
void		fastpg_mem_ensure_xact_callbacks(void);
static void fastpg_mem_acquire_storage2_update_row_lock(uint32_t relid,
														uint64_t *row_id);
void		fastpg_mem_release_row_locks(void);
bool		fastpg_mem_tableoid_tid_to_row_id(uint32_t relid,
											  ItemPointer tid,
											  uint64_t *row_id);
bool		fastpg_mem_tableoid_uses_storage2(uint32_t relid);
static void fastpg_mem_ensure_block_layout_for_slot(Relation rel,
													TupleTableSlot *slot);

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
	cached = (engine == NULL || strcmp(engine, "storage2") == 0) ? 1 : 0;
	return cached == 1;
}

static bool
fastpg_mem_storage1_xact_needed(void)
{
	return !fastpg_catalog_mode_uses_postgres() || !fastpg_mem_storage2_enabled();
}

static bool
fastpg_mem_use_storage2_for_relid(uint32_t relid)
{
	if (!fastpg_mem_storage2_enabled())
		return false;
	if (fastpg_catalog_mode_uses_postgres())
		return true;
	return fastpg_rust_catalog_policy_by_relation_oid(relid) == 0;
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
	fastpg_mem_touched_inline_count = 0;
	if (fastpg_mem_touched_context != NULL)
		MemoryContextReset(fastpg_mem_touched_context);
	fastpg_mem_touched_hash = NULL;
}

static void
fastpg_mem_reset_row_redirects(void)
{
	if (fastpg_mem_redirect_context != NULL)
		MemoryContextReset(fastpg_mem_redirect_context);
	fastpg_mem_row_redirects = NULL;
}

static void
fastpg_mem_reset_storage2_lock_roots(void)
{
	if (fastpg_mem_storage2_lock_root_context != NULL)
		MemoryContextReset(fastpg_mem_storage2_lock_root_context);
	fastpg_mem_storage2_lock_roots = NULL;
}

static void
fastpg_mem_record_storage2_lock_root(uint32_t relid, uint64_t root_row_id,
									 uint64_t resolved_row_id)
{
	MemoryContext oldcontext;
	FastPgMemStorage2LockRoot *entry;

	if (root_row_id == 0 ||
		resolved_row_id == 0 ||
		root_row_id == resolved_row_id)
		return;

	for (entry = fastpg_mem_storage2_lock_roots; entry != NULL; entry = entry->next)
	{
		if (entry->relid == relid && entry->resolved_row_id == resolved_row_id)
		{
			entry->root_row_id = root_row_id;
			return;
		}
	}

	if (fastpg_mem_storage2_lock_root_context == NULL)
		fastpg_mem_storage2_lock_root_context =
			AllocSetContextCreate(TopMemoryContext,
								  "fastpg storage2 lock roots",
								  ALLOCSET_SMALL_SIZES);

	oldcontext = MemoryContextSwitchTo(fastpg_mem_storage2_lock_root_context);
	entry = palloc0_object(FastPgMemStorage2LockRoot);
	entry->relid = relid;
	entry->root_row_id = root_row_id;
	entry->resolved_row_id = resolved_row_id;
	entry->next = fastpg_mem_storage2_lock_roots;
	fastpg_mem_storage2_lock_roots = entry;
	MemoryContextSwitchTo(oldcontext);
}

static uint64_t
fastpg_mem_storage2_lock_root(uint32_t relid, uint64_t row_id)
{
	for (int depth = 0; depth < 32; depth++)
	{
		FastPgMemStorage2LockRoot *entry;
		bool		found = false;

		for (entry = fastpg_mem_storage2_lock_roots; entry != NULL; entry = entry->next)
		{
			if (entry->relid == relid && entry->resolved_row_id == row_id)
			{
				row_id = entry->root_row_id;
				found = true;
				break;
			}
		}
		if (!found)
			break;
	}

	return row_id;
}

static void
fastpg_mem_record_row_redirect(uint32_t relid, uint64_t old_row_id,
							   uint64_t new_row_id)
{
	MemoryContext oldcontext;
	FastPgMemRowRedirect *entry;

	if (old_row_id == 0 || new_row_id == 0 || old_row_id == new_row_id)
		return;

	if (fastpg_mem_redirect_context == NULL)
		fastpg_mem_redirect_context =
			AllocSetContextCreate(TopMemoryContext,
								  "fastpg row redirects",
								  ALLOCSET_SMALL_SIZES);

	oldcontext = MemoryContextSwitchTo(fastpg_mem_redirect_context);
	entry = palloc0_object(FastPgMemRowRedirect);
	entry->relid = relid;
	entry->old_row_id = old_row_id;
	entry->new_row_id = new_row_id;
	entry->xid = GetCurrentTransactionIdIfAny();
	entry->next = fastpg_mem_row_redirects;
	fastpg_mem_row_redirects = entry;
	MemoryContextSwitchTo(oldcontext);
}

static uint64_t
fastpg_mem_resolve_row_redirect(uint32_t relid, uint64_t row_id)
{
	TransactionId xid = GetCurrentTransactionIdIfAny();

	for (int depth = 0; depth < 32; depth++)
	{
		FastPgMemRowRedirect *entry;
		bool		found = false;

		for (entry = fastpg_mem_row_redirects; entry != NULL; entry = entry->next)
		{
			if (entry->relid == relid &&
				entry->old_row_id == row_id &&
				entry->xid == xid)
			{
				row_id = entry->new_row_id;
				found = true;
				break;
			}
		}
		if (!found)
			break;
	}

	return row_id;
}

static uint64_t
fastpg_mem_reverse_row_redirect(uint32_t relid, uint64_t row_id)
{
	TransactionId xid = GetCurrentTransactionIdIfAny();

	for (int depth = 0; depth < 32; depth++)
	{
		FastPgMemRowRedirect *entry;
		bool		found = false;

		for (entry = fastpg_mem_row_redirects; entry != NULL; entry = entry->next)
		{
			if (entry->relid == relid &&
				entry->new_row_id == row_id &&
				entry->xid == xid)
			{
				row_id = entry->old_row_id;
				found = true;
				break;
			}
		}
		if (!found)
			break;
	}

	return row_id;
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

static void
fastpg_mem_note_relation_changed(uint32_t relid)
{
	FastPgMemVisibilityState *entry;

	fastpg_mem_set_relation_all_visible(relid, false);
	entry = fastpg_mem_relation_visibility_state(relid, false);
	if (entry != NULL)
		entry->known_empty = false;
}

static bool
fastpg_mem_relation_touched_by_current_xact_since(uint32_t relid, CommandId cid)
{
	FastPgMemVisibilityState *visibility;
	TransactionId xid = GetCurrentTransactionIdIfAny();

	if (!TransactionIdIsValid(xid))
		return false;
	visibility = fastpg_mem_relation_visibility_state(relid, false);
	return visibility != NULL &&
		visibility->touched_xid == xid &&
		visibility->max_touched_cid >= cid;
}

static bool
fastpg_mem_relation_touched_by_current_xact(uint32_t relid)
{
	FastPgMemVisibilityState *visibility;
	TransactionId xid = GetCurrentTransactionIdIfAny();

	if (!TransactionIdIsValid(xid))
		return false;
	visibility = fastpg_mem_relation_visibility_state(relid, false);
	return visibility != NULL && visibility->touched_xid == xid;
}

static bool
fastpg_mem_row_touched(uint32_t relid, uint64_t row_id, CommandId cid,
					   CommandId *touched_cid)
{
	FastPgMemTouchedRowHashEntry *entry;
	FastPgMemTouchedRowKey key;
	FastPgMemVisibilityState *visibility;
	TransactionId xid = GetCurrentTransactionIdIfAny();
	int			index;

	visibility = fastpg_mem_relation_visibility_state(relid, false);
	if (visibility == NULL ||
		visibility->touched_xid != xid ||
		visibility->max_touched_cid < cid)
		return false;

	for (index = 0; index < fastpg_mem_touched_inline_count; index++)
	{
		entry = &fastpg_mem_touched_inline[index];
		if (entry->key.row_id == row_id &&
			entry->key.relid == relid &&
			entry->key.xid == xid)
		{
			if (entry->cid < cid)
				return false;
			if (touched_cid != NULL)
				*touched_cid = entry->cid;
			return true;
		}
	}

	if (fastpg_mem_touched_hash == NULL)
		return false;

	memset(&key, 0, sizeof(key));
	key.row_id = row_id;
	key.relid = relid;
	key.xid = xid;

	entry = (FastPgMemTouchedRowHashEntry *) hash_search(fastpg_mem_touched_hash,
														 &key,
														 HASH_FIND,
														 NULL);
	if (entry == NULL || entry->cid < cid)
		return false;
	if (touched_cid != NULL)
		*touched_cid = entry->cid;
	return true;
}

static HTAB *
fastpg_mem_touched_hash_ensure(void)
{
	HASHCTL		ctl;
	MemoryContext oldcontext;

	if (fastpg_mem_touched_hash != NULL)
		return fastpg_mem_touched_hash;

	if (fastpg_mem_touched_context == NULL)
		fastpg_mem_touched_context =
			AllocSetContextCreate(TopMemoryContext,
								  "fastpg touched rows",
								  ALLOCSET_SMALL_SIZES);

	memset(&ctl, 0, sizeof(ctl));
	ctl.keysize = sizeof(FastPgMemTouchedRowKey);
	ctl.entrysize = sizeof(FastPgMemTouchedRowHashEntry);
	ctl.hcxt = fastpg_mem_touched_context;

	oldcontext = MemoryContextSwitchTo(fastpg_mem_touched_context);
	fastpg_mem_touched_hash =
		hash_create("fastpg touched rows hash",
					1024,
					&ctl,
					HASH_ELEM | HASH_BLOBS | HASH_CONTEXT);
	MemoryContextSwitchTo(oldcontext);

	return fastpg_mem_touched_hash;
}

static void
fastpg_mem_touched_hash_insert(uint32_t relid, uint64_t row_id, CommandId cid,
							   TransactionId xid)
{
	FastPgMemTouchedRowHashEntry *entry;
	FastPgMemTouchedRowKey key;
	bool		found;
	int			index;

	for (index = 0; index < fastpg_mem_touched_inline_count; index++)
	{
		entry = &fastpg_mem_touched_inline[index];
		if (entry->key.row_id == row_id &&
			entry->key.relid == relid &&
			entry->key.xid == xid)
		{
			if (cid > entry->cid)
				entry->cid = cid;
			return;
		}
	}

	if (fastpg_mem_touched_inline_count < FASTPG_MEM_INLINE_TOUCHED_ROWS)
	{
		entry = &fastpg_mem_touched_inline[fastpg_mem_touched_inline_count++];
		memset(entry, 0, sizeof(*entry));
		entry->key.row_id = row_id;
		entry->key.relid = relid;
		entry->key.xid = xid;
		entry->cid = cid;
		return;
	}

	memset(&key, 0, sizeof(key));
	key.row_id = row_id;
	key.relid = relid;
	key.xid = xid;

	entry = (FastPgMemTouchedRowHashEntry *)
		hash_search(fastpg_mem_touched_hash_ensure(),
					&key,
					HASH_ENTER,
					&found);
	if (!found || cid > entry->cid)
		entry->cid = cid;
}

void
FastPgMemResetCommandTouchedRows(void)
{
	fastpg_mem_reset_touched_rows();
	fastpg_mem_reset_storage2_lock_roots();
}

static void
fastpg_mem_mark_row_touched(uint32_t relid, uint64_t row_id, CommandId cid)
{
	FastPgMemVisibilityState *visibility;
	TransactionId xid;

	if (row_id == 0)
		return;
	xid = GetCurrentTransactionIdIfAny();
	fastpg_mem_touched_hash_insert(relid, row_id, cid, xid);

	visibility = fastpg_mem_relation_visibility_state(relid, true);
	if (visibility->touched_xid != xid)
	{
		visibility->touched_xid = xid;
		visibility->max_touched_cid = cid;
	}
	else if (cid > visibility->max_touched_cid)
		visibility->max_touched_cid = cid;
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

static bool
fastpg_mem_row_deleted_by_current_xact(uint32_t relid, uint64_t row_id,
									   CommandId cid,
									   bool storage2,
									   CommandId *delete_cid_out)
{
	TransactionId delete_xid;
	CommandId	delete_cid;

	if (!fastpg_catalog_mode_uses_postgres())
		return false;
	if (!fastpg_mem_relation_touched_by_current_xact_since(relid, cid))
		return false;

	delete_xid = (TransactionId) (storage2 ?
								  fastpg_storage2_relation_row_delete_xid(relid, row_id) :
								  fastpg_rust_relation_row_delete_xid(relid, row_id));
	if (!TransactionIdIsValid(delete_xid) ||
		!TransactionIdIsCurrentTransactionId(delete_xid))
		return false;

	delete_cid = (CommandId) (storage2 ?
							  fastpg_storage2_relation_row_delete_cid(relid, row_id) :
							  fastpg_rust_relation_row_delete_cid(relid, row_id));
	if (delete_cid < cid)
		return false;

	if (delete_cid_out != NULL)
		*delete_cid_out = delete_cid;
	return true;
}

static bool
fastpg_mem_storage2_row_deleted_by_current_xact_any_cid(uint32_t relid,
														uint64_t row_id,
														CommandId *delete_cid_out)
{
	TransactionId delete_xid;
	CommandId	delete_cid;

	if (!fastpg_catalog_mode_uses_postgres())
		return false;
	if (!fastpg_mem_relation_touched_by_current_xact(relid))
		return false;

	delete_xid = (TransactionId) fastpg_storage2_relation_row_delete_xid(relid,
																		 row_id);
	if (!TransactionIdIsValid(delete_xid) ||
		!TransactionIdIsCurrentTransactionId(delete_xid))
		return false;

	delete_cid = (CommandId) fastpg_storage2_relation_row_delete_cid(relid,
																	 row_id);
	if (delete_cid_out != NULL)
		*delete_cid_out = delete_cid;
	return true;
}

static void
fastpg_mem_xact_callback(XactEvent event, void *arg)
{
	switch (event)
	{
		case XACT_EVENT_COMMIT:
		case XACT_EVENT_PARALLEL_COMMIT:
		case XACT_EVENT_PREPARE:
			if (fastpg_mem_storage1_xact_needed())
				fastpg_rust_xact_commit();
			if (fastpg_mem_storage2_enabled())
			{
				if (fastpg_catalog_mode_uses_postgres())
					fastpg_storage2_xact_commit_if_implicit();
				else
					fastpg_storage2_xact_commit();
			}
			fastpg_mem_release_row_locks();
			fastpg_mem_reset_touched_rows();
			fastpg_mem_reset_row_redirects();
			fastpg_mem_reset_storage2_lock_roots();
			fastpg_mem_last_index_key.valid = false;
			break;
		case XACT_EVENT_ABORT:
		case XACT_EVENT_PARALLEL_ABORT:
			if (fastpg_mem_storage1_xact_needed())
				fastpg_rust_xact_abort();
			if (fastpg_mem_storage2_enabled())
			{
				if (fastpg_catalog_mode_uses_postgres())
					fastpg_storage2_xact_abort_if_implicit();
				else
					fastpg_storage2_xact_abort();
			}
			fastpg_mem_release_row_locks();
			fastpg_mem_reset_touched_rows();
			fastpg_mem_reset_row_redirects();
			fastpg_mem_reset_storage2_lock_roots();
			fastpg_mem_last_index_key.valid = false;
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
			if (fastpg_mem_storage1_xact_needed())
				fastpg_rust_subxact_begin();
			if (fastpg_mem_storage2_enabled())
				fastpg_storage2_subxact_begin();
			break;
		case SUBXACT_EVENT_COMMIT_SUB:
			if (fastpg_mem_storage1_xact_needed())
				fastpg_rust_subxact_commit();
			if (fastpg_mem_storage2_enabled())
				fastpg_storage2_subxact_commit();
			fastpg_mem_last_index_key.valid = false;
			break;
		case SUBXACT_EVENT_ABORT_SUB:
			if (fastpg_mem_storage1_xact_needed())
				fastpg_rust_subxact_abort();
			if (fastpg_mem_storage2_enabled())
				fastpg_storage2_subxact_abort();
			fastpg_mem_last_index_key.valid = false;
			break;
		default:
			break;
	}
}

void
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
	if (fastpg_mem_storage1_xact_needed())
		fastpg_rust_xact_begin_implicit();
	if (fastpg_mem_storage2_enabled())
		fastpg_storage2_xact_begin_implicit();
}

static bool
fastpg_mem_row_id_to_tid(Relation rel, uint64_t row_id, ItemPointer tid)
{
	uint64_t	zero_index;
	uint64_t	block;
	uint64_t	rows_per_block;
	OffsetNumber offset;

	if (row_id == 0)
		return false;

	rows_per_block = fastpg_mem_relation_rows_per_block(rel);
	zero_index = row_id - 1;
	block = zero_index / rows_per_block;
	if (block > UINT32_MAX)
		return false;

	offset = (OffsetNumber) (zero_index % rows_per_block) +
		FirstOffsetNumber;
	ItemPointerSet(tid, (BlockNumber) block, offset);
	return true;
}

static bool
fastpg_mem_tid_to_row_id(Relation rel, ItemPointer tid, uint64_t *row_id)
{
	BlockNumber block = ItemPointerGetBlockNumber(tid);
	OffsetNumber offset = ItemPointerGetOffsetNumber(tid);
	uint64_t	rows_per_block;

	if (!OffsetNumberIsValid(offset))
		return false;

	rows_per_block = fastpg_mem_relation_rows_per_block(rel);
	if (offset > (OffsetNumber) rows_per_block)
		return false;

	*row_id = ((uint64_t) block * rows_per_block) +
		(uint64_t) offset;
	return true;
}

bool
fastpg_mem_tableoid_tid_to_row_id(uint32_t relid, ItemPointer tid,
								  uint64_t *row_id)
{
	BlockNumber block = ItemPointerGetBlockNumber(tid);
	OffsetNumber offset = ItemPointerGetOffsetNumber(tid);
	FastPgMemBlockLayout *entry;
	uint64_t	rows_per_block = FASTPG_MEM_MAX_ROWS_PER_BLOCK;

	if (!OffsetNumberIsValid(offset))
		return false;
	if (fastpg_mem_use_storage2_for_relid(relid))
	{
		*row_id = (((uint64_t) block) << 16) | (uint64_t) offset;
		return *row_id != 0;
	}

	entry = fastpg_mem_block_layout_entry(relid, false);
	if (entry != NULL && entry->rows_per_block > 0)
		rows_per_block = entry->rows_per_block;

	if (offset > (OffsetNumber) rows_per_block)
		return false;

	*row_id = ((uint64_t) block * rows_per_block) +
		(uint64_t) offset;
	return true;
}

bool
fastpg_mem_tableoid_uses_storage2(uint32_t relid)
{
	return fastpg_mem_use_storage2_for_relid(relid);
}

static bool
fastpg_mem_scan_needs_row_metadata(Relation rel, Snapshot snapshot)
{
	FastPgMemVisibilityState *visibility;
	TransactionId xid;

	if (!fastpg_catalog_mode_uses_postgres() || snapshot == NULL)
		return false;
	if (snapshot->snapshot_type != SNAPSHOT_MVCC)
		return false;
	visibility =
		fastpg_mem_relation_visibility_state((uint32_t) RelationGetRelid(rel),
											 false);
	if (visibility == NULL)
		return false;
	xid = GetCurrentTransactionIdIfAny();
	if (!TransactionIdIsValid(xid))
		return false;
	if (visibility->touched_xid != xid ||
		visibility->max_touched_cid < snapshot->curcid)
		return false;
	return true;
}

static bool
fastpg_mem_row_metadata_visible_to_snapshot(TransactionId xmin, CommandId cmin,
											Snapshot snapshot)
{
	if (!TransactionIdIsValid(xmin))
		return true;
	if (!TransactionIdIsCurrentTransactionId(xmin))
		return true;
	return cmin < snapshot->curcid;
}

static CommandId
fastpg_mem_effective_snapshot_curcid(Snapshot snapshot)
{
	if (snapshot == NULL || snapshot->snapshot_type != SNAPSHOT_MVCC)
		return InvalidCommandId;

	return snapshot->curcid;
}

static CommandId
fastpg_mem_delete_cid_for_snapshot(CommandId cid, Snapshot snapshot)
{
	if (fastpg_catalog_mode_uses_postgres() &&
		snapshot != NULL &&
		snapshot->snapshot_type == SNAPSHOT_MVCC)
		return fastpg_mem_effective_snapshot_curcid(snapshot);
	return cid;
}

static bool
fastpg_mem_row_visible_to_snapshot(Relation rel, uint64_t row_id, Snapshot snapshot)
{
	TransactionId xmin;
	CommandId	cmin;

	xmin =
		(TransactionId) fastpg_rust_relation_row_xmin((uint32_t) RelationGetRelid(rel),
													  row_id);
	cmin =
		(CommandId) fastpg_rust_relation_row_cmin((uint32_t) RelationGetRelid(rel),
												  row_id);
	if (!fastpg_mem_scan_needs_row_metadata(rel, snapshot))
		return true;
	return fastpg_mem_row_metadata_visible_to_snapshot(xmin, cmin, snapshot);
}

static void
fastpg_mem_count_io_op(Relation rel, IOContext io_context, IOOp io_op,
					   uint32 count)
{
	uint64		bytes = 0;

	if (!fastpg_catalog_mode_uses_postgres() ||
		rel == NULL ||
		rel->rd_rel->relpersistence == RELPERSISTENCE_TEMP)
		return;

	if (io_op == IOOP_EXTEND || io_op == IOOP_READ || io_op == IOOP_WRITE)
		bytes = (uint64) count * BLCKSZ;
	pgstat_count_io_op(IOOBJECT_RELATION, io_context, io_op, count, bytes);

	if (io_context == IOCONTEXT_NORMAL && io_op == IOOP_WRITE)
	{
		pgstat_count_io_op(IOOBJECT_WAL, IOCONTEXT_NORMAL, IOOP_WRITE,
						   count, bytes);
		pgWalUsage.wal_records += count;
		pgWalUsage.wal_bytes += bytes;
	}
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

static uint64_t
fastpg_mem_storage2_resolve_row_id_read(uint32_t relid, uint64_t row_id)
{
	uint64_t	resolved_row_id = row_id;

	if (row_id == 0)
		return 0;
	if (fastpg_storage2_relation_resolve_tid_read(relid,
												 row_id,
												 &resolved_row_id))
		return resolved_row_id;
	return row_id;
}

static uint64_t
fastpg_mem_storage2_resolve_update_row_id(uint32_t relid, uint64_t row_id)
{
	uint64_t	resolved_row_id = row_id;

	if (row_id == 0)
		return 0;
	if (fastpg_storage2_relation_resolve_update_tid(relid, row_id, &resolved_row_id))
		return resolved_row_id;
	return row_id;
}

static uint64_t
fastpg_mem_storage2_resolve_update_row_id_read(uint32_t relid, uint64_t row_id)
{
	uint64_t	resolved_row_id = row_id;

	if (row_id == 0)
		return 0;
	if (fastpg_storage2_relation_resolve_update_tid_read(relid,
														 row_id,
														 &resolved_row_id))
		return resolved_row_id;
	return row_id;
}

static uint32_t
fastpg_mem_row_lock_hash(uint32_t relid, uint64_t row_id)
{
	uint64_t	hash = row_id ^ (((uint64_t) relid) << 32);

	hash ^= hash >> 33;
	hash *= UINT64CONST(0xff51afd7ed558ccd);
	hash ^= hash >> 33;
	return (uint32_t) (hash % FASTPG_MEM_ROW_LOCK_BUCKETS);
}

static FastPgMemRowLockEntry *
fastpg_mem_row_lock_entry(uint32_t relid, uint64_t row_id)
{
	FastPgMemRowLockEntry *entry;
	uint32_t	bucket = fastpg_mem_row_lock_hash(relid, row_id);

	pthread_mutex_lock(&fastpg_mem_row_lock_table_mutex);
	for (entry = fastpg_mem_row_lock_buckets[bucket]; entry != NULL;
		 entry = entry->next)
	{
		if (entry->relid == relid && entry->row_id == row_id)
		{
			pthread_mutex_unlock(&fastpg_mem_row_lock_table_mutex);
			return entry;
		}
	}

	entry = malloc(sizeof(*entry));
	if (entry == NULL)
	{
		pthread_mutex_unlock(&fastpg_mem_row_lock_table_mutex);
		ereport(ERROR,
				(errcode(ERRCODE_OUT_OF_MEMORY),
				 errmsg("out of memory allocating fastpg row lock")));
	}
	memset(entry, 0, sizeof(*entry));
	entry->relid = relid;
	entry->row_id = row_id;
	if (pthread_mutex_init(&entry->mutex, NULL) != 0)
	{
		free(entry);
		pthread_mutex_unlock(&fastpg_mem_row_lock_table_mutex);
		elog(ERROR, "failed to initialize fastpg row lock");
	}
	entry->next = fastpg_mem_row_lock_buckets[bucket];
	fastpg_mem_row_lock_buckets[bucket] = entry;
	pthread_mutex_unlock(&fastpg_mem_row_lock_table_mutex);
	return entry;
}

static bool
fastpg_mem_row_lock_held(FastPgMemRowLockEntry *entry)
{
	for (int index = 0; index < fastpg_mem_held_row_lock_count; index++)
	{
		if (fastpg_mem_held_row_locks[index] == entry)
			return true;
	}
	return false;
}

static void
fastpg_mem_remember_held_row_lock(FastPgMemRowLockEntry *entry)
{
	if (fastpg_mem_held_row_lock_count >= fastpg_mem_held_row_lock_capacity)
	{
		int			new_capacity =
			fastpg_mem_held_row_lock_capacity == 0 ? 8 :
			fastpg_mem_held_row_lock_capacity * 2;
		FastPgMemRowLockEntry **new_locks =
			realloc(fastpg_mem_held_row_locks,
					sizeof(*new_locks) * new_capacity);

		if (new_locks == NULL)
		{
			pthread_mutex_unlock(&entry->mutex);
			ereport(ERROR,
					(errcode(ERRCODE_OUT_OF_MEMORY),
					 errmsg("out of memory tracking fastpg row lock")));
		}
		fastpg_mem_held_row_locks = new_locks;
		fastpg_mem_held_row_lock_capacity = new_capacity;
	}
	fastpg_mem_held_row_locks[fastpg_mem_held_row_lock_count++] = entry;
}

static bool
fastpg_mem_acquire_row_lock(uint32_t relid, uint64_t row_id,
							FastPgMemRowLockEntry **entry_out)
{
	FastPgMemRowLockEntry *entry;

	if (row_id == 0)
	{
		if (entry_out != NULL)
			*entry_out = NULL;
		return false;
	}
	entry = fastpg_mem_row_lock_entry(relid, row_id);
	if (entry_out != NULL)
		*entry_out = entry;
	if (fastpg_mem_row_lock_held(entry))
		return false;
	pthread_mutex_lock(&entry->mutex);
	fastpg_mem_remember_held_row_lock(entry);
	return true;
}

static void
fastpg_mem_acquire_storage2_update_row_lock(uint32_t relid, uint64_t *row_id)
{
	uint64_t	lock_row_id;
	uint64_t	resolved_row_id;

	if (row_id == NULL || *row_id == 0)
		return;
	if (fastpg_storage2_relation_current_session_owns_inserted_tid(relid,
																   *row_id))
		return;

	fastpg_mem_ensure_xact_callbacks();
	lock_row_id = fastpg_mem_storage2_lock_root(relid, *row_id);
	(void) fastpg_mem_acquire_row_lock(relid, lock_row_id, NULL);
	resolved_row_id = fastpg_mem_storage2_resolve_update_row_id(relid, *row_id);
	if (resolved_row_id != 0)
		*row_id = resolved_row_id;
}

void
fastpg_mem_release_row_locks(void)
{
	for (int index = fastpg_mem_held_row_lock_count - 1; index >= 0; index--)
		pthread_mutex_unlock(&fastpg_mem_held_row_locks[index]->mutex);
	fastpg_mem_held_row_lock_count = 0;
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

static bool
fastpg_mem_datum_attr_equal(Datum value1, Datum value2, Form_pg_attribute attr)
{
	if (attr->attbyval)
		return value1 == value2;
	if (attr->attlen == -1)
	{
		struct varlena *varlena1 =
			(struct varlena *) PG_DETOAST_DATUM_PACKED(value1);
		struct varlena *varlena2 =
			(struct varlena *) PG_DETOAST_DATUM_PACKED(value2);
		Size		size1 = VARSIZE_ANY_EXHDR(varlena1);
		Size		size2 = VARSIZE_ANY_EXHDR(varlena2);
		bool		equal = size1 == size2 &&
			memcmp(VARDATA_ANY(varlena1), VARDATA_ANY(varlena2), size1) == 0;

		if ((Pointer) varlena1 != DatumGetPointer(value1))
			pfree(varlena1);
		if ((Pointer) varlena2 != DatumGetPointer(value2))
			pfree(varlena2);
		return equal;
	}
	if (attr->attlen == -2)
		return strcmp(DatumGetCString(value1), DatumGetCString(value2)) == 0;
	return datumIsEqual(value1, value2, attr->attbyval, attr->attlen);
}

static bool
fastpg_mem_relation_fast_data_width(Relation rel, int32 *attr_widths,
									int32 *width)
{
	int64		tuple_width = 0;

	for (int index = 1; index <= RelationGetNumberOfAttributes(rel); index++)
	{
		Form_pg_attribute attr = TupleDescAttr(rel->rd_att, index - 1);
		int32		item_width = 0;

		if (attr->attisdropped)
			continue;

		if (attr_widths != NULL && attr_widths[index] > 0)
		{
			tuple_width += attr_widths[index];
			continue;
		}

		if (attr->attlen > 0)
			item_width = attr->attlen;
		else if (attr->atttypid == BPCHAROID)
			item_width = type_maximum_size(attr->atttypid, attr->atttypmod);
		else
			return false;

		if (item_width <= 0)
			return false;
		if (attr_widths != NULL)
			attr_widths[index] = item_width;
		tuple_width += item_width;
	}

	*width = clamp_width_est(tuple_width);
	return true;
}

static double
fastpg_mem_heap_tuple_density(Relation rel, int32 *attr_widths)
{
	int32		tuple_width;
	int			fillfactor;
	double		density;

	fillfactor = RelationGetFillFactor(rel, HEAP_DEFAULT_FILLFACTOR);
	if (!fastpg_mem_relation_fast_data_width(rel, attr_widths, &tuple_width))
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

static FastPgMemBlockLayout *
fastpg_mem_block_layout_entry(uint32_t relid, bool create)
{
	FastPgMemBlockLayout *entry;
	MemoryContext oldcontext;

	for (entry = fastpg_mem_block_layouts; entry != NULL; entry = entry->next)
	{
		if (entry->relid == relid)
			return entry;
	}

	if (!create)
		return NULL;

	if (fastpg_mem_block_layout_context == NULL)
		fastpg_mem_block_layout_context =
			AllocSetContextCreate(TopMemoryContext,
								  "fastpg block layouts",
								  ALLOCSET_SMALL_SIZES);

	oldcontext = MemoryContextSwitchTo(fastpg_mem_block_layout_context);
	entry = palloc0_object(FastPgMemBlockLayout);
	entry->relid = relid;
	entry->next = fastpg_mem_block_layouts;
	fastpg_mem_block_layouts = entry;
	MemoryContextSwitchTo(oldcontext);

	return entry;
}

static uint64_t
fastpg_mem_clamp_rows_per_block(double rows_per_block)
{
	if (rows_per_block < 1.0)
		return 1;
	if (rows_per_block > (double) FASTPG_MEM_MAX_ROWS_PER_BLOCK)
		return FASTPG_MEM_MAX_ROWS_PER_BLOCK;
	return (uint64_t) floor(rows_per_block);
}

static uint64_t
fastpg_mem_relation_rows_per_block(Relation rel)
{
	FastPgMemBlockLayout *entry;
	double		density;

	if (rel == NULL)
		return FASTPG_MEM_MAX_ROWS_PER_BLOCK;

	entry =
		fastpg_mem_block_layout_entry((uint32_t) RelationGetRelid(rel), false);
	if (entry != NULL && entry->rows_per_block > 0)
		return entry->rows_per_block;

	density = fastpg_mem_heap_tuple_density(rel, NULL);
	return fastpg_mem_clamp_rows_per_block(density);
}

static bool
fastpg_mem_heap_pages_from_recorded_layout(Relation rel, size_t row_count,
										   BlockNumber *pages)
{
	FastPgMemBlockLayout *entry;

	if (rel == NULL)
		return false;

	entry =
		fastpg_mem_block_layout_entry((uint32_t) RelationGetRelid(rel), false);
	if (entry == NULL || entry->rows_per_block == 0)
		return false;

	*pages = row_count == 0 ? 0 :
		(BlockNumber) ((row_count + entry->rows_per_block - 1) /
					   entry->rows_per_block);
	return true;
}

static uint64_t
fastpg_mem_rows_per_block_for_slot(Relation rel, TupleTableSlot *slot)
{
	TupleDesc	tupdesc = RelationGetDescr(rel);
	Size		header_len;
	Size		tuple_len;
	Size		tuple_bytes;
	Size		usable_bytes;
	int			fillfactor;
	bool		hasnull = false;

	slot_getallattrs(slot);
	for (int index = 0; index < tupdesc->natts; index++)
	{
		if (slot->tts_isnull[index])
		{
			hasnull = true;
			break;
		}
	}

	header_len = offsetof(HeapTupleHeaderData, t_bits);
	if (hasnull)
		header_len += BITMAPLEN(tupdesc->natts);
	tuple_len = MAXALIGN(header_len) +
		heap_compute_data_size(tupdesc, slot->tts_values, slot->tts_isnull);
	tuple_bytes = MAXALIGN(tuple_len) + sizeof(ItemIdData);
	if (tuple_bytes == 0)
		tuple_bytes = 1;

	fillfactor = RelationGetFillFactor(rel, HEAP_DEFAULT_FILLFACTOR);
	usable_bytes = FASTPG_MEM_HEAP_USABLE_BYTES_PER_PAGE * fillfactor / 100;
	if (usable_bytes == 0)
		usable_bytes = 1;

	return fastpg_mem_clamp_rows_per_block((double) usable_bytes /
										   (double) tuple_bytes);
}

static void
fastpg_mem_ensure_block_layout_for_slot(Relation rel, TupleTableSlot *slot)
{
	FastPgMemBlockLayout *entry;

	if (!fastpg_catalog_mode_uses_postgres())
		return;

	entry =
		fastpg_mem_block_layout_entry((uint32_t) RelationGetRelid(rel), true);
	if (entry->rows_per_block == 0)
		entry->rows_per_block = fastpg_mem_rows_per_block_for_slot(rel, slot);
	if (fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(rel)) &&
		entry->rows_per_block > 0 &&
		!entry->storage2_rows_per_block_applied)
	{
		fastpg_storage2_relation_set_max_tuples_per_block((uint32_t) RelationGetRelid(rel),
														  (uint16_t) Min(entry->rows_per_block,
																		 UINT16_MAX));
		entry->storage2_rows_per_block_applied = true;
	}
}

static BlockNumber
fastpg_mem_heap_pages_for_layout(Relation rel, size_t row_count)
{
	uint64_t	rows_per_block;

	if (row_count == 0)
		return 0;

	rows_per_block = fastpg_mem_relation_rows_per_block(rel);
	return (BlockNumber) ((row_count + rows_per_block - 1) / rows_per_block);
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

	return fastpg_mem_heap_pages_for_layout(rel, row_count);
}

BlockNumber
FastPgMemRelationPhysicalPages(Relation rel)
{
	uint32_t	relid = (uint32_t) RelationGetRelid(rel);

	if (fastpg_mem_use_storage2_for_relid(relid))
		return (BlockNumber) fastpg_storage2_relation_block_count(relid);

	return FastPgMemRelationPages(rel);
}

BlockNumber
FastPgMemRelationAllVisiblePages(Relation rel)
{
	uint32_t	relid = (uint32_t) RelationGetRelid(rel);
	FastPgMemVisibilityState *entry;
	BlockNumber pages;

	if (!fastpg_catalog_mode_uses_postgres())
		return 0;

	entry = fastpg_mem_relation_visibility_state(relid, false);
	if (entry != NULL)
		return entry->all_visible ? FastPgMemRelationPages(rel) : 0;

	if (rel->rd_rel->relallvisible <= 0)
		return 0;

	pages = FastPgMemRelationPages(rel);
	return (BlockNumber) Min((BlockNumber) rel->rd_rel->relallvisible, pages);
}

static void
fastpg_mem_estimate_heap_size(Relation rel, int32 *attr_widths,
							  size_t row_count,
							  BlockNumber *pages,
							  double *tuples,
							  double *allvisfrac)
{
	BlockNumber curpages;
	BlockNumber relpages = (BlockNumber) rel->rd_rel->relpages;
	double		reltuples = (double) rel->rd_rel->reltuples;
	BlockNumber relallvisible = (BlockNumber) rel->rd_rel->relallvisible;
	double		density;

	if (!(fastpg_catalog_mode_uses_postgres() &&
		  fastpg_mem_heap_pages_from_recorded_layout(rel, row_count, &curpages)))
		curpages =
			fastpg_mem_heap_pages_for_row_count(rel, attr_widths, row_count, true);
	else if (curpages < 10 &&
			 reltuples < 0 &&
			 !rel->rd_rel->relhassubclass)
		curpages = 10;
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
fastpg_mem_estimate_exact_storage2_size(Relation rel,
										size_t row_count,
										BlockNumber *pages,
										double *tuples,
										double *allvisfrac)
{
	BlockNumber layout_pages = fastpg_mem_heap_pages_for_layout(rel, row_count);

	*pages = layout_pages;
	*tuples = (double) row_count;
	*allvisfrac = 0.0;
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
fastpg_mem_free_owned_slot_value_payloads(Relation rel,
										  const uintptr_t *values,
										  const uint8_t *isnull,
										  const uint8_t *owned)
{
	TupleDesc	tupdesc = RelationGetDescr(rel);

	for (int index = 0; index < tupdesc->natts; index++)
	{
		if (isnull[index] == 0 && owned[index] != 0 && values[index] != 0)
			pfree((void *) values[index]);
	}
}

static void
fastpg_mem_fill_slot_values_internal(Relation rel,
									 TupleTableSlot *slot,
									 uintptr_t *values,
									 uint8_t *isnull,
									 uint8_t *byval,
									 size_t *value_lens,
									 uint8_t *owned)
{
	TupleDesc	tupdesc;

	slot_getallattrs(slot);
	tupdesc = RelationGetDescr(rel);
	memset(values, 0, sizeof(uintptr_t) * tupdesc->natts);
	memset(isnull, 0, sizeof(uint8_t) * tupdesc->natts);
	memset(byval, 0, sizeof(uint8_t) * tupdesc->natts);
	memset(value_lens, 0, sizeof(size_t) * tupdesc->natts);
	if (owned != NULL)
		memset(owned, 0, sizeof(uint8_t) * tupdesc->natts);

	for (int index = 0; index < tupdesc->natts; index++)
	{
		Form_pg_attribute attr = TupleDescAttr(tupdesc, index);

		isnull[index] = slot->tts_isnull[index] ? 1 : 0;
		byval[index] = attr->attbyval ? 1 : 0;
		if (isnull[index] == 0)
		{
			if (attr->attlen == -1)
			{
				Pointer		raw = DatumGetPointer(slot->tts_values[index]);
				struct varlena *flat;

				if (raw != NULL && !VARATT_IS_EXTERNAL(raw))
				{
					Size		len = VARSIZE_ANY(raw);

					if (owned == NULL)
					{
						flat = (struct varlena *) palloc(len);
						memcpy(flat, raw, len);
					}
					else
						flat = (struct varlena *) raw;
				}
				else if (owned == NULL)
					flat =
						(struct varlena *) PG_DETOAST_DATUM_COPY(slot->tts_values[index]);
				else
				{
					flat =
						(struct varlena *) PG_DETOAST_DATUM_PACKED(slot->tts_values[index]);
					if ((Pointer) flat != raw)
						owned[index] = 1;
				}

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
}

static void
fastpg_mem_fill_slot_values(Relation rel,
							TupleTableSlot *slot,
							uintptr_t *values,
							uint8_t *isnull,
							uint8_t *byval,
							size_t *value_lens)
{
	fastpg_mem_fill_slot_values_internal(rel,
										 slot,
										 values,
										 isnull,
										 byval,
										 value_lens,
										 NULL);
}

static void
fastpg_mem_prepare_heap_tuple_header(Relation rel,
									 HeapTuple tuple,
									 CommandId cid,
									 uint32 options)
{
	TransactionId xid = GetCurrentTransactionId();

	tuple->t_data->t_infomask &= ~(HEAP_XACT_MASK);
	tuple->t_data->t_infomask2 &= ~(HEAP2_XACT_MASK);
	tuple->t_data->t_infomask |= HEAP_XMAX_INVALID;
	HeapTupleHeaderSetXmin(tuple->t_data, xid);
	if (options & HEAP_INSERT_FROZEN)
		HeapTupleHeaderSetXminFrozen(tuple->t_data);
	HeapTupleHeaderSetCmin(tuple->t_data, cid);
	HeapTupleHeaderSetXmax(tuple->t_data, 0);
	tuple->t_tableOid = RelationGetRelid(rel);
}

static void
fastpg_mem_fill_heap_tuple_values(Relation rel,
								  HeapTuple tuple,
								  uintptr_t *values,
								  uint8_t *isnull,
								  uint8_t *byval,
								  size_t *value_lens)
{
	TupleDesc	tupdesc = RelationGetDescr(rel);
	Datum	   *datums = palloc_array(Datum, tupdesc->natts);
	bool	   *nulls = palloc_array(bool, tupdesc->natts);

	heap_deform_tuple(tuple, tupdesc, datums, nulls);
	memset(values, 0, sizeof(uintptr_t) * tupdesc->natts);
	memset(isnull, 0, sizeof(uint8_t) * tupdesc->natts);
	memset(byval, 0, sizeof(uint8_t) * tupdesc->natts);
	memset(value_lens, 0, sizeof(size_t) * tupdesc->natts);

	for (int index = 0; index < tupdesc->natts; index++)
	{
		Form_pg_attribute attr = TupleDescAttr(tupdesc, index);

		isnull[index] = nulls[index] ? 1 : 0;
		byval[index] = attr->attbyval ? 1 : 0;
		if (nulls[index])
			continue;

		if (attr->attlen == -1)
		{
			struct varlena *raw = (struct varlena *) DatumGetPointer(datums[index]);

			values[index] = (uintptr_t) raw;
			value_lens[index] = VARSIZE_ANY(raw);
		}
		else
		{
			values[index] = (uintptr_t) datums[index];
			value_lens[index] = fastpg_mem_datum_size(datums[index], attr);
		}
	}

	pfree(datums);
	pfree(nulls);
}

static void
fastpg_mem_fill_slot_values_borrowed(Relation rel,
									 TupleTableSlot *slot,
									 uintptr_t *values,
									 uint8_t *isnull,
									 uint8_t *byval,
									 size_t *value_lens,
									 uint8_t *owned)
{
	fastpg_mem_fill_slot_values_internal(rel,
										 slot,
										 values,
										 isnull,
										 byval,
										 value_lens,
										 owned);
}

static void
fastpg_mem_prepare_slot_values(Relation rel,
							   TupleTableSlot *slot,
							   uintptr_t **values_out,
							   uint8_t **isnull_out,
							   uint8_t **byval_out,
							   size_t **value_lens_out)
{
	TupleDesc	tupdesc = RelationGetDescr(rel);
	uintptr_t  *values;
	uint8_t    *isnull;
	uint8_t    *byval;
	size_t	   *value_lens;

	values = palloc_array(uintptr_t, tupdesc->natts);
	isnull = palloc_array(uint8_t, tupdesc->natts);
	byval = palloc_array(uint8_t, tupdesc->natts);
	value_lens = palloc_array(size_t, tupdesc->natts);
	fastpg_mem_fill_slot_values(rel, slot, values, isnull, byval, value_lens);

	*values_out = values;
	*isnull_out = isnull;
	*byval_out = byval;
	*value_lens_out = value_lens;
}

static bool
fastpg_mem_relation_can_toast(Relation rel)
{
	return rel->rd_rel->relkind == RELKIND_RELATION ||
		rel->rd_rel->relkind == RELKIND_MATVIEW;
}

static FastPgMemToastState *
fastpg_mem_toast_state_locked(uint32_t relid, bool create)
{
	FastPgMemToastState *entry;

	for (entry = fastpg_mem_toast_states; entry != NULL; entry = entry->next)
	{
		if (entry->relid == relid)
			return entry;
	}

	if (!create)
		return NULL;

	entry = (FastPgMemToastState *) malloc(sizeof(FastPgMemToastState));
	if (entry == NULL)
		return NULL;
	entry->relid = relid;
	entry->may_have_external = false;
	entry->next = fastpg_mem_toast_states;
	fastpg_mem_toast_states = entry;
	return entry;
}

static bool
fastpg_mem_relation_may_have_external_toast(uint32_t relid)
{
	FastPgMemToastState *entry;
	bool		may_have_external = false;

	pthread_mutex_lock(&fastpg_mem_toast_state_lock);
	entry = fastpg_mem_toast_state_locked(relid, false);
	if (entry != NULL)
		may_have_external = entry->may_have_external;
	pthread_mutex_unlock(&fastpg_mem_toast_state_lock);

	return may_have_external;
}

static void
fastpg_mem_note_relation_external_toast(uint32_t relid)
{
	FastPgMemToastState *entry;

	pthread_mutex_lock(&fastpg_mem_toast_state_lock);
	entry = fastpg_mem_toast_state_locked(relid, true);
	if (entry != NULL)
		entry->may_have_external = true;
	pthread_mutex_unlock(&fastpg_mem_toast_state_lock);
}

static void
fastpg_mem_clear_relation_external_toast(uint32_t relid)
{
	FastPgMemToastState *entry;

	pthread_mutex_lock(&fastpg_mem_toast_state_lock);
	entry = fastpg_mem_toast_state_locked(relid, false);
	if (entry != NULL)
		entry->may_have_external = false;
	pthread_mutex_unlock(&fastpg_mem_toast_state_lock);
}

static bool
fastpg_mem_slot_has_external_varlena(Relation rel, TupleTableSlot *slot)
{
	TupleDesc	tupdesc = RelationGetDescr(rel);

	slot_getallattrs(slot);
	for (int index = 0; index < tupdesc->natts; index++)
	{
		Form_pg_attribute attr = TupleDescAttr(tupdesc, index);
		Pointer		raw;

		if (slot->tts_isnull[index] || attr->attlen != -1)
			continue;
		raw = DatumGetPointer(slot->tts_values[index]);
		if (raw != NULL && VARATT_IS_EXTERNAL(raw))
			return true;
	}
	return false;
}

static bool
fastpg_mem_slot_exceeds_toast_threshold(Relation rel, TupleTableSlot *slot)
{
	TupleDesc	tupdesc = RelationGetDescr(rel);
	Size		header_len;
	Size		tuple_len;
	bool		hasnull = false;

	slot_getallattrs(slot);
	for (int index = 0; index < tupdesc->natts; index++)
	{
		if (slot->tts_isnull[index])
		{
			hasnull = true;
			break;
		}
	}

	header_len = offsetof(HeapTupleHeaderData, t_bits);
	if (hasnull)
		header_len += BITMAPLEN(tupdesc->natts);
	tuple_len = MAXALIGN(header_len) +
		heap_compute_data_size(tupdesc, slot->tts_values, slot->tts_isnull);
	return tuple_len > TOAST_TUPLE_THRESHOLD;
}

static bool
fastpg_mem_relation_fits_without_toast(Relation rel)
{
	FastPgMemBlockLayout *entry;
	TupleDesc	tupdesc = RelationGetDescr(rel);
	Size		header_len;
	int32		data_width = 0;
	bool		result;

	entry =
		fastpg_mem_block_layout_entry((uint32_t) RelationGetRelid(rel), true);
	if (entry->fits_without_toast_valid &&
		entry->fits_without_toast_tupdesc == tupdesc)
		return entry->fits_without_toast;

	if (!fastpg_mem_relation_fast_data_width(rel, NULL, &data_width))
		result = false;
	else
	{
		header_len = offsetof(HeapTupleHeaderData, t_bits) + BITMAPLEN(tupdesc->natts);
		result = MAXALIGN(header_len) + (Size) data_width <= TOAST_TUPLE_THRESHOLD;
	}

	entry->fits_without_toast_tupdesc = tupdesc;
	entry->fits_without_toast_valid = true;
	entry->fits_without_toast = result;
	return result;
}

static bool
fastpg_mem_slot_needs_heap_tuple(Relation rel, TupleTableSlot *slot)
{
	if (!fastpg_catalog_mode_uses_postgres() ||
		!fastpg_mem_relation_can_toast(rel))
		return false;

	if (IsCatalogRelation(rel) && !IsToastRelation(rel))
		return true;
	if (rel->rd_rel->reltoastrelid == InvalidOid)
		return false;
	if (fastpg_mem_relation_fits_without_toast(rel))
		return false;
	if (fastpg_mem_slot_has_external_varlena(rel, slot))
		return true;
	return fastpg_mem_slot_exceeds_toast_threshold(rel, slot);
}

static void
fastpg_mem_local_relcache_invalidate_for_catalog_tuple(Relation rel,
													   HeapTuple tuple)
{
	Oid			tupleRelId = RelationGetRelid(rel);
	Oid			relationId = InvalidOid;

	if (!IsCatalogRelation(rel) || IsToastRelation(rel))
		return;

	if (tupleRelId == RelationRelationId)
	{
		Form_pg_class classtup = (Form_pg_class) GETSTRUCT(tuple);

		relationId = classtup->oid;
	}
	else if (tupleRelId == AttributeRelationId)
	{
		Form_pg_attribute atttup = (Form_pg_attribute) GETSTRUCT(tuple);

		relationId = atttup->attrelid;
	}
	else if (tupleRelId == IndexRelationId)
	{
		Form_pg_index indextup = (Form_pg_index) GETSTRUCT(tuple);

		relationId = indextup->indexrelid;
	}
	else if (tupleRelId == ConstraintRelationId)
	{
		Form_pg_constraint constrtup = (Form_pg_constraint) GETSTRUCT(tuple);

		if (constrtup->contype == CONSTRAINT_FOREIGN)
			relationId = constrtup->conrelid;
	}

	if (OidIsValid(relationId))
		RelationCacheInvalidateEntry(relationId);
}

static void
fastpg_mem_refresh_local_relcache_from_pg_class(Relation rel, HeapTuple tuple)
{
	Form_pg_class classtup;
	Relation	target;

	if (RelationGetRelid(rel) != RelationRelationId || tuple == NULL)
		return;

	classtup = (Form_pg_class) GETSTRUCT(tuple);
	target = RelationIdGetRelation(classtup->oid);
	if (target == NULL)
		return;

	target->rd_rel->relpages = classtup->relpages;
	target->rd_rel->reltuples = classtup->reltuples;
	target->rd_rel->relallvisible = classtup->relallvisible;
	target->rd_rel->relallfrozen = classtup->relallfrozen;
	RelationClose(target);
}

static void
fastpg_mem_cache_invalidate_heap_tuple(Relation rel,
									   HeapTuple tuple,
									   HeapTuple newtuple)
{
	if (IsCatalogRelation(rel) && !IsToastRelation(rel))
		CatalogCacheFlushCatalog(RelationGetRelid(rel));
	CacheInvalidateHeapTuple(rel, tuple, newtuple);
	fastpg_mem_local_relcache_invalidate_for_catalog_tuple(rel, tuple);
	fastpg_mem_refresh_local_relcache_from_pg_class(rel,
													newtuple != NULL ? newtuple : tuple);
}

static void
fastpg_mem_fill_virtual_tuple_attrs(Relation rel,
									TupleTableSlot *slot,
									const uintptr_t *values,
									const uint8_t *isnull,
									size_t stored_natts)
{
	int			natts = slot->tts_tupleDescriptor->natts;

	if (stored_natts > (size_t) natts)
		elog(ERROR, "fastpg_mem stored row has %zu attributes but relation \"%s\" has %d",
			 stored_natts,
			 RelationGetRelationName(rel),
			 natts);
	for (int index = 0; index < natts; index++)
	{
		slot->tts_values[index] = (Datum) values[index];
		slot->tts_isnull[index] = isnull[index] != 0;
	}
	if (stored_natts < (size_t) natts)
		slot_getmissingattrs(slot, (int) stored_natts, natts);
}

static void
fastpg_mem_store_virtual_tuple(Relation rel,
							   TupleTableSlot *slot,
							   const uintptr_t *values,
							   const uint8_t *isnull,
							   size_t stored_natts,
							   uint64_t row_id)
{
	fastpg_mem_fill_virtual_tuple_attrs(rel, slot, values, isnull, stored_natts);

	if (!fastpg_mem_row_id_to_tid(rel, row_id, &slot->tts_tid))
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
fastpg_mem_scan_begin_storage1(Relation rel, Snapshot snapshot, int nkeys,
							   ScanKeyData *key)
{
	uint32_t	relid = (uint32_t) RelationGetRelid(rel);
	int16_t		stack_attnums[FASTPG_MEM_STACK_NATTS];
	uintptr_t	stack_values[FASTPG_MEM_STACK_NATTS];
	int16_t    *attnums = stack_attnums;
	uintptr_t  *values = stack_values;
	size_t		filter_count = 0;
	uint64_t	scan_handle;
	bool		heap_buffers;
	bool		use_snapshot = fastpg_catalog_mode_uses_postgres() &&
		snapshot != NULL &&
		snapshot->snapshot_type == SNAPSHOT_MVCC;

	if (nkeys <= 0 || key == NULL ||
		fastpg_rust_catalog_policy_by_relation_oid(relid) == 0)
		return use_snapshot ?
			fastpg_rust_scan_begin_with_snapshot(relid,
												 1,
												 GetCurrentTransactionIdIfAny(),
												 snapshot->curcid) :
			fastpg_rust_scan_begin(relid);

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
		(use_snapshot ?
		 fastpg_rust_scan_begin_with_snapshot(relid,
											  1,
											  GetCurrentTransactionIdIfAny(),
											  snapshot->curcid) :
		 fastpg_rust_scan_begin(relid)) :
		fastpg_rust_scan_begin_filtered(relid, attnums, values, filter_count);

	if (heap_buffers)
	{
		pfree(attnums);
		pfree(values);
	}
	return scan_handle;
}

static void
fastpg_mem_scan_discard_batch(FastPgMemScanDesc *scan)
{
	scan->batch_count = 0;
	scan->batch_index = 0;
}

static void
fastpg_mem_scan_reset_batch_exhaustion(FastPgMemScanDesc *scan)
{
	scan->batch_exhausted_forward = false;
	scan->batch_exhausted_backward = false;
}

static void
fastpg_mem_scan_free_batch(FastPgMemScanDesc *scan)
{
	if (scan->batch_values != NULL)
		pfree(scan->batch_values);
	if (scan->batch_isnull != NULL)
		pfree(scan->batch_isnull);
	if (scan->batch_row_ids != NULL)
		pfree(scan->batch_row_ids);
	if (scan->batch_stored_natts != NULL)
		pfree(scan->batch_stored_natts);
	if (scan->batch_xmins != NULL)
		pfree(scan->batch_xmins);
	if (scan->batch_cmins != NULL)
		pfree(scan->batch_cmins);
	scan->batch_values = NULL;
	scan->batch_isnull = NULL;
	scan->batch_row_ids = NULL;
	scan->batch_stored_natts = NULL;
	scan->batch_xmins = NULL;
	scan->batch_cmins = NULL;
	scan->batch_natts = 0;
	fastpg_mem_scan_discard_batch(scan);
}

static void
fastpg_mem_scan_ensure_batch(FastPgMemScanDesc *scan, int natts)
{
	MemoryContext old_context;
	Size		value_count;

	if (scan->batch_row_ids != NULL && scan->batch_natts == natts)
		return;

	fastpg_mem_scan_free_batch(scan);
	scan->batch_natts = natts;
	old_context = MemoryContextSwitchTo(scan->batch_context);
	scan->batch_row_ids =
		palloc_array(uint64_t, FASTPG_MEM_SCAN_BATCH_ROWS);
	scan->batch_stored_natts =
		palloc_array(size_t, FASTPG_MEM_SCAN_BATCH_ROWS);
	if (!scan->storage2)
	{
		scan->batch_xmins =
			palloc_array(uint32_t, FASTPG_MEM_SCAN_BATCH_ROWS);
		scan->batch_cmins =
			palloc_array(uint32_t, FASTPG_MEM_SCAN_BATCH_ROWS);
	}

	if (natts <= 0)
	{
		MemoryContextSwitchTo(old_context);
		return;
	}

	value_count = (Size) natts * FASTPG_MEM_SCAN_BATCH_ROWS;
	scan->batch_values = palloc0_array(uintptr_t, value_count);
	scan->batch_isnull = palloc0_array(uint8_t, value_count);
	MemoryContextSwitchTo(old_context);
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
	scan->batch_context = CurrentMemoryContext;
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
	scan->batch_enabled = scan->storage2;
	if (scan->storage2 && fastpg_catalog_mode_uses_postgres())
	{
		AttrNumber	single_attnum;

		fastpg_mem_ensure_index_attr_bitmaps(rel);
		if (fastpg_mem_single_update_index_attr(rel,
												RelationGetDescr(rel),
												&single_attnum))
		{
			Form_pg_attribute attr =
				TupleDescAttr(RelationGetDescr(rel), single_attnum - 1);

			if (attr->attbyval)
			{
				scan->cache_single_index_key = true;
				scan->single_index_attnum = single_attnum;
			}
		}
	}
	scan->sample_block = InvalidBlockNumber;
	scan->analyze_current_block = InvalidBlockNumber;
	scan->analyze = (flags & SO_TYPE_ANALYZE) != 0;
	if (scan->analyze)
	{
		scan->analyze_row_count = scan->storage2 ?
			fastpg_storage2_relation_row_count(RelationGetRelid(rel)) :
			fastpg_rust_relation_row_count(RelationGetRelid(rel));
		scan->analyze_rows_per_block =
			fastpg_mem_relation_rows_per_block(rel);
		scan->analyze_total_blocks =
			fastpg_mem_heap_pages_for_layout(rel, scan->analyze_row_count);
		if (scan->analyze_rows_per_block == 0)
			scan->analyze_rows_per_block = 1;
	}
	scan->scan_handle = scan->storage2 ?
		(fastpg_catalog_mode_uses_postgres() &&
		 snapshot != NULL &&
		 snapshot->snapshot_type == SNAPSHOT_MVCC ?
		 fastpg_storage2_scan_begin_with_snapshot(RelationGetRelid(rel),
												  fastpg_mem_effective_snapshot_curcid(snapshot)) :
		 fastpg_storage2_scan_begin(RelationGetRelid(rel))) :
		fastpg_mem_scan_begin_storage1(rel, snapshot, nkeys, key);
	if (scan->scan_handle == 0)
		fastpg_mem_raise_storage_error("fastpg_mem failed to create Rust scan handle");

	if (fastpg_catalog_mode_uses_postgres())
	{
		pgstat_count_heap_scan(rel);
		fastpg_mem_count_io_op(rel, IOCONTEXT_NORMAL, IOOP_READ, 1);
		fastpg_mem_count_io_op(rel, IOCONTEXT_NORMAL, IOOP_HIT, 1);
	}

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
	fastpg_mem_scan_free_batch(scan);
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
	fastpg_mem_scan_discard_batch(scan);
	fastpg_mem_scan_reset_batch_exhaustion(scan);
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
	size_t		stored_natts = 0;
	uint32_t	row_xmin = 0;
	uint32_t	row_cmin = 0;
	bool		found;
	bool		heap_buffers = natts > FASTPG_MEM_STACK_NATTS;
	bool		forward = ScanDirectionIsBackward(direction) ? false : true;
	bool		use_batch;
	bool		needs_row_metadata;

	ExecClearTuple(slot);

	if (scan->batch_count > 0 && scan->batch_forward != forward)
	{
		if (scan->batch_count > scan->batch_index &&
			scan->storage2 && scan->batch_index > 0)
			(void) fastpg_storage2_scan_set_position(scan->scan_handle,
													 scan->batch_row_ids[scan->batch_index - 1]);
		fastpg_mem_scan_discard_batch(scan);
		fastpg_mem_scan_reset_batch_exhaustion(scan);
	}

	use_batch = scan->batch_enabled && !(scan->storage2 && !forward);
	if (!use_batch)
	{
		values = heap_buffers ? palloc0_array(uintptr_t, natts) : stack_values;
		isnull = heap_buffers ? palloc0_array(uint8_t, natts) : stack_isnull;
		needs_row_metadata =
			!scan->storage2 &&
			fastpg_mem_scan_needs_row_metadata(scan->base.rs_rd,
											   scan->base.rs_snapshot);

		while ((found =
				scan->storage2 ?
				fastpg_storage2_scan_next_with_stored_natts(scan->scan_handle,
															forward ? 1 : 0,
															values,
															isnull,
															natts,
															&row_id,
															&stored_natts) :
				needs_row_metadata ?
				fastpg_rust_scan_next_with_metadata(scan->scan_handle,
													forward ? 1 : 0,
													values,
													isnull,
													natts,
													&row_id,
													&stored_natts,
													&row_xmin,
													&row_cmin) :
				fastpg_rust_scan_next_with_stored_natts(scan->scan_handle,
														forward ? 1 : 0,
														values,
														isnull,
														natts,
														&row_id,
														&stored_natts)))
		{
			if (scan->storage2)
			{
				fastpg_mem_maybe_remember_scan_single_index_key(scan,
																row_id,
																values,
																isnull,
																stored_natts);
				fastpg_mem_fill_virtual_tuple_attrs(scan->base.rs_rd,
													slot,
													values,
													isnull,
													stored_natts);
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
											   stored_natts,
											   row_id);
			if (needs_row_metadata &&
				!fastpg_mem_row_metadata_visible_to_snapshot((TransactionId) row_xmin,
															 (CommandId) row_cmin,
															 scan->base.rs_snapshot))
			{
				ExecClearTuple(slot);
				continue;
			}
			if (scan->base.rs_key == NULL ||
				fastpg_mem_slot_key_test(slot, scan->base.rs_nkeys, scan->base.rs_key))
			{
				if (fastpg_catalog_mode_uses_postgres())
				{
					pgstat_count_heap_getnext(scan->base.rs_rd);
					pgstat_count_buffer_hit(scan->base.rs_rd);
				}
				break;
			}

			ExecClearTuple(slot);
		}

		if (heap_buffers)
		{
			pfree(values);
			pfree(isnull);
		}

		return found;
	}

	fastpg_mem_scan_ensure_batch(scan, natts);
	needs_row_metadata =
		!scan->storage2 &&
		fastpg_mem_scan_needs_row_metadata(scan->base.rs_rd,
										   scan->base.rs_snapshot);

	while (true)
	{
		int			batch_index;

		if (scan->batch_index >= scan->batch_count)
		{
			if ((forward && scan->batch_exhausted_forward) ||
				(!forward && scan->batch_exhausted_backward))
				return false;

			scan->batch_forward = forward;
			scan->batch_count = scan->storage2 ?
				(int) fastpg_storage2_scan_next_batch_with_stored_natts(scan->scan_handle,
																		forward ? 1 : 0,
																		scan->batch_values,
																		scan->batch_isnull,
																		(size_t) natts,
																		FASTPG_MEM_SCAN_BATCH_ROWS,
																		scan->batch_row_ids,
																		scan->batch_stored_natts) :
				(needs_row_metadata ?
				(int) fastpg_rust_scan_next_batch_with_metadata(scan->scan_handle,
																forward ? 1 : 0,
																scan->batch_values,
																scan->batch_isnull,
																(size_t) natts,
																FASTPG_MEM_SCAN_BATCH_ROWS,
																scan->batch_row_ids,
																scan->batch_stored_natts,
																scan->batch_xmins,
																scan->batch_cmins) :
				(int) fastpg_rust_scan_next_batch_with_stored_natts(scan->scan_handle,
																	forward ? 1 : 0,
																	scan->batch_values,
																	scan->batch_isnull,
																	(size_t) natts,
																	FASTPG_MEM_SCAN_BATCH_ROWS,
																		scan->batch_row_ids,
																		scan->batch_stored_natts));
			scan->batch_index = 0;
			if (scan->batch_count < FASTPG_MEM_SCAN_BATCH_ROWS)
			{
				if (forward)
				{
					scan->batch_exhausted_forward = true;
					scan->batch_exhausted_backward = false;
				}
				else
				{
					scan->batch_exhausted_backward = true;
					scan->batch_exhausted_forward = false;
				}
			}
			else if (forward)
				scan->batch_exhausted_backward = false;
			else
				scan->batch_exhausted_forward = false;
			if (scan->batch_count <= 0)
				return false;
		}

		batch_index = scan->batch_index++;
		values = natts > 0 ?
			scan->batch_values + ((Size) batch_index * natts) : NULL;
		isnull = natts > 0 ?
			scan->batch_isnull + ((Size) batch_index * natts) : NULL;
		row_id = scan->batch_row_ids[batch_index];
		stored_natts = scan->batch_stored_natts[batch_index];
		if (needs_row_metadata)
		{
			row_xmin = scan->batch_xmins[batch_index];
			row_cmin = scan->batch_cmins[batch_index];
		}

		if (scan->storage2)
		{
			fastpg_mem_maybe_remember_scan_single_index_key(scan,
															row_id,
															values,
															isnull,
															stored_natts);
			fastpg_mem_fill_virtual_tuple_attrs(scan->base.rs_rd,
												slot,
												values,
												isnull,
												stored_natts);
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
										   stored_natts,
										   row_id);
		if (needs_row_metadata &&
			!fastpg_mem_row_metadata_visible_to_snapshot((TransactionId) row_xmin,
														 (CommandId) row_cmin,
														 scan->base.rs_snapshot))
		{
			ExecClearTuple(slot);
			continue;
		}
		if (scan->base.rs_key == NULL ||
			fastpg_mem_slot_key_test(slot, scan->base.rs_nkeys, scan->base.rs_key))
		{
			if (fastpg_catalog_mode_uses_postgres())
			{
				if (scan->storage2 &&
					scan->base.rs_snapshot != NULL &&
					IsolationIsSerializable())
					PredicateLockTID(scan->base.rs_rd,
									 &slot->tts_tid,
									 scan->base.rs_snapshot,
									 (TransactionId) fastpg_storage2_relation_row_xmin((uint32_t) RelationGetRelid(scan->base.rs_rd),
																					   row_id));
				pgstat_count_heap_getnext(scan->base.rs_rd);
				pgstat_count_buffer_hit(scan->base.rs_rd);
			}
			return true;
		}

		ExecClearTuple(slot);
	}
}

static void
fastpg_mem_scan_set_tidrange(TableScanDesc sscan,
							 ItemPointer mintid,
							 ItemPointer maxtid)
{
	ItemPointerCopy(mintid, &sscan->st.tidrange.rs_mintid);
	ItemPointerCopy(maxtid, &sscan->st.tidrange.rs_maxtid);
}

static bool
fastpg_mem_scan_getnextslot_tidrange(TableScanDesc sscan,
									 ScanDirection direction,
									 TupleTableSlot *slot)
{
	ItemPointer mintid = &sscan->st.tidrange.rs_mintid;
	ItemPointer maxtid = &sscan->st.tidrange.rs_maxtid;

	if (ItemPointerCompare(mintid, maxtid) > 0)
	{
		ExecClearTuple(slot);
		return false;
	}

	while (fastpg_mem_scan_getnextslot(sscan, direction, slot))
	{
		int32		mincmp = ItemPointerCompare(&slot->tts_tid, mintid);
		int32		maxcmp = ItemPointerCompare(&slot->tts_tid, maxtid);

		if (mincmp >= 0 && maxcmp <= 0)
			return true;

		if ((ScanDirectionIsForward(direction) && maxcmp > 0) ||
			(ScanDirectionIsBackward(direction) && mincmp < 0))
		{
			ExecClearTuple(slot);
			return false;
		}

		ExecClearTuple(slot);
	}

	return false;
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
	size_t		stored_natts = 0;
	uint32_t	row_xmin = 0;
	uint32_t	row_cmin = 0;
	bool		found;
	bool		current_session_fetched = false;
	bool		heap_buffers = natts > FASTPG_MEM_STACK_NATTS;
	uint32_t	relid = (uint32_t) RelationGetRelid(rel);
	bool		storage2 = fastpg_mem_use_storage2_for_relid(relid);
	uint64_t	storage2_fetch_row_id = 0;
	bool		postgres_catalog = fastpg_catalog_mode_uses_postgres();
	bool		use_mvcc_snapshot =
		postgres_catalog &&
		snapshot != NULL &&
		snapshot->snapshot_type == SNAPSHOT_MVCC &&
		(!storage2 || fastpg_mem_relation_touched_by_current_xact(relid));

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
		storage2_fetch_row_id = row_id;
	}
	else if (!fastpg_mem_tid_to_row_id(rel, tid, &row_id))
		return false;

	ExecClearTuple(slot);
	values = heap_buffers ? palloc0_array(uintptr_t, natts) : stack_values;
	isnull = heap_buffers ? palloc0_array(uint8_t, natts) : stack_isnull;
	if (storage2 && snapshot != SnapshotAny)
	{
		uint64_t	current_session_row_id = 0;

		current_session_fetched =
			fastpg_storage2_fetch_current_session_tid_with_stored_natts(relid,
																		row_id,
																		use_mvcc_snapshot ? 1 : 0,
																		use_mvcc_snapshot ?
																		fastpg_mem_effective_snapshot_curcid(snapshot) : 0,
																		values,
																		isnull,
																		natts,
																		&stored_natts,
																		&current_session_row_id);
		if (current_session_fetched && current_session_row_id != 0)
			storage2_fetch_row_id = current_session_row_id;
	}
	if (storage2 &&
		!current_session_fetched &&
		postgres_catalog &&
		snapshot != SnapshotAny &&
		!use_mvcc_snapshot)
		storage2_fetch_row_id =
			fastpg_mem_storage2_resolve_row_id_read(relid, row_id);
	if (current_session_fetched)
		found = true;
	else
		found = storage2 ?
			(snapshot == SnapshotAny ?
			 fastpg_storage2_fetch_tid_any_with_stored_natts(relid,
														 row_id,
														 values,
														 isnull,
														 natts,
														 &stored_natts) :
			 (use_mvcc_snapshot ?
			  fastpg_storage2_fetch_tid_snapshot_with_stored_natts(relid,
																   row_id,
																   fastpg_mem_effective_snapshot_curcid(snapshot),
																   values,
																   isnull,
																   natts,
																   &stored_natts) :
			  fastpg_storage2_fetch_resolved_tid_with_stored_natts(relid,
																   storage2_fetch_row_id,
																   values,
																   isnull,
																   natts,
																   &stored_natts))) :
		(snapshot == SnapshotAny ?
		 fastpg_rust_fetch_row_any_with_stored_natts(RelationGetRelid(rel),
													 row_id,
													 values,
													 isnull,
													 natts,
													 &stored_natts) :
		 (use_mvcc_snapshot ?
		  fastpg_rust_fetch_row_with_snapshot_stored_natts(RelationGetRelid(rel),
														   row_id,
														   1,
														   GetCurrentTransactionIdIfAny(),
														   snapshot->curcid,
														   values,
														   isnull,
														   natts,
														   &stored_natts,
														   &row_xmin,
														   &row_cmin) :
		  fastpg_rust_fetch_row_with_stored_natts(RelationGetRelid(rel),
												  row_id,
												  values,
												  isnull,
												  natts,
												  &stored_natts)));
		if (found && storage2)
		{
			ItemPointerData slot_tid = *tid;

			fastpg_mem_fill_virtual_tuple_attrs(rel, slot, values, isnull, stored_natts);
			if (storage2_fetch_row_id != 0 &&
				storage2_fetch_row_id != row_id &&
				!fastpg_mem_storage2_tid_to_tid(storage2_fetch_row_id, &slot_tid))
			slot_tid = *tid;
		slot->tts_tid = slot_tid;
		slot->tts_tableOid = relid;
		ExecStoreVirtualTuple(slot);
	}
	else if (found)
		fastpg_mem_store_virtual_tuple(rel,
									   slot,
									   values,
									   isnull,
									   stored_natts,
									   row_id);
	if (found && !storage2 && !use_mvcc_snapshot &&
		!fastpg_mem_row_visible_to_snapshot(rel, row_id, snapshot))
	{
		ExecClearTuple(slot);
		found = false;
	}
	if (found &&
		snapshot != NULL &&
		postgres_catalog &&
		IsolationIsSerializable())
	{
		TransactionId predicate_xmin = storage2 ?
			(TransactionId) fastpg_storage2_relation_row_xmin(relid,
															  storage2_fetch_row_id != 0 ?
															  storage2_fetch_row_id : row_id) :
			(use_mvcc_snapshot ?
			 (TransactionId) row_xmin :
			 (TransactionId) fastpg_rust_relation_row_xmin((uint32_t) RelationGetRelid(rel),
														   row_id));

		PredicateLockTID(rel, &slot->tts_tid, snapshot, predicate_xmin);
	}
	if (found && postgres_catalog)
	{
		pgstat_count_heap_fetch(rel);
		pgstat_count_buffer_hit(rel);
	}
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
	ItemPointerData resolved_tid;
	bool		storage2 =
		fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(scan->rel));
	uint64_t	index_row_id = storage2 ?
		fastpg_mem_tid_to_storage2_tid(tid) : 0;
	bool		found;

	*call_again = false;
	if (all_dead != NULL)
		*all_dead = false;
	if (storage2 &&
		FastPgMemResolveIndexFetchTid(scan->rel, tid, &resolved_tid))
	{
		uint32_t	relid = (uint32_t) RelationGetRelid(scan->rel);
		uint64_t	root_row_id = fastpg_mem_tid_to_storage2_tid(tid);
		uint64_t	resolved_row_id = fastpg_mem_tid_to_storage2_tid(&resolved_tid);

		if (fastpg_catalog_mode_uses_postgres())
			fastpg_mem_record_storage2_lock_root(relid,
												 root_row_id,
												 resolved_row_id);
		found = fastpg_mem_tuple_fetch_row_version(scan->rel,
												   &resolved_tid,
												   snapshot,
												   slot);
	}
	else
		found = fastpg_mem_tuple_fetch_row_version(scan->rel,
												   tid,
												   snapshot,
												   slot);

	if (!found && all_dead != NULL && storage2 && index_row_id != 0)
	{
		*all_dead =
			fastpg_storage2_relation_index_tid_all_dead((uint32_t) RelationGetRelid(scan->rel),
														index_row_id);
	}

	return found;
}

bool
FastPgMemIndexFetchTupleCheck(Relation rel,
							  ItemPointer tid,
							  Snapshot snapshot,
							  bool *all_dead)
{
	bool		storage2 =
		fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(rel));
	bool		direct_storage2_snapshot =
		storage2 &&
		snapshot != NULL &&
		snapshot != SnapshotAny &&
		snapshot->snapshot_type != SNAPSHOT_MVCC;
	uint64_t	index_row_id = 0;

	if (all_dead != NULL)
		*all_dead = false;
	if (snapshot != NULL && snapshot->snapshot_type == SNAPSHOT_DIRTY)
	{
		snapshot->xmin = InvalidTransactionId;
		snapshot->xmax = InvalidTransactionId;
		snapshot->speculativeToken = 0;
	}

	if (!direct_storage2_snapshot)
	{
		TupleTableSlot *slot;
		bool		found;

		slot = table_slot_create(rel, NULL);
		found = fastpg_mem_tuple_fetch_row_version(rel, tid, snapshot, slot);
		ExecDropSingleTupleTableSlot(slot);
		return found;
	}

	index_row_id = fastpg_mem_tid_to_storage2_tid(tid);
	if (index_row_id == 0)
		return false;

	{
		ItemPointerData resolved_tid;
		uint64_t	current_session_row_id = 0;
		uint64_t	fetch_row_id = index_row_id;

		if (fastpg_storage2_relation_current_session_visible_tid((uint32_t) RelationGetRelid(rel),
																 index_row_id,
																 0,
																 0,
																 &current_session_row_id) &&
			current_session_row_id != 0)
		{
			if (fastpg_catalog_mode_uses_postgres())
			{
				pgstat_count_heap_fetch(rel);
				pgstat_count_buffer_hit(rel);
			}
			return true;
		}

		if (FastPgMemResolveIndexFetchTid(rel, tid, &resolved_tid))
		{
			uint64_t	resolved_row_id =
				fastpg_mem_tid_to_storage2_tid(&resolved_tid);

			if (resolved_row_id != 0)
			{
				if (fastpg_catalog_mode_uses_postgres())
					fastpg_mem_record_storage2_lock_root((uint32_t) RelationGetRelid(rel),
														 index_row_id,
														 resolved_row_id);
				fetch_row_id = resolved_row_id;
			}
		}

		if (fastpg_storage2_relation_contains_tid((uint32_t) RelationGetRelid(rel),
												  fetch_row_id))
		{
			if (fastpg_catalog_mode_uses_postgres())
			{
				pgstat_count_heap_fetch(rel);
				pgstat_count_buffer_hit(rel);
			}
			return true;
		}
	}

	if (all_dead != NULL)
		*all_dead =
			fastpg_storage2_relation_index_tid_all_dead((uint32_t) RelationGetRelid(rel),
														index_row_id);

	return false;
}

static bool
fastpg_mem_tuple_tid_valid(TableScanDesc scan, ItemPointer tid)
{
	uint64_t	row_id;

	if (fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(scan->rs_rd)))
	{
		uint32_t	relid = (uint32_t) RelationGetRelid(scan->rs_rd);
		uint64_t	storage2_tid = fastpg_mem_tid_to_storage2_tid(tid);
		uint64_t	resolved_tid;

		if (storage2_tid == 0)
			return false;
		if (fastpg_storage2_relation_current_session_visible_tid(relid,
																 storage2_tid,
																 0,
																 0,
																 NULL))
			return true;
		if (fastpg_storage2_relation_contains_tid(relid, storage2_tid))
			return true;
		if (!fastpg_catalog_mode_uses_postgres())
			return false;
		resolved_tid = fastpg_mem_storage2_resolve_update_row_id_read(relid,
																	  storage2_tid);
		return resolved_tid != storage2_tid &&
			fastpg_storage2_relation_contains_tid(relid, resolved_tid);
	}

	if (!fastpg_mem_tid_to_row_id(scan->rs_rd, tid, &row_id))
		return false;
	if (fastpg_catalog_mode_uses_postgres())
		row_id = fastpg_mem_resolve_row_redirect((uint32_t) RelationGetRelid(scan->rs_rd),
												 row_id);

	return fastpg_rust_relation_contains_row(RelationGetRelid(scan->rs_rd),
											 row_id);
}

static void
fastpg_mem_tuple_get_latest_tid(TableScanDesc scan, ItemPointer tid)
{
	uint64_t	row_id;
	uint64_t	latest_row_id;
	bool		storage2 = fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(scan->rs_rd));

	if (!fastpg_catalog_mode_uses_postgres())
		return;
	if (storage2)
	{
		row_id = fastpg_mem_tid_to_storage2_tid(tid);
		if (row_id == 0)
			return;
		latest_row_id =
			fastpg_mem_storage2_resolve_update_row_id_read((uint32_t) RelationGetRelid(scan->rs_rd),
														   row_id);
		if (latest_row_id != row_id &&
			!fastpg_mem_storage2_tid_to_tid(latest_row_id, tid))
			elog(ERROR, "fastpg_mem storage2 TID %llu cannot be represented as a CTID",
				 (unsigned long long) latest_row_id);
		return;
	}
	if (!fastpg_mem_tid_to_row_id(scan->rs_rd, tid, &row_id))
		return;

	latest_row_id =
		fastpg_mem_resolve_row_redirect((uint32_t) RelationGetRelid(scan->rs_rd),
										row_id);
	if (latest_row_id != row_id &&
		!fastpg_mem_row_id_to_tid(scan->rs_rd, latest_row_id, tid))
		elog(ERROR, "fastpg_mem row id %llu cannot be represented as a CTID",
			 (unsigned long long) latest_row_id);
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
	bool		index_is_unique =
		buildstate->index_info == NULL ?
		(index->rd_index != NULL && index->rd_index->indisunique) :
		buildstate->index_info->ii_Unique;
	IndexUniqueCheck checkUnique =
		(!buildstate->validate_unique_once &&
		 index_is_unique) ?
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
									   bool storage2,
									   uint64_t row_id)
{
	ItemPointerData tid;
	TupleTableSlot *slot;
	EState	   *estate;
	Datum		values[INDEX_MAX_KEYS];
	bool		isnull[INDEX_MAX_KEYS];
	char	   *key_desc = NULL;

	if (storage2)
	{
		if (!fastpg_mem_storage2_tid_to_tid(row_id, &tid))
			return NULL;
	}
	else if (!fastpg_mem_row_id_to_tid(heapRelation, row_id, &tid))
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

static bool
fastpg_mem_unique_conflict_scan(Relation heapRelation,
								Relation indexRelation,
								const Datum *values,
								const bool *isnull,
								int key_count,
								uint64_t self_row_id,
								uint64_t *conflict_row_id)
{
	TupleDesc	tupdesc = RelationGetDescr(heapRelation);
	TupleTableSlot *slot;
	uint64_t	scan_handle;
	int			natts = tupdesc->natts;
	uintptr_t	stack_values[FASTPG_MEM_STACK_NATTS];
	uint8_t		stack_isnull[FASTPG_MEM_STACK_NATTS];
	uintptr_t  *scan_values;
	uint8_t    *scan_isnull;
	bool		heap_buffers = natts > FASTPG_MEM_STACK_NATTS;
	uint64_t	row_id = 0;
	size_t		stored_natts = 0;
	bool		found = false;

	if (!indexRelation->rd_index->indnullsnotdistinct)
	{
		for (int index = 0; index < key_count; index++)
		{
			if (isnull[index])
				return false;
		}
	}

	scan_handle = fastpg_rust_scan_begin(RelationGetRelid(heapRelation));
	if (scan_handle == 0)
		fastpg_mem_raise_storage_error("fastpg_mem failed to create Rust scan handle");

	slot = MakeSingleTupleTableSlot(tupdesc, fastpg_mem_slot_callbacks(heapRelation));
	scan_values = heap_buffers ? palloc0_array(uintptr_t, natts) : stack_values;
	scan_isnull = heap_buffers ? palloc0_array(uint8_t, natts) : stack_isnull;

	while (fastpg_rust_scan_next_with_stored_natts(scan_handle,
												  1,
												  scan_values,
												  scan_isnull,
												  natts,
												  &row_id,
												  &stored_natts))
	{
		bool		matched = true;

		if (row_id == self_row_id)
			continue;

		ExecClearTuple(slot);
		fastpg_mem_store_virtual_tuple(heapRelation,
									   slot,
									   scan_values,
									   scan_isnull,
									   stored_natts,
									   row_id);

		for (int index = 0; index < key_count; index++)
		{
			AttrNumber	heap_attnum = indexRelation->rd_index->indkey.values[index];
			Datum		existing;
			bool		existing_isnull;

			if (heap_attnum <= 0 || heap_attnum > tupdesc->natts)
				fastpg_mem_index_unsupported("indexes with unsupported key metadata");

			existing = slot_getattr(slot, heap_attnum, &existing_isnull);
			if (existing_isnull || isnull[index])
			{
				if (!indexRelation->rd_index->indnullsnotdistinct ||
					existing_isnull != isnull[index])
				{
					matched = false;
					break;
				}
				continue;
			}

			if (DatumGetInt32(FunctionCall2Coll(index_getprocinfo(indexRelation,
																  index + 1,
																  BTORDER_PROC),
												 indexRelation->rd_indcollation[index],
												 existing,
												 values[index])) != 0)
			{
				matched = false;
				break;
			}
		}

		if (matched)
		{
			*conflict_row_id = row_id;
			found = true;
			break;
		}
	}

	fastpg_rust_scan_end(scan_handle);
	ExecDropSingleTupleTableSlot(slot);
	if (heap_buffers)
	{
		pfree(scan_values);
		pfree(scan_isnull);
	}

	return found;
}

static IndexBuildResult *
fastpg_mem_index_build(Relation heapRelation, Relation indexRelation,
					   IndexInfo *indexInfo)
{
	IndexBuildResult *result = palloc0_object(IndexBuildResult);
	FastPgMemIndexBuildState buildstate;
	bool		storage2 =
		fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(heapRelation));

	if (fastpg_catalog_mode_uses_postgres() &&
		FastPgMemRelationPhysicalPages(heapRelation) == 0)
		return result;

	if (storage2 && !fastpg_catalog_mode_uses_postgres())
		(void) fastpg_storage2_rebuild_primary_key_index((uint32_t) RelationGetRelid(indexRelation));

	buildstate.heap_relation = heapRelation;
	buildstate.index_info = indexInfo;
	buildstate.index_tuples = 0.0;
	buildstate.validate_unique_once = false;

	if (fastpg_catalog_mode_uses_postgres() &&
		indexRelation->rd_index != NULL &&
		(indexInfo == NULL ?
		 indexRelation->rd_index->indisunique :
		 indexInfo->ii_Unique))
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

		if (storage2 && indexRelation->rd_index->indisprimary)
			(void) fastpg_storage2_rebuild_primary_key_index_with_spec((uint32_t) RelationGetRelid(indexRelation),
																	   (uint32_t) RelationGetRelid(heapRelation),
																	   fastpg_attnums,
																	   fastpg_typbyval,
																	   fastpg_typlen,
																	   (size_t) key_count);

		if (storage2 ?
			fastpg_storage2_unique_index_validate_with_spec((uint32_t) RelationGetRelid(indexRelation),
															(uint32_t) RelationGetRelid(heapRelation),
															fastpg_attnums,
															fastpg_typbyval,
															fastpg_typlen,
															(size_t) key_count,
															indexRelation->rd_index->indnullsnotdistinct ? 1 : 0,
															&conflict_row_id) :
			fastpg_rust_unique_index_validate_with_spec((uint32_t) RelationGetRelid(indexRelation),
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
													   storage2,
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
			fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(heapRelation));

		if (indexUnchanged)
			return true;

		if (key_count <= 0 || key_count > FASTPG_MAX_INDEX_KEYS)
			fastpg_mem_index_unsupported("unique indexes with invalid key count");
		if (storage2)
		{
			self_row_id = fastpg_mem_tid_to_storage2_tid(heap_tid);
			if (self_row_id == 0)
				elog(ERROR, "fastpg_mem heap TID cannot be represented as a storage2 TID");
		}
		else if (!fastpg_mem_tid_to_row_id(heapRelation, heap_tid, &self_row_id))
			elog(ERROR, "fastpg_mem heap TID cannot be represented as a row id");
		if (fastpg_catalog_mode_uses_postgres() && !storage2)
			self_row_id =
				fastpg_mem_resolve_row_redirect((uint32_t) RelationGetRelid(heapRelation),
												 self_row_id);
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

		if ((storage2 ?
			 (fastpg_catalog_mode_uses_postgres() ?
			  fastpg_storage2_unique_index_conflict_with_spec((uint32_t) RelationGetRelid(indexRelation),
															  (uint32_t) RelationGetRelid(heapRelation),
															  fastpg_attnums,
															  fastpg_typbyval,
															  fastpg_typlen,
															  fastpg_values,
															  fastpg_isnull,
															  (size_t) key_count,
															  indexRelation->rd_index->indisprimary ? 1 : 0,
															  indexRelation->rd_index->indnullsnotdistinct ? 1 : 0,
															  self_row_id,
															  &conflict_row_id) :
			  fastpg_storage2_unique_index_conflict((uint32_t) RelationGetRelid(indexRelation),
													fastpg_values,
													fastpg_isnull,
													(size_t) key_count,
													self_row_id,
													&conflict_row_id)) :
			 (fastpg_catalog_mode_uses_postgres() ?
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

	if (fastpg_catalog_mode_uses_postgres() &&
		heapRelation != NULL &&
		indexRelation->rd_index != NULL &&
		indexRelation->rd_index->indisprimary &&
		!indexUnchanged &&
		fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(heapRelation)))
	{
		int			key_count =
			IndexRelationGetNumberOfKeyAttributes(indexRelation);
		uintptr_t	fastpg_values[FASTPG_MAX_INDEX_KEYS];
		uint8_t		fastpg_isnull[FASTPG_MAX_INDEX_KEYS];
		int16_t		fastpg_attnums[FASTPG_MAX_INDEX_KEYS];
		uint8_t		fastpg_typbyval[FASTPG_MAX_INDEX_KEYS];
		int16_t		fastpg_typlen[FASTPG_MAX_INDEX_KEYS];
		uint64_t	self_row_id;

		if (key_count <= 0 || key_count > FASTPG_MAX_INDEX_KEYS)
			fastpg_mem_index_unsupported("primary indexes with invalid key count");
		self_row_id = fastpg_mem_tid_to_storage2_tid(heap_tid);
		if (self_row_id == 0)
			elog(ERROR, "fastpg_mem heap TID cannot be represented as a storage2 TID");
		if (!fastpg_mem_index_spec(indexRelation,
								   heapRelation,
								   key_count,
								   fastpg_attnums,
								   fastpg_typbyval,
								   fastpg_typlen))
			fastpg_mem_index_unsupported("indexes with unsupported key metadata");
		for (int index = 0; index < key_count; index++)
		{
			fastpg_values[index] = (uintptr_t) values[index];
			fastpg_isnull[index] = isnull[index] ? 1 : 0;
		}
		if (!fastpg_storage2_primary_key_index_insert_with_spec((uint32_t) RelationGetRelid(indexRelation),
																(uint32_t) RelationGetRelid(heapRelation),
																fastpg_attnums,
																fastpg_typbyval,
																fastpg_typlen,
																fastpg_values,
																fastpg_isnull,
																(size_t) key_count,
																self_row_id))
			fastpg_mem_raise_storage_error("fastpg_mem failed to record primary key index insert");
		if (key_count == 1 && fastpg_typbyval[0] != 0)
			fastpg_mem_remember_single_byval_index_key((uint32_t) RelationGetRelid(heapRelation),
													   self_row_id,
													   (AttrNumber) fastpg_attnums[0],
													   fastpg_values[0],
													   fastpg_isnull[0]);
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
FastPgMemResolveIndexFetchTid(Relation heapRelation,
							  const ItemPointerData *tupleid,
							  ItemPointer resolvedTid)
{
	uint64_t	row_id;
	uint64_t	resolved_row_id;

	if (tupleid == NULL ||
		!ItemPointerIsValid(tupleid) ||
		resolvedTid == NULL ||
		!fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(heapRelation)))
		return false;

	row_id = fastpg_mem_tid_to_storage2_tid((ItemPointer) tupleid);
	if (row_id == 0)
		return false;
	if (fastpg_storage2_relation_current_session_visible_tid((uint32_t) RelationGetRelid(heapRelation),
															 row_id,
															 0,
															 0,
															 &resolved_row_id) &&
		resolved_row_id != 0)
		return fastpg_mem_storage2_tid_to_tid(resolved_row_id, resolvedTid);
	resolved_row_id =
		fastpg_mem_storage2_resolve_row_id_read((uint32_t) RelationGetRelid(heapRelation),
												row_id);
	if (resolved_row_id == 0)
		return false;
	return fastpg_mem_storage2_tid_to_tid(resolved_row_id, resolvedTid);
}

bool
FastPgMemLookupPrimaryKeyTuple(Relation heapRelation,
							   Relation indexRelation,
							   const Datum *values,
							   const bool *isnull,
							   int nkeys,
							   Snapshot snapshot,
							   TupleTableSlot *slot,
							   bool *handled)
{
	int16_t		fastpg_attnums[FASTPG_MAX_INDEX_KEYS];
	uint8_t		fastpg_typbyval[FASTPG_MAX_INDEX_KEYS];
	int16_t		fastpg_typlen[FASTPG_MAX_INDEX_KEYS];
	uintptr_t	fastpg_values[FASTPG_MAX_INDEX_KEYS];
	uint8_t		fastpg_isnull[FASTPG_MAX_INDEX_KEYS];
	uint64_t	row_id = 0;
	ItemPointerData tid;

	if (handled != NULL)
		*handled = false;
	if (heapRelation == NULL ||
		indexRelation == NULL ||
		values == NULL ||
		isnull == NULL ||
		slot == NULL ||
		indexRelation->rd_indam != GetFastPgMemIndexAmRoutine() ||
		nkeys <= 0 ||
		nkeys > FASTPG_MAX_INDEX_KEYS ||
		!fastpg_catalog_mode_uses_postgres() ||
		!fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(heapRelation)))
		return false;
	if (!fastpg_mem_index_spec(indexRelation, heapRelation, nkeys,
							   fastpg_attnums, fastpg_typbyval, fastpg_typlen))
		return false;

	if (handled != NULL)
		*handled = true;

	for (int index = 0; index < nkeys; index++)
	{
		fastpg_values[index] = (uintptr_t) values[index];
		fastpg_isnull[index] = isnull[index] ? 1 : 0;
	}

	if (nkeys == 1 && fastpg_typbyval[0] != 0)
	{
		if (!fastpg_mem_cached_single_byval_index_lookup((uint32_t) RelationGetRelid(heapRelation),
														 (AttrNumber) fastpg_attnums[0],
														 fastpg_values[0],
														 fastpg_isnull[0],
														 &row_id))
			(void) fastpg_storage2_primary_key_index_lookup_single_byval_with_spec((uint32_t) RelationGetRelid(indexRelation),
																				   (uint32_t) RelationGetRelid(heapRelation),
																				   fastpg_values[0],
																				   fastpg_isnull[0],
																				   &row_id);
		if (row_id == 0)
			(void) fastpg_storage2_primary_key_index_lookup_with_spec((uint32_t) RelationGetRelid(indexRelation),
																	  (uint32_t) RelationGetRelid(heapRelation),
																	  fastpg_attnums,
																	  fastpg_typbyval,
																	  fastpg_typlen,
																	  fastpg_values,
																	  fastpg_isnull,
																	  (size_t) nkeys,
																	  &row_id);
	}
	else
		(void) fastpg_storage2_primary_key_index_lookup_with_spec((uint32_t) RelationGetRelid(indexRelation),
																  (uint32_t) RelationGetRelid(heapRelation),
																  fastpg_attnums,
																  fastpg_typbyval,
																  fastpg_typlen,
																  fastpg_values,
																  fastpg_isnull,
																  (size_t) nkeys,
																  &row_id);
	if (row_id == 0)
		return false;
	if (nkeys == 1 && fastpg_typbyval[0] != 0)
		fastpg_mem_remember_single_byval_index_key((uint32_t) RelationGetRelid(heapRelation),
												   row_id,
												   (AttrNumber) fastpg_attnums[0],
												   fastpg_values[0],
												   fastpg_isnull[0]);
	if (!fastpg_mem_storage2_tid_to_tid(row_id, &tid))
		return false;
	return fastpg_mem_tuple_fetch_row_version(heapRelation, &tid, snapshot, slot);
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

	storage2 = fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(heapRelation));
	if (tupleid != NULL && ItemPointerIsValid(tupleid))
	{
		if (storage2)
		{
			self_row_id = fastpg_mem_tid_to_storage2_tid((ItemPointer) tupleid);
			if (self_row_id == 0)
				return false;
		}
		else if (!fastpg_mem_tid_to_row_id(heapRelation,
										   (ItemPointer) tupleid,
										   &self_row_id))
			return false;
		if (fastpg_catalog_mode_uses_postgres() && !storage2)
			self_row_id =
				fastpg_mem_resolve_row_redirect((uint32_t) RelationGetRelid(heapRelation),
												 self_row_id);
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

	conflict = storage2 ?
		(fastpg_catalog_mode_uses_postgres() ?
		 fastpg_storage2_unique_index_conflict_with_spec((uint32_t) RelationGetRelid(indexRelation),
														 (uint32_t) RelationGetRelid(heapRelation),
														 fastpg_attnums,
														 fastpg_typbyval,
														 fastpg_typlen,
														 fastpg_values,
														 fastpg_isnull,
														 (size_t) key_count,
														 indexRelation->rd_index->indisprimary ? 1 : 0,
														 indexRelation->rd_index->indnullsnotdistinct ? 1 : 0,
														 self_row_id,
														 &conflict_row_id) :
		 fastpg_storage2_unique_index_conflict((uint32_t) RelationGetRelid(indexRelation),
											   fastpg_values,
											   fastpg_isnull,
											   (size_t) key_count,
											   self_row_id,
											   &conflict_row_id)) :
		(fastpg_catalog_mode_uses_postgres() ?
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
		else if (!fastpg_mem_row_id_to_tid(heapRelation,
										   conflict_row_id,
										   conflictTid))
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
	opaque->array_key_index = -1;
	scan->opaque = opaque;
	return scan;
}

static void
fastpg_mem_index_clear_array_keys(FastPgMemIndexScan *opaque)
{
	if (opaque->array_values != NULL)
		pfree(opaque->array_values);
	if (opaque->array_isnull != NULL)
		pfree(opaque->array_isnull);
	opaque->array_values = NULL;
	opaque->array_isnull = NULL;
	opaque->array_nelems = 0;
	opaque->array_index = 0;
	opaque->array_key_index = -1;
}

static void
fastpg_mem_index_clear_scan_keys(FastPgMemIndexScan *opaque)
{
	if (opaque->scan_keys != NULL)
		pfree(opaque->scan_keys);
	opaque->scan_keys = NULL;
	opaque->scan_nkeys = 0;
}

static void
fastpg_mem_index_clear_matches(FastPgMemIndexScan *opaque)
{
	if (opaque->matched_rows != NULL)
	{
		for (int match_index = 0; match_index < opaque->matched_count; match_index++)
		{
			FastPgMemIndexMatch *match = &opaque->matched_rows[match_index];

			for (int key_index = 0; key_index < FASTPG_MAX_INDEX_KEYS; key_index++)
			{
				if (match->owned[key_index])
				{
					pfree(DatumGetPointer(match->values[key_index]));
					match->owned[key_index] = false;
				}
			}
		}
		pfree(opaque->matched_rows);
	}
	opaque->matched_rows = NULL;
	opaque->matched_count = 0;
	opaque->matched_capacity = 0;
	opaque->matched_index = 0;
	opaque->matched_ready = false;
}

static void
fastpg_mem_index_release_scan(FastPgMemIndexScan *opaque)
{
	if (opaque->scan_handle != 0)
	{
		if (opaque->scan_storage2)
			fastpg_storage2_scan_end(opaque->scan_handle);
		else
			fastpg_rust_scan_end(opaque->scan_handle);
		opaque->scan_handle = 0;
	}
	if (opaque->scan_slot != NULL)
	{
		ExecDropSingleTupleTableSlot(opaque->scan_slot);
		opaque->scan_slot = NULL;
	}
	fastpg_mem_index_clear_matches(opaque);
}

static bool
fastpg_mem_index_key_matches_slot(Relation indexRelation,
								  Relation heapRelation,
								  TupleTableSlot *slot,
								  ScanKey key)
{
	int			key_index = key->sk_attno - 1;
	AttrNumber	heap_attnum;
	Datum		value;
	bool		isnull;

	if (key->sk_flags & (SK_ORDER_BY | SK_ROW_HEADER | SK_ROW_MEMBER))
		fastpg_mem_index_unsupported("non-scalar scan keys");
	if (key_index < 0 ||
		indexRelation->rd_index == NULL ||
		key_index >= IndexRelationGetNumberOfKeyAttributes(indexRelation))
		fastpg_mem_index_unsupported("scan keys outside the primary-key prefix");

	heap_attnum = indexRelation->rd_index->indkey.values[key_index];
	if (heap_attnum <= 0 ||
		heap_attnum > RelationGetDescr(heapRelation)->natts)
		fastpg_mem_index_unsupported("indexes with unsupported key metadata");

	value = slot_getattr(slot, heap_attnum, &isnull);
	if (key->sk_flags & SK_SEARCHNULL)
		return isnull;
	if (key->sk_flags & SK_SEARCHNOTNULL)
		return !isnull;
	if (isnull || (key->sk_flags & SK_ISNULL))
		return false;

	if (key->sk_flags & SK_SEARCHARRAY)
	{
		ArrayType  *array = DatumGetArrayTypeP(key->sk_argument);
		Oid			elemtype = ARR_ELEMTYPE(array);
		int16		typlen;
		bool		typbyval;
		char		typalign;
		Datum	   *array_values;
		bool	   *array_isnull;
		int			array_nelems;
		bool		matched = false;

		get_typlenbyvalalign(elemtype, &typlen, &typbyval, &typalign);
		deconstruct_array(array,
						  elemtype,
						  typlen,
						  typbyval,
						  typalign,
						  &array_values,
						  &array_isnull,
						  &array_nelems);
		for (int index = 0; index < array_nelems; index++)
		{
			Datum		test;

			if (array_isnull[index])
				continue;
			test = FunctionCall2Coll(&key->sk_func,
									 key->sk_collation,
									 value,
									 array_values[index]);
			if (DatumGetBool(test))
			{
				matched = true;
				break;
			}
		}
		pfree(array_values);
		pfree(array_isnull);
		return matched;
	}

	return DatumGetBool(FunctionCall2Coll(&key->sk_func,
										  key->sk_collation,
										  value,
										  key->sk_argument));
}

static bool
fastpg_mem_index_slot_matches_scan(Relation indexRelation,
								   Relation heapRelation,
								   TupleTableSlot *slot,
								   int nkeys,
								   ScanKey keys)
{
	for (int index = 0; index < nkeys; index++)
	{
		if (!fastpg_mem_index_key_matches_slot(indexRelation,
											   heapRelation,
											   slot,
											   &keys[index]))
			return false;
	}

	return true;
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
	opaque->full_scan = false;
	opaque->counted_scan = false;
	fastpg_mem_index_clear_array_keys(opaque);
	fastpg_mem_index_clear_scan_keys(opaque);
	fastpg_mem_index_clear_matches(opaque);
	memset(opaque->values, 0, sizeof(opaque->values));
	memset(opaque->isnull, 1, sizeof(opaque->isnull));
	memset(opaque->key_seen, 0, sizeof(opaque->key_seen));

	if (norderbys != 0)
		fastpg_mem_index_unsupported("ordered rescans");
	if (nkeys > 0 && keys == NULL)
		fastpg_mem_index_unsupported("rescans without scan keys");

	if (nkeys != (int) opaque->nkeys)
		opaque->full_scan = true;
	if (nkeys == 0)
		opaque->full_scan = true;
	if (scan->indexRelation->rd_index != NULL &&
		!scan->indexRelation->rd_index->indisunique)
		opaque->full_scan = true;
	if (scan->xs_snapshot != NULL &&
		scan->xs_snapshot->snapshot_type == SNAPSHOT_DIRTY)
		opaque->full_scan = true;

	for (int index = 0; index < nkeys; index++)
	{
		ScanKey		key = &keys[index];
		int			key_index = key->sk_attno - 1;
		bool		search_array = (key->sk_flags & SK_SEARCHARRAY) != 0;
		bool		search_null = (key->sk_flags & SK_SEARCHNULL) != 0;

		if (key->sk_flags & (SK_ORDER_BY | SK_ROW_HEADER | SK_ROW_MEMBER))
			fastpg_mem_index_unsupported("non-scalar scan keys");
		if ((key->sk_flags & SK_SEARCHNOTNULL) ||
			(!search_null && key->sk_strategy != BTEqualStrategyNumber))
			opaque->full_scan = true;
		if (search_null)
			opaque->full_scan = true;
		if (search_array &&
			(nkeys != 1 || search_null || (key->sk_flags & SK_ISNULL)))
			opaque->full_scan = true;
		if (key_index < 0 || key_index >= (int) opaque->nkeys)
			fastpg_mem_index_unsupported("scan keys outside the primary-key prefix");
		if (opaque->full_scan)
			continue;
		if (search_array)
		{
			ArrayType  *array;
			Oid			elemtype;
			int16		typlen;
			bool		typbyval;
			char		typalign;

			if (nkeys != 1 || search_null || (key->sk_flags & SK_ISNULL))
				fastpg_mem_index_unsupported("compound scalar-array probes");
			array = DatumGetArrayTypeP(key->sk_argument);
			elemtype = ARR_ELEMTYPE(array);
			get_typlenbyvalalign(elemtype, &typlen, &typbyval, &typalign);
			deconstruct_array(array,
							  elemtype,
							  typlen,
							  typbyval,
							  typalign,
							  &opaque->array_values,
							  &opaque->array_isnull,
							  &opaque->array_nelems);
			opaque->array_index = 0;
			opaque->array_key_index = key_index;
			opaque->isnull[key_index] = 1;
			opaque->key_seen[key_index] = 1;
			continue;
		}
		if (search_null)
		{
			opaque->values[key_index] = 0;
			opaque->isnull[key_index] = 1;
			opaque->key_seen[key_index] = 1;
			continue;
		}

		opaque->values[key_index] = (uintptr_t) key->sk_argument;
		opaque->isnull[key_index] =
			(key->sk_flags & SK_ISNULL) ? 1 : 0;
		opaque->key_seen[key_index] = 1;
	}

	for (size_t index = 0; index < opaque->nkeys; index++)
	{
		if (opaque->key_seen[index] == 0)
		{
			opaque->full_scan = true;
			break;
		}
	}

	if (opaque->full_scan)
	{
		fastpg_mem_index_clear_array_keys(opaque);
		memset(opaque->values, 0, sizeof(opaque->values));
		memset(opaque->isnull, 1, sizeof(opaque->isnull));
		memset(opaque->key_seen, 0, sizeof(opaque->key_seen));
		if (nkeys > 0)
		{
			opaque->scan_keys = palloc_array(ScanKeyData, nkeys);
			memcpy(opaque->scan_keys, keys, nkeys * sizeof(ScanKeyData));
			opaque->scan_nkeys = nkeys;
		}
		if (opaque->scan_handle != 0)
		{
			if (opaque->scan_storage2)
				fastpg_storage2_scan_reset(opaque->scan_handle);
			else
				fastpg_rust_scan_reset(opaque->scan_handle);
		}
	}

	if (fastpg_catalog_mode_uses_postgres() && !opaque->counted_scan)
	{
		pgstat_count_index_scan(scan->indexRelation);
		opaque->counted_scan = true;
	}
}

static bool
fastpg_mem_index_full_scan_match_next(IndexScanDesc scan,
									  FastPgMemIndexScan *opaque,
									  uint64_t *row_id_out)
{
	Relation	heapRelation = scan->heapRelation;
	int			natts = RelationGetDescr(heapRelation)->natts;
	uintptr_t	stack_values[FASTPG_MEM_STACK_NATTS];
	uint8_t		stack_isnull[FASTPG_MEM_STACK_NATTS];
	uintptr_t  *values;
	uint8_t    *isnull;
	bool		heap_buffers = natts > FASTPG_MEM_STACK_NATTS;
	uint64_t	row_id = 0;
	size_t		stored_natts = 0;
	uint32_t	row_xmin = 0;
	uint32_t	row_cmin = 0;
	bool		found;
	bool		use_mvcc_snapshot;

	if (heapRelation == NULL)
		fastpg_mem_index_unsupported("heapless index scans");
	use_mvcc_snapshot =
		fastpg_catalog_mode_uses_postgres() &&
		scan->xs_snapshot != NULL &&
		scan->xs_snapshot->snapshot_type == SNAPSHOT_MVCC;

	if (opaque->scan_handle == 0)
	{
		opaque->scan_storage2 =
			fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(heapRelation));
		opaque->scan_handle = opaque->scan_storage2 ?
			(use_mvcc_snapshot ?
			 fastpg_storage2_scan_begin_with_snapshot(RelationGetRelid(heapRelation),
													  fastpg_mem_effective_snapshot_curcid(scan->xs_snapshot)) :
			 fastpg_storage2_scan_begin(RelationGetRelid(heapRelation))) :
			(use_mvcc_snapshot ?
			 fastpg_rust_scan_begin_with_snapshot(RelationGetRelid(heapRelation),
												  1,
												  GetCurrentTransactionIdIfAny(),
												  scan->xs_snapshot->curcid) :
			 fastpg_rust_scan_begin(RelationGetRelid(heapRelation)));
		if (opaque->scan_handle == 0)
			fastpg_mem_raise_storage_error("fastpg_mem failed to create Rust scan handle");
	}
	if (opaque->scan_slot == NULL)
		opaque->scan_slot =
			MakeSingleTupleTableSlot(RelationGetDescr(heapRelation),
									 fastpg_mem_slot_callbacks(heapRelation));

	values = heap_buffers ? palloc0_array(uintptr_t, natts) : stack_values;
	isnull = heap_buffers ? palloc0_array(uint8_t, natts) : stack_isnull;

		while ((found = opaque->scan_storage2 ?
				fastpg_storage2_scan_next_with_stored_natts(opaque->scan_handle,
															1,
															values,
															isnull,
															natts,
															&row_id,
															&stored_natts) :
				(use_mvcc_snapshot ?
				 fastpg_rust_scan_next_with_metadata(opaque->scan_handle,
													 1,
												 values,
												 isnull,
												 natts,
												 &row_id,
												 &stored_natts,
												 &row_xmin,
												 &row_cmin) :
			 fastpg_rust_scan_next_with_stored_natts(opaque->scan_handle,
													 1,
													 values,
													 isnull,
													 natts,
													 &row_id,
													 &stored_natts))))
	{
			ExecClearTuple(opaque->scan_slot);
			if (opaque->scan_storage2)
			{
				fastpg_mem_fill_virtual_tuple_attrs(heapRelation,
													opaque->scan_slot,
													values,
													isnull,
													stored_natts);
				if (!fastpg_mem_storage2_tid_to_tid(row_id, &opaque->scan_slot->tts_tid))
					elog(ERROR, "fastpg_mem storage2 TID %llu cannot be represented as a CTID",
						 (unsigned long long) row_id);
			opaque->scan_slot->tts_tableOid = RelationGetRelid(heapRelation);
			ExecStoreVirtualTuple(opaque->scan_slot);
		}
		else
			fastpg_mem_store_virtual_tuple(heapRelation,
										   opaque->scan_slot,
										   values,
										   isnull,
										   stored_natts,
										   row_id);
		if (use_mvcc_snapshot &&
			!fastpg_mem_row_metadata_visible_to_snapshot((TransactionId) row_xmin,
														 (CommandId) row_cmin,
														 scan->xs_snapshot))
		{
			ExecClearTuple(opaque->scan_slot);
			continue;
		}

		if (fastpg_mem_index_slot_matches_scan(scan->indexRelation,
											   heapRelation,
											   opaque->scan_slot,
											   opaque->scan_nkeys,
											   opaque->scan_keys))
		{
			*row_id_out = row_id;
			break;
		}
	}

	if (heap_buffers)
	{
		pfree(values);
		pfree(isnull);
	}

	if (!found)
		opaque->done = true;
	return found;
}

static int
fastpg_mem_index_match_cmp(const void *left, const void *right, void *arg)
{
	const FastPgMemIndexMatch *left_match = (const FastPgMemIndexMatch *) left;
	const FastPgMemIndexMatch *right_match = (const FastPgMemIndexMatch *) right;
	FastPgMemIndexSortContext *context = (FastPgMemIndexSortContext *) arg;
	Relation	indexRelation = context->index_relation;
	int			key_count = IndexRelationGetNumberOfKeyAttributes(indexRelation);

	for (int index = 0; index < key_count; index++)
	{
		bool		left_isnull = left_match->isnull[index];
		bool		right_isnull = right_match->isnull[index];
		bool		nulls_first =
			(indexRelation->rd_indoption[index] & INDOPTION_NULLS_FIRST) != 0;
		bool		desc =
			(indexRelation->rd_indoption[index] & INDOPTION_DESC) != 0;
		int32		cmp;

		if (left_isnull || right_isnull)
		{
			if (left_isnull && right_isnull)
				continue;
			return left_isnull ?
				(nulls_first ? -1 : 1) :
				(nulls_first ? 1 : -1);
		}

		cmp = DatumGetInt32(FunctionCall2Coll(context->order_procs[index],
											  indexRelation->rd_indcollation[index],
											  left_match->values[index],
											  right_match->values[index]));
		if (cmp != 0)
			return desc ? -cmp : cmp;
	}

	if (left_match->row_id < right_match->row_id)
		return -1;
	if (left_match->row_id > right_match->row_id)
		return 1;
	return 0;
}

static void
fastpg_mem_index_sort_matches(IndexScanDesc scan, FastPgMemIndexScan *opaque)
{
	FastPgMemIndexSortContext context;
	int			key_count = IndexRelationGetNumberOfKeyAttributes(scan->indexRelation);

	if (opaque->matched_count <= 1)
		return;

	memset(&context, 0, sizeof(context));
	context.index_relation = scan->indexRelation;
	for (int index = 0; index < key_count; index++)
		context.order_procs[index] =
			index_getprocinfo(scan->indexRelation, index + 1, BTORDER_PROC);

	qsort_arg(opaque->matched_rows,
			  opaque->matched_count,
			  sizeof(FastPgMemIndexMatch),
			  fastpg_mem_index_match_cmp,
			  &context);
}

static void
fastpg_mem_index_remember_match(IndexScanDesc scan,
								FastPgMemIndexScan *opaque,
								uint64_t row_id)
{
	Relation	indexRelation = scan->indexRelation;
	Relation	heapRelation = scan->heapRelation;
	TupleDesc	heapDesc = RelationGetDescr(heapRelation);
	int			key_count = IndexRelationGetNumberOfKeyAttributes(indexRelation);
	FastPgMemIndexMatch *match;

	if (opaque->matched_count >= opaque->matched_capacity)
	{
		int			new_capacity = opaque->matched_capacity == 0 ?
			64 : opaque->matched_capacity * 2;

		opaque->matched_rows = opaque->matched_rows == NULL ?
			palloc_array(FastPgMemIndexMatch, new_capacity) :
			repalloc_array(opaque->matched_rows, FastPgMemIndexMatch, new_capacity);
		opaque->matched_capacity = new_capacity;
	}

	match = &opaque->matched_rows[opaque->matched_count++];
	memset(match, 0, sizeof(*match));
	match->row_id = row_id;

	for (int index = 0; index < key_count; index++)
	{
		AttrNumber	heap_attnum = indexRelation->rd_index->indkey.values[index];
		Form_pg_attribute attr;
		bool		isnull;
		Datum		value;

		if (heap_attnum <= 0 || heap_attnum > heapDesc->natts)
			fastpg_mem_index_unsupported("indexes with unsupported key metadata");
		attr = TupleDescAttr(heapDesc, heap_attnum - 1);
		value = slot_getattr(opaque->scan_slot, heap_attnum, &isnull);
		match->isnull[index] = isnull;
		if (!isnull && !attr->attbyval)
		{
			if (attr->attlen == -1)
			{
				Pointer		raw = DatumGetPointer(value);
				Size		len = VARSIZE_ANY(raw);
				Pointer		copy = palloc(len);

				memcpy(copy, raw, len);
				match->values[index] = PointerGetDatum(copy);
			}
			else
				match->values[index] = datumCopy(value, false, attr->attlen);
			match->owned[index] = true;
		}
		else
			match->values[index] = value;
	}
}

static void
fastpg_mem_index_collect_matches(IndexScanDesc scan, FastPgMemIndexScan *opaque)
{
	uint64_t	row_id;

	if (opaque->matched_ready)
		return;

	while (fastpg_mem_index_full_scan_match_next(scan, opaque, &row_id))
		fastpg_mem_index_remember_match(scan, opaque, row_id);

	fastpg_mem_index_sort_matches(scan, opaque);

	opaque->matched_index = 0;
	opaque->matched_ready = true;
	opaque->done = false;
}

static bool
fastpg_mem_index_return_row_id(IndexScanDesc scan,
							   FastPgMemIndexScan *opaque,
							   uint64_t row_id)
{
	if (!opaque->scan_storage2 &&
		fastpg_catalog_mode_uses_postgres() &&
		scan->xs_snapshot != NULL &&
		scan->xs_snapshot->snapshot_type == SNAPSHOT_DIRTY)
		row_id =
			fastpg_mem_reverse_row_redirect((uint32_t) RelationGetRelid(scan->heapRelation),
											row_id);

	if (opaque->scan_storage2)
	{
		if (!fastpg_mem_storage2_tid_to_tid(row_id, &scan->xs_heaptid))
			elog(ERROR, "fastpg_mem storage2 TID %llu cannot be represented as a CTID",
				 (unsigned long long) row_id);
	}
	else if (!fastpg_mem_row_id_to_tid(scan->heapRelation,
									   row_id,
									   &scan->xs_heaptid))
		elog(ERROR, "fastpg_mem row id %llu cannot be represented as a CTID",
			 (unsigned long long) row_id);
	scan->xs_recheck = false;
	scan->xs_recheckorderby = false;
	if (fastpg_catalog_mode_uses_postgres())
		pgstat_count_index_tuples(scan->indexRelation, 1);
	return true;
}

static bool
fastpg_mem_index_get_tuple_full_scan(IndexScanDesc scan,
									 FastPgMemIndexScan *opaque,
									 ScanDirection direction)
{
	uint64_t	row_id = 0;

	if (ScanDirectionIsBackward(direction))
	{
		fastpg_mem_index_collect_matches(scan, opaque);
		if (opaque->matched_index == 0)
			opaque->matched_index = opaque->matched_count;
		if (opaque->matched_index <= 0)
		{
			opaque->done = true;
			return false;
		}
		row_id = opaque->matched_rows[--opaque->matched_index].row_id;
		if (opaque->matched_index == 0)
			opaque->done = true;
		return fastpg_mem_index_return_row_id(scan, opaque, row_id);
	}

	fastpg_mem_index_collect_matches(scan, opaque);
	if (opaque->matched_index >= opaque->matched_count)
	{
		opaque->done = true;
		return false;
	}
	row_id = opaque->matched_rows[opaque->matched_index++].row_id;
	return fastpg_mem_index_return_row_id(scan, opaque, row_id);
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
		fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(scan->heapRelation));

	if (ScanDirectionIsBackward(direction))
	{
		if (!opaque->full_scan && opaque->array_values != NULL)
			fastpg_mem_index_unsupported("backward scalar-array scans");
	}
	if (opaque->unsupported || opaque->done)
		return false;
	if (opaque->full_scan)
		return fastpg_mem_index_get_tuple_full_scan(scan, opaque, direction);

	if (fastpg_catalog_mode_uses_postgres() &&
		!fastpg_mem_index_spec(scan->indexRelation,
							   scan->heapRelation,
							   (int) opaque->nkeys,
							   fastpg_attnums,
							   fastpg_typbyval,
							   fastpg_typlen))
		fastpg_mem_index_unsupported("indexes with unsupported key metadata");

	for (;;)
	{
		bool		found = false;

		if (opaque->array_values != NULL)
		{
			if (opaque->array_index >= opaque->array_nelems)
			{
				opaque->done = true;
				return false;
			}
			opaque->values[opaque->array_key_index] =
				(uintptr_t) opaque->array_values[opaque->array_index];
			opaque->isnull[opaque->array_key_index] =
				opaque->array_isnull[opaque->array_index] ? 1 : 0;
			opaque->array_index++;
		}
		else
			opaque->done = true;

		if (storage2 &&
			fastpg_catalog_mode_uses_postgres() &&
			opaque->nkeys == 1 &&
			fastpg_typbyval[0] != 0)
			found =
				fastpg_mem_cached_single_byval_index_lookup((uint32_t) RelationGetRelid(scan->heapRelation),
															(AttrNumber) fastpg_attnums[0],
															opaque->values[0],
															opaque->isnull[0],
															&row_id);
		if (!found &&
			storage2 &&
			fastpg_catalog_mode_uses_postgres() &&
			opaque->nkeys == 1 &&
			fastpg_typbyval[0] != 0)
			found =
				fastpg_storage2_primary_key_index_lookup_single_byval_with_spec((uint32_t) RelationGetRelid(scan->indexRelation),
																				(uint32_t) RelationGetRelid(scan->heapRelation),
																				opaque->values[0],
																				opaque->isnull[0],
																				&row_id);
		if (!found)
			found = storage2 ?
				(fastpg_catalog_mode_uses_postgres() ?
				 fastpg_storage2_primary_key_index_lookup_with_spec((uint32_t) RelationGetRelid(scan->indexRelation),
																	(uint32_t) RelationGetRelid(scan->heapRelation),
																	fastpg_attnums,
																	fastpg_typbyval,
																	fastpg_typlen,
																	opaque->values,
																	opaque->isnull,
																	opaque->nkeys,
																	&row_id) :
				 fastpg_storage2_primary_key_index_lookup((uint32_t) RelationGetRelid(scan->indexRelation),
														  opaque->values,
														  opaque->isnull,
														  opaque->nkeys,
														  &row_id)) :
				(fastpg_catalog_mode_uses_postgres() ?
				 fastpg_rust_primary_key_index_lookup_with_spec((uint32_t) RelationGetRelid(scan->indexRelation),
																(uint32_t) RelationGetRelid(scan->heapRelation),
																fastpg_attnums,
																fastpg_typbyval,
																fastpg_typlen,
																opaque->values,
																opaque->isnull,
																opaque->nkeys,
																&row_id) :
				 fastpg_rust_primary_key_index_lookup((uint32_t) RelationGetRelid(scan->indexRelation),
													  opaque->values,
													  opaque->isnull,
													  opaque->nkeys,
													  &row_id));
		if (found &&
			storage2 &&
			fastpg_catalog_mode_uses_postgres() &&
			opaque->nkeys == 1 &&
			fastpg_typbyval[0] != 0)
			fastpg_mem_remember_single_byval_index_key((uint32_t) RelationGetRelid(scan->heapRelation),
													   row_id,
													   (AttrNumber) fastpg_attnums[0],
													   opaque->values[0],
													   opaque->isnull[0]);
		if (found)
			break;

		if (opaque->array_values == NULL)
			return false;
	}

	if (storage2)
	{
		if (!fastpg_mem_storage2_tid_to_tid(row_id, &scan->xs_heaptid))
			elog(ERROR, "fastpg_mem storage2 TID %llu cannot be represented as a CTID",
				 (unsigned long long) row_id);
	}
	else if (!fastpg_mem_row_id_to_tid(scan->heapRelation,
									   row_id,
									   &scan->xs_heaptid))
		elog(ERROR, "fastpg_mem row id %llu cannot be represented as a CTID",
			 (unsigned long long) row_id);
	scan->xs_recheck = false;
	scan->xs_recheckorderby = false;
	if (fastpg_catalog_mode_uses_postgres())
		pgstat_count_index_tuples(scan->indexRelation, 1);
	return true;
}

static void
fastpg_mem_index_end_scan(IndexScanDesc scan)
{
	if (scan->opaque != NULL)
	{
		fastpg_mem_index_clear_array_keys((FastPgMemIndexScan *) scan->opaque);
		fastpg_mem_index_clear_scan_keys((FastPgMemIndexScan *) scan->opaque);
		fastpg_mem_index_release_scan((FastPgMemIndexScan *) scan->opaque);
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
	uintptr_t	stack_values[FASTPG_MEM_STACK_NATTS];
	uint8_t		stack_isnull[FASTPG_MEM_STACK_NATTS];
	uint8_t		stack_byval[FASTPG_MEM_STACK_NATTS];
	size_t		stack_value_lens[FASTPG_MEM_STACK_NATTS];
	uint8_t		stack_owned[FASTPG_MEM_STACK_NATTS];
	uintptr_t  *values = stack_values;
	uint8_t    *isnull = stack_isnull;
	uint8_t    *byval = stack_byval;
	size_t	   *value_lens = stack_value_lens;
	uint8_t    *owned = stack_owned;
	uint64_t	row_id = 0;
	bool		storage2 = fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(rel));
	bool		heap_buffers = tupdesc->natts > FASTPG_MEM_STACK_NATTS;
	bool		used_toasted_tuple = false;
	bool		should_free_heap_tuple = false;
	HeapTuple	heap_tuple = NULL;
	HeapTuple	toasted_tuple = NULL;

	fastpg_mem_ensure_write_xact();
	if (heap_buffers)
	{
		values = palloc_array(uintptr_t, tupdesc->natts);
		isnull = palloc_array(uint8_t, tupdesc->natts);
		byval = palloc_array(uint8_t, tupdesc->natts);
		value_lens = palloc_array(size_t, tupdesc->natts);
		owned = palloc_array(uint8_t, tupdesc->natts);
	}
	if (fastpg_mem_slot_needs_heap_tuple(rel, slot))
	{
		heap_tuple = ExecFetchSlotHeapTuple(slot, true, &should_free_heap_tuple);
		fastpg_mem_prepare_heap_tuple_header(rel, heap_tuple, cid, options);
		if (HeapTupleHasExternal(heap_tuple) ||
			heap_tuple->t_len > TOAST_TUPLE_THRESHOLD)
		{
			toasted_tuple =
				heap_toast_insert_or_update(rel, heap_tuple, NULL, options);
			fastpg_mem_fill_heap_tuple_values(rel,
											  toasted_tuple,
											  values,
											  isnull,
											  byval,
											  value_lens);
			memset(owned, 0, sizeof(uint8_t) * tupdesc->natts);
			used_toasted_tuple = true;
		}
		else
		{
			fastpg_mem_fill_heap_tuple_values(rel,
											  heap_tuple,
											  values,
											  isnull,
											  byval,
											  value_lens);
			memset(owned, 0, sizeof(uint8_t) * tupdesc->natts);
			used_toasted_tuple = true;
		}
		if ((toasted_tuple != NULL && HeapTupleHasExternal(toasted_tuple)) ||
			(toasted_tuple == NULL && HeapTupleHasExternal(heap_tuple)))
			fastpg_mem_note_relation_external_toast((uint32_t) RelationGetRelid(rel));
	}
	if (used_toasted_tuple)
	{
		/* values already filled from the toasted heap tuple */
	}
	else if (storage2)
		fastpg_mem_fill_slot_values_borrowed(rel,
											 slot,
											 values,
											 isnull,
											 byval,
											 value_lens,
											 owned);
	else
		fastpg_mem_fill_slot_values_borrowed(rel,
											 slot,
											 values,
											 isnull,
											 byval,
											 value_lens,
											 owned);
	fastpg_mem_ensure_block_layout_for_slot(rel, slot);
	if (!(storage2 ?
		  (fastpg_catalog_mode_uses_postgres() ?
		   (rel->rd_rel != NULL && !rel->rd_rel->relhasindex ?
			fastpg_storage2_relation_insert_unchecked_no_index_with_metadata(RelationGetRelid(rel),
																			(uint32_t) GetCurrentTransactionId(),
																			(uint32_t) cid,
																			values,
																			isnull,
																			byval,
																			value_lens,
																			tupdesc->natts,
																			&row_id) :
			fastpg_storage2_relation_insert_unchecked_with_metadata(RelationGetRelid(rel),
																   (uint32_t) GetCurrentTransactionId(),
																   (uint32_t) cid,
																   values,
																   isnull,
																   byval,
																   value_lens,
																   tupdesc->natts,
																   &row_id)) :
		   fastpg_storage2_relation_insert_unchecked(RelationGetRelid(rel),
													 values,
													 isnull,
													 byval,
													 value_lens,
													 tupdesc->natts,
													 &row_id)) :
		  fastpg_rust_relation_insert_unchecked(RelationGetRelid(rel),
												values,
												isnull,
												byval,
												value_lens,
												tupdesc->natts,
												&row_id)))
	{
		if (!used_toasted_tuple)
			fastpg_mem_free_owned_slot_value_payloads(rel, values, isnull, owned);
		if (toasted_tuple != NULL && toasted_tuple != heap_tuple)
			heap_freetuple(toasted_tuple);
		if (heap_tuple != NULL && should_free_heap_tuple)
			heap_freetuple(heap_tuple);
		if (heap_buffers)
		{
			pfree(values);
			pfree(isnull);
			pfree(byval);
			pfree(value_lens);
			pfree(owned);
		}
		fastpg_mem_raise_storage_error("fastpg_mem failed to insert row into Rust storage");
	}

	if (storage2)
	{
		if (!fastpg_mem_storage2_tid_to_tid(row_id, &slot->tts_tid))
			elog(ERROR, "fastpg_mem storage2 TID %llu cannot be represented as a CTID",
				 (unsigned long long) row_id);
	}
	else if (!fastpg_mem_row_id_to_tid(rel, row_id, &slot->tts_tid))
		elog(ERROR, "fastpg_mem row id %llu cannot be represented as a CTID",
			 (unsigned long long) row_id);
	slot->tts_tableOid = RelationGetRelid(rel);
	if (fastpg_catalog_mode_uses_postgres() &&
		heap_tuple != NULL &&
		IsCatalogRelation(rel) &&
		!IsToastRelation(rel))
	{
		heap_tuple->t_self = slot->tts_tid;
		heap_tuple->t_tableOid = RelationGetRelid(rel);
		fastpg_mem_cache_invalidate_heap_tuple(rel, heap_tuple, NULL);
	}
	if (!storage2)
	{
		(void) fastpg_rust_relation_set_row_xmin((uint32_t) RelationGetRelid(rel),
												 row_id,
												 GetCurrentTransactionId(),
												 cid);
	}
	if (fastpg_catalog_mode_uses_postgres())
	{
		pgstat_count_heap_insert(rel, 1);
		fastpg_mem_count_io_op(rel, IOCONTEXT_NORMAL, IOOP_EXTEND, 1);
		fastpg_mem_count_io_op(rel, IOCONTEXT_NORMAL, IOOP_WRITE, 1);
		fastpg_mem_count_io_op(rel, IOCONTEXT_NORMAL, IOOP_FSYNC, 1);
		fastpg_mem_count_io_op(rel, IOCONTEXT_BULKWRITE, IOOP_EXTEND, 1);
	}
	fastpg_mem_mark_row_touched((uint32_t) RelationGetRelid(rel), row_id, cid);
	fastpg_mem_note_relation_changed((uint32_t) RelationGetRelid(rel));
	if (!used_toasted_tuple)
		fastpg_mem_free_owned_slot_value_payloads(rel, values, isnull, owned);
	if (toasted_tuple != NULL && toasted_tuple != heap_tuple)
		heap_freetuple(toasted_tuple);
	if (heap_tuple != NULL && should_free_heap_tuple)
		heap_freetuple(heap_tuple);
	if (heap_buffers)
	{
		pfree(values);
		pfree(isnull);
		pfree(byval);
		pfree(value_lens);
		pfree(owned);
	}
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
			if (!fastpg_mem_tid_to_row_id(rel, &slot->tts_tid, &row_id))
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
	TupleDesc	tupdesc = RelationGetDescr(rel);
	Size		total_values;
	uintptr_t  *values;
	uint8_t    *isnull;
	uint8_t    *byval;
	size_t	   *value_lens;
	uint8_t    *owned;
	uint64_t   *row_ids;
	size_t		inserted;
	bool		storage2 =
		fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(rel));

	if (storage2 || nslots <= 1 || fastpg_catalog_mode_uses_postgres())
	{
		for (int index = 0; index < nslots; index++)
			fastpg_mem_tuple_insert(rel, slots[index], cid, options, bistate);
		return;
	}

	total_values = (Size) tupdesc->natts * (Size) nslots;
	values = palloc0_array(uintptr_t, total_values);
	isnull = palloc0_array(uint8_t, total_values);
	byval = palloc0_array(uint8_t, total_values);
	value_lens = palloc0_array(size_t, total_values);
	owned = palloc0_array(uint8_t, total_values);
	row_ids = palloc_array(uint64_t, nslots);

	for (int index = 0; index < nslots; index++)
	{
		Size		offset = (Size) index * tupdesc->natts;

		fastpg_mem_fill_slot_values_borrowed(rel,
											 slots[index],
											 values + offset,
											 isnull + offset,
											 byval + offset,
											 value_lens + offset,
											 owned + offset);
	}
	if (nslots > 0)
		fastpg_mem_ensure_block_layout_for_slot(rel, slots[0]);

	fastpg_mem_ensure_write_xact();
	inserted =
		fastpg_rust_relation_multi_insert_unchecked(RelationGetRelid(rel),
													values,
													isnull,
													byval,
													value_lens,
													tupdesc->natts,
													nslots,
													row_ids);
	if (inserted != (size_t) nslots)
	{
		for (int index = 0; index < nslots; index++)
		{
			Size		offset = (Size) index * tupdesc->natts;

			fastpg_mem_free_owned_slot_value_payloads(rel,
													  values + offset,
													  isnull + offset,
													  owned + offset);
		}
		pfree(values);
		pfree(isnull);
		pfree(byval);
		pfree(value_lens);
		pfree(owned);
		pfree(row_ids);
		fastpg_mem_raise_storage_error("fastpg_mem failed to insert row batch into Rust storage");
	}

	for (int index = 0; index < nslots; index++)
	{
		if (!fastpg_mem_row_id_to_tid(rel,
									  row_ids[index],
									  &slots[index]->tts_tid))
			elog(ERROR, "fastpg_mem row id %llu cannot be represented as a CTID",
				 (unsigned long long) row_ids[index]);
		slots[index]->tts_tableOid = RelationGetRelid(rel);
		(void) fastpg_rust_relation_set_row_xmin((uint32_t) RelationGetRelid(rel),
												 row_ids[index],
												 GetCurrentTransactionId(),
												 cid);
		fastpg_mem_mark_row_touched((uint32_t) RelationGetRelid(rel),
									row_ids[index],
									cid);
	}
	if (fastpg_catalog_mode_uses_postgres())
		pgstat_count_heap_insert(rel, nslots);
	fastpg_mem_note_relation_changed((uint32_t) RelationGetRelid(rel));

	for (int index = 0; index < nslots; index++)
	{
		Size		offset = (Size) index * tupdesc->natts;

		fastpg_mem_free_owned_slot_value_payloads(rel,
												  values + offset,
												  isnull + offset,
												  owned + offset);
	}
	pfree(values);
	pfree(isnull);
	pfree(byval);
	pfree(value_lens);
	pfree(owned);
	pfree(row_ids);
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
	TupleTableSlot *old_slot = NULL;
	HeapTuple	old_heap_tuple = NULL;
	bool		old_heap_tuple_should_free = false;
	bool		old_heap_tuple_has_external = false;
	bool		invalidate_catalog_tuple =
		fastpg_catalog_mode_uses_postgres() &&
		IsCatalogRelation(rel) &&
		!IsToastRelation(rel);

	if (storage2)
	{
		row_id = fastpg_mem_tid_to_storage2_tid(tid);
		if (row_id == 0)
		{
			fastpg_mem_fill_deleted_tmfd(tid, tmfd);
			return TM_Deleted;
		}
		if (fastpg_catalog_mode_uses_postgres())
		{
			uint64_t	resolved_row_id;
			CommandId	delete_cid;

			fastpg_mem_acquire_storage2_update_row_lock((uint32_t) RelationGetRelid(rel),
														&row_id);
			if (fastpg_mem_row_deleted_by_current_xact((uint32_t) RelationGetRelid(rel),
													   row_id,
													   cid,
													   true,
													   &delete_cid))
			{
				fastpg_mem_fill_self_modified_tmfd(tid, delete_cid, tmfd);
				return TM_SelfModified;
			}
			resolved_row_id =
				fastpg_mem_storage2_resolve_update_row_id((uint32_t) RelationGetRelid(rel),
														  row_id);
			if (resolved_row_id != row_id)
				row_id = resolved_row_id;
		}
	}
	else if (!fastpg_mem_tid_to_row_id(rel, tid, &row_id))
	{
		fastpg_mem_fill_deleted_tmfd(tid, tmfd);
		return TM_Deleted;
	}
	else if (fastpg_catalog_mode_uses_postgres())
	{
		CommandId	delete_cid;

		if (fastpg_mem_row_deleted_by_current_xact((uint32_t) RelationGetRelid(rel),
												   row_id,
												   cid,
												   false,
												   &delete_cid))
		{
			fastpg_mem_fill_self_modified_tmfd(tid, delete_cid, tmfd);
			return TM_SelfModified;
		}
		row_id = fastpg_mem_resolve_row_redirect((uint32_t) RelationGetRelid(rel),
												 row_id);
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

	if (fastpg_catalog_mode_uses_postgres() &&
		fastpg_mem_relation_can_toast(rel) &&
		(rel->rd_rel->reltoastrelid != InvalidOid ||
		 invalidate_catalog_tuple))
	{
		ItemPointerData resolved_tid;

		if (storage2)
		{
			if (!fastpg_mem_storage2_tid_to_tid(row_id, &resolved_tid))
				elog(ERROR, "fastpg_mem storage2 TID %llu cannot be represented as a CTID",
					 (unsigned long long) row_id);
		}
		else if (!fastpg_mem_row_id_to_tid(rel, row_id, &resolved_tid))
			elog(ERROR, "fastpg_mem row id %llu cannot be represented as a CTID",
				 (unsigned long long) row_id);
		old_slot = MakeSingleTupleTableSlot(RelationGetDescr(rel),
											fastpg_mem_slot_callbacks(rel));
		if (fastpg_mem_tuple_fetch_row_version(rel,
											   &resolved_tid,
											   SnapshotAny,
											   old_slot))
		{
			old_heap_tuple = ExecFetchSlotHeapTuple(old_slot,
												   true,
												   &old_heap_tuple_should_free);
			old_heap_tuple_has_external = HeapTupleHasExternal(old_heap_tuple);
		}
	}

	fastpg_mem_ensure_write_xact();
	if (!(storage2 ?
		  fastpg_storage2_relation_delete(RelationGetRelid(rel), row_id) :
		  (fastpg_catalog_mode_uses_postgres() ?
		   fastpg_rust_relation_delete_with_metadata(RelationGetRelid(rel),
													 row_id,
													 GetCurrentTransactionId(),
													 fastpg_mem_delete_cid_for_snapshot(cid,
																					  snapshot)) :
		   fastpg_rust_relation_delete(RelationGetRelid(rel), row_id))))
	{
		if (old_heap_tuple != NULL && old_heap_tuple_should_free)
			heap_freetuple(old_heap_tuple);
		if (old_slot != NULL)
			ExecDropSingleTupleTableSlot(old_slot);
		fastpg_mem_fill_deleted_tmfd(tid, tmfd);
		return TM_Deleted;
	}
	if (storage2 && fastpg_catalog_mode_uses_postgres())
		(void) fastpg_storage2_relation_record_invalidate_metadata(RelationGetRelid(rel),
																   row_id,
																   GetCurrentTransactionId(),
																   fastpg_mem_delete_cid_for_snapshot(cid,
																									 snapshot));

	if (invalidate_catalog_tuple && old_heap_tuple != NULL)
		fastpg_mem_cache_invalidate_heap_tuple(rel, old_heap_tuple, NULL);
	if (old_heap_tuple_has_external)
		heap_toast_delete(rel, old_heap_tuple, false);
	if (old_heap_tuple != NULL && old_heap_tuple_should_free)
		heap_freetuple(old_heap_tuple);
	if (old_slot != NULL)
		ExecDropSingleTupleTableSlot(old_slot);

	fastpg_mem_mark_row_touched((uint32_t) RelationGetRelid(rel), row_id, cid);
	if (fastpg_catalog_mode_uses_postgres())
	{
		pgstat_count_heap_delete(rel);
		fastpg_mem_count_io_op(rel, IOCONTEXT_NORMAL, IOOP_WRITE, 1);
	}
	fastpg_mem_note_relation_changed((uint32_t) RelationGetRelid(rel));
	return TM_Ok;
}

static bool
fastpg_mem_relation_has_deferred_unique_index(Relation rel)
{
	List	   *index_oids;
	ListCell   *lc;
	bool		found = false;

	index_oids = RelationGetIndexList(rel);
	foreach(lc, index_oids)
	{
		Oid			index_oid = lfirst_oid(lc);
		Relation	index_rel = index_open(index_oid, AccessShareLock);

		if (index_rel->rd_index != NULL &&
			index_rel->rd_index->indisunique &&
			!index_rel->rd_index->indimmediate)
			found = true;

		index_close(index_rel, AccessShareLock);
		if (found)
			break;
	}
	list_free(index_oids);

	return found;
}

static bool
fastpg_mem_relation_has_brin_index(Relation rel)
{
	List	   *index_oids;
	ListCell   *lc;
	bool		found = false;

	index_oids = RelationGetIndexList(rel);
	foreach(lc, index_oids)
	{
		Oid			index_oid = lfirst_oid(lc);
		Relation	index_rel = index_open(index_oid, AccessShareLock);

		if (index_rel->rd_rel->relam == BRIN_AM_OID)
			found = true;

		index_close(index_rel, AccessShareLock);
		if (found)
			break;
	}
	list_free(index_oids);

	return found;
}

static bool
fastpg_mem_relation_has_unique_index(Relation rel)
{
	List	   *index_oids;
	ListCell   *lc;
	bool		found = false;

	index_oids = RelationGetIndexList(rel);
	foreach(lc, index_oids)
	{
		Oid			index_oid = lfirst_oid(lc);
		Relation	index_rel = index_open(index_oid, AccessShareLock);

		if (index_rel->rd_index != NULL &&
			index_rel->rd_index->indisunique)
			found = true;

		index_close(index_rel, AccessShareLock);
		if (found)
			break;
	}
	list_free(index_oids);

	return found;
}

static void
fastpg_mem_ensure_index_attr_bitmaps(Relation rel)
{
	Bitmapset  *attrs;

	if (rel->rd_attrsvalid)
		return;

	attrs = RelationGetIndexAttrBitmap(rel, INDEX_ATTR_BITMAP_HOT_BLOCKING);
	bms_free(attrs);
}

static bool
fastpg_mem_index_bitmap_has_non_user_attrs(Bitmapset *attrs, TupleDesc tupdesc)
{
	int			attidx = -1;

	while ((attidx = bms_next_member(attrs, attidx)) >= 0)
	{
		AttrNumber	attnum =
			attidx + FirstLowInvalidHeapAttributeNumber;

		if (attnum <= 0 || attnum > tupdesc->natts)
			return true;
	}

	return false;
}

static bool
fastpg_mem_update_index_attrs_empty(Relation rel)
{
	return bms_is_empty(rel->rd_hotblockingattr) &&
		bms_is_empty(rel->rd_summarizedattr) &&
		bms_is_empty(rel->rd_keyattr) &&
		bms_is_empty(rel->rd_idattr);
}

static bool
fastpg_mem_update_index_attr_member(Relation rel, AttrNumber attnum)
{
	int			attidx = attnum - FirstLowInvalidHeapAttributeNumber;

	return bms_is_member(attidx, rel->rd_hotblockingattr) ||
		bms_is_member(attidx, rel->rd_summarizedattr) ||
		bms_is_member(attidx, rel->rd_keyattr) ||
		bms_is_member(attidx, rel->rd_idattr);
}

static AttrNumber
fastpg_mem_update_index_max_attr_in_bitmap(Bitmapset *attrs)
{
	AttrNumber	max_attnum = 0;
	int			attidx = -1;

	while ((attidx = bms_next_member(attrs, attidx)) >= 0)
	{
		AttrNumber	attnum =
			attidx + FirstLowInvalidHeapAttributeNumber;

		if (attnum > max_attnum)
			max_attnum = attnum;
	}

	return max_attnum;
}

static AttrNumber
fastpg_mem_update_index_max_user_attr(Relation rel)
{
	AttrNumber	max_attnum;

	max_attnum =
		fastpg_mem_update_index_max_attr_in_bitmap(rel->rd_hotblockingattr);
	max_attnum = Max(max_attnum,
					 fastpg_mem_update_index_max_attr_in_bitmap(rel->rd_summarizedattr));
	max_attnum = Max(max_attnum,
					 fastpg_mem_update_index_max_attr_in_bitmap(rel->rd_keyattr));
	max_attnum = Max(max_attnum,
					 fastpg_mem_update_index_max_attr_in_bitmap(rel->rd_idattr));
	return max_attnum;
}

static bool
fastpg_mem_single_update_index_attr(Relation rel,
									TupleDesc tupdesc,
									AttrNumber *attnum_out)
{
	FastPgMemBlockLayout *entry;
	AttrNumber	found_attnum = 0;
	bool		result = false;

	entry =
		fastpg_mem_block_layout_entry((uint32_t) RelationGetRelid(rel), true);
	if (entry->single_update_index_attr_valid &&
		entry->single_update_index_tupdesc == tupdesc &&
		entry->single_update_hotblockingattr == rel->rd_hotblockingattr &&
		entry->single_update_summarizedattr == rel->rd_summarizedattr &&
		entry->single_update_keyattr == rel->rd_keyattr &&
		entry->single_update_idattr == rel->rd_idattr)
	{
		if (entry->single_update_index_attr_result)
			*attnum_out = entry->single_update_index_attr;
		return entry->single_update_index_attr_result;
	}

	if (fastpg_mem_index_bitmap_has_non_user_attrs(rel->rd_hotblockingattr, tupdesc) ||
		fastpg_mem_index_bitmap_has_non_user_attrs(rel->rd_summarizedattr, tupdesc) ||
		fastpg_mem_index_bitmap_has_non_user_attrs(rel->rd_keyattr, tupdesc) ||
		fastpg_mem_index_bitmap_has_non_user_attrs(rel->rd_idattr, tupdesc))
		goto done;

	for (AttrNumber attnum = 1; attnum <= tupdesc->natts; attnum++)
	{
		if (!fastpg_mem_update_index_attr_member(rel, attnum))
			continue;
		if (found_attnum != 0)
			goto done;
		found_attnum = attnum;
	}

	if (found_attnum == 0)
		goto done;
	result = true;

done:
	entry->single_update_index_tupdesc = tupdesc;
	entry->single_update_hotblockingattr = rel->rd_hotblockingattr;
	entry->single_update_summarizedattr = rel->rd_summarizedattr;
	entry->single_update_keyattr = rel->rd_keyattr;
	entry->single_update_idattr = rel->rd_idattr;
	entry->single_update_index_attr_valid = true;
	entry->single_update_index_attr_result = result;
	entry->single_update_index_attr = found_attnum;
	if (result)
		*attnum_out = found_attnum;
	return result;
}

static void
fastpg_mem_remember_single_byval_index_key(uint32_t relid,
										   uint64_t row_id,
										   AttrNumber attnum,
										   uintptr_t value,
										   uint8_t isnull)
{
	fastpg_mem_last_index_key.valid = true;
	fastpg_mem_last_index_key.relid = relid;
	fastpg_mem_last_index_key.row_id = row_id;
	fastpg_mem_last_index_key.xid = GetCurrentTransactionIdIfAny();
	fastpg_mem_last_index_key.attnum = attnum;
	fastpg_mem_last_index_key.value = value;
	fastpg_mem_last_index_key.isnull = isnull;
}

static bool
fastpg_mem_cached_single_byval_index_lookup(uint32_t relid,
											AttrNumber attnum,
											uintptr_t value,
											uint8_t isnull,
											uint64_t *row_id_out)
{
	TransactionId xid = GetCurrentTransactionIdIfAny();

	if (!fastpg_mem_last_index_key.valid ||
		!TransactionIdIsValid(xid) ||
		fastpg_mem_last_index_key.xid != xid ||
		fastpg_mem_last_index_key.relid != relid ||
		fastpg_mem_last_index_key.attnum != attnum ||
		fastpg_mem_last_index_key.isnull != isnull)
		return false;
	if (isnull == 0 && fastpg_mem_last_index_key.value != value)
		return false;

	if (fastpg_mem_use_storage2_for_relid(relid) &&
		!fastpg_storage2_relation_current_session_visible_tid(relid,
															  fastpg_mem_last_index_key.row_id,
															  0,
															  0,
															  NULL) &&
		!fastpg_storage2_relation_contains_tid(relid,
											   fastpg_mem_last_index_key.row_id))
	{
		fastpg_mem_last_index_key.valid = false;
		return false;
	}

	*row_id_out = fastpg_mem_last_index_key.row_id;
	return true;
}

static bool
fastpg_mem_cached_single_index_key_preserves(Relation rel,
											 uint64_t row_id,
											 TupleTableSlot *new_slot,
											 AttrNumber attnum,
											 bool *preserves_out)
{
	Form_pg_attribute attr;
	Datum		new_value;
	bool		new_isnull;
	bool		old_isnull;

	if (!fastpg_mem_last_index_key.valid ||
		fastpg_mem_last_index_key.relid != (uint32_t) RelationGetRelid(rel) ||
		fastpg_mem_last_index_key.row_id != row_id ||
		fastpg_mem_last_index_key.attnum != attnum)
		return false;

	old_isnull = fastpg_mem_last_index_key.isnull != 0;
	new_value = slot_getattr(new_slot, attnum, &new_isnull);
	if (old_isnull != new_isnull)
	{
		*preserves_out = false;
		return true;
	}
	if (old_isnull)
	{
		*preserves_out = true;
		return true;
	}

	attr = TupleDescAttr(RelationGetDescr(rel), attnum - 1);
	*preserves_out =
		fastpg_mem_datum_attr_equal((Datum) fastpg_mem_last_index_key.value,
									new_value,
									attr);
	return true;
}

static void
fastpg_mem_maybe_remember_scan_single_index_key(FastPgMemScanDesc *scan,
												uint64_t row_id,
												const uintptr_t *values,
												const uint8_t *isnull,
												size_t stored_natts)
{
	AttrNumber	attnum = scan->single_index_attnum;

	if (!scan->cache_single_index_key ||
		attnum <= 0 ||
		(size_t) attnum > stored_natts)
		return;

	fastpg_mem_remember_single_byval_index_key((uint32_t) RelationGetRelid(scan->base.rs_rd),
											   row_id,
											   attnum,
											   values[attnum - 1],
											   isnull[attnum - 1]);
}

static bool
fastpg_mem_storage2_update_preserves_index_attrs(Relation rel,
												 uint64_t row_id,
												 TupleTableSlot *new_slot,
												 AttrNumber max_index_attnum)
{
	TupleDesc	tupdesc = RelationGetDescr(rel);
	uintptr_t	stack_values[FASTPG_MEM_STACK_NATTS];
	uint8_t		stack_isnull[FASTPG_MEM_STACK_NATTS];
	uintptr_t  *values = stack_values;
	uint8_t    *isnull = stack_isnull;
	size_t		stored_natts = 0;
	bool		preserves = true;
	bool		current_session_fetched = false;
	bool		heap_buffers = max_index_attnum > FASTPG_MEM_STACK_NATTS;

	if (heap_buffers)
	{
		values = palloc0_array(uintptr_t, max_index_attnum);
		isnull = palloc0_array(uint8_t, max_index_attnum);
	}

	current_session_fetched =
		fastpg_storage2_fetch_current_session_tid_with_stored_natts(RelationGetRelid(rel),
																	row_id,
																	0,
																	0,
																	values,
																	isnull,
																	max_index_attnum,
																	&stored_natts,
																	NULL);
	if (!current_session_fetched &&
		!fastpg_storage2_fetch_tid_any_with_stored_natts(RelationGetRelid(rel),
														 row_id,
														 values,
														 isnull,
														 max_index_attnum,
														 &stored_natts))
	{
		preserves = false;
		goto done;
	}

	for (AttrNumber attnum = 1; attnum <= max_index_attnum; attnum++)
	{
		Form_pg_attribute attr;
		Datum		old_value;
		Datum		new_value;
		bool		old_isnull;
		bool		new_isnull;

		if (!fastpg_mem_update_index_attr_member(rel, attnum))
			continue;

		attr = TupleDescAttr(tupdesc, attnum - 1);
		if (attr->attisdropped)
			continue;

		if ((size_t) attnum > stored_natts)
		{
			preserves = false;
			break;
		}

		old_isnull = isnull[attnum - 1] != 0;
		old_value = (Datum) values[attnum - 1];
		new_value = slot_getattr(new_slot, attnum, &new_isnull);
		if (old_isnull != new_isnull ||
			(!old_isnull &&
			 !fastpg_mem_datum_attr_equal(old_value, new_value, attr)))
		{
			preserves = false;
			break;
		}
	}

done:
	if (heap_buffers)
	{
		pfree(values);
		pfree(isnull);
	}
	return preserves;
}

static bool
fastpg_mem_update_preserves_index_attrs(Relation rel,
										uint64_t row_id,
										TupleTableSlot *new_slot,
										bool storage2)
{
	TupleDesc	tupdesc = RelationGetDescr(rel);
	TupleTableSlot *old_slot;
	ItemPointerData tid;
	bool		preserves = true;
	AttrNumber	max_index_attnum;

	if (!fastpg_catalog_mode_uses_postgres())
		return false;

	fastpg_mem_ensure_index_attr_bitmaps(rel);
	if (fastpg_mem_update_index_attrs_empty(rel))
		return false;
	if (fastpg_mem_index_bitmap_has_non_user_attrs(rel->rd_hotblockingattr, tupdesc) ||
		fastpg_mem_index_bitmap_has_non_user_attrs(rel->rd_summarizedattr, tupdesc) ||
		fastpg_mem_index_bitmap_has_non_user_attrs(rel->rd_keyattr, tupdesc) ||
		fastpg_mem_index_bitmap_has_non_user_attrs(rel->rd_idattr, tupdesc))
		return false;
	max_index_attnum = fastpg_mem_update_index_max_user_attr(rel);
	if (max_index_attnum <= 0 || max_index_attnum > tupdesc->natts)
		return false;

	if (storage2)
	{
		AttrNumber	single_attnum;
		bool		cached_preserves;

		if (fastpg_mem_single_update_index_attr(rel, tupdesc, &single_attnum) &&
			!TupleDescAttr(tupdesc, single_attnum - 1)->atthasmissing &&
			fastpg_mem_cached_single_index_key_preserves(rel,
														 row_id,
														 new_slot,
														 single_attnum,
														 &cached_preserves))
			return cached_preserves;
		return fastpg_mem_storage2_update_preserves_index_attrs(rel,
																row_id,
																new_slot,
																max_index_attnum);
	}
	else if (!fastpg_mem_row_id_to_tid(rel, row_id, &tid))
		return false;

	old_slot = MakeSingleTupleTableSlot(RelationGetDescr(rel),
										fastpg_mem_slot_callbacks(rel));
	if (!fastpg_mem_tuple_fetch_row_version(rel, &tid, SnapshotAny, old_slot))
	{
		ExecDropSingleTupleTableSlot(old_slot);
		return false;
	}

	for (AttrNumber attnum = 1; attnum <= max_index_attnum; attnum++)
	{
		Form_pg_attribute attr;
		Datum		old_value;
		Datum		new_value;
		bool		old_isnull;
		bool		new_isnull;

		if (!fastpg_mem_update_index_attr_member(rel, attnum))
			continue;

		attr = TupleDescAttr(tupdesc, attnum - 1);
		if (attr->attisdropped)
			continue;

		old_value = slot_getattr(old_slot, attnum, &old_isnull);
		new_value = slot_getattr(new_slot, attnum, &new_isnull);
		if (old_isnull != new_isnull ||
			(!old_isnull &&
			 !fastpg_mem_datum_attr_equal(old_value, new_value, attr)))
		{
			preserves = false;
			break;
		}
	}

	ExecDropSingleTupleTableSlot(old_slot);
	return preserves;
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
	uintptr_t	stack_values[FASTPG_MEM_STACK_NATTS];
	uint8_t		stack_isnull[FASTPG_MEM_STACK_NATTS];
	uint8_t		stack_byval[FASTPG_MEM_STACK_NATTS];
	size_t		stack_value_lens[FASTPG_MEM_STACK_NATTS];
	uint8_t		stack_owned[FASTPG_MEM_STACK_NATTS];
	uintptr_t  *values = stack_values;
	uint8_t    *isnull = stack_isnull;
	uint8_t    *byval = stack_byval;
	size_t	   *value_lens = stack_value_lens;
	uint8_t    *owned = stack_owned;
	uint64_t	row_id;
	uint64_t	input_row_id = 0;
	bool		storage2 = fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(rel));
	bool		heap_buffers = tupdesc->natts > FASTPG_MEM_STACK_NATTS;
	bool		used_toasted_tuple = false;
	bool		should_free_heap_tuple = false;
	bool		should_free_old_heap_tuple = false;
	HeapTuple	heap_tuple = NULL;
	HeapTuple	toasted_tuple = NULL;
	HeapTuple	old_heap_tuple = NULL;
	TupleTableSlot *old_slot = NULL;
	bool		invalidate_catalog_tuple =
		fastpg_catalog_mode_uses_postgres() &&
		IsCatalogRelation(rel) &&
		!IsToastRelation(rel);
	bool		preserve_update_tid = false;
	bool		storage2_hot_update = false;
	bool		storage2_inline_hot_update = false;
	bool		storage2_index_preservation_known = false;
	AttrNumber	storage2_inline_hot_attnum = 0;

	if (update_indexes != NULL)
		*update_indexes = (storage2 || fastpg_catalog_mode_uses_postgres()) ?
			TU_All : TU_None;
	if (lockmode != NULL)
		*lockmode = LockTupleExclusive;

	if (storage2)
	{
		uint32_t	relid = (uint32_t) RelationGetRelid(rel);

		row_id = fastpg_mem_tid_to_storage2_tid(otid);
		if (row_id == 0)
		{
			fastpg_mem_fill_deleted_tmfd(otid, tmfd);
			return TM_Deleted;
		}
		input_row_id = row_id;
		if (fastpg_catalog_mode_uses_postgres())
			fastpg_mem_acquire_storage2_update_row_lock(relid, &row_id);
	}
	else if (!fastpg_mem_tid_to_row_id(rel, otid, &row_id))
	{
		fastpg_mem_fill_deleted_tmfd(otid, tmfd);
		return TM_Deleted;
	}
	else if (fastpg_catalog_mode_uses_postgres())
	{
		CommandId	delete_cid;

		if (fastpg_mem_row_deleted_by_current_xact((uint32_t) RelationGetRelid(rel),
												   row_id,
												   cid,
												   false,
												   &delete_cid))
		{
			fastpg_mem_fill_self_modified_tmfd(otid, delete_cid, tmfd);
			return TM_SelfModified;
		}
		row_id = fastpg_mem_resolve_row_redirect((uint32_t) RelationGetRelid(rel),
												 row_id);
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

	if (storage2 && fastpg_catalog_mode_uses_postgres())
	{
		AttrNumber	single_attnum;

		fastpg_mem_ensure_index_attr_bitmaps(rel);
		if (fastpg_mem_single_update_index_attr(rel, tupdesc, &single_attnum) &&
			!TupleDescAttr(tupdesc, single_attnum - 1)->atthasmissing &&
			TupleDescAttr(tupdesc, single_attnum - 1)->attbyval)
		{
			bool		cached_preserves;

			if (fastpg_mem_cached_single_index_key_preserves(rel,
															 row_id,
															 slot,
															 single_attnum,
															 &cached_preserves))
			{
				storage2_index_preservation_known = true;
				if (cached_preserves)
				{
					storage2_hot_update = true;
					if (update_indexes != NULL)
						*update_indexes = TU_None;
				}
			}
			else
			{
				storage2_inline_hot_update = true;
				storage2_inline_hot_attnum = single_attnum;
			}
		}
	}
	if (storage2 && fastpg_catalog_mode_uses_postgres() &&
		!storage2_index_preservation_known &&
		!storage2_hot_update &&
		!storage2_inline_hot_update &&
		fastpg_mem_update_preserves_index_attrs(rel, row_id, slot, true))
	{
		storage2_hot_update = true;
		if (update_indexes != NULL)
			*update_indexes = TU_None;
	}

	fastpg_mem_ensure_write_xact();
	if (heap_buffers)
	{
		values = palloc_array(uintptr_t, tupdesc->natts);
		isnull = palloc_array(uint8_t, tupdesc->natts);
		byval = palloc_array(uint8_t, tupdesc->natts);
		value_lens = palloc_array(size_t, tupdesc->natts);
		owned = palloc_array(uint8_t, tupdesc->natts);
	}
	if (fastpg_mem_slot_needs_heap_tuple(rel, slot) ||
		(fastpg_catalog_mode_uses_postgres() &&
		 rel->rd_rel->reltoastrelid != InvalidOid &&
		 fastpg_mem_relation_may_have_external_toast((uint32_t) RelationGetRelid(rel))))
	{
		heap_tuple = ExecFetchSlotHeapTuple(slot, true, &should_free_heap_tuple);
		fastpg_mem_prepare_heap_tuple_header(rel, heap_tuple, cid, 0);
		if (rel->rd_rel->reltoastrelid != InvalidOid ||
			invalidate_catalog_tuple)
		{
			ItemPointerData resolved_tid;

			if (storage2)
			{
				if (!fastpg_mem_storage2_tid_to_tid(row_id, &resolved_tid))
					elog(ERROR, "fastpg_mem storage2 TID %llu cannot be represented as a CTID",
						 (unsigned long long) row_id);
			}
			else if (!fastpg_mem_row_id_to_tid(rel, row_id, &resolved_tid))
				elog(ERROR, "fastpg_mem row id %llu cannot be represented as a CTID",
					 (unsigned long long) row_id);
			old_slot = MakeSingleTupleTableSlot(RelationGetDescr(rel),
												fastpg_mem_slot_callbacks(rel));
			if (fastpg_mem_tuple_fetch_row_version(rel,
												   &resolved_tid,
												   SnapshotAny,
												   old_slot))
				old_heap_tuple =
					ExecFetchSlotHeapTuple(old_slot,
										   true,
										   &should_free_old_heap_tuple);
		}
		if (HeapTupleHasExternal(heap_tuple) ||
			heap_tuple->t_len > TOAST_TUPLE_THRESHOLD ||
			(old_heap_tuple != NULL && HeapTupleHasExternal(old_heap_tuple)))
		{
			toasted_tuple =
				heap_toast_insert_or_update(rel,
											heap_tuple,
											old_heap_tuple,
											0);
			fastpg_mem_fill_heap_tuple_values(rel,
											  toasted_tuple,
											  values,
											  isnull,
											  byval,
											  value_lens);
			memset(owned, 0, sizeof(uint8_t) * tupdesc->natts);
			used_toasted_tuple = true;
		}
		else
		{
			fastpg_mem_fill_heap_tuple_values(rel,
											  heap_tuple,
											  values,
											  isnull,
											  byval,
											  value_lens);
			memset(owned, 0, sizeof(uint8_t) * tupdesc->natts);
			used_toasted_tuple = true;
		}
		if ((toasted_tuple != NULL && HeapTupleHasExternal(toasted_tuple)) ||
			(toasted_tuple == NULL && HeapTupleHasExternal(heap_tuple)))
			fastpg_mem_note_relation_external_toast((uint32_t) RelationGetRelid(rel));
	}
	if (used_toasted_tuple)
	{
		/* values already filled from the toasted heap tuple */
	}
	else if (storage2)
		fastpg_mem_fill_slot_values_borrowed(rel,
											 slot,
											 values,
											 isnull,
											 byval,
											 value_lens,
											 owned);
	else
		fastpg_mem_fill_slot_values_borrowed(rel,
											 slot,
											 values,
											 isnull,
											 byval,
											 value_lens,
											 owned);
	fastpg_mem_ensure_block_layout_for_slot(rel, slot);
	if (fastpg_catalog_mode_uses_postgres() && !storage2)
	{
		uint64_t	old_row_id = row_id;
		uint64_t	new_row_id = 0;
		CommandId	delete_cid =
			fastpg_mem_delete_cid_for_snapshot(cid, snapshot);

		preserve_update_tid =
			fastpg_mem_relation_has_brin_index(rel) &&
			!fastpg_mem_relation_has_unique_index(rel);

		if (preserve_update_tid)
		{
			if (!fastpg_rust_relation_update_with_metadata(RelationGetRelid(rel),
														  row_id,
														  GetCurrentTransactionId(),
														  delete_cid,
														  values,
														  isnull,
														  byval,
														  value_lens,
														  tupdesc->natts))
				new_row_id = 0;
			else
				new_row_id = row_id;
		}
		else if (!fastpg_rust_relation_delete_with_metadata(RelationGetRelid(rel),
															row_id,
															GetCurrentTransactionId(),
															delete_cid) ||
				 !fastpg_rust_relation_insert_unchecked(RelationGetRelid(rel),
														values,
														isnull,
														byval,
														value_lens,
														tupdesc->natts,
														&new_row_id))
			new_row_id = 0;

		if (new_row_id == 0)
		{
			if (!used_toasted_tuple)
				fastpg_mem_free_owned_slot_value_payloads(rel, values, isnull, owned);
			if (toasted_tuple != NULL && toasted_tuple != heap_tuple)
				heap_freetuple(toasted_tuple);
			if (heap_tuple != NULL && should_free_heap_tuple)
				heap_freetuple(heap_tuple);
			if (old_heap_tuple != NULL && should_free_old_heap_tuple)
				heap_freetuple(old_heap_tuple);
			if (old_slot != NULL)
				ExecDropSingleTupleTableSlot(old_slot);
			if (heap_buffers)
			{
				pfree(values);
				pfree(isnull);
				pfree(byval);
				pfree(value_lens);
				pfree(owned);
			}
			if (fastpg_mem_has_storage_error())
				fastpg_mem_raise_storage_error("fastpg_mem failed to update row in Rust storage");
			fastpg_mem_fill_deleted_tmfd(otid, tmfd);
			return TM_Deleted;
		}

		row_id = new_row_id;
		if (!preserve_update_tid)
			fastpg_mem_record_row_redirect((uint32_t) RelationGetRelid(rel),
										   old_row_id,
										   new_row_id);
	}
	else
	{
		bool		update_ok;

		if (storage2 && fastpg_catalog_mode_uses_postgres())
		{
			uint32_t	update_xid = (uint32_t) GetCurrentTransactionId();
			uint32_t	delete_cid =
				(uint32_t) fastpg_mem_delete_cid_for_snapshot(cid, snapshot);

			if (storage2_inline_hot_update)
			{
				bool		hot_preserved = false;
				AttrNumber	attnum = storage2_inline_hot_attnum;

				update_ok =
					fastpg_storage2_relation_update_hot_if_single_byval_preserved_with_metadata(RelationGetRelid(rel),
																								row_id,
																								(size_t) attnum,
																								values[attnum - 1],
																								isnull[attnum - 1],
																								update_xid,
																								delete_cid,
																								update_xid,
																								(uint32_t) cid,
																								update_xid,
																								values,
																								isnull,
																								byval,
																								value_lens,
																								tupdesc->natts,
																								&row_id,
																								&hot_preserved);
				if (hot_preserved && update_indexes != NULL)
					*update_indexes = TU_None;
				if (hot_preserved)
					fastpg_mem_remember_single_byval_index_key((uint32_t) RelationGetRelid(rel),
															   row_id,
															   attnum,
															   values[attnum - 1],
															   isnull[attnum - 1]);
			}
			else
				update_ok = storage2_hot_update ?
				fastpg_storage2_relation_update_hot_unchecked_with_metadata(RelationGetRelid(rel),
																			row_id,
																			update_xid,
																			delete_cid,
																			update_xid,
																			(uint32_t) cid,
																			update_xid,
																			values,
																			isnull,
																			byval,
																			value_lens,
																			tupdesc->natts,
																			&row_id) :
				fastpg_storage2_relation_update_unchecked_with_metadata(RelationGetRelid(rel),
																		row_id,
																		update_xid,
																		delete_cid,
																		update_xid,
																		(uint32_t) cid,
																		update_xid,
																		values,
																		isnull,
																		byval,
																		value_lens,
																		tupdesc->natts,
																		&row_id);
		}
		else
			update_ok = storage2 ?
				(storage2_hot_update ?
				 fastpg_storage2_relation_update_hot_unchecked(RelationGetRelid(rel),
															   row_id,
															   values,
															   isnull,
															   byval,
															   value_lens,
															   tupdesc->natts,
															   &row_id) :
				 fastpg_storage2_relation_update_unchecked(RelationGetRelid(rel),
														   row_id,
														   values,
														   isnull,
														   byval,
														   value_lens,
														   tupdesc->natts,
														   &row_id)) :
				fastpg_rust_relation_update_unchecked(RelationGetRelid(rel),
													  row_id,
													  values,
													  isnull,
													  byval,
													  value_lens,
													  tupdesc->natts);

		if (!update_ok)
		{
			if (!used_toasted_tuple)
				fastpg_mem_free_owned_slot_value_payloads(rel, values, isnull, owned);
			if (toasted_tuple != NULL && toasted_tuple != heap_tuple)
				heap_freetuple(toasted_tuple);
			if (heap_tuple != NULL && should_free_heap_tuple)
				heap_freetuple(heap_tuple);
			if (old_heap_tuple != NULL && should_free_old_heap_tuple)
				heap_freetuple(old_heap_tuple);
			if (old_slot != NULL)
				ExecDropSingleTupleTableSlot(old_slot);
			if (heap_buffers)
			{
				pfree(values);
				pfree(isnull);
				pfree(byval);
				pfree(value_lens);
				pfree(owned);
			}
			if (fastpg_mem_has_storage_error())
				fastpg_mem_raise_storage_error("fastpg_mem failed to update row in Rust storage");
			fastpg_mem_fill_deleted_tmfd(otid, tmfd);
			return TM_Deleted;
		}
	}

	if (storage2)
	{
		if (!fastpg_mem_storage2_tid_to_tid(row_id, &slot->tts_tid))
			elog(ERROR, "fastpg_mem storage2 TID %llu cannot be represented as a CTID",
				 (unsigned long long) row_id);
	}
	else if (!fastpg_mem_row_id_to_tid(rel, row_id, &slot->tts_tid))
		elog(ERROR, "fastpg_mem row id %llu cannot be represented as a CTID",
			 (unsigned long long) row_id);
	slot->tts_tableOid = RelationGetRelid(rel);
	if (invalidate_catalog_tuple &&
		old_heap_tuple != NULL &&
		heap_tuple != NULL)
	{
		heap_tuple->t_self = slot->tts_tid;
		heap_tuple->t_tableOid = RelationGetRelid(rel);
		fastpg_mem_cache_invalidate_heap_tuple(rel, old_heap_tuple, heap_tuple);
	}
	if (!storage2)
	{
		(void) fastpg_rust_relation_set_row_xmin((uint32_t) RelationGetRelid(rel),
												 row_id,
												 GetCurrentTransactionId(),
												 cid);
		if (fastpg_catalog_mode_uses_postgres())
			(void) fastpg_rust_relation_set_row_xmax((uint32_t) RelationGetRelid(rel),
													 row_id,
													 GetCurrentTransactionId());
	}
	if (fastpg_catalog_mode_uses_postgres())
	{
		pgstat_count_heap_update(rel, true, false);
		fastpg_mem_count_io_op(rel, IOCONTEXT_NORMAL, IOOP_WRITE, 1);
		fastpg_mem_count_io_op(rel, IOCONTEXT_NORMAL, IOOP_FSYNC, 1);
	}
	if (!used_toasted_tuple)
		fastpg_mem_free_owned_slot_value_payloads(rel, values, isnull, owned);
	if (toasted_tuple != NULL && toasted_tuple != heap_tuple)
		heap_freetuple(toasted_tuple);
	if (heap_tuple != NULL && should_free_heap_tuple)
		heap_freetuple(heap_tuple);
	if (old_heap_tuple != NULL && should_free_old_heap_tuple)
		heap_freetuple(old_heap_tuple);
	if (old_slot != NULL)
		ExecDropSingleTupleTableSlot(old_slot);
	if (heap_buffers)
	{
		pfree(values);
		pfree(isnull);
		pfree(byval);
		pfree(value_lens);
		pfree(owned);
	}

	if (storage2 && input_row_id != 0 && input_row_id != row_id)
		fastpg_mem_mark_row_touched((uint32_t) RelationGetRelid(rel),
									input_row_id,
									cid);
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
	bool		allow_current_touched =
		storage2 && ((flags & TUPLE_LOCK_FLAG_FIND_LAST_VERSION) != 0);

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
		if (fastpg_catalog_mode_uses_postgres())
		{
			uint64_t	resolved_row_id;
			CommandId	delete_cid;

			resolved_row_id =
				fastpg_mem_storage2_resolve_update_row_id((uint32_t) RelationGetRelid(rel),
														  row_id);
			if (resolved_row_id != row_id)
			{
				row_id = resolved_row_id;
				if (tmfd != NULL)
					tmfd->traversed = true;
			}
			else if (fastpg_mem_storage2_row_deleted_by_current_xact_any_cid((uint32_t) RelationGetRelid(rel),
																			 row_id,
																			 &delete_cid))
			{
				fastpg_mem_fill_self_modified_tmfd(tid, delete_cid, tmfd);
				return TM_SelfModified;
			}
		}
	}
	else if (!fastpg_mem_tid_to_row_id(rel, tid, &row_id))
	{
		fastpg_mem_fill_deleted_tmfd(tid, tmfd);
		return TM_Deleted;
	}
	else if (fastpg_catalog_mode_uses_postgres())
	{
		CommandId	delete_cid;

		if (fastpg_mem_row_deleted_by_current_xact((uint32_t) RelationGetRelid(rel),
												   row_id,
												   cid,
												   false,
												   &delete_cid))
		{
			fastpg_mem_fill_self_modified_tmfd(tid, delete_cid, tmfd);
			return TM_SelfModified;
		}
		row_id = fastpg_mem_resolve_row_redirect((uint32_t) RelationGetRelid(rel),
												 row_id);
	}

	{
		CommandId	touched_cid;

		if (fastpg_mem_row_touched((uint32_t) RelationGetRelid(rel),
								   row_id,
								   cid,
								   &touched_cid))
		{
			if (!allow_current_touched)
			{
				fastpg_mem_fill_self_modified_tmfd(tid, touched_cid, tmfd);
				return TM_SelfModified;
			}
			if (tmfd != NULL)
				tmfd->traversed = true;
		}
	}

	if (fastpg_catalog_mode_uses_postgres() && storage2)
	{
		ItemPointerData resolved_tid;

		if (!fastpg_mem_storage2_tid_to_tid(row_id, &resolved_tid))
			elog(ERROR, "fastpg_mem storage2 TID %llu cannot be represented as a CTID",
				 (unsigned long long) row_id);
		if (!fastpg_mem_tuple_fetch_row_version(rel, &resolved_tid, snapshot, slot))
		{
			fastpg_mem_fill_deleted_tmfd(tid, tmfd);
			return TM_Deleted;
		}
	}
	else if (fastpg_catalog_mode_uses_postgres() && !storage2)
	{
		ItemPointerData resolved_tid;

		if (!fastpg_mem_row_id_to_tid(rel, row_id, &resolved_tid))
			elog(ERROR, "fastpg_mem row id %llu cannot be represented as a CTID",
				 (unsigned long long) row_id);
		if (!fastpg_mem_tuple_fetch_row_version(rel, &resolved_tid, snapshot, slot))
		{
			fastpg_mem_fill_deleted_tmfd(tid, tmfd);
			return TM_Deleted;
		}
	}
	else if (!fastpg_mem_tuple_fetch_row_version(rel, tid, snapshot, slot))
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
	{
		fastpg_mem_ensure_write_xact();
		fastpg_storage2_relation_clear(RelationGetRelid(rel));
	}
	else if (fastpg_catalog_mode_uses_postgres() &&
			 rel->rd_rel->relkind == RELKIND_RELATION)
		fastpg_rust_relation_clear_transactional(RelationGetRelid(rel));
	else
		fastpg_rust_relation_clear(RelationGetRelid(rel));
	fastpg_mem_clear_relation_external_toast((uint32_t) RelationGetRelid(rel));
	fastpg_mem_note_relation_changed((uint32_t) RelationGetRelid(rel));
	if (fastpg_catalog_mode_uses_postgres())
	{
		*freezeXid = RecentXmin;
		*minmulti = GetOldestMultiXactId();
	}
	else
	{
		*freezeXid = InvalidTransactionId;
		*minmulti = InvalidMultiXactId;
	}
}

static void
fastpg_mem_relation_nontransactional_truncate(Relation rel)
{
	if (fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(rel)))
		fastpg_storage2_relation_clear(RelationGetRelid(rel));
	else
		fastpg_rust_relation_clear(RelationGetRelid(rel));
	fastpg_mem_clear_relation_external_toast((uint32_t) RelationGetRelid(rel));
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
	Snapshot	scan_snapshot = snapshot != NULL ? snapshot : GetTransactionSnapshot();
	Tuplesortstate *tuplesort = NULL;
	CommandId	cid = GetCurrentCommandId(true);
	double		copied = 0.0;

	old_slot = table_slot_create(OldTable, NULL);
	new_slot = table_slot_create(NewTable, NULL);
	if (OldIndex != NULL && use_sort)
		tuplesort = tuplesort_begin_cluster(RelationGetDescr(OldTable),
											OldIndex,
											maintenance_work_mem,
											NULL,
											TUPLESORT_NONE);

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
			if (tuplesort != NULL)
			{
				bool		should_free = false;
				HeapTuple	heap_tuple;

				heap_tuple = ExecFetchSlotHeapTuple(old_slot,
													true,
													&should_free);
				tuplesort_putheaptuple(tuplesort, heap_tuple);
				if (should_free)
					heap_freetuple(heap_tuple);
			}
			else
			{
				ExecCopySlot(new_slot, old_slot);
				table_tuple_insert(NewTable, new_slot, cid, 0, NULL);
				ExecClearTuple(new_slot);
				copied += 1.0;
			}
			ExecClearTuple(old_slot);
		}
		table_endscan(table_scan);
	}

	if (tuplesort != NULL)
	{
		tuplesort_performsort(tuplesort);
		for (;;)
		{
			HeapTuple	heap_tuple;

			heap_tuple = tuplesort_getheaptuple(tuplesort, true);
			if (heap_tuple == NULL)
				break;
			ExecForceStoreHeapTuple(heap_tuple, old_slot, false);
			ExecCopySlot(new_slot, old_slot);
			table_tuple_insert(NewTable, new_slot, cid, 0, NULL);
			ExecClearTuple(old_slot);
			ExecClearTuple(new_slot);
			copied += 1.0;
		}
		tuplesort_end(tuplesort);
	}

	ExecDropSingleTupleTableSlot(old_slot);
	ExecDropSingleTupleTableSlot(new_slot);

	if (num_tuples != NULL)
		*num_tuples = copied;
	if (tups_vacuumed != NULL)
		*tups_vacuumed = 0.0;
	if (tups_recently_dead != NULL)
		*tups_recently_dead = 0.0;
}

static void
fastpg_mem_relation_vacuum_indexes(Relation rel,
								   const VacuumParams *params,
								   BufferAccessStrategy bstrategy,
								   double reltuples)
{
	int			nindexes;
	Relation   *indrels;

	if (!rel->rd_rel->relhasindex ||
		params->index_cleanup == VACOPTVALUE_DISABLED)
		return;

	vac_open_indexes(rel, RowExclusiveLock, &nindexes, &indrels);
	for (int index = 0; index < nindexes; index++)
	{
		IndexVacuumInfo ivinfo;
		IndexBulkDeleteResult *istat;

		memset(&ivinfo, 0, sizeof(ivinfo));
		ivinfo.index = indrels[index];
		ivinfo.heaprel = rel;
		ivinfo.analyze_only = false;
		ivinfo.report_progress = false;
		ivinfo.estimated_count = false;
		ivinfo.message_level = DEBUG2;
		ivinfo.num_heap_tuples = reltuples;
		ivinfo.strategy = bstrategy;

		istat = vac_cleanup_one_index(&ivinfo, NULL);
		if (istat != NULL)
			pfree(istat);
	}
	vac_close_indexes(nindexes, indrels, NoLock);
}

static void
fastpg_mem_relation_vacuum(Relation rel,
						   const VacuumParams *params,
						   BufferAccessStrategy bstrategy)
{
	uint32_t	relid = (uint32_t) RelationGetRelid(rel);
	size_t		row_count;
	BlockNumber pages;
	TimestampTz starttime = GetCurrentTimestamp();

	if (!fastpg_catalog_mode_uses_postgres())
		return;

	row_count = fastpg_mem_use_storage2_for_relid(relid) ?
		fastpg_storage2_relation_row_count(RelationGetRelid(rel)) :
		fastpg_rust_relation_row_count(RelationGetRelid(rel));
	pages = fastpg_mem_heap_pages_for_layout(rel, row_count);

	fastpg_mem_set_relation_all_visible(relid, pages > 0);
	{
		FastPgMemVisibilityState *visibility =
			fastpg_mem_relation_visibility_state(relid, true);

		visibility->known_empty = row_count == 0;
	}
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
	fastpg_mem_count_io_op(rel, IOCONTEXT_VACUUM, IOOP_READ, 1);
	fastpg_mem_count_io_op(rel, IOCONTEXT_VACUUM, IOOP_REUSE, 1);
	pgstat_report_vacuum(rel, (PgStat_Counter) row_count, 0, starttime);
	fastpg_mem_relation_vacuum_indexes(rel, params, bstrategy, (double) row_count);
}

static bool
fastpg_mem_scan_analyze_next_block(TableScanDesc scan, ReadStream *stream)
{
	FastPgMemScanDesc *fscan = (FastPgMemScanDesc *) scan;
	BlockNumber blockno;
	uint64_t	first_row_index;
	size_t		rows_remaining;
	uint64_t	maxoffset;

	if (!fscan->analyze)
		return false;

	if (stream != NULL)
	{
		BufferAccessStrategy strategy;

		blockno = read_stream_next_block(stream, &strategy);
		if (blockno == InvalidBlockNumber)
			return false;
	}
	else if (fscan->analyze_blocks_started >= fscan->analyze_total_blocks)
		return false;
	else
		blockno = fscan->analyze_blocks_started;

	fscan->analyze_blocks_started++;
	fscan->analyze_current_block = blockno;
	fscan->analyze_current_offset = FirstOffsetNumber;
	fscan->analyze_current_max_offset = InvalidOffsetNumber;

	first_row_index = (uint64_t) blockno *
		(uint64_t) fscan->analyze_rows_per_block;
	if (first_row_index >= fscan->analyze_row_count)
		return true;

	rows_remaining = fscan->analyze_row_count - (size_t) first_row_index;
	maxoffset = Min((uint64_t) fscan->analyze_rows_per_block,
					(uint64_t) rows_remaining);
	if (maxoffset > (uint64_t) MaxOffsetNumber)
		maxoffset = MaxOffsetNumber;
	fscan->analyze_current_max_offset = (OffsetNumber) maxoffset;
	return true;
}

static bool
fastpg_mem_scan_analyze_next_tuple(TableScanDesc scan,
								   double *liverows,
								   double *deadrows,
								   TupleTableSlot *slot)
{
	FastPgMemScanDesc *fscan = (FastPgMemScanDesc *) scan;
	OffsetNumber offset;
	ItemPointerData tid;

	if (!fscan->analyze ||
		!BlockNumberIsValid(fscan->analyze_current_block) ||
		!OffsetNumberIsValid(fscan->analyze_current_max_offset))
		return false;

	if (fscan->storage2)
	{
		while (fscan->analyze_current_offset <=
			   fscan->analyze_current_max_offset)
		{
			CHECK_FOR_INTERRUPTS();
			fscan->analyze_current_offset++;

			if (fastpg_mem_scan_getnextslot(scan, ForwardScanDirection, slot))
			{
				*liverows += 1;
				return true;
			}

			return false;
		}

		return false;
	}

	while (fscan->analyze_current_offset <=
		   fscan->analyze_current_max_offset)
	{
		CHECK_FOR_INTERRUPTS();
		offset = fscan->analyze_current_offset;
		fscan->analyze_current_offset++;

		ItemPointerSet(&tid, fscan->analyze_current_block, offset);
		if (fastpg_mem_tuple_fetch_row_version(scan->rs_rd,
											   &tid,
											   scan->rs_snapshot,
											   slot))
		{
			*liverows += 1;
			return true;
		}
	}

	return false;
}

static bool
fastpg_mem_index_build_uses_batch_scan(Relation index_rel)
{
	Oid			relam = index_rel->rd_rel->relam;

	return relam == BTREE_AM_OID || relam == HASH_AM_OID;
}

static bool
fastpg_mem_index_build_can_use_simple_scan(Relation table_rel,
										   IndexInfo *index_info,
										   TableScanDesc scan)
{
	TupleDesc	tupdesc = RelationGetDescr(table_rel);

	if (scan != NULL ||
		fastpg_mem_use_storage2_for_relid((uint32_t) RelationGetRelid(table_rel)) ||
		index_info->ii_Expressions != NIL ||
		index_info->ii_Predicate != NIL ||
		index_info->ii_NumIndexAttrs <= 0 ||
		index_info->ii_NumIndexAttrs > INDEX_MAX_KEYS)
		return false;

	for (int index = 0; index < index_info->ii_NumIndexAttrs; index++)
	{
		AttrNumber	attnum = index_info->ii_IndexAttrNumbers[index];
		Form_pg_attribute attr;

		if (attnum <= 0 || attnum > tupdesc->natts)
			return false;
		attr = TupleDescAttr(tupdesc, attnum - 1);
		if (attr->atthasmissing)
			return false;
	}

	return true;
}

static double
fastpg_mem_index_build_simple_scan(Relation table_rel,
								   Relation index_rel,
								   IndexInfo *index_info,
								   BlockNumber start_blockno,
								   BlockNumber end_blockno,
								   IndexBuildCallback callback,
								   void *callback_state)
{
	TupleDesc	tupdesc = RelationGetDescr(table_rel);
	int			natts = tupdesc->natts;
	uint64_t	scan_handle;
	uintptr_t  *batch_values;
	uint8_t    *batch_isnull;
	uint64_t   *batch_row_ids;
	size_t	   *batch_stored_natts;
	double		reltuples = 0;

	scan_handle = fastpg_rust_scan_begin(RelationGetRelid(table_rel));
	if (scan_handle == 0)
		fastpg_mem_raise_storage_error("fastpg_mem failed to create Rust scan handle");

	if (fastpg_catalog_mode_uses_postgres())
		pgstat_count_heap_scan(table_rel);

	batch_values =
		natts > 0 ?
		palloc0_array(uintptr_t, (Size) natts * FASTPG_MEM_SCAN_BATCH_ROWS) :
		NULL;
	batch_isnull =
		natts > 0 ?
		palloc0_array(uint8_t, (Size) natts * FASTPG_MEM_SCAN_BATCH_ROWS) :
		NULL;
	batch_row_ids = palloc_array(uint64_t, FASTPG_MEM_SCAN_BATCH_ROWS);
	batch_stored_natts = palloc_array(size_t, FASTPG_MEM_SCAN_BATCH_ROWS);

	for (;;)
	{
		size_t		batch_count;

		batch_count =
			fastpg_rust_scan_next_batch_with_stored_natts(scan_handle,
														  1,
														  batch_values,
														  batch_isnull,
														  (size_t) natts,
														  FASTPG_MEM_SCAN_BATCH_ROWS,
														  batch_row_ids,
														  batch_stored_natts);
		if (batch_count == 0)
			break;

		for (size_t batch_index = 0; batch_index < batch_count; batch_index++)
		{
			uint64_t	row_id = batch_row_ids[batch_index];
			ItemPointerData tid;
			BlockNumber blockno;
			Datum		values[INDEX_MAX_KEYS];
			bool		isnull[INDEX_MAX_KEYS];

			if (!fastpg_mem_row_id_to_tid(table_rel, row_id, &tid))
				elog(ERROR, "fastpg_mem row id %llu cannot be represented as a CTID",
					 (unsigned long long) row_id);

			blockno = ItemPointerGetBlockNumber(&tid);
			if (blockno < start_blockno ||
				(end_blockno != InvalidBlockNumber && blockno >= end_blockno))
				continue;

			reltuples += 1;
			for (int index = 0; index < index_info->ii_NumIndexAttrs; index++)
			{
				AttrNumber	attnum = index_info->ii_IndexAttrNumbers[index];
				Size		offset =
					((Size) batch_index * natts) + (attnum - 1);

				values[index] = (Datum) batch_values[offset];
				isnull[index] = batch_isnull[offset] != 0;
			}

			callback(index_rel, &tid, values, isnull, true, callback_state);
			if (fastpg_catalog_mode_uses_postgres())
			{
				pgstat_count_heap_getnext(table_rel);
				pgstat_count_buffer_hit(table_rel);
			}
		}
	}

	fastpg_rust_scan_end(scan_handle);
	if (batch_values != NULL)
		pfree(batch_values);
	if (batch_isnull != NULL)
		pfree(batch_isnull);
	pfree(batch_row_ids);
	pfree(batch_stored_natts);
	return reltuples;
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
	BlockNumber end_blockno = InvalidBlockNumber;
	FastPgMemScanDesc *fscan = NULL;
	bool		old_batch_enabled = false;

	if (numblocks != InvalidBlockNumber)
		end_blockno = start_blockno + numblocks;

	if (fastpg_mem_index_build_uses_batch_scan(index_rel) &&
		fastpg_mem_index_build_can_use_simple_scan(table_rel, index_info, scan))
		return fastpg_mem_index_build_simple_scan(table_rel,
												  index_rel,
												  index_info,
												  start_blockno,
												  end_blockno,
												  callback,
												  callback_state);

	estate = CreateExecutorState();
	econtext = GetPerTupleExprContext(estate);
	slot = table_slot_create(table_rel, NULL);
	econtext->ecxt_scantuple = slot;
	predicate = ExecPrepareQual(index_info->ii_Predicate, estate);

	if (scan == NULL)
	{
		scan = table_beginscan_strat(table_rel,
									 GetTransactionSnapshot(),
									 0,
									 NULL,
									 true,
									 allow_sync);
		need_endscan = true;
	}
	fscan = (FastPgMemScanDesc *) scan;
	old_batch_enabled = fscan->batch_enabled;
	if (fastpg_mem_index_build_uses_batch_scan(index_rel))
		fscan->batch_enabled = true;

	while (table_scan_getnextslot(scan, ForwardScanDirection, slot))
	{
		BlockNumber blockno = ItemPointerGetBlockNumber(&slot->tts_tid);

		CHECK_FOR_INTERRUPTS();
		if (blockno < start_blockno ||
			(end_blockno != InvalidBlockNumber && blockno >= end_blockno))
		{
			ExecClearTuple(slot);
			continue;
		}

		reltuples += 1;
		MemoryContextReset(econtext->ecxt_per_tuple_memory);

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

	fscan->batch_enabled = old_batch_enabled;
	fastpg_mem_scan_discard_batch(fscan);
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

	if (!(fastpg_catalog_mode_uses_postgres() &&
		  fastpg_mem_heap_pages_from_recorded_layout(rel, row_count, &pages)))
		pages = fastpg_mem_heap_pages_for_row_count(rel, NULL, row_count, false);
	if (fastpg_catalog_mode_uses_postgres())
	{
		BlockNumber layout_pages =
			fastpg_mem_heap_pages_for_layout(rel, row_count);

		pages = Max(pages, layout_pages);
		if (rel->rd_rel->reltuples >= 0 &&
			rel->rd_rel->relpages > 0)
		{
			BlockNumber catalog_pages = (BlockNumber) rel->rd_rel->relpages;

			pages = Max(pages, catalog_pages);
		}
	}

	return (uint64) pages * BLCKSZ;
}

static bool
fastpg_mem_pg_class_planner_stats_by_oid(Oid relid,
										 int32_t *relpages_out,
										 float4 *reltuples_out,
										 int32_t *relallvisible_out)
{
	Relation	pg_class;
	SysScanDesc scan;
	ScanKeyData key[1];
	HeapTuple	tuple;
	bool		found = false;

	if (!fastpg_catalog_mode_uses_postgres() || IsCatalogRelationOid(relid))
		return false;

	pg_class = table_open(RelationRelationId, AccessShareLock);
	ScanKeyInit(&key[0],
				Anum_pg_class_oid,
				BTEqualStrategyNumber, F_OIDEQ,
				ObjectIdGetDatum(relid));
	scan = systable_beginscan(pg_class, ClassOidIndexId, true,
							  NULL, 1, key);
	tuple = systable_getnext(scan);
	if (HeapTupleIsValid(tuple))
	{
		Form_pg_class classtup = (Form_pg_class) GETSTRUCT(tuple);

		if (relpages_out != NULL)
			*relpages_out = classtup->relpages;
		if (reltuples_out != NULL)
			*reltuples_out = classtup->reltuples;
		if (relallvisible_out != NULL)
			*relallvisible_out = classtup->relallvisible;
		found = true;
	}
	systable_endscan(scan);
	table_close(pg_class, AccessShareLock);

	return found;
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
	if (OidIsValid(rel->rd_rel->relam))
		return rel->rd_rel->relam;
	return HEAP_TABLE_AM_OID;
}

static void
fastpg_mem_relation_fetch_toast_slice(Relation toastrel,
									  Oid valueid,
									  int32 attrsize,
									  int32 sliceoffset,
									  int32 slicelength,
									  varlena *result)
{
	heap_fetch_toast_slice(toastrel,
						   valueid,
						   attrsize,
						   sliceoffset,
						   slicelength,
						   result);
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
	BlockNumber layout_pages;
	bool		storage2;
	int32_t		catalog_relpages = 0;
	float4		catalog_reltuples = 0;
	int32_t		catalog_relallvisible = 0;

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

	storage2 = fastpg_mem_use_storage2_for_relid(relid);
	if (storage2 &&
		fastpg_storage2_relation_row_count_if_visibility_deltas(relid,
																&row_count))
	{
		fastpg_mem_estimate_exact_storage2_size(rel, row_count,
												pages, tuples, allvisfrac);
		return;
	}
#ifdef FASTPG_USE_MEM_INDEX_AM
	if (storage2 && fastpg_catalog_mode_uses_postgres())
	{
		size_t		physical_pages;
		BlockNumber runtime_pages;

		row_count = fastpg_storage2_relation_row_count(RelationGetRelid(rel));
		layout_pages = fastpg_mem_heap_pages_for_layout(rel, row_count);
		physical_pages = fastpg_storage2_relation_block_count(relid);
		runtime_pages = physical_pages > (size_t) MaxBlockNumber ?
			MaxBlockNumber : (BlockNumber) physical_pages;
		runtime_pages = Max(runtime_pages, layout_pages);
		if (row_count > 0)
			runtime_pages = Max(runtime_pages, 8);

		*pages = runtime_pages;
		*tuples = (double) row_count;
		*allvisfrac = runtime_pages == 0 ? 0.0 :
			(double) FastPgMemRelationAllVisiblePages(rel) / (double) runtime_pages;
		return;
	}
#endif
	if (fastpg_catalog_mode_uses_postgres() &&
		rel->rd_rel->reltuples >= 0 &&
		rel->rd_rel->relpages > 0)
	{
		BlockNumber catalog_relpages = (BlockNumber) rel->rd_rel->relpages;
		BlockNumber catalog_allvisible =
			(BlockNumber) Min(rel->rd_rel->relallvisible, rel->rd_rel->relpages);

		*pages = catalog_relpages;
		*tuples = (double) rel->rd_rel->reltuples;
		*allvisfrac = (double) catalog_allvisible / (double) catalog_relpages;
		return;
	}

	if (fastpg_mem_pg_class_planner_stats_by_oid(relid,
												 &catalog_relpages,
												 &catalog_reltuples,
												 &catalog_relallvisible) &&
		catalog_reltuples >= 0 &&
		catalog_relpages > 0)
	{
		BlockNumber catalog_allvisible =
			(BlockNumber) Min(catalog_relallvisible, catalog_relpages);

		*pages = (BlockNumber) catalog_relpages;
		*tuples = (double) catalog_reltuples;
		*allvisfrac = (double) catalog_allvisible / (double) catalog_relpages;
		return;
	}

	if (fastpg_use_rust_catalog() &&
		fastpg_rust_catalog_policy_by_relation_oid(relid) != 0)
		row_count = fastpg_rust_catalog_row_count(relid);
	else
	{
		row_count = storage2 ?
			fastpg_storage2_relation_row_count(RelationGetRelid(rel)) :
			fastpg_rust_relation_row_count(RelationGetRelid(rel));
	}

	if (fastpg_catalog_mode_uses_postgres() &&
		rel->rd_rel->reltuples >= 0 &&
		rel->rd_rel->relpages > 0 &&
		row_count > 0 &&
		(double) row_count <= (double) rel->rd_rel->reltuples)
	{
		BlockNumber catalog_relpages = (BlockNumber) rel->rd_rel->relpages;
		BlockNumber catalog_allvisible =
			(BlockNumber) Min(rel->rd_rel->relallvisible, rel->rd_rel->relpages);

		*pages = catalog_relpages;
		*tuples = (double) rel->rd_rel->reltuples;
		*allvisfrac = (double) catalog_allvisible / (double) catalog_relpages;
		return;
	}

	if (fastpg_catalog_mode_uses_postgres() &&
		row_count == 0 &&
		rel->rd_rel->reltuples < 0 &&
		fastpg_mem_relation_visibility_state(relid, false) != NULL &&
		fastpg_mem_relation_visibility_state(relid, false)->known_empty)
	{
		*pages = 0;
		*tuples = 0;
		*allvisfrac = 0;
		return;
	}

	fastpg_mem_estimate_heap_size(rel, attr_widths, row_count,
								  pages, tuples, allvisfrac);
	if (row_count > 0 && storage2)
		*pages = Max(*pages, 8);
	if (!storage2 &&
		!fastpg_mem_heap_pages_from_recorded_layout(rel, row_count, &layout_pages))
	{
		layout_pages = fastpg_mem_heap_pages_for_layout(rel, row_count);

		if (layout_pages > *pages)
		{
			*pages = layout_pages;
			*tuples = (double) row_count;
			*allvisfrac = *pages == 0 ? 0.0 :
				(double) FastPgMemRelationAllVisiblePages(rel) / (double) *pages;
		}
	}
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
			ItemPointerData fetch_tid;
			OffsetNumber offset;

			if (fscan->bitmap_result.lossy)
				offset = (OffsetNumber) (fscan->bitmap_index + FirstOffsetNumber);
			else
				offset = fscan->bitmap_offsets[fscan->bitmap_index];
			fscan->bitmap_index++;

			ItemPointerSet(&tid, fscan->bitmap_result.blockno, offset);
			fetch_tid = tid;
			if (!fscan->bitmap_result.lossy)
			{
				ItemPointerData resolved_tid;

				if (FastPgMemResolveIndexFetchTid(scan->rs_rd, &tid, &resolved_tid))
					fetch_tid = resolved_tid;
			}
			if (fastpg_mem_tuple_fetch_row_version(scan->rs_rd,
												   &fetch_tid,
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
	FastPgMemScanDesc *fscan = (FastPgMemScanDesc *) scan;
	TsmRoutine *tsm = scanstate->tsmroutine;
	Oid			relid = RelationGetRelid(scan->rs_rd);
	size_t		row_count = fscan->storage2 ?
		fastpg_storage2_relation_row_count(relid) :
		fastpg_rust_relation_row_count(relid);
	BlockNumber blockno;

	fscan->sample_nblocks =
		fastpg_mem_heap_pages_for_layout(scan->rs_rd, row_count);
	if (fscan->sample_nblocks == 0)
		return false;

	if (tsm->NextSampleBlock)
		blockno = tsm->NextSampleBlock(scanstate, fscan->sample_nblocks);
	else if (fscan->sample_block == InvalidBlockNumber)
		blockno = 0;
	else
	{
		blockno = fscan->sample_block + 1;
		if (blockno >= fscan->sample_nblocks)
			blockno = InvalidBlockNumber;
	}

	fscan->sample_block = blockno;
	return BlockNumberIsValid(blockno);
}

static bool
fastpg_mem_scan_sample_next_tuple(TableScanDesc scan,
								  SampleScanState *scanstate,
								  TupleTableSlot *slot)
{
	FastPgMemScanDesc *fscan = (FastPgMemScanDesc *) scan;
	TsmRoutine *tsm = scanstate->tsmroutine;
	BlockNumber blockno = fscan->sample_block;
	Oid			relid = RelationGetRelid(scan->rs_rd);
	size_t		row_count;
	uint64_t	first_row_index;
	OffsetNumber maxoffset;

	if (!BlockNumberIsValid(blockno))
		return false;

	row_count = fscan->storage2 ?
		fastpg_storage2_relation_row_count(relid) :
		fastpg_rust_relation_row_count(relid);
	first_row_index =
		(uint64_t) blockno * fastpg_mem_relation_rows_per_block(scan->rs_rd);
	if (first_row_index >= row_count)
		return false;
	maxoffset = (OffsetNumber) Min(fastpg_mem_relation_rows_per_block(scan->rs_rd),
								   row_count - first_row_index);

	for (;;)
	{
		OffsetNumber tupoffset;
		ItemPointerData tid;

		CHECK_FOR_INTERRUPTS();
		tupoffset = tsm->NextSampleTuple(scanstate, blockno, maxoffset);
		if (!OffsetNumberIsValid(tupoffset))
		{
			ExecClearTuple(slot);
			return false;
		}

		if (fscan->storage2)
		{
			uint64_t	packed_tid;

			if (!fastpg_storage2_relation_visible_tid_at(relid,
														 first_row_index + tupoffset - 1,
														 &packed_tid))
				continue;
			if (!fastpg_mem_storage2_tid_to_tid(packed_tid, &tid))
				continue;
		}
		else
			ItemPointerSet(&tid, blockno, tupoffset);
		if (fastpg_mem_tuple_fetch_row_version(scan->rs_rd,
											   &tid,
											   scan->rs_snapshot,
											   slot))
			return true;
	}
}

static const TableAmRoutine fastpg_mem_methods = {
	.type = T_TableAmRoutine,

	.slot_callbacks = fastpg_mem_slot_callbacks,

	.scan_begin = fastpg_mem_scan_begin,
	.scan_end = fastpg_mem_scan_end,
	.scan_rescan = fastpg_mem_scan_rescan,
	.scan_getnextslot = fastpg_mem_scan_getnextslot,

	.scan_set_tidrange = fastpg_mem_scan_set_tidrange,
	.scan_getnextslot_tidrange = fastpg_mem_scan_getnextslot_tidrange,

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
	.amcanorder = true,
	.amcanorderbyop = false,
	.amcanhash = false,
	.amconsistentequality = true,
	.amconsistentordering = true,
	.amcanbackward = true,
	.amcanunique = true,
	.amcanmulticol = true,
	.amoptionalkey = true,
	.amsearcharray = true,
	.amsearchnulls = true,
	.amstorage = false,
	.amclusterable = true,
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
