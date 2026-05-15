/*-------------------------------------------------------------------------
 *
 * memsmgr.c
 *	  test-only memory storage manager.
 *
 * The normal md.c storage manager is still used for bootstrap and standalone
 * modes so initdb can create a durable seed cluster.  Backends under a
 * postmaster keep new and changed relation blocks in memory, lazily reading
 * unchanged seed-catalog pages from md.c when necessary.
 *
 * Portions Copyright (c) 1996-2026, PostgreSQL Global Development Group
 *
 * IDENTIFICATION
 *	  src/backend/storage/smgr/memsmgr.c
 *
 *-------------------------------------------------------------------------
 */
#include "postgres.h"

#include "access/xlogutils.h"
#include "miscadmin.h"
#include "storage/aio.h"
#include "storage/aio_internal.h"
#include "storage/bufmgr.h"
#include "storage/lwlock.h"
#include "storage/md.h"
#include "storage/memsmgr.h"
#include "storage/relfilelocator.h"
#include "storage/shmem.h"
#include "utils/hsearch.h"
#include "utils/memutils.h"

/*
 * Shared non-temp storage is fixed-size by design: this is a disposable
 * unit-test build, and exhausting the memory budget should fail loudly.  The
 * shared hash is allocated up front, so keep this large enough for long
 * rollback-heavy test runs without making every test cluster reserve an
 * unbounded amount of memory.  Temp storage is backend-local and grows with
 * the backend memory context.
 */
#define MEMSMGR_SHARED_MAX_FORKS	8192
#define MEMSMGR_SHARED_MAX_PAGES	262144
#define MEMSMGR_MAX_COMBINE			32

typedef struct MemSmgrForkKey
{
	RelFileLocatorBackend rlocator;
	ForkNumber	forknum;
} MemSmgrForkKey;

typedef struct MemSmgrPageKey
{
	RelFileLocatorBackend rlocator;
	ForkNumber	forknum;
	BlockNumber blocknum;
} MemSmgrPageKey;

typedef struct MemSmgrForkEntry
{
	MemSmgrForkKey key;
	bool		exists;
	bool		on_disk;
	BlockNumber nblocks;
} MemSmgrForkEntry;

typedef struct MemSmgrPageEntry
{
	MemSmgrPageKey key;
	char		data[BLCKSZ];
} MemSmgrPageEntry;

typedef struct MemSmgrSharedState
{
	LWLock		lock;
	bool		initialized;
} MemSmgrSharedState;

static MemSmgrSharedState *MemSmgrState;
static HTAB *MemSmgrForkHash;
static HTAB *MemSmgrPageHash;

static MemoryContext MemSmgrLocalCxt;
static HTAB *LocalForkHash;
static HTAB *LocalPageHash;

static void MemSmgrShmemRequest(void *arg);
static void MemSmgrShmemInit(void *arg);

const ShmemCallbacks MemSmgrShmemCallbacks = {
	.request_fn = MemSmgrShmemRequest,
	.init_fn = MemSmgrShmemInit,
};

static bool mem_use_md(SMgrRelation reln);
static bool mem_key_is_temp(const RelFileLocatorBackend *rlocator);
static HTAB *mem_fork_hash_for(const RelFileLocatorBackend *rlocator);
static HTAB *mem_page_hash_for(const RelFileLocatorBackend *rlocator);
static LWLock *mem_lock_for(const RelFileLocatorBackend *rlocator);
static void mem_ensure_local_hashes(void);
static void mem_build_fork_key(MemSmgrForkKey *key,
							   RelFileLocatorBackend rlocator,
							   ForkNumber forknum);
static void mem_build_page_key(MemSmgrPageKey *key,
							   RelFileLocatorBackend rlocator,
							   ForkNumber forknum,
							   BlockNumber blocknum);
static MemSmgrForkEntry *mem_get_fork_entry(SMgrRelation reln,
											ForkNumber forknum,
											bool create);
static MemSmgrForkEntry *mem_create_fork_entry(RelFileLocatorBackend rlocator,
											   ForkNumber forknum);
