/*-------------------------------------------------------------------------
 *
 * fastpg_parser_probe.c
 *	  Probe fastpg's direct reuse of PostgreSQL's SQL parser.
 *
 * IDENTIFICATION
 *	  src/test/modules/fastpg_parser_probe/fastpg_parser_probe.c
 *
 *-------------------------------------------------------------------------
 */
#include "postgres.h"

#include "fastpg_parser_ffi.h"

#include "fmgr.h"
#include "lib/stringinfo.h"
#include "utils/builtins.h"

PG_MODULE_MAGIC;

PG_FUNCTION_INFO_V1(fastpg_parse_summary);
PG_FUNCTION_INFO_V1(fastpg_analyze_summary);
PG_FUNCTION_INFO_V1(fastpg_rewrite_summary);
PG_FUNCTION_INFO_V1(fastpg_plan_summary);
PG_FUNCTION_INFO_V1(fastpg_execute_summary);

static void
fastpg_append_raw_stmt_summary(StringInfo buf,
							   int index,
							   const FastPgRawStatement *rawstmt)
{
	appendStringInfo(buf, " stmt[%d]={raw=%s,stmt=%s,location=%d,len=%d}",
					 index,
					 fastpg_raw_statement_raw_tag(rawstmt),
					 fastpg_raw_statement_stmt_tag(rawstmt),
					 fastpg_raw_statement_location(rawstmt),
					 fastpg_raw_statement_len(rawstmt));
}

Datum
fastpg_parse_summary(PG_FUNCTION_ARGS)
{
	text	   *input = PG_GETARG_TEXT_PP(0);
	char	   *query_string = text_to_cstring(input);
	FastPgParseResult *result;
	StringInfoData buf;

	initStringInfo(&buf);
	result = fastpg_parser_parse(query_string);

	if (fastpg_parse_result_ok(result))
	{
		int			statement_count = fastpg_parse_statement_count(result);

		appendStringInfo(&buf, "ok statements=%d", statement_count);

		for (int index = 0; index < statement_count; index++)
		{
			const FastPgRawStatement *rawstmt;

			rawstmt = fastpg_parse_statement(result, index);
			fastpg_append_raw_stmt_summary(&buf, index, rawstmt);
		}
	}
	else
	{
		appendStringInfo(&buf, "error sqlstate=%s",
						 fastpg_parse_error_sqlstate(result));
	}

	fastpg_parse_result_free(result);
	PG_FREE_IF_COPY(input, 0);

	PG_RETURN_TEXT_P(cstring_to_text(buf.data));
}

Datum
fastpg_analyze_summary(PG_FUNCTION_ARGS)
{
	text	   *input = PG_GETARG_TEXT_PP(0);
	char	   *query_string = text_to_cstring(input);
	FastPgParseResult *parse_result;
	FastPgAnalyzeResult *analyze_result = NULL;
	StringInfoData buf;

	initStringInfo(&buf);
	parse_result = fastpg_parser_parse(query_string);

	if (!fastpg_parse_result_ok(parse_result))
	{
		appendStringInfo(&buf, "parse_error sqlstate=%s",
						 fastpg_parse_error_sqlstate(parse_result));
	}
	else
	{
		analyze_result = fastpg_parser_analyze(parse_result, 0);

		if (fastpg_analyze_result_ok(analyze_result))
		{
			int			parameter_count = fastpg_analyze_parameter_count(analyze_result);
			int			target_count = fastpg_analyze_target_count(analyze_result);

			appendStringInfo(&buf, "ok command=%s utility=%s utility_stmt=%s",
							 fastpg_analyze_command_tag(analyze_result),
							 fastpg_analyze_is_utility(analyze_result) ? "true" : "false",
							 fastpg_analyze_utility_stmt_tag(analyze_result));

			appendStringInfo(&buf, " params=%d", parameter_count);
			for (int index = 0; index < parameter_count; index++)
				appendStringInfo(&buf, " param[%d]={type_oid=%u}",
								 index,
								 fastpg_analyze_parameter_type_oid(analyze_result, index));

			appendStringInfo(&buf, " targets=%d", target_count);
			for (int index = 0; index < target_count; index++)
				appendStringInfo(&buf, " target[%d]={name=%s,type_oid=%u}",
								 index,
								 fastpg_analyze_target_name(analyze_result, index),
								 fastpg_analyze_target_type_oid(analyze_result, index));
		}
		else
		{
			appendStringInfo(&buf, "analyze_error sqlstate=%s",
							 fastpg_analyze_error_sqlstate(analyze_result));
		}
	}

	if (analyze_result != NULL)
		fastpg_analyze_result_free(analyze_result);
	fastpg_parse_result_free(parse_result);
	PG_FREE_IF_COPY(input, 0);

	PG_RETURN_TEXT_P(cstring_to_text(buf.data));
}

