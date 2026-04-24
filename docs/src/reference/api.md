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
| `model`             | yes      | Name of an agent from your config. |
| `messages`          | yes      | The usual OpenAI message array. At least one `user` message is required. |
| `safety_identifier` | yes¹     | Identifies the user. Can be any stable string. |
| `user`              | —        | Deprecated OpenAI field; accepted as a fallback. |

¹ Required by default. Optional if you set `require_user_id: false` — see [User identification](../configuration/user-id.md).

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

- **400** — missing `safety_identifier` (when required), no user message, unknown agent name.
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

## Auth

Coulisse doesn't check the `Authorization` header. API keys set by your SDK are ignored — authentication and rate limiting in front of Coulisse are your responsibility (run it behind a reverse proxy or API gateway).
