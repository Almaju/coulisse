# Per-user OAuth for MCP servers

Coulisse can authenticate each end-user independently with third-party MCP servers
(Todoist, Atlassian, GitHub, Google, and others) using OAuth 2.0/2.1. When an agent
calls a tool on an OAuth-enabled MCP server, Coulisse automatically uses the
credentials that the requesting user has authorized.

> ⚠️ **Trust boundary**: Coulisse trusts the `user_id` passed in the chat request's
> `safety_identifier` field the same way Stripe trusts a `customer_id` — it assumes
> the caller is your authenticated backend, not an end-user directly. If you expose
> Coulisse's `/v1/` endpoint directly to untrusted clients without an auth proxy,
> any client can claim any `user_id` and access another user's connected accounts.
> Always place an auth proxy (your own backend, a gateway, or Coulisse's
> `auth.proxy` OIDC scope) between Coulisse and untrusted callers before deploying
> with OAuth-enabled MCP servers.

## `mcp-remote` shims are auto-rewritten

The official MCP docs introduce per-user OAuth servers by telling you to put a
`npx mcp-remote@latest <URL>` stdio shim in your config. **Coulisse detects
that shape and rewrites it on the fly** to native HTTP transport + `mode:
discover` — same behavior, but tokens land in Coulisse's per-user vault
instead of `mcp-remote`'s shared on-disk cache, and no Node process or
browser-callback port is involved. You'll see a warning at boot showing the
equivalent explicit YAML.

So you can paste a docs-style snippet like this and it works as-is:

```yaml
mcp:
  todoist:
    transport: stdio
    command: npx
    args: ["-y", "mcp-remote@latest", "https://ai.todoist.net/mcp"]
```

Coulisse internally treats it identically to the explicit form in the
"Discover mode" section below. Writing it explicitly is the long-term path —
it documents the intent and silences the boot warning — but you don't have
to know about `mode: discover` to get started.

The rewrite only fires for the canonical shape (`npx`/`pnpm`/`bunx`/`yarn`
runner, args contain `mcp-remote`, args contain a URL, no custom env vars).
Anything more elaborate is left untouched.

## Two flavours

`oauth:` blocks come in two modes, picked with the `mode:` discriminator:

- **`mode: discover`** — MCP-spec OAuth 2.1 with discovery + Dynamic Client
  Registration. Coulisse reads the provider's authorization-server metadata from
  `<mcp_origin>/.well-known/oauth-authorization-server` and registers itself as a
  client on first use. **No credentials in YAML.** This is the right choice for
  modern MCP servers — Todoist, Atlassian (`mcp.atlassian.com`), Linear, and so on.
- **`mode: static`** — classic OAuth 2.0 with pre-registered app credentials. You
  register Coulisse as a client at the provider's developer console and paste the
  resulting `client_id` / `client_secret` here. Use this for providers that don't
  support Dynamic Client Registration.

Both modes drive the same per-user token flow: tokens are stored in the vault
keyed by `(server_name, user_id)`, never shared across users.

## How it works

1. **Tool call hits `NotConnected`**: The user makes a chat request, the agent
   calls a tool on the MCP server, Coulisse looks up `(server, user_id)` in the
   vault, finds no token, and returns a `NotConnectedTool` placeholder whose tool
   result contains a **per-user, single-use connect URL** built from the HMAC key.
   The LLM reads that result and relays the URL to the user.

   For agents that haven't pinned an `only:` list (the common case — "give the
   agent every tool the server exposes"), Coulisse can't know the real tool
   schemas until someone has authorised at least once. Until then it surfaces a
   single sentinel tool named `connect_<server>` whose description tells the
   LLM to call it when the user asks to use that server. Calling it returns
   the same per-user connect URL. Once the user authorises, the sentinel goes
   away and the real tool list takes its place transparently.
