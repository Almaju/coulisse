-- Current database schema for the rate limit tables. One row per
-- (user, kind); the row is overwritten in place as the user accrues tokens
-- within the current window and replaced when the window rolls over.
-- Previous revisions live in git history; never keep numbered migration files.

CREATE TABLE IF NOT EXISTS rate_limit_windows (
    count   INTEGER NOT NULL,
    kind    TEXT    NOT NULL,
    start   INTEGER NOT NULL,
    user_id TEXT    NOT NULL,
    PRIMARY KEY (user_id, kind)
);

CREATE INDEX IF NOT EXISTS idx_rate_limit_windows_user ON rate_limit_windows(user_id);
