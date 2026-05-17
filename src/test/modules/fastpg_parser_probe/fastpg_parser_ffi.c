/*-------------------------------------------------------------------------
 *
 * fastpg_parser_ffi.c
 *	  Narrow C ABI for fastpg's direct reuse of PostgreSQL's SQL parser.
 *
 * IDENTIFICATION
 *	  src/test/modules/fastpg_parser_probe/fastpg_parser_ffi.c
 *
 *-------------------------------------------------------------------------
 */
#include "postgres.h"

#include "fastpg_parser_ffi.h"

#include "nodes/nodeFuncs.h"
#include "nodes/nodes.h"
#include "nodes/parsenodes.h"
#include "nodes/pg_list.h"
#include "nodes/primnodes.h"
#include "parser/analyze.h"
#include "tcop/tcopprot.h"
#include "utils/elog.h"
#include "utils/memutils.h"

struct FastPgParseResult
{
	MemoryContext parse_context;
	char	   *source_text;
	List	   *raw_parsetrees;
	char		error_sqlstate[6];
	char	   *error_message;
	int			error_cursorpos;
};

struct FastPgAnalyzeResult
{
	MemoryContext analyze_context;
	char	   *source_text;
	Query	   *query;
	Oid		   *parameter_type_oids;
	int			parameter_count;
	char		error_sqlstate[6];
	char	   *error_message;
	int			error_cursorpos;
};

struct FastPgRewriteResult
{
	MemoryContext rewrite_context;
	char	   *source_text;
	List	   *querytrees;
	char		error_sqlstate[6];
	char	   *error_message;
	int			error_cursorpos;
};

struct FastPgPlanResult
{
	MemoryContext plan_context;
	List	   *planned_statements;
	char		error_sqlstate[6];
	char	   *error_message;
	int			error_cursorpos;
};

static const char *
fastpg_node_tag_name(NodeTag tag)
{
	switch (tag)
	{
		case T_RawStmt:
			return "T_RawStmt";
		case T_SelectStmt:
			return "T_SelectStmt";
		case T_InsertStmt:
			return "T_InsertStmt";
		case T_DeleteStmt:
			return "T_DeleteStmt";
		case T_UpdateStmt:
			return "T_UpdateStmt";
		case T_CreateStmt:
			return "T_CreateStmt";
		case T_TransactionStmt:
			return "T_TransactionStmt";
		case T_VariableSetStmt:
			return "T_VariableSetStmt";
		case T_VariableShowStmt:
			return "T_VariableShowStmt";
		case T_PlannedStmt:
			return "T_PlannedStmt";
		case T_Result:
			return "T_Result";
		case T_ProjectSet:
			return "T_ProjectSet";
		case T_SeqScan:
			return "T_SeqScan";
		case T_IndexScan:
			return "T_IndexScan";
		case T_IndexOnlyScan:
			return "T_IndexOnlyScan";
		case T_BitmapHeapScan:
			return "T_BitmapHeapScan";
		case T_ModifyTable:
			return "T_ModifyTable";
		case T_LockRows:
			return "T_LockRows";
		case T_Limit:
			return "T_Limit";
		case T_Sort:
			return "T_Sort";
		case T_Agg:
			return "T_Agg";
		default:
			return "T_Unknown";
	}
}

static const char *
fastpg_command_tag_name(CmdType command_type)
{
	switch (command_type)
	{
		case CMD_SELECT:
			return "CMD_SELECT";
		case CMD_UPDATE:
			return "CMD_UPDATE";
		case CMD_INSERT:
			return "CMD_INSERT";
		case CMD_DELETE:
			return "CMD_DELETE";
		case CMD_MERGE:
			return "CMD_MERGE";
		case CMD_UTILITY:
			return "CMD_UTILITY";
		case CMD_NOTHING:
			return "CMD_NOTHING";
		case CMD_UNKNOWN:
		default:
			return "CMD_UNKNOWN";
	}
}

