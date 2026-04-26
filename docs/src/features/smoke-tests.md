# Smoke tests

A *smoke test* is a synthetic-user persona that drives a conversation against one of your agents (or experiments). Coulisse plays the user — you write a preamble describing who they are and what they want — and the assistant replies for real. Every assistant turn flows through the same judge pipeline as production traffic, so you get a transcript and scores back without writing any harness code.

Smoke tests are most useful when you're iterating on a prompt: tweak the preamble, hit "Run now" in the studio, watch the scores. Pair them with [experiments](./experiments.md) and a single click runs every variant once, sticky-by-user routing samples them across repetitions, and the judge scores feed straight into bandit selection.

## How it works

1. You trigger a run from the studio (`/admin/smoke/<name>`) — no client needed.
2. Coulisse opens a fresh synthetic user id and starts a loop:
   - The persona model produces a "user" message — given the conversation so far with roles flipped (so the model speaks *as* the user).
   - The target agent replies as it normally would, with all its real MCP tools, subagents, and preambles.
   - The reply is fanned out to every judge the target agent opts into. Scores land in the same `scores` table as production runs, keyed by the assistant turn's id.
3. The loop stops when either side emits the configured `stop_marker`, or when `max_turns` is hit.
4. The full transcript is browsable at `/admin/smoke/runs/<run_id>` — assistant in slate, persona in amber.

Smoke runs never write to the user's memory or rate-limit windows. Each repetition uses a brand-new synthetic user id, so split/bandit experiments naturally sample variants across reps.

## YAML

```yaml
smoke_tests:
  - name: jobseeker_basic
    target: tremplin                 # agent or experiment name
    persona:
      provider: anthropic
      model: claude-haiku-4-5-20251001
      preamble: |
        You are role-playing a 28-year-old looking for a developer job in Paris.
        Reply like a real human: short questions, follow-ups as the conversation goes.
        When you have a satisfactory answer, finish with "[FIN]".
    initial_message: "Hi, I'm looking for work."
    stop_marker: "[FIN]"
    max_turns: 10
    repetitions: 5
```

| Field             | Required | Default | Notes                                                                                            |
|-------------------|----------|---------|--------------------------------------------------------------------------------------------------|
| `name`            | yes      |         | Unique within `smoke_tests`. Shows up at `/admin/smoke/<name>`.                                  |
| `target`          | yes      |         | Agent name or experiment name. Resolved through the experiment router per run.                   |
| `persona`         | yes      |         | Provider, model, and preamble for the synthetic user.                                            |
| `initial_message` | no       |         | Hard-coded first message from the persona. Skipping this lets the persona open the conversation. |
| `stop_marker`     | no       |         | Substring that ends the run when emitted by either side.                                         |
| `max_turns`       | no       | `10`    | Cap on persona-then-agent pairs.                                                                 |
| `repetitions`     | no       | `1`     | Independent runs launched per "Run now" click. Each gets a fresh synthetic user id.              |

## Iterating with experiments

Define two variants of an agent (e.g. `assistant-v1`, `assistant-v2`), wrap them in a bandit experiment, and target the experiment name from a smoke test:

```yaml
experiments:
  - name: assistant
    strategy: bandit
    metric: quality.helpfulness
    variants:
      - agent: assistant-v1
      - agent: assistant-v2

smoke_tests:
  - name: convergence
    target: assistant
    repetitions: 50
    persona: { provider: openai, model: gpt-4o-mini, preamble: "..." }
```

Hit "Run now" once and the bandit accumulates 50 samples per variant per turn pair. The experiment page picks the winner on its own.

## Limitations (today)

- Smoke runs bypass the memory pipeline. Fact extraction and semantic recall are not exercised.
- No scheduled runs — trigger is manual via the studio.
- No tool-call assertions; assertions about *what* the agent did during a turn live in the judge rubrics.
