-- The single step forward from the previous schema revision to the current one.
-- When the schema changes, replace the contents with the ALTER/CREATE/DROP
-- statements needed to bring a prior database up to date.

-- Note: the `scores` table moved out of memory into the judge crate.
-- Existing rows are preserved — judge's schema runs `CREATE TABLE IF NOT
-- EXISTS scores`, which picks up the existing rows on first boot. Memory
-- simply stops creating or referencing the table.
