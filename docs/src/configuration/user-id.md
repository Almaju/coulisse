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

## Turning it off

If you're running Coulisse as a stateless proxy — no memory, no isolation — you can disable the requirement:

```yaml
require_user_id: false

providers:
  anthropic:
    api_key: sk-ant-...

agents:
  - name: assistant
    provider: anthropic
    model: claude-sonnet-4-5-20250929
```

With `require_user_id: false`:

- Requests without an identifier are accepted.
- Those requests **bypass per-user memory entirely** — nothing is stored, nothing is recalled.
- Requests *with* an identifier still get memory as usual.

## When to flip it off

Good reasons:

- You're fronting an internal service that handles identity upstream and just wants routing.
- You're prototyping and don't care about memory yet.

Bad reasons:

- You have a multi-tenant app and don't want to bother wiring identifiers. Don't do this — without identifiers, Coulisse can't isolate users, and you lose one of its main features.
