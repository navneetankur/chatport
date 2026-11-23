# provider (`src/provider/mod.rs`, `src/provider/openrouter.rs`)

- trait `Provider`: `list_models`, `stream_chat`, `submit_batch`, `poll_batch`,
  `delete_batch`. Only `OpenRouter`. Provider-specific bits (cache markers,
  `provider` routing object, web tools, batch body) live in openrouter.rs.
- `ChatRequest {key, model, system, messages, options, cache,
  reasoning_effort, tools, web_tools, session_id}`; key per request.
- body: `usage: {include: true}`, `session_id`, `reasoning: {effort}` when set,
  `provider` passed through.
- caching: `anthropic/*` only gets `cache_control: ephemeral` on the system
  prompt and the newest input (last user msg or tool result), so tool loops
  cache per round. Anthropic has a min cacheable size (~1–4k tokens). Verified
  live: Haiku turn 2 read 14675/14697 tokens from cache.
- stream parsing (`parse_chunk`): `ChatEvent::{Model, Provider, Delta,
  Reasoning, Usage, ToolCall, Raw, Done}`. Model/provider deduped in the reader
  task. `is_plain_text` decides what is a `Raw` chunk (anything beyond
  role/content/reasoning/tool_calls deltas, or with finish_reason/usage).
- reader task `select!`s on `tx.closed()`: dropping the receiver (Stop) closes
  the connection at once, even during hidden reasoning.
- usage: tokens, cached, reasoning, `cost`, `upstream_cost`
  (`cost_details.upstream_inference_cost`, BYOK), `web_searches`.
- models list cached 1h (default key). `ModelInfo.tools` from `supported_parameters`.
- app attribution: default headers (referer = GitHub URL, title, `hidden`,
  `general-chat`). `hidden` only applies when the app is first created, so never
  send the referer without it.
- some models reject effort `none` (400, reasoning mandatory): surfaced as reply
  error, deliberately not handled.
- tests in `src/provider/openrouter/tests.rs`.
