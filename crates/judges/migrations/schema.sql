-- Current judge schema. Always reflects what `init` produces on a fresh DB.
-- When the schema changes, update this file, append the new crate version to
-- `Schema::VERSIONS` in store.rs, and write the upgrade step in
-- `Schema::upgrade_from`. Previous revisions live in git history.

CREATE TABLE IF NOT EXISTS scores (
    agent_name  TEXT    NOT NULL,
    created_at  INTEGER NOT NULL,
    criterion   TEXT    NOT NULL,
    id          TEXT    NOT NULL PRIMARY KEY,
    judge_model TEXT    NOT NULL,
    judge_name  TEXT    NOT NULL,
    message_id  TEXT    NOT NULL,
    reasoning   TEXT    NOT NULL,
    score       REAL    NOT NULL,
    user_id     TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_scores_agent   ON scores(agent_name);
CREATE INDEX IF NOT EXISTS idx_scores_message ON scores(message_id);
CREATE INDEX IF NOT EXISTS idx_scores_user    ON scores(user_id);

-- Runtime-mutable judge configs. Mirrors `dynamic_agents` in the agents
-- crate: each row either overrides a YAML-declared judge of the same name,
-- stands alone as a DB-only judge, or tombstones a YAML judge
-- (`disabled = 1`). Resolution at runtime is "DB wins, YAML fallback."
CREATE TABLE IF NOT EXISTS dynamic_judges (
    config_json TEXT,
    created_at  INTEGER NOT NULL,
    disabled    INTEGER NOT NULL DEFAULT 0,
    name        TEXT    NOT NULL PRIMARY KEY,
    updated_at  INTEGER NOT NULL
);
