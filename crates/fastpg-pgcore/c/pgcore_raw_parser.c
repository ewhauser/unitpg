/*-------------------------------------------------------------------------
 *
 * pgcore_raw_parser.c
 *	  Minimal C ABI for using PostgreSQL's raw parser from fastpg's Rust
 *	  single-process server.
 *
 *-------------------------------------------------------------------------
 */
#include "postgres.h"

#include <pthread.h>
#include <stdint.h>
#include <unistd.h>
#ifdef __APPLE__
#include <mach-o/dyld.h>
#endif

#include "access/relation.h"
#include "access/session.h"
#include "access/fastpg_catalog.h"
#include "access/fastpg_tableam.h"
#include "access/table.h"
#include "access/transam.h"
#include "access/tupdesc.h"
#include "access/xlog.h"
#include "access/xact.h"
#include "catalog/heap.h"
#include "catalog/index.h"
#include "catalog/namespace.h"
#include "catalog/pg_authid.h"
#include "catalog/pg_class.h"
#include "catalog/pg_database.h"
#include "catalog/pg_collation.h"
#include "catalog/pg_language.h"
#include "catalog/pg_namespace.h"
#include "catalog/pg_tablespace.h"
#include "catalog/pg_type.h"
#include "commands/async.h"
#include "commands/copy.h"
#include "commands/defrem.h"
#include "commands/event_trigger.h"
#include "commands/progress.h"
#include "commands/prepare.h"
#include "commands/sequence.h"
#include "executor/execdesc.h"
#include "executor/executor.h"
#include "executor/tuptable.h"
#include "fmgr.h"
#include "libpq/libpq.h"
#include "libpq/protocol.h"
#include "libpq/pqsignal.h"
#include "miscadmin.h"
#include "nodes/nodeFuncs.h"
#include "nodes/bitmapset.h"
#include "nodes/makefuncs.h"
#include "nodes/nodes.h"
#include "nodes/params.h"
#include "nodes/parsenodes.h"
#include "nodes/pg_list.h"
#include "nodes/plannodes.h"
#include "nodes/primnodes.h"
#include "nodes/value.h"
#include "optimizer/optimizer.h"
#include "parser/analyze.h"
#include "parser/parse_coerce.h"
#include "parser/parse_collate.h"
#include "parser/parse_expr.h"
#include "parser/parse_relation.h"
#include "parser/parsetree.h"
#include "parser/parser.h"
#include "pgstat.h"
#include "pgtime.h"
#include "postmaster/postmaster.h"
#include "rewrite/rewriteHandler.h"
#include "storage/fd.h"
#include "storage/bufmgr.h"
#include "storage/ipc.h"
#include "storage/lock.h"
#include "storage/proc.h"
#include "storage/shmem_internal.h"
#include "tcop/cmdtag.h"
#include "tcop/dest.h"
#include "tcop/pquery.h"
#include "tcop/tcopprot.h"
#include "tcop/utility.h"
#include "utils/backend_progress.h"
#include "utils/elog.h"
#include "utils/acl.h"
#include "utils/builtins.h"
#include "utils/fastpg_pgstat_noop.h"
#include "utils/guc.h"
#include "utils/inval.h"
#include "utils/lsyscache.h"
#include "utils/memutils.h"
#include "utils/plancache.h"
#include "utils/portal.h"
#include "utils/rel.h"
#include "utils/relcache.h"
#include "utils/reltrigger.h"
#include "utils/resowner.h"
#include "utils/rls.h"
#include "utils/snapmgr.h"
#include "utils/snapshot.h"
#include "utils/syscache.h"
#include "utils/timeout.h"
#include "utils/timestamp.h"

#ifdef USE_FASTPG
extern void fastpg_xid_begin(void);
extern void fastpg_xid_commit(void);
extern void fastpg_xid_rollback(void);
extern void fastpg_storage2_xact_begin(void);
extern void fastpg_storage2_xact_commit(void);
extern void fastpg_storage2_xact_abort(void);
extern void fastpg_storage2_xact_commit_if_implicit(void);
extern void fastpg_storage2_xact_abort_if_implicit(void);
extern void fastpg_storage2_subxact_begin(void);
extern void fastpg_storage2_subxact_commit(void);
extern void fastpg_storage2_subxact_abort(void);
extern void fastpg_mem_ensure_xact_callbacks(void);
#define FASTPG_PGCORE_CATALOG_ERROR_CLEANUP() FastPgCatalogCacheUnlockAll()
#else
#define FASTPG_PGCORE_CATALOG_ERROR_CLEANUP() ((void) 0)
#endif

typedef struct FastPgPgCoreParseResult
{
	bool		ok;
	int			statement_count;
	char		sqlstate[6];
	char	   *message;
	char	   *detail;
	char	   *hint;
	char	   *error_context;
	int			cursorpos;
} FastPgPgCoreParseResult;

typedef struct FastPgPgCoreField
{
	char	   *name;
	Oid			type_oid;
	int32		type_modifier;
	Oid			output_oid;
} FastPgPgCoreField;

typedef struct FastPgPgCoreCopyColumn
{
	char	   *name;
	AttrNumber	attnum;
	Oid			type_oid;
	int32		type_modifier;
} FastPgPgCoreCopyColumn;

typedef struct FastPgPgCoreNotice FastPgPgCoreNotice;

typedef struct FastPgPgCorePrepared
{
	MemoryContext context;
	bool		ok;
	char		sqlstate[6];
	char	   *message;
	char	   *detail;
	char	   *hint;
	char	   *error_context;
	int			cursorpos;
	char	   *internal_query;
	int			internalpos;
	char	   *source_text;
	List	   *raw_parsetrees;
	Query	   *query;
	List	   *querytrees;
	List	   *planned_statements;
	Oid		   *parameter_type_oids;
	int			parameter_count;
	FastPgPgCoreField *fields;
	int			field_count;
	int			notice_count;
	FastPgPgCoreNotice *notices;
} FastPgPgCorePrepared;

typedef struct FastPgPgCoreExecuteCell
{
	bool		is_null;
	char	   *value_text;
} FastPgPgCoreExecuteCell;

typedef struct FastPgPgCoreExecuteRow
{
	FastPgPgCoreExecuteCell *cells;
} FastPgPgCoreExecuteRow;

typedef struct FastPgPgCoreNotice
{
	char	   *severity;
	char		sqlstate[6];
	char	   *message;
	char	   *detail;
	char	   *hint;
	char	   *context;
	char	   *error_context;
	int			cursorpos;
} FastPgPgCoreNotice;

typedef struct FastPgPgCoreCopyOutChunk
{
	char	   *data;
	int			len;
} FastPgPgCoreCopyOutChunk;
typedef struct FastPgPgCoreExecuteStatement
{
	CmdType		command_type;
	char	   *command_tag;
	bool		copy_in;
	char	   *copy_table;
	Oid			copy_table_oid;
	int			copy_relation_column_count;
	int			copy_column_count;
	int			copy_format;
	int			copy_header_line;
	int			copy_on_error;
	bool		copy_freeze;
	bool		copy_foreign_table;
	bool		copy_partitioned_table;
	bool		copy_has_insert_triggers;
	bool		copy_has_generated_columns;
	char	   *copy_source_text;
	char	   *copy_delimiter;
	char	   *copy_null_print;
	char	   *copy_default_print;
	char	  **copy_column_names;
	FastPgPgCoreCopyColumn *copy_columns;
	bool		copy_out;
	int			copy_out_format;
	int			copy_out_columns;
	int			copy_out_chunk_count;
	FastPgPgCoreCopyOutChunk *copy_out_chunks;
	bool		has_plan_tree;
	NodeTag		plan_tree_tag;
	int			column_count;
	FastPgPgCoreField *columns;
	int			row_count;
	FastPgPgCoreExecuteRow *rows;
	bool		has_processed_count;
	uint64		processed_count;
} FastPgPgCoreExecuteStatement;

typedef struct FastPgPgCoreExecuteResult
{
	MemoryContext context;
	bool		ok;
	char		sqlstate[6];
	char	   *message;
	char	   *detail;
	char	   *hint;
	char	   *error_context;
	char	   *internal_query;
	int			cursorpos;
	int			internalpos;
	int			notice_count;
	FastPgPgCoreNotice *notices;
	int			statement_count;
	FastPgPgCoreExecuteStatement *statements;
} FastPgPgCoreExecuteResult;

typedef struct FastPgPgCoreInputDatumResult
{
	bool		ok;
	char		sqlstate[6];
	char	   *message;
	char	   *detail;
	char	   *hint;
	char	   *error_context;
	int			cursorpos;
	Datum		value;
	bool		typbyval;
	int16		typlen;
	size_t		value_len;
	unsigned char *payload;
} FastPgPgCoreInputDatumResult;

typedef struct FastPgPgCoreCaptureDestReceiver
{
	DestReceiver pub;
	FastPgPgCoreExecuteStatement *statement;
	MemoryContext context;
} FastPgPgCoreCaptureDestReceiver;

static DestReceiver *fastpg_pgcore_create_capture_receiver(FastPgPgCoreExecuteStatement *statement,
														   MemoryContext context);
static char *fastpg_pgcore_strdup(const char *value);

static pthread_once_t fastpg_pgcore_initialized = PTHREAD_ONCE_INIT;
static _Thread_local FastPgPgCoreExecuteResult *fastpg_pgcore_active_notice_result = NULL;
static _Thread_local const char *fastpg_pgcore_active_notice_source_text = NULL;
static _Thread_local emit_log_hook_type fastpg_pgcore_previous_client_message_hook = NULL;
static _Thread_local bool fastpg_pgcore_notice_capture_active = false;
static _Thread_local FastPgPgCoreExecuteStatement *fastpg_pgcore_active_copy_out_statement = NULL;
static _Thread_local MemoryContext fastpg_pgcore_active_copy_out_context = NULL;
typedef struct FastPgPgCoreCopyBuffer
{
	const char *data;
	size_t		len;
	size_t		offset;
} FastPgPgCoreCopyBuffer;

static _Thread_local FastPgPgCoreCopyBuffer *fastpg_pgcore_active_copy_buffer = NULL;

static void
fastpg_pgcore_copy_out_append(char msgtype, const char *s, size_t len)
{
	FastPgPgCoreExecuteStatement *statement =
		fastpg_pgcore_active_copy_out_statement;
	MemoryContext context = fastpg_pgcore_active_copy_out_context;
	MemoryContext oldcontext;
	FastPgPgCoreCopyOutChunk *chunk;

	if (statement == NULL || context == NULL)
		return;

	if (msgtype == PqMsg_CopyOutResponse)
	{
		if (len >= 3)
		{
			const unsigned char *bytes = (const unsigned char *) s;

			statement->copy_out = true;
			statement->copy_out_format = bytes[0];
			statement->copy_out_columns = ((int) bytes[1] << 8) | (int) bytes[2];
		}
		return;
	}
	if (msgtype != PqMsg_CopyData)
		return;

	oldcontext = MemoryContextSwitchTo(context);
	if (statement->copy_out_chunks == NULL)
		statement->copy_out_chunks =
			palloc_array(FastPgPgCoreCopyOutChunk, 1);
	else
		statement->copy_out_chunks =
			repalloc_array(statement->copy_out_chunks,
						   FastPgPgCoreCopyOutChunk,
						   statement->copy_out_chunk_count + 1);
	chunk = &statement->copy_out_chunks[statement->copy_out_chunk_count++];
	chunk->len = (int) len;
	chunk->data = palloc(len == 0 ? 1 : len);
	if (len > 0)
		memcpy(chunk->data, s, len);
	MemoryContextSwitchTo(oldcontext);
}

static void
fastpg_pgcore_copy_out_comm_reset(void)
{
}

static int
fastpg_pgcore_copy_out_flush(void)
{
	return 0;
}

static int
fastpg_pgcore_copy_out_flush_if_writable(void)
{
	return 0;
}

static bool
fastpg_pgcore_copy_out_is_send_pending(void)
{
	return false;
}

static int
fastpg_pgcore_copy_out_putmessage(char msgtype, const char *s, size_t len)
{
	fastpg_pgcore_copy_out_append(msgtype, s, len);
	return 0;
}

static void
fastpg_pgcore_copy_out_putmessage_noblock(char msgtype, const char *s, size_t len)
{
	fastpg_pgcore_copy_out_append(msgtype, s, len);
}

static const PQcommMethods fastpg_pgcore_copy_out_methods = {
	.comm_reset = fastpg_pgcore_copy_out_comm_reset,
	.flush = fastpg_pgcore_copy_out_flush,
	.flush_if_writable = fastpg_pgcore_copy_out_flush_if_writable,
	.is_send_pending = fastpg_pgcore_copy_out_is_send_pending,
	.putmessage = fastpg_pgcore_copy_out_putmessage,
	.putmessage_noblock = fastpg_pgcore_copy_out_putmessage_noblock,
};

static _Thread_local struct
{
	bool		active;
	int			backend_type;
	FastPgPgCoreNotice *notices;
	int			count;
	int			capacity;
} fastpg_pgcore_notice_capture;

static pthread_mutex_t fastpg_pgcore_notice_hook_mutex = PTHREAD_MUTEX_INITIALIZER;
static emit_log_hook_type fastpg_pgcore_notice_global_previous_hook = NULL;
static bool fastpg_pgcore_notice_hook_installed = false;
static int fastpg_pgcore_notice_capture_counts[BACKEND_NUM_TYPES];
static int fastpg_pgcore_notice_previous_log_min_messages[BACKEND_NUM_TYPES];

static const char *
fastpg_pgcore_notice_severity(int elevel)
{
	if (elevel == INFO)
		return "INFO";
	if (elevel == WARNING || elevel == WARNING_CLIENT_ONLY)
		return "WARNING";
	if (elevel >= NOTICE && elevel < WARNING)
		return "NOTICE";
	if (elevel == LOG || elevel == LOG_SERVER_ONLY)
		return "LOG";
	return "DEBUG";
}

static bool
fastpg_pgcore_should_capture_notice(int elevel)
{
	return elevel == INFO || (elevel >= NOTICE && elevel < ERROR);
}

static char *
fastpg_pgcore_strdup_or_null(const char *value)
{
	if (value == NULL)
		return NULL;
	return fastpg_pgcore_strdup(value);
}

static void
fastpg_pgcore_memory_context_delete(MemoryContext context)
{
	if (context == NULL)
		return;
#ifdef USE_FASTPG
	if (context->firstchild != NULL && context->firstchild->parent != context)
		return;
#endif
	MemoryContextDelete(context);
}

static void
fastpg_pgcore_notice_hook(ErrorData *edata)
{
	emit_log_hook_type previous_hook;

	pthread_mutex_lock(&fastpg_pgcore_notice_hook_mutex);
	previous_hook = fastpg_pgcore_notice_global_previous_hook;
	pthread_mutex_unlock(&fastpg_pgcore_notice_hook_mutex);

	if (previous_hook != NULL && previous_hook != fastpg_pgcore_notice_hook)
		(*previous_hook) (edata);

	if (!fastpg_pgcore_notice_capture.active)
		return;

	/*
	 * We lower log_min_messages only while capturing so PostgreSQL considers
	 * client-facing notices interesting in this standalone embedding.  The hook
	 * then suppresses server-log output; Rust will forward the client-facing
	 * messages over the pgwire connection instead.
	 */
	edata->output_to_server = false;

	if (!fastpg_pgcore_should_capture_notice(edata->elevel) ||
		!edata->output_to_client ||
		edata->message == NULL)
		return;

	if (fastpg_pgcore_notice_capture.count == fastpg_pgcore_notice_capture.capacity)
	{
		int			next_capacity =
			fastpg_pgcore_notice_capture.capacity == 0 ? 8 :
			fastpg_pgcore_notice_capture.capacity * 2;
		FastPgPgCoreNotice *next_notices =
			(FastPgPgCoreNotice *) realloc(fastpg_pgcore_notice_capture.notices,
										   sizeof(FastPgPgCoreNotice) * next_capacity);

		if (next_notices == NULL)
			return;

		fastpg_pgcore_notice_capture.notices = next_notices;
		fastpg_pgcore_notice_capture.capacity = next_capacity;
	}

	FastPgPgCoreNotice *notice =
		&fastpg_pgcore_notice_capture.notices[fastpg_pgcore_notice_capture.count++];
	memset(notice, 0, sizeof(*notice));
	notice->severity = fastpg_pgcore_strdup(fastpg_pgcore_notice_severity(edata->elevel));
	strlcpy(notice->sqlstate, unpack_sql_state(edata->sqlerrcode),
			sizeof(notice->sqlstate));
	notice->message = fastpg_pgcore_strdup(edata->message);
	notice->detail = fastpg_pgcore_strdup_or_null(edata->detail);
	notice->hint = fastpg_pgcore_strdup_or_null(edata->hint);
	notice->context = fastpg_pgcore_strdup_or_null(edata->context);
	notice->cursorpos = edata->cursorpos;
	if (notice->cursorpos <= 0 &&
		(edata->context == NULL || edata->context[0] == '\0') &&
		edata->internalpos > 0 &&
		edata->internalquery != NULL &&
		edata->internalquery[0] != '\0' &&
		fastpg_pgcore_active_notice_source_text != NULL)
	{
		const char *match = strstr(fastpg_pgcore_active_notice_source_text,
								   edata->internalquery);

		if (match != NULL)
			notice->cursorpos =
				(int) (match - fastpg_pgcore_active_notice_source_text) +
				edata->internalpos;
	}
}

static int
fastpg_pgcore_notice_backend_type(void)
{
	if (MyBackendType > B_INVALID && MyBackendType < BACKEND_NUM_TYPES)
		return MyBackendType;
	return B_STANDALONE_BACKEND;
}

static void
fastpg_pgcore_notice_capture_global_begin(int backend_type)
{
	pthread_mutex_lock(&fastpg_pgcore_notice_hook_mutex);
	if (!fastpg_pgcore_notice_hook_installed ||
		emit_log_hook != fastpg_pgcore_notice_hook)
	{
		if (emit_log_hook != NULL && emit_log_hook != fastpg_pgcore_notice_hook)
			fastpg_pgcore_notice_global_previous_hook = emit_log_hook;
		emit_log_hook = fastpg_pgcore_notice_hook;
		fastpg_pgcore_notice_hook_installed = true;
	}
	if (fastpg_pgcore_notice_capture_counts[backend_type] == 0)
	{
		fastpg_pgcore_notice_previous_log_min_messages[backend_type] =
			log_min_messages[backend_type];
		if (log_min_messages[backend_type] > INFO)
			log_min_messages[backend_type] = INFO;
	}
	fastpg_pgcore_notice_capture_counts[backend_type]++;
	pthread_mutex_unlock(&fastpg_pgcore_notice_hook_mutex);
}

static void
fastpg_pgcore_notice_capture_global_end(int backend_type)
{
	pthread_mutex_lock(&fastpg_pgcore_notice_hook_mutex);
	if (fastpg_pgcore_notice_capture_counts[backend_type] > 0)
	{
		fastpg_pgcore_notice_capture_counts[backend_type]--;
		if (fastpg_pgcore_notice_capture_counts[backend_type] == 0)
			log_min_messages[backend_type] =
				fastpg_pgcore_notice_previous_log_min_messages[backend_type];
	}
	pthread_mutex_unlock(&fastpg_pgcore_notice_hook_mutex);
}
static void
fastpg_pgcore_configure_library_paths(void)
{
	const char *libdir;

	libdir = getenv("FASTPG_PGLIBDIR");
	if (libdir == NULL || libdir[0] == '\0')
		libdir = getenv("PG_LIBDIR");
	if (libdir == NULL || libdir[0] == '\0')
		return;
	if (!is_absolute_path(libdir))
		return;

	strlcpy(pkglib_path, libdir, MAXPGPATH);
	SetConfigOption("dynamic_library_path", "$libdir",
					PGC_SUSET, PGC_S_OVERRIDE);
}