static void
fastpg_parse_result_set_error(FastPgParseResult *result,
							  const char *sqlstate,
							  const char *message,
							  int cursorpos)
{
	strlcpy(result->error_sqlstate, sqlstate, sizeof(result->error_sqlstate));
	result->error_message = pstrdup(message);
	result->error_cursorpos = cursorpos;
}

static void
fastpg_analyze_result_set_error(FastPgAnalyzeResult *result,
								const char *sqlstate,
								const char *message,
								int cursorpos)
{
	strlcpy(result->error_sqlstate, sqlstate, sizeof(result->error_sqlstate));
	result->error_message = pstrdup(message);
	result->error_cursorpos = cursorpos;
}

static void
fastpg_rewrite_result_set_error(FastPgRewriteResult *result,
								const char *sqlstate,
								const char *message,
								int cursorpos)
{
	strlcpy(result->error_sqlstate, sqlstate, sizeof(result->error_sqlstate));
	result->error_message = pstrdup(message);
	result->error_cursorpos = cursorpos;
}

static void
fastpg_plan_result_set_error(FastPgPlanResult *result,
							 const char *sqlstate,
							 const char *message,
							 int cursorpos)
{
	strlcpy(result->error_sqlstate, sqlstate, sizeof(result->error_sqlstate));
	result->error_message = pstrdup(message);
	result->error_cursorpos = cursorpos;
}

FastPgParseResult *
fastpg_parser_parse(const char *query_string)
{
	MemoryContext oldcontext = CurrentMemoryContext;
	FastPgParseResult *result;

	result = palloc0_object(FastPgParseResult);
	result->parse_context = AllocSetContextCreate(CurrentMemoryContext,
												  "fastpg parser ffi",
												  ALLOCSET_DEFAULT_SIZES);

	if (query_string == NULL)
	{
		fastpg_parse_result_set_error(result, "22004",
									  "query string must not be NULL",
									  0);
		return result;
	}

	PG_TRY();
	{
		MemoryContextSwitchTo(result->parse_context);
		result->source_text = pstrdup(query_string);
		result->raw_parsetrees = pg_parse_query(query_string);
	}
	PG_CATCH();
	{
		ErrorData  *edata;

		MemoryContextSwitchTo(oldcontext);
		edata = CopyErrorData();
		FlushErrorState();

		fastpg_parse_result_set_error(result,
									  unpack_sql_state(edata->sqlerrcode),
									  edata->message ? edata->message : "",
									  edata->cursorpos);
		FreeErrorData(edata);
	}
	PG_END_TRY();

	MemoryContextSwitchTo(oldcontext);
	return result;
}

void
fastpg_parse_result_free(FastPgParseResult *result)
{
	if (result == NULL)
		return;

	if (result->parse_context != NULL)
		MemoryContextDelete(result->parse_context);
	if (result->error_message != NULL)
		pfree(result->error_message);

	pfree(result);
}

bool
fastpg_parse_result_ok(const FastPgParseResult *result)
{
	return result != NULL && result->error_sqlstate[0] == '\0';
}

int
fastpg_parse_statement_count(const FastPgParseResult *result)
{
	if (!fastpg_parse_result_ok(result))
		return 0;

	return list_length(result->raw_parsetrees);
}

const FastPgRawStatement *
fastpg_parse_statement(const FastPgParseResult *result, int index)
{
	if (!fastpg_parse_result_ok(result) ||
		index < 0 ||
		index >= list_length(result->raw_parsetrees))
		return NULL;

	return (const FastPgRawStatement *) list_nth(result->raw_parsetrees, index);
}

const char *
fastpg_parse_error_sqlstate(const FastPgParseResult *result)
{
	if (result == NULL || fastpg_parse_result_ok(result))
		return NULL;

	return result->error_sqlstate;
}

const char *
fastpg_parse_error_message(const FastPgParseResult *result)
{
	if (result == NULL || fastpg_parse_result_ok(result))
		return NULL;

	return result->error_message;
}

int
fastpg_parse_error_cursorpos(const FastPgParseResult *result)
{
	if (result == NULL || fastpg_parse_result_ok(result))
		return 0;

	return result->error_cursorpos;
}

