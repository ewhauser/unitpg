/*-------------------------------------------------------------------------
 *
 * memsmgr.c
 *	  test-only memory storage manager.
 *
 * The normal md.c storage manager is still used for bootstrap and standalone
 * modes so initdb can create a durable seed cluster.  Backends under a
 * postmaster keep new and changed relation blocks in memory, lazily reading
 * unchanged seed pages from the seed backing layer when necessary.
 *
 * Portions Copyright (c) 1996-2026, PostgreSQL Global Development Group
 *
 * IDENTIFICATION
 *	  src/backend/storage/smgr/memsmgr.c
 *
 *-------------------------------------------------------------------------
 */
#include "postgres.h"

#include <fcntl.h>
#include <sys/mman.h>
#include <sys/stat.h>

#include "access/transam.h"
#include "access/xact.h"
#include "access/xlogutils.h"
#include "commands/sequence.h"
#include "common/relpath.h"
#include "fmgr.h"
#include "miscadmin.h"
#include "portability/mem.h"
#include "storage/aio.h"
#include "storage/aio_internal.h"
#include "storage/bufmgr.h"
#include "storage/fd.h"
#include "storage/lwlock.h"
#include "storage/md.h"
#include "storage/memsmgr.h"
#include "storage/procarray.h"
#include "storage/relfilelocator.h"
#include "storage/shmem.h"
#include "utils/hsearch.h"
#include "utils/builtins.h"
#include "utils/inval.h"
#include "utils/memutils.h"
#include "utils/wait_event.h"

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
	uint32		page_index;
} MemSmgrPageEntry;

typedef struct MemSmgrSnapshotPageEntry
{
	MemSmgrPageKey key;
	char		data[BLCKSZ];
} MemSmgrSnapshotPageEntry;

typedef struct MemSmgrSharedState
{
	LWLock		lock;
	bool		initialized;
	uint32		next_page;
	char	   *page_data;
} MemSmgrSharedState;

#ifdef USE_TEST_MEM_SMGR
typedef struct MemSmgrSnapshot
{
	char	   *name;
	int			nforks;
	MemSmgrForkEntry *forks;
	int			npages;
	MemSmgrSnapshotPageEntry *pages;
	Oid			next_oid;
	uint32		oid_count;
	MemoryContext context;
	struct MemSmgrSnapshot *next;
} MemSmgrSnapshot;
#endif

static MemSmgrSharedState *MemSmgrState;
static HTAB *MemSmgrForkHash;
static HTAB *MemSmgrPageHash;

static MemoryContext MemSmgrLocalCxt;
static HTAB *LocalForkHash;
static HTAB *LocalPageHash;

#ifdef USE_TEST_MEM_SMGR
static MemoryContext MemSmgrSnapshotCxt;
static MemSmgrSnapshot *MemSmgrSnapshots;
#endif

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
static char *mem_page_data(const MemSmgrPageEntry *entry);
static uint32 mem_alloc_page_index(void);
static void mem_remove_pages(RelFileLocatorBackend rlocator, ForkNumber forknum,
							 BlockNumber first_block);
static bool mem_relation_is_memory_only(RelFileLocatorBackend rlocator);
static void mem_remove_memory_only_fork(RelFileLocatorBackend rlocator,
										ForkNumber forknum);
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
static const char *mem_seed_root(void);
static bool mem_seed_active_for(const RelFileLocatorBackend *rlocator);
static char *mem_seed_segment_path(RelFileLocatorBackend rlocator,
								   ForkNumber forknum, BlockNumber segno);
static bool mem_seed_segment_stat(RelFileLocatorBackend rlocator,
								  ForkNumber forknum, BlockNumber segno,
								  struct stat *statbuf);
static bool mem_seed_exists(RelFileLocatorBackend rlocator, ForkNumber forknum);
static BlockNumber mem_seed_nblocks(RelFileLocatorBackend rlocator,
									ForkNumber forknum);
static void mem_seed_read_block(RelFileLocatorBackend rlocator,
								ForkNumber forknum, BlockNumber blocknum,
								void *buffer);
