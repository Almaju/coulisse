-- The single step forward from the previous schema revision to the current one.
-- When the schema changes, replace the contents with the ALTER/CREATE/DROP
-- statements needed to bring a prior database up to date.

-- Initial schema: the scores table previously lived in the memory crate's
-- schema. Existing deployments already have the table in the same SQLite
-- file; the CREATE TABLE IF NOT EXISTS in schema.sql is a no-op for them.
