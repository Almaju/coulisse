# Examples

End-to-end `coulisse.yaml` files for realistic deployments. Each one mixes
several features (memory, MCP tools with OAuth, subagents, judges,
experiments, triggers, telemetry) the way you'd actually combine them in
production — not one feature per file.

| File | Scenario |
|---|---|
| [`customer-support.yaml`](./customer-support.yaml) | SaaS in-app help widget. Front-line agent A/B-tests Claude vs GPT, delegates billing to a Stripe specialist and escalations to a Linear filer. Every reply scored by a judge. |
| [`personal-assistant.yaml`](./personal-assistant.yaml) | Single-user productivity setup. Orchestrator agent dispatches to planner / coder / scribe / researcher subagents, backed by Todoist, Atlassian, GitHub, and a local notes filesystem MCP. Cron triggers run a standup, an end-of-day wrap, and a weekly review. |
| [`pr-review-bot.yaml`](./pr-review-bot.yaml) | Automated PR review. GitHub webhook fires a reviewer agent that fans out to security / performance / clarity subagents; a bandit experiment shifts traffic to whichever prompt scores best over a rolling 14-day window. |

## Trying one locally

```bash
cp examples/personal-assistant.yaml coulisse.yaml
# fill in the env vars referenced in the file
export ANTHROPIC_API_KEY=...
export OPENAI_API_KEY=...
coulisse check     # validate
coulisse start     # run
```

`coulisse check` validates the file without contacting any provider, so
it's the fastest way to confirm a config is structurally sound before
booting.