const char *
fastpg_raw_statement_raw_tag(const FastPgRawStatement *statement)
{
	const RawStmt *rawstmt = (const RawStmt *) statement;

	if (rawstmt == NULL)
		return "NULL";

	return fastpg_node_tag_name(nodeTag(rawstmt));
}

const char *
fastpg_raw_statement_stmt_tag(const FastPgRawStatement *statement)
{
	const RawStmt *rawstmt = (const RawStmt *) statement;

	if (rawstmt == NULL || rawstmt->stmt == NULL)
		return "NULL";

	return fastpg_node_tag_name(nodeTag(rawstmt->stmt));
}

int
fastpg_raw_statement_location(const FastPgRawStatement *statement)
{
	const RawStmt *rawstmt = (const RawStmt *) statement;

	if (rawstmt == NULL)
		return -1;

	return rawstmt->stmt_location;
}

int
fastpg_raw_statement_len(const FastPgRawStatement *statement)
{
	const RawStmt *rawstmt = (const RawStmt *) statement;

	if (rawstmt == NULL)
		return -1;

	return rawstmt->stmt_len;
}

FastPgAnalyzeResult *
fastpg_parser_analyze(FastPgParseResult *parse_result, int statement_index)
{
	MemoryContext oldcontext = CurrentMemoryContext;
	FastPgAnalyzeResult *result;
	RawStmt    *rawstmt;

	result = palloc0_object(FastPgAnalyzeResult);
	result->analyze_context = AllocSetContextCreate(CurrentMemoryContext,
													"fastpg analyze ffi",
													ALLOCSET_DEFAULT_SIZES);

	if (!fastpg_parse_result_ok(parse_result))
	{
		fastpg_analyze_result_set_error(result, "XX000",
										"parse result is not successful",
										0);
		return result;
	}

	rawstmt = (RawStmt *) fastpg_parse_statement(parse_result, statement_index);
	if (rawstmt == NULL)
	{
		fastpg_analyze_result_set_error(result, "2202E",
										"statement index is out of range",
										0);
		return result;
	}

	PG_TRY();
	{
		MemoryContextSwitchTo(result->analyze_context);
		result->source_text = pstrdup(parse_result->source_text);
		result->query = parse_analyze_varparams(rawstmt,
												result->source_text,
												&result->parameter_type_oids,
												&result->parameter_count,
												NULL);
	}
	PG_CATCH();
	{
		ErrorData  *edata;

		MemoryContextSwitchTo(oldcontext);
		edata = CopyErrorData();
		FlushErrorState();

		fastpg_analyze_result_set_error(result,
										unpack_sql_state(edata->sqlerrcode),
										edata->message ? edata->message : "",
										edata->cursorpos);
		FreeErrorData(edata);
	}
	PG_END_TRY();

	MemoryContextSwitchTo(oldcontext);
	return result;
}

void
fastpg_analyze_result_free(FastPgAnalyzeResult *result)
{
	if (result == NULL)
		return;

	if (result->analyze_context != NULL)
		MemoryContextDelete(result->analyze_context);
	if (result->error_message != NULL)
		pfree(result->error_message);

	pfree(result);
}

bool
fastpg_analyze_result_ok(const FastPgAnalyzeResult *result)
{
	return result != NULL && result->error_sqlstate[0] == '\0';
}

const char *
fastpg_analyze_error_sqlstate(const FastPgAnalyzeResult *result)
{
	if (result == NULL || fastpg_analyze_result_ok(result))
		return NULL;

	return result->error_sqlstate;
}

const char *
fastpg_analyze_error_message(const FastPgAnalyzeResult *result)
{
	if (result == NULL || fastpg_analyze_result_ok(result))
		return NULL;

	return result->error_message;
}

int
fastpg_analyze_error_cursorpos(const FastPgAnalyzeResult *result)
{
	if (result == NULL || fastpg_analyze_result_ok(result))
		return 0;

	return result->error_cursorpos;
}

