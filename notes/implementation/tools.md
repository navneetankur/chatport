# tools (`src/tools.rs`, web tools in openrouter.rs)

## file tools
- per chat `meta.tools`, needs `file_root`. `list_dir`, `read_file` (optional
  line range), `write_file` (creates dirs), `edit_file` (exact unique replace).
  No delete. 256 KB cap. Applied directly, no approval.
- jail: relative paths only, no `..`, symlinks checked on the deepest existing
  ancestor. Errors go back to the model as results.
- each write stores a unified diff (diff only). One assistant message holds
  `tool_rounds: [{content, calls, results}]`; SSE `tool_round`. UI: 🔧
  collapsible per call with coloured diff.
- tool calls while tools are off → message `warning`.

## web tools
- per chat `meta.web_tools`, off by default. OpenRouter server tools on
  `/chat/completions` (OpenRouter runs the loop; the stream pauses with
  `: OPENROUTER PROCESSING` comments): `openrouter:web_search` and
  `openrouter:web_fetch` pinned to `engine: "exa"`, plus `openrouter:datetime`.
- limits are consts in `openrouter::web_tools()`: search `max_results 5,
  max_uses 3`; fetch `max_uses 3, max_content_tokens 10000`. Per API request, so
  with file tools they reset every round (accepted).
- sent only when `/models` lists `tools` for the model (unknown ids get them);
  otherwise message `warning`. Never in batch.
- citations come back as inline markdown links; `annotations` ignored.
  `server_tool_use_details.web_search_requests` → `Usage.web_searches`.
- cost: response gives only `cost` + upstream cost; tool fee = difference, not
  split. Fetched/search text counts as prompt tokens. Why Exa: design/decisions.md.
