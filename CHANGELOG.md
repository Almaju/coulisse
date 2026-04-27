# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Pre-1.0 caveat: minor version bumps (0.x → 0.y) may include breaking changes to
the YAML schema, HTTP surface, or CLI. Patch bumps (0.x.y → 0.x.z) will not.

## [Unreleased]

### Added

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

[0.1.0]: https://github.com/Almaju/coulisse/releases/tag/v0.1.0
