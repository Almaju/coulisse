# Studio UI

Coulisse ships a read-only studio UI that lets you browse the conversations and memories the server has seen. It's served by the same binary, under `/studio`.

Point a browser at `http://localhost:8421/studio` while the server is running.

## What you can do

- List every user the server has seen, most recent activity first, with message / memory / score / tool-call counts.
- Open a user to see their full conversation (user, assistant, and system messages) with per-message token counts and relative timestamps.
- See every tool invocation that happened during each assistant turn — rendered inline in the conversation as a collapsed block above the assistant bubble. Expand to see the args, the result (or error body), and a badge marking MCP vs subagent calls. This is the debug view for figuring out *what the agent tried and what came back*.
- Open the per-turn **Telemetry** block under any assistant message to see the full causal tree that produced it: every tool call (MCP or subagent) at every depth, with args, result, error, and duration. Unlike the inline top-level tool calls, the telemetry tree also surfaces tool calls made *inside* subagents — so when a subagent's MCP call fails, the real error is right there instead of being paraphrased into the assistant's text.
- See the long-term memories recalled for that user, tagged as `fact` or `preference`.
- See the LLM-as-judge scores for that user, including mean score per `(judge, criterion)` and the most recent individual scores with reasoning.

That's it — this is a **read-only** tool. There's no way to send messages, edit memory, or mutate server state from the UI.

## Authentication

The studio UI can be gated in two mutually exclusive ways: HTTP Basic auth (good for local dev) or OIDC single sign-on (appropriate for shared deployments). Exactly one belongs under `studio` in `coulisse.yaml`.

The `/v1/chat/completions` and `/v1/models` endpoints are never behind this guard — it only covers `/studio/*`, so OpenAI-compatible SDK clients stay cookie-free.

### Basic auth

```yaml
studio:
  basic:
    password: choose-something-strong
    username: admin   # optional, defaults to "admin"
```

Every `/studio/*` request must carry `Authorization: Basic <base64(user:pass)>`. Browsers prompt via the native login dialog and cache credentials per origin.

### OIDC (single sign-on)

Works with any OIDC-compliant IdP: Authentik, Keycloak, Auth0, Google, Microsoft, Okta.

```yaml
studio:
  oidc:
    issuer_url:    https://authentik.example.com/application/o/coulisse/
    client_id:     coulisse-studio
    client_secret: <confidential-client-secret>   # omit for public PKCE clients
    redirect_url:  http://localhost:8421/studio/
    scopes:        [email, profile]               # optional; openid is always added
```

On first request, the user is redirected to the IdP to log in; afterwards an encrypted session cookie keeps them authenticated on `/studio/*` until it expires (8 hours of inactivity).

Access control (**who** may log in) is delegated to the IdP. Coulisse treats "successfully authenticated by your IdP" as "authorized studio" — configure the allow-list in the IdP's application policy, not here.

**Authentik setup**: create a new OAuth2/OpenID Provider and Application, set the redirect URI to the `redirect_url` above (Authentik allows every subpath of it by default), and point Coulisse at the issuer URL of the provider. Add the application to the groups that should have access via Authentik bindings.

**Sessions are in-memory**: they evaporate on restart — users re-authenticate silently if their IdP session is still valid, otherwise they see the login page again.

### Leaving it open

Omit the `studio` block entirely to leave the studio surface unauthenticated. That's fine on a loopback-only dev box, but never expose an unauthenticated studio to the network. If you'd rather terminate auth at your infrastructure layer, put Coulisse behind a reverse proxy (oauth2-proxy, Cloudflare Access, Caddy's `forward_auth`), a VPN, or an SSH tunnel.

## How it's built

The studio is a server-side Axum app in `crates/studio/`, rendered with [askama](https://djc.github.io/askama/) templates and styled with Tailwind (loaded via CDN). [htmx](https://htmx.org/) handles the only interactive bit — lazy-loading the per-turn telemetry tree when the operator expands it. Everything ships in the single Coulisse binary; there is no separate frontend build step.
