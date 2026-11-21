use super::*;
use anyhow::{Context, anyhow, bail};
use futures::StreamExt;
use serde_json::json;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio_stream::wrappers::UnboundedReceiverStream;

const BASE: &str = "https://openrouter.ai/api/v1";
const MODELS_TTL: Duration = Duration::from_secs(3600);

pub struct OpenRouter {
    key: String,
    http: reqwest::Client,
    models: Mutex<Option<(Instant, Vec<ModelInfo>)>>,
}

impl OpenRouter {
    pub fn new(key: String) -> Self {
        Self {
            key,
            http: reqwest::Client::new(),
            models: Mutex::new(None),
        }
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
    let last_user = req.messages.iter().rposition(|m| m.role == Role::User);
    for (i, m) in req.messages.iter().enumerate() {
        let content = if markers && Some(i) == last_user {
            cached_text(&m.content)
        } else {
            json!(m.content)
        };
        messages.push(json!({ "role": m.role, "content": content }));
    }
    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "stream": true,
        "usage": { "include": true },
    });
    if let Some(p) = req.options.as_ref().filter(|p| !p.is_null()) {
        body["provider"] = p.clone();
    }
    body
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
    if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
        let n = |p: &str| u.pointer(p).and_then(Value::as_u64).unwrap_or(0);
        out.push(ChatEvent::Usage(Usage {
            prompt_tokens: n("/prompt_tokens"),
            completion_tokens: n("/completion_tokens"),
            cached_tokens: n("/prompt_tokens_details/cached_tokens"),
            reasoning_tokens: n("/completion_tokens_details/reasoning_tokens"),
            cost: u.get("cost").and_then(Value::as_f64),
        }));
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
            .bearer_auth(&self.key)
            .header("X-Title", "chatport")
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(model: &str, cache: CachePolicy) -> ChatRequest {
        ChatRequest {
            model: model.into(),
            system: Some("sys".into()),
            messages: vec![
                ChatMessage {
                    role: Role::User,
                    content: "a".into(),
                },
                ChatMessage {
                    role: Role::Assistant,
                    content: "b".into(),
                },
                ChatMessage {
                    role: Role::User,
                    content: "c".into(),
                },
            ],
            options: Some(json!({ "order": ["anthropic"] })),
            cache,
        }
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
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":8},"completion_tokens_details":{"reasoning_tokens":1},"cost":0.01}}"#,
        )
        .unwrap();
        assert!(
            matches!(&ev[0], ChatEvent::Usage(u) if u.cached_tokens == 8 && u.reasoning_tokens == 1 && u.cost == Some(0.01))
        );
        assert!(matches!(parse_chunk("[DONE]").unwrap()[0], ChatEvent::Done));
        assert!(parse_chunk(r#"{"error":{"message":"nope"}}"#).is_err());
    }
}