/*
 * We link selected backend objects without backend/main/main.c because that file
 * also owns the postgres executable's main(). Some cold command-line paths in
 * those backend objects still reference these dispatch symbols.
 */
const char *progname = "fastpg-server";

DispatchOption
parse_dispatch_option(const char *name)
{
	if (strcmp(name, "check") == 0)
		return DISPATCH_CHECK;
	if (strcmp(name, "boot") == 0)
		return DISPATCH_BOOT;
	if (strcmp(name, "forkchild") == 0)
		return DISPATCH_FORKCHILD;
	if (strcmp(name, "describe-config") == 0)
		return DISPATCH_DESCRIBE_CONFIG;
	if (strcmp(name, "single") == 0)
		return DISPATCH_SINGLE;
	return DISPATCH_POSTMASTER;
}

static void
fastpg_pgcore_init_rust_catalog_once(void)
{
	MyProcPid = getpid();
	MemoryContextInit();
#ifdef USE_FASTPG
	FastPgEnsureThreadTransactionState();
#endif
	MyDatabaseId = PostgresDbOid;
	MyDatabaseTableSpace = DEFAULTTABLESPACE_OID;
	DatabasePath = pstrdup("base/5");
	InitializeGUCOptions();
	fastpg_pgcore_configure_library_paths();
	pg_timezone_initialize();
	RelationCacheInitialize();
	InitCatalogCache();
	InitializeSession();
	EnablePortalManager();
	namespace_search_path = pstrdup("\"$user\", public");
	InitializeSearchPath();
}

static const char *
fastpg_pgcore_pgdata(void)
{
	const char *pgdata = getenv("FASTPG_PGDATA");

	if (pgdata == NULL || pgdata[0] == '\0')
		ereport(ERROR,
				(errcode(ERRCODE_INVALID_PARAMETER_VALUE),
				 errmsg("Postgres catalog mode requires FASTPG_PGDATA")));
	if (!is_absolute_path(pgdata))
		ereport(ERROR,
				(errcode(ERRCODE_INVALID_PARAMETER_VALUE),
				 errmsg("FASTPG_PGDATA must be an absolute path")));
	return pgdata;
}

static void
fastpg_pgcore_set_my_exec_path(void)
{
	const char *exec_path = getenv("FASTPG_EXEC_PATH");

	if (my_exec_path[0] != '\0')
		return;
	if (exec_path != NULL &&
		exec_path[0] != '\0' &&
		is_absolute_path(exec_path))
	{
		strlcpy(my_exec_path, exec_path, MAXPGPATH);
		return;
	}

#ifdef __APPLE__
	{
		char		path[MAXPGPATH];
		uint32_t	size = sizeof(path);

		if (_NSGetExecutablePath(path, &size) == 0 &&
			is_absolute_path(path))
			strlcpy(my_exec_path, path, MAXPGPATH);
	}
#else
	{
		ssize_t		len;

		len = readlink("/proc/self/exe", my_exec_path, MAXPGPATH - 1);
		if (len > 0)
			my_exec_path[len] = '\0';
		else
			my_exec_path[0] = '\0';
	}
#endif
}

static void
fastpg_pgcore_init_catalog_cache_phase2(void)
{
	ResourceOwner saved_owner = CurrentResourceOwner;
	ResourceOwner startup_owner = NULL;

	if (saved_owner == NULL)
	{
		startup_owner =
			ResourceOwnerCreate(NULL, "fastpg catalog cache phase2");
		CurrentResourceOwner = startup_owner;
	}

	PG_TRY();
	{
		InitCatalogCachePhase2();
	}
	PG_CATCH();
	{
		FASTPG_PGCORE_CATALOG_ERROR_CLEANUP();
		CurrentResourceOwner = saved_owner;
		if (startup_owner != NULL)
		{
			ResourceOwnerRelease(startup_owner, RESOURCE_RELEASE_BEFORE_LOCKS,
								 false, true);
			ResourceOwnerRelease(startup_owner, RESOURCE_RELEASE_LOCKS,
								 false, true);
			ResourceOwnerRelease(startup_owner, RESOURCE_RELEASE_AFTER_LOCKS,
								 false, true);
			ResourceOwnerDelete(startup_owner);
		}
		PG_RE_THROW();
	}
	PG_END_TRY();

	CurrentResourceOwner = saved_owner;
	if (startup_owner != NULL)
	{
		ResourceOwnerRelease(startup_owner, RESOURCE_RELEASE_BEFORE_LOCKS,
							 true, true);
		ResourceOwnerRelease(startup_owner, RESOURCE_RELEASE_LOCKS,
							 true, true);
		ResourceOwnerRelease(startup_owner, RESOURCE_RELEASE_AFTER_LOCKS,
							 true, true);
		ResourceOwnerDelete(startup_owner);
	}
}

static void
fastpg_pgcore_init_postgres_catalog_once(void)
{
	const char *pgdata = fastpg_pgcore_pgdata();
	bool		old_ignore_system_indexes;

	MyProcPid = getpid();
	MemoryContextInit();
#ifdef USE_FASTPG
	FastPgEnsureThreadTransactionState();
#endif
	(void) set_stack_base();
	fastpg_pgcore_set_my_exec_path();
	InitStandaloneProcess(progname);
	InitializeGUCOptions();
	SetDataDir(pgdata);
	fastpg_pgcore_configure_library_paths();
	pg_timezone_initialize();

	if (!SelectConfigFiles(pgdata, progname))
		ereport(ERROR,
				(errcode(ERRCODE_CONFIG_FILE_ERROR),
				 errmsg("could not load PostgreSQL configuration for FASTPG_PGDATA")));
	SetConfigOption("synchronous_commit", "on",
					PGC_USERSET, PGC_S_OVERRIDE);
	SetConfigOption("io_method", "sync",
					PGC_POSTMASTER, PGC_S_OVERRIDE);
	SetConfigOption("fsync", "off",
					PGC_SIGHUP, PGC_S_OVERRIDE);
	SetConfigOption("full_page_writes", "off",
					PGC_SIGHUP, PGC_S_OVERRIDE);

	checkDataDir();
	ChangeToDataDir();
	CreateDataDirLockFile(false);
	LocalProcessControlFile(false);
	RegisterBuiltinShmemCallbacks();
	if (!fastpg_pgstat_noop_active())
		process_shared_preload_libraries();
	InitializeMaxBackends();
	InitPostmasterChildSlots();
	InitializeFastPathLocks();
	process_shmem_requests();
	ShmemCallRequestCallbacks();
	InitializeShmemGUCs();
	InitializeWalConsistencyChecking();
	CreateSharedMemoryAndSemaphores();
	set_max_safe_fds();
	PgStartTime = GetCurrentTimestamp();
	InitProcess();
	InitializeTimeouts();
	BaseInit();
	sigprocmask(SIG_SETMASK, &UnBlockSig, NULL);

	old_ignore_system_indexes = IgnoreSystemIndexes;
	IgnoreSystemIndexes = true;
	InitPostgres(NULL, PostgresDbOid,
				 "postgres", InvalidOid,
				 0, NULL);
	IgnoreSystemIndexes = old_ignore_system_indexes;
	fastpg_pgcore_init_catalog_cache_phase2();
	SetProcessingMode(NormalProcessing);
	MyBackendType = B_BACKEND;
}

static void
fastpg_pgcore_init_once(void)
{
	if (fastpg_catalog_mode_uses_postgres())
		fastpg_pgcore_init_postgres_catalog_once();
	else
		fastpg_pgcore_init_rust_catalog_once();
}

static void
fastpg_pgcore_enter(void)
{
	pthread_once(&fastpg_pgcore_initialized, fastpg_pgcore_init_once);
#ifdef USE_FASTPG
	FastPgEnsureThreadMemoryContexts();
	FastPgEnsureThreadTransactionState();
	FastPgEnsureThreadProc();
	FastPgEnsureThreadPgStat();
	FastPgEnsureThreadBufferManagerAccess();
	FastPgEnsureThreadNamespaceState();
	FastPgEnsureThreadLockManagerAccess();
#endif
	(void) set_stack_base();
}

static void
fastpg_pgcore_notice_free(FastPgPgCoreNotice *notice)
{
	if (notice == NULL)
		return;
	free(notice->severity);
	free(notice->message);
	free(notice->detail);
	free(notice->hint);
	free(notice->context);
	free(notice->error_context);
	memset(notice, 0, sizeof(*notice));
}

void
fastpg_pgcore_notice_capture_clear(void)
{
	for (int i = 0; i < fastpg_pgcore_notice_capture.count; i++)
		fastpg_pgcore_notice_free(&fastpg_pgcore_notice_capture.notices[i]);
	free(fastpg_pgcore_notice_capture.notices);
	fastpg_pgcore_notice_capture.notices = NULL;
	fastpg_pgcore_notice_capture.count = 0;
	fastpg_pgcore_notice_capture.capacity = 0;
}

void
fastpg_pgcore_notice_capture_begin(void)
{
	int			backend_type;

	fastpg_pgcore_enter();
	if (fastpg_pgcore_notice_capture.active)
		return;

	fastpg_pgcore_notice_capture_clear();
	backend_type = fastpg_pgcore_notice_backend_type();
	fastpg_pgcore_notice_capture.backend_type = backend_type;
	fastpg_pgcore_notice_capture_global_begin(backend_type);
	fastpg_pgcore_notice_capture.active = true;
}

void
fastpg_pgcore_notice_capture_end(void)
{
	if (!fastpg_pgcore_notice_capture.active)
		return;

	fastpg_pgcore_notice_capture_global_end(fastpg_pgcore_notice_capture.backend_type);
	fastpg_pgcore_notice_capture.backend_type = B_INVALID;
	fastpg_pgcore_notice_capture.active = false;
}

int
fastpg_pgcore_notice_capture_count(void)
{
	return fastpg_pgcore_notice_capture.count;
}

static const FastPgPgCoreNotice *
fastpg_pgcore_notice_capture_get(int index)
{
	if (index < 0 || index >= fastpg_pgcore_notice_capture.count)
		return NULL;
	return &fastpg_pgcore_notice_capture.notices[index];
}

const char *
fastpg_pgcore_notice_capture_severity(int index)
{
	const FastPgPgCoreNotice *notice = fastpg_pgcore_notice_capture_get(index);

	if (notice == NULL || notice->severity == NULL)
		return "";
	return notice->severity;
}

const char *
fastpg_pgcore_notice_capture_sqlstate(int index)
{
	const FastPgPgCoreNotice *notice = fastpg_pgcore_notice_capture_get(index);

	if (notice == NULL || notice->sqlstate[0] == '\0')
		return "00000";
	return notice->sqlstate;
}

const char *
fastpg_pgcore_notice_capture_message(int index)
{
	const FastPgPgCoreNotice *notice = fastpg_pgcore_notice_capture_get(index);

	if (notice == NULL || notice->message == NULL)
		return "";
	return notice->message;
}

const char *
fastpg_pgcore_notice_capture_detail(int index)
{
	const FastPgPgCoreNotice *notice = fastpg_pgcore_notice_capture_get(index);

	if (notice == NULL || notice->detail == NULL)
		return "";
	return notice->detail;
}

const char *
fastpg_pgcore_notice_capture_hint(int index)
{
	const FastPgPgCoreNotice *notice = fastpg_pgcore_notice_capture_get(index);

	if (notice == NULL || notice->hint == NULL)
		return "";
	return notice->hint;
}

const char *
fastpg_pgcore_notice_capture_context(int index)
{
	const FastPgPgCoreNotice *notice = fastpg_pgcore_notice_capture_get(index);

	if (notice == NULL || notice->context == NULL)
		return "";
	return notice->context;
}

int
fastpg_pgcore_notice_capture_cursorpos(int index)
{
	const FastPgPgCoreNotice *notice = fastpg_pgcore_notice_capture_get(index);

	if (notice == NULL)
		return 0;
	return notice->cursorpos;
}

void
fastpg_pgcore_set_database(uint32_t database_oid)
{
	MemoryContext old_context;

	fastpg_pgcore_enter();
	if (!OidIsValid((Oid) database_oid))
		database_oid = PostgresDbOid;

	if (MyDatabaseId == (Oid) database_oid &&
		MyDatabaseTableSpace == DEFAULTTABLESPACE_OID &&
		DatabasePath != NULL)
	{
		if (MyProc != NULL)
			MyProc->databaseId = MyDatabaseId;
		return;
	}

	MyDatabaseId = (Oid) database_oid;
	MyDatabaseTableSpace = DEFAULTTABLESPACE_OID;
	if (MyProc != NULL)
		MyProc->databaseId = MyDatabaseId;

	if (DatabasePath != NULL)
		pfree(DatabasePath);
	old_context = MemoryContextSwitchTo(TopMemoryContext);
	DatabasePath = psprintf("base/%u", database_oid);
	MemoryContextSwitchTo(old_context);
}

void
fastpg_pgcore_invalidate_system_caches(void)
{
	fastpg_pgcore_enter();
#ifdef USE_FASTPG
	FastPgCatalogCacheLock();
	PG_TRY();
	{
#endif
	InvalidateSystemCaches();
#ifdef USE_FASTPG
	}
	PG_CATCH();
	{
		FastPgCatalogCacheUnlockAll();
		PG_RE_THROW();
	}
	PG_END_TRY();
	FastPgCatalogCacheUnlock();
#endif
}

static void
fastpg_pgcore_ensure_execution_owner(void)
{
#ifdef USE_FASTPG
	if (!IsUnderPostmaster && fastpg_use_rust_catalog())
	{
		FastPgEnsureStandaloneTransactionState();
		return;
	}
#endif
	if (TopTransactionContext == NULL)
		TopTransactionContext =
			AllocSetContextCreate(TopMemoryContext,
								  "fastpg pgcore top transaction",
								  ALLOCSET_DEFAULT_SIZES);
	if (CurTransactionContext == NULL)
		CurTransactionContext = TopTransactionContext;
	if (CurrentResourceOwner == NULL)
		CurrentResourceOwner =
			ResourceOwnerCreate(NULL, "fastpg pgcore resource owner");
}

static void
fastpg_pgcore_release_error_resources(void)
{
	ResourceOwner owner = CurrentResourceOwner;

	if (owner == NULL)
		return;

#ifdef USE_FASTPG
	if (!IsUnderPostmaster && fastpg_use_rust_catalog())
	{
		if (!fastpg_rust_xact_is_explicit())
			FastPgReleaseStandaloneStatementResources(false);
		fastpg_pgcore_ensure_execution_owner();
		return;
	}
#endif
	ResourceOwnerRelease(owner, RESOURCE_RELEASE_BEFORE_LOCKS, false, true);
	ResourceOwnerRelease(owner, RESOURCE_RELEASE_LOCKS, false, true);
	ResourceOwnerRelease(owner, RESOURCE_RELEASE_AFTER_LOCKS, false, true);
	ResourceOwnerDelete(owner);
	CurrentResourceOwner = NULL;
	fastpg_pgcore_ensure_execution_owner();
}

static void
fastpg_pgcore_start_statement_timestamp(void)
{
	SetCurrentStatementStartTimestamp();
#ifdef USE_FASTPG
	if (fastpg_use_rust_catalog() && !fastpg_rust_xact_is_explicit())
	{
		FastPgStartStandaloneStatement();
		FastPgSetCurrentTransactionStartTimestampToStatement();
	}
	else if (fastpg_use_rust_catalog())
		FastPgEnsureStandaloneTransactionState();
#endif
}

static void
fastpg_pgcore_push_analyze_snapshot(void)
{
	/*
	 * In the embedded multithreaded server, keep this short-lived analyze/planner
	 * snapshot independent from the transaction's reusable CurrentSnapshotData.
	 */
	PushCopiedSnapshot(GetTransactionSnapshot());
}

static void
fastpg_pgcore_start_postgres_catalog_command(void)
{
#ifdef USE_FASTPG
	fastpg_mem_ensure_xact_callbacks();
#endif
	SetCurrentStatementStartTimestamp();
	StartTransactionCommand();
}

static char *
fastpg_pgcore_notice_strdup(const char *value)
{
	if (value == NULL)
		return NULL;
	return pstrdup(value);
}

static void
fastpg_pgcore_append_notice(FastPgPgCoreExecuteResult *result,
							ErrorData *edata)
{
	MemoryContext oldcontext;
	FastPgPgCoreNotice *notice;
	int			old_count;
	const char *message;
	const char *match;

	if (result == NULL || result->context == NULL || edata == NULL)
		return;
	if (edata->elevel >= ERROR)
		return;

	oldcontext = MemoryContextSwitchTo(result->context);
	old_count = result->notice_count;
	if (result->notices == NULL)
		result->notices = palloc0_array(FastPgPgCoreNotice, old_count + 1);
	else
		result->notices = repalloc0_array(result->notices,
										  FastPgPgCoreNotice,
										  old_count,
										  old_count + 1);
	notice = &result->notices[old_count];
	notice->severity = pstrdup(error_severity(edata->elevel));
	memcpy(notice->sqlstate, unpack_sql_state(edata->sqlerrcode), 6);
	message = edata->message != NULL ? edata->message : "missing error text";
	notice->message = pstrdup(message);
	notice->detail = fastpg_pgcore_notice_strdup(edata->detail);
	notice->hint = fastpg_pgcore_notice_strdup(edata->hint);
	notice->error_context = fastpg_pgcore_notice_strdup(edata->context);
	notice->cursorpos = edata->cursorpos;
	if (notice->cursorpos <= 0 &&
		(edata->context == NULL || edata->context[0] == '\0') &&
		edata->internalpos > 0 &&
		edata->internalquery != NULL &&
		edata->internalquery[0] != '\0' &&
		fastpg_pgcore_active_notice_source_text != NULL)
	{
		match = strstr(fastpg_pgcore_active_notice_source_text,
					   edata->internalquery);
		if (match != NULL)
			notice->cursorpos =
				(int) (match - fastpg_pgcore_active_notice_source_text) +
				edata->internalpos;
	}
	result->notice_count = old_count + 1;
	MemoryContextSwitchTo(oldcontext);
}

static void
fastpg_pgcore_client_message_hook(ErrorData *edata)
{
	if (fastpg_pgcore_previous_client_message_hook != NULL &&
		fastpg_pgcore_previous_client_message_hook != fastpg_pgcore_client_message_hook)
		(*fastpg_pgcore_previous_client_message_hook) (edata);

	if (!edata->output_to_client)
		return;
	fastpg_pgcore_append_notice(fastpg_pgcore_active_notice_result, edata);
	if (fastpg_pgcore_active_notice_result != NULL)
		edata->output_to_client = false;
}

static void
fastpg_pgcore_begin_notice_capture(FastPgPgCoreExecuteResult *result,
								   const char *source_text)
{
#ifdef USE_FASTPG
	if (!fastpg_catalog_mode_uses_postgres() ||
		fastpg_pgcore_notice_capture_active)
		return;

	fastpg_pgcore_active_notice_result = result;
	fastpg_pgcore_active_notice_source_text = source_text;
	fastpg_pgcore_previous_client_message_hook = fastpg_client_message_hook;
	fastpg_client_message_hook = fastpg_pgcore_client_message_hook;
	fastpg_pgcore_notice_capture_active = true;
#else
	(void) result;
	(void) source_text;
#endif
}