#ifdef USE_TEST_MEM_SMGR
static void mem_require_snapshot_allowed(const char *function_name);
static bool mem_snapshot_key_matches(const RelFileLocatorBackend *rlocator);
static MemSmgrSnapshot *mem_find_snapshot(const char *name);
static void mem_delete_snapshot(MemSmgrSnapshot *snapshot);
static MemSmgrSnapshot *mem_create_snapshot(const char *name);
static void mem_capture_snapshot(MemSmgrSnapshot *snapshot);
static void mem_restore_snapshot(MemSmgrSnapshot *snapshot);
static void mem_restore_reset_caches(void);
#endif

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
		Size		map_size;
		void	   *map;

		LWLockInitialize(&MemSmgrState->lock, LWTRANCHE_BUFFER_MAPPING);
		MemSmgrState->next_page = 0;

		/*
		 * Keep page contents out of PostgreSQL's fixed-size shared hash.
		 * Reserving the address space here is cheap on overcommitting
		 * platforms; physical pages are only faulted in as tests write data.
		 */
		map_size = mul_size(MEMSMGR_SHARED_MAX_PAGES, BLCKSZ);
		map = mmap(NULL, map_size, PROT_READ | PROT_WRITE,
				   MAP_SHARED | MAP_ANONYMOUS | MAP_HASSEMAPHORE, -1, 0);
		if (map == MAP_FAILED)
			ereport(FATAL,
					(errcode(ERRCODE_OUT_OF_MEMORY),
					 errmsg("could not allocate memory storage manager page arena: %m")));

		MemSmgrState->page_data = (char *) map;
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
	else if (mem_seed_active_for(&reln->smgr_rlocator) &&
			 mem_seed_exists(reln->smgr_rlocator, forknum))
	{
		entry = mem_create_fork_entry(reln->smgr_rlocator, forknum);
		entry->exists = true;
		entry->on_disk = true;
		entry->nblocks = mem_seed_nblocks(reln->smgr_rlocator, forknum);
		exists = true;
	}
	else if (!mem_seed_active_for(&reln->smgr_rlocator) &&
			 !mem_key_is_temp(&reln->smgr_rlocator) &&
			 mdexists(reln, forknum))
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
		if (mem_relation_is_memory_only(rlocator))
		{
			for (ForkNumber fork = 0; fork <= MAX_FORKNUM; fork++)
				mem_remove_memory_only_fork(rlocator, fork);
		}
		else
		{
			for (ForkNumber fork = 0; fork <= MAX_FORKNUM; fork++)
				mem_remove_fork(rlocator, fork);
		}
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

Datum
pg_fastfork_snapshot(PG_FUNCTION_ARGS)
{
#ifndef USE_TEST_MEM_SMGR
	ereport(ERROR,
			(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
			 errmsg("pg_fastfork_snapshot() requires --enable-test-mem-smgr")));
	PG_RETURN_VOID();
#else
	char	   *name = text_to_cstring(PG_GETARG_TEXT_PP(0));
	MemSmgrSnapshot *snapshot;
	MemSmgrSnapshot *existing;

	mem_require_snapshot_allowed("pg_fastfork_snapshot");

	if (name[0] == '\0')
		ereport(ERROR,
				(errcode(ERRCODE_INVALID_PARAMETER_VALUE),
				 errmsg("fast-fork snapshot name must not be empty")));

	existing = mem_find_snapshot(name);
	if (existing != NULL)
		mem_delete_snapshot(existing);

	FlushDatabaseBuffers(InvalidOid);
	FlushDatabaseBuffers(MyDatabaseId);

	snapshot = mem_create_snapshot(name);
	mem_capture_snapshot(snapshot);

	pfree(name);
	PG_RETURN_VOID();
#endif
}

Datum
pg_fastfork_restore(PG_FUNCTION_ARGS)
{
#ifndef USE_TEST_MEM_SMGR
	ereport(ERROR,
			(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
			 errmsg("pg_fastfork_restore() requires --enable-test-mem-smgr")));
	PG_RETURN_VOID();
#else
	char	   *name = text_to_cstring(PG_GETARG_TEXT_PP(0));
	MemSmgrSnapshot *snapshot;

	mem_require_snapshot_allowed("pg_fastfork_restore");

	snapshot = mem_find_snapshot(name);
	if (snapshot == NULL)
		ereport(ERROR,
				(errcode(ERRCODE_UNDEFINED_OBJECT),
				 errmsg("fast-fork snapshot \"%s\" does not exist", name)));

	DropDatabaseBuffers(InvalidOid);
	DropDatabaseBuffers(MyDatabaseId);
	mem_restore_snapshot(snapshot);
	mem_restore_reset_caches();

	pfree(name);
	PG_RETURN_VOID();
#endif
}

