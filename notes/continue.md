# continue

*Overwritten each session, not appended. Current state + what to do next.*

- llmjob `--keyfile` now wins over profile and stored key names
  (implementation/llmjob.md "key"). Not live-tested; unit test on `key_of`.
  Open question parked in deferred.md: what upstream answers for another
  account's batch (404 would make `rm` without `--keyfile` drop the record).
- Many UI features are only curl-tested, never clicked through in a browser:
  list in todo.md "browser click-through". Doing that pass is the best next step.