2. **User clicks the link**: lands on `GET /mcp/{server}/connect?token=…` on
   Coulisse. Coulisse validates the HMAC, then **for `discover` mode only**, lazily
   runs discovery + Dynamic Client Registration if it hasn't yet (cached in
   `mcp_oauth_clients` afterwards). Discovery is a two-step walk: first
   `<mcp_origin>/.well-known/oauth-protected-resource` (RFC 9728) to find which
   issuer hosts the authorization server (Todoist's MCP lives on `ai.todoist.net`,
   its auth server lives on `todoist.com`), then
   `<issuer>/.well-known/oauth-authorization-server` (RFC 8414) for the actual
   endpoints. Coulisse then 302s to the provider's `authorization_endpoint`.
3. **User authorizes**: signs into their own account at the provider, sees a
   consent screen, and the provider redirects back to Coulisse's callback.
4. **Token stored**: Coulisse exchanges the code for tokens and stores them
   encrypted in `mcp_oauth_tokens` under the user's id.
5. **Subsequent tool calls succeed**: the next chat turn on the same `user_id`
   spawns a real per-user MCP session backed by the stored token.

Every user authorizes independently. Alice's token is **never** usable by Bob —
they have separate vault rows, separate MCP sessions, and separate consent flows.

## YAML configuration

### Discover mode (recommended for spec-compliant servers)

```yaml
public_base_url: http://localhost:8421   # see "Public base URL" below

mcp:
  todoist:
    transport: http
    url: https://ai.todoist.net/mcp
    oauth:
      mode: discover
      # optional override; defaults to the provider's `scopes_supported`
      # scopes: [data:read_write]

auth:
  mcp_consumer_secret: "${COULISSE_MCP_SECRET}"
```

Nothing else to fill in. Coulisse handles discovery and DCR on first use.

### Static mode (for non-DCR providers)

```yaml
mcp:
  jira:
    transport: http
    url: https://mcp.atlassian.example.com
    oauth:
      mode: static
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

| Field | Mode | Description |
|---|---|---|
| `mode` | both | `discover` or `static` |
| `scopes` | both | OAuth scopes to request (optional; `discover` falls back to `scopes_supported`) |
| `authorization_url` | static | Provider's OAuth authorize endpoint |
| `client_id` | static | OAuth application client ID |
| `client_secret` | static | OAuth application client secret; `${ENV}` expansion supported |
| `redirect_uri` | static | Must match what you registered with the provider |
| `token_url` | static | Provider's token exchange endpoint |

For `discover` mode, the `redirect_uri` is computed automatically from
`public_base_url` as `{public_base_url}/mcp/{server}/oauth/callback`. The
authorization, token, and registration endpoints all come from discovery.

## Public base URL

Coulisse needs to know its own externally reachable URL to build OAuth redirect
URIs and the per-user connect links surfaced to LLMs:

```yaml
public_base_url: https://coulisse.example.com   # no trailing slash
```

If omitted, defaults to `http://localhost:{port}`, which is right for personal
and local-dev setups. Set it explicitly when Coulisse runs behind a tunnel,
reverse proxy, or on a public hostname — the same value must match whatever the
OAuth provider sees as the redirect URI host.

## Secrets (zero config by default)

Coulisse needs two long-lived 32-byte secrets when an OAuth-enabled MCP
server is configured:

- **vault key** — encrypts stored tokens (and any cached DCR `client_secret`) at rest with AES-256-GCM
- **HMAC key** — signs the per-user connect links Coulisse mints for the LLM, plus the OAuth `state` token

You don't have to manage these for local use. On first boot Coulisse
generates both and writes them to `.coulisse/secrets.env` (mode `0600`,
already `.gitignore`d), then reuses the file on every subsequent start.
**Back this file up.** Losing it invalidates every token in
`mcp_oauth_tokens` — users have to re-authorize each connected MCP server.

For deployments that source secrets from a vault/k8s/CI, set them as
environment variables and Coulisse will use those instead of touching the
on-disk file:

| Variable | Purpose |
|---|---|
| `COULISSE_VAULT_KEY` | 32 bytes, base64-encoded. Overrides the on-disk vault key. |
| `COULISSE_HMAC_KEY`  | 32 bytes, base64-encoded. Overrides the on-disk HMAC key. |

Both are optional. Resolution order: **env vars > `.coulisse/secrets.env` > generated on the fly**.

