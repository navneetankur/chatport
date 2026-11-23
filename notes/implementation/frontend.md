# frontend (`web/`, `src/web.rs`)

- Vite 8 + Svelte 5 + TS 6 (TS 7 breaks svelte-check; keep `~6`). Dev:
  `cd web && npm run dev` (proxies `/api` → 8787). `npm run check` = typecheck.
- `src/web.rs` embeds `web/dist` via rust-embed; SPA fallback to index.html
  (404 for missing `/assets/*`, `/api/*`). URLs are relative (see CLAUDE.md).
- files: `App.svelte` (all views, theme CSS vars, `run()` = all streaming:
  send/edit/regenerate), `api.ts` (fetch + SSE reader), `tree.ts` (`pathTo`,
  `siblings`), `markdown.ts` (marked + DOMPurify), `Settings.svelte`,
  `ModelPicker.svelte` + `modelSearch.ts`, `MessageInput.svelte` (composer and
  inline edit), `mentions.ts`, `clipboard.ts`, `emacsKeys.ts`.
- routing: hash only. `#<id>` chat, `#g/<name>` group view (new chat joins the
  group), empty = landing draft. Sidebar rows are real links; `route()` on hashchange.
- draft chats: "+ New chat" / landing show a draft from `GET conversations/new`,
  kept only in the browser; first send = `POST conversations` with the draft's
  patch + group. No empty "New chat" folders.
- settings (⚙): raw provider JSON is the source of truth, form fields write
  through it. Save/Cancel sticky. Key picker only with >1 key. Effort dropdown;
  PATCH `null` clears.
- model picker: word-wise search (every word matches id or name; exact id, then
  id prefix first; max 100). "Recent" (`state.json`, `src/recent.rs`, 20 max,
  the picked id) then "All models". New chats default to the newest recent.
  List fetched once per page load.
- header: double-click title renames; model ▾ opens an inline picker; shows
  `· <effort> effort` and branch totals (tokens/cost; hidden on phone).
- messages: markdown for user and assistant, `</>` raw toggle, copy/download
  (`<title-slug>-<msg-id>.md`), code-block copy; meta row ‹n/m›, ↻, ✎, short
  model · cost, ⓘ details (model, requested, provider, tokens, cost incl.
  upstream, web searches, time). Collapsible "Thinking".
- `{}` raw chunks of the last reply (memory only, gone on reload).
- composer: "+" menu (Upload file… .txt/.md, 2 MB; Pick existing… picker with
  pin/hide/filter), `@` chips + autocomplete dropdown (↑/↓, Tab, Enter, Esc;
  quoted names). Edit can drop attachments. 📎 chip opens text (and rendered
  .md) in a new tab.
- emacs keys in the editor: C-a (twice = select all), C-e, Alt-←/→, C-w /
  Alt-Backspace, C-u, Alt-Enter newline; deletes via execCommand (undo works).
- sidebar: collapsible groups (localStorage), ⋯ row actions, ⇄ move, rename/
  delete group; drag-resizable 160–600px on desktop; phone (<640px) slide-over via ☰.
- tab title `<chat title> · chatport`, `● ` prefix while streaming.
- clipboard falls back to execCommand on plain-HTTP LAN.
