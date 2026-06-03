# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Pre-1.0 caveat: minor version bumps (0.x → 0.y) may include breaking changes to
the YAML schema, HTTP surface, or CLI. Patch bumps (0.x.y → 0.x.z) will not.

## [Unreleased]

### Added

- **Structured outputs (`response_format`).** The chat endpoint now accepts OpenAI's `response_format` field — `{"type": "json_object"}` or `{"type": "json_schema", "json_schema": {...}}`. Coulisse enforces it uniformly for every provider by injecting a shape instruction into the system preamble and validating the reply server-side, so structured output works even on models with no native structured-output mode. Non-streaming requests re-prompt the model with the exact validation error up to twice before failing; a malformed schema is rejected with `400` up front, and a reply that never validates returns `502`. Streaming validates the accumulated reply at the end and surfaces an SSE error event on failure. See [Structured outputs](docs/src/features/structured-output.md).
- Auto-generated infrastructure secrets — `COULISSE_VAULT_KEY` / `COULISSE_HMAC_KEY` are persisted to `.coulisse/secrets.env` on first boot when unset.
- `npx mcp-remote <URL>` shims are auto-rewritten to native HTTP + `oauth: { mode: discover }`.
- MCP OAuth `mode: discover` — spec-compliant servers (Todoist, Atlassian, Linear…) wire up with no credentials via RFC 8414 + RFC 7591.
- `NotConnectedTool` embeds a real connect URL in its result so the LLM can relay it to the user.
- Sentinel `connect_<server>` tool surfaced when an OAuth-pending server is mounted without `only:` — previously the agent saw zero tools and the LLM had no way to trigger the connect flow.
- RFC 9728 protected-resource discovery — Coulisse now follows the MCP origin's `/.well-known/oauth-protected-resource` to the actual authorization server before fetching its metadata. Fixes Todoist (MCP on `ai.todoist.net`, auth on `todoist.com`) and any other server whose auth lives on a different origin than its MCP endpoint. Falls back to the previous "MCP origin is the auth server" behaviour when no protected-resource metadata is published.
- Discovered scopes now prefer the resource-specific `scopes_supported` from RFC 9728 metadata over the broader auth-server list. Fixes Todoist's `invalid_scope` rejection (the AS lists admin/billing scopes the MCP endpoint won't grant).
- RFC 8707 resource indicators — Coulisse now echoes the MCP endpoint URL as `&resource=...` in the authorize redirect and the token-exchange body. Without this, tokens issued by an AS on a different origin than the MCP endpoint (Todoist: AS on `todoist.com`, MCP on `ai.todoist.net`) are bound to the AS origin and rejected with 401 by the MCP endpoint.
- Auto-recover from rejected MCP tokens — when an MCP endpoint responds with 401/403 during session setup (parsed as `AuthRequired` / `InsufficientScope`, or as the bare `UnexpectedServerResponse("HTTP 401 ...")` shape that Atlassian returns without a Bearer challenge), Coulisse now drops the stored token and reports `NotConnected` so the agent's next turn surfaces a fresh connect URL via the sentinel tool. Previously a stale token would cause every chat turn to fail with 502 in a loop until the user manually deleted the row from `mcp_oauth_tokens`.
- Refresh-token flow (RFC 6749 §6) — Coulisse now exchanges refresh tokens for fresh access tokens both preemptively (when the stored `expires_at` is within 60s of now) and reactively (when an MCP call returns 401 despite the expiry check passing). Refresh-token rotation is honoured: a new refresh token in the response replaces the old one. Previously every 1-hour-old session would force the user to re-authorize through the browser. Falls back to `NotConnected` only when the refresh grant itself fails.
- `mcp.<server>.no_rewrite: true` opt-out for the `npx mcp-remote` auto-rewrite. Use when the upstream MCP server only honours tokens issued to `mcp-remote`'s well-known (grandfathered) `client_id` and refuses fresh DCR registrations. Confirmed case: Todoist's MCP at `ai.todoist.net/mcp` currently whitelists `tdd_d3dc6f62265849b79ced9c0787eefe4a` (mcp-remote's first-issued client_id from May 23, 2026) and 401s every other public-or-confidential DCR-issued token. With `no_rewrite: true`, `mcp-remote` continues to run as a stdio child the way it always did.
- URL-only `mcp.<server>` config — `transport:` is now optional. Coulisse infers it from the entry shape (`url:` → http/sse based on path, `command:` → stdio). Same UX ChatGPT uses for remote MCP setup: paste the URL, optionally add `oauth: discover`, done. The explicit `transport:` tag form still works for backwards compatibility.
- `oauth: discover` string shorthand. Replaces `oauth: { mode: discover }` for the common case. `mode:` defaults to `discover` everywhere — you only write it when switching to `mode: static`. Map-form `oauth: { scopes: [...] }` (without `mode:`) also parses as discover.
- **Zero-config OAuth for URL-based MCP servers.** When `oauth:` is absent and the transport is `url:` (http/sse), Coulisse now defaults to `oauth: discover` automatically. Pasting `url: https://ai.todoist.net/mcp` is now sufficient — same UX as ChatGPT. Opt out with `oauth: false` for the rare non-auth remote MCP. Stdio servers are unaffected (no auth implied).
- `use_mcp_remote: true` on URL configs — internally rewrites to `npx mcp-remote@latest <url>` stdio so the user keeps the clean URL mental model while routing through mcp-remote's grandfathered identity. Required for Todoist whose MCP whitelists mcp-remote's first-issued client_id and refuses fresh DCR registrations. Validated end-to-end: pm agent → mcp-remote stdio child → Todoist MCP → real task data returned.
- 20-second per-server init timeout in `McpServers::connect_with_vault`. Blocked or auth-pending stdio children (e.g. mcp-remote waiting on browser consent) no longer prevent the HTTP server from binding port 8423 — children that don't complete `initialize` in time get logged as `agents will see an _unreachable placeholder until the next successful retry` and Coulisse continues to start. The child process keeps running, so completing auth out-of-band and restarting brings the server online for the agent on next boot.
- PKCE (RFC 7636) on every OAuth flow — Coulisse generates a 32-byte verifier, sends `code_challenge` + `code_challenge_method=S256` in the authorize redirect, and replays the verifier in the token exchange. The verifier rides inside the AES-GCM-encrypted `state` parameter so it never appears as cleartext in the URL. Required for MCP OAuth 2.1 compliance; without it Todoist (and any other AS that mandates PKCE) issues tokens that the MCP endpoint rejects with 401.
- Per-server failure isolation in `tools_for_user` — a runtime failure for one MCP server (network blip, crash, malformed response, vault DB error) now surfaces a `<server>_unreachable` placeholder tool for that server instead of erroring the whole call. The agent's other tools — filesystem, subagents, other working MCPs — keep functioning. Before, one broken MCP turned every chat turn into a 502 wall.
- Warning logged at discovery time when both the AS metadata and the resource metadata expose `scopes_supported: []` and YAML doesn't pin scopes — the resulting token will likely be rejected, and the warning tells the user to set `oauth.scopes:` explicitly (Atlassian is the typical case).
- DCR responses are now parsed for the RFC 7591 §3.2.1 `scope` field. When the AS metadata advertises no `scopes_supported` (Atlassian), Coulisse uses the space-separated list returned at client registration time as the effective scopes — matching what `mcp-remote` does and removing the need for manual `oauth.scopes:` config in this common case.
- DCR now registers Coulisse as a public client (`token_endpoint_auth_method: "none"`, PKCE-only) when the AS advertises support for it, falling back to `client_secret_post` otherwise. Matches `mcp-remote`'s pattern and the MCP OAuth 2.1 recommendation for local clients. Todoist's MCP resource server in particular only honoured tokens issued to public clients — Coulisse's previous confidential-client registration was a silent root cause behind every Todoist 401 cascade. The DCR request also now carries a `client_uri` pointing at Coulisse's repo for consent-screen attribution.
- OIDC default scopes (`openid email profile`) as a last-resort fallback when YAML, PRM, AS metadata, and DCR all yield no scopes. This is what `mcp-remote` does (priority 6 of its scope ladder). The fallback is announced via `tracing::warn!` so operators can debug if their provider needs more specific scopes.
- Native MCP-over-SSE client transport. Older MCP servers — Atlassian's `mcp.atlassian.com/v1/sse` being the prominent example — speak the previous protocol revision (long-lived `GET <url>` event-stream + `POST` to an endpoint announced by the first `event: endpoint` SSE event). `rmcp` 1.7 ships only a Streamable-HTTP client, so Coulisse rolled its own via the rmcp `Worker` trait and `eventsource-stream`. Configure with `transport: sse` and a `url:`; everything else (per-user OAuth + vault, PKCE, RFC 8707 resource binding, auto-recovery from 401s, the `connect_<server>` sentinel) works identically to the HTTP transport.
- `mcp-remote` shim auto-rewrite now picks the right transport based on URL shape: paths containing an `/sse` segment land on the new native SSE transport (Atlassian); everything else continues to use Streamable HTTP (Todoist). The Node `mcp-remote` child process is no longer spawned for either — both run through Coulisse's own transports with vault-managed tokens.
- Connect URL tokens switched from `base64url(json) + HMAC` to AES-256-GCM authenticated encryption. The previous format exposed the `exp`, `server`, and `user_id` fields in plaintext, which Claude in particular treated as permission to "freshen" old URLs from conversation history by bumping `exp` and forging the signature. AES-GCM removes the visible structure — tokens are now opaque ciphertext, and any mutation breaks decryption. Same key material (`COULISSE_HMAC_KEY`), same wire-shape envelope, same 10-minute expiry.
- `GET /mcp/{server}/connect?token=…` — user-facing OAuth entry point validated by HMAC token.
- Top-level `public_base_url:` for building OAuth redirect URIs and connect links.

### Changed

- `mcp.<server>.oauth:` now requires `mode:` (`static` or `discover`). Breaking.
- `auth.mcp_consumer_secret` is now optional.
- `mcp` crate schema bumped to `0.2.0` (new `mcp_oauth_clients` table).
- When `memory.storage` is omitted, the default SQLite database now lives at `.coulisse/coulisse-memory.db` (the project state directory, next to the log/PID/secrets) instead of `./coulisse-memory.db` beside the config. Existing deployments with data in `./coulisse-memory.db` should set `memory.storage: ./coulisse-memory.db` explicitly to keep using it, or move the file into `.coulisse/`.

## [0.2.0] - 2025-07-26

### Added

- `vars:` top-level block + `${vars.<name>}` interpolation across all string fields.
- Templated `agent:` field on webhook triggers.
- SSE heartbeat during subagent handoff (closes #42).
- Boot trigger (`type: boot`) — fires once on `coulisse start`.
- `tasks_status` agent tool — read-only counterpart to `dispatch_task`.
- Boot-time task reaper — marks orphaned `running` tasks as `errored` on startup.
- Sidecars — new top-level `sidecars:` block spawns long-lived helper processes (bridges, listeners, exporters).
- Webhook triggers (`type: webhook`) — `POST /hooks/<path>` enqueues a task with `{{a.b.c}}` payload templating.
- Cron triggers (`type: cron`) — schedule-driven agent runs.
- `/admin/live` activity board — htmx-polled view of recent tasks and tool calls.
- Async task queue — new `tasks` crate, four-worker pool, `dispatch_task` agent tool.
- `coulisse schema` subcommand — emits JSON Schema for `coulisse.yaml`.
- Top-level `port:` field (default `8421`).
- Runtime overrides for agents, judges, experiments, and smoke tests via admin UI / HTTP.

### Changed

- **Breaking:** `memory:` block reshaped around `storage` + `user_state`. Old fields (`backend`, `embedder`, `extractor`, `context_budget`, `memory_budget_fraction`, `recall_k`) removed.
- Database migrations switched to a `SchemaMigrator` trait + shared `coulisse_schema_versions` table.
- `agents` schema → 0.1.0 (`dynamic_agents`).
- `judges` schema → 0.2.0 (`dynamic_judges`).
- `experiments` becomes persistent at schema 0.1.0 (`dynamic_experiments`).
- `smoke` schema → 0.2.0 (`dynamic_smoke_tests`).
- `/admin/judges`, `/admin/experiments`, `/admin/smoke` write to the DB instead of `coulisse.yaml`; each gains a `reset` route.

### Fixed

- MCP tool names with characters outside `[a-zA-Z0-9_-]` are sanitized before being handed to the LLM provider.

### Notes de migration depuis v0.1.0

- Lancer `coulisse migrate` (ou démarrer normalement — le migrator tourne au boot).
- Remplacer l'ancien `memory.extractor:` par `user_state: true`.
- Relire la config YAML `sidecars:` et `triggers:` si vous en aviez une custom.

## [0.1.0] - 2026-04-26

Initial release.

### Added

- OpenAI-compatible HTTP API (`/v1/chat/completions`, `/v1/models`) with SSE streaming.
- YAML-driven configuration from a single `coulisse.yaml`.
- Multi-agent routing.
- Multi-backend support: Anthropic, OpenAI, Gemini, Cohere, Deepseek, Groq.
- Per-user conversation memory in SQLite.
- Long-term memory with semantic recall (OpenAI, Voyage, offline `hash`).
- Tunable memory budgets.
- MCP tool integration over stdio and HTTP, per-agent filtering.
- Subagents with `purpose:` description.
- Per-user token rate limiting (hour, day, month).
- LLM-as-judge evaluation.
- Experiments (A/B testing) — `split`, `shadow`, `bandit`.
- Smoke tests.
- Read-only admin UI at `/admin/`.
- OpenTelemetry export + SQLite event store.
- Docker image at `ghcr.io/almaju/coulisse` (`linux/amd64`, `linux/arm64`).

[Unreleased]: https://github.com/Almaju/coulisse/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/Almaju/coulisse/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Almaju/coulisse/releases/tag/v0.1.0
