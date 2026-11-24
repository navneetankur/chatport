# llmjob (`src/llmjob.rs`, `src/llmjob/{compose,jobs}.rs`, `src/bin/llmjob.rs`)

- same config, keys and data_dir as the server. Hand-rolled arg parsing (no
  clap); options may go before or after the command. `llmjob --help`.
- message = profile `msg` files + `-m` files + TEXT (batch: + the `--dir` file),
  each trimmed at the end, empty ones skipped, joined by a blank line.
  `--headers` / profile `headers` wrap files in `<file name="…">`. System =
  profile `system` + `-s` files, no headers; all empty → no system message.
  `-m -` = stdin (once).
- profiles `[profile.NAME]`: model, effort, key, system, msg, headers, provider.
  `-p NAME`; `default` used when present. Relative paths against the config
  file's folder. `--model` / `--effort` override.
- key: `--keyfile` replaces the default key's value (as in the server) and wins
  over every key name: profile `key` (cleared at resolve, so nothing new stores
  a name) and names stored in conversations/jobs (`Ctx::key`). Without it:
  profile/stored name, else the default key (`OPENROUTER_API_KEY` replaces that
  one's value). A job sent with `--keyfile` stores no name: `get`/`rm` need the
  same `--keyfile`; the server poller uses its own default key.
- job record `key_hash` = sha256 hex of the submit key. Before any upstream
  call (`check`, `rm_one`) the key is compared: mismatch → `WrongKey` error,
  record untouched; the server poller warns once and stops polling the job.
  Cached results need no key. Records without the field take any key.
  OpenRouter: valid key + batch unknown to its account → 404, bad key → 401.
- `run`: streams to stdout; stored as a chatport conversation. Unsaved → one-chat
  scratch store `data_dir/llmjob/scratch/` (replaced by the next unsaved run);
  `--save` / `keep` → `conversations/llmjob/` (Store::adopt). Shown id = last 6
  chars (random part of uuid v7); lookup by prefix or suffix.
- `-c ID|last`: `data_dir/llmjob/last` holds the last run's conversation id.
  Continue keeps the stored system prompt (`-s` is an error), skips profile
  system/msg; explicit `-p`/`--model`/`--effort` update the conversation.
  `--save` with `-c` on a scratch chat keeps it. Batch models refused.
- `batch [--dir DIR]`: one request per regular non-hidden file (sorted), or one
  without `--dir` (named `output`). `:batch` appended to the model if missing.
  Prints a 3-char job id. Record `data_dir/llmjob/jobs/pending/<id>.json`; custom_id sent
  is `r<i>` (Anthropic allows only `[A-Za-z0-9_-]`), file names stay in the record.
- `get ID|last [--dir OUT] [--wait]`: exit 2 + status on stderr while running
  (`--wait` polls every 60 s). Finished: raw batch cached as
  `jobs/done/<id>.results.json`, then the record moves `pending/` → `done/`
  (results first, so a crash in between is repaired on the next listing);
  upstream deleted, so `get` repeats offline. In-progress jobs = list
  `pending/`, fetched ones are never read for it. Without
  `--dir` only one-request jobs (stdout); with it `OUT/<name>`, errors in
  `OUT/<name>.err`. Summary + batch cost on stderr; exit 1 if any failed.
- `get --all`: checks every unfetched job once, caches finished ones (no
  outputs written), updates statuses for `ls`. Exit 2 if any still running, 1
  if a check failed. Manual or cron use, e.g. while the server is down.
- `ls` local jobs (status as of the last check).
- a running chatport server fetches jobs by itself: it watches `pending/` with
  inotify (new jobs scheduled at once) and scans it at startup. batch.md.
- `ls` local jobs (status as of the last check).
- `notify [--server ADDR]`: asks a running chatport server to rescan the jobs
  folder (`POST api/llmjob/rescan`, Basic auth from config) and auto-fetch every
  unfetched job. No ids: there is no reason not to watch all. Address:
  `--server` (host:port or URL), else config `bind` with 0.0.0.0/:: made local.
  Explicit on purpose (not done by `batch`); no server or auto-poll off →
  error. The server's startup scan also picks up every unfetched job. `rm ID…` / `rm --all`: all ids resolved first (a typo
  removes nothing). Fetched → local files removed. Not fetched: running is
  always skipped (no cancel); finished upstream is skipped unless `--force`
  (then deleted upstream); gone upstream → local removed. Summary on stderr,
  exit 1 if anything was skipped or failed.
- live-tested 2026-11-23 with `openrouter/free`: run, `-c last`, `-m -`, keep,
  `--save`, `-c <suffix> --effort low`. Batch (Haiku 5.5): 2-file `--dir`
  job → `out/a.txt`, `out/b.txt` with `--headers` profile, $0.000068; 1-request
  job → stdout. Both ~8 min, `ls` showed `completed`.
