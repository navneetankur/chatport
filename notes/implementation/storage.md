# storage (`src/store.rs`, `src/tree.rs`)

- `data_dir/conversations/<group>/<id>-<label>/meta.json` + `messages.jsonl`.
  `<group>` = group name or `_ungrouped` (reserved). `<label>` = title made
  folder-safe (`label_for`: `/`→`∕`, control chars → space, 150 bytes max).
- ids: uuid v7, simple form. A chat's folder is found by `<id>-` prefix on
  every op (`Store::find`); moves/renames happen under the write lock, so a move
  during a streaming reply is safe. A read racing a move may fail once.
- `meta.json` rewritten atomically (tmp + rename); a changed title renames the
  folder. `messages.jsonl` append-only, never rewritten. `Store::append` strips
  content of hashed attachments.
- `Store::list` returns `Summary {id, group, title (=label), updated_at (mtime
  of messages.jsonl, else meta.json)}` without reading files.
- groups: `move_to`, `rename_group`, `delete_group`. Groups exist only while
  they have chats; empty group folders are removed. Old flat layout was
  migrated once by a script (not in the code).
- `path_of` / `adopt`: move a chat folder in from another store (llmjob scratch).
- meta: model, provider (routing object), system, cache, reasoning_effort,
  key_name, file_root, tools, web_tools, leaf, pending_batch.
- message: id, parent_id, role, content, reasoning, attachments, tool_rounds,
  model, requested_model, provider_name, usage, partial, error, warning, created_at.

## tree
- siblings = same `parent_id`, order = file order. Edit = new user msg with the
  same parent; regenerate = new assistant msg with the same parent.
- active path: `meta.leaf` (fallback: last appended) walked to the root
  (`tree::path`, `resolve_leaf`). Switching to a sibling sets leaf to its latest
  descendant (`latest_descendant`).
- partial/failed replies saved with `partial: true` (+ `error`).
