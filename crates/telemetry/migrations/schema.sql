-- Current telemetry schema. Always reflects what Coulisse creates on startup.
-- When the schema changes, update this file and write the step in migrate.sql.
-- Previous revisions live in git history; never keep numbered migration files here.

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
