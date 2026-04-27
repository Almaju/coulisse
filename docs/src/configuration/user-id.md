# User identification

Coulisse keeps separate memory per user. To do that, it needs to know *who* is making each request. The `users` setting picks how that identity is derived.

```yaml
users: shared        # default — single shared identity for every request
# users: per-request # require an identifier on every request
```

## `shared` (default)

Every request is attributed to the same hardcoded internal identity. All conversation history, extracted memories, and rate-limit counters land in one bucket.

This is what `coulisse init` writes. It's the right setting for:

- Trying Coulisse out locally.
- Single-user setups (you're the only person hitting your instance).
- Plugging tools like LibreChat at your desk and seeing it work without first wiring up an identifier on the client side.

The startup banner prints a loud warning whenever this mode is active, because shipping it to a multi-user deployment silently merges everyone's memory together.

## `per-request`

Coulisse rejects any request that doesn't identify its user. The identifier travels in the OpenAI request body:

1. `safety_identifier` (preferred — matches OpenAI's recent schema)
2. `user` (deprecated, but still accepted as a fallback)

```json
{
  "model": "assistant",
  "safety_identifier": "alice@example.com",
  "messages": [...]
}
```

The identifier can be anything — an email, an internal user ID, a UUID, an opaque token. Coulisse derives a stable internal UUID from it: a real UUID stays as-is; any other string is hashed to a deterministic v5 UUID, so the same identifier always maps to the same user.

When a request arrives without one, Coulisse returns a 400 with a message that names the active mode and how to fix it — either set the field on the client, or switch the server back to `shared`.

## Picking a mode

| You're… | Use |
|---|---|
| Trying Coulisse on your laptop. | `shared` |
| Running a single-user deployment behind your own auth. | `shared` |
| Pointing LibreChat / Open WebUI / your own SaaS at Coulisse with multiple end users. | `per-request`, and configure the client to forward each user's identifier in `safety_identifier`. |
| Building any multi-tenant deployment. | `per-request` |

Switching modes is a single line in `coulisse.yaml`. There's no migration: identities derived in `shared` mode keep their memory under the hardcoded bucket; identities derived in `per-request` mode keep their own buckets.
