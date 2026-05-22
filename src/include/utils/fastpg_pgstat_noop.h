/*-------------------------------------------------------------------------
 *
 * fastpg_pgstat_noop.h
 *	  Runtime guard for fastpg's embedded pgcore pgstat no-op mode.
 *
 *-------------------------------------------------------------------------
 */
#ifndef FASTPG_PGSTAT_NOOP_H
#define FASTPG_PGSTAT_NOOP_H

static inline bool
fastpg_pgstat_noop_active(void)
{
#if defined(USE_FASTPG) && defined(FASTPG_NOOP_PGSTAT)
	return true;
#else
	return false;
#endif
}

#endif							/* FASTPG_PGSTAT_NOOP_H */
