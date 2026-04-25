-- Current judge schema. Always reflects what Coulisse creates on startup.
-- When the schema changes, update this file and write the step in migrate.sql.

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
