use crate::AppState;
use crate::batch;
use crate::files;
use crate::provider::{
    CachePolicy, ChatEvent, ChatMessage, ChatRequest, ReasoningEffort, Role, ToolCall, Usage,
    is_batch_model,
};
use crate::store::{self, Attachment, Message, Meta, ToolRound, new_id, now_ms};
use crate::tools;
use crate::tree;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;
use tokio_stream::wrappers::UnboundedReceiverStream;

pub(crate) type St = State<Arc<AppState>>;

/// Cancel handles for in-flight generations, keyed by conversation id.
/// A newer generation in the same conversation replaces the older handle.
pub type Running = Mutex<HashMap<String, (String, oneshot::Sender<()>)>>;

pub struct ApiError(pub StatusCode, pub String);
impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("{:#}", e.into()))
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}
pub(crate) fn bad(msg: &str) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}
pub(crate) fn conflict(msg: &str) -> ApiError {
    ApiError(StatusCode::CONFLICT, msg.into())
}
pub(crate) fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "not found".into())
}
pub(crate) type R<T> = Result<T, ApiError>;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/models", get(models))
        .route("/api/keys", get(keys))
        .route("/api/recent-models", get(recent_models))
        .route("/api/file", get(file))
        .route("/api/ls", get(ls))
        .route("/api/conversations", get(list).post(create))
        .route("/api/conversations/new", get(template))
        .route(
            "/api/groups/{name}",
            axum::routing::patch(rename_group).delete(delete_group),
        )
        .route(
            "/api/conversations/{id}",
            get(get_conv).patch(patch_conv).delete(delete_conv),
        )
        .route("/api/conversations/{id}/messages", post(send))
        .route("/api/conversations/{id}/regenerate", post(regenerate))
        .route("/api/conversations/{id}/leaf", post(set_leaf))
        .route("/api/conversations/{id}/stop", post(stop))
        .route(
            "/api/conversations/{id}/batch/refresh",
            post(batch::refresh_handler),
        )
        .route(
            "/api/conversations/{id}/batch/forget",
            post(batch::forget_handler),
        )
        .with_state(state)
}

async fn models(State(s): St) -> R<Json<Value>> {
    Ok(Json(json!(s.provider.list_models().await?)))
}

/// Key names only, never the keys themselves.
/// Model ids the user picked, newest first.
async fn recent_models(State(s): St) -> Json<Vec<String>> {
    Json(s.recent.models())
}

/// Records the picked model; a failure only costs the recent list, not the send.
fn note_model(s: &AppState, model: &str) {
    if let Err(e) = s.recent.record(model) {
        log::warn!("recording recent model {model}: {e:#}");
    }
}

async fn keys(State(s): St) -> Json<Value> {
    Json(json!({ "names": s.config.key_names(), "default": s.config.default_key }))
}

/// File root of `conv`, or the config default when no conversation exists yet.
fn file_root(s: &AppState, conv: Option<&str>) -> R<Option<String>> {
    match conv {
        Some(id) => Ok(load_meta(s, id)?.file_root),
        None => Ok(s.config.default_file_root.clone()),
    }
}

#[derive(Deserialize)]
struct FileQuery {
    /// Conversation whose file root applies; absent = config default.
    conv: Option<String>,
    path: String,
}

/// Reads a server-side text file for an `@path` mention in the composer.
async fn file(State(s): St, Query(q): Query<FileQuery>) -> R<Json<Attachment>> {
    let root = file_root(&s, q.conv.as_deref())?;
    let read = files::resolve(root.as_deref(), &q.path).and_then(|p| files::read_text(&p, &q.path));
    match read {
        Ok(a) => {
            log::info!("attached @{} ({} bytes)", q.path, a.content.len());
            Ok(Json(a))
        }
        Err(e) => {
            log::info!("@{}: {e:#}", q.path);
            Err(bad(&format!("{e:#}")))
        }
    }
}

#[derive(Deserialize)]
struct LsQuery {
    conv: Option<String>,
    #[serde(default)]
    prefix: String,
}

