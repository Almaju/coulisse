-- The single step forward from the previous schema revision to the current one.
-- When the schema changes, replace the contents with the ALTER/CREATE/DROP
-- statements needed to bring a prior database up to date.

-- The tool_calls table moved here from the memory crate. On legacy
-- databases it has a `message_id` column anchoring each row to a
-- memory message. Rename it to `turn_id` — these were always the same
-- UUID value (the chat handler reuses the assistant message id as the
-- turn correlation id), the new column name reflects that tool calls
-- belong to a turn, not a message. On fresh databases the column was
-- already named `turn_id` by schema.sql and this ALTER is a no-op
-- (it errors with "duplicate column" and the runtime swallows it).
ALTER TABLE tool_calls RENAME COLUMN message_id TO turn_id;

CREATE INDEX IF NOT EXISTS idx_tool_calls_turn ON tool_calls(turn_id);
