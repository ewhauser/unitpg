DROP TABLE IF EXISTS fastpg_reg_index;
CREATE TABLE fastpg_reg_index (id int NOT NULL, code text, amount int);
ALTER TABLE fastpg_reg_index ADD PRIMARY KEY (id);
SELECT 'index_count', count(*) FROM pg_catalog.pg_index WHERE indrelid = 'fastpg_reg_index'::pg_catalog.regclass;
DROP TABLE fastpg_reg_index;
