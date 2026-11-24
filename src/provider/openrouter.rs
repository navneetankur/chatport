use super::*;
use anyhow::{Context, anyhow, bail};
use futures::StreamExt;
use serde_json::json;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio_stream::wrappers::UnboundedReceiverStream;

const BASE: &str = "https://openrouter.ai/api/v1";
const MODELS_TTL: Duration = Duration::from_secs(3600);

/// App attribution, sent on every request. `hidden` only takes effect on the
/// request that first creates the app on OpenRouter, so it must always be sent
/// together with the referer. Names are lowercase as `from_static` requires.
const ATTRIBUTION_HEADERS: [(&str, &str); 4] = [
    ("http-referer", "https://github.com/navneetankur/chatport"),
    ("x-openrouter-title", "chatport"),
    ("x-openrouter-app-visibility", "hidden"),
    ("x-openrouter-categories", "general-chat"),
];

fn attribution_headers() -> reqwest::header::HeaderMap {
    return ATTRIBUTION_HEADERS
        .iter()
        .map(|(k, v)| {
            (
                reqwest::header::HeaderName::from_static(k),
                reqwest::header::HeaderValue::from_static(v),
            )
        })
        .collect();
}

pub struct OpenRouter {
    /// Used for model listing only; chat requests carry their own key.
    key: String,
    http: reqwest::Client,
    models: Mutex<Option<(Instant, Vec<ModelInfo>)>>,
}

impl OpenRouter {
    /// `POST /batches` with a prebuilt body (see `build_multi_batch_body`).
    async fn submit_batch_body(&self, key: &str, body: &Value) -> Result<(String, BatchInfo)> {
        let resp = self
            .http
            .post(format!("{BASE}/batches"))
            .bearer_auth(key)
            .json(body)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(fail(resp).await);
        }
        let v: Value = resp.json().await?;
        let id = v["id"]
            .as_str()
            .ok_or_else(|| anyhow!("batch submit: no id in response"))?;
        let info = batch_info(&v, v["status"].as_str().unwrap_or("validating"));
        log::info!("batch {id} submitted: {info:?}");
        return Ok((id.to_string(), info));
    }

    pub fn new(key: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .default_headers(attribution_headers())
            .build()
            .context("building HTTP client")?;
        return Ok(Self {
            key,
            http,
            models: Mutex::new(None),
        });
    }
}

/// Models that need explicit `cache_control` breakpoints. Others (incl. Gemini 2.5+) cache implicitly.
fn needs_cache_markers(model: &str) -> bool {
    model.starts_with("anthropic/")
}

fn cached_text(text: &str) -> Value {
    json!([{ "type": "text", "text": text, "cache_control": { "type": "ephemeral" } }])
}

pub fn build_body(req: &ChatRequest) -> Value {
    let markers = req.cache == CachePolicy::Auto && needs_cache_markers(&req.model);
    let mut messages = Vec::new();
    if let Some(s) = req.system.as_deref().filter(|s| !s.is_empty()) {
        let content = if markers { cached_text(s) } else { json!(s) };
        messages.push(json!({ "role": "system", "content": content }));
    }
    // Breakpoint on the newest input: the user message, or a tool result mid-loop.
    let last_input = req
        .messages
        .iter()
        .rposition(|m| matches!(m.role, Role::User | Role::Tool));
    for (i, m) in req.messages.iter().enumerate() {
        let content = if markers && Some(i) == last_input {
            cached_text(&m.content)
        } else if m.content.is_empty() && !m.tool_calls.is_empty() {
            Value::Null
        } else {
            json!(m.content)
        };
        let mut msg = json!({ "role": m.role, "content": content });
        if !m.tool_calls.is_empty() {
            msg["tool_calls"] = m
                .tool_calls
                .iter()
                .map(|c| {
                    json!({
                        "id": c.id,
                        "type": "function",
                        "function": { "name": c.name, "arguments": c.arguments },
                    })
                })
                .collect();
        }
        if let Some(id) = &m.tool_call_id {
            msg["tool_call_id"] = json!(id);
        }
        messages.push(msg);
    }
    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "stream": true,
        "usage": { "include": true },
        // OpenRouter caps it at 256 chars.
        "session_id": req.session_id.chars().take(256).collect::<String>(),
    });
    if let Some(p) = req.options.as_ref().filter(|p| !p.is_null()) {
        body["provider"] = p.clone();
    }
    if let Some(e) = req.reasoning_effort {
        body["reasoning"] = json!({ "effort": e });
    }
    let mut tools: Vec<Value> = req
        .tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                },
            })
        })
        .collect();
    if req.web_tools {
        tools.extend(web_tools());
    }
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }
    body
}

