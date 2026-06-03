# Structured outputs

Coulisse lets the caller pin the *shape* of the reply, not just its language. Send a JSON Schema and you get back a JSON value that conforms to it — validated server-side before it ever reaches you.

This is the same `response_format` field OpenAI's API exposes, so existing SDK calls work unchanged. The difference: Coulisse enforces it for **every** provider, including models that have no native structured-output mode. The schema is taught to the model through the system preamble and the reply is validated (and repaired) on the way out, so `anthropic`, `gemini`, `groq`, `cohere`, and `deepseek` behave the same as `openai`.

## How to send it

Add a `response_format` object to the request. Two shapes are supported.

### Any JSON object

```json
{
  "model": "assistant",
  "safety_identifier": "user-123",
  "messages": [{"role": "user", "content": "Give me a config skeleton"}],
  "response_format": {"type": "json_object"}
}
```

The reply is guaranteed to be a single valid JSON value — no markdown fences, no prose.

### A specific JSON Schema

```json
{
  "model": "assistant",
  "safety_identifier": "user-123",
  "messages": [{"role": "user", "content": "Extract the person from: Ada Lovelace, 36"}],
  "response_format": {
    "type": "json_schema",
    "json_schema": {
      "name": "person",
      "description": "a single person record",
      "schema": {
        "type": "object",
        "properties": {
          "age": {"type": "integer"},
          "name": {"type": "string"}
        },
        "required": ["age", "name"],
        "additionalProperties": false
      }
    }
  }
}
```

The `json_schema` object mirrors OpenAI's: `name` (required), `schema` (required, a standard JSON Schema), and optional `description` and `strict`. The reply is validated against `schema` before it's returned.

Omit `response_format` entirely (or send `{"type": "text"}`) for a normal free-form reply.

## How it reaches the model

Coulisse appends a short instruction to the system preamble before calling the provider — for a `json_schema` request it embeds the schema, its name, and (if given) its description, and tells the model to emit only the raw JSON value. Your own `system` messages and the agent's `coulisse.yaml` preamble are preserved.

After the model replies, Coulisse:

1. **Extracts** the JSON, tolerating a stray markdown code fence if the model added one.
2. **Validates** it — parses it, and for `json_schema` checks it against the schema.
3. **Returns the cleaned JSON** as the reply content (re-serialized, so any surrounding prose or fences are stripped).

### Repair on failure (non-streaming)

If validation fails, Coulisse re-prompts the same model with its own invalid reply plus the exact validation error, up to **two** times. Each retry is targeted ("you were missing the required field `age`"), not a blind re-roll. Token usage across every attempt is summed into the response's `usage` so billing and rate limits stay accurate.

If the reply still doesn't validate after the retries, the request fails with `502 Bad Gateway` — the model couldn't comply.

### Streaming

With `stream: true`, the instruction is injected the same way and tokens stream to you as usual. Coulisse validates the full accumulated reply once the stream ends. Because already-streamed tokens can't be retracted, a validation failure surfaces as an SSE error event rather than a repair retry — so for guaranteed-valid-or-error semantics, prefer non-streaming requests with structured output.

## Errors

| Status | When |
|--------|------|
| `400`  | The supplied JSON Schema itself is malformed (rejected before any model call). |
| `502`  | The model's reply never validated, even after repair retries. |

```json
{
  "error": {
    "type": "upstream_error",
    "message": "response did not match the schema: \"age\" is a required property"
  }
}
```
