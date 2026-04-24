-- The single step forward from the previous schema revision to the current one.
-- When the schema changes, replace the contents with the ALTER/CREATE/DROP
-- statements needed to bring a prior database up to date.

CREATE TABLE IF NOT EXISTS tool_calls (
    args        TEXT    NOT NULL,
    created_at  INTEGER NOT NULL,
    error       TEXT,
    id          TEXT    NOT NULL PRIMARY KEY,
    kind        TEXT    NOT NULL,
    message_id  TEXT    NOT NULL,
    ordinal     INTEGER NOT NULL,
    result      TEXT,
    tool_name   TEXT    NOT NULL,
    user_id     TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_tool_calls_message ON tool_calls(message_id);
CREATE INDEX IF NOT EXISTS idx_tool_calls_user    ON tool_calls(user_id);