static MemSmgrForkEntry *mem_lookup_fork_entry(RelFileLocatorBackend rlocator,
											   ForkNumber forknum);
static MemSmgrPageEntry *mem_get_page_entry(RelFileLocatorBackend rlocator,
											ForkNumber forknum,
											BlockNumber blocknum,
											bool create);
static void mem_remove_pages(RelFileLocatorBackend rlocator, ForkNumber forknum,
							 BlockNumber first_block);
static void mem_remove_fork(RelFileLocatorBackend rlocator, ForkNumber forknum);
static void mem_readv_locked(SMgrRelation reln, ForkNumber forknum,
							 BlockNumber blocknum, void **buffers,
							 BlockNumber nblocks);
static void mem_writev_locked(SMgrRelation reln, ForkNumber forknum,
							  BlockNumber blocknum, const void **buffers,
							  BlockNumber nblocks);
static void mem_zeroextend_locked(SMgrRelation reln, ForkNumber forknum,
								  BlockNumber blocknum, int nblocks);
static BlockNumber mem_nblocks_locked(SMgrRelation reln, ForkNumber forknum);
static void mem_complete_aio_read(PgAioHandle *ioh, SMgrRelation reln,
								  ForkNumber forknum, BlockNumber blocknum,
								  void **buffers, BlockNumber nblocks);

static void
MemSmgrShmemRequest(void *arg)
{
	ShmemRequestStruct(.name = "MemSmgr State",
					   .size = sizeof(MemSmgrSharedState),
					   .ptr = (void **) &MemSmgrState);

	ShmemRequestHash(.name = "MemSmgr Fork Hash",
					 .nelems = MEMSMGR_SHARED_MAX_FORKS,
					 .ptr = &MemSmgrForkHash,
					 .hash_info.keysize = sizeof(MemSmgrForkKey),
					 .hash_info.entrysize = sizeof(MemSmgrForkEntry),
					 .hash_flags = HASH_ELEM | HASH_BLOBS | HASH_FIXED_SIZE);

	ShmemRequestHash(.name = "MemSmgr Page Hash",
					 .nelems = MEMSMGR_SHARED_MAX_PAGES,
					 .ptr = &MemSmgrPageHash,
					 .hash_info.keysize = sizeof(MemSmgrPageKey),
					 .hash_info.entrysize = sizeof(MemSmgrPageEntry),
					 .hash_flags = HASH_ELEM | HASH_BLOBS | HASH_FIXED_SIZE);
}

static void
MemSmgrShmemInit(void *arg)
{
	if (!MemSmgrState->initialized)
	{
		LWLockInitialize(&MemSmgrState->lock, LWTRANCHE_BUFFER_MAPPING);
		MemSmgrState->initialized = true;
	}
}

void
meminit(void)
{
	mdinit();

	MemSmgrLocalCxt = AllocSetContextCreate(TopMemoryContext,
											"MemSmgr local storage",
											ALLOCSET_DEFAULT_SIZES);
}

void
memshutdown(void)
{
	if (MemSmgrLocalCxt != NULL)
	{
		MemoryContextDelete(MemSmgrLocalCxt);
		MemSmgrLocalCxt = NULL;
		LocalForkHash = NULL;
		LocalPageHash = NULL;
	}
}

void
memopen(SMgrRelation reln)
{
	mdopen(reln);
}

void
memclose(SMgrRelation reln, ForkNumber forknum)
{
	mdclose(reln, forknum);
}

void
memcreate(SMgrRelation reln, ForkNumber forknum, bool isRedo)
{
	LWLock	   *lock;
	MemSmgrForkEntry *entry;

	if (mem_use_md(reln))
	{
		mdcreate(reln, forknum, isRedo);
		return;
	}

	lock = mem_lock_for(&reln->smgr_rlocator);
	if (lock != NULL)
		LWLockAcquire(lock, LW_EXCLUSIVE);

	entry = mem_get_fork_entry(reln, forknum, true);
	if (entry->exists && !isRedo)
	{
		if (lock != NULL)
			LWLockRelease(lock);
		ereport(ERROR,
				(errcode(ERRCODE_DUPLICATE_FILE),
				 errmsg("memory relation fork already exists")));
	}

	entry->exists = true;
	entry->on_disk = false;
	entry->nblocks = 0;
	mem_remove_pages(reln->smgr_rlocator, forknum, 0);

	if (lock != NULL)
		LWLockRelease(lock);
}

