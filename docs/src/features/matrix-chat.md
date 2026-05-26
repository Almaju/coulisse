# Matrix as the chat UI

Coulisse uses Matrix as its **office UI** — the place where humans address agents and where agents address each other — without learning what Matrix is. There's no Matrix code in the Rust binary. The integration lives entirely in a standalone Python bridge sidecar. The reference implementation lives at `local/matrix-bridge/bridge.py` in this repo's gitignored personal workspace.

This is a deliberate inversion from the earlier "Matrix as a narration sink" model: agents no longer post status updates from inside their preamble. Instead, **Matrix is the request/response surface itself** — like Slack for coworkers.

## Architecture

```
                   ┌─────────────────────────────────────┐
                   │  Matrix homeserver (Synapse)        │
                   │  ┌───────────────────────────────┐  │
   Human types     │  │ #engineering                  │  │
   "@business      │  │   alice: @business analyse X  │  │
   analyse X" ────►│  │   coulisse: [business] …      │  │
   in Element      │  └───────────────────────────────┘  │
                   └────────────────┬────────────────────┘
                                    │  sync
                                    ▼
                   ┌─────────────────────────────────────┐
                   │  Matrix bridge (Python sidecar)     │
                   │  - parses @<agent> from body        │
                   │  - fetches last N messages          │
                   │  - calls /v1/chat/completions       │
                   │    with model=<agent>               │
                   │  - posts reply back to room         │
                   └────────────────┬────────────────────┘
                                    │  HTTP
                                    ▼
                   ┌─────────────────────────────────────┐
                   │  Coulisse                           │
                   │  - OpenAI-compatible proxy          │
                   │  - knows nothing about Matrix       │
                   └─────────────────────────────────────┘
```

Coulisse's job: take an OpenAI request, run the named agent, return a reply. The bridge's job: translate Matrix mentions into OpenAI requests and post replies back. Cleanly separated.

## How it works

1. **A human (or another agent) posts in a Matrix room** with `@<agent>` somewhere in the message — e.g. `@business is this worth shipping this quarter?`.
2. **The bridge sees the message** via matrix-nio's sync loop. It parses the body for `@<name>` patterns and matches against its configured agent list.
3. **The bridge calls `POST /v1/chat/completions`** with `model: <agent>` and the last N messages from the room as conversation history.
4. **Coulisse runs the agent**: full preamble, MCP tools, subagent dispatch, judges — everything. Returns the reply.
5. **The bridge posts the reply back** to the same room, prefixed with `[<agent>]`. A custom `m.coulisse.hop` field on the message tracks how many agent ↔ agent forwards have already happened.
6. **If the agent's reply itself mentions another agent**, the bridge picks it up on the next sync and recurses — up to `COULISSE_MAX_HOPS` (3 by default) to prevent runaway loops.

The user_id Coulisse uses for memory partitioning is per-room (each room is its own memory bucket).

## What's no longer there (and why)

The old "agents post narration to Matrix as they work" model is gone. Three reasons:

1. **Coulisse stays agnostic.** No more `mcp.matrix` server, no more `messages_send_text` tool, no more per-agent `mcp_tools: - server: matrix` lines. The Rust binary doesn't know Matrix exists.
2. **Agents reply when they have something to say.** Humans don't narrate every keystroke. Removing the narration mandate cleans up the preambles and aligns with how real chat works.
3. **The internal "what's happening right now" surface is `/admin/live`,** not Matrix. Matrix is for *conversations*; the live page is for *operational visibility*.

## Configuration

The bridge runs as a sidecar declared in `coulisse.yaml`:

```yaml
sidecars:
  - name: matrix-bridge
    command: matrix-bridge/.venv/bin/python
    args: [matrix-bridge/bridge.py]
    env:
      MATRIX_BOT_PASSWORD: coulisse-dev
    restart: on-failure
```

(Paths are resolved relative to wherever Coulisse is invoked — typically your personal workspace folder.)

Environment variables consumed by the bridge:

| Variable                  | Default                                                        | Meaning                                                  |
| ------------------------- | -------------------------------------------------------------- | -------------------------------------------------------- |
| `MATRIX_HOMESERVER`       | `http://localhost:8008`                                        | Synapse client-server API base URL                       |
| `MATRIX_BOT_USER`         | `coulisse`                                                     | username localpart used to log in                        |
| `MATRIX_BOT_PASSWORD`     | `coulisse-dev`                                                 | bot account password                                     |
| `MATRIX_BOT_MXID`         | `@coulisse:localhost`                                          | full MXID for mention detection                          |
| `MATRIX_ACCESS_TOKEN`     | (unset)                                                        | optional pre-obtained token; skips `login()` if set      |
| `COULISSE_API`            | `http://localhost:8421/v1/chat/completions`                    | OpenAI-compatible chat endpoint                          |
| `COULISSE_AGENTS`         | `pm,business,coder,feature-lead,qa,qa-lead,release-manager,user-tester` | comma-separated agent names addressable via `@<name>`    |
| `COULISSE_MAX_HOPS`       | `3`                                                            | max agent ↔ agent forwards per chain                     |
| `COULISSE_CONTEXT_DEPTH`  | `10`                                                           | number of recent messages passed as conversation history |
| `COULISSE_TIMEOUT_S`      | `300`                                                          | hard cap on each `/v1/chat/completions` call             |

## Worked example

In Element, type in `#engineering`:

```
@business is the dispatch_task tool worth promoting in the README?
```

The bridge sees `@business`, calls Coulisse with `model: business`, business runs (with the recent room history as context), produces an answer. The bridge posts:

```
[business] One paragraph of business's analysis, then maybe a verdict.
```

If business's reply itself contains `@user-tester give it a try`, the bridge will pick that up on next sync, call user-tester, post their reply too — up to 3 hops.

## Element autocomplete

Today, `@business` is plain text — Element won't autocomplete it. For real Matrix autocomplete you'd need each agent registered as its own Matrix user (`@business:localhost`, `@pm:localhost`, …) and the bridge logged in as each. That's a future iteration; for now, plain-text `@<name>` works functionally and feels close enough to "real" mentions in practice.

## Plugging in other chat platforms

The split — agnostic Coulisse + thin Python bridge — generalises. To wire Slack, write a Slack listener that:
1. Watches for messages mentioning known agent names
2. Calls `POST /v1/chat/completions` with `model: <agent>`
3. Posts the reply back to the Slack channel

That's ~150 lines of Python with `slack-bolt`. The Coulisse side doesn't change. Same recipe for Discord, Mattermost, Zulip, etc.

The first-class chat substrate Coulisse ships with is Matrix because it's the only open-protocol, self-hostable option. Everything else is a bridge.
