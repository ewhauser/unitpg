/*-------------------------------------------------------------------------
 *
 * fastpg_ipc_guard.c
 *	  Build-time IPC guard for fastpg's single-process Rust server.
 *
 *-------------------------------------------------------------------------
 */
#include "postgres.h"

#ifdef USE_FASTPG

#include "utils/fastpg_ipc_guard.h"

bool
fastpg_internal_ipc_forbidden(void)
{
	/*
	 * fastpg builds are single-process by default. Reaching PostgreSQL's
	 * shared-memory, semaphore, background worker, or wait-latch IPC paths is
	 * a bug in the Rust-server execution path.
	 */
	return true;
}

void
fastpg_forbid_internal_ipc(const char *operation, const char *file, int line)
{
	if (!fastpg_internal_ipc_forbidden())
		return;

	ereport(ERROR,
			(errcode(ERRCODE_INTERNAL_ERROR),
			 errmsg_internal("FASTPG_INTERNAL_IPC_FORBIDDEN: fastpg internal IPC path reached: %s",
							 operation),
			 errdetail_internal("source: %s:%d", file, line)));
}

#endif							/* USE_FASTPG */