/// OpenRouter server tools, run on their side within the request. Exa for both:
/// the `openrouter` fetch engine returns raw HTML, native search costs ~2x
/// (see PLAN "Web tools"). Limits are per request.
fn web_tools() -> [Value; 3] {
    return [
        json!({ "type": "openrouter:web_search",
                "parameters": { "engine": "exa", "max_results": 5, "max_uses": 3 } }),
        json!({ "type": "openrouter:web_fetch",
                "parameters": { "engine": "exa", "max_uses": 3, "max_content_tokens": 10000 } }),
        json!({ "type": "openrouter:datetime" }),
    ];
}

/// Batch keeps only these `provider` keys; any other routing preference is a 400.
const BATCH_PROVIDER_KEYS: [&str; 2] = ["only", "allow_fallbacks"];

/// `POST /batches` body with one request per `(custom_id, request)`. All
/// requests must use the same model; top-level `provider` comes from the
/// first. The top-level fields must precede `requests` (OpenRouter streams
/// the body); serde_json sorts keys, and endpoint < model < provider <
/// requests, which a test pins down.
pub fn build_multi_batch_body(reqs: &[(String, ChatRequest)]) -> Result<Value> {
    let (_, first) = reqs.first().context("batch needs at least one request")?;
    let mut requests = Vec::with_capacity(reqs.len());
    let mut provider = None;
    for (i, (custom_id, req)) in reqs.iter().enumerate() {
        if req.model != first.model {
            bail!("batch mixes models {:?} and {:?}", first.model, req.model);
        }
        let mut body = build_body(req);
        let obj = body.as_object_mut().expect("body is an object");
        obj.remove("stream");
        obj.remove("model");
        let p = obj.remove("provider");
        if i == 0 {
            provider = p.and_then(|p| batch_provider(&p));
        }
        requests.push(json!({ "custom_id": custom_id, "body": body }));
    }
    let mut batch = json!({
        "endpoint": "/v1/chat/completions",
        "model": first.model,
        "requests": requests,
    });
    if let Some(p) = provider {
        batch["provider"] = p;
    }
    return Ok(batch);
}