static FastPgAnalyzeResult *
fastpg_parse_and_analyze(const char *query_string,
						 FastPgParseResult **parse_result,
						 StringInfo buf)
{
	FastPgAnalyzeResult *analyze_result = NULL;

	*parse_result = fastpg_parser_parse(query_string);
	if (!fastpg_parse_result_ok(*parse_result))
	{
		appendStringInfo(buf, "parse_error sqlstate=%s",
						 fastpg_parse_error_sqlstate(*parse_result));
		return NULL;
	}

	analyze_result = fastpg_parser_analyze(*parse_result, 0);
	if (!fastpg_analyze_result_ok(analyze_result))
	{
		appendStringInfo(buf, "analyze_error sqlstate=%s",
						 fastpg_analyze_error_sqlstate(analyze_result));
		return analyze_result;
	}

	return analyze_result;
}

static FastPgPlanResult *
fastpg_parse_analyze_rewrite_plan(const char *query_string,
								  FastPgParseResult **parse_result,
								  FastPgAnalyzeResult **analyze_result,
								  FastPgRewriteResult **rewrite_result,
								  StringInfo buf)
{
	FastPgPlanResult *plan_result = NULL;

	*analyze_result = fastpg_parse_and_analyze(query_string, parse_result, buf);
	if (*analyze_result == NULL || !fastpg_analyze_result_ok(*analyze_result))
		return NULL;

	*rewrite_result = fastpg_parser_rewrite(*analyze_result);
	if (!fastpg_rewrite_result_ok(*rewrite_result))
	{
		appendStringInfo(buf, "rewrite_error sqlstate=%s",
						 fastpg_rewrite_error_sqlstate(*rewrite_result));
		return NULL;
	}

	plan_result = fastpg_parser_plan(*rewrite_result);
	if (!fastpg_plan_result_ok(plan_result))
	{
		appendStringInfo(buf, "plan_error sqlstate=%s",
						 fastpg_plan_error_sqlstate(plan_result));
		return plan_result;
	}

	return plan_result;
}

Datum
fastpg_rewrite_summary(PG_FUNCTION_ARGS)
{
	text	   *input = PG_GETARG_TEXT_PP(0);
	char	   *query_string = text_to_cstring(input);
	FastPgParseResult *parse_result = NULL;
	FastPgAnalyzeResult *analyze_result;
	FastPgRewriteResult *rewrite_result = NULL;
	StringInfoData buf;

	initStringInfo(&buf);
	analyze_result = fastpg_parse_and_analyze(query_string, &parse_result, &buf);

	if (analyze_result != NULL && fastpg_analyze_result_ok(analyze_result))
	{
		rewrite_result = fastpg_parser_rewrite(analyze_result);

		if (fastpg_rewrite_result_ok(rewrite_result))
		{
			int			query_count = fastpg_rewrite_query_count(rewrite_result);

			appendStringInfo(&buf, "ok queries=%d", query_count);
			for (int index = 0; index < query_count; index++)
				appendStringInfo(&buf,
								 " query[%d]={command=%s,utility=%s,utility_stmt=%s,targets=%d}",
								 index,
								 fastpg_rewrite_query_command_tag(rewrite_result, index),
								 fastpg_rewrite_query_is_utility(rewrite_result, index) ? "true" : "false",
								 fastpg_rewrite_query_utility_stmt_tag(rewrite_result, index),
								 fastpg_rewrite_query_target_count(rewrite_result, index));
		}
		else
		{
			appendStringInfo(&buf, "rewrite_error sqlstate=%s",
							 fastpg_rewrite_error_sqlstate(rewrite_result));
		}
	}

	if (rewrite_result != NULL)
		fastpg_rewrite_result_free(rewrite_result);
	if (analyze_result != NULL)
		fastpg_analyze_result_free(analyze_result);
	if (parse_result != NULL)
		fastpg_parse_result_free(parse_result);
	PG_FREE_IF_COPY(input, 0);

	PG_RETURN_TEXT_P(cstring_to_text(buf.data));
}

