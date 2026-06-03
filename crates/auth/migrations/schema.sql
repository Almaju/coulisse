-- Current auth schema. Always reflects what `init` produces on a fresh DB.
-- When the schema changes, update this file, append the new crate version to
-- `Schema::VERSIONS` in token.rs, and write the upgrade step in
-- `Schema::upgrade_from`. Previous revisions live in git history.

-- Self-issued API tokens that gate the `/v1/*` proxy when `auth.proxy.tokens`
-- is configured. The plaintext secret (`sk-coulisse-…`) is shown once at
-- creation and never stored — only its SHA-256 hex digest, which is what
-- verification looks up. `principal` is the user identity the token binds to
-- (the value that partitions memory, recall, and rate limits). Budget is
-- one of `unlimited` (no cap), `total` (lifetime cap), or `monthly`
-- (per-calendar-month cap); `budget_micro_usd` is NULL only when unlimited.
CREATE TABLE IF NOT EXISTS api_tokens (
    budget_kind      TEXT    NOT NULL,
    budget_micro_usd INTEGER,
    created_at       INTEGER NOT NULL,
    id               TEXT    NOT NULL PRIMARY KEY,
    label            TEXT    NOT NULL,
    last_used_at     INTEGER,
    principal        TEXT    NOT NULL,
    revoked_at       INTEGER,
    secret_hash      TEXT    NOT NULL UNIQUE
);

-- One row per LLM round-trip charged to a token. Summed (all-time, or since
-- the start of the current calendar month) to enforce budgets and to render
-- the per-token spend column in the studio. Cost is stored as integer
-- micro-USD to avoid floating-point drift across millions of additions.
CREATE TABLE IF NOT EXISTS token_usage (
    cost_micro_usd INTEGER NOT NULL,
    created_at     INTEGER NOT NULL,
    id             TEXT    NOT NULL PRIMARY KEY,
    token_id       TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_token_usage_token ON token_usage(token_id, created_at DESC);
