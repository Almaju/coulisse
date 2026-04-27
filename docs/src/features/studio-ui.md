# Studio UI

Coulisse ships a studio UI for browsing the conversations and memories the server has seen, and for editing the live YAML config. It's served by the same binary, under `/admin/`.

Point a browser at `http://localhost:8421/admin/` while the server is running.

## What you can do

- List every user the server has seen, most recent activity first, with message and memory counts.
- Open a user to see their full conversation (user, assistant, and system messages) with per-message token counts and relative timestamps.
- See every tool invocation that happened during each assistant turn — rendered inline in the conversation as a collapsed block above the assistant bubble. Expand to see the args, the result (or error body), and a badge marking MCP vs subagent calls. This is the debug view for figuring out *what the agent tried and what came back*.
- Open the per-turn **Telemetry** block under any assistant message to see the full causal tree that produced it: every tool call (MCP or subagent) at every depth, with args, result, error, and duration. Unlike the inline top-level tool calls, the telemetry tree also surfaces tool calls made *inside* subagents — so when a subagent's MCP call fails, the real error is right there instead of being paraphrased into the assistant's text.
- See the long-term memories recalled for that user, tagged as `fact` or `preference`.
- See the LLM-as-judge scores for that user, including mean score per `(judge, criterion)` and the most recent individual scores with reasoning.
- Browse configured experiments at `/admin/experiments` — strategy, sticky-by-user flag, per-variant weights, and bandit-strategy mean scores live-loaded from judges.
- Run **smoke tests** at `/admin/smoke` — a synthetic-user persona drives a real conversation against any agent or experiment, scores fan out through the same judge pipeline, and the run viewer shows the full transcript with persona/assistant turns side by side. Useful for iterating on agent prompts without writing test scaffolding.
- **Edit, add, or disable agents, judges, experiments, and smoke tests** at `/admin/agents`, `/admin/judges`, `/admin/experiments`, and `/admin/smoke`. Each form is a YAML textarea over the same config shape used in `coulisse.yaml`. Edits and creations write to the database, never to `coulisse.yaml`; runtime resolution checks the database first, then falls back to YAML. List views label each row as `yaml`, `dynamic` (database-only), `override` (database shadows YAML), or `tombstoned` (disabled). Override rows expose a "Reset to YAML" action that drops the database row so the YAML version reasserts. See [Agents → Runtime overrides](../configuration/agents.md#runtime-overrides) for the full semantics — judges, experiments, and smoke tests follow the same model.

## Editing config: admin UI = API

Every admin route is content-negotiated. The same URL serves an HTML page in a browser, an HTML fragment to htmx, and JSON to a script — whichever the client's `Accept`/`HX-Request` headers ask for. The UI is a thin representation of the API; nothing the UI can do is unavailable to a `curl` call.

```bash
# List agents as JSON (effective merged view: database overrides + YAML)
curl -H 'Accept: application/json' http://localhost:8421/admin/agents

# Update an agent (writes to the database, not to coulisse.yaml)
curl -X PUT http://localhost:8421/admin/agents/bob \
     -H 'Content-Type: application/yaml' \
     --data-binary $'name: bob\nprovider: openai\nmodel: gpt-4o\n'

# Reset an override or tombstone — drops the database row, YAML reasserts
curl -X POST http://localhost:8421/admin/agents/bob/reset

# Replace the whole config file in one shot (this writes to coulisse.yaml)
curl -X PUT http://localhost:8421/admin/config \
     -H 'Content-Type: application/yaml' \
     --data-binary @coulisse.yaml
```

Agent writes through `/admin/agents` go to the database, never to `coulisse.yaml`. Other sections (`/admin/config`, providers, judges, experiments, smoke tests, etc.) still write to YAML. The two write paths are independent: editing an agent in the database has no effect on the file you committed to git.

## File watcher: hand-edits hot-reload

Coulisse watches `coulisse.yaml` while it runs. Edit it in your editor, save, and the live config updates without a restart. The validator runs before any reload — a broken edit is logged and the previous in-memory config keeps serving traffic until you fix the file.

What hot-reloads today: the **agents** list (runtime + admin display), the **judges** and **experiments** lists (admin display only — the routing tables that consume them are still rebuilt on restart). What still requires restart: providers, MCP servers, memory backend, telemetry pipeline, auth.

## YAML formatting

Admin saves go through `serde_yaml` round-trip serialization, so comments, blank lines, and key ordering are not preserved. If you want commented config, hand-edit the file — the watcher picks the change up the same way an admin save would. Comment-preserving writes are tracked as a follow-up.

## Authentication

The admin surface is gated by the `auth.admin` scope in `coulisse.yaml`. Two mutually exclusive modes: HTTP Basic auth (good for local dev) or OIDC single sign-on (appropriate for shared deployments). Exactly one belongs under `auth.admin`.

The `/v1/chat/completions` and `/v1/models` endpoints use the separate `auth.proxy` scope — they are never gated by admin auth. SDK clients stay cookie-free even when the studio runs behind OIDC.

### Basic auth

```yaml
auth:
  admin:
    basic:
      password: choose-something-strong
      username: admin   # optional, defaults to "admin"
```

Every `/admin/*` request must carry `Authorization: Basic <base64(user:pass)>`. Browsers prompt via the native login dialog and cache credentials per origin.

### OIDC (single sign-on)

Works with any OIDC-compliant IdP: Authentik, Keycloak, Auth0, Google, Microsoft, Okta.

```yaml
auth:
  admin:
    oidc:
      issuer_url:    https://authentik.example.com/application/o/coulisse/
      client_id:     coulisse-admin
      client_secret: <confidential-client-secret>   # omit for public PKCE clients
      redirect_url:  http://localhost:8421/admin/
      scopes:        [email, profile]               # optional; openid is always added
```

On first request, the user is redirected to the IdP to log in; afterwards an encrypted session cookie keeps them authenticated on `/admin/*` until it expires (8 hours of inactivity).

Access control (**who** may log in) is delegated to the IdP. Coulisse treats "successfully authenticated by your IdP" as "authorized admin" — configure the allow-list in the IdP's application policy, not here.

**Authentik setup**: create a new OAuth2/OpenID Provider and Application, set the redirect URI to the `redirect_url` above (Authentik allows every subpath of it by default), and point Coulisse at the issuer URL of the provider. Add the application to the groups that should have access via Authentik bindings.

**Sessions are in-memory**: they evaporate on restart — users re-authenticate silently if their IdP session is still valid, otherwise they see the login page again.

### Leaving it open

Omit the `auth.admin` block to leave the admin surface unauthenticated. That's fine on a loopback-only dev box, but never expose an unauthenticated admin surface to the network. If you'd rather terminate auth at your infrastructure layer, put Coulisse behind a reverse proxy (oauth2-proxy, Cloudflare Access, Caddy's `forward_auth`), a VPN, or an SSH tunnel.

## How it's built

The studio is composed in the cli binary. Each feature crate (`memory`, `telemetry`, `judges`, `experiments`) owns its own admin module — its routes, its [askama](https://djc.github.io/askama/) templates, and its view models. Cli wires them together: a single `base.html` shell, the auth wrapping, and a tower middleware that wraps non-htmx responses in the layout so bookmarked deep URLs render with full navigation.

Cross-feature views (e.g. tool-call panels inside a conversation page) are filled in via [htmx](https://htmx.org/) fragments — the conversation page, owned by `memory`, embeds `hx-get` requests against `telemetry` and `judges`. No feature crate depends on another for its admin surface; the browser orchestrates the composition. Tailwind (loaded via CDN) provides styling. Everything ships in the single Coulisse binary; there is no separate frontend build step.
