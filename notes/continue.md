# continue

*Overwritten each session, not appended. Current state + what to do next.*

- batch upstream core shared by chatport and llmjob (`src/batch/upstream.rs`,
  batch.md): one record (`BatchRef`), submit, look, key guard (`key_hash`).
  llmjob `--keyfile` wins over profile/stored key names (llmjob.md "key").
- Live: submit on both sides and chatport's wrong-key refusal (pending kept)
  verified. Fetch of the finished batches not yet seen at commit time (a
  check script was left running by hand); if it failed, that's the next step.
- Many UI features are only curl-tested, never clicked through in a browser:
  list in todo.md "browser click-through".
