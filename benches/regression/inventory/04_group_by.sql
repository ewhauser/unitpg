DROP TABLE IF EXISTS fastpg_reg_group;
CREATE TABLE fastpg_reg_group (id int NOT NULL, group_id int, amount int);
INSERT INTO fastpg_reg_group VALUES (1, 1, 10);
INSERT INTO fastpg_reg_group VALUES (2, 1, 20);
INSERT INTO fastpg_reg_group VALUES (3, 2, 30);
SELECT 'group_count', group_id, count(*) FROM fastpg_reg_group GROUP BY group_id ORDER BY group_id;
DROP TABLE fastpg_reg_group;
