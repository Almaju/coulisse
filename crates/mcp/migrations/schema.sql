CREATE TABLE IF NOT EXISTS mcp_oauth_tokens (
    access_token_enc  BLOB    NOT NULL,
    created_at        INTEGER NOT NULL,
    expires_at        INTEGER,
    refresh_token_enc BLOB,
    server_name       TEXT    NOT NULL,
    updated_at        INTEGER NOT NULL,
    user_id           TEXT    NOT NULL,
    PRIMARY KEY (server_name, user_id)
);

-- Cached OAuth client registrations for `oauth: { mode: discover }` MCP
-- servers. One row per server_name. The client_secret_enc column is null
-- when the registration is a public client (no secret returned by DCR).
-- Coulisse-wide — the same row is reused across every user authorising
-- against that server, since the client_id identifies the Coulisse
-- instance, not the end user.
CREATE TABLE IF NOT EXISTS mcp_oauth_clients (
    client_id         TEXT    NOT NULL,
    client_secret_enc BLOB,
    metadata_json     TEXT    NOT NULL,
    redirect_uri      TEXT    NOT NULL,
    registered_at     INTEGER NOT NULL,
    server_name       TEXT    NOT NULL PRIMARY KEY
);