bool
memexists(SMgrRelation reln, ForkNumber forknum)
{
	LWLock	   *lock;
	MemSmgrForkEntry *entry;
	bool		exists;

	if (mem_use_md(reln))
		return mdexists(reln, forknum);

	lock = mem_lock_for(&reln->smgr_rlocator);
	if (lock != NULL)
		LWLockAcquire(lock, LW_EXCLUSIVE);

	entry = mem_lookup_fork_entry(reln->smgr_rlocator, forknum);
	if (entry != NULL)
		exists = entry->exists;
	else if (!mem_key_is_temp(&reln->smgr_rlocator) && mdexists(reln, forknum))
	{
		entry = mem_create_fork_entry(reln->smgr_rlocator, forknum);
		entry->exists = true;
		entry->on_disk = true;
		entry->nblocks = mdnblocks(reln, forknum);
		exists = true;
	}
	else
		exists = false;

	if (lock != NULL)
		LWLockRelease(lock);

	return exists;
}

void
memunlink(RelFileLocatorBackend rlocator, ForkNumber forknum, bool isRedo)
{
	LWLock	   *lock;

	if (!IsUnderPostmaster)
	{
		mdunlink(rlocator, forknum, isRedo);
		return;
	}

	lock = mem_lock_for(&rlocator);
	if (lock != NULL)
		LWLockAcquire(lock, LW_EXCLUSIVE);

	if (forknum == InvalidForkNumber)
	{
		for (ForkNumber fork = 0; fork <= MAX_FORKNUM; fork++)
			mem_remove_fork(rlocator, fork);
	}
	else
		mem_remove_fork(rlocator, forknum);

	if (lock != NULL)
		LWLockRelease(lock);
}

void
memextend(SMgrRelation reln, ForkNumber forknum, BlockNumber blocknum,
		  const void *buffer, bool skipFsync)
{
	const void *buffers[1];
	LWLock	   *lock;

	if (mem_use_md(reln))
	{
		mdextend(reln, forknum, blocknum, buffer, skipFsync);
		return;
	}

	buffers[0] = buffer;
	lock = mem_lock_for(&reln->smgr_rlocator);
	if (lock != NULL)
		LWLockAcquire(lock, LW_EXCLUSIVE);

	mem_writev_locked(reln, forknum, blocknum, buffers, 1);

	if (lock != NULL)
		LWLockRelease(lock);
}

void
memzeroextend(SMgrRelation reln, ForkNumber forknum,
			  BlockNumber blocknum, int nblocks, bool skipFsync)
{
	LWLock	   *lock;

	if (mem_use_md(reln))
	{
		mdzeroextend(reln, forknum, blocknum, nblocks, skipFsync);
		return;
	}

	lock = mem_lock_for(&reln->smgr_rlocator);
	if (lock != NULL)
		LWLockAcquire(lock, LW_EXCLUSIVE);

	mem_zeroextend_locked(reln, forknum, blocknum, nblocks);

	if (lock != NULL)
		LWLockRelease(lock);
}

bool
memprefetch(SMgrRelation reln, ForkNumber forknum, BlockNumber blocknum,
			int nblocks)
{
	BlockNumber blocks;
	LWLock	   *lock;

	if (mem_use_md(reln))
		return mdprefetch(reln, forknum, blocknum, nblocks);

	lock = mem_lock_for(&reln->smgr_rlocator);
	if (lock != NULL)
		LWLockAcquire(lock, LW_EXCLUSIVE);

	blocks = mem_nblocks_locked(reln, forknum);

	if (lock != NULL)
		LWLockRelease(lock);

	return blocknum + nblocks <= blocks;
}

