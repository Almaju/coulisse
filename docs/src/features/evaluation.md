# LLM-as-judge evaluation

Coulisse can score every agent reply with a separate LLM — a *judge* — and persist the result so you can track quality over time. You describe *what* to evaluate in the YAML rubric; Coulisse handles scoring shape, format, sampling, and storage.

This is useful for watching agent drift, comparing model/preamble changes, and catching regressions without standing up a separate evaluation pipeline.

## How it works

1. A client sends a chat request. The agent replies as usual — the judge never blocks the response.
2. After the reply is persisted, Coulisse runs each judge the agent opted in to, in a background task.
3. Each judge samples according to its `sampling_rate` (skip entirely if the draw misses), then asks its backing model to score the assistant's reply against every rubric at once.
4. The response is parsed into one `score` row per rubric — persisted under the same user id as the conversation.
5. Failures (bad JSON, provider error, timeout) are logged at `warn` and swallowed — the user already got their answer.

Scores are stored in the same SQLite database as messages and memories, in a `scores` table keyed by `message_id`. Averages are computed at read time, not aggregated on write.

## YAML

```yaml
agents:
  - name: assistant
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    preamble: You are a helpful assistant.
    judges: [quality]              # opt in by name

  - name: translator
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    preamble: Translate into French.
    judges: [fluency]

judges:
  # Cheap, broad check — 100% of turns, small model.
  - name: quality
    provider: openai
    model: gpt-4o-mini
    sampling_rate: 1.0
    rubrics:
      accuracy:     Factual accuracy. Flag hallucinations.
      helpfulness:  Whether the assistant answered the user's question.
      tone:         Politeness and tone.

  # Targeted check for the translator — only 20% of turns.
  - name: fluency
    provider: openai
    model: gpt-4o-mini
    sampling_rate: 0.2
    rubrics:
      grammar:      Grammatical correctness of the French output.
      naturalness:  How native the phrasing sounds.
```

The wiring is visible from the agent: when you read an agent block you see which judges score it, rather than having to hunt through the judge list to figure out coverage.

## Rubrics

A rubric is a map from **criterion name** to a short description of what to assess.

```yaml
rubrics:
  accuracy:    Factual accuracy. Flag hallucinations.
  helpfulness: Whether the assistant answered the user's question.
```

Keep descriptions terse and assess-able. Don't write scale, format, or JSON instructions into them — Coulisse adds those internally. The description should tell the judge *what* matters, not *how* to answer.

Each criterion produces one `Score` row per scored turn, with its own numeric value and short reasoning. All criteria for one judge are evaluated in a **single** LLM call, so adding criteria to a judge doesn't multiply cost.

## Scoring shape

Every score is an integer in `0..=10` with a one-sentence reasoning. Coulisse forces this shape through the preamble and parses the judge's JSON reply — you don't configure it.

If you need a different scale (e.g. boolean pass/fail, categorical), that will arrive as a future `scale:` field; the default stays `numeric 0-10`.

## Sampling

`sampling_rate` controls what fraction of turns are scored.

| Value | Meaning |
|-------|---------|
| `1.0` (default) | Score every turn. |
| `0.1`           | Roughly 10% of turns. |
| `0.0`           | Never score (useful to park a judge without deleting it). |

The draw is independent per turn, per judge. Over many turns the scored fraction converges on the configured rate. Lower rates save tokens for expensive judges; broad cheap judges can run at `1.0`.

## Choosing a judge model

Pick a model that's *different* from the agent being scored whenever you can. A judge scoring its own output is biased — a cheap cross-provider judge (e.g. `gpt-4o-mini` judging a Claude agent, or vice versa) is usually closer to neutral.

Strong, slow models make sense for low-volume deep checks (`sampling_rate: 0.1`). Cheap, fast models make sense for high-volume broad checks (`sampling_rate: 1.0`).

## Multiple judges per agent

Stack judges to get different dimensions at different cost points:

```yaml
agents:
  - name: assistant
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    judges: [broad_check, deep_audit]

judges:
  - name: broad_check
    provider: openai
    model: gpt-4o-mini
    sampling_rate: 1.0
    rubrics:
      helpfulness: Whether the user's question was answered.
      tone:        Politeness and tone.

  - name: deep_audit
    provider: anthropic
    model: claude-opus-4
    sampling_rate: 0.05             # 5% of turns, expensive
    rubrics:
      accuracy:    Factual accuracy, including references and claims.
      safety:      Harmful, biased, or unsafe content.
```

Each judge is independent — its own model, rate, and rubric set. A turn can end up with zero, one, or both of these judges scoring it, depending on the sampling draw.

## Viewing scores

The admin UI at `/admin` now shows a **Scores** panel per user. It surfaces two things:

- **Averages** — mean score per `(judge, criterion)` across every turn the user has had, with sample count.
- **Recent** — the most recent individual scores with reasoning.

You can also query the JSON directly:

```
GET /admin/api/users/{user_id}/scores
```

returns `{"averages": [...], "scores": [...]}` — the same payload the UI renders.

## Validation at startup

Coulisse fails fast on:

- A judge referencing a provider that's not declared under `providers:`.
- A judge with no rubrics.
- A `sampling_rate` outside `[0.0, 1.0]`.
- An agent referencing a judge name that doesn't exist.

Any violation aborts startup with a message naming the offending judge or agent.

## Cost control

Two knobs matter:

1. **`sampling_rate`** — the easy one. Halve it, halve the judge bill.
2. **Judge model** — the big one. A `gpt-4o-mini` judge at 100% sampling often costs less than a `gpt-4o` judge at 10%. Pick the cheapest model that gives you a stable signal.

A useful pattern is to run a cheap judge at 100% and a strong judge at a small fraction — the cheap one catches the broad signal, the strong one spot-checks the hardest cases.
