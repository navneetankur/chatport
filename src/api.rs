use crate::AppState;
use crate::provider::{CachePolicy, ChatEvent, ChatMessage, ChatRequest, Role};
use crate::store::{Attachment, Message, Meta, new_id, now_ms};
use crate::tree;
use axum::extract::{Path, State};
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

type St = State<Arc<AppState>>;

/// Cancel handles for in-flight generations, keyed by conversation id.
/// A newer generation in the same conversation replaces the older handle.
pub type Running = Mutex<HashMap<String, (String, oneshot::Sender<()>)>>;

pub struct ApiError(StatusCode, String);
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
fn bad(msg: &str) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}
fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "not found".into())
}
type R<T> = Result<T, ApiError>;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/models", get(models))
        .route("/api/conversations", get(list).post(create))
        .route(
            "/api/conversations/{id}",
            get(get_conv).patch(patch_conv).delete(delete_conv),
        )
        .route("/api/conversations/{id}/messages", post(send))
        .route("/api/conversations/{id}/regenerate", post(regenerate))
        .route("/api/conversations/{id}/leaf", post(set_leaf))
        .route("/api/conversations/{id}/stop", post(stop))
        .with_state(state)
}

async fn models(State(s): St) -> R<Json<Value>> {
    Ok(Json(json!(s.provider.list_models().await?)))
}

async fn list(State(s): St) -> R<Json<Vec<Meta>>> {
    Ok(Json(s.store.list()?))
}

#[derive(Deserialize, Default)]
struct CreateReq {
    title: Option<String>,
    model: Option<String>,
    provider: Option<Value>,
    system: Option<String>,
    cache: Option<CachePolicy>,
}

async fn create(State(s): St, body: Option<Json<CreateReq>>) -> R<Json<Meta>> {
    let r = body.map(|b| b.0).unwrap_or_default();
    let now = now_ms();
    let meta = Meta {
        id: new_id(),
        title: r.title.unwrap_or_else(|| DEFAULT_TITLE.into()),
        created_at: now,
        updated_at: now,
        model: r.model.unwrap_or_else(|| s.config.default_model.clone()),
        provider: r.provider.or_else(|| s.config.default_provider.clone()),
        system: r.system,
        cache: r.cache.unwrap_or_default(),
        leaf: None,
    };
    s.store.put_meta(&meta)?;
    Ok(Json(meta))
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

fn load_meta(s: &AppState, id: &str) -> R<Meta> {
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
#[derive(Deserialize)]
struct PatchReq {
    title: Option<String>,
    model: Option<String>,
    #[serde(default, deserialize_with = "some_value")]
    provider: Option<Value>,
    system: Option<String>,
    cache: Option<CachePolicy>,
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
    meta.updated_at = now_ms();
    s.store.put_meta(&meta)?;
    Ok(Json(meta))
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
    tracing::info!("stop {id}: stopped={stopped}");
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
async fn send(
    State(s): St,
    Path(id): Path<String>,
    Json(r): Json<SendReq>,
) -> R<impl IntoResponse> {
    let mut meta = load_meta(&s, &id)?;
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
    Ok(generate(s, meta, msgs, user, true))
}

#[derive(Deserialize)]
struct RegenReq {
    /// User message to produce a new reply for.
    user_id: String,
}

async fn regenerate(
    State(s): St,
    Path(id): Path<String>,
    Json(r): Json<RegenReq>,
) -> R<impl IntoResponse> {
    let meta = load_meta(&s, &id)?;
    let msgs = s.store.messages(&id)?;
    let user = match msgs.iter().find(|m| m.id == r.user_id) {
        Some(m) if m.role == Role::User => m.clone(),
        Some(_) => return Err(bad("user_id must be a user message")),
        None => return Err(bad("unknown user_id")),
    };
    Ok(generate(s, meta, msgs, user, false))
}

fn ev(name: &str, data: impl Serialize) -> Event {
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
    let req = ChatRequest {
        model: meta.model.clone(),
        system: meta.system.clone(),
        messages: tree::path(&msgs, &user.id)
            .into_iter()
            .filter(|m| !(m.role == Role::Assistant && m.content.is_empty()))
            .map(|m| ChatMessage {
                role: m.role,
                content: flatten(m),
            })
            .collect(),
        options: meta.provider.clone(),
        cache: meta.cache,
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
        let started = tokio::select! {
            r = s.provider.stream_chat(req) => Some(r),
            _ = &mut cancel => None,
        };
        match started {
            None => tracing::info!("generation in {conv_id} stopped before first token"),
            Some(Err(e)) => asst.error = Some(format!("{e:#}")),
            Some(Ok(mut stream)) => loop {
                let item = tokio::select! {
                    i = stream.next() => i,
                    _ = &mut cancel => {
                        tracing::info!("generation in {conv_id} stopped");
                        break;
                    }
                };
                let Some(item) = item else { break };
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
                        let _ = tx.send(ev("usage", &u));
                        asst.usage = Some(u);
                    }
                    Ok(ChatEvent::Done) => {
                        done = true;
                        break;
                    }
                    Err(e) => {
                        asst.error = Some(format!("{e:#}"));
                        break;
                    }
                }
            },
        }
        {
            let mut running = s.running.lock().unwrap();
            if running.get(&conv_id).is_some_and(|(g, _)| *g == gen_id) {
                running.remove(&conv_id);
            }
        }
        asst.partial = !done;
        if let Err(e) = save_reply(&s, &conv_id, &asst) {
            tracing::error!("saving reply in {conv_id}: {e:#}");
        }
        if let Some(err) = &asst.error {
            let _ = tx.send(ev("error", json!({ "message": err })));
        }
        let _ = tx.send(ev("done", &asst));
    });
    Sse::new(UnboundedReceiverStream::new(rx).map(Ok)).keep_alive(KeepAlive::default())
}

/// Record the model that actually answered; keep the requested id only if it differs.
fn record_model(asst: &mut Message, requested: &str, actual: String) {
    asst.requested_model = (actual != requested).then(|| requested.to_string());
    asst.model = Some(actual);
}

fn save_reply(s: &AppState, conv_id: &str, asst: &Message) -> anyhow::Result<()> {
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
}
