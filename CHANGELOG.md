# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Pre-1.0 caveat: minor version bumps (0.x → 0.y) may include breaking changes to
the YAML schema, HTTP surface, or CLI. Patch bumps (0.x.y → 0.x.z) will not.

## [Unreleased]

## [0.2.0] - 2025-07-26

### Added

- **`vars:` top-level YAML block + `${vars.<name>}` interpolation.** Declare
  named text snippets once under a top-level `vars:` map and splice them into
  any string field — preambles, prompts, URLs, env values — with
  `${vars.<name>}`. Resolved in a second pass *after* env-var expansion, so a
  var's value can itself contain `${VAR}` references. Single-pass: a
  substituted value containing `${vars.x}` is not re-expanded. Multi-line
  values inherit the placeholder's leading indent so they splice cleanly into
  YAML block scalars (`preamble: |`) without breaking the indentation
  contract. Useful for collapsing the duplicated team-table footer that every
  multi-agent setup ends up writing six times.
- **Templated `agent:` field on webhook triggers.** The `agent:` field on a
  `type: webhook` trigger now accepts the same `{{a.b.c}}` templating as
  `prompt:`, so one webhook can fan out to different agents based on the
  inbound payload (e.g. `agent: "{{agent}}"` lets a bridge POST one event
  per mentioned agent without declaring N webhooks). Templated values are
  not cross-validated at config load — the worker surfaces unknown-agent
  errors via the task's error state. Payloads that leave the placeholder
  unresolved get rejected with `400 Bad Request` before enqueueing.
- **SSE heartbeat during subagent handoff** (closes #42). When an agent
  delegates to a subagent, the stream emits `event: handoff_started` with
  the agent name within 3 seconds, then a `': heartbeat'` SSE comment
  every 20 seconds until the subagent finishes. Prevents proxies and
  browsers from closing the connection during long subagent turns; reduces
  client abandon rate. Heartbeat loop is cancelled cleanly on client
  disconnect via `select!` — no orphaned tasks.
- **Boot trigger** (`type: boot` under `triggers:`). Fires exactly once when
  `coulisse start` runs, then never again. Same submission path as cron and
  webhook — the prompt enqueues a task; a worker drains it through the
  normal agent runtime. Use case: a wake-up prompt that asks an
  orchestrator agent to check the queue's leftovers and decide whether to
  resume work, post a standup, or stay quiet. Paired with the new
  boot-time reaper (below), it gives "resume after `coulisse stop`" a
  clean primitive without forcing a ritual.
- **`tasks_status` agent tool.** Read-only counterpart to `dispatch_task`.
  Returns recent tasks across every agent — queued, running, done, or
  errored — as JSON, with an optional `state` filter. Agents that see it
  can answer "what's going on right now?" from chat instead of pointing
  a user at `/admin/live`. Plumbed via a new `TaskStatus` trait in
  `coulisse-core` so `agents` can read the queue without a hard dep on
  `tasks`, matching the existing `ScoreLookup` / `TaskQueue` pattern.
- **Boot-time task reaper.** On `coulisse start`, every task still in
  `running` is marked `errored` with the reason
  `process restarted before task completed`. This catches tasks that
  were mid-flight when the process stopped (worker died before
  `mark_done`/`mark_errored` could run). The sweep happens before
  workers spawn so they never claim stale rows.
- Sidecars. New top-level `sidecars:` YAML section + new `sidecars` crate
  let Coulisse spawn long-lived helper processes alongside itself —
  bridge scripts, listeners, exporters, anything you'd otherwise launch
  in a separate terminal. Coulisse captures stdout/stderr into its own
  log (tagged `sidecar=<name>`), restarts on crash per policy
  (`always` / `on-failure` (default) / `never`), and lets you pass env
  vars with `${VAR}` expansion. The canonical use case is paired with
  the new webhook trigger: declare the Matrix bridge at
  `local/matrix-bridge/bridge.py` as a sidecar and "one YAML, one
  start command" now actually starts everything. Known limitations
  documented in `docs/src/features/sidecars.md`: orphan processes on
  abrupt shutdown, no retry backoff, no health checks, no admin
  surface yet.
- Webhook triggers. New `type: webhook` variant of the `triggers:` YAML
  section. Coulisse exposes `POST <path>` for each entry (path must start
  with `/hooks/`); inbound JSON payloads are run through a simple
  `{{a.b.c}}` template substitution against the trigger's `prompt`, then
  enqueued as a task on the queue — same shape as cron and `dispatch_task`.

  ```yaml
  triggers:
    - name: matrix-mention
      type: webhook
      path: /hooks/matrix-mention
      agent: pm
      prompt: "{{sender}} dans {{room_name}}: {{body}}"
  ```

  Lives in the `triggers` crate (`webhook_router(triggers, queue, user_id)`
  returns an `axum::Router` the cli merges into the main app). Validates
  paths at startup: must start with `/hooks/`, must be unique. Coulisse
  stays platform-agnostic — there's no Matrix or Slack code in the binary.
  External bridges (Matrix, Slack outgoing webhooks, Discord, GitHub repo
  hooks, anything HTTP-capable) POST JSON to the configured path. A
  worked example ships at `local/matrix-bridge/bridge.py` — a 90-line
  Python script using `matrix-nio` that listens for `@coulisse` mentions
  and POSTs to Coulisse.
- Cron triggers. New top-level `triggers:` YAML section lets you declare
  agents that fire on a schedule, no HTTP request needed:

  ```yaml
  triggers:
    - name: daily-standup
      type: cron
      schedule: "0 9 * * *"
      agent: pm
      prompt: "Résume l'activité d'hier."
  ```

  Schedules accept either 5-field POSIX cron (`min hour dom mon dow`) or
  6-field with leading seconds; the 5-field form is normalised
  automatically. Schedules are validated at startup — bad expressions
  refuse to boot. Lives in a new `triggers` crate; each cron entry runs as
  a tokio task that sleeps until next-fire and enqueues a task via the
  same `TaskQueue` trait that `dispatch_task` already uses, so workers
  don't know cron exists. Webhook triggers (the path that lets any HTTP
  source — Matrix bridge, Slack, GitHub — summon agents without coupling
  Coulisse to any specific tool) land in a follow-up.