/// Autocomplete for `@` mentions: entries matching the typed prefix.
async fn ls(State(s): St, Query(q): Query<LsQuery>) -> R<Json<Vec<files::Entry>>> {
    let root = file_root(&s, q.conv.as_deref())?;
    let entries = files::list(root.as_deref(), &q.prefix).map_err(|e| bad(&format!("{e:#}")))?;
    Ok(Json(entries))
}

async fn list(State(s): St) -> R<Json<Vec<store::Summary>>> {
    Ok(Json(s.store.list()?))
}

#[derive(Deserialize)]
struct GroupRenameReq {
    name: String,
}

async fn rename_group(
    State(s): St,
    Path(name): Path<String>,
    Json(r): Json<GroupRenameReq>,
) -> R<StatusCode> {
    s.store
        .rename_group(&name, r.name.trim())
        .map_err(|e| bad(&format!("{e:#}")))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct GroupDeleteQuery {
    /// true = delete the chats too; false = move them to ungrouped.
    #[serde(default)]
    chats: bool,
}

async fn delete_group(
    State(s): St,
    Path(name): Path<String>,
    Query(q): Query<GroupDeleteQuery>,
) -> R<StatusCode> {
    s.store
        .delete_group(&name, q.chats)
        .map_err(|e| bad(&format!("{e:#}")))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Settings a new chat starts with: last picked model, config defaults.
fn new_meta(s: &AppState, id: String) -> Meta {
    let now = now_ms();
    return Meta {
        id,
        title: DEFAULT_TITLE.into(),
        created_at: now,
        updated_at: now,
        model: s
            .recent
            .latest()
            .unwrap_or_else(|| s.config.default_model.clone()),
        provider: Some(with_data_collection_default(
            s.config.default_provider.clone(),
        )),
        system: None,
        cache: CachePolicy::default(),
        reasoning_effort: None,
        key_name: None,
        file_root: s.config.default_file_root.clone(),
        tools: false,
        leaf: None,
        pending_batch: None,
    };
}

/// Meta for a draft chat (id ""), not saved. The UI keeps it until the
/// first message, then creates the chat with it.
async fn template(State(s): St) -> Json<Meta> {
    Json(new_meta(&s, String::new()))
}

/// Body fields as in PATCH, applied over the defaults.
async fn create(State(s): St, body: Option<Json<PatchReq>>) -> R<Json<Meta>> {
    let r = body.map(|b| b.0).unwrap_or_default();
    let mut meta = new_meta(&s, new_id());
    let group = apply_patch(&s, &mut meta, r)?;
    s.store.put_meta(&meta)?;
    if let Some(g) = group {
        s.store.move_to(&meta.id, Some(&g))?;
    }
    return Ok(Json(meta));
}

/// New conversations opt out of providers that log/train on prompts, unless
/// the request or config sets `data_collection` explicitly.
fn with_data_collection_default(provider: Option<Value>) -> Value {
    let mut p = provider.unwrap_or_else(|| json!({}));
    if let Some(obj) = p.as_object_mut() {
        obj.entry("data_collection").or_insert(json!("deny"));
    }
    return p;
}

const DEFAULT_TITLE: &str = "New chat";
const TITLE_MAX_CHARS: usize = 20;

/// Title from the first message: its first non-empty line, else the first
/// attachment's name, else the conversation id. Truncated with an ellipsis.
fn title_from(first: &Message, conv_id: &str) -> String {
    let base = first
        .content
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .or_else(|| first.attachments.first().map(|a| a.name.as_str()))
        .unwrap_or(conv_id);
    if base.chars().count() <= TITLE_MAX_CHARS {
        return base.to_string();
    }
    let cut: String = base.chars().take(TITLE_MAX_CHARS).collect();
    return format!("{}…", cut.trim_end());
}

pub(crate) fn load_meta(s: &AppState, id: &str) -> R<Meta> {
    s.store
        .meta(id)
        .map_err(|_| not_found())?
        .ok_or_else(not_found)
}

#[derive(Serialize)]
struct ConvResp {
    meta: Meta,
    /// Resolved current tip (meta.leaf if valid, else last message).
    leaf: Option<String>,
    /// All messages in append order; client builds the tree.
    messages: Vec<Message>,
}

async fn get_conv(State(s): St, Path(id): Path<String>) -> R<Json<ConvResp>> {
    let meta = load_meta(&s, &id)?;
    let messages = s.store.messages(&id)?;
    let leaf = tree::resolve_leaf(&messages, meta.leaf.as_deref());
    Ok(Json(ConvResp {
        meta,
        leaf,
        messages,
    }))
}

/// Absent field = unchanged. `provider: null` / `system: ""` clear.
#[derive(Deserialize, Default)]
struct PatchReq {
    title: Option<String>,
    model: Option<String>,
    #[serde(default, deserialize_with = "some_value")]
    provider: Option<Value>,
    system: Option<String>,
    cache: Option<CachePolicy>,
    /// null = back to model default.
    #[serde(default, deserialize_with = "some_option")]
    reasoning_effort: Option<Option<ReasoningEffort>>,
    /// null = config default key.
    #[serde(default, deserialize_with = "some_option")]
    key_name: Option<Option<String>>,
    /// "" clears.
    file_root: Option<String>,
    tools: Option<bool>,
    /// Move to this group; null or "" = ungrouped.
    #[serde(default, deserialize_with = "some_option")]
    group: Option<Option<String>>,
}
fn some_option<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}
fn some_value<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

async fn patch_conv(
    State(s): St,
    Path(id): Path<String>,
    Json(r): Json<PatchReq>,
) -> R<Json<Meta>> {
    let mut meta = load_meta(&s, &id)?;
    let moving = r.group.is_some();
    let group = apply_patch(&s, &mut meta, r)?;
    if moving {
        s.store.move_to(&id, group.as_deref())?;
    }
    meta.updated_at = now_ms();
    s.store.put_meta(&meta)?;
    Ok(Json(meta))
}

/// Applies everything but the group to `meta`, validating first.
/// Returns the checked group name (None = ungrouped or not given).
fn apply_patch(s: &AppState, meta: &mut Meta, r: PatchReq) -> R<Option<String>> {
    let group = r
        .group
        .flatten()
        .map(|g| g.trim().to_string())
        .filter(|g| !g.is_empty());
    if let Some(g) = &group {
        store::check_group(g).map_err(|e| bad(&format!("{e:#}")))?;
    }
    if let Some(t) = r.title {
        meta.title = t;
    }
    if let Some(m) = r.model {
        meta.model = m;
    }
    if let Some(p) = r.provider {
        meta.provider = (!p.is_null()).then_some(p);
    }
    if let Some(sys) = r.system {
        meta.system = (!sys.is_empty()).then_some(sys);
    }
    if let Some(c) = r.cache {
        meta.cache = c;
    }
    if let Some(e) = r.reasoning_effort {
        meta.reasoning_effort = e;
    }
    if let Some(k) = r.key_name {
        if let Some(name) = &k {
            s.config
                .key_for(Some(name))
                .map_err(|e| bad(&format!("{e:#}")))?;
        }
        meta.key_name = k;
    }
    if let Some(r) = r.file_root {
        let r = r.trim();
        meta.file_root = (!r.is_empty()).then(|| r.to_string());
    }
    if let Some(t) = r.tools {
        meta.tools = t;
    }
    return Ok(group);
}

async fn delete_conv(State(s): St, Path(id): Path<String>) -> R<StatusCode> {
    match s.store.delete(&id).map_err(|_| not_found())? {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(not_found()),
    }
}

/// Abort the in-flight generation; the partial reply is saved as usual.
async fn stop(State(s): St, Path(id): Path<String>) -> R<Json<Value>> {
    let entry = s.running.lock().unwrap().remove(&id);
    let stopped = match entry {
        Some((_, tx)) => tx.send(()).is_ok(),
        None => false,
    };
    log::info!("stop {id}: stopped={stopped}");
    Ok(Json(json!({ "stopped": stopped })))
}

#[derive(Deserialize)]
struct LeafReq {
    id: String,
}

/// Switch branch: tip becomes the latest descendant of `id`.
async fn set_leaf(State(s): St, Path(id): Path<String>, Json(r): Json<LeafReq>) -> R<Json<Value>> {
    let mut meta = load_meta(&s, &id)?;
    let msgs = s.store.messages(&id)?;
    if !msgs.iter().any(|m| m.id == r.id) {
        return Err(bad("unknown message id"));
    }
    let leaf = tree::latest_descendant(&msgs, &r.id);
    meta.leaf = Some(leaf.clone());
    s.store.put_meta(&meta)?;
    Ok(Json(json!({ "leaf": leaf })))
}

#[derive(Deserialize)]
struct SendReq {
    /// Assistant message to reply under; null = new root (or edit of first message).
    parent_id: Option<String>,
    content: String,
    #[serde(default)]
    attachments: Vec<Attachment>,
}

/// New user message (also used for edits: same parent as the edited one). Streams reply.
async fn send(State(s): St, Path(id): Path<String>, Json(r): Json<SendReq>) -> R<Response> {
    let mut meta = load_meta(&s, &id)?;
    no_pending_batch(&meta)?;
    let mut msgs = s.store.messages(&id)?;
    if let Some(p) = &r.parent_id {
        match msgs.iter().find(|m| &m.id == p) {
            None => return Err(bad("unknown parent_id")),
            Some(m) if m.role != Role::Assistant => {
                return Err(bad("parent must be an assistant message"));
            }
            _ => {}
        }
    }
    if r.content.trim().is_empty() && r.attachments.is_empty() {
        return Err(bad("empty message"));
    }
    let user = Message::user(r.parent_id, r.content, r.attachments);
    s.store.append(&id, &user)?;
    if msgs.is_empty() && meta.title == DEFAULT_TITLE {
        meta.title = title_from(&user, &id);
    }
    meta.leaf = Some(user.id.clone());
    meta.updated_at = now_ms();
    s.store.put_meta(&meta)?;
    msgs.push(user.clone());
    note_model(&s, &meta.model);
    if is_batch_model(&meta.model) {
        return Ok(batch::submit(s, meta, msgs, user, true).into_response());
    }
    Ok(generate(s, meta, msgs, user, true).into_response())
}

#[derive(Deserialize)]
struct RegenReq {
    /// User message to produce a new reply for.
    user_id: String,
}

async fn regenerate(State(s): St, Path(id): Path<String>, Json(r): Json<RegenReq>) -> R<Response> {
    let meta = load_meta(&s, &id)?;
    no_pending_batch(&meta)?;
    let msgs = s.store.messages(&id)?;
    let user = match msgs.iter().find(|m| m.id == r.user_id) {
        Some(m) if m.role == Role::User => m.clone(),
        Some(_) => return Err(bad("user_id must be a user message")),
        None => return Err(bad("unknown user_id")),
    };
    note_model(&s, &meta.model);
    if is_batch_model(&meta.model) {
        return Ok(batch::submit(s, meta, msgs, user, false).into_response());
    }
    Ok(generate(s, meta, msgs, user, false).into_response())
}

/// One batch at a time per conversation: its reply has a fixed parent.
fn no_pending_batch(meta: &Meta) -> R<()> {
    match &meta.pending_batch {
        Some(b) => Err(conflict(&format!(
            "batch {} is still pending; refresh or forget it first",
            b.id
        ))),
        None => Ok(()),
    }
}

pub(crate) fn ev(name: &str, data: impl Serialize) -> Event {
    Event::default()
        .event(name)
        .json_data(data)
        .unwrap_or_else(|_| Event::default().event(name))
}

/// Fence that can't be closed by backticks inside `content`.
fn fence_for(content: &str) -> String {
    let mut max = 0;
    let mut run = 0;
    for c in content.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        max = max.max(run);
    }
    "`".repeat((max + 1).max(3))
}

