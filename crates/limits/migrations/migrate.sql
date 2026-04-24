-- The single step forward from the previous schema revision to the current one.
-- When the schema changes, replace the contents with the ALTER/CREATE/DROP
-- statements needed to bring a prior database up to date.

CREATE TABLE IF NOT EXISTS rate_limit_windows (
    count   INTEGER NOT NULL,
    kind    TEXT    NOT NULL,
    start   INTEGER NOT NULL,
    user_id TEXT    NOT NULL,
    PRIMARY KEY (user_id, kind)
);

CREATE INDEX IF NOT EXISTS idx_rate_limit_windows_user ON rate_limit_windows(user_id);
