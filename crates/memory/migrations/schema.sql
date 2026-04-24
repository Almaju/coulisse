-- Current database schema. Always reflects what Coulisse creates on startup.
-- When the schema changes, update this file and write the step in migrate.sql.
-- Previous revisions live in git history; never keep numbered migration files here.

CREATE TABLE IF NOT EXISTS memories (
    content         TEXT    NOT NULL,
    created_at      INTEGER NOT NULL,
    embedding       BLOB    NOT NULL,
    embedding_dims  INTEGER NOT NULL,
    embedding_model TEXT    NOT NULL,
    id              TEXT    NOT NULL PRIMARY KEY,
    kind            TEXT    NOT NULL,
    user_id         TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_memories_user ON memories(user_id);

CREATE TABLE IF NOT EXISTS messages (
    content     TEXT    NOT NULL,
    created_at  INTEGER NOT NULL,
    id          TEXT    NOT NULL PRIMARY KEY,
    role        TEXT    NOT NULL,
    token_count INTEGER NOT NULL,
    user_id     TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_messages_user ON messages(user_id);

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
