# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Pre-1.0 caveat: minor version bumps (0.x → 0.y) may include breaking changes to
the YAML schema, HTTP surface, or CLI. Patch bumps (0.x.y → 0.x.z) will not.

## [Unreleased]

## [0.3.0] - 2026-06-03

### Added

- **Skills.** Reusable instruction bundles, Claude Code / Codex style. Drop a folder with a `SKILL.md` (YAML frontmatter `name` + `description`, then a markdown body) under `./skills` and it's discovered automatically — no config required. An agent opts in by listing skill names under its own `skills:` array; each becomes a tool whose description is advertised to the model up front, while the full body is delivered only when the model calls it (progressive disclosure). A skill's instructions can point at bundled resource files in its directory, fetched on demand via the `skill_file` tool and sandboxed to that directory. Lives in its own `skills` crate; the catalog is loaded into memory at boot. Side-effecting *execution* stays an MCP concern — a skill provides the instructions, an MCP server provides the running. See [Skills](docs/src/features/skills.md).
- **`coulisse reset`.** Deletes the SQLite database — wiping conversation memory, long-term memories, telemetry, judge scores, rate-limit windows, background tasks, and API tokens (the `coulisse.yaml` is untouched). Refuses to run while a server holds the database open, and prompts for confirmation unless `-y` is passed; removes the `-wal`/`-shm` sidecars too. See [CLI → `coulisse reset`](docs/src/reference/cli.md#coulisse-reset).
- **Self-issued API tokens.** Set `auth.proxy.tokens: {}` to gate `/v1/*` on Coulisse-minted `sk-coulisse-…` bearer keys — the same model as the OpenAI dashboard. Each token binds to a principal (the user id that partitions memory, recall, and rate limits) and carries a spend budget: `unlimited`, a lifetime `total` cap, or a per-calendar-month `monthly` cap. A request that would exceed the cap is rejected with `429 insufficient_quota` before any provider call; spend is tracked per token in USD from the same pricing table the cost tracker uses. Mint/monitor/revoke from the studio **Tokens** page (`/admin/tokens`) or the new `coulisse token create|list|revoke` CLI. Only a SHA-256 digest of each secret is stored — the plaintext is shown once at creation. New `auth` schema (`api_tokens`, `token_usage` tables) at version 0.1.0. See [API tokens](docs/src/features/api-tokens.md).
- **`server:` config block.** New top-level section for how the process binds and listens: `bind` (default `0.0.0.0`), `port` (default `8421`), `worker_threads` (default: CPU count), and `max_body_bytes` (default: axum's 2 MiB). Lives in its own `server` crate. See [YAML reference → `server`](docs/src/reference/yaml.md#server).
- **Structured outputs (`response_format`).** The chat endpoint now accepts OpenAI's `response_format` field — `{"type": "json_object"}` or `{"type": "json_schema", "json_schema": {...}}`. Coulisse enforces it uniformly for every provider by injecting a shape instruction into the system preamble and validating the reply server-side, so structured output works even on models with no native structured-output mode. Non-streaming requests re-prompt the model with the exact validation error up to twice before failing; a malformed schema is rejected with `400` up front, and a reply that never validates returns `502`. Streaming validates the accumulated reply at the end and surfaces an SSE error event on failure. See [Structured outputs](docs/src/features/structured-output.md).
- Auto-generated infrastructure secrets — `COULISSE_VAULT_KEY` / `COULISSE_HMAC_KEY` are persisted to `.coulisse/secrets.env` on first boot when unset.
- MCP OAuth `mode: discover` — spec-compliant servers (Todoist, Atlassian, Linear…) wire up with no credentials via RFC 8414 + RFC 7591.
- `NotConnectedTool` embeds a real connect URL in its result so the LLM can relay it to the user.
- Sentinel `connect_<server>` tool surfaced when an OAuth-pending server is mounted without `only:` — previously the agent saw zero tools and the LLM had no way to trigger the connect flow.
- RFC 9728 protected-resource discovery — Coulisse now follows the MCP origin's `/.well-known/oauth-protected-resource` to the actual authorization server before fetching its metadata. Fixes Todoist (MCP on `ai.todoist.net`, auth on `todoist.com`) and any other server whose auth lives on a different origin than its MCP endpoint. Falls back to the previous "MCP origin is the auth server" behaviour when no protected-resource metadata is published.
- Discovered scopes now prefer the resource-specific `scopes_supported` from RFC 9728 metadata over the broader auth-server list. Fixes Todoist's `invalid_scope` rejection (the AS lists admin/billing scopes the MCP endpoint won't grant).
- RFC 8707 resource indicators — Coulisse now echoes the MCP endpoint URL as `&resource=...` in the authorize redirect and the token-exchange body. Without this, tokens issued by an AS on a different origin than the MCP endpoint (Todoist: AS on `todoist.com`, MCP on `ai.todoist.net`) are bound to the AS origin and rejected with 401 by the MCP endpoint.
- Auto-recover from rejected MCP tokens — when an MCP endpoint responds with 401/403 during session setup (parsed as `AuthRequired` / `InsufficientScope`, or as the bare `UnexpectedServerResponse("HTTP 401 ...")` shape that Atlassian returns without a Bearer challenge), Coulisse now drops the stored token and reports `NotConnected` so the agent's next turn surfaces a fresh connect URL via the sentinel tool. Previously a stale token would cause every chat turn to fail with 502 in a loop until the user manually deleted the row from `mcp_oauth_tokens`.
- Refresh-token flow (RFC 6749 §6) — Coulisse now exchanges refresh tokens for fresh access tokens both preemptively (when the stored `expires_at` is within 60s of now) and reactively (when an MCP call returns 401 despite the expiry check passing). Refresh-token rotation is honoured: a new refresh token in the response replaces the old one. Previously every 1-hour-old session would force the user to re-authorize through the browser. Falls back to `NotConnected` only when the refresh grant itself fails.
- **Minimal MCP config — paste a URL, done.** `transport:` is optional: Coulisse infers it from the entry shape (`url:` → http, or sse when the path has `/sse`; `command:` → stdio). URL-based servers default to per-user `oauth: discover` automatically, so the common case needs nothing beyond the URL — opt out with `oauth: false` for a non-auth remote MCP. Stdio servers imply no auth. The explicit `transport:` tag still works (use it for an SSE endpoint whose path lacks `/sse`). For a server that only honours `mcp-remote`'s grandfathered client id (e.g. Todoist today), declare the `command: npx` / `args: [-y, mcp-remote, <url>]` stdio form explicitly — no special flag.
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

- The top-level `port:` field moved under the new `server:` block (`server.port`). A bare top-level `port:` is no longer read. Breaking.
- `mcp.<server>.oauth.mode` is now optional and defaults to `discover`; you only write `mode: static` to switch off DCR. The `oauth: discover` string shorthand and the `oauth: true` boolean have been removed — omit `oauth:` for the default flow, write `oauth: false` to disable, or a map (`oauth: { scopes: [...] }` / `oauth: { mode: static, ... }`) to configure. Breaking.
- `auth.mcp_consumer_secret` is now optional.
- `mcp` crate schema bumped to `0.2.0` (new `mcp_oauth_clients` table).

### Removed

- `memory.storage` and `storage.fs.path` are gone. State now always lives under `.coulisse/` next to the config — the SQLite database at `.coulisse/coulisse-memory.db` and uploaded files under `.coulisse/files`. There are no path knobs; to persist across container restarts, mount `.coulisse/` on a volume. Existing deployments that pointed `memory.storage` at `./coulisse-memory.db` (or elsewhere) should move that file to `.coulisse/coulisse-memory.db`. Breaking.
- `mcp.<server>.use_mcp_remote` and `mcp.<server>.no_rewrite`, along with the automatic rewrite of `npx mcp-remote <url>` stdio shims into native transports. Declare the transport you want directly: a `url:` for native http/sse + discover, or the `command:`/`args:` stdio form to run `mcp-remote` yourself. Breaking.

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

[Unreleased]: https://github.com/Almaju/coulisse/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/Almaju/coulisse/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/Almaju/coulisse/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Almaju/coulisse/releases/tag/v0.1.0
