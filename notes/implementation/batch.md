# batch (server: `src/batch.rs`; shared: `src/batch/upstream.rs`; CLI: llmjob.md)

- shared upstream core (`batch::upstream`), used by both sides: `BatchRef
  {id, key_name, key_hash, submitted_at}` (flattened into `PendingBatch` and
  llmjob's `Job`), `submit` (request i = custom_id `r<i>`), `look` (one GET:
  `Submitted` <2 min old and 404 / `Pending {info, raw}` / `Done(raw)` /
  `Gone`), `delete`, `gone_msg`. `look` first compares the key with
  `key_hash` (sha256 of the submit key): mismatch → `WrongKey`, no request.
  Reason: upstream answers 404 for a batch unknown to the key's account, so
  a wrong key looked like "gone". `key_hash` is required. Each
  side keeps what happens to a finished batch.

- model id ending `:batch` → send/regenerate submit a one-request batch
  (`POST /batches`, endpoint `/v1/chat/completions`, `upstream::submit`)
  instead of streaming. SSE: `user_saved`, then `batch {PendingBatch}`, or
  `error` + `done` with an error reply.
- top-level batch fields must precede `requests` (OpenRouter streams the body);
  serde_json key order gives that, a test pins it. `provider` keeps only
  `only`/`allow_fallbacks` (others are a 400; logged as dropped).
- `meta.pending_batch {id, key_name, key_hash, submitted_at, user_id, model,
  status, provider, expires_at}` (flat JSON: `BatchRef` + `BatchInfo` flattened) blocks send/regenerate (409) until refresh saves the
  reply or `forget`. No upstream cancel: forget on a running batch only stops
  waiting; it is still billed.
- refresh: poll once; done → reply appended, leaf set, pending cleared, upstream
  delete best-effort. Fresh batches 404 for a few seconds → `submitted` for 2 min;
  gone after that → saved as error reply. Wrong key → error, pending kept;
  the poller stops polling it (a restart with the right key rescans).
- per-request usage has no cost → filled from batch-level usage (verified live).
- no tools in batch (one round; tool calls → warning).
- auto-poll: `start_poller` (off when `[batch] poll_interval` is 0). In-memory
  `AppState.batch_polls` (conv id → next check), filled by a startup scan and on
  submit. Sleeps until the earliest due check, woken by `batch_wake` (tokio
  `Notify`). Gap = batch age, min 5 min, max `poll_interval` (5, 10, 20, 40 min,
  …), so restarts need no stored schedule. `batch_lock` serialises saving, so ⟳
  racing the poller saves once (loser gets `{status:"done"}`, UI reloads).
- an open page does not update by itself: reload or ⟳.
- body/parse: `build_multi_batch_body`, `parse_batch_all` (`BatchState`);
  `parse_batch` = the reply of a finished one-request batch (None while
  finalizing). `Provider::submit_batch(key, [(custom_id, req)])`.
- llmjob jobs share the poller under keys `llmjob:<short>`: scheduled by the
  startup scan (every job in `data_dir/llmjob/jobs/pending/`, checked at
  once), by an inotify watch on `pending/` (`inotify` crate, tokio stream;
  new record → scheduled unless already), or by `POST api/llmjob/rescan` (for
  a future UI button; adds unscheduled jobs only). Same gaps, by job age. Each
  check is `llmjob::jobs::poll_once` (shared `check()`: cache atomically, delete
  upstream; `WrongKey` → warn, stop polling). CLI and server fetching the same job at once is harmless.
