# Tracker

## Status
Milestones 1–2 done. Frontend scaffolded + embedded; UI itself not started.

## Milestones
- [x] 1. Backend skeleton: config, OpenRouter streaming, JSONL storage, SSE API
- [x] 2. Tree logic: edit / regenerate / switch leaf (+ unit tests)
- [ ] 3. Svelte UI: conversation list, chat view, streaming, markdown  ← **NEXT**
- [ ] 4. Branch switcher `‹ 2/3 ›`, edit / regenerate buttons
- [ ] 5. Settings panel: model picker, system prompt, provider options, cache toggle, usage/cost display
- [ ] 6. Attachments UI, stop button

## Next session — start here
- Done: `web/` = Vite 8 + Svelte 5 + TS 6 (TS 7 breaks svelte-check; keep `~6`). Dev: `cd web && npm run dev` (proxies `/api` → 8787). `npm run check` = typecheck.
- Done: `src/web.rs` embeds `web/dist` via rust-embed, SPA fallback to index.html (404 for missing `/assets/*`, `/api/*`).
- Build: `tools/build.sh [--release] [--force]` (npm install if needed, web build only if `web/` newer than `dist/index.html`, then cargo). `tools/run.sh [--release] [chatport args]`.
- `web/dist` must exist for `cargo build` (rust-embed) — use tools/build.sh.
1. Build the UI in `web/src/App.svelte` (placeholder now).
2. Client SSE: endpoints are POST, so use `fetch` + stream reader (not `EventSource`). Events: `user_saved`, `delta {text}`, `usage`, `error {message}`, `done {full assistant msg}`.
3. Client builds tree from `GET /api/conversations/:id` → `{meta, leaf, messages}`; siblings = same `parent_id`, order = array order.

## Backlog / TODO
- [x] Live test with real key (`openrouter/free`): streaming, usage/cost, multi-turn context, regenerate sibling — all OK.
- [ ] Verify cache hits with Claude Haiku: 2nd turn should show `cached_tokens > 0`. (Note: Anthropic has a min cacheable prompt size, ~1–4k tokens depending on model — use a long system prompt.)
- [ ] Record actual model used: router ids like `openrouter/free` resolve to a real model; response chunks carry `model` — store that in the assistant msg instead of the requested id.
- [ ] Stop endpoint `POST /:id/stop` (needs per-conversation cancel handle; generation runs in a spawned task).

## Deferred (not needed)
- Auto-title after first reply.
- [ ] Guard against two concurrent generations in one conversation (currently allowed; both save fine, last one wins leaf).
- [ ] Tidy: `store` uses blocking std fs inside async handlers — fine at this scale, revisit only if it matters.

## Decisions log
- Separate service from remotecontrol server (independent restarts/failures).
- OpenRouter only, thin `Provider` trait kept anyway.
- Per-conversation folder: `meta.json` (mutable) + `messages.jsonl` (append-only). Leaf lives in meta.json, not as JSONL records.
- No index.json: list = scan `meta.json` files.
- Branching = message tree via `parent_id` in one file (not file-per-branch).
- Attachments: text/md only, inline in message. Images later → `attachments/<hash>` refs.
- Caching: explicit `cache_control` on system + last user msg for `anthropic/*` only; others (incl. Gemini) implicit.
- Generation runs in a spawned task → reply saved even if browser disconnects.

## Handoff notes
- 2026-11-21: Backend done, 7 unit tests pass, no warnings. Smoke-tested with curl + fake key: create → send (with attachment) → SSE `user_saved/error/done` → partial msg persisted → leaf updated; bad leaf id rejected. Project moved to `workspace/rust/chatport`, renamed crate/binary to `chatport`.
- 2026-11-21: Added `--key` flag. Live test passed with `openrouter/free`. Auto-title deferred.
- 2026-11-21: Scaffolded web/ (Svelte 5/Vite/TS6), embedded via rust-embed, added tools/build.sh + run.sh. Smoke-tested `/`, SPA fallback, `/api`.