static void
fastpg_pgcore_end_notice_capture(void)
{
#ifdef USE_FASTPG
	if (!fastpg_pgcore_notice_capture_active)
		return;

	fastpg_client_message_hook = fastpg_pgcore_previous_client_message_hook;
	fastpg_pgcore_previous_client_message_hook = NULL;
	fastpg_pgcore_active_notice_result = NULL;
	fastpg_pgcore_active_notice_source_text = NULL;
	fastpg_pgcore_notice_capture_active = false;
#endif
}

static void fastpg_pgcore_finish_postgres_catalog_command(volatile bool *command_started);
static void fastpg_pgcore_abort_postgres_catalog_command(volatile bool *command_started);

static void
fastpg_pgcore_force_idle_transaction_state(void)
{
	if (IsTransactionBlock() || IsAbortedTransactionBlockState())
	{
		UserAbortTransactionBlock(false);
		CommitTransactionCommand();
	}
	else
		AbortCurrentTransaction();
}

static void
fastpg_pgcore_report_activity_running(const char *source_text)
{
	if (fastpg_catalog_mode_uses_postgres())
		pgstat_report_activity(STATE_RUNNING, source_text);
}

static void
fastpg_pgcore_refresh_login_event_flag(void)
{
	HeapTuple	dbtuple;

	if (!fastpg_catalog_mode_uses_postgres() || !OidIsValid(MyDatabaseId))
		return;

	StartTransactionCommand();
	dbtuple = SearchSysCache1(DATABASEOID, ObjectIdGetDatum(MyDatabaseId));
	if (HeapTupleIsValid(dbtuple))
	{
		Form_pg_database dbform = (Form_pg_database) GETSTRUCT(dbtuple);

		MyDatabaseHasLoginEventTriggers = dbform->dathasloginevt;
		ReleaseSysCache(dbtuple);
	}
	else
		MyDatabaseHasLoginEventTriggers = false;
	CommitTransactionCommand();
}

void
fastpg_pgcore_reset_session_state(void)
{
	MemoryContext old_context;
	volatile bool postgres_command_started = false;

	fastpg_pgcore_enter();
	if (!fastpg_catalog_mode_uses_postgres())
		return;

	old_context = CurrentMemoryContext;
	PG_TRY();
	{
		fastpg_pgcore_force_idle_transaction_state();
		fastpg_pgcore_start_postgres_catalog_command();
		postgres_command_started = true;
		FastPgEnsureStandaloneUserId();
		PortalHashTableDeleteAll();
		SetPGVariable("session_authorization", NIL, false);
		SetPGVariable("role", NIL, false);
#ifdef USE_FASTPG
		FastPgUnreserveGUCPrefixForSession("plpgsql");
#endif
		ResetAllOptions();
		DropAllPreparedStatements();
		Async_UnlistenAll();
		LockReleaseAll(USER_LOCKMETHOD, true);
		ResetPlanCache();
#ifdef USE_FASTPG
		FastPgResetTempNamespaceSessionState();
#else
		ResetTempTableNamespace();
#endif
#ifdef USE_FASTPG
		FastPgResetLocalBuffers();
#endif
		ResetSequenceCaches();
		SetSessionAuthorization(BOOTSTRAP_SUPERUSERID, true);
		SetCurrentRoleId(InvalidOid, false);
		FastPgEnsureStandaloneUserId();
		fastpg_pgcore_finish_postgres_catalog_command(&postgres_command_started);
		if (!IsTransactionOrTransactionBlock())
			pgstat_report_stat(true);
	}
	PG_CATCH();
	{
		FASTPG_PGCORE_CATALOG_ERROR_CLEANUP();
		FlushErrorState();
		MemoryContextSwitchTo(old_context);
		fastpg_pgcore_abort_postgres_catalog_command(&postgres_command_started);
	}
	PG_END_TRY();
	MemoryContextSwitchTo(old_context);
}

void
fastpg_pgcore_start_client_session(void)
{
	fastpg_pgcore_enter();
	if (!fastpg_catalog_mode_uses_postgres())
		return;

	fastpg_pgcore_reset_session_state();
	fastpg_pgcore_refresh_login_event_flag();
	EventTriggerOnLogin();
}

static bool
fastpg_pgcore_is_transaction_exit_stmt(Node *parsetree)
{
	if (parsetree != NULL && IsA(parsetree, TransactionStmt))
	{
		TransactionStmt *stmt = (TransactionStmt *) parsetree;

		return stmt->kind == TRANS_STMT_COMMIT ||
			stmt->kind == TRANS_STMT_PREPARE ||
			stmt->kind == TRANS_STMT_ROLLBACK ||
			stmt->kind == TRANS_STMT_ROLLBACK_TO;
	}

	return false;
}

static bool
fastpg_pgcore_is_transaction_exit_planned_stmt(PlannedStmt *stmt)
{
	return stmt != NULL &&
		stmt->commandType == CMD_UTILITY &&
		fastpg_pgcore_is_transaction_exit_stmt(stmt->utilityStmt);
}

static bool
fastpg_pgcore_is_transaction_planned_stmt(PlannedStmt *stmt)
{
	return stmt != NULL &&
		stmt->commandType == CMD_UTILITY &&
		stmt->utilityStmt != NULL &&
		IsA(stmt->utilityStmt, TransactionStmt);
}

static void
fastpg_pgcore_reject_if_aborted_transaction(Node *parsetree)
{
	if (IsAbortedTransactionBlockState() &&
		!fastpg_pgcore_is_transaction_exit_stmt(parsetree))
		ereport(ERROR,
				(errcode(ERRCODE_IN_FAILED_SQL_TRANSACTION),
				 errmsg("current transaction is aborted, "
						"commands ignored until end of transaction block")));
}

static void
fastpg_pgcore_finish_postgres_catalog_command(volatile bool *command_started)
{
	if (!*command_started)
		return;

	CommitTransactionCommand();
#ifdef USE_FASTPG
	if (fastpg_catalog_mode_uses_postgres())
		FastPgResetRoleMembershipCache();
#endif
	if (!IsTransactionOrTransactionBlock())
		pgstat_report_stat(false);
	*command_started = false;
}

static void
fastpg_pgcore_abort_postgres_catalog_command(volatile bool *command_started)
{
	if (!*command_started)
		return;

	AbortCurrentTransaction();
	*command_started = false;
}

static FastPgPgCoreParseResult *
fastpg_pgcore_parse_result_alloc(void)
{
	FastPgPgCoreParseResult *result;

	result = (FastPgPgCoreParseResult *) calloc(1, sizeof(FastPgPgCoreParseResult));
	if (result == NULL)
		return NULL;

	result->ok = false;
	result->statement_count = 0;
	result->sqlstate[0] = '\0';
	result->message = NULL;
	result->detail = NULL;
	result->hint = NULL;
	result->error_context = NULL;
	result->cursorpos = 0;

	return result;
}

static char *
fastpg_pgcore_strdup(const char *value)
{
	size_t		len;
	char	   *copy;

	if (value == NULL)
		value = "";

	len = strlen(value);
	copy = (char *) malloc(len + 1);
	if (copy == NULL)
		return NULL;
	memcpy(copy, value, len + 1);
	return copy;
}

static void
fastpg_pgcore_set_error(FastPgPgCoreParseResult *result, ErrorData *edata)
{
	const char *sqlstate = unpack_sql_state(edata->sqlerrcode);
	const char *message = edata->message != NULL ? edata->message : "PostgreSQL parser error";

	memcpy(result->sqlstate, sqlstate, sizeof(result->sqlstate));
	result->message = fastpg_pgcore_strdup(message);
	result->detail = fastpg_pgcore_strdup(edata->detail);
	result->hint = fastpg_pgcore_strdup(edata->hint);
	result->error_context = fastpg_pgcore_strdup(edata->context);
	result->cursorpos = edata->cursorpos;
}

static void
fastpg_pgcore_copy_error(char sqlstate_out[6],
						 char **message_out,
						 char **detail_out,
						 char **hint_out,
						 char **context_out,
						 int *cursorpos_out,
						 ErrorData *edata)
{
	const char *sqlstate = unpack_sql_state(edata->sqlerrcode);
	const char *message = edata->message != NULL ? edata->message : "PostgreSQL error";

	memcpy(sqlstate_out, sqlstate, 6);
	free(*message_out);
	*message_out = fastpg_pgcore_strdup(message);
	free(*detail_out);
	*detail_out = fastpg_pgcore_strdup(edata->detail);
	free(*hint_out);
	*hint_out = fastpg_pgcore_strdup(edata->hint);
	free(*context_out);
	*context_out = fastpg_pgcore_strdup(edata->context);
	*cursorpos_out = edata->cursorpos;
}

static void
fastpg_pgcore_set_prepared_error(FastPgPgCorePrepared *result,
								 ErrorData *edata)
{
	result->ok = false;
	fastpg_pgcore_copy_error(result->sqlstate,
							 &result->message,
							 &result->detail,
							 &result->hint,
							 &result->error_context,
							 &result->cursorpos,
							 edata);
	free(result->internal_query);
	result->internal_query = fastpg_pgcore_strdup(edata->internalquery);
	result->internalpos = edata->internalpos;
}

static void
fastpg_pgcore_transpose_internal_error(FastPgPgCoreExecuteResult *result,
									   const char *source_text)
{
	const char *match;

	if (result == NULL ||
		source_text == NULL ||
		(fastpg_catalog_mode_uses_postgres() &&
		 result->error_context != NULL &&
		 result->error_context[0] != '\0') ||
		result->cursorpos > 0 ||
		result->internalpos <= 0 ||
		result->internal_query == NULL ||
		result->internal_query[0] == '\0')
		return;

	match = strstr(source_text, result->internal_query);
	if (match == NULL)
		return;

	result->cursorpos = (int) (match - source_text) + result->internalpos;
	free(result->internal_query);
	result->internal_query = NULL;
	result->internalpos = 0;
}

static void
fastpg_pgcore_set_execute_error(FastPgPgCoreExecuteResult *result,
								ErrorData *edata,
								const char *source_text)
{
	result->ok = false;
	fastpg_pgcore_copy_error(result->sqlstate,
							 &result->message,
							 &result->detail,
							 &result->hint,
							 &result->error_context,
							 &result->cursorpos,
							 edata);
	free(result->internal_query);
	result->internal_query = fastpg_pgcore_strdup(edata->internalquery);
	result->internalpos = edata->internalpos;
	fastpg_pgcore_transpose_internal_error(result, source_text);
}

static FastPgPgCoreExecuteStatement *
fastpg_pgcore_next_execute_statement(FastPgPgCoreExecuteResult *result,
									 int *statement_capacity,
									 int *statement_index)
{
	FastPgPgCoreExecuteStatement *summary;
	int			old_capacity;
	int			new_capacity;

	if (*statement_index >= *statement_capacity)
	{
		old_capacity = *statement_capacity;
		new_capacity = old_capacity > 0 ? old_capacity * 2 : 8;
		while (*statement_index >= new_capacity)
			new_capacity *= 2;
		if (result->statements == NULL)
			result->statements =
				palloc0_array(FastPgPgCoreExecuteStatement, new_capacity);
		else
		{
			result->statements =
				repalloc_array(result->statements,
							  FastPgPgCoreExecuteStatement,
							  new_capacity);
			memset(result->statements + old_capacity,
				   0,
				   sizeof(FastPgPgCoreExecuteStatement) *
				   (new_capacity - old_capacity));
		}
		*statement_capacity = new_capacity;
	}

	summary = &result->statements[*statement_index];
	(*statement_index)++;
	return summary;
}

static const char *
fastpg_pgcore_command_tag_name(CmdType command_type)
{
	switch (command_type)
	{
		case CMD_SELECT:
			return "SELECT";
		case CMD_UPDATE:
			return "UPDATE";
		case CMD_INSERT:
			return "INSERT";
		case CMD_DELETE:
			return "DELETE";
		case CMD_MERGE:
			return "MERGE";
		case CMD_UTILITY:
			return "UTILITY";
		case CMD_NOTHING:
			return "NOTHING";
		case CMD_UNKNOWN:
		default:
			return "UNKNOWN";
	}
}

static CommandTag
fastpg_pgcore_command_tag_enum(CmdType command_type)
{
	switch (command_type)
	{
		case CMD_SELECT:
			return CMDTAG_SELECT;
		case CMD_UPDATE:
			return CMDTAG_UPDATE;
		case CMD_INSERT:
			return CMDTAG_INSERT;
		case CMD_DELETE:
			return CMDTAG_DELETE;
		case CMD_MERGE:
			return CMDTAG_MERGE;
		case CMD_UTILITY:
		case CMD_NOTHING:
		case CMD_UNKNOWN:
		default:
			return CMDTAG_UNKNOWN;
	}
}

static void
fastpg_pgcore_set_processed_count(FastPgPgCoreExecuteStatement *summary,
								  CommandTag command_tag,
								  uint64 processed_count)
{
	if (!command_tag_display_rowcount(command_tag))
		return;

	summary->has_processed_count = true;
	summary->processed_count = processed_count;
}

static int
fastpg_pgcore_make_sqlstate(const char *sqlstate)
{
	if (sqlstate == NULL || strlen(sqlstate) < 5)
		return ERRCODE_INTERNAL_ERROR;
	return MAKE_SQLSTATE(sqlstate[0],
						 sqlstate[1],
						 sqlstate[2],
						 sqlstate[3],
						 sqlstate[4]);
}

static void
fastpg_pgcore_raise_rust_catalog_error(const char *sqlstate,
									   const char *message)
{
	ereport(ERROR,
			(errcode(fastpg_pgcore_make_sqlstate(sqlstate)),
			 errmsg("%s", message != NULL ? message : "fastpg catalog error")));
}

static const char *
fastpg_pgcore_rangevar_name(const RangeVar *relation)
{
	if (relation == NULL || relation->relname == NULL)
		ereport(ERROR,
				(errcode(ERRCODE_INVALID_NAME),
				 errmsg("fastpg utility statement is missing a relation name")));
	if (relation->schemaname != NULL &&
		pg_strcasecmp(relation->schemaname, "public") != 0)
		ereport(ERROR,
				(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
				 errmsg("fastpg pgcore only supports public schema relations")));
	return relation->relname;
}

static void
fastpg_pgcore_execute_noop_utility(const char *command_tag,
								   FastPgPgCoreExecuteStatement *summary)
{
	summary->command_tag = (char *) command_tag;
}

static void
fastpg_pgcore_copy_column_from_attr(FastPgPgCoreExecuteStatement *summary,
									int column_index,
									Form_pg_attribute attr)
{
	summary->copy_column_names[column_index] = pstrdup(NameStr(attr->attname));
	summary->copy_columns[column_index].name =
		summary->copy_column_names[column_index];
	summary->copy_columns[column_index].attnum = attr->attnum;
	summary->copy_columns[column_index].type_oid = attr->atttypid;
	summary->copy_columns[column_index].type_modifier = attr->atttypmod;
}

static Form_pg_attribute
fastpg_pgcore_copy_attr_by_name(TupleDesc tupdesc, const char *name)
{
	for (int index = 0; index < tupdesc->natts; index++)
	{
		Form_pg_attribute attr = TupleDescAttr(tupdesc, index);

		if (attr->attnum <= 0 || attr->attisdropped)
			continue;
		if (namestrcmp(&attr->attname, name) == 0)
			return attr;
	}
	return NULL;
}

static bool
fastpg_pgcore_copy_attlist_contains_attnum(TupleDesc tupdesc,
										   List *attlist,
										   int attnum)
{
	Form_pg_attribute attr;
	ListCell   *lc;

	if (attlist == NIL)
		return true;
	if (attnum <= 0 || attnum > tupdesc->natts)
		return false;

	attr = TupleDescAttr(tupdesc, attnum - 1);
	if (attr->attisdropped)
		return false;

	foreach(lc, attlist)
	{
		const char *column_name = strVal(lfirst(lc));

		if (namestrcmp(&attr->attname, column_name) == 0)
			return true;
	}
	return false;
}

static void
fastpg_pgcore_validate_copy_default_startup(Relation relation,
											TupleDesc tupdesc,
											List *attlist,
											const CopyFormatOptions *opts)
{
	for (int attnum = 1; attnum <= tupdesc->natts; attnum++)
	{
		Form_pg_attribute attr = TupleDescAttr(tupdesc, attnum - 1);

		if (attr->attisdropped || attr->attgenerated)
			continue;
		if (opts->default_print == NULL &&
			fastpg_pgcore_copy_attlist_contains_attnum(tupdesc, attlist, attnum))
			continue;

		Expr	   *defexpr = (Expr *) build_column_default(relation, attnum);

		if (defexpr == NULL)
			continue;

		defexpr = expression_planner(defexpr);
		(void) ExecInitExpr(defexpr, NULL);
	}
}

static bool
fastpg_pgcore_copy_relation_has_insert_triggers(Relation relation)
{
	TriggerDesc *trigdesc = relation->trigdesc;

	return trigdesc != NULL &&
		(trigdesc->trig_insert_before_row ||
		 trigdesc->trig_insert_after_row ||
		 trigdesc->trig_insert_instead_row ||
		 trigdesc->trig_insert_before_statement ||
		 trigdesc->trig_insert_after_statement ||
		 trigdesc->trig_insert_new_table);
}

static Form_pg_attribute
fastpg_pgcore_copy_first_generated_attr(TupleDesc tupdesc)
{
	for (int index = 0; index < tupdesc->natts; index++)
	{
		Form_pg_attribute attr = TupleDescAttr(tupdesc, index);

		if (attr->attnum > 0 && !attr->attisdropped && attr->attgenerated)
			return attr;
	}
	return NULL;
}

static void
fastpg_pgcore_validate_copy_force_columns(Relation relation,
										  TupleDesc tupdesc,
										  List *copy_attnums,
										  List *force_columns,
										  const char *option_name)
{
	List	   *force_attnums;
	ListCell   *cur;

	if (force_columns == NIL)
		return;

	force_attnums = CopyGetAttnums(tupdesc, relation, force_columns);
	foreach(cur, force_attnums)
	{
		int			attnum = lfirst_int(cur);
		Form_pg_attribute attr = TupleDescAttr(tupdesc, attnum - 1);

		if (!list_member_int(copy_attnums, attnum))
			ereport(ERROR,
					(errcode(ERRCODE_INVALID_COLUMN_REFERENCE),
					 errmsg("%s column \"%s\" not referenced by COPY",
							option_name, NameStr(attr->attname))));
	}
}

