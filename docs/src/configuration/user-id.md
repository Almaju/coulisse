# User identification

Coulisse keeps separate memory per user. To do that, it needs to know *who* is making each request.

## How users are identified

Requests identify the user via one of these fields, in order:

1. `safety_identifier` (preferred — matches OpenAI's recent schema)
2. `user` (deprecated, but still accepted)

```json
{
  "model": "assistant",
  "safety_identifier": "alice@example.com",
  "messages": [...]
}
```

The identifier can be anything — an email, an internal user ID, a UUID, an opaque token. Coulisse derives a stable internal UUID from it:

- If you pass a valid UUID, that's what's used.
- Otherwise, a deterministic v5 UUID is derived from the string, so the same identifier always maps to the same user.

## Requiring identification

By default, Coulisse **requires** every request to carry an identifier. Unidentified requests are rejected with an error. This is the safe default: memory only works if you know who you're talking to.

## `default_user_id`: a single-user fallback

For local development or single-user deployments, you can declare a `default_user_id` in `coulisse.yaml`. When a request arrives without `safety_identifier` or `user`, Coulisse acts as if that default had been passed.

```yaml
default_user_id: main        # everyone's anonymous requests bucket here

providers:
  anthropic:
    api_key: sk-ant-...

agents:
  - name: assistant
    provider: anthropic
    model: claude-sonnet-4-5-20250929
```

With a `default_user_id` set:

- Requests that omit both `safety_identifier` and `user` fall back to the default. They get memory like any other user — just scoped to that shared bucket.
- Requests that *do* include an identifier still get their own scope.
- All anonymous requests share one memory bucket and one rate-limit counter, because they all map to the same id.

## When to set it

Good reasons:

- Local / single-user setups where you don't want to bother sending an identifier.
- Small deployments behind an auth layer that handles identity upstream but doesn't want to plumb it through.

Don't set `default_user_id` in multi-tenant deployments — every user would share one bucket, which defeats isolation. Leave it unset so missing identifiers are rejected.

## Trust model

Everything keyed by user — conversation history, long-term memory, semantic recall, per-user MCP OAuth sessions, and rate-limit counters — is partitioned by the identifier on the request. Those partitions are airtight: a query never crosses users, and one user's handle can't reach another user's data.

But understand where the identifier *comes from*. By default it is **asserted by the client in the request body** (`safety_identifier`). In that mode the `auth` layer gates *access* to the proxy but does not bind the authenticated principal to the identifier, so any caller who can reach `/v1/chat/completions` can claim any identifier:

```json
{ "model": "assistant", "safety_identifier": "someone-else", "messages": [...] }
```

This is the right default for two common shapes, and unsafe for a third:

- **Single-user / local.** One identity, nothing to spoof.
- **Trusted first-party backend.** A backend that authenticates its own users and sets `safety_identifier` honestly on their behalf gets full isolation. The identifier-setting boundary lives on a server you control.
- **Untrusted clients calling directly.** If end users hold credentials and call Coulisse themselves — each able to send arbitrary JSON — any of them can read or write another user's memory and drive any MCP server that user has authorized, simply by claiming their identifier. Body-asserted identity does **not** isolate these clients.

## Binding identity to the credential

For the third shape, set `auth.proxy.identity: from_credential`. Coulisse then ignores the body's `safety_identifier` and derives the user from the **authenticated principal** — the Basic `username` or the OIDC `sub` claim. A request that claims a *different* identifier is rejected with `403`; the front desk now checks ID against the credential.

```yaml
auth:
  proxy:
    oidc:
      issuer_url:    https://authentik.example.com/application/o/coulisse/
      client_id:     coulisse-proxy
      client_secret: <secret>
      redirect_url:  http://localhost:8421/v1/
    identity: from_credential
```

Two rules the server enforces at startup:

- `from_credential` requires `auth.proxy` to configure `basic` or `oidc` — you can't bind to a credential that isn't checked.
- It is mutually exclusive with `default_user_id`. A shared default bucket would be a silent bypass, so the combination is rejected rather than letting one quietly win.

With **Basic**, the username *is* the identity, so each distinct user needs distinct credentials — a single shared username collapses everyone into one bucket. **OIDC** is the natural fit for many users: each gets a distinct `sub` automatically. See the [`auth.proxy.identity` reference](../reference/yaml.md#authproxyidentity) for the field details.