Datum
pg_fastfork_drop_snapshot(PG_FUNCTION_ARGS)
{
#ifndef USE_TEST_MEM_SMGR
	ereport(ERROR,
			(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
			 errmsg("pg_fastfork_drop_snapshot() requires --enable-test-mem-smgr")));
	PG_RETURN_VOID();
#else
	char	   *name = text_to_cstring(PG_GETARG_TEXT_PP(0));
	MemSmgrSnapshot *snapshot;

	mem_require_snapshot_allowed("pg_fastfork_drop_snapshot");

	snapshot = mem_find_snapshot(name);
	if (snapshot != NULL)
		mem_delete_snapshot(snapshot);

	pfree(name);
	PG_RETURN_VOID();
#endif
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
	return (Block) mem_page_data(page);
}
#endif

#ifdef USE_TEST_MEM_SMGR
static void
mem_require_snapshot_allowed(const char *function_name)
{
	int			notherbackends;
	int			npreparedxacts;

	if (!superuser())
		ereport(ERROR,
				(errcode(ERRCODE_INSUFFICIENT_PRIVILEGE),
				 errmsg("must be superuser to call %s()", function_name)));

	if (!IsUnderPostmaster || MemSmgrState == NULL ||
		MemSmgrForkHash == NULL || MemSmgrPageHash == NULL)
		ereport(ERROR,
				(errcode(ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE),
				 errmsg("%s() requires the in-memory storage manager",
						function_name)));

	if (!OidIsValid(MyDatabaseId))
		ereport(ERROR,
				(errcode(ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE),
				 errmsg("%s() requires a database connection", function_name)));

	if (IsTransactionBlock())
		ereport(ERROR,
				(errcode(ERRCODE_ACTIVE_SQL_TRANSACTION),
				 errmsg("%s() cannot run inside a transaction block",
						function_name)));

	if (CountOtherDBBackends(MyDatabaseId, &notherbackends, &npreparedxacts))
		ereport(ERROR,
				(errcode(ERRCODE_OBJECT_IN_USE),
				 errmsg("database is being accessed by other users"),
				 errdetail("%d other session(s) and %d prepared transaction(s) are using the database.",
						   notherbackends, npreparedxacts)));
}

static bool
mem_snapshot_key_matches(const RelFileLocatorBackend *rlocator)
{
	if (mem_key_is_temp(rlocator))
		return false;

	return rlocator->locator.dbOid == MyDatabaseId ||
		rlocator->locator.dbOid == InvalidOid;
}

static MemSmgrSnapshot *
mem_find_snapshot(const char *name)
{
	MemSmgrSnapshot *snapshot;

	for (snapshot = MemSmgrSnapshots; snapshot != NULL; snapshot = snapshot->next)
	{
		if (strcmp(snapshot->name, name) == 0)
			return snapshot;
	}

	return NULL;
}

static void
mem_delete_snapshot(MemSmgrSnapshot *snapshot)
{
	MemSmgrSnapshot **link;

	for (link = &MemSmgrSnapshots; *link != NULL; link = &(*link)->next)
	{
		if (*link == snapshot)
		{
			*link = snapshot->next;
			MemoryContextDelete(snapshot->context);
			return;
		}
	}
}

static MemSmgrSnapshot *
mem_create_snapshot(const char *name)
{
	MemoryContext context;
	MemoryContext oldcontext;
	MemSmgrSnapshot *snapshot;

	if (MemSmgrSnapshotCxt == NULL)
		MemSmgrSnapshotCxt = AllocSetContextCreate(TopMemoryContext,
												   "MemSmgr snapshots",
												   ALLOCSET_DEFAULT_SIZES);

	context = AllocSetContextCreate(MemSmgrSnapshotCxt,
									"MemSmgr snapshot",
									ALLOCSET_DEFAULT_SIZES);
	oldcontext = MemoryContextSwitchTo(context);

	snapshot = palloc0(sizeof(MemSmgrSnapshot));
	snapshot->context = context;
	snapshot->name = pstrdup(name);

#ifdef USE_TEST_MEM_SMGR
	TestFastForkGetOidState(&snapshot->next_oid, &snapshot->oid_count);
#endif

	MemoryContextSwitchTo(oldcontext);

	snapshot->next = MemSmgrSnapshots;
	MemSmgrSnapshots = snapshot;
	return snapshot;
}

