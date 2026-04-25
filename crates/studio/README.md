# studio — Coulisse studio UI

Read-only Leptos WASM app for browsing Coulisse conversations and memories. Served by the main binary under `/studio`; the compiled bundle in `dist/` is embedded into the server via `rust-embed`.

This crate is **excluded from the root workspace** so `cargo build` / `cargo test` don't try to cross-compile it. Build it with `trunk` explicitly.

## One-time setup

```bash
rustup target add wasm32-unknown-unknown
cargo install trunk --locked
```

## Build the embedded bundle

```bash
trunk build --release
```

Produces `dist/`. Then rebuild the server so the new assets are embedded:

```bash
cd ../..
cargo run
```

Open `http://localhost:8421/studio`.

## Dev loop (hot reload)

```bash
# terminal 1 — main server on :8421
cargo run

# terminal 2 — Trunk on :4422, proxies /studio/api/* to :8421
cd crates/studio
trunk serve
```

Then open `http://127.0.0.1:4422/studio/`. Edit `src/`, the page hot-reloads in ~1s.

## Layout

- `src/api.rs` — typed client for `/studio/api/*`. Wire types mirror `crates/server/src/studio.rs` — keep them in sync.
- `src/components/` — shadcn-styled primitives (card, badge, spinner, empty state). Tailwind classes only.
- `src/pages/` — users list, conversation view.
- `index.html` — loads Tailwind via CDN.
- `Trunk.toml` — build/serve config and the dev API proxy.
