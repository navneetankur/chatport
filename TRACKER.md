# Tracker

## Status
Milestones 1–6 done (5 and 6 not yet clicked through in a browser).

## Milestones
- [x] 1. Backend skeleton: config, OpenRouter streaming, JSONL storage, SSE API
- [x] 2. Tree logic: edit / regenerate / switch leaf (+ unit tests)
- [x] 3. Svelte UI: conversation list, chat view, streaming, markdown
- [x] 4. Branch switcher `‹ 2/3 ›`, edit / regenerate buttons
- [x] 5. Settings panel: model picker, system prompt, provider options, cache toggle, usage/cost display
- [x] 6. Attachments UI, stop button

## Next session — start here
- Done: `web/` = Vite 8 + Svelte 5 + TS 6 (TS 7 breaks svelte-check; keep `~6`). Dev: `cd web && npm run dev` (proxies `/api` → 8787). `npm run check` = typecheck.
- Done: `src/web.rs` embeds `web/dist` via rust-embed, SPA fallback to index.html (404 for missing `/assets/*`, `/api/*`).
- Build: `tools/build.sh [--release] [--force]` (npm install if needed, web build only if `web/` newer than `dist/index.html`, then cargo). `tools/run.sh [--release] [chatport args]`.
- `web/dist` must exist for `cargo build` (rust-embed) — use tools/build.sh.
- UI: `App.svelte` (all views), `api.ts` (fetch + SSE reader over POST), `tree.ts` (`pathTo`), `markdown.ts` (marked + DOMPurify). Open conv in URL hash.
- Branching UI: `siblings()` in tree.ts; all streaming goes through `run()` in App.svelte (send / edit / regenerate). Switch = `POST /leaf` with the sibling id → server returns its latest descendant.
- SSE: endpoints are POST, so use `fetch` + stream reader (not `EventSource`). Events: `user_saved`, `delta {text}`, `usage`, `error {message}`, `done {full assistant msg}`.
- Settings: `Settings.svelte` (⚙ in header). Raw provider JSON is the source of truth; form fields write through it. Model picker = `<input list>` + datalist from `/api/models`. Header shows branch totals (tokens/cost).
- Stop: `AppState.running` = conv id -> (gen id, oneshot). Generation `select!`s on it; stopped reply saved `partial` with no error. Newer generation replaces the handle; finisher removes only its own.
- Attachments: 📎 in composer reads .txt/.md client-side (2 MB cap), chips removable; edit re-sends the original attachments.
- Title: server sets it on the first message of a "New chat" (`api::title_from`: first non-empty line → attachment name → conv id, 20 chars + …). Rename: ✎ in sidebar (or dbl-click) → PATCH title.
- Theme: CSS vars on `:root` in App.svelte (light/dark), global styles for button/textarea/input/select; `.primary` / submit = accent blue.
- Actual model: `ChatEvent::Model` from chunk `model` (deduped in provider task) → assistant `model`; `requested_model` set only when it differs. SSE `model {model}` event. Also `provider` from chunks → `provider_name` (SSE `provider` event).
- Message meta row: ‹n/m›, ↻ regen / ✎ edit icons, short line (model w/o vendor · cost), ⓘ toggles details (model, requested, provider, tokens, cost, time).
- `bigfiles/` (gitignored) holds raw captures, e.g. `openrouter-stream-free.sse` — read these instead of making new live calls.
- Client builds tree from `GET /api/conversations/:id` → `{meta, leaf, messages}`; siblings = same `parent_id`, order = array order.

## Backlog / TODO
- [x] Live test with real key (`openrouter/free`): streaming, usage/cost, multi-turn context, regenerate sibling — all OK.
- [ ] Verify cache hits with Claude Haiku: 2nd turn should show `cached_tokens > 0`. (Note: Anthropic has a min cacheable prompt size, ~1–4k tokens depending on model — use a long system prompt.)
- [x] Record actual model used: router ids like `openrouter/free` resolve to a real model; response chunks carry `model` — store that in the assistant msg instead of the requested id.
- [x] Stop endpoint `POST /:id/stop`.
- [x] UI colors: message box and send button are yellow on dark bg — ugly, pick something that fits the dark theme.
- [x] Chat title from first user msg (no AI): truncate to ~20 chars (fallback: conv id), instead of "New chat".
- [x] Rename chat (UI + endpoint to update meta title).

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
- 2026-11-21: Milestone 3 UI. svelte-check clean; live-tested send/stream via scratch config. Not yet clicked through in a real browser.
- 2026-11-21: Milestone 4 (branch switcher, inline edit, regenerate). svelte-check clean, builds. Needs a manual browser click-through.
- 2026-11-21: Milestone 5 (settings panel + branch usage totals). Frontend only; backend PATCH already existed. svelte-check clean; curl-tested /api/models + PATCH on scratch server. Router ids like `openrouter/free` aren't in /models, so the picker shows "Unknown model id" for them (harmless).
- 2026-11-21: Milestone 6 (stop + attachments). Live-tested via curl: stop mid-stream → partial saved; attachment content reaches the model. Next: the 3 UI TODOs (colors, title from first msg, rename).
- 2026-11-21: Auto-title from first msg, rename, dark-theme control colors (replaced unstyled default controls). Curl-tested title + rename. Colors need a look in the browser.
- 2026-11-21: Record actual model. Live: `openrouter/free` → `nvidia/nemotron-3.5-super-vl:free`, requested_model kept. Remaining backlog: Haiku cache-hit check.
- 2026-11-21: Provider name captured from stream; compact meta row + details toggle. Live: openrouter/free → baidu/ernie-5.5-flash-preview:free via Baidu. Not yet seen in a browser.