static void
fastpg_pgcore_validate_copy_startup(const CopyStmt *stmt,
									Relation relation,
									TupleDesc tupdesc,
									const CopyFormatOptions *opts,
									const char *source_text)
{
	Oid			relation_oid = RelationGetRelid(relation);
	ParseState *pstate = make_parsestate(NULL);
	ParseNamespaceItem *nsitem;
	RTEPermissionInfo *perminfo;
	List	   *attnums;
	ListCell   *cur;

	nsitem = addRangeTableEntryForRelation(pstate,
										   relation,
										   RowExclusiveLock,
										   NULL,
										   false,
										   false);
	pstate->p_sourcetext = source_text;
	perminfo = nsitem->p_perminfo;
	perminfo->requiredPerms = ACL_INSERT;

	if (stmt->whereClause)
	{
		Node	   *whereClause;
		Bitmapset  *expr_attrs = NULL;
		int			i;

		addNSItemToQuery(pstate, nsitem, false, true, true);
		whereClause = transformExpr(pstate,
									stmt->whereClause,
									EXPR_KIND_COPY_WHERE);
		whereClause = coerce_to_boolean(pstate, whereClause, "WHERE");
		assign_expr_collations(pstate, whereClause);
		pull_varattnos(whereClause, 1, &expr_attrs);
		if (bms_is_member(0 - FirstLowInvalidHeapAttributeNumber, expr_attrs))
		{
			expr_attrs = bms_add_range(expr_attrs,
									   1 - FirstLowInvalidHeapAttributeNumber,
									   RelationGetNumberOfAttributes(relation) - FirstLowInvalidHeapAttributeNumber);
			expr_attrs = bms_del_member(expr_attrs,
										0 - FirstLowInvalidHeapAttributeNumber);
		}
		i = -1;
		while ((i = bms_next_member(expr_attrs, i)) >= 0)
		{
			AttrNumber	attno = i + FirstLowInvalidHeapAttributeNumber;
			Form_pg_attribute attr;

			if (attno < 0)
				ereport(ERROR,
						(errcode(ERRCODE_INVALID_COLUMN_REFERENCE),
						 errmsg("system columns are not supported in COPY FROM WHERE conditions"),
						 errdetail("Column \"%s\" is a system column.",
								   get_attname(relation_oid, attno, false))));

			attr = TupleDescAttr(tupdesc, attno - 1);
			if (attr->attgenerated && !attr->attisdropped)
				ereport(ERROR,
						(errcode(ERRCODE_INVALID_COLUMN_REFERENCE),
						 errmsg("generated columns are not supported in COPY FROM WHERE conditions"),
						 errdetail("Column \"%s\" is a generated column.",
								   get_attname(relation_oid, attno, false))));
		}
	}

	attnums = CopyGetAttnums(tupdesc, relation, stmt->attlist);
	fastpg_pgcore_validate_copy_force_columns(relation,
											  tupdesc,
											  attnums,
											  opts->force_notnull,
											  "FORCE_NOT_NULL");
	fastpg_pgcore_validate_copy_force_columns(relation,
											  tupdesc,
											  attnums,
											  opts->force_null,
											  "FORCE_NULL");
	foreach(cur, attnums)
	{
		int			attno =
			lfirst_int(cur) - FirstLowInvalidHeapAttributeNumber;

		perminfo->insertedCols =
			bms_add_member(perminfo->insertedCols, attno);
	}
	ExecCheckPermissions(pstate->p_rtable, list_make1(perminfo), true);

	if (check_enable_rls(relation_oid, InvalidOid, false) == RLS_ENABLED)
		ereport(ERROR,
				(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
				 errmsg("COPY FROM not supported with row-level security"),
				 errhint("Use INSERT statements instead.")));

	free_parsestate(pstate);
}

static void
fastpg_pgcore_execute_copy_stmt(const CopyStmt *stmt,
								const char *source_text,
								FastPgPgCoreExecuteStatement *summary)
{
	const char *relation_name;
	Oid			relation_oid;
	Relation	relation;
	TupleDesc	tupdesc;
	CopyFormatOptions opts;
	Form_pg_attribute generated_attr;

	if (!stmt->is_from ||
		stmt->filename != NULL ||
		stmt->is_program ||
		stmt->query != NULL ||
		stmt->relation == NULL)
		ereport(ERROR,
				(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
				 errmsg("fastpg pgcore only supports COPY relation FROM STDIN")));

	memset(&opts, 0, sizeof(opts));
	{
		ParseState *pstate = make_parsestate(NULL);

		pstate->p_sourcetext = source_text;
		ProcessCopyOptions(pstate, &opts, true, stmt->options);
		free_parsestate(pstate);
	}

	relation_name = fastpg_pgcore_rangevar_name(stmt->relation);
	relation = table_openrv(stmt->relation, RowExclusiveLock);
	relation_oid = RelationGetRelid(relation);
	tupdesc = RelationGetDescr(relation);
	generated_attr = fastpg_pgcore_copy_first_generated_attr(tupdesc);
	fastpg_pgcore_validate_copy_startup(stmt,
										relation,
										tupdesc,
										&opts,
										source_text);

	summary->copy_in = true;
	summary->copy_table = pstrdup(relation_name);
	summary->copy_table_oid = relation_oid;
	summary->copy_relation_column_count = tupdesc->natts;
	summary->copy_format = opts.format;
	summary->copy_header_line = opts.header_line;
	summary->copy_on_error = opts.on_error;
	summary->copy_freeze = opts.freeze;
	summary->copy_foreign_table =
		relation->rd_rel->relkind == RELKIND_FOREIGN_TABLE;
	summary->copy_partitioned_table =
		relation->rd_rel->relkind == RELKIND_PARTITIONED_TABLE;
	summary->copy_has_insert_triggers =
		fastpg_pgcore_copy_relation_has_insert_triggers(relation);
	summary->copy_has_generated_columns = generated_attr != NULL;
	summary->copy_source_text = pstrdup(source_text);
	summary->copy_delimiter = opts.delim != NULL ? pstrdup(opts.delim) : NULL;
	summary->copy_null_print =
		opts.null_print != NULL ? pstrdup(opts.null_print) : NULL;
	summary->copy_default_print =
		opts.default_print != NULL ? pstrdup(opts.default_print) : NULL;
	if (stmt->attlist != NIL)
	{
		ListCell   *lc;
		int			column_index = 0;

		summary->copy_column_count = list_length(stmt->attlist);
		summary->copy_column_names =
			palloc0_array(char *, summary->copy_column_count);
		summary->copy_columns =
			palloc0_array(FastPgPgCoreCopyColumn, summary->copy_column_count);
		foreach(lc, stmt->attlist)
		{
			const char *column_name = strVal(lfirst(lc));
			Form_pg_attribute attr =
				fastpg_pgcore_copy_attr_by_name(tupdesc, column_name);

			if (attr == NULL)
				ereport(ERROR,
						(errcode(ERRCODE_UNDEFINED_COLUMN),
						 errmsg("column \"%s\" of relation \"%s\" does not exist",
								column_name, relation_name)));
			if (attr->attgenerated)
				ereport(ERROR,
						(errcode(ERRCODE_INVALID_COLUMN_REFERENCE),
						 errmsg("column \"%s\" is a generated column",
								column_name),
						 errdetail("Generated columns cannot be used in COPY.")));
			for (int prior = 0; prior < column_index; prior++)
			{
				if (strcmp(summary->copy_column_names[prior], column_name) == 0)
					ereport(ERROR,
							(errcode(ERRCODE_DUPLICATE_COLUMN),
							 errmsg("column \"%s\" specified more than once",
									column_name)));
			}
			fastpg_pgcore_copy_column_from_attr(summary, column_index++, attr);
		}
	}
	else
	{
		int			column_count = 0;
		int			column_index = 0;

		for (int index = 0; index < tupdesc->natts; index++)
		{
			Form_pg_attribute attr = TupleDescAttr(tupdesc, index);

			if (attr->attnum > 0 && !attr->attisdropped && !attr->attgenerated)
				column_count++;
		}
		summary->copy_column_count = column_count;
		summary->copy_column_names = palloc0_array(char *, column_count);
		summary->copy_columns =
			palloc0_array(FastPgPgCoreCopyColumn, column_count);
		for (int index = 0; index < tupdesc->natts; index++)
		{
			Form_pg_attribute attr = TupleDescAttr(tupdesc, index);

			if (attr->attnum > 0 && !attr->attisdropped && !attr->attgenerated)
				fastpg_pgcore_copy_column_from_attr(summary, column_index++, attr);
		}
	}
	if (stmt->whereClause != NULL && generated_attr != NULL)
		ereport(ERROR,
				(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
				 errmsg("generated columns are not supported in COPY FROM WHERE conditions"),
				 errdetail("Column \"%s\" is a generated column.",
						   NameStr(generated_attr->attname))));
	fastpg_pgcore_validate_copy_default_startup(relation,
											   tupdesc,
											   stmt->attlist,
											   &opts);
	relation_close(relation, RowExclusiveLock);
	summary->command_tag = "COPY";
}

static void
fastpg_pgcore_execute_transaction_stmt(const TransactionStmt *stmt,
									   FastPgPgCoreExecuteStatement *summary)
{
	switch (stmt->kind)
	{
		case TRANS_STMT_BEGIN:
		case TRANS_STMT_START:
#ifdef USE_FASTPG
			if (fastpg_catalog_mode_uses_postgres())
			{
				ListCell   *lc;

				BeginTransactionBlock();
				foreach(lc, stmt->options)
				{
					DefElem    *item = (DefElem *) lfirst(lc);

					if (strcmp(item->defname, "transaction_isolation") == 0)
						SetPGVariable("transaction_isolation",
									  list_make1(item->arg),
									  true);
					else if (strcmp(item->defname, "transaction_read_only") == 0)
						SetPGVariable("transaction_read_only",
									  list_make1(item->arg),
									  true);
					else if (strcmp(item->defname, "transaction_deferrable") == 0)
						SetPGVariable("transaction_deferrable",
									  list_make1(item->arg),
									  true);
				}
			}
			else
			{
				fastpg_xid_begin();
				fastpg_rust_xact_begin();
				fastpg_storage2_xact_begin();
			}
#else
			(void) stmt;
#endif
			summary->command_tag = "BEGIN";
			break;
		case TRANS_STMT_COMMIT:
		{
			bool		committed = true;

#ifdef USE_FASTPG
			if (fastpg_catalog_mode_uses_postgres())
				committed = EndTransactionBlock(stmt->chain);
			else if (committed)
			{
				fastpg_xid_commit();
				fastpg_rust_xact_commit();
				fastpg_storage2_xact_commit();
			}
			else
			{
				fastpg_xid_rollback();
				fastpg_rust_xact_abort();
				fastpg_storage2_xact_abort();
			}
#endif
			summary->command_tag = committed ? "COMMIT" : "ROLLBACK";
			break;
		}
		case TRANS_STMT_ROLLBACK:
#ifdef USE_FASTPG
			if (fastpg_catalog_mode_uses_postgres())
				UserAbortTransactionBlock(stmt->chain);
			else
			{
				fastpg_xid_rollback();
				fastpg_rust_xact_abort();
				fastpg_storage2_xact_abort();
			}
#endif
			summary->command_tag = "ROLLBACK";
			break;
		case TRANS_STMT_SAVEPOINT:
#ifdef USE_FASTPG
			if (fastpg_catalog_mode_uses_postgres())
			{
				RequireTransactionBlock(true, "SAVEPOINT");
				DefineSavepoint(stmt->savepoint_name);
			}
			else
			{
				fastpg_rust_subxact_begin();
				fastpg_storage2_subxact_begin();
			}
#endif
			summary->command_tag = "SAVEPOINT";
			break;
		case TRANS_STMT_RELEASE:
#ifdef USE_FASTPG
			if (fastpg_catalog_mode_uses_postgres())
			{
				RequireTransactionBlock(true, "RELEASE SAVEPOINT");
				ReleaseSavepoint(stmt->savepoint_name);
			}
			else
			{
				fastpg_rust_subxact_commit();
				fastpg_storage2_subxact_commit();
			}
#endif
			summary->command_tag = "RELEASE";
			break;
		case TRANS_STMT_ROLLBACK_TO:
#ifdef USE_FASTPG
			if (fastpg_catalog_mode_uses_postgres())
			{
				RequireTransactionBlock(true, "ROLLBACK TO SAVEPOINT");
				RollbackToSavepoint(stmt->savepoint_name);
			}
			else
			{
				fastpg_rust_subxact_abort();
				fastpg_rust_subxact_begin();
				fastpg_storage2_subxact_abort();
				fastpg_storage2_subxact_begin();
				FastPgReconcileRelcacheAfterCatalogRollback();
				InvalidateSystemCaches();
				RelationCacheInvalidate(false);
			}
#endif
			summary->command_tag = "ROLLBACK";
			break;
		default:
			ereport(ERROR,
					(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
					 errmsg("fastpg pgcore does not yet support transaction statement kind %d",
							(int) stmt->kind)));
	}
}

static bool
fastpg_pgcore_utility_is_copy_stdin_bridge(Node *utility_stmt)
{
	CopyStmt   *stmt;

	if (!IsA(utility_stmt, CopyStmt))
		return false;
	stmt = (CopyStmt *) utility_stmt;
	return stmt->is_from &&
		stmt->filename == NULL &&
		!stmt->is_program &&
		stmt->query == NULL &&
		stmt->relation != NULL;
}

static bool
fastpg_pgcore_should_noop_utility(Node *utility_stmt)
{
	switch (nodeTag(utility_stmt))
	{
		case T_GrantStmt:
		case T_GrantRoleStmt:
		case T_CreateTableSpaceStmt:
		case T_DropTableSpaceStmt:
		case T_AlterTableSpaceOptionsStmt:
		case T_CommentStmt:
		case T_SecLabelStmt:
		case T_VacuumStmt:
			return true;
		default:
			return false;
	}
}

#ifdef USE_FASTPG
static bool
fastpg_pgcore_resets_session_authorization(Node *utility_stmt)
{
	VariableSetStmt *stmt;

	if (!IsA(utility_stmt, VariableSetStmt))
		return false;

	stmt = (VariableSetStmt *) utility_stmt;
	if (stmt->kind == VAR_RESET_ALL)
		return true;
	if (stmt->name == NULL || strcmp(stmt->name, "session_authorization") != 0)
		return false;

	return stmt->kind == VAR_RESET || stmt->kind == VAR_SET_DEFAULT;
}

static void
fastpg_pgcore_repair_session_authorization_reset(Node *utility_stmt)
{
	if (IsUnderPostmaster ||
		!fastpg_pgcore_resets_session_authorization(utility_stmt))
		return;

	SetSessionAuthorization(BOOTSTRAP_SUPERUSERID, true);
	SetCurrentRoleId(InvalidOid, false);
}
#endif

static void
fastpg_pgcore_execute_utility(PlannedStmt *statement,
							  const char *source_text,
							  ParamListInfo params,
							  FastPgPgCoreExecuteStatement *summary,
							  MemoryContext result_context)
{
	Node	   *utility_stmt = statement->utilityStmt;
	DestReceiver *dest = NULL;
	QueryCompletion qc;
	volatile bool snapshot_pushed = false;
	volatile bool copy_out_capture_active = false;
	const PQcommMethods *save_pq_methods = NULL;
	CommandDest save_where_to_send_output = whereToSendOutput;

	if (fastpg_pgcore_utility_is_copy_stdin_bridge(utility_stmt))
	{
		fastpg_pgcore_execute_copy_stmt((const CopyStmt *) utility_stmt,
										source_text,
										summary);
		return;
	}

	if (fastpg_use_rust_catalog() && IsA(utility_stmt, TransactionStmt))
	{
		fastpg_pgcore_execute_transaction_stmt((const TransactionStmt *) utility_stmt,
											   summary);
		return;
	}

	if (fastpg_use_rust_catalog() &&
		fastpg_pgcore_should_noop_utility(utility_stmt))
	{
		fastpg_pgcore_execute_noop_utility(CreateCommandName(utility_stmt), summary);
		return;
	}

	InitializeQueryCompletion(&qc);
	dest = fastpg_pgcore_create_capture_receiver(summary, result_context);
	fastpg_pgcore_ensure_execution_owner();

	if (PlannedStmtRequiresSnapshot(statement))
	{
		fastpg_pgcore_push_analyze_snapshot();
		snapshot_pushed = true;
	}

	PG_TRY();
	{
		if (IsA(utility_stmt, CopyStmt))
		{
			CopyStmt   *copy = (CopyStmt *) utility_stmt;

			if (!copy->is_from &&
				copy->filename == NULL &&
				!copy->is_program)
			{
				save_pq_methods = PqCommMethods;
				save_where_to_send_output = whereToSendOutput;
				fastpg_pgcore_active_copy_out_statement = summary;
				fastpg_pgcore_active_copy_out_context = result_context;
				PqCommMethods = &fastpg_pgcore_copy_out_methods;
				whereToSendOutput = DestRemote;
				copy_out_capture_active = true;
			}
		}

		ProcessUtility(statement,
					   source_text,
					   false,
					   PROCESS_UTILITY_TOPLEVEL,
					   params,
					   NULL,
					   dest,
					   &qc);

#ifdef USE_FASTPG
		fastpg_pgcore_repair_session_authorization_reset(utility_stmt);
#endif

		if (snapshot_pushed && ActiveSnapshotSet())
			PopActiveSnapshot();
		snapshot_pushed = false;
		if (copy_out_capture_active)
		{
			PqCommMethods = save_pq_methods;
			whereToSendOutput = save_where_to_send_output;
			fastpg_pgcore_active_copy_out_statement = NULL;
			fastpg_pgcore_active_copy_out_context = NULL;
			copy_out_capture_active = false;
		}
	}
	PG_CATCH();
	{
		FASTPG_PGCORE_CATALOG_ERROR_CLEANUP();
		if (copy_out_capture_active)
		{
			PqCommMethods = save_pq_methods;
			whereToSendOutput = save_where_to_send_output;
			fastpg_pgcore_active_copy_out_statement = NULL;
			fastpg_pgcore_active_copy_out_context = NULL;
			copy_out_capture_active = false;
		}
		if (snapshot_pushed && ActiveSnapshotSet())
			PopActiveSnapshot();
		PG_RE_THROW();
	}
	PG_END_TRY();

	if (qc.commandTag != CMDTAG_UNKNOWN)
	{
		summary->command_tag = (char *) GetCommandTagName(qc.commandTag);
		fastpg_pgcore_set_processed_count(summary,
										  qc.commandTag,
										  qc.nprocessed);
	}
	else
		summary->command_tag = (char *) CreateCommandName(utility_stmt);

	dest->rDestroy(dest);
}

#ifdef USE_FASTPG
static bool
fastpg_pgcore_statement_targets_system_catalog(const PlannedStmt *statement)
{
	int			rtindex = -1;

	if (statement == NULL || statement->resultRelationRelids == NULL)
		return false;

	while ((rtindex = bms_next_member(statement->resultRelationRelids,
									  rtindex)) >= 0)
	{
		RangeTblEntry *rte;

		if (rtindex <= 0 || rtindex > list_length(statement->rtable))
			continue;

		rte = rt_fetch(rtindex, statement->rtable);
		if (rte->rtekind == RTE_RELATION && rte->relid < FirstNormalObjectId)
			return true;
	}

	return false;
}

static bool
fastpg_pgcore_should_noop_system_catalog_write(const PlannedStmt *statement)
{
	switch (statement->commandType)
	{
		case CMD_INSERT:
		case CMD_UPDATE:
		case CMD_DELETE:
		case CMD_MERGE:
			return fastpg_pgcore_statement_targets_system_catalog(statement);
		default:
			return false;
	}
}
#endif

static void
fastpg_pgcore_capture_analyze_fields(FastPgPgCorePrepared *result)
{
	ListCell   *lc;
	int			field_index = 0;

	if (result->query == NULL)
		return;

	foreach(lc, result->query->targetList)
	{
		const TargetEntry *target = lfirst_node(TargetEntry, lc);

		if (!target->resjunk)
			result->field_count++;
	}

	if (result->field_count == 0)
		return;

	result->fields = palloc0_array(FastPgPgCoreField, result->field_count);
	foreach(lc, result->query->targetList)
	{
		const TargetEntry *target = lfirst_node(TargetEntry, lc);

		if (target->resjunk)
			continue;

		result->fields[field_index].name =
			pstrdup(target->resname != NULL ? target->resname : "");
		if (target->expr != NULL)
		{
			Oid			type_oid = exprType((const Node *) target->expr);
			int32		type_modifier = exprTypmod((const Node *) target->expr);

			type_oid = getBaseTypeAndTypmod(type_oid, &type_modifier);
			result->fields[field_index].type_oid = type_oid;
			result->fields[field_index].type_modifier = type_modifier;
		}
		else
		{
			result->fields[field_index].type_oid = InvalidOid;
			result->fields[field_index].type_modifier = -1;
		}
		field_index++;
	}
}

