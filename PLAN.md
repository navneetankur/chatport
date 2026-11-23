# LAN Chat Server — Plan

Self-hosted chat UI for LLMs via OpenRouter. Runs on one machine (started on demand), used from any browser on the LAN. Single user.

## Decisions

| Topic | Decision |
|---|---|
| Backend | Rust, axum, tokio, reqwest (streaming) |
| Frontend | Svelte + TypeScript, Vite; built output embedded in binary (`rust-embed`) |
| Provider | OpenRouter only, behind a thin `Provider` trait |
| Storage | JSONL, one file per conversation, append-only message tree |
| Attachments | Text/markdown only, stored inline in the message |
| Auth | None (LAN only). Bind address configurable. |
| Streaming | SSE from server to browser |
| Separate from remotecontrol server | Yes |

## Layout

```
chatport/
  Cargo.toml
  src/
    main.rs          # config, router, startup
    config.rs
    api.rs           # HTTP handlers
    store.rs         # JSONL read/append, index
    tree.rs          # message tree, active path
    provider/
      mod.rs         # Provider trait, common types
      openrouter.rs  # request building, caching markers, SSE parsing
  web/               # Svelte app (vite build -> web/dist, embedded)
  PLAN.md
```

## Config

`config.toml` (path via `--config`, default `~/.config/chatport/config.toml`):

```toml
bind = "0.0.0.0:8787"
data_dir = "~/.local/share/chatport"
[[keys]]                          # named keys, picked per conversation
name = "personal"                 # --keyfile / OPENROUTER_API_KEY override the default key
key = "..."
default_model = "anthropic/claude-sonnet-4.5"

[default_provider]                # OpenRouter `provider` object, passed through
# order = ["anthropic"]
# allow_fallbacks = false
```

## Data format

```
data_dir/conversations/<conv_id>/
  meta.json         # mutable settings, rewritten atomically (tmp + rename)
  messages.jsonl    # append-only, one message per line
```
No global index: listing scans `*/meta.json` (fine at personal scale).

```jsonc
// meta.json
{"id":"..","title":"..","created_at":..,"updated_at":..,"model":"..",
 "provider":{...}|null,"system":".."|null,"cache":"auto"|"off","leaf":"<msg id>"|null}
// messages.jsonl line
{"id":"..","parent_id":".."|null,"role":"user"|"assistant","content":"..",
 "attachments":[{"name":"notes.md","content":".."}],      // user, optional
 "model":"..","usage":{"prompt_tokens":..,"completion_tokens":..,"cached_tokens":..,"cost":..},
 "partial":true,"error":"..",                                // only when set
 "created_at":..}
```

- **Why two files:** settings change mid-chat (model, system, provider, leaf). Keeping them in `meta.json` lets `messages.jsonl` stay strictly append-only. Each assistant message records the model it actually used.
- **Tree:** siblings = messages with same `parent_id`. Edit = new user msg with same parent as the edited one. Regenerate = new assistant msg with same parent.
- **Active path:** `meta.leaf` (falls back to last appended message) walked up to root.
- **Switching branch:** set `leaf` to the latest descendant of the chosen sibling.
- Partial/failed streams are saved with `"partial": true` (+ `error`).

## Provider abstraction

```rust
#[async_trait]
trait Provider: Send + Sync {
    async fn list_models(&self) -> Result<Vec<ModelInfo>>;
    async fn stream_chat(&self, req: ChatRequest)
        -> Result<BoxStream<'static, Result<ChatEvent>>>;
}

struct ChatRequest { model: String, messages: Vec<Msg>, system: Option<String>,
                     options: ProviderOptions /* opaque JSON per provider */,
                     cache: CachePolicy }
enum ChatEvent { Delta(String), Usage(Usage), Done, Error(String) }
```

Only `OpenRouterProvider` implemented. Attachments are flattened into text in the common layer before reaching the provider:

````
File: notes.md
```markdown
...contents...
```
````

## Caching

- `CachePolicy::Auto` (default) / `Off`.
- Models needing explicit markers (`anthropic/*` only; Gemini 2.5+ caches implicitly): add `cache_control: {type: "ephemeral"}` to the system prompt and to the last user message (max 4 breakpoints for Anthropic; we use ≤2). Content goes in array-of-parts form for these.
- Other models: nothing to do (automatic on provider side).
- Report `cached` tokens from usage in the UI per message.