One additional optional secret gates the admin endpoint only:

| Variable | Purpose |
|---|---|
| `COULISSE_MCP_SECRET` (via `auth.mcp_consumer_secret`) | Arbitrary string. When set, gates `POST /mcp/{server}/connect-link`. When unset, that endpoint returns 503 and the per-user `GET /connect` flow keeps working. |

## Endpoints

Coulisse exposes three OAuth-related HTTP routes:

### `GET /mcp/{server}/connect`

The user-facing route. The URL Coulisse mints inside `NotConnectedTool` looks
like this and is what the LLM hands the user:

```
{public_base_url}/mcp/{server}/connect?token={hmac_signed_token}
```

The `token` is HMAC-signed with `COULISSE_HMAC_KEY` and embeds the `user_id`
plus a 10-minute expiry. The handler:

1. Validates the HMAC and expiry.
2. For `discover` mode: ensures the server is registered (lazily runs
   discovery + DCR on the first hit; reuses the cached `mcp_oauth_clients` row
   on subsequent hits).
3. 302-redirects to the provider's authorization endpoint with a fresh `state`
   token carrying the same `user_id`.

### `POST /mcp/{server}/connect-link`

Admin-facing alternative. Bearer-authed with `COULISSE_MCP_SECRET`. Useful when
your backend wants to email a user a connect link without going through the
LLM's tool result:

```
POST /mcp/{server}/connect-link?user_id=<user_id>
Authorization: Bearer <mcp_consumer_secret>
```

Response `200`:

```json
{ "url": "https://...provider.../authorize?client_id=...&state=<signed_token>" }
```

Hand this URL to your end-user. Valid for 10 minutes.

**Error codes:**

| Code | Reason |
|---|---|
| 401 | Wrong or missing consumer secret |
| 404 | Server name not found in config |
| 422 | `user_id` query parameter missing, or server exists but has no `oauth:` block |
| 502 | Discovery or DCR failed (discover mode only — check Coulisse logs) |

### `GET /mcp/{server}/oauth/callback`

The provider's redirect target. Coulisse validates the state HMAC, exchanges
the authorization code for tokens, stores them encrypted in SQLite, and shows
an HTML success page to the user.

A tampered or expired state returns HTTP 400.

## Token + client storage

Two tables under the shared SQLite database, both maintained by the `mcp`
crate's schema migrator:

- `mcp_oauth_tokens` — encrypted per-user tokens keyed by
  `(server_name, user_id)`. AES-256-GCM with the nonce prepended. Connecting
  again overwrites the previous token.
- `mcp_oauth_clients` — cached Dynamic Client Registration for `discover` mode
  servers. One row per server. The `client_secret` is encrypted when present;
  the `metadata_json` document is stored plaintext (the provider's
  authorization-server metadata isn't a secret). Coulisse-wide, **not**
  per-user — the `client_id` identifies the Coulisse instance, not the end user.

## Per-user session lifecycle

**stdio transport**: Each `(user_id, server_name)` gets its own spawned process
on first use, held in an LRU cache (cap: 256 by default, idle timeout: 30
minutes). The access token is passed as the `MCP_OAUTH_TOKEN` environment
variable. (Most spec-compliant MCP servers use HTTP transport — the stdio path
is preserved mostly for shims like `mcp-remote`.)

**HTTP transport**: A per-user connection is established with
`Authorization: Bearer <token>` as a default header. Same LRU cache applies.

## When a user hasn't connected yet

If an agent calls a tool on an OAuth-enabled MCP server and the calling user
has no stored token (or the token is expired), the tool returns a placeholder
result containing the connect URL. The LLM reads it and relays it to the user:

> Not connected: the user has not authorized access to the 'todoist' MCP server.
> Show them this link and ask them to open it to link their account — the link
> is single-use and tied to their identity, do not share it with anyone else:
> `http://localhost:8421/mcp/todoist/connect?token=…`

This is a tool result, not a 500 error. The user clicks the link, authorizes,
and the next chat turn just works. No backend intervention required for the
common case.