pub fn flatten(m: &Message) -> String {
    let mut out = String::new();
    for a in &m.attachments {
        let f = fence_for(&a.content);
        out.push_str(&format!("File: {}\n{f}\n{}\n{f}\n\n", a.name, a.content));
    }
    out.push_str(&m.content);
    out
}

/// Tool rounds beyond this end the reply with an error.
const MAX_TOOL_ROUNDS: usize = 20;

/// Conversation path as provider messages. Tool rounds become assistant
/// `tool_calls` + `tool` messages when tools are offered; otherwise they are
/// folded into the assistant text (providers reject tool messages without tools).
pub(crate) fn context(path: &[&Message], with_tools: bool) -> Vec<ChatMessage> {
    let mut out = Vec::new();
    for m in path {
        match m.role {
            Role::User => out.push(ChatMessage::text(Role::User, flatten(m))),
            Role::Assistant if with_tools => {
                for r in &m.tool_rounds {
                    out.push(ChatMessage {
                        role: Role::Assistant,
                        content: r.content.clone(),
                        tool_calls: r.calls.clone(),
                        tool_call_id: None,
                    });
                    for res in &r.results {
                        out.push(ChatMessage {
                            role: Role::Tool,
                            content: res.output.clone(),
                            tool_calls: vec![],
                            tool_call_id: Some(res.call_id.clone()),
                        });
                    }
                }
                if !m.content.is_empty() {
                    out.push(ChatMessage::text(Role::Assistant, m.content.clone()));
                }
            }
            Role::Assistant => {
                let mut text = String::new();
                for r in &m.tool_rounds {
                    if !r.content.is_empty() {
                        text.push_str(&r.content);
                        text.push_str("\n\n");
                    }
                    for (c, res) in r.calls.iter().zip(&r.results) {
                        text.push_str(&format!(
                            "[tool {}({}) → {}]\n\n",
                            c.name, c.arguments, res.output
                        ));
                    }
                }
                text.push_str(&m.content);
                if !text.is_empty() {
                    out.push(ChatMessage::text(Role::Assistant, text));
                }
            }
            Role::Tool => {}
        }
    }
    return out;
}

