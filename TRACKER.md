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
- Settings: `Settings.svelte` (⚙ in header). Raw provider JSON is the source of truth; form fields write through it. Model picker = `ModelPicker.svelte` (custom dropdown, word-wise search in `modelSearch.ts`: every word must match id or name; exact id, then id prefix first; max 100 shown). Model list fetched once per page load (`api.listModels` memoised); server also caches 1h. Header shows branch totals (tokens/cost).
- Stop: `AppState.running` = conv id -> (gen id, oneshot). Generation `select!`s on it; stopped reply saved `partial` with no error. Newer generation replaces the handle; finisher removes only its own.
- Attachments: 📎 in composer reads .txt/.md client-side (2 MB cap), chips removable; edit re-sends the original attachments.
- Title: server sets it on the first message of a "New chat" (`api::title_from`: first non-empty line → attachment name → conv id, 20 chars + …). Rename: ✎ in sidebar (or dbl-click) → PATCH title.
- Theme: CSS vars on `:root` in App.svelte (light/dark), global styles for button/textarea/input/select; `.primary` / submit = accent blue.
- Actual model: `ChatEvent::Model` from chunk `model` (deduped in provider task) → assistant `model`; `requested_model` set only when it differs. SSE `model {model}` event. Also `provider` from chunks → `provider_name` (SSE `provider` event).
- Message meta row: ‹n/m›, ↻ regen / ✎ edit icons, short line (model w/o vendor · cost), ⓘ toggles details (model, requested, provider, tokens, cost, time).
- Reasoning: `delta.reasoning` → `ChatEvent::Reasoning` → message `reasoning` (stored, never sent back as context), SSE `reasoning {text}`. UI: collapsible "Thinking" block (open while thinking). `usage.reasoning_tokens` (part of completion) shown in details.
- Reasoning effort: `meta.reasoning_effort` (enum max…none, unset = model default) → body `reasoning: {effort}`. Settings dropdown. PATCH `null` clears. Note: some models reject `none` with a 400 (reasoning mandatory) — surfaced as reply error, deliberately not auto-handled.
- API keys: config `[[keys]] {name, key}` + `default_key` (default: first). `--keyfile`/env override the default key only (or create "default"). `Config::resolve_keys` at startup, `key_for(name)` per request. `meta.key_name` (None = default) → `ChatRequest.key`; missing key = reply error. `GET /api/keys` returns names only. `/api/models` uses the default key. Settings shows a picker only when >1 key.
- Composer and inline edit share `web/src/MessageInput.svelte` (textarea, 📎, `@` chips + autocomplete, removable attachment chips; Enter submits, Esc cancels edit; buttons via `actions` snippet). App.svelte only owns send/edit/stop.
- `@path` mentions: `web/src/mentions.ts` parses complete tokens (`@a.md ` ends at whitespace, `@"a b.md"` at the closing quote, no escapes; on send the trailing one counts too). Composer `$effect` on `input` → `GET /api/file?conv=&path=` (`src/files.rs`: `/abs`, `~/x`, else relative to `meta.file_root`; text only, 256 KB cap) → chips (ok / loading / error). × dismisses (text kept). `@` stays in the text; found files are sent as ordinary attachments named by the typed path. `config.default_file_root` seeds new chats; settings has "File root".
- `@` autocomplete: `typingAt` finds the token at the cursor → 150 ms debounce → `GET /api/ls?conv=&prefix=` (`files::list`: dirs first, dot-entries only if typed, 50 max) → dropdown above the textarea. ↑/↓ or Tab/Shift+Tab move, Enter picks (dir keeps completing, file adds a space / closing quote → chip), Esc closes. Names with spaces get quoted.
- File tools: `src/tools.rs` (specs, `jail`, run → `ToolResult {output, error, diff}`). `generate` loops rounds (max 20) while the model returns tool calls; one assistant msg holds `tool_rounds`. SSE `tool_round {content, calls, results}` (client moves streamed content into the round), `usage` is the running total. `api::context` builds provider messages (expanded / folded). UI: 🔧 collapsible per call with coloured diff; settings checkbox "File tools" (disabled without file root). Tested with `google/gemini-3.8-flash`.
- Web tools: `meta.web_tools` → `ChatRequest.web_tools` → `openrouter::web_tools()` appended to `tools` (after function tools). Engines/limits are consts there, not config. `generate` drops them (message `warning`) when `/models` lists the model without `tools` (`ModelInfo.tools` from `supported_parameters`); unknown ids keep them. `Usage.web_searches` from `server_tool_use_details`; ⓘ shows "Web searches". Settings checkbox "Web tools" (disabled for known no-tools models). Batch never sends them.
- Raw view: `openrouter::is_plain_text` (delta only role/content/reasoning/reasoning_details/tool_calls, no finish_reason, no usage) — every other chunk → `ChatEvent::Raw` → SSE `raw` (all rounds). Never stored: `run()` collects them, `lastRaw {id, chunks}` set on `done`, reset at next run; `{}` button on that message only, gone on reload.
- `bigfiles/` (gitignored) holds raw captures, e.g. `openrouter-stream-free.sse` — read these instead of making new live calls.
- Groups: `Store::{move_to, rename_group, delete_group}`; `PATCH /conversations/:id {group}` (null/"" = ungroup), `PATCH /api/groups/:name {name}`, `DELETE /api/groups/:name?chats=true|false`. `GET /conversations` returns `Summary {id, group, title (=folder label), updated_at (mtime)}`, not Meta.
- Client builds tree from `GET /api/conversations/:id` → `{meta, leaf, messages}`; siblings = same `parent_id`, order = array order.

