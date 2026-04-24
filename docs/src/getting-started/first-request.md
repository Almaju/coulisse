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

Coulisse identifies users through the `safety_identifier` field (or the deprecated `user` field, which works too). The identifier is what keeps each user's conversation history isolated.

You can turn this off — see [User identification](../configuration/user-id.md) — but by default every request needs one.

## Listing available agents

```bash
curl http://localhost:8421/v1/models
```

Returns every agent you've defined, in OpenAI's model-list format.

---

That's the whole loop. Next, dig into how to [configure providers](../configuration/providers.md).