uint32
memmaxcombine(SMgrRelation reln, ForkNumber forknum, BlockNumber blocknum)
{
	BlockNumber blocks;
	LWLock	   *lock;
	uint32		ret;

	if (mem_use_md(reln))
		return mdmaxcombine(reln, forknum, blocknum);

	lock = mem_lock_for(&reln->smgr_rlocator);
	if (lock != NULL)
		LWLockAcquire(lock, LW_EXCLUSIVE);

	blocks = mem_nblocks_locked(reln, forknum);
	if (blocknum >= blocks)
		ret = 1;
	else
		ret = (uint32) Min((BlockNumber) MEMSMGR_MAX_COMBINE,
						   blocks - blocknum);

	if (lock != NULL)
		LWLockRelease(lock);

	return ret;
}

void
memreadv(SMgrRelation reln, ForkNumber forknum, BlockNumber blocknum,
		 void **buffers, BlockNumber nblocks)
{
	LWLock	   *lock;

	if (mem_use_md(reln))
	{
		mdreadv(reln, forknum, blocknum, buffers, nblocks);
		return;
	}

	lock = mem_lock_for(&reln->smgr_rlocator);
	if (lock != NULL)
		LWLockAcquire(lock, LW_EXCLUSIVE);

	mem_readv_locked(reln, forknum, blocknum, buffers, nblocks);

	if (lock != NULL)
		LWLockRelease(lock);
}

void
memstartreadv(PgAioHandle *ioh, SMgrRelation reln, ForkNumber forknum,
			  BlockNumber blocknum, void **buffers, BlockNumber nblocks)
{
	if (mem_use_md(reln))
	{
		mdstartreadv(ioh, reln, forknum, blocknum, buffers, nblocks);
		return;
	}

	mem_complete_aio_read(ioh, reln, forknum, blocknum, buffers, nblocks);
}

void
memwritev(SMgrRelation reln, ForkNumber forknum, BlockNumber blocknum,
		  const void **buffers, BlockNumber nblocks, bool skipFsync)
{
	LWLock	   *lock;

	if (mem_use_md(reln))
	{
		mdwritev(reln, forknum, blocknum, buffers, nblocks, skipFsync);
		return;
	}

	lock = mem_lock_for(&reln->smgr_rlocator);
	if (lock != NULL)
		LWLockAcquire(lock, LW_EXCLUSIVE);

	mem_writev_locked(reln, forknum, blocknum, buffers, nblocks);

	if (lock != NULL)
		LWLockRelease(lock);
}

void
memwriteback(SMgrRelation reln, ForkNumber forknum,
			 BlockNumber blocknum, BlockNumber nblocks)
{
	if (mem_use_md(reln))
		mdwriteback(reln, forknum, blocknum, nblocks);
}

BlockNumber
memnblocks(SMgrRelation reln, ForkNumber forknum)
{
	BlockNumber nblocks;
	LWLock	   *lock;

	if (mem_use_md(reln))
		return mdnblocks(reln, forknum);

	lock = mem_lock_for(&reln->smgr_rlocator);
	if (lock != NULL)
		LWLockAcquire(lock, LW_EXCLUSIVE);

	nblocks = mem_nblocks_locked(reln, forknum);

	if (lock != NULL)
		LWLockRelease(lock);

	return nblocks;
}

void
memtruncate(SMgrRelation reln, ForkNumber forknum,
			BlockNumber curnblk, BlockNumber nblocks)
{
	LWLock	   *lock;
	MemSmgrForkEntry *entry;

	if (mem_use_md(reln))
	{
		mdtruncate(reln, forknum, curnblk, nblocks);
		return;
	}

	lock = mem_lock_for(&reln->smgr_rlocator);
	if (lock != NULL)
		LWLockAcquire(lock, LW_EXCLUSIVE);

	entry = mem_get_fork_entry(reln, forknum, true);
	entry->exists = true;
	entry->nblocks = nblocks;
	mem_remove_pages(reln->smgr_rlocator, forknum, nblocks);

	if (lock != NULL)
		LWLockRelease(lock);
}

void
memimmedsync(SMgrRelation reln, ForkNumber forknum)
{
	if (mem_use_md(reln))
		mdimmedsync(reln, forknum);
}