## Provider settings

OpenRouter `provider` object (order, only, ignore, allow_fallbacks, data_collection, sort) stored per conversation in `meta`, defaulting from config. UI edits it as a small form plus raw-JSON escape hatch. Also passed: `usage: {include: true}` to get cost.

## HTTP API

```
GET    /api/models                         # cached list from OpenRouter
GET    /api/conversations                  # index
POST   /api/conversations                  # {model?, provider?, system?} -> meta
GET    /api/conversations/:id              # meta + all messages + leaf
PATCH  /api/conversations/:id              # title/model/provider/system
DELETE /api/conversations/:id
POST   /api/conversations/:id/messages     # {parent_id, content, attachments} -> SSE
POST   /api/conversations/:id/regenerate   # {user_id} -> SSE
POST   /api/conversations/:id/leaf         # {id}
POST   /api/conversations/:id/stop         # abort in-flight stream
GET    /*                                  # embedded Svelte app
```

SSE events: `user_saved {msg}`, `delta {text}`, `usage {...}`, `done {msg}`, `error {message}`.

Title: after first assistant reply, fire a cheap model call to generate a title (configurable model; off-able).

## Frontend (Svelte)

- Sidebar: conversation list, new chat, delete, rename.
- Chat view: active path, markdown rendering (marked + highlight.js), copy button on code blocks.
- Per message: `‹ 2/3 ›` sibling switcher, edit (user), regenerate (assistant), usage/cost/cached tokens.
- Composer: textarea, attach `.txt`/`.md` (read client-side), send / stop.
- Settings panel per conversation: model picker (searchable), system prompt, provider options, cache on/off.

## RAM / lifecycle

- Idle: small binary, no conversations held in memory except on request.
- Conversations loaded from disk per request (files are small); optional small LRU later if needed.

## Milestones

1. Backend skeleton: config, OpenRouter streaming, curl-able `/messages` with JSONL append.
2. Tree logic: edit/regenerate/leaf, with unit tests.
3. Svelte UI: list, chat, streaming, markdown.
4. Branch switcher, edit/regenerate UI.
5. Settings: model picker, provider options, caching, usage display.
6. Attachments, auto-title, stop button.

## File tools
Per-chat switch `meta.tools` (needs `file_root`). Tools: `list_dir`, `read_file` (opt. line range), `write_file` (creates dirs), `edit_file` (exact unique replace). No delete. Jailed to `file_root` (relative only, no `..`, symlinks checked on the deepest existing ancestor). 256 KB cap. Applied directly, no approval; each write stores a unified diff (diff only, no old content). No MCP.
Storage: the whole loop runs in one generation and is saved as one assistant message with `tool_rounds: [{content, calls, results}]` — tree/branching unchanged. Context expands rounds into `tool_calls` + `tool` messages; with tools off they are folded into the assistant text. Calls while tools are off → message `warning`.

## Web tools
Per-chat switch `meta.web_tools` (off by default). Uses OpenRouter server tools on `/chat/completions` (OpenRouter runs the loop, the stream just pauses with `: OPENROUTER PROCESSING` comments): `openrouter:web_search` and `openrouter:web_fetch`, both pinned to `engine: "exa"`, plus `openrouter:datetime` (free). Limits: search `max_results 5, max_uses 3`; fetch `max_uses 3, max_content_tokens 10000`. Limits are per API request, so with file tools on they reset every tool round (accepted). Only sent when the model's `supported_parameters` has `tools` (unknown ids, e.g. routers, get them anyway); otherwise a message `warning`. Not in batch. Citations come back inline as markdown links; `annotations` are ignored. `usage.server_tool_use_details.web_search_requests` is kept as `Usage.web_searches`.
Why Exa (tested 2026-11-23, see TRACKER): `openrouter` fetch engine returns raw HTML; native fetch (Anthropic) was blocked on one site and stale on another; Gemini has no native fetch. Native search costs ~2x Exa (Google $14/1k per query, several queries per request; Anthropic $10/1k + results as input tokens) with redirect URLs (Google) or secondary sources (Anthropic).

## Later / not now

Images & PDFs (→ `attachments/<hash>` refs), search (SQLite index over JSONL), auth, other providers, multi-user.
