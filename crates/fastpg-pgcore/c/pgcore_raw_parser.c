/*-------------------------------------------------------------------------
 *
 * pgcore_raw_parser.c
 *	  Minimal C ABI for using PostgreSQL's raw parser from fastpg's Rust
 *	  single-process server.
 *
 *-------------------------------------------------------------------------
 */
#include "postgres.h"

#include <errno.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "access/session.h"
#include "access/fastpg_catalog.h"
#include "access/transam.h"
#include "access/tupdesc.h"
#include "access/xact.h"
#include "catalog/index.h"
#include "catalog/namespace.h"
#include "catalog/pg_class.h"
#include "catalog/pg_authid.h"
#include "catalog/pg_database.h"
#include "catalog/pg_collation.h"
#include "catalog/pg_language.h"
#include "catalog/pg_namespace.h"
#include "catalog/pg_tablespace.h"
#include "catalog/pg_type.h"
#include "commands/defrem.h"
#include "executor/execdesc.h"
#include "executor/executor.h"
#include "executor/tuptable.h"
#include "fmgr.h"
#include "miscadmin.h"
#include "nodes/nodeFuncs.h"
#include "nodes/bitmapset.h"
#include "nodes/nodes.h"
#include "nodes/params.h"
#include "nodes/parsenodes.h"
#include "nodes/pg_list.h"
#include "nodes/plannodes.h"
#include "nodes/primnodes.h"
#include "nodes/value.h"
#include "parser/analyze.h"
#include "parser/parsetree.h"
#include "parser/parser.h"
#include "pgtime.h"
#include "postmaster/postmaster.h"
#include "storage/fd.h"
#include "tcop/cmdtag.h"
#include "tcop/dest.h"
#include "tcop/pquery.h"
#include "tcop/tcopprot.h"
#include "tcop/utility.h"
#include "utils/elog.h"
#include "utils/guc.h"
#include "utils/inval.h"
#include "utils/lsyscache.h"
#include "utils/memutils.h"
#include "utils/portal.h"
#include "utils/relcache.h"
#include "utils/resowner.h"
#include "utils/snapmgr.h"
#include "utils/snapshot.h"
#include "utils/syscache.h"

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
#endif

typedef struct FastPgPgCoreParseResult
{
	bool		ok;
	int			statement_count;
	char		sqlstate[6];
	char	   *message;
	char	   *detail;
	char	   *hint;
	int			cursorpos;
} FastPgPgCoreParseResult;

typedef struct FastPgPgCoreField
{
	char	   *name;
	Oid			type_oid;
	int32		type_modifier;
	Oid			output_oid;
} FastPgPgCoreField;

typedef struct FastPgPgCorePrepared
{
	MemoryContext context;
	bool		ok;
	char		sqlstate[6];
	char	   *message;
	char	   *detail;
	char	   *hint;
	int			cursorpos;
	char	   *source_text;
	List	   *raw_parsetrees;
	Query	   *query;
	List	   *querytrees;
	List	   *planned_statements;
	Oid		   *parameter_type_oids;
	int			parameter_count;
	FastPgPgCoreField *fields;
	int			field_count;
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
	int			cursorpos;
} FastPgPgCoreNotice;

typedef struct FastPgPgCoreExecuteStatement
{
	CmdType		command_type;
	char	   *command_tag;
	bool		copy_in;
	char	   *copy_table;
	int			copy_column_count;
	char	  **copy_column_names;
	bool		has_plan_tree;
	NodeTag		plan_tree_tag;
	int			column_count;
	FastPgPgCoreField *columns;
	int			row_count;
	FastPgPgCoreExecuteRow *rows;
} FastPgPgCoreExecuteStatement;

