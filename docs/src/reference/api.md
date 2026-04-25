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

## Studio endpoints

Coulisse also serves a read-only studio surface under `/studio`:

| Method | Path                                    | Returns                                         |
|--------|-----------------------------------------|-------------------------------------------------|
| `GET`  | `/studio`                               | The studio UI (Leptos WASM app). |
| `GET`  | `/studio/api/users`                     | List of users with message/memory counts. |
| `GET`  | `/studio/api/users/{user_id}/messages`  | Full conversation history for one user. |
| `GET`  | `/studio/api/users/{user_id}/memories`  | Long-term memories for one user (no embeddings). |

`{user_id}` must be a real UUID. See [Studio UI](../features/studio-ui.md) for what the interface does and how to build it.

## Auth

Coulisse doesn't check the `Authorization` header. API keys set by your SDK are ignored — authentication and rate limiting in front of Coulisse are your responsibility (run it behind a reverse proxy or API gateway). This applies to `/studio` too: expose it only on trusted networks.
