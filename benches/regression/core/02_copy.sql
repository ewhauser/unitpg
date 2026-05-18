DROP TABLE IF EXISTS fastpg_reg_copy;
CREATE TABLE fastpg_reg_copy (id int NOT NULL, amount int, note text);
COPY fastpg_reg_copy FROM STDIN;
1	10	ten
2	20	twenty
3	30	thirty
\.
SELECT 'copy_count', count(*) FROM fastpg_reg_copy;
SELECT 'copy_lookup', amount, note FROM fastpg_reg_copy WHERE id = 2;
DROP TABLE fastpg_reg_copy;
