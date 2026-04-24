# Admin UI

Coulisse ships a read-only admin UI that lets you browse the conversations and memories the server has seen. It's served by the same binary, under `/admin`.

Point a browser at `http://localhost:8421/admin` while the server is running.

## What you can do

- List every user the server has seen, most recent activity first, with message / memory / score counts.
- Open a user to see their full conversation (user, assistant, and system messages) with per-message token counts and relative timestamps.
- See the long-term memories recalled for that user, tagged as `fact` or `preference`.
- See the LLM-as-judge scores for that user, including mean score per `(judge, criterion)` and the most recent individual scores with reasoning.

That's it — this is a **read-only** tool. There's no way to send messages, edit memory, or mutate server state from the UI.

## No authentication

The admin UI is **not authenticated**. Don't expose it to the public internet. Put Coulisse behind a reverse proxy, a VPN, or an SSH tunnel if you need remote access.

## How it's built

The UI is a Leptos WASM app in `crates/admin/`, styled with Tailwind (loaded via CDN) and hand-rolled shadcn-style components. The compiled bundle is embedded into the server binary at build time via `rust-embed`, so there's still only one binary to ship.

## Building the bundle

The admin crate is excluded from the main workspace so `cargo build` / `cargo test` don't try to cross-compile it. Build it explicitly when you want to update the embedded bundle:

```bash
rustup target add wasm32-unknown-unknown   # once
cargo install trunk --locked               # once
cd crates/admin
trunk build --release
```

This produces `crates/admin/dist/`, which the server picks up the next time you rebuild it:

```bash
cargo run
```

If you hit `/admin` without having run `trunk build`, the server serves a placeholder page with these instructions instead of a blank 404. The JSON API under `/admin/api/*` still works either way.

## Dev loop

For iterative UI work, run `trunk serve` alongside the server. Trunk hot-reloads on every change and proxies `/admin/api/*` to the Coulisse server.

```bash
cargo run                # terminal 1 — server on :8421
cd crates/admin
trunk serve              # terminal 2 — UI on :4422 with hot reload
```

Open `http://127.0.0.1:4422/admin/`. Changes to `crates/admin/src/` rebuild and reload in about a second.

## JSON API

The UI is backed by three read-only endpoints. They're not part of the OpenAI-compatible surface — they're specifically for the admin UI, but you're free to hit them from scripts.

| Method | Path                                        | Returns                                                      |
|--------|---------------------------------------------|--------------------------------------------------------------|
| `GET`  | `/admin/api/users`                          | List of users with message / memory / score counts.          |
| `GET`  | `/admin/api/users/{user_id}/messages`       | Full conversation history for one user.                      |
| `GET`  | `/admin/api/users/{user_id}/memories`       | Long-term memories for one user (no embeddings).             |
| `GET`  | `/admin/api/users/{user_id}/scores`         | Judge scores for one user, plus mean per (judge, criterion). |

`{user_id}` must be a real UUID — the admin endpoints don't derive one from arbitrary strings the way `/v1/chat/completions` does, because they're looking up existing records.
