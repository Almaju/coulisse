# HTTP API

Coulisse listens on `0.0.0.0:8421` and exposes an OpenAI-compatible surface.

## `POST /v1/chat/completions`

The main chat endpoint. Accepts the standard OpenAI chat completion request shape.

### Request

```json
{
  "model": "assistant",
  "safety_identifier": "user-123",
  "messages": [
    {"role": "user", "content": "Hello!"}
  ]
}
```

| Field               | Required | Notes |
|---------------------|----------|-------|
| `messages`          | yes      | The usual OpenAI message array. At least one `user` message is required. |
| `metadata`          | no       | Optional map of strings. Used for per-request rate limits — see below. |
| `model`             | yes      | Name of an agent from your config. |
| `safety_identifier` | yes¹     | Identifies the user. Can be any stable string. |
| `stream`            | no       | When `true`, the response is an SSE stream of `chat.completion.chunk` frames. See [Streaming responses](../features/streaming.md). |
| `stream_options`    | no       | Object. `include_usage: true` adds the `usage` field to the terminal stream chunk. |
| `user`              | —        | Deprecated OpenAI field; accepted as a fallback. |

¹ Required unless a `default_user_id` is set in `coulisse.yaml` — see [User identification](../configuration/user-id.md).

### Recognized metadata keys

`metadata` is a passthrough map of strings. Coulisse interprets the following keys; any other keys are ignored.

| Key                 | Type                  | Meaning |
|---------------------|-----------------------|---------|
| `language`          | BCP 47 tag            | Forces the response language, e.g. `fr-FR`. See [Response language](../features/language.md). |
| `tokens_per_day`    | integer (as string)   | Max tokens per rolling day. |
| `tokens_per_hour`   | integer (as string)   | Max tokens per rolling hour. |
| `tokens_per_month`  | integer (as string)   | Max tokens per rolling 30-day window. |

All optional. See [Rate limiting](../features/rate-limiting.md) for the token-limit behavior.

### Response

Standard OpenAI chat completion response:

```json
{
  "id": "...",
  "object": "chat.completion",
  "created": 1714000000,
  "model": "assistant",
  "choices": [
    {
      "index": 0,
      "message": {"role": "assistant", "content": "Hi!"},
      "finish_reason": "stop"
    }
  ]
}
```

### Streaming

Set `stream: true` to receive `chat.completion.chunk` frames over Server-Sent Events instead of one JSON response. The full wire format and disconnect semantics live in [Streaming responses](../features/streaming.md).

### Errors

Errors come back in OpenAI's error shape:

```json
{
  "error": {
    "type": "invalid_request_error",
    "message": "safety_identifier is required",
    "code": null
  }
}
```

Common cases:

- **400** — missing `safety_identifier` (when required), no user message, unknown agent name, unparseable `metadata` values.
- **429** — per-user token limit exceeded. Includes a `Retry-After` header with seconds until the window resets. See [Rate limiting](../features/rate-limiting.md).
- **5xx** — upstream provider error, MCP server failure.

## `GET /v1/models`

Lists every agent defined in the config.

### Response

```json
{
  "object": "list",
  "data": [
    {"id": "assistant", "object": "model", "owned_by": "coulisse"},
    {"id": "code-reviewer", "object": "model", "owned_by": "coulisse"}
  ]
}
```

Useful for UI dropdowns that want to populate a model picker from the server.

## Admin / config endpoints

Everything under `/admin/*` is a single content-negotiated surface. The same routes serve HTML pages to browsers, HTML fragments to htmx, and JSON to scripts — set `Accept: application/json` (or send an `HX-Request` header) to switch representation. Request bodies are equally tolerant: `application/json`, `application/yaml`, and `application/x-www-form-urlencoded` all deserialize into the same target type.

All admin routes are gated by the `auth.admin` scope.

### Agents

| Method | Path | Body | Notes |
|--------|------|------|-------|
| `GET`    | `/admin/agents`         | —              | List configured agents (HTML or JSON). |
| `POST`   | `/admin/agents`         | `AgentConfig`  | Create a new agent. 409 if the name is taken. |
| `GET`    | `/admin/agents/{name}`  | —              | Detail (HTML or JSON). |
| `PUT`    | `/admin/agents/{name}`  | `AgentConfig`  | Replace the named agent. Body name must match URL. |
| `DELETE` | `/admin/agents/{name}`  | —              | Remove the named agent. |
| `GET`    | `/admin/agents/new`     | —              | HTML form for a new agent. |
| `GET`    | `/admin/agents/{name}/edit` | —          | HTML edit form. |

`AgentConfig` is the same shape used in `coulisse.yaml`: `name`, `provider`, `model`, `preamble`, `purpose` (optional), `judges` (list, optional), `subagents` (list, optional), `mcp_tools` (list, optional).

### Judges, experiments, providers, MCP servers

Same CRUD shape as agents — list / create / one / update / delete. Adjust the path to suit:

| Path                            | Body                        | Notes |
|---------------------------------|-----------------------------|-------|
| `/admin/judges` + `/admin/judges/{name}`         | `JudgeConfig`         | LLM-as-judge evaluators. |
| `/admin/experiments` + `/admin/experiments/{name}` | `ExperimentConfig` | A/B routing groups. The runtime `ExperimentRouter` rebuilds on restart; admin display reflects the file in real time. |
| `/admin/providers` + `/admin/providers/{kind}`   | `ProviderConfig` (just `api_key`); `POST` body adds `kind` | Where `{kind}` is one of `anthropic`, `cohere`, `deepseek`, `gemini`, `groq`, `openai`. The runtime client is built at boot — restart to swap. |
| `/admin/mcp` + `/admin/mcp/{name}`               | `McpServerConfig` (`transport: stdio` + `command`/`args`/`env`, or `transport: http` + `url`); `POST` body adds `name` | Connections open at boot — restart to attach a new server. |

### Whole-file config

| Method | Path | Body | Notes |
|--------|------|------|-------|
| `GET` | `/admin/config` | — | Returns the file contents (`application/yaml` by default, JSON when `Accept: application/json`). |
| `PUT` | `/admin/config` | full YAML/JSON | Replaces `coulisse.yaml` atomically. Validation runs before any disk write. |
| `GET` | `/admin/openapi.json` | — | OpenAPI 3.1 description of every admin route, including request/response schemas. Feed it to `openapi-generator` or any client codegen for typed SDKs. |

### Validation, hot reload, the file watcher

Every write — admin form save, JSON `PUT`, hand-edit in `$EDITOR` — flows through the same pipeline:

1. The body is merged into the on-disk YAML (preserving sections this binary doesn't recognize).
2. The full result is deserialized into a `Config` and run through cross-feature validation (provider references, judge references, experiment variants, …).
3. Only on success does anything touch disk: a temp file is written and renamed atomically.
4. The file watcher fires, the new config is reloaded, and feature crates' hot-reloadable state (agent list, judges list, experiments list, settings view) atomically swaps in.

Errors return the validator's message verbatim with a `422 Unprocessable Entity` (or `400` for malformed bodies). The on-disk file is unchanged when validation rejects a write.

The studio UI is just one client of these endpoints — see [Studio UI](../features/studio-ui.md) for what the rendered surface offers and authentication options.

## Auth

By default Coulisse leaves `/v1/*` open. Configure the `auth.proxy` scope in YAML to require Basic credentials or OIDC for SDK clients; configure `auth.admin` to gate the studio. See [Authentication](../configuration/auth.md) for the full schema. Anything you don't gate is your responsibility to terminate at the infrastructure layer (reverse proxy, API gateway, VPN).
