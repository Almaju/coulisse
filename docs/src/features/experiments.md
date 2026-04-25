# Experiments (A/B testing)

Run multiple agent configurations under a single addressable name and let Coulisse pick which one serves each request. Useful for comparing models, preambles, or tool sets without changing client code.

## How it works

1. Define each candidate as a normal agent under `agents:`.
2. Declare an `experiment` whose `name` is what clients send as `model`.
3. List the candidate agents as variants and choose a strategy.

When a request arrives, the router resolves the experiment name to one variant (and optionally fires off shadow runs in the background). The variant choice is **sticky-by-user** by default, so the same user always lands on the same variant for a given experiment — conversation memory and persona stay consistent across turns.

## Strategies

Three strategies are wired today: `split`, `shadow`, and `bandit`.

### `split`

Weighted random sampling. Sticky by user when `sticky_by_user: true` (the default) — the variant is a deterministic hash of `(user_id, experiment_name)` modulo the cumulative weights, with no database writes. Adding or removing a variant reshuffles users.

```yaml
agents:
  - name: assistant-sonnet
    provider: anthropic
    model: claude-sonnet-4-5-20250929
  - name: assistant-gpt
    provider: openai
    model: gpt-4o

experiments:
  - name: assistant            # what clients send as model
    strategy: split
    variants:
      - agent: assistant-sonnet
        weight: 0.5
      - agent: assistant-gpt
        weight: 0.5
```

### `shadow`

Designate one variant as `primary`; it serves the user normally. The other variants run in the background against the same prepared context, are scored by their judges, and never write to the user's message history. The user never waits on shadow variants.

`sampling_rate` (default `1.0`) controls how often shadow runs fire — set it lower to cap cost.

```yaml
experiments:
  - name: assistant
    strategy: shadow
    primary: assistant-sonnet
    sampling_rate: 0.25       # 25% of turns also run the shadows
    variants:
      - agent: assistant-sonnet
      - agent: assistant-gpt
```

Use shadow to collect comparison data before flipping a `split` rollout — the primary serves all real traffic while you build up scoring evidence on the challenger.

### `bandit`

Epsilon-greedy multi-armed bandit. Reads recent mean scores per variant from the existing `scores` table, picks the leader most of the time (`1 - epsilon`), and explores a random arm otherwise. Arms with fewer than `min_samples` recent scores are forced — the bandit only exploits once every arm has enough evidence.

```yaml
agents:
  - name: assistant-sonnet
    provider: anthropic
    model: claude-sonnet-4-5-20250929
    judges: [quality]
  - name: assistant-gpt
    provider: openai
    model: gpt-4o
    judges: [quality]

judges:
  - name: quality
    provider: openai
    model: gpt-4o-mini
    rubrics:
      helpfulness: Whether the assistant answered the user's question.

experiments:
  - name: assistant
    strategy: bandit
    metric: quality.helpfulness     # judge.criterion
    epsilon: 0.1
    min_samples: 30
    bandit_window_seconds: 604800   # 7 days
    variants:
      - agent: assistant-sonnet
      - agent: assistant-gpt
```

The configured judge (`quality`) and the criterion (`helpfulness`) must be declared on every variant agent — otherwise the bandit starves on that arm. Validation enforces this at startup.

A note on stickiness: with `sticky_by_user: true` (the default), the bandit decision is computed at request time via a deterministic hash of `(user_id, experiment_name)`, so a given user typically lands on the same arm. Mean scores update as new data arrives, so a user can shift if a different arm overtakes the leader — that is the trade-off for keeping the assignment stateless.

## Namespace and migration

Experiment names share a namespace with agent names. To A/B-test an existing agent without breaking clients:

1. Rename the agent (`assistant` → `assistant-v1`).
2. Add a sibling agent (`assistant-v2`).
3. Add an experiment named `assistant` with both as variants.

Clients keep sending `model: assistant` and it resolves transparently.

Variants stay individually addressable as agents under their own names (`assistant-v1`, `assistant-v2`) — useful for isolating one variant in tests or debugging.

## Subagents

A subagent reference can name an agent **or** an experiment. If `orchestrator` lists `subagents: [assistant]` and `assistant` is an experiment, every subagent call resolves to a variant for the calling user, the same way a top-level request would. Sticky-by-user keeps the variant consistent across the whole conversation.

Give the experiment a `purpose:` if it's exposed as a subagent — it becomes the tool description the calling agent's LLM sees:

```yaml
experiments:
  - name: assistant
    purpose: A general-purpose chat assistant.
    strategy: split
    variants:
      - agent: assistant-sonnet
      - agent: assistant-gpt
```

Bandit subagents read mean scores at call time, so the same exploit/explore behaviour applies inside subagent dispatch.

## Telemetry

Each turn's `TurnStart` event includes `agent` (the resolved variant), and when an experiment was hit, `experiment` (the experiment name) and `variant` (same as `agent`). Judge scores are tagged with the variant's agent name in the database, so per-variant aggregation flows through the same table without a join — used by the bandit's mean-score query and the studio's per-variant view.

## Studio

The studio shows configured experiments at `/studio/experiments`: strategy, sticky-by-user flag, and per-variant weight + share. For bandit experiments, the page additionally shows the configured metric, epsilon, and min-samples threshold, plus per-variant sample counts and mean scores, with the current leader highlighted. Shadow experiments call out the primary variant.

## Validation

Coulisse rejects the following at startup:

- Experiment name colliding with an agent name (rename one).
- Experiment name colliding with another experiment.
- Experiment with zero variants.
- Variant referencing an undefined agent.
- Variant weight `<= 0`.
- Duplicate variant agent within one experiment.
- Strategy-specific fields used with the wrong strategy (e.g. `primary` on a `split` experiment).
- `shadow` without a `primary`, or with a `primary` that's not one of the variants.
- `shadow` `sampling_rate` outside `[0.0, 1.0]`.
- `bandit` without a `metric`.
- `bandit` `metric` that doesn't match an existing `judge.criterion`, or a variant that doesn't opt into the metric's judge.
- `bandit` `epsilon` outside `[0.0, 1.0]`.
