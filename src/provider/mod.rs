//! Provider abstraction. Only OpenRouter is implemented; the trait exists so
//! another backend could be added without touching api/store code.

pub mod openrouter;

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    /// Tool result (only in requests, never a stored message's role).
    Tool,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CachePolicy {
    #[default]
    Auto,
    Off,
}

/// Reasoning effort (OpenRouter `reasoning.effort`). Unset = model default.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Max,
    Xhigh,
    High,
    Medium,
    Low,
    Minimal,
    None,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    #[serde(default)]
    pub cached_tokens: u64,
    /// Part of `completion_tokens` spent on reasoning.
    #[serde(default)]
    pub reasoning_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    /// What the upstream provider charged (BYOK: billed to the user's own key,
    /// while `cost` is OpenRouter's charge, often 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_cost: Option<f64>,
    /// Web searches run by server tools (`server_tool_use_details`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub web_searches: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON arguments as the model sent them.
    pub arguments: String,
}

/// A tool offered to the model; `parameters` is a JSON schema.
#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    /// Assistant: calls it made.
    pub tool_calls: Vec<ToolCall>,
    /// Tool: the call this answers.
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn text(role: Role, content: String) -> Self {
        Self {
            role,
            content,
            tool_calls: vec![],
            tool_call_id: None,
        }
    }
}

impl Usage {
    /// Sum of several requests (one per tool round).
    pub fn add(&mut self, o: &Usage) {
        self.prompt_tokens += o.prompt_tokens;
        self.completion_tokens += o.completion_tokens;
        self.cached_tokens += o.cached_tokens;
        self.reasoning_tokens += o.reasoning_tokens;
        self.web_searches += o.web_searches;
        self.cost = add_opt(self.cost, o.cost);
        self.upstream_cost = add_opt(self.upstream_cost, o.upstream_cost);
    }
}

fn add_opt(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
    }
}

#[derive(Clone, Debug)]
pub struct ChatRequest {
    /// API key to bill this request to.
    pub key: String,
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<ChatMessage>,
    /// Provider-specific options (OpenRouter: the `provider` routing object).
    pub options: Option<Value>,
    pub cache: CachePolicy,
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Tools offered; empty = none.
    pub tools: Vec<ToolSpec>,
    /// Offer the provider's server-side web tools (search, fetch, datetime).
    pub web_tools: bool,
    /// Stable per-conversation key (OpenRouter `session_id`: sticky provider
    /// routing from the first request, log grouping).
    pub session_id: String,
}

#[derive(Debug)]
pub enum ChatEvent {
    /// Model that actually serves the request (may differ from the requested id, e.g. routers).
    Model(String),
    /// Upstream provider serving the request (e.g. "Nvidia").
    Provider(String),
    Delta(String),
    /// Reasoning ("thinking") text, streamed before/alongside the answer.
    Reasoning(String),
    Usage(Usage),
    /// Fragment of a tool call; fragments with the same `index` belong together.
    ToolCall {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    },
    /// A raw chunk that is not plain text streaming (usage, annotations,
    /// finish_reason, …), verbatim, for the debug view.
    Raw(Value),
    Done,
}

#[derive(Serialize, Clone, Debug)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub context_length: Option<u64>,
    pub pricing: Value,
    /// Model accepts `tools` (function calling); server tools need it too.
    pub tools: bool,
}

/// Batch models are listed with this suffix (OpenRouter `<slug>:batch`).
pub fn is_batch_model(model: &str) -> bool {
    model.ends_with(":batch")
}

/// The finished reply of a one-request batch.
#[derive(Debug, Default)]
pub struct BatchReply {
    pub content: String,
    pub reasoning: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub usage: Option<Usage>,
    /// Request-level failure (the batch itself completed).
    pub error: Option<String>,
    /// The model asked for tools; batch runs one round only, so they were not run.
    pub tool_calls: Vec<String>,
}

/// Where a batch stands.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct BatchInfo {
    /// validating, in_progress, finalizing, …
    pub status: String,
    /// Upstream provider running it (can change before it starts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Unix seconds after which the provider gives up; None = no hard deadline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

#[derive(Debug)]
pub enum BatchPoll {
    /// Still running.
    Pending(BatchInfo),
    Done(BatchReply),
    /// Unknown id. Also seen for a few seconds right after submit.
    NotFound,
}

#[async_trait]
pub trait Provider: Send + Sync {
    async fn list_models(&self) -> Result<Vec<ModelInfo>>;
    async fn stream_chat(&self, req: ChatRequest) -> Result<BoxStream<'static, Result<ChatEvent>>>;
    /// Submits `req` as a one-request batch. Returns the batch id.
    async fn submit_batch(&self, req: &ChatRequest) -> Result<(String, BatchInfo)>;
    async fn poll_batch(&self, key: &str, id: &str) -> Result<BatchPoll>;
    /// Removes the stored batch upstream. Fails while it is still running.
    async fn delete_batch(&self, key: &str, id: &str) -> Result<()>;
}