static void
fastpg_pgcore_capture_startup(DestReceiver *self, int operation, TupleDesc typeinfo)
{
	FastPgPgCoreCaptureDestReceiver *receiver =
		(FastPgPgCoreCaptureDestReceiver *) self;
	FastPgPgCoreExecuteStatement *statement = receiver->statement;
	MemoryContext oldcontext;

	oldcontext = MemoryContextSwitchTo(receiver->context);

	statement->column_count = typeinfo->natts;
	statement->columns = palloc0_array(FastPgPgCoreField, statement->column_count);

	for (int index = 0; index < statement->column_count; index++)
	{
		Form_pg_attribute attr = TupleDescAttr(typeinfo, index);
		Oid			type_oid = attr->atttypid;
		int32		type_modifier = attr->atttypmod;
		bool		type_is_varlena;

		type_oid = getBaseTypeAndTypmod(type_oid, &type_modifier);
		statement->columns[index].name = pstrdup(NameStr(attr->attname));
		statement->columns[index].type_oid = type_oid;
		statement->columns[index].type_modifier = type_modifier;
		getTypeOutputInfo(attr->atttypid,
						  &statement->columns[index].output_oid,
						  &type_is_varlena);
	}

	MemoryContextSwitchTo(oldcontext);
}

static bool
fastpg_pgcore_capture_receive_slot(TupleTableSlot *slot, DestReceiver *self)
{
	FastPgPgCoreCaptureDestReceiver *receiver =
		(FastPgPgCoreCaptureDestReceiver *) self;
	FastPgPgCoreExecuteStatement *statement = receiver->statement;
	FastPgPgCoreExecuteRow *row;
	MemoryContext oldcontext;

	oldcontext = MemoryContextSwitchTo(receiver->context);

	if (statement->rows == NULL)
		statement->rows = palloc0_array(FastPgPgCoreExecuteRow, 1);
	else
		statement->rows = repalloc_array(statement->rows,
										 FastPgPgCoreExecuteRow,
										 statement->row_count + 1);
	row = &statement->rows[statement->row_count];
	row->cells = palloc0_array(FastPgPgCoreExecuteCell, statement->column_count);

	for (int index = 0; index < statement->column_count; index++)
	{
		bool		is_null;
		Datum		value;

		value = slot_getattr(slot, index + 1, &is_null);
		row->cells[index].is_null = is_null;

		if (!is_null)
			row->cells[index].value_text =
				OidOutputFunctionCall(statement->columns[index].output_oid, value);
	}

	statement->row_count++;
	MemoryContextSwitchTo(oldcontext);
	return true;
}

static void
fastpg_pgcore_capture_shutdown(DestReceiver *self)
{
}

static void
fastpg_pgcore_capture_destroy(DestReceiver *self)
{
	pfree(self);
}

static DestReceiver *
fastpg_pgcore_create_capture_receiver(FastPgPgCoreExecuteStatement *statement,
									  MemoryContext context)
{
	FastPgPgCoreCaptureDestReceiver *receiver =
		palloc0_object(FastPgPgCoreCaptureDestReceiver);

	receiver->pub.receiveSlot = fastpg_pgcore_capture_receive_slot;
	receiver->pub.rStartup = fastpg_pgcore_capture_startup;
	receiver->pub.rShutdown = fastpg_pgcore_capture_shutdown;
	receiver->pub.rDestroy = fastpg_pgcore_capture_destroy;
	receiver->pub.mydest = DestTuplestore;
	receiver->statement = statement;
	receiver->context = context;

	return (DestReceiver *) receiver;
}

FastPgPgCoreParseResult *
fastpg_pgcore_raw_parse(const char *query)
{
	FastPgPgCoreParseResult *result;
	MemoryContext oldcontext;
	MemoryContext parse_context = NULL;

	result = fastpg_pgcore_parse_result_alloc();
	if (result == NULL)
		return NULL;

	fastpg_pgcore_enter();
	oldcontext = CurrentMemoryContext;

	PG_TRY();
	{
		List	   *raw_parsetrees;

		parse_context = AllocSetContextCreate(TopMemoryContext,
											  "fastpg pgcore raw parser",
											  ALLOCSET_DEFAULT_SIZES);
		MemoryContextSwitchTo(parse_context);

		raw_parsetrees = raw_parser(query, RAW_PARSE_DEFAULT);
		result->ok = true;
		result->statement_count = list_length(raw_parsetrees);

		MemoryContextSwitchTo(oldcontext);
		fastpg_pgcore_memory_context_delete(parse_context);
		parse_context = NULL;
	}
	PG_CATCH();
	{
		ErrorData  *edata;

		FASTPG_PGCORE_CATALOG_ERROR_CLEANUP();
		MemoryContextSwitchTo(oldcontext);
		edata = CopyErrorData();
		FlushErrorState();
		fastpg_pgcore_set_error(result, edata);
		FreeErrorData(edata);

		if (parse_context != NULL)
			fastpg_pgcore_memory_context_delete(parse_context);
		parse_context = NULL;
	}
	PG_END_TRY();

	return result;
}

void
fastpg_pgcore_parse_result_free(FastPgPgCoreParseResult *result)
{
	if (result == NULL)
		return;

	free(result->message);
	free(result->detail);
	free(result->hint);
	free(result->error_context);
	free(result);
}

bool
fastpg_pgcore_parse_result_ok(const FastPgPgCoreParseResult *result)
{
	return result != NULL && result->ok;
}

int
fastpg_pgcore_parse_result_statement_count(const FastPgPgCoreParseResult *result)
{
	return result != NULL ? result->statement_count : 0;
}

const char *
fastpg_pgcore_parse_result_sqlstate(const FastPgPgCoreParseResult *result)
{
	if (result == NULL || result->sqlstate[0] == '\0')
		return "XX000";
	return result->sqlstate;
}

const char *
fastpg_pgcore_parse_result_message(const FastPgPgCoreParseResult *result)
{
	if (result == NULL || result->message == NULL)
		return "PostgreSQL parser error";
	return result->message;
}

const char *
fastpg_pgcore_parse_result_detail(const FastPgPgCoreParseResult *result)
{
	if (result == NULL || result->detail == NULL)
		return "";
	return result->detail;
}

const char *
fastpg_pgcore_parse_result_hint(const FastPgPgCoreParseResult *result)
{
	if (result == NULL || result->hint == NULL)
		return "";
	return result->hint;
}

const char *
fastpg_pgcore_parse_result_context(const FastPgPgCoreParseResult *result)
{
	if (result == NULL || result->error_context == NULL)
		return "";
	return result->error_context;
}

int
fastpg_pgcore_parse_result_cursorpos(const FastPgPgCoreParseResult *result)
{
	return result != NULL ? result->cursorpos : 0;
}

FastPgPgCorePrepared *
fastpg_pgcore_prepare(const char *query)
{
	FastPgPgCorePrepared *result;
	MemoryContext oldcontext;
	FastPgPgCoreExecuteResult notice_capture = {0};
	volatile bool postgres_command_started = false;
	volatile bool snapshot_pushed = false;

	result = (FastPgPgCorePrepared *) calloc(1, sizeof(FastPgPgCorePrepared));
	if (result == NULL)
		return NULL;

	fastpg_pgcore_enter();
	oldcontext = CurrentMemoryContext;
	if (fastpg_catalog_mode_uses_postgres())
	{
		fastpg_pgcore_start_postgres_catalog_command();
		postgres_command_started = true;
	}
	else
		fastpg_pgcore_ensure_execution_owner();
	result->context = AllocSetContextCreate(TopMemoryContext,
											"fastpg pgcore prepared statement",
											ALLOCSET_DEFAULT_SIZES);
	notice_capture.context = result->context;
	fastpg_pgcore_begin_notice_capture(&notice_capture, query);

	PG_TRY();
	{
		int			raw_count;
		int			cursor_options;
		ListCell   *lc;

		MemoryContextSwitchTo(result->context);
		result->source_text = pstrdup(query);
		result->raw_parsetrees = raw_parser(query, RAW_PARSE_DEFAULT);
		raw_count = list_length(result->raw_parsetrees);
		if (raw_count == 0)
		{
			result->ok = true;
			MemoryContextSwitchTo(oldcontext);
		}
		else
		{
#ifdef USE_FASTPG
			cursor_options = fastpg_catalog_mode_uses_postgres() ?
				CURSOR_OPT_PARALLEL_OK : 0;
#else
			cursor_options = CURSOR_OPT_PARALLEL_OK;
#endif
			if (raw_count != 1 && strchr(result->source_text, '$') != NULL)
			{
				ereport(ERROR,
						(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
						 errmsg("fastpg pgcore does not support parameters in multi-statement queries")));
			}

			result->querytrees = NIL;
			result->planned_statements = NIL;
			if (fastpg_catalog_mode_uses_postgres() && raw_count > 1)
			{
				result->ok = true;
				MemoryContextSwitchTo(oldcontext);
			}
			else
			{
			foreach(lc, result->raw_parsetrees)
			{
				RawStmt    *rawstmt = lfirst_node(RawStmt, lc);
				Query	   *querytree;
				List	   *rewritten;
				List	   *planned;

				if (fastpg_catalog_mode_uses_postgres())
					fastpg_pgcore_reject_if_aborted_transaction(rawstmt->stmt);
				if (fastpg_catalog_mode_uses_postgres() &&
					analyze_requires_snapshot(rawstmt))
				{
					fastpg_pgcore_push_analyze_snapshot();
					snapshot_pushed = true;
				}
				if (strchr(result->source_text, '$') == NULL)
				{
					querytree = parse_analyze_fixedparams(rawstmt,
														 result->source_text,
														 NULL,
														 0,
														 NULL);
				}
				else
				{
					querytree = parse_analyze_varparams(rawstmt,
														result->source_text,
														&result->parameter_type_oids,
														&result->parameter_count,
														NULL);
				}
				result->query = querytree;
				if (result->field_count == 0)
					fastpg_pgcore_capture_analyze_fields(result);
				rewritten = pg_rewrite_query(querytree);
				planned = pg_plan_queries(rewritten,
										  result->source_text,
										  cursor_options,
										  NULL);
				result->querytrees = list_concat(result->querytrees, rewritten);
				result->planned_statements =
					list_concat(result->planned_statements, planned);
				if (snapshot_pushed && ActiveSnapshotSet())
				{
					PopActiveSnapshot();
					snapshot_pushed = false;
				}
			}
			result->ok = true;
			MemoryContextSwitchTo(oldcontext);
			}
		}
		MemoryContextSwitchTo(oldcontext);
		fastpg_pgcore_finish_postgres_catalog_command(&postgres_command_started);
	}
	PG_CATCH();
	{
		ErrorData  *edata;

		FASTPG_PGCORE_CATALOG_ERROR_CLEANUP();
		MemoryContextSwitchTo(oldcontext);
		edata = CopyErrorData();
		FlushErrorState();
		fastpg_pgcore_set_prepared_error(result, edata);
		FreeErrorData(edata);
		if (snapshot_pushed && ActiveSnapshotSet())
			PopActiveSnapshot();
		if (fastpg_catalog_mode_uses_postgres())
			fastpg_pgcore_abort_postgres_catalog_command(&postgres_command_started);
		else
			fastpg_pgcore_release_error_resources();
	}
	PG_END_TRY();
	fastpg_pgcore_end_notice_capture();
	result->notice_count = notice_capture.notice_count;
	result->notices = notice_capture.notices;

	MemoryContextSwitchTo(oldcontext);
	return result;
}

void
fastpg_pgcore_prepared_free(FastPgPgCorePrepared *prepared)
{
	if (prepared == NULL)
		return;

	if (prepared->context != NULL)
		fastpg_pgcore_memory_context_delete(prepared->context);
	free(prepared->message);
	free(prepared->detail);
	free(prepared->hint);
	free(prepared->error_context);
	free(prepared->internal_query);
	free(prepared);
}

bool
fastpg_pgcore_prepared_ok(const FastPgPgCorePrepared *prepared)
{
	return prepared != NULL && prepared->ok;
}

const char *
fastpg_pgcore_prepared_sqlstate(const FastPgPgCorePrepared *prepared)
{
	if (prepared == NULL || prepared->sqlstate[0] == '\0')
		return "XX000";
	return prepared->sqlstate;
}

const char *
fastpg_pgcore_prepared_message(const FastPgPgCorePrepared *prepared)
{
	if (prepared == NULL || prepared->message == NULL)
		return "PostgreSQL prepare error";
	return prepared->message;
}

const char *
fastpg_pgcore_prepared_detail(const FastPgPgCorePrepared *prepared)
{
	if (prepared == NULL || prepared->detail == NULL)
		return "";
	return prepared->detail;
}

const char *
fastpg_pgcore_prepared_hint(const FastPgPgCorePrepared *prepared)
{
	if (prepared == NULL || prepared->hint == NULL)
		return "";
	return prepared->hint;
}

const char *
fastpg_pgcore_prepared_context(const FastPgPgCorePrepared *prepared)
{
	if (prepared == NULL || prepared->error_context == NULL)
		return "";
	return prepared->error_context;
}

int
fastpg_pgcore_prepared_cursorpos(const FastPgPgCorePrepared *prepared)
{
	return prepared != NULL ? prepared->cursorpos : 0;
}

const char *
fastpg_pgcore_prepared_internal_query(const FastPgPgCorePrepared *prepared)
{
	if (prepared == NULL || prepared->internal_query == NULL)
		return "";
	return prepared->internal_query;
}

int
fastpg_pgcore_prepared_internalpos(const FastPgPgCorePrepared *prepared)
{
	return prepared != NULL ? prepared->internalpos : 0;
}

int
fastpg_pgcore_prepared_notice_count(const FastPgPgCorePrepared *prepared)
{
	if (prepared == NULL || prepared->notice_count < 0)
		return 0;
	return prepared->notice_count;
}

static const FastPgPgCoreNotice *
fastpg_pgcore_prepared_notice_at(const FastPgPgCorePrepared *prepared,
								 int notice_index)
{
	if (prepared == NULL ||
		prepared->notices == NULL ||
		notice_index < 0 ||
		notice_index >= prepared->notice_count)
		return NULL;
	return &prepared->notices[notice_index];
}

const char *
fastpg_pgcore_prepared_notice_severity(const FastPgPgCorePrepared *prepared,
									   int notice_index)
{
	const FastPgPgCoreNotice *notice =
		fastpg_pgcore_prepared_notice_at(prepared, notice_index);

	if (notice == NULL || notice->severity == NULL)
		return "NOTICE";
	return notice->severity;
}

const char *
fastpg_pgcore_prepared_notice_sqlstate(const FastPgPgCorePrepared *prepared,
									   int notice_index)
{
	const FastPgPgCoreNotice *notice =
		fastpg_pgcore_prepared_notice_at(prepared, notice_index);

	if (notice == NULL || notice->sqlstate[0] == '\0')
		return "00000";
	return notice->sqlstate;
}

const char *
fastpg_pgcore_prepared_notice_message(const FastPgPgCorePrepared *prepared,
									  int notice_index)
{
	const FastPgPgCoreNotice *notice =
		fastpg_pgcore_prepared_notice_at(prepared, notice_index);

	if (notice == NULL || notice->message == NULL)
		return "";
	return notice->message;
}

const char *
fastpg_pgcore_prepared_notice_detail(const FastPgPgCorePrepared *prepared,
									 int notice_index)
{
	const FastPgPgCoreNotice *notice =
		fastpg_pgcore_prepared_notice_at(prepared, notice_index);

	if (notice == NULL || notice->detail == NULL)
		return "";
	return notice->detail;
}

const char *
fastpg_pgcore_prepared_notice_hint(const FastPgPgCorePrepared *prepared,
								   int notice_index)
{
	const FastPgPgCoreNotice *notice =
		fastpg_pgcore_prepared_notice_at(prepared, notice_index);

	if (notice == NULL || notice->hint == NULL)
		return "";
	return notice->hint;
}

const char *
fastpg_pgcore_prepared_notice_context(const FastPgPgCorePrepared *prepared,
									  int notice_index)
{
	const FastPgPgCoreNotice *notice =
		fastpg_pgcore_prepared_notice_at(prepared, notice_index);

	if (notice == NULL || notice->error_context == NULL)
		return "";
	return notice->error_context;
}

int
fastpg_pgcore_prepared_notice_cursorpos(const FastPgPgCorePrepared *prepared,
										int notice_index)
{
	const FastPgPgCoreNotice *notice =
		fastpg_pgcore_prepared_notice_at(prepared, notice_index);

	if (notice == NULL)
		return 0;
	return notice->cursorpos;
}

int
fastpg_pgcore_prepared_parameter_count(const FastPgPgCorePrepared *prepared)
{
	if (!fastpg_pgcore_prepared_ok(prepared))
		return 0;
	return prepared->parameter_count;
}

unsigned int
fastpg_pgcore_prepared_parameter_type_oid(const FastPgPgCorePrepared *prepared,
										  int index)
{
	if (!fastpg_pgcore_prepared_ok(prepared) ||
		index < 0 ||
		index >= prepared->parameter_count ||
		prepared->parameter_type_oids == NULL)
		return InvalidOid;
	return prepared->parameter_type_oids[index];
}

int
fastpg_pgcore_prepared_field_count(const FastPgPgCorePrepared *prepared)
{
	if (!fastpg_pgcore_prepared_ok(prepared))
		return 0;
	return prepared->field_count;
}

const char *
fastpg_pgcore_prepared_field_name(const FastPgPgCorePrepared *prepared,
								  int index)
{
	if (!fastpg_pgcore_prepared_ok(prepared) ||
		index < 0 ||
		index >= prepared->field_count)
		return NULL;
	return prepared->fields[index].name;
}

unsigned int
fastpg_pgcore_prepared_field_type_oid(const FastPgPgCorePrepared *prepared,
									  int index)
{
	if (!fastpg_pgcore_prepared_ok(prepared) ||
		index < 0 ||
		index >= prepared->field_count)
		return InvalidOid;
	return prepared->fields[index].type_oid;
}

int
fastpg_pgcore_prepared_field_type_modifier(const FastPgPgCorePrepared *prepared,
										   int index)
{
	if (!fastpg_pgcore_prepared_ok(prepared) ||
		index < 0 ||
		index >= prepared->field_count)
		return -1;
	return prepared->fields[index].type_modifier;
}

