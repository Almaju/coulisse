# Rate limiting

Coulisse enforces per-user **token** limits across three rolling windows: hour, day, and month. Limits are set by the **client**, per request — not in the YAML — so callers can plug Coulisse into existing quota schemes without redeploying the server.

## How it works

1. Each request carries optional limit hints in its `metadata` field: `tokens_per_hour`, `tokens_per_day`, `tokens_per_month`.
2. Before the model is called, Coulisse looks up the user's current usage in each window. If any counter is already at its cap, the request is rejected with `429 Too Many Requests`.
3. If the request passes, Coulisse runs it. On success, the total tokens consumed (request + response) are added to the user's counters.
4. Counters reset on fixed boundaries: every hour, every 24 hours, every 30 days (aligned to UTC windows from the Unix epoch).

## Sending limits

Put the caps in the `metadata` object. Values are strings (OpenAI's metadata contract), parsed as non-negative integers:

```json
{
  "model": "assistant",
  "safety_identifier": "alice@example.com",
  "metadata": {
    "tokens_per_hour": "50000",
    "tokens_per_day": "500000",
    "tokens_per_month": "5000000"
  },
  "messages": [
    {"role": "user", "content": "Hi!"}
  ]
}
```

All three keys are independent and all are optional — send only the windows you care about. Omit the whole `metadata` object and the request is unlimited.

## When a limit is hit

The server responds with:

- **Status:** `429 Too Many Requests`
- **Header:** `Retry-After: <seconds>` — time until the offending window resets
- **Body:**

```json
{
  "error": {
    "type": "rate_limited",
    "message": "daily token limit exceeded: used 512000/500000, retry after 40213s"
  }
}
```

The message names which window tripped (`hourly`, `daily`, `monthly`), how many tokens were used, the cap, and the seconds to wait.

## Invalid metadata

If a metadata value isn't a valid non-negative integer, the server returns `400 Bad Request`:

```json
{
  "error": {
    "type": "invalid_request",
    "message": "metadata key 'tokens_per_hour' must be a non-negative integer, got 'abc'"
  }
}
```

## Scope and isolation

- **Per user.** Each user (keyed by `safety_identifier` or the fallback `user` field) has isolated counters.
- **Anonymous requests can't be rate-limited.** Coulisse needs an identifier. In setups with a `default_user_id` (see [User identification](../configuration/user-id.md)), all anonymous requests share *that* user's counter.
- **Per process.** Counters live in memory. If you run multiple Coulisse instances behind a load balancer, each has its own view — for shared quotas, limit upstream (in a gateway) instead.
- **Lost on restart.** Counters are not persisted. This is deliberate for now; durable accounting is on the [roadmap](../reference/roadmap.md).

## Why per-request limits instead of YAML?

Quotas usually live in your user/billing system, not your model-routing config. Putting limits in the request lets the caller decide — e.g. your app looks up the user's plan, fills in the numbers, and forwards the request. Coulisse just honors what you send.