static void
mem_capture_snapshot(MemSmgrSnapshot *snapshot)
{
	HASH_SEQ_STATUS status;
	MemSmgrForkEntry *fork;
	MemSmgrPageEntry *page;
	MemoryContext oldcontext;
	int			nforks = 0;
	int			npages = 0;
	int			i;

	LWLockAcquire(&MemSmgrState->lock, LW_EXCLUSIVE);

	hash_seq_init(&status, MemSmgrForkHash);
	while ((fork = hash_seq_search(&status)) != NULL)
	{
		if (mem_snapshot_key_matches(&fork->key.rlocator))
			nforks++;
	}

	hash_seq_init(&status, MemSmgrPageHash);
	while ((page = hash_seq_search(&status)) != NULL)
	{
		if (mem_snapshot_key_matches(&page->key.rlocator))
			npages++;
	}

	LWLockRelease(&MemSmgrState->lock);

	oldcontext = MemoryContextSwitchTo(snapshot->context);
	snapshot->nforks = nforks;
	snapshot->npages = npages;
	if (nforks > 0)
		snapshot->forks = palloc(sizeof(MemSmgrForkEntry) * nforks);
	if (npages > 0)
		snapshot->pages = palloc(sizeof(MemSmgrSnapshotPageEntry) * npages);
	MemoryContextSwitchTo(oldcontext);

	LWLockAcquire(&MemSmgrState->lock, LW_EXCLUSIVE);

	i = 0;
	hash_seq_init(&status, MemSmgrForkHash);
	while ((fork = hash_seq_search(&status)) != NULL)
	{
		if (mem_snapshot_key_matches(&fork->key.rlocator))
		{
			if (i < nforks)
				snapshot->forks[i++] = *fork;
		}
	}
	snapshot->nforks = i;

	i = 0;
	hash_seq_init(&status, MemSmgrPageHash);
	while ((page = hash_seq_search(&status)) != NULL)
	{
		if (mem_snapshot_key_matches(&page->key.rlocator))
		{
			if (i < npages)
			{
				snapshot->pages[i].key = page->key;
				memcpy(snapshot->pages[i].data, mem_page_data(page), BLCKSZ);
				i++;
			}
		}
	}
	snapshot->npages = i;

	LWLockRelease(&MemSmgrState->lock);
}

static void
mem_restore_snapshot(MemSmgrSnapshot *snapshot)
{
	HASH_SEQ_STATUS status;
	MemSmgrForkEntry *fork;
	MemSmgrPageEntry *page;

	LWLockAcquire(&MemSmgrState->lock, LW_EXCLUSIVE);

	hash_seq_init(&status, MemSmgrForkHash);
	while ((fork = hash_seq_search(&status)) != NULL)
	{
		if (mem_snapshot_key_matches(&fork->key.rlocator))
			hash_search(MemSmgrForkHash, &fork->key, HASH_REMOVE, NULL);
	}

	hash_seq_init(&status, MemSmgrPageHash);
	while ((page = hash_seq_search(&status)) != NULL)
	{
		if (mem_snapshot_key_matches(&page->key.rlocator))
			hash_search(MemSmgrPageHash, &page->key, HASH_REMOVE, NULL);
	}

	for (int i = 0; i < snapshot->nforks; i++)
	{
		bool		found;
		MemSmgrForkEntry *target;

		target = hash_search(MemSmgrForkHash, &snapshot->forks[i].key,
							 HASH_ENTER_NULL, &found);
		if (target == NULL)
			ereport(ERROR,
					(errcode(ERRCODE_OUT_OF_MEMORY),
					 errmsg("memory storage manager fork table is full")));
		*target = snapshot->forks[i];
	}

	for (int i = 0; i < snapshot->npages; i++)
	{
		bool		found;
		MemSmgrPageEntry *target;

		target = hash_search(MemSmgrPageHash, &snapshot->pages[i].key,
							 HASH_ENTER_NULL, &found);
		if (target == NULL)
			ereport(ERROR,
					(errcode(ERRCODE_OUT_OF_MEMORY),
					 errmsg("memory storage manager page table is full")));
		if (!found)
			target->page_index = mem_alloc_page_index();
		memcpy(mem_page_data(target), snapshot->pages[i].data, BLCKSZ);
	}

	LWLockRelease(&MemSmgrState->lock);

#ifdef USE_TEST_MEM_SMGR
	TestFastForkSetOidState(snapshot->next_oid, snapshot->oid_count);
#endif
}