const char *
fastpg_analyze_command_tag(const FastPgAnalyzeResult *result)
{
	if (!fastpg_analyze_result_ok(result) || result->query == NULL)
		return "CMD_UNKNOWN";

	return fastpg_command_tag_name(result->query->commandType);
}

bool
fastpg_analyze_is_utility(const FastPgAnalyzeResult *result)
{
	return fastpg_analyze_result_ok(result) &&
		result->query != NULL &&
		result->query->commandType == CMD_UTILITY;
}

const char *
fastpg_analyze_utility_stmt_tag(const FastPgAnalyzeResult *result)
{
	if (!fastpg_analyze_is_utility(result) || result->query->utilityStmt == NULL)
		return "NULL";

	return fastpg_node_tag_name(nodeTag(result->query->utilityStmt));
}

int
fastpg_analyze_parameter_count(const FastPgAnalyzeResult *result)
{
	if (!fastpg_analyze_result_ok(result))
		return 0;

	return result->parameter_count;
}

unsigned int
fastpg_analyze_parameter_type_oid(const FastPgAnalyzeResult *result, int index)
{
	if (!fastpg_analyze_result_ok(result) ||
		index < 0 ||
		index >= result->parameter_count ||
		result->parameter_type_oids == NULL)
		return InvalidOid;

	return result->parameter_type_oids[index];
}

static const TargetEntry *
fastpg_analyze_target_entry(const FastPgAnalyzeResult *result, int index)
{
	ListCell   *lc;
	int			visible_index = 0;

	if (!fastpg_analyze_result_ok(result) || result->query == NULL || index < 0)
		return NULL;

	foreach(lc, result->query->targetList)
	{
		const TargetEntry *target = lfirst_node(TargetEntry, lc);

		if (target->resjunk)
			continue;

		if (visible_index == index)
			return target;

		visible_index++;
	}

	return NULL;
}

int
fastpg_analyze_target_count(const FastPgAnalyzeResult *result)
{
	ListCell   *lc;
	int			target_count = 0;

	if (!fastpg_analyze_result_ok(result) || result->query == NULL)
		return 0;

	foreach(lc, result->query->targetList)
	{
		const TargetEntry *target = lfirst_node(TargetEntry, lc);

		if (!target->resjunk)
			target_count++;
	}

	return target_count;
}

const char *
fastpg_analyze_target_name(const FastPgAnalyzeResult *result, int index)
{
	const TargetEntry *target = fastpg_analyze_target_entry(result, index);

	if (target == NULL || target->resname == NULL)
		return "";

	return target->resname;
}

unsigned int
fastpg_analyze_target_type_oid(const FastPgAnalyzeResult *result, int index)
{
	const TargetEntry *target = fastpg_analyze_target_entry(result, index);

	if (target == NULL || target->expr == NULL)
		return InvalidOid;

	return exprType((const Node *) target->expr);
}

FastPgRewriteResult *
fastpg_parser_rewrite(FastPgAnalyzeResult *analyze_result)
{
	MemoryContext oldcontext = CurrentMemoryContext;
	FastPgRewriteResult *result;

	result = palloc0_object(FastPgRewriteResult);
	result->rewrite_context = AllocSetContextCreate(CurrentMemoryContext,
													"fastpg rewrite ffi",
													ALLOCSET_DEFAULT_SIZES);

	if (!fastpg_analyze_result_ok(analyze_result) || analyze_result->query == NULL)
	{
		fastpg_rewrite_result_set_error(result, "XX000",
										"analyze result is not successful",
										0);
		return result;
	}

	PG_TRY();
	{
		Query	   *query_copy;

		MemoryContextSwitchTo(result->rewrite_context);
		result->source_text = pstrdup(analyze_result->source_text);
		query_copy = copyObject(analyze_result->query);
		result->querytrees = pg_rewrite_query(query_copy);
	}
	PG_CATCH();
	{
		ErrorData  *edata;

		MemoryContextSwitchTo(oldcontext);
		edata = CopyErrorData();
		FlushErrorState();

		fastpg_rewrite_result_set_error(result,
										unpack_sql_state(edata->sqlerrcode),
										edata->message ? edata->message : "",
										edata->cursorpos);
		FreeErrorData(edata);
	}
	PG_END_TRY();

	MemoryContextSwitchTo(oldcontext);
	return result;
}

