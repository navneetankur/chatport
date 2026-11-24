# deferred

*Not needed now; revisit if the condition changes.*

- persisted sources list: keep `url`+`title` from `delta.annotations`
  url_citations (drop ~500-char `content`), collapsible "Sources" under the
  reply. Citations come only from search (fetch adds none), all `uncited: true`
  with 0 indices, so no inline markers possible. `{}` raw view covers the last
  reply for now.
- auto-title by a model after the first reply (title comes from the first line instead).
- own `fetch_url` function tool (HTTP GET + a Readability port, e.g.
  `dom_smoothie`): only if Exa fetch gets ~10x pricier than $1/1k pages, or
  LAN/private URLs are needed.
- guard against two concurrent generations in one conversation (allowed today;
  both save fine, last one wins leaf).
- images & PDFs (→ `attachments/<hash>` refs), search (SQLite index over
  JSONL), other providers, multi-user.
- llmjob job marker "sent with --keyfile": `get`/`rm` without it would error
  clearly instead of using the default key, and the server poller would skip
  such jobs. Matters if upstream answers 404 (not 401) for another account's
  batch: today `rm` then treats it as gone and deletes the record, the poller
  stops polling it. Unknown which upstream does; check before building.
