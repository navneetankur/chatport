# decisions

- separate service from the remotecontrol server (independent restarts/failures).
- OpenRouter only; thin `Provider` trait kept anyway.
- storage: per-conversation folder, `meta.json` (mutable settings incl. leaf)
  + `messages.jsonl` (append-only). Settings change mid-chat; keeping them out
  of the JSONL keeps it strictly append-only. Each assistant message records
  the model it actually used.
- branching = message tree via `parent_id` in one file, not file-per-branch.
- no index file: list = folder names + mtimes.
- attachments text/md only. Content-addressed blobs; message keeps
  `{name, hash, size}`. Picker data = append-only log, not an index; newest line
  per hash is the truth. No automatic deletion: `chatport gc`.
- generation runs in a spawned task → reply saved even if the browser disconnects.
- caching: explicit `cache_control` for `anthropic/*` only; others (incl.
  Gemini 2.5+) cache implicitly.
- app attribution headers on every request, app created hidden (no public rankings).
- batch = `:batch` model suffix, no separate toggle; one pending batch per
  conversation in meta.json.
- `session_id` = conversation id on every request (shared across branches):
  sticky provider routing from turn 1, Sessions grouping in OpenRouter logs.
- new conversations default to `provider.data_collection: "deny"`.
- web tools pinned to Exa (tested 2026-11-23): `openrouter` fetch engine returns
  raw HTML; Anthropic native fetch was blocked on one site and stale on another;
  Gemini has no native fetch. Native search costs ~2x Exa (Google $14/1k per
  query, 2–3 queries per request; Anthropic $10/1k + results as input tokens)
  with redirect URLs (Google) or secondary sources (Anthropic). Exa ~$0.017 per
  request, official sources first.
- llmjob is a second binary in the same crate (lib split), not a separate
  crate/workspace or a script: reuses provider, config, keys, store. Its
  conversations are normal chatport conversations (group `llmjob`).
- llmjob stdin only via explicit `-m -` (implicit read bites in loops/cron).
  `-c` always takes a value so it can't swallow the message text. `last` is an
  id ref file, not a symlink/copy (UI renames/moves would break a symlink).
- one fetcher: the server's batch poller also fetches llmjob jobs. Chatport
  batches need no outside fetcher (startup scan). A CLI fetcher of chatport
  batches was rejected: the save-once lock is in-process only. New CLI jobs
  reach the server by an inotify watch on `jobs/pending/` (`inotify` crate:
  Linux only, thin, tokio stream; `notify` crate's cross-platform layer not
  needed): no idle wake-ups, no HTTP/address/auth in the CLI. Tried before:
  an explicit `llmjob notify` over HTTP (dropped); polling the folder
  (rejected: idle wake-ups). CLI and server share a machine, so inotify sees
  the writes. Server down → `llmjob get` / `get --all` by hand.
- llmjob `--keyfile` wins over profile/stored key names. A `[profile.default]`
  `key` applies with nothing on the command line saying so; an explicit flag
  quietly losing to it is the surprise. One rule ("the flag wins") over
  "wins unless `-p`". `OPENROUTER_API_KEY` stays below key names: nothing on
  the command line sets it.
- llmjob job records carry the submit key's full sha256 (`key_hash`), checked
  before upstream calls. Upstream answers 404 for a batch unknown to the
  key's account, so a wrong key looked like "gone": `rm` deleted the record,
  the poller gave up. A hash over a "sent with --keyfile" flag: also catches a
  changed config key or the wrong file. Full hash: the key is random, so the
  hash reveals nothing usable; truncating would only save bytes.
- one batch upstream core (`batch::upstream`) for chatport and llmjob: record,
  submit, one look, key guard, delete were written twice and the key guard
  would have been a third copy. Split at "what happens to a finished batch"
  (conversation tree under `batch_lock` vs job files). Record fields unified
  (`id`, `submitted_at`); llmjob's old names read via serde aliases.
  `PendingBatch` JSON unchanged (flatten), so the frontend is untouched.
