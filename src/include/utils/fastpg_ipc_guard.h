/*-------------------------------------------------------------------------
 *
 * fastpg_ipc_guard.h
 *	  Validation guard for fastpg's single-process Rust server.
 *
 *-------------------------------------------------------------------------
 */
#ifndef FASTPG_IPC_GUARD_H
#define FASTPG_IPC_GUARD_H

#include "postgres.h"

#ifdef USE_FASTPG

extern bool fastpg_internal_ipc_forbidden(void);
extern void fastpg_forbid_internal_ipc(const char *operation,
									   const char *file, int line);

#define FASTPG_FORBID_INTERNAL_IPC(operation) \
	fastpg_forbid_internal_ipc((operation), __FILE__, __LINE__)

#else

#define FASTPG_FORBID_INTERNAL_IPC(operation) ((void) 0)

#endif							/* USE_FASTPG */

#endif							/* FASTPG_IPC_GUARD_H */
