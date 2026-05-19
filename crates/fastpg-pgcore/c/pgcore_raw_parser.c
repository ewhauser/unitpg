/*-------------------------------------------------------------------------
 *
 * pgcore_raw_parser.c
 *	  Minimal C ABI for using PostgreSQL's raw parser from fastpg's Rust
 *	  single-process server.
 *
 *-------------------------------------------------------------------------
 */
#include "postgres.h"

#include <unistd.h>

#include "access/session.h"
#include "access/fastpg_catalog.h"
#include "access/transam.h"
#include "access/tupdesc.h"
#include "access/xact.h"
#include "catalog/index.h"
#include "catalog/namespace.h"
#include "catalog/pg_collation.h"
#include "catalog/pg_language.h"
#include "catalog/pg_namespace.h"
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

typedef struct FastPgPgCoreExecuteStatement
{
	CmdType		command_type;
	char	   *command_tag;
	bool		copy_in;
	char	   *copy_table;
	int			copy_column_count;
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

typedef struct FastPgPgCoreCaptureDestReceiver
{
	DestReceiver pub;
	FastPgPgCoreExecuteStatement *statement;
	MemoryContext context;
} FastPgPgCoreCaptureDestReceiver;

static DestReceiver *fastpg_pgcore_create_capture_receiver(FastPgPgCoreExecuteStatement *statement,
														   MemoryContext context);

static bool fastpg_pgcore_initialized = false;

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
	InitializeGUCOptions();
	pg_timezone_initialize();
	RelationCacheInitialize();
	InitCatalogCache();
	InitializeSession();
	EnablePortalManager();
	namespace_search_path = pstrdup("pg_catalog, public");
	InitializeSearchPath();

	fastpg_pgcore_initialized = true;
}

static void
fastpg_pgcore_enter(void)
{
	fastpg_pgcore_init_once();
	(void) set_stack_base();
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
		FastPgSetCurrentTransactionStartTimestampToStatement();
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
		summary->copy_column_count = list_length(stmt->attlist);
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
#endif
			summary->command_tag = "BEGIN";
			break;
		case TRANS_STMT_COMMIT:
#ifdef USE_FASTPG
			fastpg_xid_commit();
			fastpg_rust_xact_commit();
#endif
			summary->command_tag = "COMMIT";
			break;
		case TRANS_STMT_ROLLBACK:
#ifdef USE_FASTPG
			fastpg_xid_rollback();
			fastpg_rust_xact_abort();
#endif
			summary->command_tag = "ROLLBACK";
			break;
		case TRANS_STMT_SAVEPOINT:
#ifdef USE_FASTPG
			fastpg_rust_subxact_begin();
#endif
			summary->command_tag = "SAVEPOINT";
			break;
		case TRANS_STMT_RELEASE:
#ifdef USE_FASTPG
			fastpg_rust_subxact_commit();
#endif
			summary->command_tag = "RELEASE";
			break;
		case TRANS_STMT_ROLLBACK_TO:
#ifdef USE_FASTPG
			fastpg_rust_subxact_abort();
			fastpg_rust_subxact_begin();
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
		case T_AlterDefaultPrivilegesStmt:
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

	InitializeQueryCompletion(&qc);
	dest = fastpg_pgcore_create_capture_receiver(summary, result_context);
	fastpg_pgcore_ensure_execution_owner();

	if (PlannedStmtRequiresSnapshot(statement))
	{
		PushActiveSnapshot(GetTransactionSnapshot());
		snapshot_pushed = true;
	}

	PG_TRY();
	{
		ProcessUtility(statement,
					   source_text,
					   false,
					   PROCESS_UTILITY_TOPLEVEL,
					   params,
					   NULL,
					   dest,
					   &qc);

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

	result = (FastPgPgCoreExecuteResult *) calloc(1, sizeof(FastPgPgCoreExecuteResult));
	if (result == NULL)
		return NULL;

	fastpg_pgcore_enter();
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
		if (!fastpg_rust_xact_is_explicit())
		{
			fastpg_xid_rollback();
			fastpg_rust_xact_abort_if_implicit();
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
