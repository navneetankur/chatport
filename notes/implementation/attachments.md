# attachments (`src/attachments.rs`, `src/files.rs`)

- text/markdown only. Content in `data_dir/attachments/<sha256>`; message
  stores `{name, hash, size}` (+ `path` for `@path` files). Old inline
  attachments (content in the message) still load.
- send: `Attachments::save` hashes content (or loads it for a hash-only pick),
  writes/touches the blob, appends a log line (chat id + title, `@` path).
- load: `api::load_messages` hydrates; a gone blob → `missing: true` + error log;
  send/regenerate on that path → 422 naming the file.
- log `attachments.jsonl`: every line carries full state (`pinned`, `hidden`);
  newest line per hash is the truth. Re-attach copies `pinned` from the previous
  newest line and clears `hidden`. Pin/unpin/hide append a copy with the flag
  changed (keeping `at`; no-op changes append nothing). `find_newest` reads the
  log backwards in 4 KB steps; picker and gc read it all.
- API: `GET attachments` `{pinned, recent}`, `POST|DELETE attachments/{hash}/pin`,
  `POST attachments/{hash}/hide` (hiding unpins).
- `chatport gc [--delete]`: live = hashes in all messages + pins; blobs under 1 h
  skipped and printed; `--delete` removes orphans and compacts the log (newest
  line per kept hash; bytes appended meanwhile are copied over).
- `@path`: `GET file?conv=&path=` resolves `/abs`, `~/x`, else relative to
  `meta.file_root`; text only, 256 KB cap. Found files are sent as ordinary
  attachments named by the typed path. `config.default_file_root` seeds new chats.
- `@` autocomplete: `GET ls?conv=&prefix=` (`files::list`: dirs first,
  dot-entries only if typed, 50 max).
