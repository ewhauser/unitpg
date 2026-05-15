/*-------------------------------------------------------------------------
 *
 * memsmgr.h
 *	  test-only memory storage manager declarations.
 *
 * Portions Copyright (c) 1996-2026, PostgreSQL Global Development Group
 *
 * src/include/storage/memsmgr.h
 *
 *-------------------------------------------------------------------------
 */
#ifndef MEMSMGR_H
#define MEMSMGR_H

#include "storage/aio_types.h"
#include "storage/block.h"
#include "storage/relfilelocator.h"
#include "storage/shmem.h"
#include "storage/smgr.h"

extern PGDLLIMPORT const ShmemCallbacks MemSmgrShmemCallbacks;

extern void meminit(void);
extern void memshutdown(void);
extern void memopen(SMgrRelation reln);
extern void memclose(SMgrRelation reln, ForkNumber forknum);
extern void memcreate(SMgrRelation reln, ForkNumber forknum, bool isRedo);
extern bool memexists(SMgrRelation reln, ForkNumber forknum);
extern void memunlink(RelFileLocatorBackend rlocator, ForkNumber forknum, bool isRedo);
extern void memextend(SMgrRelation reln, ForkNumber forknum,
					  BlockNumber blocknum, const void *buffer, bool skipFsync);
extern void memzeroextend(SMgrRelation reln, ForkNumber forknum,
						  BlockNumber blocknum, int nblocks, bool skipFsync);
extern bool memprefetch(SMgrRelation reln, ForkNumber forknum,
						BlockNumber blocknum, int nblocks);
extern uint32 memmaxcombine(SMgrRelation reln, ForkNumber forknum,
							BlockNumber blocknum);
extern void memreadv(SMgrRelation reln, ForkNumber forknum, BlockNumber blocknum,
					 void **buffers, BlockNumber nblocks);
extern void memstartreadv(PgAioHandle *ioh,
						  SMgrRelation reln, ForkNumber forknum, BlockNumber blocknum,
						  void **buffers, BlockNumber nblocks);
extern void memwritev(SMgrRelation reln, ForkNumber forknum,
					  BlockNumber blocknum,
					  const void **buffers, BlockNumber nblocks, bool skipFsync);
extern void memwriteback(SMgrRelation reln, ForkNumber forknum,
						 BlockNumber blocknum, BlockNumber nblocks);
extern BlockNumber memnblocks(SMgrRelation reln, ForkNumber forknum);
extern void memtruncate(SMgrRelation reln, ForkNumber forknum,
						BlockNumber curnblk, BlockNumber nblocks);
extern void memimmedsync(SMgrRelation reln, ForkNumber forknum);
extern void memregistersync(SMgrRelation reln, ForkNumber forknum);
extern int	memfd(SMgrRelation reln, ForkNumber forknum, BlockNumber blocknum,
				  uint32 *off);

#endif							/* MEMSMGR_H */
