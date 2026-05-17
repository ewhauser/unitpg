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
