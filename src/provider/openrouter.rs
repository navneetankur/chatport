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
    if !req.tools.is_empty() {
        body["tools"] = req
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
    }
    body
}

/// Batch keeps only these `provider` keys; any other routing preference is a 400.
const BATCH_PROVIDER_KEYS: [&str; 2] = ["only", "allow_fallbacks"];

/// `POST /batches` body with one request. The top-level fields must precede
/// `requests` (OpenRouter streams the body); serde_json sorts keys, and
/// endpoint < model < provider < requests, which a test pins down.
pub fn build_batch_body(req: &ChatRequest) -> Value {
    let mut body = build_body(req);
    let obj = body.as_object_mut().expect("body is an object");
    obj.remove("stream");
    obj.remove("model");
    let provider = obj.remove("provider").and_then(|p| {
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
        (!kept.is_empty()).then_some(Value::Object(kept))
    });
    let mut batch = json!({
        "endpoint": "/v1/chat/completions",
        "model": req.model,
        "requests": [{ "custom_id": "r1", "body": body }],
    });
    if let Some(p) = provider {
        batch["provider"] = p;
    }
    return batch;
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
    };
}

/// Interprets `GET /batches/{id}`.
pub fn parse_batch(v: &Value) -> Result<BatchPoll> {
    let status = v["status"].as_str().unwrap_or("unknown");
    let msg = |v: &Value| {
        v.pointer("/error/message")
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    match status {
        "completed" => {}
        "failed" | "expired" | "cancelled" => {
            return Ok(BatchPoll::Done(BatchReply {
                error: Some(format!(
                    "batch {status}: {}",
                    msg(v).unwrap_or_else(|| "no details".into())
                )),
                ..Default::default()
            }));
        }
        s => return Ok(BatchPoll::Pending(s.to_string())),
    }
    let Some(result) = v["results"].as_array().and_then(|r| r.first()) else {
        // Completed, but results not attached yet (docs: can lag briefly).
        return Ok(BatchPoll::Pending("finalizing".into()));
    };
    let batch_usage = v.get("usage").filter(|u| !u.is_null()).map(usage_from);
    if !result["error"].is_null() {
        return Ok(BatchPoll::Done(BatchReply {
            error: Some(msg(result).unwrap_or_else(|| result["error"].to_string())),
            usage: batch_usage,
            ..Default::default()
        }));
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
    return Ok(BatchPoll::Done(BatchReply {
        content: s("/choices/0/message/content").unwrap_or_default(),
        reasoning: s("/choices/0/message/reasoning").unwrap_or_default(),
        model: s("/model").filter(|m| !m.is_empty()),
        provider: s("/provider")
            .or_else(|| v["provider"].as_str().map(str::to_string))
            .filter(|p| !p.is_empty()),
        usage: body
            .get("usage")
            .filter(|u| !u.is_null())
            .map(usage_from)
            .or(batch_usage),
        error: None,
        tool_calls,
    }));
}

/// Error body of a failed OpenRouter call as an `anyhow` error.
async fn fail(resp: reqwest::Response) -> anyhow::Error {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    return anyhow!("OpenRouter {status}: {text}");
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
            while let Some(chunk) = bytes.next().await {
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

    async fn submit_batch(&self, req: &ChatRequest) -> Result<String> {
        let resp = self
            .http
            .post(format!("{BASE}/batches"))
            .bearer_auth(&req.key)
            .json(&build_batch_body(req))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(fail(resp).await);
        }
        let v: Value = resp.json().await?;
        let id = v["id"]
            .as_str()
            .ok_or_else(|| anyhow!("batch submit: no id in response"))?;
        log::info!("batch {id} submitted ({})", v["status"]);
        return Ok(id.to_string());
    }

    async fn poll_batch(&self, key: &str, id: &str) -> Result<BatchPoll> {
        let resp = self
            .http
            .get(format!("{BASE}/batches/{id}"))
            .bearer_auth(key)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(BatchPoll::NotFound);
        }
        if !resp.status().is_success() {
            return Err(fail(resp).await);
        }
        let v: Value = resp.json().await?;
        return parse_batch(&v);
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
mod tests {
    use super::*;

    #[test]
    fn attribution_headers_all_present() {
        let h = attribution_headers();
        assert_eq!(
            h["HTTP-Referer"],
            "https://github.com/navneetankur/chatport"
        );
        assert_eq!(h["X-OpenRouter-Title"], "chatport");
        assert_eq!(h["X-OpenRouter-App-Visibility"], "hidden");
        assert_eq!(h["X-OpenRouter-Categories"], "general-chat");
    }

    fn req(model: &str, cache: CachePolicy) -> ChatRequest {
        ChatRequest {
            key: String::new(),
            model: model.into(),
            system: Some("sys".into()),
            messages: vec![
                ChatMessage::text(Role::User, "a".into()),
                ChatMessage::text(Role::Assistant, "b".into()),
                ChatMessage::text(Role::User, "c".into()),
            ],
            options: Some(json!({ "order": ["anthropic"] })),
            cache,
            reasoning_effort: None,
            tools: vec![],
            session_id: "conv1".into(),
        }
    }

    #[test]
    fn session_id_always_sent() {
        assert_eq!(
            build_body(&req("x/y", CachePolicy::Off))["session_id"],
            "conv1"
        );
    }

    #[test]
    fn anthropic_gets_markers_on_system_and_last_user() {
        let b = build_body(&req("anthropic/claude-x", CachePolicy::Auto));
        let m = b["messages"].as_array().unwrap();
        assert!(m[0]["content"][0]["cache_control"].is_object());
        assert!(m[1]["content"].is_string());
        assert!(m[3]["content"][0]["cache_control"].is_object());
        assert_eq!(b["provider"]["order"][0], "anthropic");
    }

    #[test]
    fn reasoning_effort_only_when_set() {
        let mut r = req("x/y", CachePolicy::Off);
        assert!(build_body(&r).get("reasoning").is_none());
        r.reasoning_effort = Some(ReasoningEffort::Xhigh);
        assert_eq!(build_body(&r)["reasoning"], json!({ "effort": "xhigh" }));
    }

    #[test]
    fn no_markers_when_off_or_auto_model() {
        for b in [
            build_body(&req("anthropic/claude-x", CachePolicy::Off)),
            build_body(&req("openai/gpt-x", CachePolicy::Auto)),
            build_body(&req("google/gemini-x", CachePolicy::Auto)),
        ] {
            assert!(
                b["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|m| m["content"].is_string())
            );
        }
    }

    #[test]
    fn parses_delta_usage_done_error() {
        let ev = parse_chunk(r#"{"choices":[{"delta":{"content":"hi"}}]}"#).unwrap();
        assert!(matches!(&ev[0], ChatEvent::Delta(t) if t == "hi"));
        let ev = parse_chunk(
            r#"{"model":"x/y","provider":"Nvidia","choices":[{"delta":{"content":"hi"}}]}"#,
        )
        .unwrap();
        assert!(matches!(&ev[0], ChatEvent::Model(m) if m == "x/y"));
        assert!(matches!(&ev[1], ChatEvent::Provider(p) if p == "Nvidia"));
        assert!(matches!(&ev[2], ChatEvent::Delta(t) if t == "hi"));
        let ev =
            parse_chunk(r#"{"choices":[{"delta":{"content":"","reasoning":"hmm"}}]}"#).unwrap();
        assert!(matches!(&ev[..], [ChatEvent::Reasoning(t)] if t == "hmm"));
        let ev = parse_chunk(
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":8},"completion_tokens_details":{"reasoning_tokens":1},"cost":0.01,"cost_details":{"upstream_inference_cost":0.5}}}"#,
        )
        .unwrap();
        assert!(
            matches!(&ev[0], ChatEvent::Usage(u) if u.cached_tokens == 8 && u.reasoning_tokens == 1 && u.cost == Some(0.01) && u.upstream_cost == Some(0.5))
        );
        assert!(matches!(parse_chunk("[DONE]").unwrap()[0], ChatEvent::Done));
        assert!(parse_chunk(r#"{"error":{"message":"nope"}}"#).is_err());
    }

    #[test]
    fn tools_and_tool_messages_in_body() {
        let mut r = req("x/y", CachePolicy::Off);
        assert!(build_body(&r).get("tools").is_none());
        r.tools = crate::tools::specs();
        r.messages.push(ChatMessage {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: "t1".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"a"}"#.into(),
            }],
            tool_call_id: None,
        });
        r.messages.push(ChatMessage {
            role: Role::Tool,
            content: "hello".into(),
            tool_calls: vec![],
            tool_call_id: Some("t1".into()),
        });
        let b = build_body(&r);
        assert_eq!(b["tools"][1]["function"]["name"], "read_file");
        let m = b["messages"].as_array().unwrap();
        let call = &m[4];
        assert!(call["content"].is_null());
        assert_eq!(
            call["tool_calls"][0]["function"]["arguments"],
            r#"{"path":"a"}"#
        );
        assert_eq!(
            m[5],
            json!({ "role": "tool", "content": "hello", "tool_call_id": "t1" })
        );
    }

    #[test]
    fn marker_moves_to_last_tool_result() {
        let mut r = req("anthropic/claude-x", CachePolicy::Auto);
        r.messages.push(ChatMessage {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: "t1".into(),
                name: "list_dir".into(),
                arguments: "{}".into(),
            }],
            tool_call_id: None,
        });
        r.messages.push(ChatMessage {
            role: Role::Tool,
            content: "x".into(),
            tool_calls: vec![],
            tool_call_id: Some("t1".into()),
        });
        let b = build_body(&r);
        let m = b["messages"].as_array().unwrap();
        assert!(m[3]["content"].is_string(), "user msg no longer marked");
        assert!(m[4]["content"].is_null());
        assert_eq!(m[5]["content"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(m[5]["tool_call_id"], "t1");
    }

    #[test]
    fn parses_tool_call_fragments() {
        let ev = parse_chunk(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"t1","type":"function","function":{"name":"read_file","arguments":""}}]}}]}"#,
        )
        .unwrap();
        assert!(
            matches!(&ev[..], [ChatEvent::ToolCall { index: 0, id: Some(i), name: Some(n), arguments }]
            if i == "t1" && n == "read_file" && arguments.is_empty())
        );
        let ev = parse_chunk(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"pa"}}]}}]}"#,
        )
        .unwrap();
        assert!(
            matches!(&ev[..], [ChatEvent::ToolCall { index: 1, id: None, name: None, arguments }]
            if arguments == "{\"pa")
        );
    }

    #[test]
    fn batch_body_fields_before_requests() {
        let mut r = req("anthropic/claude-x:batch", CachePolicy::Auto);
        r.options = Some(json!({ "only": ["anthropic"], "data_collection": "deny" }));
        let b = build_batch_body(&r);
        let s = serde_json::to_string(&b).unwrap();
        let at = |k: &str| s.find(&format!("\"{k}\"")).unwrap();
        assert!(at("endpoint") < at("requests"));
        assert!(at("model") < at("requests"));
        assert!(at("provider") < at("requests"));
        assert_eq!(b["provider"], json!({ "only": ["anthropic"] }));
        let body = &b["requests"][0]["body"];
        assert!(body.get("stream").is_none() && body.get("provider").is_none());
        assert!(body.get("model").is_none());
        assert_eq!(body["session_id"], "conv1");
        assert!(body["messages"][0]["content"][0]["cache_control"].is_object());
        // Only unsupported keys → no provider at all.
        r.options = Some(json!({ "data_collection": "deny" }));
        assert!(build_batch_body(&r).get("provider").is_none());
    }

    #[test]
    fn parse_batch_states() {
        let p = parse_batch(&json!({ "status": "in_progress", "results": null })).unwrap();
        assert!(matches!(p, BatchPoll::Pending(s) if s == "in_progress"));
        let p = parse_batch(&json!({ "status": "completed", "results": null })).unwrap();
        assert!(matches!(p, BatchPoll::Pending(s) if s == "finalizing"));
        let p =
            parse_batch(&json!({ "status": "expired", "error": { "message": "late" } })).unwrap();
        assert!(
            matches!(p, BatchPoll::Done(r) if r.error.as_deref() == Some("batch expired: late"))
        );
        let p = parse_batch(&json!({ "status": "completed", "results": [
            { "custom_id": "r1", "response": null, "error": { "message": "too long" } }
        ]}))
        .unwrap();
        assert!(matches!(p, BatchPoll::Done(r) if r.error.as_deref() == Some("too long")));
        let p = parse_batch(
            &json!({ "status": "completed", "provider": "Anthropic", "results": [
                { "custom_id": "r1", "error": null, "response": { "status_code": 200, "body": {
                    "model": "anthropic/claude-x",
                    "choices": [{ "message": { "content": "hi", "reasoning": "hm",
                        "tool_calls": [{ "function": { "name": "read_file" } }] } }],
                    "usage": { "prompt_tokens": 5, "completion_tokens": 2, "cost": 0.01 }
                }}}
            ]}),
        )
        .unwrap();
        let BatchPoll::Done(r) = p else { panic!() };
        assert_eq!((r.content.as_str(), r.reasoning.as_str()), ("hi", "hm"));
        assert_eq!(r.model.as_deref(), Some("anthropic/claude-x"));
        assert_eq!(r.provider.as_deref(), Some("Anthropic"));
        assert_eq!(r.usage.unwrap().cost, Some(0.01));
        assert_eq!(r.tool_calls, ["read_file"]);
        assert!(r.error.is_none());
    }
}