Datum
fastpg_plan_summary(PG_FUNCTION_ARGS)
{
	text	   *input = PG_GETARG_TEXT_PP(0);
	char	   *query_string = text_to_cstring(input);
	FastPgParseResult *parse_result = NULL;
	FastPgAnalyzeResult *analyze_result;
	FastPgRewriteResult *rewrite_result = NULL;
	FastPgPlanResult *plan_result = NULL;
	StringInfoData buf;

	initStringInfo(&buf);
	analyze_result = fastpg_parse_and_analyze(query_string, &parse_result, &buf);

	if (analyze_result != NULL && fastpg_analyze_result_ok(analyze_result))
	{
		rewrite_result = fastpg_parser_rewrite(analyze_result);

		if (!fastpg_rewrite_result_ok(rewrite_result))
		{
			appendStringInfo(&buf, "rewrite_error sqlstate=%s",
							 fastpg_rewrite_error_sqlstate(rewrite_result));
		}
		else
		{
			plan_result = fastpg_parser_plan(rewrite_result);

			if (fastpg_plan_result_ok(plan_result))
			{
				int			statement_count = fastpg_plan_statement_count(plan_result);

				appendStringInfo(&buf, "ok statements=%d", statement_count);
				for (int index = 0; index < statement_count; index++)
					appendStringInfo(&buf,
									 " stmt[%d]={command=%s,plan=%s,utility_stmt=%s,targets=%d,relations=%d}",
									 index,
									 fastpg_plan_statement_command_tag(plan_result, index),
									 fastpg_plan_statement_plan_tree_tag(plan_result, index),
									 fastpg_plan_statement_utility_stmt_tag(plan_result, index),
									 fastpg_plan_statement_target_count(plan_result, index),
									 fastpg_plan_statement_relation_count(plan_result, index));
			}
			else
			{
				appendStringInfo(&buf, "plan_error sqlstate=%s",
								 fastpg_plan_error_sqlstate(plan_result));
			}
		}
	}

	if (plan_result != NULL)
		fastpg_plan_result_free(plan_result);
	if (rewrite_result != NULL)
		fastpg_rewrite_result_free(rewrite_result);
	if (analyze_result != NULL)
		fastpg_analyze_result_free(analyze_result);
	if (parse_result != NULL)
		fastpg_parse_result_free(parse_result);
	PG_FREE_IF_COPY(input, 0);

	PG_RETURN_TEXT_P(cstring_to_text(buf.data));
}

Datum
fastpg_execute_summary(PG_FUNCTION_ARGS)
{
	text	   *input = PG_GETARG_TEXT_PP(0);
	char	   *query_string = text_to_cstring(input);
	FastPgParseResult *parse_result = NULL;
	FastPgAnalyzeResult *analyze_result = NULL;
	FastPgRewriteResult *rewrite_result = NULL;
	FastPgPlanResult *plan_result = NULL;
	FastPgExecuteResult *execute_result = NULL;
	StringInfoData buf;

	initStringInfo(&buf);
	plan_result = fastpg_parse_analyze_rewrite_plan(query_string,
													&parse_result,
													&analyze_result,
													&rewrite_result,
													&buf);

	if (plan_result != NULL && fastpg_plan_result_ok(plan_result))
	{
		execute_result = fastpg_parser_execute(plan_result);

		if (fastpg_execute_result_ok(execute_result))
		{
			int			statement_count = fastpg_execute_statement_count(execute_result);

			appendStringInfo(&buf, "ok statements=%d", statement_count);
			for (int statement_index = 0; statement_index < statement_count; statement_index++)
			{
				int			column_count;
				int			row_count;

				column_count = fastpg_execute_statement_column_count(execute_result,
																	 statement_index);
				row_count = fastpg_execute_statement_row_count(execute_result,
															  statement_index);
				appendStringInfo(&buf,
								 " stmt[%d]={command=%s,plan=%s,columns=%d,rows=%d",
								 statement_index,
								 fastpg_execute_statement_command_tag(execute_result,
																	  statement_index),
								 fastpg_execute_statement_plan_tree_tag(execute_result,
																		statement_index),
								 column_count,
								 row_count);

				for (int column_index = 0; column_index < column_count; column_index++)
					appendStringInfo(&buf,
									 ",column[%d]={name=%s,type_oid=%u}",
									 column_index,
									 fastpg_execute_column_name(execute_result,
																statement_index,
																column_index),
									 fastpg_execute_column_type_oid(execute_result,
																	statement_index,
																	column_index));

				for (int row_index = 0; row_index < row_count; row_index++)
				{
					appendStringInfo(&buf, ",row[%d]=[", row_index);
					for (int column_index = 0; column_index < column_count; column_index++)
					{
						const char *column_name;
						const char *value_text;

						if (column_index > 0)
							appendStringInfoChar(&buf, ',');

						column_name = fastpg_execute_column_name(execute_result,
																 statement_index,
																 column_index);
						value_text = fastpg_execute_value_text(execute_result,
															  statement_index,
															  row_index,
															  column_index);
						appendStringInfo(&buf, "%s=%s",
										 column_name,
										 fastpg_execute_value_is_null(execute_result,
																	  statement_index,
																	  row_index,
																	  column_index) ? "NULL" : value_text);
					}
					appendStringInfoChar(&buf, ']');
				}

				appendStringInfoChar(&buf, '}');
			}
		}
		else
		{
			appendStringInfo(&buf, "execute_error sqlstate=%s",
							 fastpg_execute_error_sqlstate(execute_result));
		}
	}

	if (execute_result != NULL)
		fastpg_execute_result_free(execute_result);
	if (plan_result != NULL)
		fastpg_plan_result_free(plan_result);
	if (rewrite_result != NULL)
		fastpg_rewrite_result_free(rewrite_result);
	if (analyze_result != NULL)
		fastpg_analyze_result_free(analyze_result);
	if (parse_result != NULL)
		fastpg_parse_result_free(parse_result);
	PG_FREE_IF_COPY(input, 0);

	PG_RETURN_TEXT_P(cstring_to_text(buf.data));
}