typedef struct FastPgPgCoreExecuteResult
{
	MemoryContext context;
	bool		ok;
	char		sqlstate[6];
	char	   *message;
	char	   *detail;
	char	   *hint;
	int			cursorpos;
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

static bool fastpg_pgcore_initialized = false;

static struct
{
	bool		active;
	emit_log_hook_type previous_hook;
	int			previous_log_min_messages;
	FastPgPgCoreNotice *notices;
	int			count;
	int			capacity;
} fastpg_pgcore_notice_capture;

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
fastpg_pgcore_notice_hook(ErrorData *edata)
{
	if (fastpg_pgcore_notice_capture.previous_hook != NULL)
		(*fastpg_pgcore_notice_capture.previous_hook) (edata);

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

static void
fastpg_pgcore_make_directory(const char *path)
{
	if (MakePGDirectory(path) < 0 && errno != EEXIST)
		ereport(ERROR,
				(errcode_for_file_access(),
				 errmsg("could not create directory \"%s\": %m", path)));
}

static void
fastpg_pgcore_configure_data_dir(void)
{
	static char data_dir[MAXPGPATH];
	const char *configured_dir;

	if (DataDir != NULL)
		return;

	configured_dir = getenv("FASTPG_PGDATA");
	if (configured_dir != NULL && configured_dir[0] != '\0')
	{
		if (strlen(configured_dir) >= sizeof(data_dir))
			ereport(ERROR,
					(errmsg("FASTPG_PGDATA path is too long")));
		strlcpy(data_dir, configured_dir, sizeof(data_dir));
		fastpg_pgcore_make_directory(data_dir);
	}
	else
	{
		char		template[MAXPGPATH];

		snprintf(template, sizeof(template),
				 "/private/tmp/fastpg-pgcore-%ld-XXXXXX", (long) getpid());
		if (mkdtemp(template) == NULL)
			ereport(ERROR,
					(errcode_for_file_access(),
					 errmsg("could not create fastpg pgcore data directory: %m")));
		strlcpy(data_dir, template, sizeof(data_dir));
	}

	SetDataDir(data_dir);
	ChangeToDataDir();
	fastpg_pgcore_make_directory("base");
	fastpg_pgcore_make_directory("base/5");
	fastpg_pgcore_make_directory("base/pgsql_tmp");
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
fastpg_pgcore_init_once(void)
{
	if (fastpg_pgcore_initialized)
		return;

	MyProcPid = getpid();
	MemoryContextInit();
	fastpg_pgcore_configure_data_dir();
	InitFileAccess();
	InitTemporaryFileAccess();
	MyDatabaseId = PostgresDbOid;
	MyDatabaseTableSpace = DEFAULTTABLESPACE_OID;
	DatabasePath = pstrdup("base/5");
	InitializeGUCOptions();
	SetConfigOption("track_counts", "off", PGC_SUSET, PGC_S_OVERRIDE);
	fastpg_pgcore_configure_library_paths();
	pg_timezone_initialize();
	RelationCacheInitialize();
	InitCatalogCache();
	InitializeSession();
	EnablePortalManager();
	namespace_search_path = pstrdup("\"$user\", public");
	InitializeSearchPath();

	fastpg_pgcore_initialized = true;
}

static void
fastpg_pgcore_enter(void)
{
	fastpg_pgcore_init_once();
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
	fastpg_pgcore_enter();
	if (fastpg_pgcore_notice_capture.active)
		return;

	fastpg_pgcore_notice_capture_clear();
	fastpg_pgcore_notice_capture.previous_hook = emit_log_hook;
	fastpg_pgcore_notice_capture.previous_log_min_messages =
		log_min_messages[MyBackendType];
	emit_log_hook = fastpg_pgcore_notice_hook;
	if (log_min_messages[MyBackendType] > INFO)
		log_min_messages[MyBackendType] = INFO;
	fastpg_pgcore_notice_capture.active = true;
}

void
fastpg_pgcore_notice_capture_end(void)
{
	if (!fastpg_pgcore_notice_capture.active)
		return;

	emit_log_hook = fastpg_pgcore_notice_capture.previous_hook;
	log_min_messages[MyBackendType] =
		fastpg_pgcore_notice_capture.previous_log_min_messages;
	fastpg_pgcore_notice_capture.previous_hook = NULL;
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

	MyDatabaseId = (Oid) database_oid;
	MyDatabaseTableSpace = DEFAULTTABLESPACE_OID;

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
	InvalidateSystemCaches();
}

static void
fastpg_pgcore_ensure_execution_owner(void)
{
#ifdef USE_FASTPG
	if (!IsUnderPostmaster)
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
	if (!IsUnderPostmaster)
	{
		if (!fastpg_rust_xact_is_explicit())
			FastPgReleaseStandaloneStatementResources(false);
		else
		{
			AtAbort_Portals();
			AtCleanup_Portals();
		}
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
	if (!fastpg_rust_xact_is_explicit())
	{
		FastPgStartStandaloneStatement();
		FastPgSetCurrentTransactionStartTimestampToStatement();
	}
	else
		FastPgEnsureStandaloneTransactionState();
#endif
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
	result->cursorpos = edata->cursorpos;
}

static void
fastpg_pgcore_copy_error(char sqlstate_out[6],
						 char **message_out,
						 char **detail_out,
						 char **hint_out,
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
							 &result->cursorpos,
							 edata);
}

static void
fastpg_pgcore_set_execute_error(FastPgPgCoreExecuteResult *result,
								ErrorData *edata)
{
	result->ok = false;
	fastpg_pgcore_copy_error(result->sqlstate,
							 &result->message,
							 &result->detail,
							 &result->hint,
							 &result->cursorpos,
							 edata);
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

static size_t
fastpg_pgcore_call_relation_column_count(const char *relation_name)
{
	char		sqlstate[6] = "";
	char		message[256] = "";
	size_t		count = 0;
	bool		ok;

#ifdef USE_FASTPG
	ok = fastpg_rust_catalog_relation_column_count(relation_name,
												   &count,
												   sqlstate,
												   sizeof(sqlstate),
												   message,
												   sizeof(message));
#else
	ok = false;
	strlcpy(sqlstate, "0A000", sizeof(sqlstate));
	strlcpy(message, "fastpg pgcore catalog DDL requires a USE_FASTPG build", sizeof(message));
#endif
	if (!ok)
		fastpg_pgcore_raise_rust_catalog_error(sqlstate, message);
	return count;
}

static void
fastpg_pgcore_execute_noop_utility(const char *command_tag,
								   FastPgPgCoreExecuteStatement *summary)
{
	summary->command_tag = (char *) command_tag;
}

static void
fastpg_pgcore_execute_copy_stmt(const CopyStmt *stmt,
								FastPgPgCoreExecuteStatement *summary)
{
	const char *relation_name;

	if (!stmt->is_from ||
		stmt->filename != NULL ||
		stmt->is_program ||
		stmt->query != NULL ||
		stmt->relation == NULL)
		ereport(ERROR,
				(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
				 errmsg("fastpg pgcore only supports COPY relation FROM STDIN")));

	relation_name = fastpg_pgcore_rangevar_name(stmt->relation);
	summary->copy_in = true;
	summary->copy_table = pstrdup(relation_name);
	if (stmt->attlist != NIL)
	{
		ListCell   *lc;
		int			column_index = 0;

		summary->copy_column_count = list_length(stmt->attlist);
		summary->copy_column_names =
			palloc0_array(char *, summary->copy_column_count);
		foreach(lc, stmt->attlist)
			summary->copy_column_names[column_index++] =
				pstrdup(strVal(lfirst(lc)));
	}
	else
		summary->copy_column_count =
			(int) fastpg_pgcore_call_relation_column_count(relation_name);
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
			fastpg_xid_begin();
			fastpg_rust_xact_begin();
			fastpg_storage2_xact_begin();
#endif
			summary->command_tag = "BEGIN";
			break;
		case TRANS_STMT_COMMIT:
#ifdef USE_FASTPG
			fastpg_xid_commit();
			fastpg_rust_xact_commit();
			fastpg_storage2_xact_commit();
			FastPgReleaseStandaloneStatementResources(true);
#endif
			summary->command_tag = "COMMIT";
			break;
		case TRANS_STMT_ROLLBACK:
#ifdef USE_FASTPG
			fastpg_xid_rollback();
			fastpg_rust_xact_abort();
			fastpg_storage2_xact_abort();
			FastPgReleaseStandaloneStatementResources(false);
#endif
			summary->command_tag = "ROLLBACK";
			break;
		case TRANS_STMT_SAVEPOINT:
#ifdef USE_FASTPG
			fastpg_rust_subxact_begin();
			fastpg_storage2_subxact_begin();
#endif
			summary->command_tag = "SAVEPOINT";
			break;
		case TRANS_STMT_RELEASE:
#ifdef USE_FASTPG
			fastpg_rust_subxact_commit();
			fastpg_storage2_subxact_commit();
#endif
			summary->command_tag = "RELEASE";
			break;
		case TRANS_STMT_ROLLBACK_TO:
#ifdef USE_FASTPG
			fastpg_rust_subxact_abort();
			fastpg_rust_subxact_begin();
			fastpg_storage2_subxact_abort();
			fastpg_storage2_subxact_begin();
			FastPgReconcileRelcacheAfterCatalogRollback();
			InvalidateSystemCaches();
			RelationCacheInvalidate(false);
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
		case T_CheckPointStmt:
#ifdef USE_FASTPG
			return !IsUnderPostmaster;
#else
			return false;
#endif
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

static bool
fastpg_pgcore_reindex_param_is_concurrently(const DefElem *opt)
{
	return opt->defname != NULL && strcmp(opt->defname, "concurrently") == 0;
}

static bool
fastpg_pgcore_reindex_stmt_has_concurrently(const ReindexStmt *stmt)
{
	ListCell   *lc;

	foreach(lc, stmt->params)
	{
		DefElem    *opt = (DefElem *) lfirst(lc);

		if (fastpg_pgcore_reindex_param_is_concurrently(opt))
			return true;
	}

	return false;
}

static List *
fastpg_pgcore_reindex_params_without_concurrently(List *params)
{
	List	   *result = NIL;
	ListCell   *lc;

	foreach(lc, params)
	{
		DefElem    *opt = (DefElem *) lfirst(lc);

		if (!fastpg_pgcore_reindex_param_is_concurrently(opt))
			result = lappend(result, opt);
	}

	return result;
}

static bool
fastpg_pgcore_reindex_uses_internal_transactions(const ReindexStmt *stmt)
{
	Oid			relid;
	char		relkind;

	switch (stmt->kind)
	{
		case REINDEX_OBJECT_SCHEMA:
		case REINDEX_OBJECT_SYSTEM:
		case REINDEX_OBJECT_DATABASE:
			return true;
		case REINDEX_OBJECT_INDEX:
		case REINDEX_OBJECT_TABLE:
			break;
		default:
			return false;
	}

	if (stmt->relation == NULL)
		return false;

	relid = RangeVarGetRelid(stmt->relation, NoLock, true);
	if (!OidIsValid(relid))
		return false;

	relkind = get_rel_relkind(relid);
	return (stmt->kind == REINDEX_OBJECT_INDEX &&
			relkind == RELKIND_PARTITIONED_INDEX) ||
		(stmt->kind == REINDEX_OBJECT_TABLE &&
		 relkind == RELKIND_PARTITIONED_TABLE);
}

static void
fastpg_pgcore_reject_internal_transaction_reindex(Node *utility_stmt)
{
	if (IsUnderPostmaster || !IsA(utility_stmt, ReindexStmt))
		return;

	if (!fastpg_pgcore_reindex_uses_internal_transactions((ReindexStmt *) utility_stmt))
		return;

	ereport(ERROR,
			(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
			 errmsg("fastpg pgcore does not support REINDEX commands that run internal transactions")));
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
	PlannedStmt fastpg_statement;
	IndexStmt	fastpg_index_stmt;
	DropStmt	fastpg_drop_stmt;
	ReindexStmt fastpg_reindex_stmt;
	bool		use_fastpg_statement = false;
	DestReceiver *dest = NULL;
	QueryCompletion qc;
	volatile bool snapshot_pushed = false;

	if (fastpg_pgcore_utility_is_copy_stdin_bridge(utility_stmt))
	{
		fastpg_pgcore_execute_copy_stmt((const CopyStmt *) utility_stmt, summary);
		return;
	}

	if (IsA(utility_stmt, TransactionStmt))
	{
		fastpg_pgcore_execute_transaction_stmt((const TransactionStmt *) utility_stmt,
											   summary);
		return;
	}

	if (fastpg_pgcore_should_noop_utility(utility_stmt))
	{
		fastpg_pgcore_execute_noop_utility(CreateCommandName(utility_stmt), summary);
		return;
	}

#ifdef USE_FASTPG
	fastpg_pgcore_reject_internal_transaction_reindex(utility_stmt);

	if (!IsUnderPostmaster &&
		IsA(utility_stmt, IndexStmt) &&
		((IndexStmt *) utility_stmt)->concurrent)
	{
		fastpg_statement = *statement;
		fastpg_index_stmt = *((IndexStmt *) utility_stmt);
		fastpg_index_stmt.concurrent = false;
		fastpg_statement.utilityStmt = (Node *) &fastpg_index_stmt;
		use_fastpg_statement = true;
	}
	else if (!IsUnderPostmaster &&
			 IsA(utility_stmt, DropStmt) &&
			 ((DropStmt *) utility_stmt)->removeType == OBJECT_INDEX &&
			 ((DropStmt *) utility_stmt)->concurrent)
	{
		fastpg_statement = *statement;
		fastpg_drop_stmt = *((DropStmt *) utility_stmt);
		fastpg_drop_stmt.concurrent = false;
		fastpg_statement.utilityStmt = (Node *) &fastpg_drop_stmt;
		use_fastpg_statement = true;
	}
	else if (!IsUnderPostmaster &&
			 IsA(utility_stmt, ReindexStmt) &&
			 fastpg_pgcore_reindex_stmt_has_concurrently((ReindexStmt *) utility_stmt))
	{
		fastpg_statement = *statement;
		fastpg_reindex_stmt = *((ReindexStmt *) utility_stmt);
		fastpg_reindex_stmt.params =
			fastpg_pgcore_reindex_params_without_concurrently(fastpg_reindex_stmt.params);
		fastpg_statement.utilityStmt = (Node *) &fastpg_reindex_stmt;
		use_fastpg_statement = true;
	}
#endif

	InitializeQueryCompletion(&qc);
	dest = fastpg_pgcore_create_capture_receiver(summary, result_context);
	fastpg_pgcore_ensure_execution_owner();

	if (PlannedStmtRequiresSnapshot(use_fastpg_statement ? &fastpg_statement : statement))
	{
		PushActiveSnapshot(GetTransactionSnapshot());
		snapshot_pushed = true;
	}

	PG_TRY();
	{
		ProcessUtility(use_fastpg_statement ? &fastpg_statement : statement,
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

		if (snapshot_pushed)
			PopActiveSnapshot();
		snapshot_pushed = false;
	}
	PG_CATCH();
	{
		if (snapshot_pushed)
			PopActiveSnapshot();
		PG_RE_THROW();
	}
	PG_END_TRY();

	if (qc.commandTag != CMDTAG_UNKNOWN)
		summary->command_tag = (char *) GetCommandTagName(qc.commandTag);
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
		result->fields[field_index].type_oid =
			target->expr != NULL ? exprType((const Node *) target->expr) : InvalidOid;
		result->fields[field_index].type_modifier =
			target->expr != NULL ? exprTypmod((const Node *) target->expr) : -1;
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
		bool		type_is_varlena;

		statement->columns[index].name = pstrdup(NameStr(attr->attname));
		statement->columns[index].type_oid = attr->atttypid;
		statement->columns[index].type_modifier = attr->atttypmod;
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
	receiver->pub.mydest = DestNone;
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
		MemoryContextDelete(parse_context);
		parse_context = NULL;
	}
	PG_CATCH();
	{
		ErrorData  *edata;

		MemoryContextSwitchTo(oldcontext);
		edata = CopyErrorData();
		FlushErrorState();
		fastpg_pgcore_set_error(result, edata);
		FreeErrorData(edata);

		if (parse_context != NULL)
			MemoryContextDelete(parse_context);
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

	result = (FastPgPgCorePrepared *) calloc(1, sizeof(FastPgPgCorePrepared));
	if (result == NULL)
		return NULL;

	fastpg_pgcore_enter();
	fastpg_pgcore_ensure_execution_owner();
	oldcontext = CurrentMemoryContext;
	result->context = AllocSetContextCreate(TopMemoryContext,
											"fastpg pgcore prepared statement",
											ALLOCSET_DEFAULT_SIZES);

	PG_TRY();
	{
		RawStmt    *rawstmt;
		int			raw_count;
		int			cursor_options;

		MemoryContextSwitchTo(result->context);
		result->source_text = pstrdup(query);
		result->raw_parsetrees = raw_parser(query, RAW_PARSE_DEFAULT);
		raw_count = list_length(result->raw_parsetrees);
		if (raw_count == 0)
		{
			result->ok = true;
			MemoryContextSwitchTo(oldcontext);
		}
		else if (raw_count != 1)
		{
			ereport(ERROR,
					(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
					 errmsg("fastpg pgcore currently prepares exactly one statement at a time")));
		}
		else
		{
			rawstmt = linitial_node(RawStmt, result->raw_parsetrees);
#ifdef USE_FASTPG
			cursor_options = 0;
#else
			cursor_options = CURSOR_OPT_PARALLEL_OK;
#endif
			if (strchr(result->source_text, '$') == NULL)
			{
				result->query = parse_analyze_fixedparams(rawstmt,
														 result->source_text,
														 NULL,
														 0,
														 NULL);
			}
			else
			{
				result->query = parse_analyze_varparams(rawstmt,
														result->source_text,
														&result->parameter_type_oids,
														&result->parameter_count,
														NULL);
			}
			fastpg_pgcore_capture_analyze_fields(result);
			result->querytrees = pg_rewrite_query(result->query);
			result->planned_statements = pg_plan_queries(result->querytrees,
														 result->source_text,
														 cursor_options,
														 NULL);
			result->ok = true;
			MemoryContextSwitchTo(oldcontext);
		}
	}
	PG_CATCH();
	{
		ErrorData  *edata;

		MemoryContextSwitchTo(oldcontext);
		edata = CopyErrorData();
		FlushErrorState();
		fastpg_pgcore_set_prepared_error(result, edata);
		FreeErrorData(edata);
		fastpg_pgcore_release_error_resources();
	}
	PG_END_TRY();

	MemoryContextSwitchTo(oldcontext);
	return result;
}

void
fastpg_pgcore_prepared_free(FastPgPgCorePrepared *prepared)
{
	if (prepared == NULL)
		return;

	if (prepared->context != NULL)
		MemoryContextDelete(prepared->context);
	free(prepared->message);
	free(prepared->detail);
	free(prepared->hint);
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

int
fastpg_pgcore_prepared_cursorpos(const FastPgPgCorePrepared *prepared)
{
	return prepared != NULL ? prepared->cursorpos : 0;
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
	bool		snapshot_pushed = false;
	volatile bool executor_started = false;
	Oid			statement_userid = InvalidOid;
	int			statement_sec_context = 0;

	result = (FastPgPgCoreExecuteResult *) calloc(1, sizeof(FastPgPgCoreExecuteResult));
	if (result == NULL)
		return NULL;

	fastpg_pgcore_enter();
	GetUserIdAndSecContext(&statement_userid, &statement_sec_context);
	fastpg_pgcore_start_statement_timestamp();
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

	PG_TRY();
	{
		ListCell   *lc;
		int			statement_index = 0;
		ParamListInfo params;

		MemoryContextSwitchTo(result->context);
		params = fastpg_pgcore_build_params(prepared,
											parameter_values,
											parameter_is_null,
											parameter_datums,
											parameter_is_datum,
											parameter_count);
		result->statement_count = list_length(prepared->planned_statements);
		result->statements = palloc0_array(FastPgPgCoreExecuteStatement,
										   result->statement_count);

		foreach(lc, prepared->planned_statements)
		{
			PlannedStmt *statement = lfirst_node(PlannedStmt, lc);
			FastPgPgCoreExecuteStatement *summary =
				&result->statements[statement_index++];

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
				if (!fastpg_rust_xact_is_explicit())
				{
					fastpg_xid_commit();
					fastpg_rust_xact_commit_if_implicit();
					fastpg_storage2_xact_commit_if_implicit();
				}
#endif
				continue;
			}

#ifdef USE_FASTPG
			if (fastpg_pgcore_should_noop_system_catalog_write(statement))
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
			ExecutorFinish(query_desc);
			ExecutorEnd(query_desc);
			executor_started = false;
			FreeQueryDesc(query_desc);
			query_desc = NULL;
			PopActiveSnapshot();
			snapshot_pushed = false;
#ifdef USE_FASTPG
			if (!fastpg_rust_xact_is_explicit())
			{
				fastpg_xid_commit();
				fastpg_rust_xact_commit_if_implicit();
				fastpg_storage2_xact_commit_if_implicit();
			}
#endif

			dest->rDestroy(dest);
			dest = NULL;

		}

		result->ok = true;
		MemoryContextSwitchTo(oldcontext);
	}
	PG_CATCH();
	{
		ErrorData  *edata;

		MemoryContextSwitchTo(result->context);
		edata = CopyErrorData();
		FlushErrorState();
		MemoryContextSwitchTo(oldcontext);
		if (dest != NULL)
			dest->rDestroy(dest);
#ifdef USE_FASTPG
		if (!IsUnderPostmaster)
			SetUserIdAndSecContext(statement_userid, statement_sec_context);
#endif
#ifdef USE_FASTPG
		if (!fastpg_rust_xact_is_explicit())
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
		fastpg_pgcore_set_execute_error(result, edata);
		FreeErrorData(edata);
		fastpg_pgcore_release_error_resources();
	}
	PG_END_TRY();

	MemoryContextSwitchTo(oldcontext);
	return result;
}

FastPgPgCoreExecuteResult *
fastpg_pgcore_execute(const FastPgPgCorePrepared *prepared)
{
	return fastpg_pgcore_execute_params(prepared, NULL, NULL, NULL, NULL, 0);
}

void
fastpg_pgcore_execute_result_free(FastPgPgCoreExecuteResult *result)
{
	if (result == NULL)
		return;

	if (result->context != NULL)
		MemoryContextDelete(result->context);
	free(result->message);
	free(result->detail);
	free(result->hint);
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

	result = (FastPgPgCoreInputDatumResult *) calloc(1, sizeof(FastPgPgCoreInputDatumResult));
	if (result == NULL)
		return NULL;

	fastpg_pgcore_enter();
	fastpg_pgcore_ensure_execution_owner();
	oldcontext = CurrentMemoryContext;

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
	}
	PG_CATCH();
	{
		ErrorData  *edata;

		MemoryContextSwitchTo(oldcontext);
		edata = CopyErrorData();
		FlushErrorState();
		fastpg_pgcore_copy_error(result->sqlstate,
								 &result->message,
								 &result->detail,
								 &result->hint,
								 &result->cursorpos,
								 edata);
		FreeErrorData(edata);
		fastpg_pgcore_release_error_resources();
	}
	PG_END_TRY();

	MemoryContextSwitchTo(oldcontext);
	if (input_context != NULL)
		MemoryContextDelete(input_context);
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

int
fastpg_pgcore_execute_result_cursorpos(const FastPgPgCoreExecuteResult *result)
{
	return result != NULL ? result->cursorpos : 0;
}

int
fastpg_pgcore_execute_statement_count(const FastPgPgCoreExecuteResult *result)
{
	if (!fastpg_pgcore_execute_result_ok(result))
		return 0;
	return result->statement_count;
}

const char *
fastpg_pgcore_execute_statement_command_tag(const FastPgPgCoreExecuteResult *result,
											int statement_index)
{
	if (!fastpg_pgcore_execute_result_ok(result) ||
		statement_index < 0 ||
		statement_index >= result->statement_count)
		return "UNKNOWN";
	if (result->statements[statement_index].command_tag != NULL)
		return result->statements[statement_index].command_tag;
	return fastpg_pgcore_command_tag_name(result->statements[statement_index].command_type);
}

int
fastpg_pgcore_execute_statement_column_count(const FastPgPgCoreExecuteResult *result,
											 int statement_index)
{
	if (!fastpg_pgcore_execute_result_ok(result) ||
		statement_index < 0 ||
		statement_index >= result->statement_count)
		return 0;
	return result->statements[statement_index].column_count;
}

int
fastpg_pgcore_execute_statement_row_count(const FastPgPgCoreExecuteResult *result,
										  int statement_index)
{
	if (!fastpg_pgcore_execute_result_ok(result) ||
		statement_index < 0 ||
		statement_index >= result->statement_count)
		return 0;
	return result->statements[statement_index].row_count;
}

bool
fastpg_pgcore_execute_statement_is_copy_in(const FastPgPgCoreExecuteResult *result,
										   int statement_index)
{
	if (!fastpg_pgcore_execute_result_ok(result) ||
		statement_index < 0 ||
		statement_index >= result->statement_count)
		return false;
	return result->statements[statement_index].copy_in;
}

const char *
fastpg_pgcore_execute_statement_copy_table(const FastPgPgCoreExecuteResult *result,
										   int statement_index)
{
	if (!fastpg_pgcore_execute_statement_is_copy_in(result, statement_index))
		return NULL;
	return result->statements[statement_index].copy_table;
}

int
fastpg_pgcore_execute_statement_copy_column_count(const FastPgPgCoreExecuteResult *result,
												  int statement_index)
{
	if (!fastpg_pgcore_execute_statement_is_copy_in(result, statement_index))
		return 0;
	return result->statements[statement_index].copy_column_count;
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

const char *
fastpg_pgcore_execute_column_name(const FastPgPgCoreExecuteResult *result,
								  int statement_index,
								  int column_index)
{
	FastPgPgCoreExecuteStatement *statement;

	if (!fastpg_pgcore_execute_result_ok(result) ||
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

	if (!fastpg_pgcore_execute_result_ok(result) ||
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

	if (!fastpg_pgcore_execute_result_ok(result) ||
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

	if (!fastpg_pgcore_execute_result_ok(result) ||
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
