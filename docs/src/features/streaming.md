# Streaming responses

Coulisse implements OpenAI's Server-Sent Events (SSE) format for chat completions. Set `stream: true` in the request and the server emits incremental `chat.completion.chunk` frames over the wire — drop-in compatible with the OpenAI Python and JavaScript SDKs and any client that already speaks the OpenAI streaming protocol.

## Asking for a stream

Add `stream: true` to a normal `/v1/chat/completions` request:

```json
{
  "model": "assistant",
  "safety_identifier": "user-123",
  "messages": [{"role": "user", "content": "Hello!"}],
  "stream": true
}
```

The response is `text/event-stream` instead of JSON. Each frame is one `chat.completion.chunk`.

## Wire format

The first frame announces the assistant role:

```text
data: {"id":"chatcmpl-coulisse-...","object":"chat.completion.chunk","created":...,"model":"assistant","choices":[{"index":0,"delta":{"role":"assistant"}}]}
```

Then one frame per text delta:

```text
data: {"id":"chatcmpl-coulisse-...","object":"chat.completion.chunk","created":...,"model":"assistant","choices":[{"index":0,"delta":{"content":"Hello"}}]}

data: {"id":"chatcmpl-coulisse-...","object":"chat.completion.chunk","created":...,"model":"assistant","choices":[{"index":0,"delta":{"content":" there"}}]}
```

A terminal frame sets `finish_reason`:

```text
data: {"id":"chatcmpl-coulisse-...","object":"chat.completion.chunk","created":...,"model":"assistant","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: [DONE]
```

## Including token usage

Set `stream_options.include_usage: true` to receive a `usage` field on the terminal chunk:

```json
{
  "model": "assistant",
  "messages": [{"role": "user", "content": "Hi"}],
  "stream": true,
  "stream_options": {"include_usage": true}
}
```

The terminal frame then carries usage:

```text
data: {"...":"...","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"completion_tokens":3,"prompt_tokens":7,"total_tokens":10}}
```

When `include_usage` is missing or `false`, the field is omitted — matching OpenAI's contract.

## Memory and rate limiting

Streaming responses use the same per-user memory bucket and rate-limit accounting as non-streaming requests:

- The user's message and the assistant's reply are appended to memory **after** the stream ends.
- Token usage is recorded against the rate-limit window when the stream ends.
- If the **client disconnects mid-stream**, Coulisse persists the partial assistant reply (everything received before the disconnect). This matches what the user actually saw — the next turn won't claim the model said something the user never received.

## Tool-using agents

Agents with MCP tools attached stream the same way. Tool-call internals run inside the rig multi-turn loop and are not surfaced to the client; you'll see a pause while a tool runs, then the model's text continues. The `delta.content` field is the only delta variant Coulisse currently emits.

## Errors mid-stream

If the upstream provider fails after the stream has started, Coulisse emits one terminal frame containing an `error` field with the failure reason, then `[DONE]`. The HTTP status is already `200` by then — clients should check for the `error` field on the final chunk.
