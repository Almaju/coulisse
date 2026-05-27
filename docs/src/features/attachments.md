# File attachments (OpenAI-compatible storage)

Coulisse exposes a `/v1/files` API that matches the [OpenAI Files API](https://platform.openai.com/docs/api-reference/files) shape exactly. Any OpenAI-compatible SDK works without modification.

## What this lets you do

- Upload a file once, reference it by `file_id` in any subsequent chat request.
- Pass multimodal content (images, PDFs, text) to an LLM backend that supports it — Coulisse stores the file and forwards it transparently.
- Set a storage quota so the disk never fills up (oldest files evicted first).

## Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/v1/files` | Upload a file (multipart/form-data) |
| `GET` | `/v1/files` | List all uploaded files |
| `GET` | `/v1/files/:id` | Get metadata for one file |
| `GET` | `/v1/files/:id/content` | Download file content |
| `DELETE` | `/v1/files/:id` | Delete a file (idempotent) |

### Upload example

```bash
curl -X POST http://localhost:3000/v1/files \
  -F "file=@cv.pdf;type=application/pdf" \
  -F "purpose=assistants"
```

Response:

```json
{
  "id": "file-01j9abc...",
  "object": "file",
  "bytes": 42381,
  "created_at": 1722000000,
  "filename": "cv.pdf",
  "purpose": "assistants",
  "content_type": "application/pdf"
}
```

Then reference the file in a chat request:

```json
{
  "model": "gpt-4o",
  "messages": [{
    "role": "user",
    "content": [
      { "type": "text", "text": "Summarise this CV in three bullet points." },
      { "type": "input_file", "file_id": "file-01j9abc..." }
    ]
  }]
}
```

## Configuration

Add a `storage:` block to `coulisse.yaml`. Everything has a default — if you omit the block, a filesystem backend under `./coulisse-files` is used with no quota.

```yaml
storage:
  backend: fs           # "fs" (default) or "s3"
  fs:
    path: ./coulisse-files   # where blobs are stored on disk
  max_file_bytes: 52428800   # 50 MB per file — omit for no limit
  max_total_bytes: 524288000 # 500 MB total — omit for no limit
```

### S3-compatible backend

Swap `backend: s3` to store blobs in AWS S3, Cloudflare R2, or MinIO:

```yaml
storage:
  backend: s3
  s3:
    bucket: my-coulisse-files
    region: eu-west-3
    # endpoint_url: http://localhost:9000  # for MinIO / local S3
  max_file_bytes: 52428800
```

Credentials are read from the standard AWS credential chain (`AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` env vars, IAM role, `~/.aws/credentials`, etc.).

> **Note:** Set `endpoint_url` when using MinIO or another self-hosted S3-compatible service — path-style addressing is enabled automatically in that case.

## Allowed file types

Coulisse validates file content via magic bytes (not just the declared `Content-Type`) and rejects anything outside this list:

- `text/*`
- `image/*`
- `application/pdf`
- `application/json`
- `application/octet-stream`

Attempting to upload an executable or other unsupported type returns `415 Unsupported Media Type`.

## Storage limits and eviction

| Setting | Default | Effect |
|---------|---------|--------|
| `max_file_bytes` | no limit | `413 Payload Too Large` if exceeded |
| `max_total_bytes` | no limit | Oldest file is deleted to make room |

Eviction is FIFO: when a new upload would push the total over `max_total_bytes`, the oldest file (by `created_at`) is deleted first, then the next oldest, until there is room.

**S3 caveat:** quota accounting is best-effort under concurrent load — two simultaneous uploads might both pass the check and briefly exceed the limit. The next upload will evict back within bounds.

## Deduplication

Coulisse computes a SHA-256 of each uploaded file. If you upload the same bytes twice, the second call returns the *same `file_id`* — no storage is consumed and no blob is written twice.

> **⚠ v1 limitation — single-tenant only.**
> Deduplication is **global**: two different users uploading identical bytes share the same `file_id` and the same underlying blob. This means a `DELETE` by user A removes the file for user B as well.
>
> Coulisse v1 is designed for single-tenant use or for environments where all users trust each other (e.g. a team's internal tooling). **Do not expose Coulisse to mutually untrusted users until per-user scoped deduplication is implemented** (tracked in [#61](https://github.com/Almaju/coulisse/issues/61)).

## What Coulisse does NOT do

Coulisse does not parse, extract, or summarise file content. It stores the bytes and forwards them to the LLM backend. If the model supports the file type (e.g. GPT-4o reads PDFs natively), it will process it. If it does not, the request fails at the LLM level — Coulisse surfaces the error as-is.

If you want structured extraction (e.g. parse a CV into memory facts), that is a pattern you implement with a Coulisse agent that calls `memory.put` — see the [per-user memory](./memory.md) chapter.
