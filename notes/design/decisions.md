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
- one fetcher: the server's batch poller also fetches llmjob jobs (startup scan
  + `llmjob notify`). Chatport batches need no outside fetcher: the startup scan
  catches them. A CLI fetcher of chatport batches was rejected: the
  save-once lock is in-process only. Notify is a separate command, not
  automatic on `batch`. Server down → `llmjob get` / `get --all` by hand.
