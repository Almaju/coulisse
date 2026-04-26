-- Current telemetry schema. Always reflects what `init` produces on a fresh
-- DB. When the schema changes, update this file, append the new crate version
-- to `Schema::VERSIONS` in sink.rs, and write the upgrade step in
-- `Schema::upgrade_from`. Previous revisions live in git history.

CREATE TABLE IF NOT EXISTS events (
    correlation_id  TEXT    NOT NULL,
    created_at      INTEGER NOT NULL,
    duration_ms     INTEGER,
    id              TEXT    NOT NULL PRIMARY KEY,
    kind            TEXT    NOT NULL,
    parent_id       TEXT,
    payload         TEXT    NOT NULL,
    user_id         TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_events_correlation ON events(correlation_id);
CREATE INDEX IF NOT EXISTS idx_events_user_time   ON events(user_id, created_at DESC);

-- One row per tool invocation that happened during a turn. Anchored on
-- `turn_id` (the public-visible correlation id used elsewhere in
-- telemetry), not on a memory message id — tool calls are a
-- turn-scoped concept, not a message-scoped one.
CREATE TABLE IF NOT EXISTS tool_calls (
    args        TEXT    NOT NULL,
    created_at  INTEGER NOT NULL,
    error       TEXT,
    id          TEXT    NOT NULL PRIMARY KEY,
    kind        TEXT    NOT NULL,
    ordinal     INTEGER NOT NULL,
    result      TEXT,
    tool_name   TEXT    NOT NULL,
    turn_id     TEXT    NOT NULL,
    user_id     TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_tool_calls_turn ON tool_calls(turn_id);
CREATE INDEX IF NOT EXISTS idx_tool_calls_user ON tool_calls(user_id);
