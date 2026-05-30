/*-------------------------------------------------------------------------
 *
 * fastpg_tableam.h
 *	  fastpg table access method declarations.
 *
 * IDENTIFICATION
 *	  src/include/access/fastpg_tableam.h
 *
 *-------------------------------------------------------------------------
 */
#ifndef FASTPG_TABLEAM_H
#define FASTPG_TABLEAM_H

#ifdef USE_FASTPG

#include "access/amapi.h"
#include "access/tableam.h"
#include "nodes/execnodes.h"

extern const TableAmRoutine *GetFastPgMemTableAmRoutine(void);
extern const IndexAmRoutine *GetFastPgMemIndexAmRoutine(void);
extern BlockNumber FastPgMemRelationPages(Relation rel);
extern BlockNumber FastPgMemIndexPages(Relation rel, double reltuples);
extern BlockNumber FastPgMemRelationPhysicalPages(Relation rel);
extern BlockNumber FastPgMemRelationAllVisiblePages(Relation rel);
extern void FastPgMemResetCommandTouchedRows(void);
extern void FastPgMemRelationDropStorage(Relation rel);
extern bool FastPgMemResolveIndexFetchTid(Relation heapRelation,
										  const ItemPointerData *tupleid,
										  ItemPointer resolvedTid);
extern bool FastPgMemIndexFetchTupleCheck(Relation rel,
										  ItemPointer tid,
										  Snapshot snapshot,
										  bool *all_dead);
extern bool FastPgMemLookupPrimaryKeyTuple(Relation heapRelation,
										   Relation indexRelation,
										   const Datum *values,
										   const bool *isnull,
										   int nkeys,
										   Snapshot snapshot,
										   TupleTableSlot *slot,
										   bool *handled);
extern bool FastPgMemIndexCheckUniqueConflict(Relation heapRelation,
											  Relation indexRelation,
											  const Datum *values,
											  const bool *isnull,
											  const ItemPointerData *tupleid,
											  bool *satisfies,
											  ItemPointer conflictTid);
extern IndexBuildResult *FastPgMemBtreeBuild(Relation heapRelation,
											 Relation indexRelation,
											 IndexInfo *indexInfo);
extern bool FastPgMemBtreeCanHandleIndex(Relation heapRelation,
										 Relation indexRelation);
extern bool FastPgMemBtreeInsert(Relation indexRelation,
								 Datum *values,
								 bool *isnull,
								 ItemPointer heap_tid,
								 Relation heapRelation,
								 IndexUniqueCheck checkUnique,
								 bool indexUnchanged,
								 IndexInfo *indexInfo);

#endif							/* USE_FASTPG */

#endif							/* FASTPG_TABLEAM_H */
