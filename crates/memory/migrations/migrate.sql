-- The single step forward from the previous schema revision to the current one.
-- When the schema changes, replace the contents with the ALTER/CREATE/DROP
-- statements needed to bring a prior database up to date.

-- The `scores` and `tool_calls` tables moved out of memory into the
-- `judge` and `telemetry` crates respectively. Existing rows are
-- preserved — those crates run their own `CREATE TABLE IF NOT EXISTS`
-- on first boot and pick up the existing data. Memory simply stops
-- creating or referencing the tables.
