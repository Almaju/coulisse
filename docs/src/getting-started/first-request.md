# Making a request

Coulisse exposes an OpenAI-compatible API, so any OpenAI SDK works. Point the client at `http://localhost:8421/v1` and set the `model` field to an agent name from your config.

## curl

```bash
curl http://localhost:8421/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "assistant",
    "safety_identifier": "user-123",
    "messages": [
      {"role": "user", "content": "Hello!"}
    ]
  }'
```

## Python (openai SDK)

```python
from openai import OpenAI

client = OpenAI(
    base_url="http://localhost:8421/v1",
    api_key="not-needed",  # Coulisse doesn't check this
)

response = client.chat.completions.create(
    model="assistant",
    messages=[{"role": "user", "content": "Hello!"}],
    extra_body={"safety_identifier": "user-123"},
)

print(response.choices[0].message.content)
```

## TypeScript / JavaScript

```typescript
import OpenAI from "openai";

const client = new OpenAI({
  baseURL: "http://localhost:8421/v1",
  apiKey: "not-needed",
});

const response = await client.chat.completions.create({
  model: "assistant",
  messages: [{ role: "user", content: "Hello!" }],
  // @ts-expect-error — extra field passed through
  safety_identifier: "user-123",
});

console.log(response.choices[0].message.content);
```

## The `safety_identifier` field

Coulisse identifies users through the `safety_identifier` field (or the deprecated `user` field, which works too). The identifier is what keeps each user's conversation history isolated when you run Coulisse for multiple users.

By default (`users: shared`), Coulisse routes every request to a single shared identity, so the field is ignored and you don't need to send it — handy when you're trying things out. Switch to `users: per-request` in `coulisse.yaml` once you have real users; Coulisse will then require the field on every request and reject anything that omits it. See [User identification](../configuration/user-id.md).

## Listing available agents

```bash
curl http://localhost:8421/v1/models
```

Returns every agent you've defined, in OpenAI's model-list format.

---

That's the whole loop. Next, dig into how to [configure providers](../configuration/providers.md).