void
fastpg_rewrite_result_free(FastPgRewriteResult *result)
{
	if (result == NULL)
		return;

	if (result->rewrite_context != NULL)
		MemoryContextDelete(result->rewrite_context);
	if (result->error_message != NULL)
		pfree(result->error_message);

	pfree(result);
}

bool
fastpg_rewrite_result_ok(const FastPgRewriteResult *result)
{
	return result != NULL && result->error_sqlstate[0] == '\0';
}

const char *
fastpg_rewrite_error_sqlstate(const FastPgRewriteResult *result)
{
	if (result == NULL || fastpg_rewrite_result_ok(result))
		return NULL;

	return result->error_sqlstate;
}

const char *
fastpg_rewrite_error_message(const FastPgRewriteResult *result)
{
	if (result == NULL || fastpg_rewrite_result_ok(result))
		return NULL;

	return result->error_message;
}

int
fastpg_rewrite_error_cursorpos(const FastPgRewriteResult *result)
{
	if (result == NULL || fastpg_rewrite_result_ok(result))
		return 0;

	return result->error_cursorpos;
}

static const Query *
fastpg_rewrite_query_at(const FastPgRewriteResult *result, int index)
{
	if (!fastpg_rewrite_result_ok(result) ||
		index < 0 ||
		index >= list_length(result->querytrees))
		return NULL;

	return list_nth_node(Query, result->querytrees, index);
}

int
fastpg_rewrite_query_count(const FastPgRewriteResult *result)
{
	if (!fastpg_rewrite_result_ok(result))
		return 0;

	return list_length(result->querytrees);
}

const char *
fastpg_rewrite_query_command_tag(const FastPgRewriteResult *result, int index)
{
	const Query *query = fastpg_rewrite_query_at(result, index);

	if (query == NULL)
		return "CMD_UNKNOWN";

	return fastpg_command_tag_name(query->commandType);
}

bool
fastpg_rewrite_query_is_utility(const FastPgRewriteResult *result, int index)
{
	const Query *query = fastpg_rewrite_query_at(result, index);

	return query != NULL && query->commandType == CMD_UTILITY;
}

const char *
fastpg_rewrite_query_utility_stmt_tag(const FastPgRewriteResult *result, int index)
{
	const Query *query = fastpg_rewrite_query_at(result, index);

	if (query == NULL ||
		query->commandType != CMD_UTILITY ||
		query->utilityStmt == NULL)
		return "NULL";

	return fastpg_node_tag_name(nodeTag(query->utilityStmt));
}

int
fastpg_rewrite_query_target_count(const FastPgRewriteResult *result, int index)
{
	const Query *query = fastpg_rewrite_query_at(result, index);
	ListCell   *lc;
	int			target_count = 0;

	if (query == NULL)
		return 0;

	foreach(lc, query->targetList)
	{
		const TargetEntry *target = lfirst_node(TargetEntry, lc);

		if (!target->resjunk)
			target_count++;
	}

	return target_count;
}

FastPgPlanResult *
fastpg_parser_plan(FastPgRewriteResult *rewrite_result)
{
	MemoryContext oldcontext = CurrentMemoryContext;
	FastPgPlanResult *result;

	result = palloc0_object(FastPgPlanResult);
	result->plan_context = AllocSetContextCreate(CurrentMemoryContext,
												 "fastpg plan ffi",
												 ALLOCSET_DEFAULT_SIZES);

	if (!fastpg_rewrite_result_ok(rewrite_result))
	{
		fastpg_plan_result_set_error(result, "XX000",
									 "rewrite result is not successful",
									 0);
		return result;
	}

	PG_TRY();
	{
		List	   *querytrees_copy;

		MemoryContextSwitchTo(result->plan_context);
		querytrees_copy = copyObject(rewrite_result->querytrees);
		result->planned_statements = pg_plan_queries(querytrees_copy,
													 rewrite_result->source_text,
													 CURSOR_OPT_PARALLEL_OK,
													 NULL);
	}
	PG_CATCH();
	{
		ErrorData  *edata;

		MemoryContextSwitchTo(oldcontext);
		edata = CopyErrorData();
		FlushErrorState();

		fastpg_plan_result_set_error(result,
									 unpack_sql_state(edata->sqlerrcode),
									 edata->message ? edata->message : "",
									 edata->cursorpos);
		FreeErrorData(edata);
	}
	PG_END_TRY();

	MemoryContextSwitchTo(oldcontext);
	return result;
}

