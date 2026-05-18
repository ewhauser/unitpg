DROP TABLE IF EXISTS fastpg_reg_attr;
CREATE TABLE fastpg_reg_attr (id int NOT NULL, code text, amount int);
SELECT 'attribute_count', count(*) FROM pg_catalog.pg_attribute WHERE attrelid = 'fastpg_reg_attr'::pg_catalog.regclass;
DROP TABLE fastpg_reg_attr;