- `/admin/live` activity board. Cross-feature admin page that polls itself
  every two seconds via htmx and renders two panels: the most recent rows
  from the `tasks` queue (queued / running / done / errored, with relative
  ages) and the most recent tool calls from the telemetry crate (MCP and
  subagent kinds, with error markers). Lives in `cli/src/admin/live.rs`
  because the data sources span two feature crates; uses the existing
  base.html shell and matches the studio's visual style. Two new helper
  query methods land alongside: `telemetry::Sink::recent_tool_calls(limit)`
  and `tasks::Tasks::recent(limit)`.
- Async task queue. New `tasks` crate stores fire-and-forget agent runs in a
  `tasks` SQLite table with a `queued → running → done | errored` state
  machine. A worker pool in `cli` (four workers by default) polls
  `Tasks::next_runnable` and drives each task through the same
  `Agents::complete` path the sync `/v1/chat/completions` endpoint uses, so a
  background run gets the same MCP tools, subagent dispatch, and narration
  it would get from a synchronous request. Agents see a built-in
  `dispatch_task` tool — they call it with a target agent and prompt, get a
  `task_id` back immediately, and the worker pool runs the dispatched agent
  in the background. The new `TaskQueue` trait in `coulisse-core` keeps
  `agents` from depending on `tasks` directly; mirrors the existing
  `ScoreLookup` / `OneShotPrompt` pattern. No new YAML section yet —
  triggers (cron, webhook, MCP-event) and a configurable `tasks:` block are
  follow-ups.
- `coulisse schema` subcommand emits a JSON Schema for `coulisse.yaml`
  derived from the Rust types via `schemars`. The repo ships
  `coulisse.schema.json` at the root; reference it from the top of your
  yaml with `# yaml-language-server: $schema=./coulisse.schema.json` for
  IDE autocompletion and validation (VS Code YAML extension, Helix,
  Neovim, Zed, JetBrains).
- Top-level `port:` field in `coulisse.yaml`. Defaults to 8421; set it to
  run multiple Coulisse instances against different yamls on one machine.
- Runtime overrides for agents, judges, experiments, and smoke tests. Each
  can now be created, edited, and disabled via the admin UI or HTTP without
  modifying `coulisse.yaml`. Runtime entries live in new `dynamic_agents`,
  `dynamic_judges`, `dynamic_experiments`, and `dynamic_smoke_tests` SQLite
  tables; resolution checks the database first and falls back to YAML, with
  tombstone rows able to disable a YAML-declared name. Admin list pages
  label each row as `yaml`, `dynamic`, `override`, or `tombstoned`, and
  expose a "Reset to YAML" action that drops the database row so the YAML
  version reasserts. The YAML file is never written by the server through
  these paths.

### Changed

