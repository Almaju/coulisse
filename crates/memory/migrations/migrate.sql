-- The single step forward from the previous schema revision to the current one.
-- When the schema changes, replace the contents with the ALTER/CREATE/DROP
-- statements needed to bring a prior database up to date.

CREATE TABLE IF NOT EXISTS scores (
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

CREATE INDEX IF NOT EXISTS idx_scores_message ON scores(message_id);
CREATE INDEX IF NOT EXISTS idx_scores_user    ON scores(user_id);