void
memregistersync(SMgrRelation reln, ForkNumber forknum)
{
	if (mem_use_md(reln))
		mdregistersync(reln, forknum);
}

int
memfd(SMgrRelation reln, ForkNumber forknum, BlockNumber blocknum, uint32 *off)
{
	if (mem_use_md(reln))
		return mdfd(reln, forknum, blocknum, off);

	ereport(ERROR,
			(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
			 errmsg("memory storage manager does not expose file descriptors")));

	return -1;
}

#if defined(USE_TEST_EPHEMERAL_BUFFERS) && defined(USE_TEST_MEM_SMGR)
bool
mem_buffer_direct_enabled(SMgrRelation reln)
{
	return reln != NULL &&
		!mem_use_md(reln) &&
		mem_key_is_temp(&reln->smgr_rlocator);
}

Block
mem_buffer_direct_page(SMgrRelation reln, ForkNumber forknum,
					   BlockNumber blocknum, bool create, bool *found)
{
	MemSmgrForkEntry *fork;
	MemSmgrPageEntry *page;

	if (found != NULL)
		*found = false;

	if (!mem_buffer_direct_enabled(reln))
		return NULL;

	fork = mem_get_fork_entry(reln, forknum, false);
	if (fork == NULL || !fork->exists || blocknum >= fork->nblocks)
		return NULL;

	page = mem_get_page_entry(reln->smgr_rlocator, forknum, blocknum, create);
	if (page == NULL)
		return NULL;

	if (found != NULL)
		*found = true;
	return (Block) page->data;
}
#endif

static bool
mem_use_md(SMgrRelation reln)
{
	return !IsUnderPostmaster;
}

static bool
mem_key_is_temp(const RelFileLocatorBackend *rlocator)
{
	return RelFileLocatorBackendIsTemp(*rlocator);
}

static HTAB *
mem_fork_hash_for(const RelFileLocatorBackend *rlocator)
{
	if (mem_key_is_temp(rlocator))
	{
		mem_ensure_local_hashes();
		return LocalForkHash;
	}
	return MemSmgrForkHash;
}

static HTAB *
mem_page_hash_for(const RelFileLocatorBackend *rlocator)
{
	if (mem_key_is_temp(rlocator))
	{
		mem_ensure_local_hashes();
		return LocalPageHash;
	}
	return MemSmgrPageHash;
}

static LWLock *
mem_lock_for(const RelFileLocatorBackend *rlocator)
{
	if (mem_key_is_temp(rlocator))
		return NULL;

	if (MemSmgrState == NULL)
		elog(ERROR, "memory storage manager shared state is not initialized");

	return &MemSmgrState->lock;
}

static void
mem_ensure_local_hashes(void)
{
	HASHCTL		ctl;
	MemoryContext oldcxt;

	if (LocalForkHash != NULL)
		return;

	if (MemSmgrLocalCxt == NULL)
		MemSmgrLocalCxt = AllocSetContextCreate(TopMemoryContext,
												"MemSmgr local storage",
												ALLOCSET_DEFAULT_SIZES);

	oldcxt = MemoryContextSwitchTo(MemSmgrLocalCxt);

	ctl.keysize = sizeof(MemSmgrForkKey);
	ctl.entrysize = sizeof(MemSmgrForkEntry);
	LocalForkHash = hash_create("local memory storage forks", 128,
								&ctl, HASH_ELEM | HASH_BLOBS);

	ctl.keysize = sizeof(MemSmgrPageKey);
	ctl.entrysize = sizeof(MemSmgrPageEntry);
	LocalPageHash = hash_create("local memory storage pages", 1024,
								&ctl, HASH_ELEM | HASH_BLOBS);

	MemoryContextSwitchTo(oldcxt);
}

static void
mem_build_fork_key(MemSmgrForkKey *key, RelFileLocatorBackend rlocator,
				   ForkNumber forknum)
{
	memset(key, 0, sizeof(*key));
	key->rlocator = rlocator;
	key->forknum = forknum;
}

