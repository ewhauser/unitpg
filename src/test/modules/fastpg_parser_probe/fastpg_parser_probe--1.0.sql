/* src/test/modules/fastpg_parser_probe/fastpg_parser_probe--1.0.sql */

-- complain if script is sourced in psql, rather than via CREATE EXTENSION
\echo Use "CREATE EXTENSION fastpg_parser_probe" to load this file. \quit

CREATE FUNCTION fastpg_parse_summary(query pg_catalog.text)
	RETURNS pg_catalog.text
	AS 'MODULE_PATHNAME' LANGUAGE C STRICT PARALLEL SAFE;

CREATE FUNCTION fastpg_analyze_summary(query pg_catalog.text)
	RETURNS pg_catalog.text
	AS 'MODULE_PATHNAME' LANGUAGE C STRICT PARALLEL SAFE;

CREATE FUNCTION fastpg_rewrite_summary(query pg_catalog.text)
	RETURNS pg_catalog.text
	AS 'MODULE_PATHNAME' LANGUAGE C STRICT PARALLEL SAFE;

CREATE FUNCTION fastpg_plan_summary(query pg_catalog.text)
	RETURNS pg_catalog.text
	AS 'MODULE_PATHNAME' LANGUAGE C STRICT PARALLEL SAFE;

CREATE FUNCTION fastpg_execute_summary(query pg_catalog.text)
	RETURNS pg_catalog.text
	AS 'MODULE_PATHNAME' LANGUAGE C STRICT PARALLEL UNSAFE;
