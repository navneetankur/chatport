# generation + HTTP API (`src/api.rs`)

## API (all under `api/`, relative URLs)
- `GET models`, `GET recent-models`, `GET keys` (names only)
- `GET conversations` (Summary list), `GET conversations/new` (draft meta),
  `POST conversations` (takes PATCH fields + group), `GET|PATCH|DELETE conversations/:id`
- `POST conversations/:id/messages` / `regenerate` → SSE; `POST …/leaf`, `POST …/stop`
- `POST …/batch/refresh`, `POST …/batch/forget` (batch.md)
- `PATCH|DELETE groups/:name`; `GET file`, `GET ls` (attachments.md)
- `GET|POST|DELETE attachments…` (attachments.md)

## SSE
- POST endpoints, so the client uses `fetch` + stream reader, not EventSource.
- events: `user_saved`, `model`, `provider`, `delta {text}`, `reasoning {text}`,
  `tool_round`, `usage` (running total), `raw`, `batch`, `error {message}`,
  `done {full assistant msg}`.

## generate
- runs in a spawned task; reply saved even if the browser goes away.
- loops tool rounds (max 20) while the model returns tool calls; one assistant
  message holds `tool_rounds` (tools.md). `api::context` builds provider messages
  (rounds expanded with tools on, folded into text with tools off).
- attachments flattened into the prompt (`api::flatten`, safe code fence).
- actual model: `record_model` sets `model`, keeps `requested_model` only if it
  differs (routers like `openrouter/free`).
- reasoning stored on the message, never sent back as context.
- stop: `AppState.running` = conv id → (gen id, oneshot). Generation `select!`s
  on it; stopped reply saved `partial` without error. Newer generation replaces
  the handle; finisher removes only its own.
- title: set on the first message of a "New chat" (`title_from`: first non-empty
  line → attachment name → conv id, 20 chars + …).
- new conversations: `provider.data_collection: "deny"` unless set.