static void
mem_build_page_key(MemSmgrPageKey *key, RelFileLocatorBackend rlocator,
				   ForkNumber forknum, BlockNumber blocknum)
{
	memset(key, 0, sizeof(*key));
	key->rlocator = rlocator;
	key->forknum = forknum;
	key->blocknum = blocknum;
}

static MemSmgrForkEntry *
mem_lookup_fork_entry(RelFileLocatorBackend rlocator, ForkNumber forknum)
{
	MemSmgrForkKey key;

	mem_build_fork_key(&key, rlocator, forknum);
	return hash_search(mem_fork_hash_for(&rlocator), &key, HASH_FIND, NULL);
}

static MemSmgrForkEntry *
mem_create_fork_entry(RelFileLocatorBackend rlocator, ForkNumber forknum)
{
	MemSmgrForkKey key;
	MemSmgrForkEntry *entry;
	bool		found;

	mem_build_fork_key(&key, rlocator, forknum);
	entry = hash_search(mem_fork_hash_for(&rlocator), &key, HASH_ENTER_NULL,
						&found);
	if (entry == NULL)
		ereport(ERROR,
				(errcode(ERRCODE_OUT_OF_MEMORY),
				 errmsg("memory storage manager fork table is full")));

	if (!found)
	{
		entry->exists = false;
		entry->on_disk = false;
		entry->nblocks = 0;
	}

	return entry;
}

static MemSmgrForkEntry *
mem_get_fork_entry(SMgrRelation reln, ForkNumber forknum, bool create)
{
	MemSmgrForkEntry *entry;

	entry = mem_lookup_fork_entry(reln->smgr_rlocator, forknum);
	if (entry != NULL)
		return entry;

	if (!create)
		return NULL;

	entry = mem_create_fork_entry(reln->smgr_rlocator, forknum);

	if (!mem_key_is_temp(&reln->smgr_rlocator) && mdexists(reln, forknum))
	{
		entry->exists = true;
		entry->on_disk = true;
		entry->nblocks = mdnblocks(reln, forknum);
	}

	return entry;
}

static MemSmgrPageEntry *
mem_get_page_entry(RelFileLocatorBackend rlocator, ForkNumber forknum,
				   BlockNumber blocknum, bool create)
{
	MemSmgrPageKey key;
	MemSmgrPageEntry *entry;
	bool		found;

	mem_build_page_key(&key, rlocator, forknum, blocknum);
	entry = hash_search(mem_page_hash_for(&rlocator), &key,
						create ? HASH_ENTER_NULL : HASH_FIND, &found);

	if (entry == NULL && create)
		ereport(ERROR,
				(errcode(ERRCODE_OUT_OF_MEMORY),
				 errmsg("memory storage manager page table is full")));

	if (entry != NULL && create && !found)
		memset(entry->data, 0, BLCKSZ);

	return entry;
}

static void
mem_remove_pages(RelFileLocatorBackend rlocator, ForkNumber forknum,
				 BlockNumber first_block)
{
	HTAB	   *hash = mem_page_hash_for(&rlocator);
	HASH_SEQ_STATUS status;
	MemSmgrPageEntry *entry;

	hash_seq_init(&status, hash);
	while ((entry = hash_seq_search(&status)) != NULL)
	{
		if (RelFileLocatorBackendEquals(entry->key.rlocator, rlocator) &&
			entry->key.forknum == forknum &&
			entry->key.blocknum >= first_block)
		{
			MemSmgrPageKey key = entry->key;

			hash_search(hash, &key, HASH_REMOVE, NULL);
		}
	}
}

static void
mem_remove_fork(RelFileLocatorBackend rlocator, ForkNumber forknum)
{
	MemSmgrForkEntry *entry;

	entry = mem_create_fork_entry(rlocator, forknum);
	entry->exists = false;
	entry->on_disk = false;
	entry->nblocks = 0;

	mem_remove_pages(rlocator, forknum, 0);
}

