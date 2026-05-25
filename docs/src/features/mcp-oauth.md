# Per-user OAuth for MCP servers

Coulisse can authenticate each end-user independently with third-party MCP servers
(Jira, GitHub, Google, and others) using OAuth 2.0. When an agent calls a tool on
an OAuth-enabled MCP server, Coulisse automatically uses the credentials that the
requesting user has authorized.

> ⚠️ **Trust boundary**: Coulisse trusts the `user_id` passed in the chat request's
> `safety_identifier` field the same way Stripe trusts a `customer_id` — it assumes
> the caller is your authenticated backend, not an end-user directly. If you expose
> Coulisse's `/v1/` endpoint directly to untrusted clients without an auth proxy,
> any client can claim any `user_id` and access another user's connected accounts.
> Always place an auth proxy (your own backend, a gateway, or Coulisse's
> `auth.proxy` OIDC scope) between Coulisse and untrusted callers before deploying
> with OAuth-enabled MCP servers.

## How it works

1. **Connect link**: your backend calls `POST /mcp/{server}/connect-link?user_id=X`
   and receives a short-lived signed URL.
2. **User authorizes**: your backend delivers that URL to the user (email, in-app
   button, etc.). The user visits it and grants access to the OAuth provider.
3. **Callback**: the OAuth provider redirects back to Coulisse's callback endpoint.
   Coulisse exchanges the code for tokens and stores them encrypted at rest.
4. **Automatic injection**: subsequent chat requests for `user_id=X` that trigger
   tools on `{server}` automatically use the stored token — no manual plumbing.

## YAML configuration

```yaml
mcp:
  jira:
    transport: http
    url: https://mcp.atlassian.example.com
    oauth:
      authorization_url: https://auth.atlassian.com/authorize
      client_id: "${JIRA_CLIENT_ID}"
      client_secret: "${JIRA_CLIENT_SECRET}"
      redirect_uri: https://coulisse.example.com/mcp/jira/oauth/callback
      scopes:
        - read:jira-work
        - write:jira-work
      token_url: https://auth.atlassian.com/oauth/token

auth:
  mcp_consumer_secret: "${COULISSE_MCP_SECRET}"
```

### `oauth:` block fields

| Field | Required | Description |
|---|---|---|
| `authorization_url` | ✓ | Provider's OAuth authorize endpoint |
| `client_id` | ✓ | OAuth application client ID |
| `client_secret` | ✓ | OAuth application client secret; `${ENV}` expansion supported |
| `redirect_uri` | ✓ | Must match what you registered with the provider |
| `scopes` | — | OAuth scopes to request |
| `token_url` | ✓ | Provider's token exchange endpoint |

## Required environment variables

| Variable | Purpose |
|---|---|
| `COULISSE_VAULT_KEY` | 32 bytes, base64-encoded. Encrypts stored tokens at rest. |
| `COULISSE_HMAC_KEY` | 32 bytes, base64-encoded. Signs one-time connect links. |
| `COULISSE_MCP_SECRET` | Arbitrary string. Authenticates your backend's calls to `/mcp/{server}/connect-link`. |

Generate `COULISSE_VAULT_KEY` and `COULISSE_HMAC_KEY` with:

```bash
openssl rand -base64 32   # COULISSE_VAULT_KEY
openssl rand -base64 32   # COULISSE_HMAC_KEY (store separately)
```

Missing either key variable at startup when any server has `oauth:` → fatal error.

## Connect-link endpoint

Your backend calls this to generate an authorization URL:

```
POST /mcp/{server}/connect-link?user_id=<user_id>
Authorization: Bearer <mcp_consumer_secret>
```

Response `200`:

```json
{ "url": "https://...provider.../authorize?client_id=...&state=<signed_token>" }
```

Hand this URL to your end-user. It is valid for 10 minutes.

**Error codes:**

| Code | Reason |
|---|---|
| 401 | Wrong or missing consumer secret |
| 404 | Server name not found in config |
| 422 | `user_id` query parameter missing, or server exists but has no `oauth:` block |

## OAuth callback

Coulisse handles `GET /mcp/{server}/oauth/callback` automatically. It validates the
state HMAC, exchanges the authorization code for tokens, stores them encrypted in
SQLite, and shows an HTML success page to the user.

A tampered or expired state returns HTTP 400.

## Token storage

Tokens are stored in the shared SQLite database under `mcp_oauth_tokens`, encrypted
with AES-256-GCM (nonce prepended). Each `(server_name, user_id)` pair has one row;
connecting again overwrites the previous token.

## Per-user session lifecycle

**stdio transport**: Each `(user_id, server_name)` gets its own spawned process on
first use, held in an LRU cache (cap: 256 by default, idle timeout: 30 minutes). The
access token is passed as the `MCP_OAUTH_TOKEN` environment variable.

**HTTP transport**: A per-user connection is established with
`Authorization: Bearer <token>` as a default header. Same LRU cache applies.

## When a user hasn't connected yet

If an agent calls a tool on an OAuth-enabled MCP server and the user has no stored
token (or the token is expired), the tool returns an error message that the LLM can
relay to the user:

> Not connected: the user has not authorized access to the 'jira' MCP server. Ask
> them to visit the connect URL to link their account.

This is a tool result, not a 500 error. Your backend can call
`POST /mcp/{server}/connect-link` at that point and send the user a fresh link.
