-- Current rate-limit schema. Always reflects what `init` produces on a fresh
-- DB. One row per (user, kind); the row is overwritten in place as the user
-- accrues tokens within the current window and replaced when the window rolls
-- over. When the schema changes, update this file, append the new crate
-- version to `Schema::VERSIONS` in tracker.rs, and write the upgrade step in
-- `Schema::upgrade_from`. Previous revisions live in git history.

CREATE TABLE IF NOT EXISTS rate_limit_windows (
    count   INTEGER NOT NULL,
    kind    TEXT    NOT NULL,
    start   INTEGER NOT NULL,
    user_id TEXT    NOT NULL,
    PRIMARY KEY (user_id, kind)
);

CREATE INDEX IF NOT EXISTS idx_rate_limit_windows_user ON rate_limit_windows(user_id);
