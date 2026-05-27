-- Current storage schema. Always reflects what `init` produces on a fresh DB.
-- When the schema changes, update this file, append the new crate version to
-- `Schema::VERSIONS` in store.rs, and write the upgrade step in
-- `Schema::upgrade_from`.

CREATE TABLE IF NOT EXISTS storage_files (
    bytes        INTEGER NOT NULL,
    content_type TEXT    NOT NULL,
    created_at   INTEGER NOT NULL,
    filename     TEXT    NOT NULL,
    id           TEXT    NOT NULL PRIMARY KEY,
    purpose      TEXT    NOT NULL,
    sha256       TEXT    NOT NULL,
    storage_key  TEXT    NOT NULL,
    user_id      TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_storage_files_sha256        ON storage_files (sha256);
CREATE INDEX IF NOT EXISTS idx_storage_files_user_created  ON storage_files (user_id, created_at);
CREATE INDEX IF NOT EXISTS idx_storage_files_created       ON storage_files (created_at);
