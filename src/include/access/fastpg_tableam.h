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

#include "access/tableam.h"

extern const TableAmRoutine *GetFastPgMemTableAmRoutine(void);

#endif							/* USE_FASTPG */

#endif							/* FASTPG_TABLEAM_H */