/// Adds a streamed tool-call fragment to `calls`.
fn merge_call(
    calls: &mut Vec<ToolCall>,
    index: usize,
    id: Option<String>,
    name: Option<String>,
    args: &str,
) {
    while calls.len() <= index {
        calls.push(ToolCall {
            id: String::new(),
            name: String::new(),
            arguments: String::new(),
        });
    }
    let c = &mut calls[index];
    if let Some(id) = id {
        c.id = id;
    }
    if let Some(n) = name {
        c.name.push_str(&n);
    }
    c.arguments.push_str(args);
}

/// Streams the reply as SSE. Generation runs in a spawned task so the reply is
/// saved even if the browser disconnects.
fn generate(
    s: Arc<AppState>,
    meta: Meta,
    msgs: Vec<Message>,
    user: Message,
    announce_user: bool,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    if announce_user {
        let _ = tx.send(ev("user_saved", &user));
    }
    // A missing key is reported as the reply's error, like any provider failure.
    let key = s
        .config
        .key_for(meta.key_name.as_deref())
        .map(str::to_string);
    // Tools need a root to work in.
    let tool_root = meta
        .file_root
        .clone()
        .filter(|r| meta.tools && !r.trim().is_empty());
    let mut req = ChatRequest {
        key: key.as_ref().cloned().unwrap_or_default(),
        model: meta.model.clone(),
        system: meta.system.clone(),
        messages: context(&tree::path(&msgs, &user.id), tool_root.is_some()),
        options: meta.provider.clone(),
        cache: meta.cache,
        reasoning_effort: meta.reasoning_effort,
        tools: if tool_root.is_some() {
            tools::specs()
        } else {
            vec![]
        },
        session_id: meta.id.clone(),
    };
    let conv_id = meta.id.clone();
    let gen_id = new_id();
    let (cancel_tx, mut cancel) = oneshot::channel::<()>();
    s.running
        .lock()
        .unwrap()
        .insert(conv_id.clone(), (gen_id.clone(), cancel_tx));
    tokio::spawn(async move {
        let mut asst = Message::assistant(user.id.clone(), meta.model.clone());
        let mut done = false;
        let mut total: Option<Usage> = None;
        // Each round is one model request; it continues while the model calls tools.
        'rounds: for round in 0.. {
            if let Err(e) = &key {
                asst.error = Some(format!("{e:#}"));
                break;
            }
            let started = tokio::select! {
                r = s.provider.stream_chat(req.clone()) => r,
                _ = &mut cancel => {
                    log::info!("generation in {conv_id} stopped before first token");
                    break;
                }
            };
            let mut stream = match started {
                Ok(st) => st,
                Err(e) => {
                    asst.error = Some(format!("{e:#}"));
                    break;
                }
            };
            let mut calls: Vec<ToolCall> = Vec::new();
            loop {
                let item = tokio::select! {
                    i = stream.next() => i,
                    _ = &mut cancel => {
                        log::info!("generation in {conv_id} stopped");
                        break 'rounds;
                    }
                };
                let Some(item) = item else { break 'rounds };
                match item {
                    Ok(ChatEvent::Model(m)) => {
                        record_model(&mut asst, &meta.model, m);
                        let _ = tx.send(ev("model", json!({ "model": &asst.model })));
                    }
                    Ok(ChatEvent::Provider(p)) => {
                        let _ = tx.send(ev("provider", json!({ "provider": &p })));
                        asst.provider_name = Some(p);
                    }
                    Ok(ChatEvent::Reasoning(t)) => {
                        asst.reasoning.push_str(&t);
                        let _ = tx.send(ev("reasoning", json!({ "text": t })));
                    }
                    Ok(ChatEvent::Delta(t)) => {
                        asst.content.push_str(&t);
                        let _ = tx.send(ev("delta", json!({ "text": t })));
                    }
                    Ok(ChatEvent::Usage(u)) => {
                        let t = total.get_or_insert_with(Usage::default);
                        t.add(&u);
                        let _ = tx.send(ev("usage", &*t));
                        asst.usage = Some(t.clone());
                    }
                    Ok(ChatEvent::ToolCall {
                        index,
                        id,
                        name,
                        arguments,
                    }) => merge_call(&mut calls, index, id, name, &arguments),
                    Ok(ChatEvent::Done) => break,
                    Err(e) => {
                        asst.error = Some(format!("{e:#}"));
                        break 'rounds;
                    }
                }
            }
            if calls.is_empty() {
                done = true;
                break;
            }
            let Some(root) = &tool_root else {
                let names: Vec<&str> = calls.iter().map(|c| c.name.as_str()).collect();
                let w = format!(
                    "model tried to call {}, but tools are off for this chat",
                    names.join(", ")
                );
                log::warn!("{conv_id}: {w}");
                asst.warning = Some(w);
                done = true;
                break;
            };
            if round >= MAX_TOOL_ROUNDS {
                asst.error = Some(format!("stopped after {MAX_TOOL_ROUNDS} tool rounds"));
                break;
            }
            for (i, c) in calls.iter_mut().enumerate() {
                if c.id.is_empty() {
                    c.id = format!("call_{round}_{i}");
                }
            }
            let results: Vec<_> = calls.iter().map(|c| tools::run(root, c)).collect();
            let r = ToolRound {
                content: std::mem::take(&mut asst.content),
                calls,
                results,
            };
            let _ = tx.send(ev("tool_round", &r));
            req.messages.extend(context(
                &[&Message {
                    tool_rounds: vec![r.clone()],
                    ..Message::assistant(String::new(), String::new())
                }],
                true,
            ));
            asst.tool_rounds.push(r);
        }
        {
            let mut running = s.running.lock().unwrap();
            if running.get(&conv_id).is_some_and(|(g, _)| *g == gen_id) {
                running.remove(&conv_id);
            }
        }
        asst.partial = !done;
        if let Err(e) = save_reply(&s, &conv_id, &asst) {
            log::error!("saving reply in {conv_id}: {e:#}");
        }
        if let Some(err) = &asst.error {
            let _ = tx.send(ev("error", json!({ "message": err })));
        }
        let _ = tx.send(ev("done", &asst));
    });
    Sse::new(UnboundedReceiverStream::new(rx).map(Ok)).keep_alive(KeepAlive::default())
}

