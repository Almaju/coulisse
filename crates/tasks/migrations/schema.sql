CREATE TABLE IF NOT EXISTS tasks (
    agent       TEXT    NOT NULL,
    created_at  INTEGER NOT NULL,
    error       TEXT,
    finished_at INTEGER,
    id          TEXT    NOT NULL PRIMARY KEY,
    prompt      TEXT    NOT NULL,
    result      TEXT,
    started_at  INTEGER,
    state       TEXT    NOT NULL,
    user_id     TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS tasks_runnable ON tasks (state, created_at);
