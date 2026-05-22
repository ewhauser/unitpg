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

extern const TableAmRoutine *GetFastPgMemTableAmRoutine(void);
extern const IndexAmRoutine *GetFastPgMemIndexAmRoutine(void);
extern BlockNumber FastPgMemRelationPages(Relation rel);
extern BlockNumber FastPgMemRelationPhysicalPages(Relation rel);
extern BlockNumber FastPgMemRelationAllVisiblePages(Relation rel);
extern void FastPgMemResetCommandTouchedRows(void);
extern bool FastPgMemResolveIndexFetchTid(Relation heapRelation,
										  const ItemPointerData *tupleid,
										  ItemPointer resolvedTid);
extern bool FastPgMemIndexCheckUniqueConflict(Relation heapRelation,
											  Relation indexRelation,
											  const Datum *values,
											  const bool *isnull,
											  const ItemPointerData *tupleid,
											  bool *satisfies,
											  ItemPointer conflictTid);

#endif							/* USE_FASTPG */

#endif							/* FASTPG_TABLEAM_H */
