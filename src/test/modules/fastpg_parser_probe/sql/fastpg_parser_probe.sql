CREATE EXTENSION fastpg_parser_probe;

SELECT fastpg_parse_summary('SELECT 1;');
SELECT fastpg_parse_summary('CREATE TABLE fastpg_probe(id int); INSERT INTO fastpg_probe VALUES (1);');
SELECT fastpg_parse_summary('SELECT * FROM');

SELECT fastpg_analyze_summary('SELECT 1;');
SELECT fastpg_analyze_summary('SELECT $1::int4;');
SELECT fastpg_analyze_summary('CREATE TABLE fastpg_analyzed(id int);');
SELECT fastpg_analyze_summary('SELECT * FROM fastpg_missing;');

SELECT fastpg_rewrite_summary('SELECT 1;');
SELECT fastpg_rewrite_summary('CREATE TABLE fastpg_rewritten(id int);');

SELECT fastpg_plan_summary('SELECT 1;');
SELECT fastpg_plan_summary('SELECT $1::int4;');
SELECT fastpg_plan_summary('CREATE TABLE fastpg_planned(id int);');

SELECT fastpg_execute_summary('SELECT 1;');

CREATE TABLE fastpg_mem_probe(id int);
INSERT INTO fastpg_mem_probe VALUES (1), (2);
SELECT id FROM fastpg_mem_probe ORDER BY id;
SELECT pg_relation_size('fastpg_mem_probe'::regclass);