static ParamListInfo
fastpg_pgcore_build_params(const FastPgPgCorePrepared *prepared,
						   const char *const *parameter_values,
						   const bool *parameter_is_null,
						   const Datum *parameter_datums,
						   const bool *parameter_is_datum,
						   int parameter_count)
{
	ParamListInfo param_list;

	if (parameter_count != prepared->parameter_count)
		ereport(ERROR,
				(errcode(ERRCODE_PROTOCOL_VIOLATION),
				 errmsg("fastpg pgcore expected %d parameters but got %d",
						prepared->parameter_count,
						parameter_count)));
	if (parameter_count == 0)
		return NULL;
	if (prepared->parameter_type_oids == NULL)
		ereport(ERROR,
				(errcode(ERRCODE_PROTOCOL_VIOLATION),
				 errmsg("fastpg pgcore prepared statement has no parameter type table")));
	if (parameter_values == NULL || parameter_is_null == NULL)
		ereport(ERROR,
				(errcode(ERRCODE_PROTOCOL_VIOLATION),
				 errmsg("fastpg pgcore parameter buffers are missing")));
	if (parameter_datums == NULL || parameter_is_datum == NULL)
		ereport(ERROR,
				(errcode(ERRCODE_PROTOCOL_VIOLATION),
				 errmsg("fastpg pgcore datum parameter buffers are missing")));

	param_list = makeParamList(parameter_count);
	for (int i = 0; i < parameter_count; i++)
	{
		ParamExternData *param = &param_list->params[i];
		Oid			parameter_type = prepared->parameter_type_oids[i];

		if (!OidIsValid(parameter_type))
			ereport(ERROR,
					(errcode(ERRCODE_PROTOCOL_VIOLATION),
					 errmsg("fastpg pgcore parameter %d has no inferred type",
							i + 1)));

		param->ptype = parameter_type;
		param->pflags = PARAM_FLAG_CONST;

		if (parameter_is_null[i])
		{
			param->isnull = true;
			param->value = (Datum) 0;
		}
		else if (parameter_is_datum[i])
		{
			param->isnull = false;
			param->value = parameter_datums[i];
		}
		else
		{
			Oid			typinput;
			Oid			typioparam;

			if (parameter_values[i] == NULL)
				ereport(ERROR,
						(errcode(ERRCODE_PROTOCOL_VIOLATION),
						 errmsg("fastpg pgcore parameter %d is missing text data",
								i + 1)));

			getTypeInputInfo(parameter_type, &typinput, &typioparam);
			param->isnull = false;
			param->value =
				OidInputFunctionCall(typinput,
									 (char *) parameter_values[i],
									 typioparam,
									 -1);
		}
	}

	return param_list;
}

FastPgPgCoreExecuteResult *
fastpg_pgcore_execute_params(const FastPgPgCorePrepared *prepared,
							 const char *const *parameter_values,
							 const bool *parameter_is_null,
							 const Datum *parameter_datums,
							 const bool *parameter_is_datum,
							 int parameter_count)
{
	FastPgPgCoreExecuteResult *result;
	MemoryContext oldcontext;
	QueryDesc  *query_desc = NULL;
	DestReceiver *dest = NULL;
	MemoryContext save_portal_context = NULL;
	bool		snapshot_pushed = false;
	bool		portal_context_set = false;
	volatile bool executor_started = false;
	volatile bool postgres_command_started = false;
	bool		use_implicit_block = false;
	bool		postgres_finish_at_end = true;
	volatile int completed_statement_count = 0;

	result = (FastPgPgCoreExecuteResult *) calloc(1, sizeof(FastPgPgCoreExecuteResult));
	if (result == NULL)
		return NULL;

	fastpg_pgcore_enter();
	oldcontext = CurrentMemoryContext;
	result->context = AllocSetContextCreate(TopMemoryContext,
											"fastpg pgcore execute result",
											ALLOCSET_DEFAULT_SIZES);

	if (!fastpg_pgcore_prepared_ok(prepared))
	{
		memcpy(result->sqlstate, fastpg_pgcore_prepared_sqlstate(prepared), 6);
		result->message = fastpg_pgcore_strdup(fastpg_pgcore_prepared_message(prepared));
		result->detail = fastpg_pgcore_strdup(fastpg_pgcore_prepared_detail(prepared));
		result->hint = fastpg_pgcore_strdup(fastpg_pgcore_prepared_hint(prepared));
		result->cursorpos = fastpg_pgcore_prepared_cursorpos(prepared);
		return result;
	}
	save_portal_context = PortalContext;
	if (PortalContext == NULL)
	{
		PortalContext = result->context;
		portal_context_set = true;
	}

	fastpg_pgcore_begin_notice_capture(result, prepared->source_text);
	PG_TRY();
	{
		ListCell   *lc;
		int			statement_index = 0;
		ParamListInfo params;

		if (fastpg_catalog_mode_uses_postgres())
		{
			fastpg_pgcore_start_postgres_catalog_command();
			postgres_command_started = true;
#ifdef USE_FASTPG
			FastPgMemResetCommandTouchedRows();
#endif
		}
		else
			fastpg_pgcore_start_statement_timestamp();

		MemoryContextSwitchTo(result->context);
		params = fastpg_pgcore_build_params(prepared,
											parameter_values,
											parameter_is_null,
											parameter_datums,
											parameter_is_datum,
											parameter_count);
		use_implicit_block = fastpg_catalog_mode_uses_postgres() &&
			list_length(prepared->raw_parsetrees) > 1;
		if (use_implicit_block && prepared->planned_statements == NIL)
		{
			ListCell   *raw_lc;
			int			statement_capacity = 0;
			int			cursor_options;

#ifdef USE_FASTPG
			cursor_options = fastpg_catalog_mode_uses_postgres() ?
				CURSOR_OPT_PARALLEL_OK : 0;
#else
			cursor_options = CURSOR_OPT_PARALLEL_OK;
#endif
			result->statement_count = 0;
			result->statements = NULL;
			foreach(raw_lc, prepared->raw_parsetrees)
			{
				RawStmt    *rawstmt = lfirst_node(RawStmt, raw_lc);
				bool		is_last_raw = lnext(prepared->raw_parsetrees, raw_lc) == NULL;
				bool		is_transaction_stmt =
					rawstmt->stmt != NULL && IsA(rawstmt->stmt, TransactionStmt);
				Query	   *querytree;
				List	   *rewritten;
				List	   *planned;
				ListCell   *planned_lc;

				if (!postgres_command_started)
				{
					fastpg_pgcore_start_postgres_catalog_command();
					postgres_command_started = true;
#ifdef USE_FASTPG
					FastPgMemResetCommandTouchedRows();
#endif
				}

				fastpg_pgcore_report_activity_running(prepared->source_text);
				fastpg_pgcore_reject_if_aborted_transaction(rawstmt->stmt);
				BeginImplicitTransactionBlock();
				if (analyze_requires_snapshot(rawstmt))
				{
					fastpg_pgcore_push_analyze_snapshot();
					snapshot_pushed = true;
				}
				querytree = parse_analyze_fixedparams(rawstmt,
													  prepared->source_text,
													  NULL,
													  0,
													  NULL);
				rewritten = pg_rewrite_query(querytree);
				planned = pg_plan_queries(rewritten,
										  prepared->source_text,
										  cursor_options,
										  NULL);
				if (snapshot_pushed && ActiveSnapshotSet())
				{
					PopActiveSnapshot();
					snapshot_pushed = false;
				}

				foreach(planned_lc, planned)
				{
					PlannedStmt *statement = lfirst_node(PlannedStmt, planned_lc);
					FastPgPgCoreExecuteStatement *summary =
						fastpg_pgcore_next_execute_statement(result,
															 &statement_capacity,
															 &statement_index);

					summary->command_type = statement->commandType;
					summary->has_plan_tree = statement->planTree != NULL;
					if (summary->has_plan_tree)
						summary->plan_tree_tag = nodeTag(statement->planTree);

					if (statement->utilityStmt != NULL)
					{
						fastpg_pgcore_execute_utility(statement,
													  prepared->source_text,
													  params,
													  summary,
													  result->context);
#ifdef USE_FASTPG
						if (fastpg_use_rust_catalog() && !fastpg_rust_xact_is_explicit())
						{
							fastpg_xid_commit();
							fastpg_rust_xact_commit_if_implicit();
							fastpg_storage2_xact_commit_if_implicit();
						}
#endif
						continue;
					}

#ifdef USE_FASTPG
					if (fastpg_use_rust_catalog() &&
						fastpg_pgcore_should_noop_system_catalog_write(statement))
					{
						summary->command_tag =
							(char *) fastpg_pgcore_command_tag_name(statement->commandType);
						continue;
					}
#endif

					fastpg_pgcore_ensure_execution_owner();
					if (statement->commandType != CMD_SELECT && !statement->hasReturning)
						dest = None_Receiver;
					else
						dest = fastpg_pgcore_create_capture_receiver(summary,
																	 result->context);
					query_desc = CreateQueryDesc(statement,
												 prepared->source_text,
												 GetTransactionSnapshot(),
												 InvalidSnapshot,
												 dest,
												 params,
												 NULL,
												 0);

					PushActiveSnapshot(query_desc->snapshot);
					snapshot_pushed = true;
					ExecutorStart(query_desc, 0);
					executor_started = true;
					ExecutorRun(query_desc, ForwardScanDirection, 0);
					fastpg_pgcore_set_processed_count(summary,
													  fastpg_pgcore_command_tag_enum(statement->commandType),
													  query_desc->estate->es_processed);
					ExecutorFinish(query_desc);
					ExecutorEnd(query_desc);
					executor_started = false;
					FreeQueryDesc(query_desc);
					query_desc = NULL;
					PopActiveSnapshot();
					snapshot_pushed = false;
#ifdef USE_FASTPG
					if (fastpg_use_rust_catalog() && !fastpg_rust_xact_is_explicit())
					{
						fastpg_xid_commit();
						fastpg_rust_xact_commit_if_implicit();
						fastpg_storage2_xact_commit_if_implicit();
					}
#endif

					dest->rDestroy(dest);
					dest = NULL;
				}

				if (is_last_raw)
				{
					EndImplicitTransactionBlock();
					fastpg_pgcore_finish_postgres_catalog_command(&postgres_command_started);
					postgres_finish_at_end = false;
				}
				else if (is_transaction_stmt)
					fastpg_pgcore_finish_postgres_catalog_command(&postgres_command_started);
				else
					CommandCounterIncrement();
				completed_statement_count = statement_index;
			}
			result->statement_count = statement_index;
		}
		else
		{
		result->statement_count = list_length(prepared->planned_statements);
		result->statements = palloc0_array(FastPgPgCoreExecuteStatement,
										   result->statement_count);

		foreach(lc, prepared->planned_statements)
		{
			PlannedStmt *statement = lfirst_node(PlannedStmt, lc);
			FastPgPgCoreExecuteStatement *summary =
				&result->statements[statement_index++];
			bool		is_last_statement = lnext(prepared->planned_statements, lc) == NULL;
			bool		is_transaction_stmt =
				fastpg_pgcore_is_transaction_planned_stmt(statement);

			summary->command_type = statement->commandType;
			summary->has_plan_tree = statement->planTree != NULL;
			if (summary->has_plan_tree)
				summary->plan_tree_tag = nodeTag(statement->planTree);

			if (fastpg_catalog_mode_uses_postgres() && !postgres_command_started)
			{
				fastpg_pgcore_start_postgres_catalog_command();
				postgres_command_started = true;
#ifdef USE_FASTPG
				FastPgMemResetCommandTouchedRows();
#endif
			}

			fastpg_pgcore_report_activity_running(prepared->source_text);
			if (fastpg_catalog_mode_uses_postgres() &&
				IsAbortedTransactionBlockState() &&
				!fastpg_pgcore_is_transaction_exit_planned_stmt(statement))
				ereport(ERROR,
						(errcode(ERRCODE_IN_FAILED_SQL_TRANSACTION),
						 errmsg("current transaction is aborted, "
								"commands ignored until end of transaction block")));

			if (use_implicit_block)
				BeginImplicitTransactionBlock();

			if (statement->utilityStmt != NULL)
			{
				fastpg_pgcore_execute_utility(statement,
											  prepared->source_text,
											  params,
											  summary,
											  result->context);
#ifdef USE_FASTPG
				if (fastpg_use_rust_catalog() && !fastpg_rust_xact_is_explicit())
				{
					fastpg_xid_commit();
					fastpg_rust_xact_commit_if_implicit();
					fastpg_storage2_xact_commit_if_implicit();
				}
#endif
				goto finish_statement;
			}

#ifdef USE_FASTPG
			if (fastpg_use_rust_catalog() &&
				fastpg_pgcore_should_noop_system_catalog_write(statement))
			{
				summary->command_tag =
					(char *) fastpg_pgcore_command_tag_name(statement->commandType);
				goto finish_statement;
			}
#endif

			fastpg_pgcore_ensure_execution_owner();
			if (statement->commandType != CMD_SELECT && !statement->hasReturning)
				dest = None_Receiver;
			else
				dest = fastpg_pgcore_create_capture_receiver(summary,
															 result->context);
			query_desc = CreateQueryDesc(statement,
										 prepared->source_text,
										 GetTransactionSnapshot(),
										 InvalidSnapshot,
										 dest,
										 params,
										 NULL,
										 0);

			PushActiveSnapshot(query_desc->snapshot);
			snapshot_pushed = true;
			ExecutorStart(query_desc, 0);
			executor_started = true;
			ExecutorRun(query_desc, ForwardScanDirection, 0);
			fastpg_pgcore_set_processed_count(summary,
											  fastpg_pgcore_command_tag_enum(statement->commandType),
											  query_desc->estate->es_processed);
			ExecutorFinish(query_desc);
			ExecutorEnd(query_desc);
			executor_started = false;
			FreeQueryDesc(query_desc);
			query_desc = NULL;
			PopActiveSnapshot();
			snapshot_pushed = false;
#ifdef USE_FASTPG
			if (fastpg_use_rust_catalog() && !fastpg_rust_xact_is_explicit())
			{
				fastpg_xid_commit();
				fastpg_rust_xact_commit_if_implicit();
				fastpg_storage2_xact_commit_if_implicit();
			}
#endif

			dest->rDestroy(dest);
			dest = NULL;

finish_statement:
			if (fastpg_catalog_mode_uses_postgres())
			{
				if (is_last_statement)
				{
					if (use_implicit_block)
						EndImplicitTransactionBlock();
					fastpg_pgcore_finish_postgres_catalog_command(&postgres_command_started);
					postgres_finish_at_end = false;
				}
				else if (is_transaction_stmt)
					fastpg_pgcore_finish_postgres_catalog_command(&postgres_command_started);
				else
					CommandCounterIncrement();
			}
			completed_statement_count = statement_index;
		}
		}

		MemoryContextSwitchTo(oldcontext);
		if (postgres_finish_at_end)
			fastpg_pgcore_finish_postgres_catalog_command(&postgres_command_started);
		result->ok = true;
	}
	PG_CATCH();
	{
		ErrorData  *edata;

		FASTPG_PGCORE_CATALOG_ERROR_CLEANUP();
		MemoryContextSwitchTo(result->context);
		edata = CopyErrorData();
		FlushErrorState();
		MemoryContextSwitchTo(oldcontext);
		if (dest != NULL)
			dest->rDestroy(dest);
		result->statement_count = completed_statement_count;
#ifdef USE_FASTPG
		if (fastpg_use_rust_catalog() && !fastpg_rust_xact_is_explicit())
		{
			fastpg_xid_rollback();
			fastpg_rust_xact_abort_if_implicit();
			fastpg_storage2_xact_abort_if_implicit();
		}
#endif
		if (query_desc != NULL)
		{
			if (executor_started)
			{
				ExecutorEnd(query_desc);
				executor_started = false;
			}
			FreeQueryDesc(query_desc);
		}
		if (snapshot_pushed)
			PopActiveSnapshot();
		fastpg_pgcore_set_execute_error(result, edata, prepared->source_text);
		FreeErrorData(edata);
		if (fastpg_catalog_mode_uses_postgres())
			fastpg_pgcore_abort_postgres_catalog_command(&postgres_command_started);
		else
			fastpg_pgcore_release_error_resources();
	}
	PG_END_TRY();
	fastpg_pgcore_end_notice_capture();

	if (portal_context_set)
		PortalContext = save_portal_context;
	MemoryContextSwitchTo(oldcontext);
	return result;
}

FastPgPgCoreExecuteResult *
fastpg_pgcore_execute(const FastPgPgCorePrepared *prepared)
{
	return fastpg_pgcore_execute_params(prepared, NULL, NULL, NULL, NULL, 0);
}

