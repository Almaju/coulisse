# Per-user memory

Every request that carries a user identifier gets an isolated, persistent memory scope. Coulisse tracks two kinds of memory:

- **Conversation history** — the running transcript of messages the user has exchanged.
- **Long-term memories** — facts and preferences, embedded for semantic recall.

You don't need to manage this — it happens automatically on every request.

## What happens on each request

1. Coulisse identifies the user from `safety_identifier` (or `user`).
2. It pulls the user's recent messages, fitting as many as possible into the context budget.
3. It runs a semantic recall against the user's long-term memories, picking the top matches.
4. It builds the final prompt: agent preamble → recalled memories → recent history → new message.
5. The model's reply is sent back *and* saved to that user's memory.

## Isolation guarantees

User isolation is enforced at the type level — a user's handle (`UserMemory`) can only ever read or write that user's data. There is no code path in Coulisse that mixes data across users. You can't accidentally leak user A's history to user B, because the API simply doesn't let you.

## The context budget

The default budget is:

| Knob                    | Default     | Meaning |
|-------------------------|-------------|---------|
| `context_budget`        | 8,000 tokens | Total window size for messages + memories. |
| `memory_budget_fraction`| 0.1 (10%)   | Share of the budget reserved for recalled long-term memories. |
| `recall_k`              | 5            | How many long-term memories to recall per request. |

The remaining 90% goes to recent message history, newest first. If the history doesn't fit, older messages are dropped.

> **Note:** these defaults live in code right now and aren't YAML-configurable yet. See the [roadmap](../reference/roadmap.md).

## Semantic recall

Long-term memories are embedded as vectors. On each request, Coulisse embeds the incoming message and retrieves the top-k most similar memories by cosine similarity. This is how context from a conversation two weeks ago can surface when it becomes relevant again.

> **Note:** the bundled embedder is a **placeholder** hash-based embedder (great for tests and demos, not for production). Swap it for a real embedder before you ship. You'll see this warning at startup:
>
> ```text
> memory: HashEmbedder (MVP placeholder — swap for a real embedder before production)
> ```

## What gets stored where

| Data                          | Scope       | Lifetime |
|-------------------------------|-------------|----------|
| Conversation messages         | Per user    | In-memory (lost on restart) |
| Long-term memories + vectors  | Per user    | In-memory (lost on restart) |
| User identifier → internal ID | Shared      | Derived deterministically — no storage needed |

Persistence across restarts is on the [roadmap](../reference/roadmap.md) — the current memory store is in-process only.
