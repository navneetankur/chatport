# continue

*Overwritten each session, not appended. Current state + what to do next.*

- 2026-11-23: notes moved from PLAN.md/TRACKER.md into notes/ (old dated
  handoff log dropped; facts folded into implementation/).
- llmjob just landed (see implementation/llmjob.md). Live batch test of a
  2-file + 1-request job (Haiku 5.5 `:batch`) was still running at handoff;
  check `llmjob ls` / `get` behaviour if not confirmed in todo.md.
- Many UI features are only curl-tested, never clicked through in a browser:
  list in todo.md "browser click-through". Doing that pass is the best next step.
