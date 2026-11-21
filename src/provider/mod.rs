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
}

#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
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
    Done,
}

#[derive(Serialize, Clone, Debug)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub context_length: Option<u64>,
    pub pricing: Value,
}

#[async_trait]
pub trait Provider: Send + Sync {
    async fn list_models(&self) -> Result<Vec<ModelInfo>>;
    async fn stream_chat(&self, req: ChatRequest) -> Result<BoxStream<'static, Result<ChatEvent>>>;
}
