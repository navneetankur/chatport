# overview

*A routing table, not an explanation. One line per fact: enough to know the
thing exists and where to look. A bullet that starts explaining itself belongs
in implementation/. If a bullet has a note behind it, name the note and stop.*

*Keep this file under 10,000 characters (`wc -c notes/overview.md`). Bare
note names (storage.md, …) are in implementation/.*

Personal LAN chat server for LLMs via OpenRouter, single user, started on
demand. Rust (axum) backend, Svelte/TS frontend embedded in the binary. Plus
`llmjob`, a CLI in the same crate for one-shot calls and OpenRouter batches.

## start here
- current state and next step: continue.md. open work: todo.md.
- why things are the way they are: design/decisions.md.
- parked: deferred.md. decided against: dropped.md.

## shape
- one crate. `src/lib.rs` = modules + `AppState`; `src/main.rs` = server;
  `src/bin/llmjob.rs` = CLI (logic in `src/llmjob.rs` + `src/llmjob/`).
- one `Provider` trait, OpenRouter only (`src/provider/`). provider.md
- HTTP API + generation loop in `src/api.rs`; SSE over POST. generation.md
- `web/` Svelte app, built to `web/dist`, embedded by `src/web.rs`. frontend.md

## on disk (data_dir)
- `conversations/<group>/<id>-<label>/{meta.json,messages.jsonl}`;
  `_ungrouped` for no group. storage.md
- `attachments/<sha256>` blobs + `attachments.jsonl` log. attachments.md
- `state.json` recent models (`src/recent.rs`). frontend.md
- `llmjob/{scratch/,jobs/,last}` CLI state. llmjob.md

## features
- branching tree: edit / regenerate / switch leaf (`src/tree.rs`). storage.md
- streaming, stop, actual model + provider recorded, reasoning text,
  reasoning effort, usage/cost incl. BYOK upstream cost. generation.md
- prompt caching: `cache_control` markers for `anthropic/*` only. provider.md
- named API keys per conversation. server.md
- text attachments: upload, pick existing, `@path` mentions + autocomplete. attachments.md
- file tools (list/read/write/edit in `file_root`, diffs stored). tools.md
- web tools (OpenRouter server tools: Exa search/fetch, datetime). tools.md
- batch replies for `:batch` models, server auto-poll. batch.md
- groups, draft chats, recent models, rename. frontend.md, storage.md
- optional Basic auth, systemd socket activation. server.md
- `llmjob run/keep/batch/get/ls/rm`, config profiles; server poller also
  fetches llmjob jobs (inotify on `jobs/pending/`). llmjob.md, batch.md

## build / run / test
- `tools/build.sh [--release] [--force]`, `tools/run.sh`; `web/dist` must exist
  for `cargo build`. server.md
- live tests: scratch config + data_dir, `key.txt`, `openrouter/free`. see CLAUDE.md.
- `untracked/` (gitignored): raw SSE captures, read instead of new live calls.
