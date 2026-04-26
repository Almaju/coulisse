-- Current smoke schema. Always reflects what `init` produces on a fresh DB.
-- When the schema changes, update this file, append the new crate version to
-- `Schema::VERSIONS` in store.rs, and write the upgrade step in
-- `Schema::upgrade_from`. Previous revisions live in git history.

CREATE TABLE IF NOT EXISTS smoke_runs (
    agent_resolved TEXT,
    ended_at       INTEGER,
    error          TEXT,
    experiment     TEXT,
    id             TEXT    NOT NULL PRIMARY KEY,
    started_at     INTEGER NOT NULL,
    status         TEXT    NOT NULL,
    test_name      TEXT    NOT NULL,
    total_turns    INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_smoke_runs_test_started ON smoke_runs(test_name, started_at);

CREATE TABLE IF NOT EXISTS smoke_messages (
    content    TEXT    NOT NULL,
    message_id TEXT,
    role       TEXT    NOT NULL,
    run_id     TEXT    NOT NULL,
    turn_index INTEGER NOT NULL,
    PRIMARY KEY (run_id, turn_index, role)
);

CREATE INDEX IF NOT EXISTS idx_smoke_messages_message ON smoke_messages(message_id);