## Backlog / TODO
- [x] Batch models (`<slug>:batch` from the normal picker): reply comes later via ⟳.
- [x] Batch auto-poll, server side (`src/batch.rs` poller, `[batch] poll_interval`, default 1h).
- [ ] Batch UI (incl. provider + expiry in the pending row): not yet clicked through in a browser (API live-tested via curl).
- [x] Live test with real key (`openrouter/free`): streaming, usage/cost, multi-turn context, regenerate sibling — all OK.
- [x] Verify cache hits with Claude Haiku: 2nd turn should show `cached_tokens > 0`. (Note: Anthropic has a min cacheable prompt size, ~1–4k tokens depending on model — use a long system prompt.)
- [x] Record actual model used: router ids like `openrouter/free` resolve to a real model; response chunks carry `model` — store that in the assistant msg instead of the requested id.
- [x] Stop endpoint `POST /:id/stop`.
- [x] UI colors: message box and send button are yellow on dark bg — ugly, pick something that fits the dark theme.
- [x] systemd socket activation (`src/listen.rs`, example units in `contrib/`). Unusable activation → warn + bind from config. No idle exit.
- [x] Chat title from first user msg (no AI): truncate to ~20 chars (fallback: conv id), instead of "New chat".
- [x] Rename chat (UI + endpoint to update meta title).
- [x] Show current reasoning effort somewhere visible, e.g. header next to title/model (only when set).
- [x] New conversations default to `provider.data_collection: "deny"` (unless request/config sets it). Existing convs not migrated. Verified `openrouter/free` still routes with deny.
- [x] Per-message copy / download (.md, `<title-slug>-<msg-id>.md`) for user + assistant; raw-markdown toggle on assistant msgs (not persisted); copy button on code blocks. Reasoning excluded. Clipboard falls back to execCommand on plain-HTTP LAN.
- [x] Multiple named API keys in config, picked per conversation in settings (switchable mid-chat).
- [ ] Code blocks: syntax highlighting (highlight.js).
- [x] File tools (list/read/write/edit in file root, auto-applied, diffs stored).
- [ ] File tools UI: not yet clicked through in a browser (live-tested via curl only).
- [x] Cache marker on the newest input (user or tool result), so tool loops cache per round.
- [x] `--key` replaced by `--keyfile PATH` (argv is visible in ps).
- [x] `@path` attachments: server-side text files, chip above the composer once the token is complete.
- [x] `@` autocomplete: debounced `GET /api/ls?conv=&prefix=` → dropdown (dir → append `/`, file → finish token).
- [x] Editing a user message can remove its attachments (× per attachment while editing; the edit sends the rest).
- [x] View sent attachments: click the 📎 chip → plain text in a new tab (blob URL); `.md` also gets "rendered" (DOMPurify'd HTML page).
- [x] Chat groups: folder per group (`conversations/<group>/<id>-<label>/`, `_ungrouped` reserved). Old layout was migrated by a one-off script (not in the code). List = folder names + `messages.jsonl` mtime (no meta.json reads). Groups exist only while they have chats (created by "move to group", removed when empty). Sidebar: collapsible groups (state in localStorage, closed by default), group view at `#g/<name>` (new chat from there joins the group), ⇄ move, rename group, delete group (ungroup chats / delete all).
- [ ] Chat groups: not yet clicked through in a browser (API live-tested via curl).
- [x] Sidebar rows are real `<a href="#id">` / `#g/<name>` links (middle-click, open in new tab); `route()` on hashchange drives navigation.
- [x] Draft new chat: "+ New chat" and the landing page (empty hash) show a draft (`id ""`, from `GET /api/conversations/new`) kept only in the browser. Settings/header edits change it locally; first send does `POST /api/conversations` with `draftPatch` + group (create takes PATCH fields). No more empty "New chat" folders.
- [x] Recent models: `<data_dir>/state.json` (`src/recent.rs`, `MAX_RECENT = 20`), the *picked* id (e.g. `openrouter/free`), recorded on send/regenerate. New chats default to the newest (config `default_model` only when empty). `GET /api/recent-models`; ModelPicker shows "Recent" then "All models" until you type.
- [x] Header: double-click title to rename; click model (▾) for an inline ModelPicker that applies on pick.
- [x] Settings: Save/Cancel row is sticky at the panel bottom.
- [ ] Draft chat / recent models / header editing: not yet clicked through in a browser (API live-tested via curl).
- [x] Web tools (OpenRouter server tools: Exa search + fetch, datetime), per-chat checkbox, off by default. See PLAN "Web tools".
- [ ] Web tools UI: not yet clicked through in a browser (live-tested via curl).
- [ ] Web tools: optional per-model-prefix engine override (e.g. `google/` → native, pinned to AI Studio) — only if BYOK free grounding quota turns out to be real.
- [ ] Web tools + file tools: server-tool limits reset each tool round; cap total `web_searches` across the loop if it ever matters.
- [ ] Nice-to-have: per-group system prompt addendum (`<group>/group.json`).
- [ ] Nice-to-have: attachment viewer as modal/side panel instead of a new tab.
- [x] User messages render markdown too (shared `content` snippet in App.svelte, same `</>` raw toggle as replies).

## Deferred (not needed)
- Persisted sources list: keep `url`+`title` from `delta.annotations` url_citations (drop ~500-char `content`), collapsible "Sources" under the reply. Citations come only from search (fetch adds none), all `uncited: true` with 0 indices, so no inline markers possible. The `{}` raw view covers the last reply for now.
- Auto-title after first reply.
- Own `fetch_url` function tool (HTTP GET + a Readability port, e.g. `dom_smoothie`): only if Exa fetch gets ~10x pricier than $1/1k pages, or LAN/private URLs are needed.
- [ ] Guard against two concurrent generations in one conversation (currently allowed; both save fine, last one wins leaf).
- Won't do (by choice): `store` uses blocking std fs from async handlers. Ops are tiny (small meta.json, one-line appends), single user — accepted practice. `tokio::fs` is just spawn_blocking per call, gains nothing. If it ever matters: keep `Store` sync, wrap handler calls in `tokio::task::spawn_blocking` (std Mutex stays fine — never held across `.await`).

## Decisions log
- Separate service from remotecontrol server (independent restarts/failures).
- OpenRouter only, thin `Provider` trait kept anyway.
- Per-conversation folder: `meta.json` (mutable) + `messages.jsonl` (append-only). Leaf lives in meta.json, not as JSONL records.
- No index.json: list = scan `meta.json` files.
- Branching = message tree via `parent_id` in one file (not file-per-branch).
- Attachments: text/md only, inline in message. Images later → `attachments/<hash>` refs.
- App attribution headers on every request, created hidden (no public rankings).
- Caching: explicit `cache_control` on system + last user/tool msg for `anthropic/*` only; others (incl. Gemini) implicit.
- Generation runs in a spawned task → reply saved even if browser disconnects.
- Batch = `:batch` model suffix (no separate toggle); one pending batch per conversation, stored in meta.json; manual ⟳ only, no webhooks (LAN).
- `session_id` = conversation id on every request (shared across branches): sticky provider routing from turn 1, Sessions grouping in OpenRouter logs. Ignored for routing when `provider.order` is set.

## Handoff notes
- Batch auto-poll: `batch::start_poller` (skipped when `poll_interval` is 0). In-memory `AppState.batch_polls` (conv id → next check), filled by a startup scan of all metas and on submit. The poller sleeps until the earliest due check (`sleep_until`), or indefinitely when none; `schedule`/`unschedule` ring `AppState.batch_wake` (tokio `Notify`, keeps a permit if the poller is busy) so it re-reads the earliest time. Due checks go through the same `batch::refresh` as ⟳. Gap = batch age, min 5 min, max `poll_interval` (checks at 5, 10, 20, 40 min, then every interval), so restarts need no stored schedule. `AppState.batch_lock` serialises saving/clearing a pending batch (⟳ vs poller → saved once; loser gets `{status:"done"}` and the UI reloads). Batch gone upstream after 2 min → saved as an error reply. An open page does not update itself: reload or ⟳.
- Batch: model id ending `:batch` → send/regenerate submit a one-request batch (`POST /batches`, `/v1/chat/completions`, `build_batch_body`) instead of streaming. SSE: `user_saved`, then `batch {PendingBatch}` (or `error` + `done` with an error reply). `meta.pending_batch {id, user_id, model, key_name, submitted_at}` blocks send/regenerate (409) until `POST …/batch/refresh` (poll once; done → reply appended, leaf set, pending cleared, upstream delete best-effort) or `POST …/batch/forget`. No upstream cancel exists: forget on a running batch just stops waiting, it is still billed. Fresh batches 404 for a few seconds → refresh reports `submitted` for 2 min. No tools in batch (one round; tool calls → warning). `provider` keeps only `only`/`allow_fallbacks` (others are a 400 for batch; logged as dropped).
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
- 2026-11-21: Reasoning text + reasoning token count. Also added the missing provider parse test (last round's test edit silently didn't apply).
- 2026-11-21: Reasoning effort setting. Live: bad value → 422; `none` on ernie-5.5-flash → OpenRouter 400 (mandatory reasoning), `high` OK. Capture in bigfiles/openrouter-stream-effort-high.sse.
- 2026-11-21: Cache verified with anthropic/claude-haiku-5.5, ~14.7k-token system prompt: turn 2 cached_tokens=14675/14697, cost $0.00184 → $0.00015. Raw SSE in bigfiles/chatport-haiku-cache-2turns.sse. Added TODO: show effort in header.
- 2026-11-21: Header shows `· <effort> effort` when set.
- 2026-11-21: Phone layout: ☰ in header opens sidebar as slide-over (<640px), closes on open/new chat/backdrop. Header totals hidden on phone.
- 2026-11-21: Copy/download/raw toggle + code-block copy (web/src/clipboard.ts). Builds clean; not yet clicked through in a browser.
- 2026-11-21: Named API keys (`openrouter_api_key` removed, no compat). Live: default key OK, switching to a bogus key → 401 reply error, PATCH to an unknown name → 400. Picker not yet clicked in a browser.
- 2026-11-21: Optional HTTP Basic auth (`[auth] user/password`; off + startup warning when absent). Middleware precomputes the header with `base64` (already in tree via reqwest). curl: none/wrong → 401 + WWW-Authenticate, right → 200 for API and static. Not yet tried in a browser (SSE fetch should resend creds same-origin).
- 2026-11-21: `@path` file attachments (no autocomplete yet). Tests for resolve/read; curl: relative/abs/missing/dir/no-root all as expected; parser checked in node. Not clicked through in a browser yet.
- 2026-11-21: `@` autocomplete. Tests for `files::list`; curl `ls` for root/sub/abs/~; parser checked in node. Not clicked through in a browser yet.
- 2026-11-21: Edit can drop attachments (frontend only). Not clicked through in a browser yet.
- 2026-11-21: Extracted MessageInput.svelte; edit now has 📎, `@` mentions and Enter-to-send. Not clicked through in a browser yet.
- 2026-11-21: File tools. Live (Gemini 3.8 Flash, scratch root): list → read → edit → create in 5 rounds, diffs stored; follow-up rebuilt history from rounds; `../` refused and reported to the model; tools-off follow-up used folded history. 25 tests pass. UI typechecks, not seen in a browser.
- 2026-11-21: `--keyfile` replaces `--key`. Cache marker moved to the last user/tool msg; live Haiku 4.5 tool loop: rounds 2–4 each read ~the whole previous prompt from cache (e.g. 10096/10185).
- 2026-11-21: Config parse now warns on unknown keys via `serde_ignored` (e.g. `bind` placed under `[auth]` → `auth.bind`). Test added.
- 2026-11-21: OpenRouter app attribution as client default headers (referer = GitHub URL, title, `hidden`, `general-chat`). `hidden` only applies when the app is first created, so never send the referer without it. Not live-tested yet: check the dashboard no longer shows "Unknown".
- 2026-11-22: Attachment viewer (frontend only, `openText`/`openHtml` in clipboard.ts). Builds; not clicked through in a browser yet.
- 2026-11-22: User messages rendered as markdown via the same snippet as assistant ones. svelte-check clean; not clicked through in a browser yet.
- 2026-11-22: Emacs keys in the message editor (web/src/emacsKeys.ts): C-a (twice = select all), C-e, Alt-←/→, C-w / Alt-Backspace, C-u, Alt-Enter newline. Deletes via execCommand so undo works. svelte-check clean, built; not tried in a browser. C-w likely still closes the tab outside app/PWA mode.
- 2026-11-22: BYOK upstream cost: `Usage.upstream_cost` from `usage.cost_details.upstream_inference_cost` (`cost` is 0 for BYOK). Collapsed line + totals show cost+upstream; ⓘ shows non-zero ones, "Cost $0" if both 0. Parser unit-tested; field verified via curl on gemini-3.1-flash-lite, not clicked through in the UI.
- 2026-11-22: systemd socket activation. 4 parse tests; live-tested with `systemd-socket-activate` (served on passed fd, config bind ignored).
- 2026-11-22: `--bind ADDR` CLI flag overrides config `bind` (socket activation still takes precedence).
- 2026-11-22: Sidebar: per-row actions folded behind a ⋯ toggle (chats + groups); desktop sidebar drag-resizable (160–600px, saved in localStorage, hidden on phones). Builds; not yet clicked through in a browser.
- 2026-11-22: Send conversation id as OpenRouter `session_id`. Unit-tested; not live-tested.
- 2026-11-22: Batch models. Unit tests for body field order + result parsing. Live (Haiku 5.5 :batch): submit OK, refresh → in_progress, second send → 409; after ~8 min ⟳ saved "batch ok" (provider Anthropic, 20/6 tokens) and the upstream delete succeeded. Per-request usage has no cost → filled from batch-level usage (live-verified: 12/4 tokens → $0.0000016, matches batch pricing).
- 2026-11-22: Batch pending row shows provider + expiry (`PendingBatch` flattens `BatchInfo {status, provider, expires_at}` from submit, updated on each refresh). Live: Anthropic + expires_at at submit, status saved on refresh.
- 2026-11-22: Batch auto-poll (server side, doubling gaps capped at `[batch] poll_interval`). Moved batch code from api.rs to src/batch.rs. Tests: gap schedule, duration parsing. Live (poll_interval 1m): startup scan found the pending batch after a restart; auto-poll saved the reply (cost filled), cleared pending, deleted upstream. Checks land up to one 60 s tick late.
- 2026-11-22: Batch poller: fixed 60 s tick replaced by sleep-until-earliest-due + `Notify` wake (no idle wake-ups, checks on time; the tick made checks land up to 60 s late, ~1 tick late every time because the next due was set after the HTTP call). Live (cap 1m, rebuilt binary): idle poller woken by submit, checks at +61 s then every ~61 s (60 s gap + ~1 s HTTP), reply saved- 2026-11-23: Web tools via OpenRouter server tools. Engine tests (scratch scripts, Gemini 3.6 Flash / Haiku 5.5 as judges): fetch `openrouter` = raw HTML, Exa clean; Anthropic native fetch blocked on theguardian.com and stale on Wikipedia; Gemini has no native fetch. Search: Google native ~$0.03–0.046/request (per-query $14/1k, 2–3 queries; results not counted as input), Anthropic native $0.012 + 14k tokens, Exa ~$0.017 with official sources first. Live (chatport, Gemini 3.6 Flash, effort low): date + Rust 1.100.0 with sources, `web_searches` 3 (= max_uses), cost $0.021 search + BYOK upstream; no-tools model → warning, tools not sent. Moved openrouter tests to `src/provider/openrouter/tests.rs`. 50 tests pass.
.
- 2026-11-23: Cost check (gpt-4o-mini, 1 search + 1 fetch): response gives only `cost` and `cost_details.upstream_inference_cost`; tool fee = difference ($0.008), not split. Fetched/search text counts as normal prompt tokens (11k). `/generation` (lags a few s) adds `num_search_results`, `num_fetches`, still one total. Added `{}` raw chunk view (last reply, memory only). Live: 58 deltas skipped, 7 raw chunks (5 annotations, finish_reason, usage). 51 tests pass.
