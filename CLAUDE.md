# chatport — preamble for new sessions

Personal LAN chat server for LLMs via OpenRouter. Single user, started on demand.
Rust backend (axum) + Svelte/TS frontend (not started yet), frontend to be embedded in the binary.

**Read first:** `TRACKER.md` (status, next steps, handoff notes), then `PLAN.md` (design) as needed.

## Ground rules
- Storage: `conversations/<group>/<id>-<label>/meta.json` (mutable, atomic rewrite) + `messages.jsonl` (append-only tree). `<group>` = group name or `_ungrouped`; `<label>` = title made folder-safe. Paths are looked up by id prefix on every op (`Store::find`), moves/renames under the write lock. Never rewrite messages.jsonl.
- Provider abstraction stays thin: one `Provider` trait, OpenRouter only. Provider-specific stuff (caching markers, `provider` routing object) lives in `src/provider/openrouter.rs`.
- Attachments: text/markdown only, stored inline in the message, flattened into the prompt in `api::flatten`.
- Optional HTTP Basic auth via `[auth]` in config (`src/auth.rs`); off when absent. Plain HTTP, LAN only.
- Frontend URLs are relative (`api/...`, Vite `base: './'`) so the app works behind a path prefix. Never add a leading `/`. Today routing is hash-only, so the page is always at the root and plain `api/...` is correct. If you add path-based routes, be careful: it's an SPA, so `api.ts` and `index.html` are shared across all depths, and the `../` count must be worked out per request from the current path (or resolve against `import.meta.url`), not hard-coded.
- Keep it small. Don't add features not in PLAN.md/TRACKER.md without asking.

## Commands
- `cargo test` — unit tests (tree, request body/caching, SSE parsing, flatten)
- `cargo build && ./target/debug/chatport -c config.toml` — run (see `config.example.toml`)
- Key priority: `--keyfile` > `OPENROUTER_API_KEY` env > config. (`--key` removed: argv is visible in `ps`.)

## Live testing
- Key is in `key.txt` (gitignored). Run: `./target/debug/chatport -c <cfg> --keyfile key.txt`. Never print/log the key.
- Use a throwaway config in the session scratchpad (own `bind` port + `data_dir`), not the real data dir.
- Model: `openrouter/free` by default. Only when a test needs a specific provider, use the cheapest fitting one (e.g. Claude Haiku for Anthropic cache markers). Check exact ids via `/api/models`.

## Maintaining the tracker
Update `TRACKER.md` at the end of each work chunk: tick done items, add new TODOs, write a short handoff note.
