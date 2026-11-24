# continue

*Overwritten each session, not appended. Current state + what to do next.*

- llmjob: `--keyfile` wins over profile/stored key names; job records carry
  `key_hash`, a wrong key is refused before upstream (implementation/llmjob.md
  "key"). CLI paths checked on hand-made records; the server poller's
  `WrongKey` branch only by the unit test (no real batch submitted).
- Many UI features are only curl-tested, never clicked through in a browser:
  list in todo.md "browser click-through". Doing that pass is the best next step.
