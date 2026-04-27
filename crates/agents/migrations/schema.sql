-- Current dynamic-agents schema. Always reflects what `init` produces on a
-- fresh DB. When the schema changes, update this file, append the new crate
-- version to `Schema::VERSIONS` in store.rs, and write the upgrade step in
-- `Schema::upgrade_from`. Previous revisions live in git history.
--
-- One row per overridden or runtime-created agent name. `config_json` holds
-- the full `AgentConfig` for active rows; tombstones carry `disabled = 1` and
-- `config_json = NULL`. Resolution at runtime is "DB wins, YAML fallback":
-- a row here shadows the YAML entry of the same name (or stands alone).

CREATE TABLE IF NOT EXISTS dynamic_agents (
    config_json TEXT,
    created_at  INTEGER NOT NULL,
    disabled    INTEGER NOT NULL DEFAULT 0,
    name        TEXT    NOT NULL PRIMARY KEY,
    updated_at  INTEGER NOT NULL
);
