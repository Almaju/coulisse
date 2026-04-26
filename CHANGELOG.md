# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Pre-1.0 caveat: minor version bumps (0.x → 0.y) may include breaking changes to
the YAML schema, HTTP surface, or CLI. Patch bumps (0.x.y → 0.x.z) will not.

## [Unreleased]

### Changed

- Database migrations replaced the prior two-file `schema.sql` + `migrate.sql`
  model with a `coulisse_core::migrate::SchemaMigrator` trait. Each persistent
  crate declares an ascending `VERSIONS` list of schema-bumping crate
  versions; older databases walk `upgrade_from(version)` forward until they
  reach the latest. Versions are stored in a shared
  `coulisse_schema_versions` table, with arbitrary Rust available per upgrade
  step.

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
