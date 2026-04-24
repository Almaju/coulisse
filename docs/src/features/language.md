# Response language

Coulisse lets the caller pin the language the model replies in. Without it, the model infers language from the user's message — which can drift when the user switches languages mid-conversation or types short, ambiguous prompts. With it, every response comes back in the language you asked for.

Language is set per request, via the `metadata` object. The caller decides — Coulisse doesn't maintain a user-level language preference.

## How to send it

Add a `language` key to `metadata`. The value is a [BCP 47](https://en.wikipedia.org/wiki/IETF_language_tag) tag (RFC 5646):

```json
{
  "model": "assistant",
  "safety_identifier": "user-123",
  "metadata": {
    "language": "fr-FR"
  },
  "messages": [
    {"role": "user", "content": "Hello!"}
  ]
}
```

Any valid BCP 47 tag works: `en`, `fr`, `fr-FR`, `es-MX`, `zh-Hant`, `ja-JP`. The tag is validated — malformed values come back as `400 Bad Request`. Omit the key entirely to let the model pick.

## How it reaches the model

Coulisse appends a short instruction to the system preamble before calling the provider — something like `Respond in French.`. For tags in the built-in language-name table (common ISO 639-1 subtags: en, fr, es, de, it, pt, ja, zh, ko, ar, nl, pl, ru, sv, tr, hi), the instruction uses the English name. For anything else, the raw tag is passed through — frontier models understand BCP 47 directly, so `Respond in cy.` (Welsh) works fine.

The instruction is added once per request, as the first system message. Your own `system` messages in the `messages` array still apply, and agent preambles from `coulisse.yaml` are preserved.

## Real-world example: country code to language

A common pattern is to derive the language from the caller's locale on your side — phone country code, IP-based geolocation, browser `Accept-Language`, a user profile setting — and forward the resulting tag:

```json
{
  "model": "assistant",
  "safety_identifier": "+33612345678",
  "metadata": {
    "language": "fr-FR"
  },
  "messages": [
    {"role": "user", "content": "What's the weather?"}
  ]
}
```

Coulisse doesn't do the mapping itself. It takes the tag you send and asks the model to respond in that language. That keeps the metadata format stable and the country-code-to-language table (which changes slowly but does change) out of server code.

## Errors

A malformed tag returns `400 Bad Request`:

```json
{
  "error": {
    "type": "invalid_request",
    "message": "invalid `metadata.language`: invalid language tag: ..."
  }
}
```

Empty-string and whitespace-only values are rejected the same way.
