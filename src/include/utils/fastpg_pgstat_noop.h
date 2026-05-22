/*-------------------------------------------------------------------------
 *
 * fastpg_pgstat_noop.h
 *	  Runtime guard for fastpg's embedded pgcore pgstat no-op mode.
 *
 *-------------------------------------------------------------------------
 */
#ifndef FASTPG_PGSTAT_NOOP_H
#define FASTPG_PGSTAT_NOOP_H

#include "utils/timestamp.h"

static inline bool
fastpg_pgstat_noop_active(void)
{
#if defined(USE_FASTPG) && defined(FASTPG_NOOP_PGSTAT)
	return true;
#else
	return false;
#endif
}

extern void fastpg_pgstat_noop_report_vacuum(Oid relid, TimestampTz ts);
extern void fastpg_pgstat_noop_report_analyze(Oid relid, TimestampTz ts);
extern bool fastpg_pgstat_noop_relation_int64(Oid relid, const char *stat,
											  int64 *result);
extern bool fastpg_pgstat_noop_relation_timestamp(Oid relid, const char *stat,
												  TimestampTz *result);

#endif							/* FASTPG_PGSTAT_NOOP_H */