static void
mem_readv_locked(SMgrRelation reln, ForkNumber forknum, BlockNumber blocknum,
				 void **buffers, BlockNumber nblocks)
{
	MemSmgrForkEntry *fork;

	fork = mem_get_fork_entry(reln, forknum, true);
	if (fork == NULL || !fork->exists || blocknum + nblocks > fork->nblocks)
		ereport(ERROR,
				(errcode(ERRCODE_DATA_CORRUPTED),
				 errmsg("could not read blocks %u..%u in memory relation fork",
						blocknum, blocknum + nblocks - 1)));

	for (BlockNumber i = 0; i < nblocks; i++)
	{
		BlockNumber curblock = blocknum + i;
		MemSmgrPageEntry *page;

		page = mem_get_page_entry(reln->smgr_rlocator, forknum, curblock,
								  false);
		if (page != NULL)
			memcpy(buffers[i], page->data, BLCKSZ);
		else if (fork->on_disk)
			mdreadv(reln, forknum, curblock, &buffers[i], 1);
		else
			memset(buffers[i], 0, BLCKSZ);
	}
}

static void
mem_writev_locked(SMgrRelation reln, ForkNumber forknum, BlockNumber blocknum,
				  const void **buffers, BlockNumber nblocks)
{
	MemSmgrForkEntry *fork;

	fork = mem_get_fork_entry(reln, forknum, true);
	fork->exists = true;

	if (blocknum > fork->nblocks)
		mem_zeroextend_locked(reln, forknum, fork->nblocks,
							  blocknum - fork->nblocks);

	for (BlockNumber i = 0; i < nblocks; i++)
	{
		MemSmgrPageEntry *page;

		page = mem_get_page_entry(reln->smgr_rlocator, forknum, blocknum + i,
								  true);
		memcpy(page->data, buffers[i], BLCKSZ);
	}

	if (blocknum + nblocks > fork->nblocks)
		fork->nblocks = blocknum + nblocks;
}

static void
mem_zeroextend_locked(SMgrRelation reln, ForkNumber forknum,
					  BlockNumber blocknum, int nblocks)
{
	MemSmgrForkEntry *fork;

	fork = mem_get_fork_entry(reln, forknum, true);
	fork->exists = true;

	for (int i = 0; i < nblocks; i++)
	{
		MemSmgrPageEntry *page;

		page = mem_get_page_entry(reln->smgr_rlocator, forknum,
								  blocknum + i, true);
		memset(page->data, 0, BLCKSZ);
	}

	if (blocknum + nblocks > fork->nblocks)
		fork->nblocks = blocknum + nblocks;
}

static BlockNumber
mem_nblocks_locked(SMgrRelation reln, ForkNumber forknum)
{
	MemSmgrForkEntry *fork;

	fork = mem_get_fork_entry(reln, forknum, true);
	if (!fork->exists)
		return 0;

	return fork->nblocks;
}

static void
mem_complete_aio_read(PgAioHandle *ioh, SMgrRelation reln, ForkNumber forknum,
					  BlockNumber blocknum, void **buffers, BlockNumber nblocks)
{
	LWLock	   *lock;

	Assert(ioh->state == PGAIO_HS_HANDED_OUT);
	Assert(pgaio_my_backend->handed_out_io == ioh);

	pgaio_io_set_target_smgr(ioh, reln, forknum, blocknum, nblocks, false);
	pgaio_io_register_callbacks(ioh, PGAIO_HCB_MD_READV, 0);

	HOLD_INTERRUPTS();

	ioh->op = PGAIO_OP_READV;
	ioh->result = 0;
	pg_write_barrier();
	ioh->state = PGAIO_HS_DEFINED;
	pgaio_my_backend->handed_out_io = NULL;

	pgaio_io_call_stage(ioh);
	pg_write_barrier();
	ioh->state = PGAIO_HS_STAGED;

	lock = mem_lock_for(&reln->smgr_rlocator);
	if (lock != NULL)
		LWLockAcquire(lock, LW_EXCLUSIVE);

	mem_readv_locked(reln, forknum, blocknum, buffers, nblocks);

	if (lock != NULL)
		LWLockRelease(lock);

	pgaio_io_prepare_submit(ioh);

	START_CRIT_SECTION();
	pgaio_io_process_completion(ioh, nblocks * BLCKSZ);
	END_CRIT_SECTION();

	RESUME_INTERRUPTS();
}
