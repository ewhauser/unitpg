/*-------------------------------------------------------------------------
 *
 * fastpg_parser_ffi.h
 *	  Narrow C ABI for fastpg's direct reuse of PostgreSQL's SQL parser.
 *
 * This header intentionally exposes opaque handles instead of PostgreSQL node
 * structs, so Rust can hold parser results without depending on RawStmt layout.
 *
 * IDENTIFICATION
 *	  src/test/modules/fastpg_parser_probe/fastpg_parser_ffi.h
 *
 *-------------------------------------------------------------------------
 */
#ifndef FASTPG_PARSER_FFI_H
#define FASTPG_PARSER_FFI_H

#include <stdbool.h>

#ifndef PGDLLEXPORT
#define PGDLLEXPORT
#endif

#ifdef __cplusplus
extern "C" {
#endif

typedef struct FastPgParseResult FastPgParseResult;
typedef struct FastPgRawStatement FastPgRawStatement;
typedef struct FastPgAnalyzeResult FastPgAnalyzeResult;

extern PGDLLEXPORT FastPgParseResult *fastpg_parser_parse(const char *query_string);
extern PGDLLEXPORT void fastpg_parse_result_free(FastPgParseResult *result);

extern PGDLLEXPORT bool fastpg_parse_result_ok(const FastPgParseResult *result);
extern PGDLLEXPORT int fastpg_parse_statement_count(const FastPgParseResult *result);
extern PGDLLEXPORT const FastPgRawStatement *fastpg_parse_statement(const FastPgParseResult *result,
																	int index);

extern PGDLLEXPORT const char *fastpg_parse_error_sqlstate(const FastPgParseResult *result);
extern PGDLLEXPORT const char *fastpg_parse_error_message(const FastPgParseResult *result);
extern PGDLLEXPORT int fastpg_parse_error_cursorpos(const FastPgParseResult *result);

extern PGDLLEXPORT const char *fastpg_raw_statement_raw_tag(const FastPgRawStatement *statement);
extern PGDLLEXPORT const char *fastpg_raw_statement_stmt_tag(const FastPgRawStatement *statement);
extern PGDLLEXPORT int fastpg_raw_statement_location(const FastPgRawStatement *statement);
extern PGDLLEXPORT int fastpg_raw_statement_len(const FastPgRawStatement *statement);

extern PGDLLEXPORT FastPgAnalyzeResult *fastpg_parser_analyze(FastPgParseResult *parse_result,
															  int statement_index);
extern PGDLLEXPORT void fastpg_analyze_result_free(FastPgAnalyzeResult *result);

extern PGDLLEXPORT bool fastpg_analyze_result_ok(const FastPgAnalyzeResult *result);
extern PGDLLEXPORT const char *fastpg_analyze_error_sqlstate(const FastPgAnalyzeResult *result);
extern PGDLLEXPORT const char *fastpg_analyze_error_message(const FastPgAnalyzeResult *result);
extern PGDLLEXPORT int fastpg_analyze_error_cursorpos(const FastPgAnalyzeResult *result);

extern PGDLLEXPORT const char *fastpg_analyze_command_tag(const FastPgAnalyzeResult *result);
extern PGDLLEXPORT bool fastpg_analyze_is_utility(const FastPgAnalyzeResult *result);
extern PGDLLEXPORT const char *fastpg_analyze_utility_stmt_tag(const FastPgAnalyzeResult *result);

extern PGDLLEXPORT int fastpg_analyze_parameter_count(const FastPgAnalyzeResult *result);
extern PGDLLEXPORT unsigned int fastpg_analyze_parameter_type_oid(const FastPgAnalyzeResult *result,
																  int index);

extern PGDLLEXPORT int fastpg_analyze_target_count(const FastPgAnalyzeResult *result);
extern PGDLLEXPORT const char *fastpg_analyze_target_name(const FastPgAnalyzeResult *result,
														  int index);
extern PGDLLEXPORT unsigned int fastpg_analyze_target_type_oid(const FastPgAnalyzeResult *result,
															   int index);

#ifdef __cplusplus
}
#endif

#endif							/* FASTPG_PARSER_FFI_H */