void
fastpg_plan_result_free(FastPgPlanResult *result)
{
	if (result == NULL)
		return;

	if (result->plan_context != NULL)
		MemoryContextDelete(result->plan_context);
	if (result->error_message != NULL)
		pfree(result->error_message);

	pfree(result);
}

bool
fastpg_plan_result_ok(const FastPgPlanResult *result)
{
	return result != NULL && result->error_sqlstate[0] == '\0';
}

const char *
fastpg_plan_error_sqlstate(const FastPgPlanResult *result)
{
	if (result == NULL || fastpg_plan_result_ok(result))
		return NULL;

	return result->error_sqlstate;
}

const char *
fastpg_plan_error_message(const FastPgPlanResult *result)
{
	if (result == NULL || fastpg_plan_result_ok(result))
		return NULL;

	return result->error_message;
}

int
fastpg_plan_error_cursorpos(const FastPgPlanResult *result)
{
	if (result == NULL || fastpg_plan_result_ok(result))
		return 0;

	return result->error_cursorpos;
}

static const PlannedStmt *
fastpg_plan_statement_at(const FastPgPlanResult *result, int index)
{
	if (!fastpg_plan_result_ok(result) ||
		index < 0 ||
		index >= list_length(result->planned_statements))
		return NULL;

	return list_nth_node(PlannedStmt, result->planned_statements, index);
}

int
fastpg_plan_statement_count(const FastPgPlanResult *result)
{
	if (!fastpg_plan_result_ok(result))
		return 0;

	return list_length(result->planned_statements);
}

const char *
fastpg_plan_statement_command_tag(const FastPgPlanResult *result, int index)
{
	const PlannedStmt *statement = fastpg_plan_statement_at(result, index);

	if (statement == NULL)
		return "CMD_UNKNOWN";

	return fastpg_command_tag_name(statement->commandType);
}

const char *
fastpg_plan_statement_plan_tree_tag(const FastPgPlanResult *result, int index)
{
	const PlannedStmt *statement = fastpg_plan_statement_at(result, index);

	if (statement == NULL || statement->planTree == NULL)
		return "NULL";

	return fastpg_node_tag_name(nodeTag(statement->planTree));
}

const char *
fastpg_plan_statement_utility_stmt_tag(const FastPgPlanResult *result, int index)
{
	const PlannedStmt *statement = fastpg_plan_statement_at(result, index);

	if (statement == NULL || statement->utilityStmt == NULL)
		return "NULL";

	return fastpg_node_tag_name(nodeTag(statement->utilityStmt));
}

int
fastpg_plan_statement_target_count(const FastPgPlanResult *result, int index)
{
	const PlannedStmt *statement = fastpg_plan_statement_at(result, index);
	ListCell   *lc;
	int			target_count = 0;

	if (statement == NULL || statement->planTree == NULL)
		return 0;

	foreach(lc, statement->planTree->targetlist)
	{
		const TargetEntry *target = lfirst_node(TargetEntry, lc);

		if (!target->resjunk)
			target_count++;
	}

	return target_count;
}

int
fastpg_plan_statement_relation_count(const FastPgPlanResult *result, int index)
{
	const PlannedStmt *statement = fastpg_plan_statement_at(result, index);

	if (statement == NULL)
		return 0;

	return list_length(statement->rtable);
}
