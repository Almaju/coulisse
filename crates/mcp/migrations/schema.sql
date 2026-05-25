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