/// The `provider` object reduced to what batch accepts; None when nothing is left.
fn batch_provider(p: &Value) -> Option<Value> {
    let p = p.as_object()?;
    let dropped: Vec<&String> = p
        .keys()
        .filter(|k| !BATCH_PROVIDER_KEYS.contains(&k.as_str()))
        .collect();
    if !dropped.is_empty() {
        log::info!("batch: dropping provider options {dropped:?} (not supported)");
    }
    let kept: serde_json::Map<String, Value> = p
        .iter()
        .filter(|(k, _)| BATCH_PROVIDER_KEYS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    return (!kept.is_empty()).then_some(Value::Object(kept));
}

fn usage_from(u: &Value) -> Usage {
    let n = |p: &str| u.pointer(p).and_then(Value::as_u64).unwrap_or(0);
    return Usage {
        prompt_tokens: n("/prompt_tokens"),
        completion_tokens: n("/completion_tokens"),
        cached_tokens: n("/prompt_tokens_details/cached_tokens"),
        reasoning_tokens: n("/completion_tokens_details/reasoning_tokens"),
        cost: u.get("cost").and_then(Value::as_f64),
        upstream_cost: u
            .pointer("/cost_details/upstream_inference_cost")
            .and_then(Value::as_f64),
        web_searches: n("/server_tool_use_details/web_search_requests"),
    };
}

/// Per-request usage lacks `cost`; the batch-level one has it. With one
/// request per batch they are the same request, so fill the gap from it.
fn merge_batch_cost(req: Option<Usage>, batch: Option<Usage>) -> Option<Usage> {
    let Some(mut u) = req else { return batch };
    if let Some(b) = batch {
        u.cost = u.cost.or(b.cost);
        u.upstream_cost = u.upstream_cost.or(b.upstream_cost);
    }
    return Some(u);
}

fn batch_info(v: &Value, status: &str) -> BatchInfo {
    return BatchInfo {
        status: status.to_string(),
        provider: v["provider"]
            .as_str()
            .filter(|p| !p.is_empty())
            .map(str::to_string),
        expires_at: v["expires_at"].as_u64(),
    };
}

/// Where a multi-request batch stands.
#[derive(Debug)]
pub enum BatchState {
    Pending(BatchInfo),
    /// The whole batch failed, expired or was cancelled.
    Failed(String),
    /// Finished. One reply per request, in upstream order (match on custom_id).
    /// `usage` is the batch total, the only place `cost` is reported.
    Done {
        replies: Vec<(String, BatchReply)>,
        usage: Option<Usage>,
    },
}

/// Interprets `GET /batches/{id}` for any number of requests.
pub fn parse_batch_all(v: &Value) -> BatchState {
    let status = v["status"].as_str().unwrap_or("unknown");
    match status {
        "completed" => {}
        "failed" | "expired" | "cancelled" => {
            return BatchState::Failed(format!(
                "batch {status}: {}",
                err_msg(v).unwrap_or_else(|| "no details".into())
            ));
        }
        s => return BatchState::Pending(batch_info(v, s)),
    }
    let Some(results) = v["results"].as_array() else {
        // Completed, but results not attached yet (docs: can lag briefly).
        return BatchState::Pending(batch_info(v, "finalizing"));
    };
    let replies = results
        .iter()
        .map(|r| {
            let id = r["custom_id"].as_str().unwrap_or_default().to_string();
            (id, parse_result(v, r))
        })
        .collect();
    let usage = v.get("usage").filter(|u| !u.is_null()).map(usage_from);
    return BatchState::Done { replies, usage };
}

fn err_msg(v: &Value) -> Option<String> {
    v.pointer("/error/message")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// One entry of `results`. `batch` is the whole batch (fallback provider).
fn parse_result(batch: &Value, result: &Value) -> BatchReply {
    if !result["error"].is_null() {
        return BatchReply {
            error: Some(err_msg(result).unwrap_or_else(|| result["error"].to_string())),
            ..Default::default()
        };
    }
    let body = &result["response"]["body"];
    let s = |p: &str| body.pointer(p).and_then(Value::as_str).map(str::to_string);
    let tool_calls = body
        .pointer("/choices/0/message/tool_calls")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|c| c.pointer("/function/name")?.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    return BatchReply {
        content: s("/choices/0/message/content").unwrap_or_default(),
        reasoning: s("/choices/0/message/reasoning").unwrap_or_default(),
        model: s("/model").filter(|m| !m.is_empty()),
        provider: s("/provider")
            .or_else(|| batch["provider"].as_str().map(str::to_string))
            .filter(|p| !p.is_empty()),
        usage: body.get("usage").filter(|u| !u.is_null()).map(usage_from),
        error: None,
        tool_calls,
    };
}

/// The reply of a one-request batch, from `GET /batches/{id}`. None while it
/// is still running or finalizing. A failed/expired batch → reply with error.
pub fn parse_batch(v: &Value) -> Option<BatchReply> {
    match parse_batch_all(v) {
        BatchState::Pending(_) => return None,
        BatchState::Failed(e) => {
            return Some(BatchReply {
                error: Some(e),
                ..Default::default()
            });
        }
        BatchState::Done { replies, usage } => {
            let (_, mut r) = replies.into_iter().next()?;
            // An errored request has no usage of its own: show the batch's.
            r.usage = match r.error {
                Some(_) => usage,
                None => merge_batch_cost(r.usage, usage),
            };
            return Some(r);
        }
    }
}

/// Error body of a failed OpenRouter call as an `anyhow` error.
async fn fail(resp: reqwest::Response) -> anyhow::Error {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    return anyhow!("OpenRouter {status}: {text}");
}

/// Delta keys that only carry streamed text (tool-call arguments stream too;
/// the UI shows the calls already).
const TEXT_DELTA_KEYS: [&str; 5] = [
    "role",
    "content",
    "reasoning",
    "reasoning_details",
    "tool_calls",
];

/// A chunk that only streams text: no usage, no finish_reason, and a delta
/// with nothing but text keys. Everything else is kept for the raw view.
fn is_plain_text(v: &Value) -> bool {
    if v.get("usage").is_some_and(|u| !u.is_null()) {
        return false;
    }
    let Some(choices) = v.get("choices").and_then(Value::as_array) else {
        return false;
    };
    return choices.iter().all(|c| {
        c.get("finish_reason").is_none_or(Value::is_null)
            && c.get("delta")
                .and_then(Value::as_object)
                .is_some_and(|d| d.keys().all(|k| TEXT_DELTA_KEYS.contains(&k.as_str())))
    });
}

/// Parse one SSE `data:` payload. Returns events to emit.
fn parse_chunk(data: &str) -> Result<Vec<ChatEvent>> {
    if data == "[DONE]" {
        return Ok(vec![ChatEvent::Done]);
    }
    let v: Value = serde_json::from_str(data).context("bad SSE json")?;
    if let Some(err) = v.get("error") {
        bail!(
            "provider error: {}",
            err.get("message")
                .and_then(Value::as_str)
                .unwrap_or(&err.to_string())
        );
    }
    let mut out = Vec::new();
    if let Some(m) = v.get("model").and_then(Value::as_str)
        && !m.is_empty()
    {
        out.push(ChatEvent::Model(m.to_string()));
    }
    if let Some(p) = v.get("provider").and_then(Value::as_str)
        && !p.is_empty()
    {
        out.push(ChatEvent::Provider(p.to_string()));
    }
    if let Some(t) = v
        .pointer("/choices/0/delta/reasoning")
        .and_then(Value::as_str)
        && !t.is_empty()
    {
        out.push(ChatEvent::Reasoning(t.to_string()));
    }
    if let Some(t) = v
        .pointer("/choices/0/delta/content")
        .and_then(Value::as_str)
        && !t.is_empty()
    {
        out.push(ChatEvent::Delta(t.to_string()));
    }
    if let Some(calls) = v
        .pointer("/choices/0/delta/tool_calls")
        .and_then(Value::as_array)
    {
        for (pos, c) in calls.iter().enumerate() {
            let s = |p: &str| c.pointer(p).and_then(Value::as_str).map(str::to_string);
            out.push(ChatEvent::ToolCall {
                index: c
                    .get("index")
                    .and_then(Value::as_u64)
                    .map_or(pos, |i| i as usize),
                id: s("/id").filter(|x| !x.is_empty()),
                name: s("/function/name").filter(|x| !x.is_empty()),
                arguments: s("/function/arguments").unwrap_or_default(),
            });
        }
    }
    if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
        out.push(ChatEvent::Usage(usage_from(u)));
    }
    if !is_plain_text(&v) {
        out.push(ChatEvent::Raw(v));
    }
    Ok(out)
}

#[async_trait]
impl Provider for OpenRouter {
    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        if let Some((at, m)) = &*self.models.lock().unwrap()
            && at.elapsed() < MODELS_TTL
        {
            return Ok(m.clone());
        }
        let v: Value = self
            .http
            .get(format!("{BASE}/models"))
            .bearer_auth(&self.key)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let models: Vec<ModelInfo> = v["data"]
            .as_array()
            .ok_or_else(|| anyhow!("unexpected /models response"))?
            .iter()
            .filter_map(|m| {
                Some(ModelInfo {
                    id: m["id"].as_str()?.to_string(),
                    name: m["name"].as_str().unwrap_or_default().to_string(),
                    context_length: m["context_length"].as_u64(),
                    pricing: m["pricing"].clone(),
                    tools: m["supported_parameters"]
                        .as_array()
                        .is_some_and(|p| p.iter().any(|x| x == "tools")),
                })
            })
            .collect();
        *self.models.lock().unwrap() = Some((Instant::now(), models.clone()));
        Ok(models)
    }

    async fn stream_chat(&self, req: ChatRequest) -> Result<BoxStream<'static, Result<ChatEvent>>> {
        let resp = self
            .http
            .post(format!("{BASE}/chat/completions"))
            .bearer_auth(&req.key)
            .json(&build_body(&req))
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("OpenRouter {status}: {text}");
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut bytes = resp.bytes_stream();
            let mut buf: Vec<u8> = Vec::new();
            let mut last_model: Option<String> = None;
            let mut last_provider: Option<String> = None;
            loop {
                // Stop as soon as the receiver is dropped (user pressed Stop), even
                // while nothing is streaming (e.g. hidden reasoning): dropping `resp`
                // closes the connection, which is what cancels upstream billing.
                let chunk = tokio::select! {
                    c = bytes.next() => c,
                    _ = tx.closed() => {
                        log::debug!("receiver dropped; aborting upstream stream");
                        return;
                    }
                };
                let Some(chunk) = chunk else { break };
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx.send(Err(e.into()));
                        return;
                    }
                };
                buf.extend_from_slice(&chunk);
                while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=pos).collect();
                    let line = String::from_utf8_lossy(&line);
                    let Some(data) = line.trim_end().strip_prefix("data:") else {
                        continue;
                    };
                    match parse_chunk(data.trim_start()) {
                        Ok(evs) => {
                            for ev in evs {
                                // Every chunk repeats model/provider; forward them only when they change.
                                let last = match &ev {
                                    ChatEvent::Model(_) => Some(&mut last_model),
                                    ChatEvent::Provider(_) => Some(&mut last_provider),
                                    _ => None,
                                };
                                if let Some(last) = last {
                                    let (ChatEvent::Model(v) | ChatEvent::Provider(v)) = &ev else {
                                        unreachable!()
                                    };
                                    if last.as_ref() == Some(v) {
                                        continue;
                                    }
                                    *last = Some(v.clone());
                                }
                                let done = matches!(ev, ChatEvent::Done);
                                if tx.send(Ok(ev)).is_err() || done {
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Err(e));
                            return;
                        }
                    }
                }
            }
            // Stream closed without [DONE]: treat as finished.
            let _ = tx.send(Ok(ChatEvent::Done));
        });
        Ok(UnboundedReceiverStream::new(rx).boxed())
    }

    async fn submit_batch(
        &self,
        key: &str,
        reqs: &[(String, ChatRequest)],
    ) -> Result<(String, BatchInfo)> {
        let body = build_multi_batch_body(reqs)?;
        return self.submit_batch_body(key, &body).await;
    }

    async fn batch_raw(&self, key: &str, id: &str) -> Result<Option<Value>> {
        let resp = self
            .http
            .get(format!("{BASE}/batches/{id}"))
            .bearer_auth(key)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(fail(resp).await);
        }
        return Ok(Some(resp.json().await?));
    }

    async fn delete_batch(&self, key: &str, id: &str) -> Result<()> {
        let resp = self
            .http
            .delete(format!("{BASE}/batches/{id}"))
            .bearer_auth(key)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(fail(resp).await);
        }
        log::info!("batch {id} deleted upstream");
        return Ok(());
    }
}

#[cfg(test)]
mod tests;