FastPgPgCoreExecuteResult *
fastpg_pgcore_execute_simple(const char *query)
{
	FastPgPgCoreExecuteResult *result;
	MemoryContext oldcontext;
	QueryDesc  *query_desc = NULL;
	DestReceiver *dest = NULL;
	MemoryContext save_portal_context = NULL;
	bool		snapshot_pushed = false;
	bool		portal_context_set = false;
	volatile bool executor_started = false;
	volatile bool postgres_command_started = false;
	bool		postgres_finish_at_end = true;
	volatile int completed_statement_count = 0;
	char	   *source_text = NULL;

	result = (FastPgPgCoreExecuteResult *) calloc(1, sizeof(FastPgPgCoreExecuteResult));
	if (result == NULL)
		return NULL;

	fastpg_pgcore_enter();
	oldcontext = CurrentMemoryContext;
	result->context = AllocSetContextCreate(TopMemoryContext,
											"fastpg pgcore simple execute result",
											ALLOCSET_DEFAULT_SIZES);
	save_portal_context = PortalContext;
	if (PortalContext == NULL)
	{
		PortalContext = result->context;
		portal_context_set = true;
	}

	fastpg_pgcore_begin_notice_capture(result, query);
	PG_TRY();
	{
		List	   *raw_parsetrees;
		ListCell   *raw_lc;
		int			raw_count;
		int			cursor_options;
		int			statement_capacity = 0;
		int			statement_index = 0;

		if (query == NULL)
			ereport(ERROR,
					(errcode(ERRCODE_INVALID_PARAMETER_VALUE),
					 errmsg("fastpg simple execution requires a query")));

		if (fastpg_catalog_mode_uses_postgres())
		{
			fastpg_pgcore_start_postgres_catalog_command();
			postgres_command_started = true;
#ifdef USE_FASTPG
			FastPgMemResetCommandTouchedRows();
#endif
		}
		else
			fastpg_pgcore_start_statement_timestamp();

		MemoryContextSwitchTo(result->context);
		source_text = pstrdup(query);
		raw_parsetrees = raw_parser(source_text, RAW_PARSE_DEFAULT);
		raw_count = list_length(raw_parsetrees);
		if (raw_count == 0)
		{
			result->statement_count = 0;
			result->statements = NULL;
			goto simple_execute_done;
		}

#ifdef USE_FASTPG
		cursor_options = fastpg_catalog_mode_uses_postgres() ?
			CURSOR_OPT_PARALLEL_OK : 0;
#else
		cursor_options = CURSOR_OPT_PARALLEL_OK;
#endif
		if (raw_count != 1 && strchr(source_text, '$') != NULL)
			ereport(ERROR,
					(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
					 errmsg("fastpg pgcore does not support parameters in multi-statement queries")));

		result->statement_count = 0;
		result->statements = NULL;
		foreach(raw_lc, raw_parsetrees)
		{
			RawStmt    *rawstmt = lfirst_node(RawStmt, raw_lc);
			bool		is_last_raw = lnext(raw_parsetrees, raw_lc) == NULL;
			bool		is_transaction_stmt =
				rawstmt->stmt != NULL && IsA(rawstmt->stmt, TransactionStmt);
			bool		use_implicit_block = fastpg_catalog_mode_uses_postgres() &&
				raw_count > 1;
			Query	   *querytree;
			List	   *rewritten;
			List	   *planned;
			ListCell   *planned_lc;

			if (fastpg_catalog_mode_uses_postgres() && !postgres_command_started)
			{
				fastpg_pgcore_start_postgres_catalog_command();
				postgres_command_started = true;
#ifdef USE_FASTPG
				FastPgMemResetCommandTouchedRows();
#endif
			}

			fastpg_pgcore_report_activity_running(source_text);
			if (fastpg_catalog_mode_uses_postgres())
				fastpg_pgcore_reject_if_aborted_transaction(rawstmt->stmt);
			if (use_implicit_block)
				BeginImplicitTransactionBlock();
#ifdef USE_FASTPG
			if (fastpg_use_rust_catalog() && !is_transaction_stmt)
				fastpg_pgcore_ensure_execution_owner();
#endif
			if (fastpg_catalog_mode_uses_postgres() &&
				analyze_requires_snapshot(rawstmt))
			{
				fastpg_pgcore_push_analyze_snapshot();
				snapshot_pushed = true;
			}
			querytree = parse_analyze_fixedparams(rawstmt,
												  source_text,
												  NULL,
												  0,
												  NULL);
			rewritten = pg_rewrite_query(querytree);
			planned = pg_plan_queries(rewritten,
									  source_text,
									  cursor_options,
									  NULL);
			if (snapshot_pushed && ActiveSnapshotSet())
			{
				PopActiveSnapshot();
				snapshot_pushed = false;
			}

			foreach(planned_lc, planned)
			{
				PlannedStmt *statement = lfirst_node(PlannedStmt, planned_lc);
				FastPgPgCoreExecuteStatement *summary =
					fastpg_pgcore_next_execute_statement(result,
														 &statement_capacity,
														 &statement_index);

				summary->command_type = statement->commandType;
				summary->has_plan_tree = statement->planTree != NULL;
				if (summary->has_plan_tree)
					summary->plan_tree_tag = nodeTag(statement->planTree);

				if (statement->utilityStmt != NULL)
				{
					fastpg_pgcore_execute_utility(statement,
												  source_text,
												  NULL,
												  summary,
												  result->context);
#ifdef USE_FASTPG
					if (fastpg_use_rust_catalog() && !fastpg_rust_xact_is_explicit())
					{
						fastpg_xid_commit();
						fastpg_rust_xact_commit_if_implicit();
						fastpg_storage2_xact_commit_if_implicit();
					}
#endif
					continue;
				}

#ifdef USE_FASTPG
				if (fastpg_use_rust_catalog() &&
					fastpg_pgcore_should_noop_system_catalog_write(statement))
				{
					summary->command_tag =
						(char *) fastpg_pgcore_command_tag_name(statement->commandType);
#ifdef USE_FASTPG
					if (fastpg_use_rust_catalog() && !fastpg_rust_xact_is_explicit())
					{
						fastpg_xid_commit();
						fastpg_rust_xact_commit_if_implicit();
						fastpg_storage2_xact_commit_if_implicit();
					}
#endif
					continue;
				}
#endif

				fastpg_pgcore_ensure_execution_owner();
				if (statement->commandType != CMD_SELECT && !statement->hasReturning)
					dest = None_Receiver;
				else
					dest = fastpg_pgcore_create_capture_receiver(summary,
																 result->context);
				query_desc = CreateQueryDesc(statement,
											 source_text,
											 GetTransactionSnapshot(),
											 InvalidSnapshot,
											 dest,
											 NULL,
											 NULL,
											 0);

				PushActiveSnapshot(query_desc->snapshot);
				snapshot_pushed = true;
				ExecutorStart(query_desc, 0);
				executor_started = true;
				ExecutorRun(query_desc, ForwardScanDirection, 0);
				fastpg_pgcore_set_processed_count(summary,
												  fastpg_pgcore_command_tag_enum(statement->commandType),
												  query_desc->estate->es_processed);
				ExecutorFinish(query_desc);
				ExecutorEnd(query_desc);
				executor_started = false;
				FreeQueryDesc(query_desc);
				query_desc = NULL;
				PopActiveSnapshot();
				snapshot_pushed = false;
#ifdef USE_FASTPG
				if (fastpg_use_rust_catalog() && !fastpg_rust_xact_is_explicit())
				{
					fastpg_xid_commit();
					fastpg_rust_xact_commit_if_implicit();
					fastpg_storage2_xact_commit_if_implicit();
				}
#endif

				dest->rDestroy(dest);
				dest = NULL;
			}

			if (fastpg_catalog_mode_uses_postgres())
			{
				if (is_last_raw)
				{
					if (use_implicit_block)
						EndImplicitTransactionBlock();
					fastpg_pgcore_finish_postgres_catalog_command(&postgres_command_started);
					postgres_finish_at_end = false;
				}
				else if (is_transaction_stmt)
					fastpg_pgcore_finish_postgres_catalog_command(&postgres_command_started);
				else
					CommandCounterIncrement();
			}
			completed_statement_count = statement_index;
		}
		result->statement_count = statement_index;

simple_execute_done:
		MemoryContextSwitchTo(oldcontext);
		if (postgres_finish_at_end)
			fastpg_pgcore_finish_postgres_catalog_command(&postgres_command_started);
		result->ok = true;
	}
	PG_CATCH();
	{
		ErrorData  *edata;

		FASTPG_PGCORE_CATALOG_ERROR_CLEANUP();
		MemoryContextSwitchTo(result->context);
		edata = CopyErrorData();
		FlushErrorState();
		MemoryContextSwitchTo(oldcontext);
		if (dest != NULL)
			dest->rDestroy(dest);
		result->statement_count = completed_statement_count;
#ifdef USE_FASTPG
		if (fastpg_use_rust_catalog() && !fastpg_rust_xact_is_explicit())
		{
			fastpg_xid_rollback();
			fastpg_rust_xact_abort_if_implicit();
			fastpg_storage2_xact_abort_if_implicit();
		}
#endif
		if (query_desc != NULL)
		{
			if (executor_started)
			{
				ExecutorEnd(query_desc);
				executor_started = false;
			}
			FreeQueryDesc(query_desc);
		}
		if (snapshot_pushed)
			PopActiveSnapshot();
		fastpg_pgcore_set_execute_error(result,
										edata,
										source_text != NULL ? source_text : query);
		FreeErrorData(edata);
		if (fastpg_catalog_mode_uses_postgres())
			fastpg_pgcore_abort_postgres_catalog_command(&postgres_command_started);
		else
			fastpg_pgcore_release_error_resources();
	}
	PG_END_TRY();
	fastpg_pgcore_end_notice_capture();

	if (portal_context_set)
		PortalContext = save_portal_context;
	MemoryContextSwitchTo(oldcontext);
	return result;
}

static int
fastpg_pgcore_copy_buffer_source_cb(void *outbuf, int minread, int maxread)
{
	FastPgPgCoreCopyBuffer *buffer = fastpg_pgcore_active_copy_buffer;
	size_t		remaining;
	size_t		amount;

	(void) minread;

	if (buffer == NULL ||
		buffer->data == NULL ||
		buffer->offset >= buffer->len ||
		maxread <= 0)
		return 0;

	remaining = buffer->len - buffer->offset;
	amount = remaining < (size_t) maxread ? remaining : (size_t) maxread;
	memcpy(outbuf, buffer->data + buffer->offset, amount);
	buffer->offset += amount;
	return (int) amount;
}

static Node *
fastpg_pgcore_transform_copy_where(ParseState *pstate,
								   Relation relation,
								   const CopyStmt *stmt)
{
	ParseNamespaceItem *nsitem;
	Node	   *whereClause;

	nsitem = addRangeTableEntryForRelation(pstate,
										   relation,
										   RowExclusiveLock,
										   NULL,
										   false,
										   false);
	if (stmt->whereClause == NULL)
		return NULL;

	addNSItemToQuery(pstate, nsitem, false, true, true);
	whereClause = transformExpr(pstate,
								stmt->whereClause,
								EXPR_KIND_COPY_WHERE);
	whereClause = coerce_to_boolean(pstate, whereClause, "WHERE");
	assign_expr_collations(pstate, whereClause);
	whereClause = eval_const_expressions(NULL, whereClause);
	whereClause = (Node *) canonicalize_qual((Expr *) whereClause, false);
	whereClause = (Node *) make_ands_implicit((Expr *) whereClause);
	return whereClause;
}

FastPgPgCoreExecuteResult *
fastpg_pgcore_execute_copy_from_stdin(const char *query,
									  const char *data,
									  size_t data_len)
{
	FastPgPgCoreExecuteResult *result;
	MemoryContext oldcontext;
	MemoryContext save_portal_context = NULL;
	volatile bool portal_context_set = false;
	volatile bool postgres_command_started = false;
	volatile bool snapshot_pushed = false;
	Relation	relation = NULL;
	CopyFromState cstate = NULL;
	ParseState *pstate = NULL;
	FastPgPgCoreCopyBuffer buffer;

	result = (FastPgPgCoreExecuteResult *) calloc(1, sizeof(FastPgPgCoreExecuteResult));
	if (result == NULL)
		return NULL;

	fastpg_pgcore_enter();
	oldcontext = CurrentMemoryContext;
	result->context = AllocSetContextCreate(TopMemoryContext,
											"fastpg pgcore copy stdin result",
											ALLOCSET_DEFAULT_SIZES);
	save_portal_context = PortalContext;
	if (PortalContext == NULL)
	{
		PortalContext = result->context;
		portal_context_set = true;
	}

	memset(&buffer, 0, sizeof(buffer));
	buffer.data = data;
	buffer.len = data_len;

	fastpg_pgcore_begin_notice_capture(result, query);
	PG_TRY();
	{
		List	   *raw_parsetrees;
		RawStmt    *rawstmt;
		CopyStmt   *stmt;
		Node	   *whereClause;
		uint64		processed;
		FastPgPgCoreExecuteStatement *summary;

		if (query == NULL)
			ereport(ERROR,
					(errcode(ERRCODE_INVALID_PARAMETER_VALUE),
					 errmsg("fastpg COPY buffer execution requires a query")));
		if (data == NULL && data_len > 0)
			ereport(ERROR,
					(errcode(ERRCODE_INVALID_PARAMETER_VALUE),
					 errmsg("fastpg COPY buffer execution requires data")));

		if (fastpg_catalog_mode_uses_postgres())
		{
			fastpg_pgcore_start_postgres_catalog_command();
			postgres_command_started = true;
#ifdef USE_FASTPG
			FastPgMemResetCommandTouchedRows();
#endif
		}
		else
			fastpg_pgcore_start_statement_timestamp();

		MemoryContextSwitchTo(result->context);
		raw_parsetrees = raw_parser(query, RAW_PARSE_DEFAULT);
		if (list_length(raw_parsetrees) != 1)
			ereport(ERROR,
					(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
					 errmsg("fastpg COPY buffer execution requires exactly one COPY statement")));

		rawstmt = linitial_node(RawStmt, raw_parsetrees);
		if (!IsA(rawstmt->stmt, CopyStmt))
			ereport(ERROR,
					(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
					 errmsg("fastpg COPY buffer execution requires a COPY statement")));
		stmt = (CopyStmt *) rawstmt->stmt;
		if (!fastpg_pgcore_utility_is_copy_stdin_bridge((Node *) stmt))
			ereport(ERROR,
					(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
					 errmsg("fastpg COPY buffer execution only supports COPY relation FROM STDIN")));

		relation = table_openrv(stmt->relation, RowExclusiveLock);
		pstate = make_parsestate(NULL);
		whereClause = fastpg_pgcore_transform_copy_where(pstate, relation, stmt);

		fastpg_pgcore_push_analyze_snapshot();
		snapshot_pushed = true;
		fastpg_pgcore_active_copy_buffer = &buffer;
		cstate = BeginCopyFrom(pstate,
							   relation,
							   whereClause,
							   NULL,
							   false,
							   fastpg_pgcore_copy_buffer_source_cb,
							   stmt->attlist,
							   stmt->options);
		pgstat_progress_update_param(PROGRESS_COPY_TYPE,
									 PROGRESS_COPY_TYPE_PIPE);
		processed = CopyFrom(cstate);
		fastpg_pgcore_active_copy_buffer = NULL;
		EndCopyFrom(cstate);
		cstate = NULL;
		PopActiveSnapshot();
		snapshot_pushed = false;

		table_close(relation, NoLock);
		relation = NULL;
		free_parsestate(pstate);
		pstate = NULL;

		result->statement_count = 1;
		result->statements = palloc0_array(FastPgPgCoreExecuteStatement, 1);
		summary = &result->statements[0];
		summary->command_type = CMD_UTILITY;
		summary->command_tag = "COPY";
		summary->has_processed_count = true;
		summary->processed_count = processed;

		MemoryContextSwitchTo(oldcontext);
		fastpg_pgcore_finish_postgres_catalog_command(&postgres_command_started);
		result->ok = true;
	}
	PG_CATCH();
	{
		ErrorData  *edata;

		FASTPG_PGCORE_CATALOG_ERROR_CLEANUP();
		fastpg_pgcore_active_copy_buffer = NULL;
		if (snapshot_pushed && ActiveSnapshotSet())
			PopActiveSnapshot();
		if (relation != NULL)
			table_close(relation, NoLock);
		if (pstate != NULL)
			free_parsestate(pstate);
		MemoryContextSwitchTo(result->context);
		edata = CopyErrorData();
		FlushErrorState();
		MemoryContextSwitchTo(oldcontext);
		fastpg_pgcore_set_execute_error(result, edata, query);
		FreeErrorData(edata);
		if (fastpg_catalog_mode_uses_postgres())
			fastpg_pgcore_abort_postgres_catalog_command(&postgres_command_started);
		else
			fastpg_pgcore_release_error_resources();
	}
	PG_END_TRY();
	fastpg_pgcore_end_notice_capture();

	if (portal_context_set)
		PortalContext = save_portal_context;
	MemoryContextSwitchTo(oldcontext);
	return result;
}

void
fastpg_pgcore_execute_result_free(FastPgPgCoreExecuteResult *result)
{
	if (result == NULL)
		return;

	if (result->context != NULL)
		fastpg_pgcore_memory_context_delete(result->context);
	free(result->message);
	free(result->detail);
	free(result->hint);
	free(result->error_context);
	free(result->internal_query);
	free(result);
}

static size_t
fastpg_pgcore_byref_datum_len(Datum value, int16 typlen)
{
	Pointer		pointer = DatumGetPointer(value);

	if (pointer == NULL)
		ereport(ERROR,
				(errcode(ERRCODE_NULL_VALUE_NOT_ALLOWED),
				 errmsg("PostgreSQL type input returned a null pointer for a non-null value")));

	if (typlen > 0)
		return (size_t) typlen;
	if (typlen == -1)
		return (size_t) VARSIZE_ANY(pointer);
	if (typlen == -2)
		return strlen((const char *) pointer) + 1;

	ereport(ERROR,
			(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
			 errmsg("fastpg cannot copy by-reference Datum with typlen %d",
					(int) typlen)));
	return 0;
}

FastPgPgCoreInputDatumResult *
fastpg_pgcore_input_text_datum(Oid type_oid, int32 typmod, const char *value_text)
{
	FastPgPgCoreInputDatumResult *result;
	MemoryContext oldcontext;
	MemoryContext input_context = NULL;
	volatile bool postgres_command_started = false;

	result = (FastPgPgCoreInputDatumResult *) calloc(1, sizeof(FastPgPgCoreInputDatumResult));
	if (result == NULL)
		return NULL;

	fastpg_pgcore_enter();
	oldcontext = CurrentMemoryContext;
	if (fastpg_catalog_mode_uses_postgres())
	{
		fastpg_pgcore_start_postgres_catalog_command();
		postgres_command_started = true;
	}
	else
		fastpg_pgcore_ensure_execution_owner();

	PG_TRY();
	{
		Oid			typinput;
		Oid			typioparam;

		if (!OidIsValid(type_oid))
			ereport(ERROR,
					(errcode(ERRCODE_INVALID_PARAMETER_VALUE),
					 errmsg("fastpg cannot input text for invalid type OID")));
		if (value_text == NULL)
			ereport(ERROR,
					(errcode(ERRCODE_INVALID_PARAMETER_VALUE),
					 errmsg("fastpg cannot input a null text pointer as a Datum")));

		input_context = AllocSetContextCreate(TopMemoryContext,
											  "fastpg pgcore datum input",
											  ALLOCSET_DEFAULT_SIZES);
		MemoryContextSwitchTo(input_context);

		getTypeInputInfo(type_oid, &typinput, &typioparam);
		get_typlenbyval(type_oid, &result->typlen, &result->typbyval);
		result->value = OidInputFunctionCall(typinput,
											 (char *) value_text,
											 typioparam,
											 typmod);

		if (!result->typbyval)
		{
			size_t		len = fastpg_pgcore_byref_datum_len(result->value,
															 result->typlen);
			Pointer		pointer = DatumGetPointer(result->value);

			result->payload = (unsigned char *) malloc(len);
			if (result->payload == NULL && len > 0)
				ereport(ERROR,
						(errcode(ERRCODE_OUT_OF_MEMORY),
						 errmsg("out of memory copying PostgreSQL Datum payload")));
			if (len > 0)
				memcpy(result->payload, pointer, len);
			result->value = PointerGetDatum(result->payload);
			result->value_len = len;
		}

		result->ok = true;
		MemoryContextSwitchTo(oldcontext);
		fastpg_pgcore_finish_postgres_catalog_command(&postgres_command_started);
	}
	PG_CATCH();
	{
		ErrorData  *edata;

		FASTPG_PGCORE_CATALOG_ERROR_CLEANUP();
		MemoryContextSwitchTo(oldcontext);
		edata = CopyErrorData();
		FlushErrorState();
	fastpg_pgcore_copy_error(result->sqlstate,
								 &result->message,
								 &result->detail,
								 &result->hint,
								 &result->error_context,
								 &result->cursorpos,
								 edata);
		FreeErrorData(edata);
		if (fastpg_catalog_mode_uses_postgres())
			fastpg_pgcore_abort_postgres_catalog_command(&postgres_command_started);
		else
			fastpg_pgcore_release_error_resources();
	}
	PG_END_TRY();

	MemoryContextSwitchTo(oldcontext);
	if (input_context != NULL)
		fastpg_pgcore_memory_context_delete(input_context);
	return result;
}

void
fastpg_pgcore_input_datum_result_free(FastPgPgCoreInputDatumResult *result)
{
	if (result == NULL)
		return;

	free(result->message);
	free(result->detail);
	free(result->hint);
	free(result->error_context);
	free(result->payload);
	free(result);
}

bool
fastpg_pgcore_input_datum_result_ok(const FastPgPgCoreInputDatumResult *result)
{
	return result != NULL && result->ok;
}

const char *
fastpg_pgcore_input_datum_result_sqlstate(const FastPgPgCoreInputDatumResult *result)
{
	if (result == NULL)
		return "";
	return result->sqlstate;
}

const char *
fastpg_pgcore_input_datum_result_message(const FastPgPgCoreInputDatumResult *result)
{
	if (result == NULL || result->message == NULL)
		return "";
	return result->message;
}

const char *
fastpg_pgcore_input_datum_result_detail(const FastPgPgCoreInputDatumResult *result)
{
	if (result == NULL || result->detail == NULL)
		return "";
	return result->detail;
}

const char *
fastpg_pgcore_input_datum_result_hint(const FastPgPgCoreInputDatumResult *result)
{
	if (result == NULL || result->hint == NULL)
		return "";
	return result->hint;
}

const char *
fastpg_pgcore_input_datum_result_context(const FastPgPgCoreInputDatumResult *result)
{
	if (result == NULL || result->error_context == NULL)
		return "";
	return result->error_context;
}

int
fastpg_pgcore_input_datum_result_cursorpos(const FastPgPgCoreInputDatumResult *result)
{
	if (result == NULL)
		return 0;
	return result->cursorpos;
}

uintptr_t
fastpg_pgcore_input_datum_result_value(const FastPgPgCoreInputDatumResult *result)
{
	if (result == NULL)
		return 0;
	return (uintptr_t) result->value;
}