- **Breaking:** the `memory:` YAML block was reshaped around two pillars —
  `storage` (where data lives) and `user_state` (long-term memory; off by
  default). The old `memory.backend`, `memory.embedder`, `memory.extractor`,
  `memory.context_budget`, `memory.memory_budget_fraction`, and
  `memory.recall_k` fields are gone. To match the previous "auto-extraction
  on" behavior, replace the old `extractor:` block with `user_state: true`;
  Coulisse now picks the embedder and extraction model from your
  `providers:` automatically. Advanced overrides live under
  `user_state: { embed_with: ..., learn_from: ..., recall_k: ..., ... }`.
  Configs that still use the old field names fail loudly at startup.
- Database migrations replaced the prior two-file `schema.sql` + `migrate.sql`
  model with a `coulisse_core::migrate::SchemaMigrator` trait. Each persistent
  crate declares an ascending `VERSIONS` list of schema-bumping crate
  versions; older databases walk `upgrade_from(version)` forward until they
  reach the latest. Versions are stored in a shared
  `coulisse_schema_versions` table, with arbitrary Rust available per upgrade
  step.
- `agents` schema bumped to 0.1.0 (initial: adds the `dynamic_agents` table).
- `judges` schema bumped to 0.2.0 (adds the `dynamic_judges` table).
- `experiments` becomes a persistent crate at schema version 0.1.0 (initial:
  adds the `dynamic_experiments` table).
- `smoke` schema bumped to 0.2.0 (adds the `dynamic_smoke_tests` table).
- Admin endpoints for `/admin/judges`, `/admin/experiments`, and
  `/admin/smoke` no longer write to `coulisse.yaml` — they write to the
  database. Each gains a `POST /admin/<crate>/{name}/reset` route that
  drops the database row.

### Fixed

- MCP tool names with characters outside `[a-zA-Z0-9_-]` are now sanitized
  before being handed to the LLM provider. Anthropic enforces that pattern
  on tool names; several MCP servers in the wild (e.g.
  `ricelines/matrix-mcp`) use dots as namespace separators
  (`matrix.v1.messages.send_text`), which previously caused the provider to
  reject the entire request with `tools.N.custom.name: String should match
  pattern`. The fix lives in `crates/mcp`: invalid characters become `_`,
  names get truncated to 128 chars, and collisions are resolved with a
  numeric suffix. The inner `McpTool` keeps the original name so MCP
  dispatch still resolves correctly.

### Notes de migration depuis v0.1.0

- Lancer `coulisse migrate` (ou démarrer normalement — le migrator tourne au boot)
- Vérifier la continuité des sessions si memory était utilisé
- Relire la config YAML sidecars/triggers si vous en aviez une custom (champs renommés possibles)

## [0.1.0] - 2026-04-26

Initial release.

### Added

- OpenAI-compatible HTTP API (`/v1/chat/completions`, `/v1/models`) with
  streaming support over Server-Sent Events.
- YAML-driven configuration with startup validation — every feature is
  configured from a single `coulisse.yaml`.
- Multi-agent routing — each agent has its own provider, model, preamble, and
  tools, addressed by name via the `model` field.
- Multi-backend support: Anthropic, OpenAI, Gemini, Cohere, Deepseek, Groq.
- Per-user conversation memory with isolation, persistent in SQLite.
- Long-term memory with semantic recall — OpenAI and Voyage embedders, plus
  an offline `hash` fallback. Optional auto-extraction of durable facts.
- Tunable memory budgets (`context_budget`, `memory_budget_fraction`,
  `recall_k`).
- MCP tool integration over stdio and HTTP, with per-agent filtering.
- Subagents — expose one agent to another under `subagents:` with a `purpose:`
  description, depth-bounded.
- Per-user token rate limiting on hour, day, and month windows.
- LLM-as-judge evaluation — background scoring of agent replies against
  YAML-defined rubrics, with per-judge sampling.
- Experiments (A/B testing) — `split`, `shadow`, and `bandit` strategies with
  sticky-by-user routing.
- Smoke tests — scripted multi-turn conversations against agents or
  experiments.
- Read-only admin UI at `/admin/` for browsing conversations, memories, judge
  scores, and configuration.
- OpenTelemetry export and SQLite-backed event store.
- Docker image published to `ghcr.io/almaju/coulisse` for `linux/amd64` and
  `linux/arm64`.

[Unreleased]: https://github.com/Almaju/coulisse/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/Almaju/coulisse/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Almaju/coulisse/releases/tag/v0.1.0
