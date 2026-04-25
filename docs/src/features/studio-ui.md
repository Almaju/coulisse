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

The studio UI and its JSON API can be gated in two mutually exclusive ways: HTTP Basic auth (good for local dev) or OIDC single sign-on (appropriate for shared deployments). Exactly one belongs under `studio` in `coulisse.yaml`.

The `/v1/chat/completions` and `/v1/models` endpoints are never behind this guard — it only covers `/studio/*`, so OpenAI-compatible SDK clients stay cookie-free.

### Basic auth

```yaml
studio:
  basic:
    password: choose-something-strong
    username: admin   # optional, defaults to "admin"
```

Every `/studio/*` request must carry `Authorization: Basic <base64(user:pass)>`. Browsers prompt via the native login dialog and cache credentials per origin, so the Leptos SPA needs no extra wiring.

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

On first request, the user is redirected to the IdP to log in; afterwards an encrypted session cookie keeps them authenticated on `/studio/*` until it expires (8 hours of inactivity). The Leptos SPA again needs no awareness — its `fetch` calls automatically ride on the session cookie.

Access control (**who** may log in) is delegated to the IdP. Coulisse treats "successfully authenticated by your IdP" as "authorized studio" — configure the allow-list in the IdP's application policy, not here.

**Authentik setup**: create a new OAuth2/OpenID Provider and Application, set the redirect URI to the `redirect_url` above (Authentik allows every subpath of it by default), and point Coulisse at the issuer URL of the provider. Add the application to the groups that should have access via Authentik bindings.

**Sessions are in-memory**: they evaporate on restart — users re-authenticate silently if their IdP session is still valid, otherwise they see the login page again.

### Leaving it open

Omit the `studio` block entirely to leave the studio surface unauthenticated. That's fine on a loopback-only dev box, but never expose an unauthenticated studio to the network. If you'd rather terminate auth at your infrastructure layer, put Coulisse behind a reverse proxy (oauth2-proxy, Cloudflare Access, Caddy's `forward_auth`), a VPN, or an SSH tunnel.

## How it's built

The UI is a Leptos WASM app in `crates/studio/`, styled with Tailwind (loaded via CDN) and hand-rolled shadcn-style components. The compiled bundle is embedded into the server binary at build time via `rust-embed`, so there's still only one binary to ship.

## Building the bundle

The studio crate is excluded from the main workspace so `cargo build` / `cargo test` don't try to cross-compile it. Build it explicitly when you want to update the embedded bundle:

```bash
rustup target add wasm32-unknown-unknown   # once
cargo install trunk --locked               # once
cd crates/studio
trunk build --release
```

This produces `crates/studio/dist/`, which the server picks up the next time you rebuild it:

```bash
cargo run
```

If you hit `/studio` without having run `trunk build`, the server serves a placeholder page with these instructions instead of a blank 404. The JSON API under `/studio/api/*` still works either way.

## Dev loop

For iterative UI work, run `trunk serve` alongside the server. Trunk hot-reloads on every change and proxies `/studio/api/*` to the Coulisse server.

```bash
cargo run                # terminal 1 — server on :8421
cd crates/studio
trunk serve              # terminal 2 — UI on :4422 with hot reload
```

Open `http://127.0.0.1:4422/studio/`. Changes to `crates/studio/src/` rebuild and reload in about a second.

## JSON API

The UI is backed by three read-only endpoints. They're not part of the OpenAI-compatible surface — they're specifically for the studio UI, but you're free to hit them from scripts.

| Method | Path                                                              | Returns                                                      |
|--------|-------------------------------------------------------------------|--------------------------------------------------------------|
| `GET`  | `/studio/api/users`                                                | List of users with message / memory / score / tool-call counts. |
| `GET`  | `/studio/api/users/{user_id}/messages`                             | Full conversation history for one user; each assistant message carries the tool invocations that produced it in fire order. |
| `GET`  | `/studio/api/users/{user_id}/memories`                             | Long-term memories for one user (no embeddings).             |
| `GET`  | `/studio/api/users/{user_id}/scores`                               | Judge scores for one user, plus mean per (judge, criterion). |
| `GET`  | `/studio/api/users/{user_id}/turns/{turn_id}/events`               | Telemetry event list for one turn (tool calls at every depth, subagent calls, future LLM snapshots). The turn id is the assistant `message_id` returned by the messages endpoint. |

`{user_id}` must be a real UUID — the studio endpoints don't derive one from arbitrary strings the way `/v1/chat/completions` does, because they're looking up existing records. `{turn_id}` is also a UUID (equal to the assistant message id).
