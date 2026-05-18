/*-------------------------------------------------------------------------
 *
 * fastpg_ipc_guard.c
 *	  Validation guard for fastpg's single-process Rust server.
 *
 *-------------------------------------------------------------------------
 */
#include "postgres.h"

#ifdef USE_FASTPG

#include <string.h>

#include "utils/fastpg_ipc_guard.h"

bool
fastpg_internal_ipc_forbidden(void)
{
	const char *setting = getenv("FASTPG_NO_INTERNAL_IPC");

	return setting != NULL &&
		setting[0] != '\0' &&
		strcmp(setting, "0") != 0;
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
