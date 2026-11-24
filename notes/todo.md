# todo

## browser click-through (only curl/typecheck-tested so far)
- settings panel (5), attachments + stop (6)
- file tools UI; web tools UI
- chat groups; draft chat / recent models / header editing
- attachment picker, attachment viewer, "+" menu
- copy/download/raw toggle; emacs keys (C-w may still close the tab outside PWA mode)
- basic auth in a browser (SSE fetch should resend creds same-origin)
- BYOK upstream cost display; tab title; sidebar ⋯ rows + drag-resize
- not live-tested: Stop closing upstream during hidden reasoning;
  `session_id`; app attribution (dashboard should no longer show "Unknown")

## features
- [ ] code blocks: syntax highlighting (highlight.js).
- [ ] web tools: optional per-model-prefix engine override (e.g. `google/` →
  native, pinned to AI Studio), only if BYOK free grounding quota turns out real.
- [ ] web + file tools: server-tool limits reset each tool round; cap total
  `web_searches` across the loop if it ever matters.
- [ ] nice-to-have: per-group system prompt addendum (`<group>/group.json`).
- [ ] nice-to-have: attachment viewer as modal/side panel instead of a new tab.
- [ ] llmjob: Ctrl-C during `run` leaves the user message without a reply; the
  next `-c` stacks a second user message under it. Save a partial reply on
  SIGINT if it bothers.
