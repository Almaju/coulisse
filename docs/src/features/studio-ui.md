# Studio UI

Coulisse ships a read-only studio UI that lets you browse the conversations and memories the server has seen. It's served by the same binary, under `/admin/`.

Point a browser at `http://localhost:8421/admin/` while the server is running.

## What you can do

- List every user the server has seen, most recent activity first, with message and memory counts.
- Open a user to see their full conversation (user, assistant, and system messages) with per-message token counts and relative timestamps.
- See every tool invocation that happened during each assistant turn — rendered inline in the conversation as a collapsed block above the assistant bubble. Expand to see the args, the result (or error body), and a badge marking MCP vs subagent calls. This is the debug view for figuring out *what the agent tried and what came back*.
- Open the per-turn **Telemetry** block under any assistant message to see the full causal tree that produced it: every tool call (MCP or subagent) at every depth, with args, result, error, and duration. Unlike the inline top-level tool calls, the telemetry tree also surfaces tool calls made *inside* subagents — so when a subagent's MCP call fails, the real error is right there instead of being paraphrased into the assistant's text.
- See the long-term memories recalled for that user, tagged as `fact` or `preference`.
- See the LLM-as-judge scores for that user, including mean score per `(judge, criterion)` and the most recent individual scores with reasoning.
- Browse configured experiments at `/admin/experiments` — strategy, sticky-by-user flag, per-variant weights, and bandit-strategy mean scores live-loaded from judges.

That's it — this is a **read-only** tool. There's no way to send messages, edit memory, or mutate server state from the UI.

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
