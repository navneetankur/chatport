# dropped

*Decided against. Reason included.*

- async fs in `Store`: it uses blocking std fs from async handlers. Ops are tiny
  (small meta.json, one-line appends), single user. `tokio::fs` is just
  spawn_blocking per call, gains nothing. If it ever matters: keep `Store` sync,
  wrap handler calls in `tokio::task::spawn_blocking` (std Mutex stays fine,
  never held across `.await`).
- `--key` CLI flag: argv is visible in `ps`; `--keyfile` instead.
- `pinned.json` for attachment pins: pins are log lines now; not migrated.
- webhooks for batches: LAN server, polling instead.
- index.json for the conversation list: folder names + mtimes are enough.
