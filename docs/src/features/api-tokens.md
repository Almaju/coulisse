# API tokens

Coulisse can issue its own API keys — the same model as the OpenAI dashboard. It mints `sk-coulisse-…` bearer tokens, stores only their hash, gates the `/v1/*` proxy on them, tracks how much each token spends, and lets you cap that spend or revoke the token at any time.

This is the recommended way to expose Coulisse beyond loopback: hand each client (a teammate, a script, a deployed app) its own token instead of a shared password, and you get per-token attribution and control for free.

## Enabling

Turn the scheme on under the `proxy` auth scope:

```yaml
auth:
  proxy:
    tokens: {}   # the empty map is the switch
```

With this set, every `/v1/*` request must carry a valid token:

```
Authorization: Bearer sk-coulisse-…
```

A missing or unknown token gets `401`; a revoked one also gets `401`. Point any OpenAI SDK at Coulisse and pass the token as the API key — nothing else changes.

> Until `auth.proxy.tokens` is set, the proxy stays open and any tokens you mint are inert (the studio notes this). The **Tokens** studio page and the `coulisse token` CLI are always available, so you can pre-mint before flipping the switch.

## Identity binding

Every token binds to a **principal** — the user id that partitions [memory](./memory.md), recall, and [rate limits](./rate-limiting.md). Token auth therefore always implies credential-bound identity: the request runs as the token's principal, and a request body claiming a different `safety_identifier` is rejected with `403`. Because the identity comes from the token, [`default_user_id`](../reference/yaml.md#default_user_id) is meaningless here and combining the two is rejected at startup.

Issue multiple tokens with the same principal to give one user several keys (laptop, CI, phone) that share one memory bucket. Issue distinct principals to keep clients fully isolated.

## Budgets

Each token carries a spend budget, checked **before** every call — a request that would exceed it is rejected with `429 insufficient_quota` (matching OpenAI's quota response) and no provider call is made:

| Budget      | Behaviour                                                            |
| ----------- | ------------------------------------------------------------------- |
| `unlimited` | Never blocks. Spend is still tracked for monitoring.                |
| `total`     | Lifetime cap. Blocks once cumulative spend reaches the limit.       |
| `monthly`   | Per-calendar-month cap (UTC). Resets on the first of each month.    |

Spend is computed from the same [pricing table](./pricing.md) the cost tracker uses, summed per token in USD. Both streaming and non-streaming turns are charged.

## Managing tokens

### Studio

The **Tokens** page (under *Configure* in the studio nav) lists every token with its principal, budget, current-period spend, and lifetime spend. Use the form to mint a new one — the secret is shown **once**, immediately after creation, and never again. Each active token has a **Revoke** button.

### CLI

```console
# Mint an unlimited token
$ coulisse token create laptop --principal alice
created token 4f3c… for alice (unlimited)
sk-coulisse-9bQ…                       # the secret, on stdout only

# Mint with a $20/month cap
$ coulisse token create ci --principal alice --budget monthly --limit 20

# List tokens with spend
$ coulisse token list
4f3c…  active   laptop                unlimited           spent $1.27  [alice]
a91d…  active   ci                    $20.00 / month      spent $0.04  [alice]

# Revoke
$ coulisse token revoke 4f3c…
revoked 4f3c…
```

The secret prints to stdout and the context to stderr, so `coulisse token create … > key.txt` captures only the key. The CLI talks to the same SQLite database the server uses (WAL mode), so tokens minted while the server is running are live immediately.

## How it's stored

The `auth` crate owns two tables in the shared database: `api_tokens` (the hashed secret, label, principal, budget, and timestamps) and `token_usage` (one row per charged turn, in integer micro-USD). The plaintext secret exists only in the response to the mint call — Coulisse keeps a SHA-256 digest and nothing more, so a database leak never exposes a usable key.
