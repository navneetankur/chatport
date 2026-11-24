# continue

*Overwritten each session, not appended. Current state + what to do next.*

- batch upstream core shared by chatport and llmjob (`src/batch/upstream.rs`,
  batch.md): one record (`BatchRef`), submit, look, key guard (`key_hash`).
  llmjob `--keyfile` wins over profile/stored key names (llmjob.md "key").
- Live-verified (Haiku :batch): submit + server-poller fetch on both sides,
  chatport wrong-key refusal keeps the batch pending.
- Many UI features are only curl-tested, never clicked through in a browser:
  list in todo.md "browser click-through".
