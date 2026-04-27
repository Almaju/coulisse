-- Current experiments schema. Always reflects what `init` produces on a
-- fresh DB. When the schema changes, update this file, append the new crate
-- version to `Schema::VERSIONS` in store.rs, and write the upgrade step in
-- `Schema::upgrade_from`. Previous revisions live in git history.
--
-- Runtime-mutable experiment configs. Mirrors `dynamic_agents` and
-- `dynamic_judges`: each row either overrides a YAML-declared experiment of
-- the same name, stands alone as a DB-only experiment, or tombstones a
-- YAML experiment (`disabled = 1`).

CREATE TABLE IF NOT EXISTS dynamic_experiments (
    config_json TEXT,
    created_at  INTEGER NOT NULL,
    disabled    INTEGER NOT NULL DEFAULT 0,
    name        TEXT    NOT NULL PRIMARY KEY,
    updated_at  INTEGER NOT NULL
);