static void
mem_restore_reset_caches(void)
{
	smgrreleaseall();
	InvalidateSystemCaches();
	ResetSequenceCaches();
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

static const char *
mem_seed_root(void)
{
#ifdef USE_TEST_NO_DATA_DIRECTORY_STARTUP
	const char *seed_root;

	if (!IsUnderPostmaster)
		return NULL;

	seed_root = getenv("PG_FASTFORK_SEED_DIR");
	if (seed_root == NULL || seed_root[0] == '\0')
		return NULL;

	return seed_root;
#else
	return NULL;
#endif
}

static bool
mem_seed_active_for(const RelFileLocatorBackend *rlocator)
{
	return !mem_key_is_temp(rlocator) && mem_seed_root() != NULL;
}

static char *
mem_seed_segment_path(RelFileLocatorBackend rlocator, ForkNumber forknum,
					  BlockNumber segno)
{
	const char *seed_root = mem_seed_root();
	RelPathStr	path;

	Assert(seed_root != NULL);

	path = relpath(rlocator, forknum);
	if (segno > 0)
		return psprintf("%s/%s.%u", seed_root, path.str, segno);

	return psprintf("%s/%s", seed_root, path.str);
}

static bool
mem_seed_segment_stat(RelFileLocatorBackend rlocator, ForkNumber forknum,
					  BlockNumber segno, struct stat *statbuf)
{
	char	   *path;
	int			rc;
	int			save_errno;

	path = mem_seed_segment_path(rlocator, forknum, segno);
	rc = stat(path, statbuf);
	save_errno = errno;
	pfree(path);

	if (rc == 0)
		return true;

	errno = save_errno;
	if (errno == ENOENT)
		return false;

	ereport(ERROR,
			(errcode_for_file_access(),
			 errmsg("could not stat fast-fork seed relation segment: %m")));
	return false;
}

static bool
mem_seed_exists(RelFileLocatorBackend rlocator, ForkNumber forknum)
{
	struct stat statbuf;

	if (!mem_seed_active_for(&rlocator))
		return false;

	return mem_seed_segment_stat(rlocator, forknum, 0, &statbuf);
}

static BlockNumber
mem_seed_nblocks(RelFileLocatorBackend rlocator, ForkNumber forknum)
{
	BlockNumber total_blocks = 0;

	for (BlockNumber segno = 0;; segno++)
	{
		struct stat statbuf;
		BlockNumber segment_blocks;

		if (!mem_seed_segment_stat(rlocator, forknum, segno, &statbuf))
		{
			if (segno == 0)
				return 0;
			break;
		}

		if (statbuf.st_size % BLCKSZ != 0)
			ereport(ERROR,
					(errcode(ERRCODE_DATA_CORRUPTED),
					 errmsg("fast-fork seed relation segment has partial block")));

		segment_blocks = (BlockNumber) (statbuf.st_size / BLCKSZ);
		if (segment_blocks > (BlockNumber) RELSEG_SIZE)
			ereport(ERROR,
					(errcode(ERRCODE_DATA_CORRUPTED),
					 errmsg("fast-fork seed relation segment is too large")));

		total_blocks += segment_blocks;
		if (segment_blocks < (BlockNumber) RELSEG_SIZE)
			break;
	}

	return total_blocks;
}

static void
mem_seed_read_block(RelFileLocatorBackend rlocator, ForkNumber forknum,
					BlockNumber blocknum, void *buffer)
{
	BlockNumber segno = blocknum / ((BlockNumber) RELSEG_SIZE);
	BlockNumber segoff = blocknum % ((BlockNumber) RELSEG_SIZE);
	pgoff_t		offset = (pgoff_t) BLCKSZ * segoff;
	char	   *path;
	File		file;
	ssize_t		nread;

	path = mem_seed_segment_path(rlocator, forknum, segno);
	file = PathNameOpenFile(path, O_RDONLY | PG_BINARY);
	if (file < 0)
	{
		ereport(ERROR,
				(errcode_for_file_access(),
				 errmsg("could not open fast-fork seed relation segment \"%s\": %m",
						path)));
	}

	nread = FileRead(file, buffer, BLCKSZ, offset, WAIT_EVENT_DATA_FILE_READ);
	FileClose(file);

	if (nread != BLCKSZ)
	{
		if (nread < 0)
			ereport(ERROR,
					(errcode_for_file_access(),
					 errmsg("could not read fast-fork seed relation segment \"%s\": %m",
							path)));
		ereport(ERROR,
				(errcode(ERRCODE_DATA_CORRUPTED),
				 errmsg("could not read block %u from fast-fork seed relation segment \"%s\": read only %zd of %d bytes",
						blocknum, path, nread, BLCKSZ)));
	}

	pfree(path);
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

	if (mem_seed_active_for(&reln->smgr_rlocator) &&
		mem_seed_exists(reln->smgr_rlocator, forknum))
	{
		entry->exists = true;
		entry->on_disk = true;
		entry->nblocks = mem_seed_nblocks(reln->smgr_rlocator, forknum);
	}
	else if (!mem_seed_active_for(&reln->smgr_rlocator) &&
			 !mem_key_is_temp(&reln->smgr_rlocator) &&
			 mdexists(reln, forknum))
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
	{
		entry->page_index = mem_alloc_page_index();
		memset(mem_page_data(entry), 0, BLCKSZ);
	}

	return entry;
}

static char *
mem_page_data(const MemSmgrPageEntry *entry)
{
	return MemSmgrState->page_data + ((Size) entry->page_index * BLCKSZ);
}

static uint32
mem_alloc_page_index(void)
{
	uint32		page_index;

	if (MemSmgrState == NULL || MemSmgrState->page_data == NULL)
		elog(ERROR, "memory storage manager page arena is not initialized");

	page_index = MemSmgrState->next_page++;
	if (page_index >= MEMSMGR_SHARED_MAX_PAGES)
		ereport(ERROR,
				(errcode(ERRCODE_OUT_OF_MEMORY),
				 errmsg("memory storage manager page arena is full")));

	return page_index;
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

static bool
mem_relation_is_memory_only(RelFileLocatorBackend rlocator)
{
	bool		saw_entry = false;

	for (ForkNumber fork = 0; fork <= MAX_FORKNUM; fork++)
	{
		MemSmgrForkEntry *entry;

		entry = mem_lookup_fork_entry(rlocator, fork);
		if (entry == NULL)
			continue;

		saw_entry = true;
		if (entry->on_disk)
			return false;
	}

	return saw_entry;
}

static void
mem_remove_memory_only_fork(RelFileLocatorBackend rlocator, ForkNumber forknum)
{
	MemSmgrForkKey key;
	HTAB	   *fork_hash;
	MemSmgrForkEntry *entry;

	mem_build_fork_key(&key, rlocator, forknum);
	fork_hash = mem_fork_hash_for(&rlocator);
	entry = hash_search(fork_hash, &key, HASH_FIND, NULL);

	if (entry != NULL && !entry->on_disk)
		hash_search(fork_hash, &key, HASH_REMOVE, NULL);

	mem_remove_pages(rlocator, forknum, 0);
}

static void
mem_remove_fork(RelFileLocatorBackend rlocator, ForkNumber forknum)
{
	MemSmgrForkKey key;
	HTAB	   *fork_hash;
	MemSmgrForkEntry *entry;

	mem_build_fork_key(&key, rlocator, forknum);
	fork_hash = mem_fork_hash_for(&rlocator);
	entry = hash_search(fork_hash, &key, HASH_FIND, NULL);

	if (entry != NULL && !entry->on_disk)
		hash_search(fork_hash, &key, HASH_REMOVE, NULL);
	else
	{
		if (entry == NULL)
		{
			bool		found;

			entry = hash_search(fork_hash, &key, HASH_ENTER_NULL, &found);
		}
		if (entry == NULL)
			ereport(ERROR,
					(errcode(ERRCODE_OUT_OF_MEMORY),
					 errmsg("memory storage manager fork table is full")));

		entry->exists = false;
		entry->on_disk = true;
		entry->nblocks = 0;
	}

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
			memcpy(buffers[i], mem_page_data(page), BLCKSZ);
		else if (fork->on_disk && mem_seed_active_for(&reln->smgr_rlocator))
			mem_seed_read_block(reln->smgr_rlocator, forknum, curblock,
								buffers[i]);
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
		memcpy(mem_page_data(page), buffers[i], BLCKSZ);
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
		memset(mem_page_data(page), 0, BLCKSZ);
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