bool
fastpg_pgcore_input_datum_result_typbyval(const FastPgPgCoreInputDatumResult *result)
{
	return result != NULL && result->typbyval;
}

int16
fastpg_pgcore_input_datum_result_typlen(const FastPgPgCoreInputDatumResult *result)
{
	if (result == NULL)
		return 0;
	return result->typlen;
}

size_t
fastpg_pgcore_input_datum_result_value_len(const FastPgPgCoreInputDatumResult *result)
{
	if (result == NULL)
		return 0;
	return result->value_len;
}

const unsigned char *
fastpg_pgcore_input_datum_result_payload(const FastPgPgCoreInputDatumResult *result)
{
	if (result == NULL)
		return NULL;
	return result->payload;
}

bool
fastpg_pgcore_execute_result_ok(const FastPgPgCoreExecuteResult *result)
{
	return result != NULL && result->ok;
}

const char *
fastpg_pgcore_execute_result_sqlstate(const FastPgPgCoreExecuteResult *result)
{
	if (result == NULL || result->sqlstate[0] == '\0')
		return "XX000";
	return result->sqlstate;
}

const char *
fastpg_pgcore_execute_result_message(const FastPgPgCoreExecuteResult *result)
{
	if (result == NULL || result->message == NULL)
		return "PostgreSQL execute error";
	return result->message;
}

const char *
fastpg_pgcore_execute_result_detail(const FastPgPgCoreExecuteResult *result)
{
	if (result == NULL || result->detail == NULL)
		return "";
	return result->detail;
}

const char *
fastpg_pgcore_execute_result_hint(const FastPgPgCoreExecuteResult *result)
{
	if (result == NULL || result->hint == NULL)
		return "";
	return result->hint;
}

const char *
fastpg_pgcore_execute_result_context(const FastPgPgCoreExecuteResult *result)
{
	if (result == NULL || result->error_context == NULL)
		return "";
	return result->error_context;
}

int
fastpg_pgcore_execute_result_cursorpos(const FastPgPgCoreExecuteResult *result)
{
	return result != NULL ? result->cursorpos : 0;
}

const char *
fastpg_pgcore_execute_result_internal_query(const FastPgPgCoreExecuteResult *result)
{
	if (result == NULL || result->internal_query == NULL)
		return "";
	return result->internal_query;
}

int
fastpg_pgcore_execute_result_internalpos(const FastPgPgCoreExecuteResult *result)
{
	return result != NULL ? result->internalpos : 0;
}

int
fastpg_pgcore_execute_notice_count(const FastPgPgCoreExecuteResult *result)
{
	if (result == NULL || result->notice_count < 0)
		return 0;
	return result->notice_count;
}

static const FastPgPgCoreNotice *
fastpg_pgcore_execute_notice_at(const FastPgPgCoreExecuteResult *result,
								int notice_index)
{
	if (result == NULL ||
		notice_index < 0 ||
		notice_index >= result->notice_count)
		return NULL;
	return &result->notices[notice_index];
}

const char *
fastpg_pgcore_execute_notice_severity(const FastPgPgCoreExecuteResult *result,
									  int notice_index)
{
	const FastPgPgCoreNotice *notice =
		fastpg_pgcore_execute_notice_at(result, notice_index);

	if (notice == NULL || notice->severity == NULL)
		return "NOTICE";
	return notice->severity;
}

const char *
fastpg_pgcore_execute_notice_sqlstate(const FastPgPgCoreExecuteResult *result,
									  int notice_index)
{
	const FastPgPgCoreNotice *notice =
		fastpg_pgcore_execute_notice_at(result, notice_index);

	if (notice == NULL || notice->sqlstate[0] == '\0')
		return "00000";
	return notice->sqlstate;
}

const char *
fastpg_pgcore_execute_notice_message(const FastPgPgCoreExecuteResult *result,
									 int notice_index)
{
	const FastPgPgCoreNotice *notice =
		fastpg_pgcore_execute_notice_at(result, notice_index);

	if (notice == NULL || notice->message == NULL)
		return "";
	return notice->message;
}

const char *
fastpg_pgcore_execute_notice_detail(const FastPgPgCoreExecuteResult *result,
									int notice_index)
{
	const FastPgPgCoreNotice *notice =
		fastpg_pgcore_execute_notice_at(result, notice_index);

	if (notice == NULL || notice->detail == NULL)
		return "";
	return notice->detail;
}

const char *
fastpg_pgcore_execute_notice_hint(const FastPgPgCoreExecuteResult *result,
								  int notice_index)
{
	const FastPgPgCoreNotice *notice =
		fastpg_pgcore_execute_notice_at(result, notice_index);

	if (notice == NULL || notice->hint == NULL)
		return "";
	return notice->hint;
}

const char *
fastpg_pgcore_execute_notice_context(const FastPgPgCoreExecuteResult *result,
									 int notice_index)
{
	const FastPgPgCoreNotice *notice =
		fastpg_pgcore_execute_notice_at(result, notice_index);

	if (notice == NULL || notice->error_context == NULL)
		return "";
	return notice->error_context;
}

int
fastpg_pgcore_execute_notice_cursorpos(const FastPgPgCoreExecuteResult *result,
									   int notice_index)
{
	const FastPgPgCoreNotice *notice =
		fastpg_pgcore_execute_notice_at(result, notice_index);

	if (notice == NULL)
		return 0;
	return notice->cursorpos;
}

int
fastpg_pgcore_execute_statement_count(const FastPgPgCoreExecuteResult *result)
{
	if (result == NULL || result->statement_count < 0)
		return 0;
	return result->statement_count;
}

const char *
fastpg_pgcore_execute_statement_command_tag(const FastPgPgCoreExecuteResult *result,
											int statement_index)
{
	CmdType		command_type;

	if (result == NULL ||
		statement_index < 0 ||
		statement_index >= result->statement_count)
		return "UNKNOWN";
	command_type = result->statements[statement_index].command_type;
	if (command_type != CMD_UTILITY)
		return fastpg_pgcore_command_tag_name(command_type);
	if (result->statements[statement_index].command_tag != NULL)
		return result->statements[statement_index].command_tag;
	return fastpg_pgcore_command_tag_name(command_type);
}

bool
fastpg_pgcore_execute_statement_is_select(const FastPgPgCoreExecuteResult *result,
										  int statement_index)
{
	if (result == NULL ||
		statement_index < 0 ||
		statement_index >= result->statement_count)
		return false;
	return result->statements[statement_index].command_type == CMD_SELECT;
}

int
fastpg_pgcore_execute_statement_column_count(const FastPgPgCoreExecuteResult *result,
											 int statement_index)
{
	if (result == NULL ||
		statement_index < 0 ||
		statement_index >= result->statement_count)
		return 0;
	return result->statements[statement_index].column_count;
}

int
fastpg_pgcore_execute_statement_row_count(const FastPgPgCoreExecuteResult *result,
										  int statement_index)
{
	if (result == NULL ||
		statement_index < 0 ||
		statement_index >= result->statement_count)
		return 0;
	return result->statements[statement_index].row_count;
}

bool
fastpg_pgcore_execute_statement_has_processed_count(const FastPgPgCoreExecuteResult *result,
													int statement_index)
{
	if (result == NULL ||
		statement_index < 0 ||
		statement_index >= result->statement_count)
		return false;
	return result->statements[statement_index].has_processed_count;
}

uint64
fastpg_pgcore_execute_statement_processed_count(const FastPgPgCoreExecuteResult *result,
												int statement_index)
{
	if (!fastpg_pgcore_execute_statement_has_processed_count(result, statement_index))
		return 0;
	return result->statements[statement_index].processed_count;
}

bool
fastpg_pgcore_execute_statement_is_copy_in(const FastPgPgCoreExecuteResult *result,
										   int statement_index)
{
	if (result == NULL ||
		statement_index < 0 ||
		statement_index >= result->statement_count)
		return false;
	return result->statements[statement_index].copy_in;
}

bool
fastpg_pgcore_execute_statement_is_copy_out(const FastPgPgCoreExecuteResult *result,
											int statement_index)
{
	if (result == NULL ||
		statement_index < 0 ||
		statement_index >= result->statement_count)
		return false;
	return result->statements[statement_index].copy_out;
}

int
fastpg_pgcore_execute_statement_copy_out_format(const FastPgPgCoreExecuteResult *result,
												int statement_index)
{
	if (!fastpg_pgcore_execute_statement_is_copy_out(result, statement_index))
		return 0;
	return result->statements[statement_index].copy_out_format;
}

int
fastpg_pgcore_execute_statement_copy_out_columns(const FastPgPgCoreExecuteResult *result,
												 int statement_index)
{
	if (!fastpg_pgcore_execute_statement_is_copy_out(result, statement_index))
		return 0;
	return result->statements[statement_index].copy_out_columns;
}

int
fastpg_pgcore_execute_statement_copy_out_chunk_count(const FastPgPgCoreExecuteResult *result,
													 int statement_index)
{
	if (!fastpg_pgcore_execute_statement_is_copy_out(result, statement_index))
		return 0;
	return result->statements[statement_index].copy_out_chunk_count;
}

const char *
fastpg_pgcore_execute_statement_copy_out_chunk_data(const FastPgPgCoreExecuteResult *result,
													int statement_index,
													int chunk_index)
{
	FastPgPgCoreExecuteStatement *statement;

	if (!fastpg_pgcore_execute_statement_is_copy_out(result, statement_index))
		return NULL;
	statement = &result->statements[statement_index];
	if (statement->copy_out_chunks == NULL ||
		chunk_index < 0 ||
		chunk_index >= statement->copy_out_chunk_count)
		return NULL;
	return statement->copy_out_chunks[chunk_index].data;
}

int
fastpg_pgcore_execute_statement_copy_out_chunk_len(const FastPgPgCoreExecuteResult *result,
												   int statement_index,
												   int chunk_index)
{
	FastPgPgCoreExecuteStatement *statement;

	if (!fastpg_pgcore_execute_statement_is_copy_out(result, statement_index))
		return 0;
	statement = &result->statements[statement_index];
	if (statement->copy_out_chunks == NULL ||
		chunk_index < 0 ||
		chunk_index >= statement->copy_out_chunk_count)
		return 0;
	return statement->copy_out_chunks[chunk_index].len;
}

const char *
fastpg_pgcore_execute_statement_copy_table(const FastPgPgCoreExecuteResult *result,
										   int statement_index)
{
	if (!fastpg_pgcore_execute_statement_is_copy_in(result, statement_index))
		return NULL;
	return result->statements[statement_index].copy_table;
}

unsigned int
fastpg_pgcore_execute_statement_copy_table_oid(const FastPgPgCoreExecuteResult *result,
											   int statement_index)
{
	if (!fastpg_pgcore_execute_statement_is_copy_in(result, statement_index))
		return InvalidOid;
	return result->statements[statement_index].copy_table_oid;
}

int
fastpg_pgcore_execute_statement_copy_relation_column_count(const FastPgPgCoreExecuteResult *result,
														   int statement_index)
{
	if (!fastpg_pgcore_execute_statement_is_copy_in(result, statement_index))
		return 0;
	return result->statements[statement_index].copy_relation_column_count;
}

int
fastpg_pgcore_execute_statement_copy_column_count(const FastPgPgCoreExecuteResult *result,
												  int statement_index)
{
	if (!fastpg_pgcore_execute_statement_is_copy_in(result, statement_index))
		return 0;
	return result->statements[statement_index].copy_column_count;
}

int
fastpg_pgcore_execute_statement_copy_format(const FastPgPgCoreExecuteResult *result,
											int statement_index)
{
	if (!fastpg_pgcore_execute_statement_is_copy_in(result, statement_index))
		return 0;
	return result->statements[statement_index].copy_format;
}

int
fastpg_pgcore_execute_statement_copy_header_line(const FastPgPgCoreExecuteResult *result,
												 int statement_index)
{
	if (!fastpg_pgcore_execute_statement_is_copy_in(result, statement_index))
		return 0;
	return result->statements[statement_index].copy_header_line;
}

int
fastpg_pgcore_execute_statement_copy_on_error(const FastPgPgCoreExecuteResult *result,
											  int statement_index)
{
	if (!fastpg_pgcore_execute_statement_is_copy_in(result, statement_index))
		return 0;
	return result->statements[statement_index].copy_on_error;
}

bool
fastpg_pgcore_execute_statement_copy_freeze(const FastPgPgCoreExecuteResult *result,
											int statement_index)
{
	if (!fastpg_pgcore_execute_statement_is_copy_in(result, statement_index))
		return false;
	return result->statements[statement_index].copy_freeze;
}

bool
fastpg_pgcore_execute_statement_copy_foreign_table(const FastPgPgCoreExecuteResult *result,
												   int statement_index)
{
	if (!fastpg_pgcore_execute_statement_is_copy_in(result, statement_index))
		return false;
	return result->statements[statement_index].copy_foreign_table;
}

bool
fastpg_pgcore_execute_statement_copy_partitioned_table(const FastPgPgCoreExecuteResult *result,
													   int statement_index)
{
	if (!fastpg_pgcore_execute_statement_is_copy_in(result, statement_index))
		return false;
	return result->statements[statement_index].copy_partitioned_table;
}

bool
fastpg_pgcore_execute_statement_copy_has_insert_triggers(const FastPgPgCoreExecuteResult *result,
														 int statement_index)
{
	if (!fastpg_pgcore_execute_statement_is_copy_in(result, statement_index))
		return false;
	return result->statements[statement_index].copy_has_insert_triggers;
}

bool
fastpg_pgcore_execute_statement_copy_has_generated_columns(const FastPgPgCoreExecuteResult *result,
														  int statement_index)
{
	if (!fastpg_pgcore_execute_statement_is_copy_in(result, statement_index))
		return false;
	return result->statements[statement_index].copy_has_generated_columns;
}

const char *
fastpg_pgcore_execute_statement_copy_source_text(const FastPgPgCoreExecuteResult *result,
												 int statement_index)
{
	if (!fastpg_pgcore_execute_statement_is_copy_in(result, statement_index))
		return NULL;
	return result->statements[statement_index].copy_source_text;
}

const char *
fastpg_pgcore_execute_statement_copy_delimiter(const FastPgPgCoreExecuteResult *result,
											   int statement_index)
{
	if (!fastpg_pgcore_execute_statement_is_copy_in(result, statement_index))
		return NULL;
	return result->statements[statement_index].copy_delimiter;
}

const char *
fastpg_pgcore_execute_statement_copy_null_print(const FastPgPgCoreExecuteResult *result,
												int statement_index)
{
	if (!fastpg_pgcore_execute_statement_is_copy_in(result, statement_index))
		return NULL;
	return result->statements[statement_index].copy_null_print;
}

const char *
fastpg_pgcore_execute_statement_copy_default_print(const FastPgPgCoreExecuteResult *result,
												   int statement_index)
{
	if (!fastpg_pgcore_execute_statement_is_copy_in(result, statement_index))
		return NULL;
	return result->statements[statement_index].copy_default_print;
}

const char *
fastpg_pgcore_execute_statement_copy_column_name(const FastPgPgCoreExecuteResult *result,
												 int statement_index,
												 int column_index)
{
	FastPgPgCoreExecuteStatement *statement;

	if (!fastpg_pgcore_execute_statement_is_copy_in(result, statement_index))
		return NULL;
	statement = &result->statements[statement_index];
	if (statement->copy_column_names == NULL ||
		column_index < 0 ||
		column_index >= statement->copy_column_count)
		return NULL;
	return statement->copy_column_names[column_index];
}

int
fastpg_pgcore_execute_statement_copy_column_attnum(const FastPgPgCoreExecuteResult *result,
												   int statement_index,
												   int column_index)
{
	FastPgPgCoreExecuteStatement *statement;

	if (!fastpg_pgcore_execute_statement_is_copy_in(result, statement_index))
		return 0;
	statement = &result->statements[statement_index];
	if (statement->copy_columns == NULL ||
		column_index < 0 ||
		column_index >= statement->copy_column_count)
		return 0;
	return statement->copy_columns[column_index].attnum;
}

unsigned int
fastpg_pgcore_execute_statement_copy_column_type_oid(const FastPgPgCoreExecuteResult *result,
													 int statement_index,
													 int column_index)
{
	FastPgPgCoreExecuteStatement *statement;

	if (!fastpg_pgcore_execute_statement_is_copy_in(result, statement_index))
		return InvalidOid;
	statement = &result->statements[statement_index];
	if (statement->copy_columns == NULL ||
		column_index < 0 ||
		column_index >= statement->copy_column_count)
		return InvalidOid;
	return statement->copy_columns[column_index].type_oid;
}

int
fastpg_pgcore_execute_statement_copy_column_type_modifier(const FastPgPgCoreExecuteResult *result,
														  int statement_index,
														  int column_index)
{
	FastPgPgCoreExecuteStatement *statement;

	if (!fastpg_pgcore_execute_statement_is_copy_in(result, statement_index))
		return -1;
	statement = &result->statements[statement_index];
	if (statement->copy_columns == NULL ||
		column_index < 0 ||
		column_index >= statement->copy_column_count)
		return -1;
	return statement->copy_columns[column_index].type_modifier;
}

const char *
fastpg_pgcore_execute_column_name(const FastPgPgCoreExecuteResult *result,
								  int statement_index,
								  int column_index)
{
	FastPgPgCoreExecuteStatement *statement;

	if (result == NULL ||
		statement_index < 0 ||
		statement_index >= result->statement_count)
		return NULL;
	statement = &result->statements[statement_index];
	if (column_index < 0 || column_index >= statement->column_count)
		return NULL;
	return statement->columns[column_index].name;
}

unsigned int
fastpg_pgcore_execute_column_type_oid(const FastPgPgCoreExecuteResult *result,
									  int statement_index,
									  int column_index)
{
	FastPgPgCoreExecuteStatement *statement;

	if (result == NULL ||
		statement_index < 0 ||
		statement_index >= result->statement_count)
		return InvalidOid;
	statement = &result->statements[statement_index];
	if (column_index < 0 || column_index >= statement->column_count)
		return InvalidOid;
	return statement->columns[column_index].type_oid;
}

int
fastpg_pgcore_execute_column_type_modifier(const FastPgPgCoreExecuteResult *result,
										   int statement_index,
										   int column_index)
{
	FastPgPgCoreExecuteStatement *statement;

	if (result == NULL ||
		statement_index < 0 ||
		statement_index >= result->statement_count)
		return -1;
	statement = &result->statements[statement_index];
	if (column_index < 0 || column_index >= statement->column_count)
		return -1;
	return statement->columns[column_index].type_modifier;
}

bool
fastpg_pgcore_execute_value_is_null(const FastPgPgCoreExecuteResult *result,
									int statement_index,
									int row_index,
									int column_index)
{
	FastPgPgCoreExecuteStatement *statement;

	if (result == NULL ||
		statement_index < 0 ||
		statement_index >= result->statement_count)
		return true;
	statement = &result->statements[statement_index];
	if (row_index < 0 ||
		row_index >= statement->row_count ||
		column_index < 0 ||
		column_index >= statement->column_count)
		return true;
	return statement->rows[row_index].cells[column_index].is_null;
}

const char *
fastpg_pgcore_execute_value_text(const FastPgPgCoreExecuteResult *result,
								 int statement_index,
								 int row_index,
								 int column_index)
{
	FastPgPgCoreExecuteStatement *statement;

	if (fastpg_pgcore_execute_value_is_null(result,
											statement_index,
											row_index,
											column_index))
		return NULL;
	statement = &result->statements[statement_index];
	return statement->rows[row_index].cells[column_index].value_text;
}