/// Record the model that actually answered; keep the requested id only if it differs.
pub(crate) fn record_model(asst: &mut Message, requested: &str, actual: String) {
    asst.requested_model = (actual != requested).then(|| requested.to_string());
    asst.model = Some(actual);
}

pub(crate) fn save_reply(s: &AppState, conv_id: &str, asst: &Message) -> anyhow::Result<()> {
    s.store.append(conv_id, asst)?;
    // Re-read meta: settings may have been patched while streaming.
    if let Some(mut meta) = s.store.meta(conv_id)? {
        meta.leaf = Some(asst.id.clone());
        meta.updated_at = now_ms();
        s.store.put_meta(&meta)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_collection_defaults_to_deny() {
        assert_eq!(
            with_data_collection_default(None),
            json!({ "data_collection": "deny" })
        );
        assert_eq!(
            with_data_collection_default(Some(json!({ "order": ["a"] }))),
            json!({ "order": ["a"], "data_collection": "deny" })
        );
        assert_eq!(
            with_data_collection_default(Some(json!({ "data_collection": "allow" }))),
            json!({ "data_collection": "allow" })
        );
    }

    #[test]
    fn title_from_first_message() {
        let m = |c: &str, atts: Vec<Attachment>| Message::user(None, c.into(), atts);
        assert_eq!(
            title_from(&m("\n  hi there \nmore", vec![]), "cid"),
            "hi there"
        );
        assert_eq!(
            title_from(&m("ääääääääääääääääääääääää", vec![]), "cid"),
            "ää".repeat(10) + "…"
        );
        let att = Attachment {
            name: "notes.md".into(),
            content: "x".into(),
        };
        assert_eq!(title_from(&m("  ", vec![att]), "cid"), "notes.md");
        assert_eq!(title_from(&m("", vec![]), "cid"), "cid");
    }

    #[test]
    fn record_model_keeps_requested_only_when_different() {
        let mut a = Message::assistant("u".into(), "openrouter/free".into());
        record_model(&mut a, "openrouter/free", "x/llama".into());
        assert_eq!(a.model.as_deref(), Some("x/llama"));
        assert_eq!(a.requested_model.as_deref(), Some("openrouter/free"));
        record_model(&mut a, "openrouter/free", "openrouter/free".into());
        assert_eq!(a.requested_model, None);
    }

    #[test]
    fn flatten_uses_safe_fence() {
        let m = Message::user(
            None,
            "explain".into(),
            vec![Attachment {
                name: "a.md".into(),
                content: "x ```` y".into(),
            }],
        );
        let f = flatten(&m);
        assert!(f.starts_with("File: a.md\n`````\n"));
        assert!(f.ends_with("`````\n\nexplain"));
    }

    #[test]
    fn context_expands_or_folds_tool_rounds() {
        let user = Message::user(None, "hi".into(), vec![]);
        let mut asst = Message::assistant(user.id.clone(), "m".into());
        asst.content = "done".into();
        asst.tool_rounds = vec![ToolRound {
            content: "looking".into(),
            calls: vec![ToolCall {
                id: "t1".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"a"}"#.into(),
            }],
            results: vec![tools::ToolResult {
                call_id: "t1".into(),
                output: "AAA".into(),
                error: false,
                diff: None,
            }],
        }];
        let with = context(&[&user, &asst], true);
        let roles: Vec<Role> = with.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            [Role::User, Role::Assistant, Role::Tool, Role::Assistant]
        );
        assert_eq!(with[1].tool_calls[0].id, "t1");
        assert_eq!(with[2].tool_call_id.as_deref(), Some("t1"));
        assert_eq!(with[3].content, "done");

        let without = context(&[&user, &asst], false);
        assert_eq!(without.len(), 2);
        let t = &without[1].content;
        assert!(
            t.starts_with("looking") && t.contains("AAA") && t.ends_with("done"),
            "{t}"
        );
        assert!(without.iter().all(|m| m.tool_calls.is_empty()));
    }
}
