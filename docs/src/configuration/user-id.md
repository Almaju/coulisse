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
