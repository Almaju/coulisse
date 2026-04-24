# Multi-backend support

Coulisse speaks to six providers out of the box:

- Anthropic
- OpenAI
- Gemini
- Cohere
- Deepseek
- Groq

You can mix them freely in a single config.

## Why mix backends?

- **Cost tiering.** Run quick tasks on a cheap model (Groq, Haiku, gpt-4o-mini), hard tasks on a flagship.
- **Capability routing.** Some tasks benefit from a specific provider's strengths — long-context summarization on Gemini, coding on Sonnet, reasoning on Opus.
- **Redundancy.** If one provider has an outage, flip a single `provider` field to route through another.
- **Evaluation.** A/B the same preamble on two different models without changing any client code.

## One config, many backends

```yaml
providers:
  anthropic:
    api_key: sk-ant-...
  openai:
    api_key: sk-...
  gemini:
    api_key: ...
  groq:
    api_key: ...

agents:
  - name: quick
    provider: groq
    model: llama-3.3-70b-versatile
    preamble: Answer briefly.

  - name: smart
    provider: anthropic
    model: claude-opus-4-7
    preamble: Think carefully.

  - name: long-context
    provider: gemini
    model: gemini-2.0-flash
    preamble: You excel at synthesizing long documents.
```

Your client picks one by name — everything else stays the same.

## The client side is unchanged

Because Coulisse exposes an OpenAI-compatible API no matter which provider is behind an agent, your client code never has to know. You don't install the Anthropic SDK, Gemini SDK, *and* OpenAI SDK side by side — you just use the OpenAI SDK and change the `model` field.
