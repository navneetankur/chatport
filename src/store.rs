//! On-disk storage.
//!
//! data_dir/conversations/<id>/meta.json      mutable settings, rewritten atomically
//! data_dir/conversations/<id>/messages.jsonl append-only message tree

use crate::provider::{CachePolicy, ReasoningEffort, Role, Usage};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn new_id() -> String {
    uuid::Uuid::now_v7().simple().to_string()
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Meta {
    pub id: String,
    pub title: String,
    pub created_at: u64,
    pub updated_at: u64,
    /// Model for the next request. Each assistant message records its own model.
    pub model: String,
    #[serde(default)]
    pub provider: Option<Value>,
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub cache: CachePolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Currently shown branch tip. None = last appended message.
    #[serde(default)]
    pub leaf: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Attachment {
    pub name: String,
    pub content: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Message {
    pub id: String,
    pub parent_id: Option<String>,
    pub role: Role,
    pub content: String,
    /// Model's reasoning text (assistant only). Shown in the UI, never sent back as context.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reasoning: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Model id the request asked for, when it differs from `model` (e.g. a router like `openrouter/free`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<String>,
    /// Upstream provider that served the reply (e.g. "Nvidia").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Stream ended early (error / abort). Content is whatever arrived.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub partial: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at: u64,
}

impl Message {
    pub fn user(parent_id: Option<String>, content: String, attachments: Vec<Attachment>) -> Self {
        Self {
            id: new_id(),
            parent_id,
            role: Role::User,
            content,
            reasoning: String::new(),
            attachments,
            model: None,
            requested_model: None,
            provider_name: None,
            usage: None,
            partial: false,
            error: None,
            created_at: now_ms(),
        }
    }
    pub fn assistant(parent_id: String, model: String) -> Self {
        Self {
            role: Role::Assistant,
            model: Some(model),
            ..Self::user(Some(parent_id), String::new(), vec![])
        }
    }
}

pub struct Store {
    root: PathBuf,
    write: Mutex<()>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

impl Store {
    pub fn open(data_dir: PathBuf) -> Result<Self> {
        let root = data_dir.join("conversations");
        std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
        Ok(Self {
            root,
            write: Mutex::new(()),
        })
    }

    fn dir(&self, id: &str) -> Result<PathBuf> {
        if !valid_id(id) {
            bail!("invalid conversation id");
        }
        Ok(self.root.join(id))
    }

    pub fn list(&self) -> Result<Vec<Meta>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(&self.root)? {
            let path = e?.path().join("meta.json");
            match std::fs::read(&path)
                .map_err(anyhow::Error::from)
                .and_then(|b| Ok(serde_json::from_slice(&b)?))
            {
                Ok(m) => out.push(m),
                Err(err) => tracing::warn!("skipping {}: {err:#}", path.display()),
            }
        }
        out.sort_by_key(|m: &Meta| std::cmp::Reverse(m.updated_at));
        Ok(out)
    }

    pub fn meta(&self, id: &str) -> Result<Option<Meta>> {
        let path = self.dir(id)?.join("meta.json");
        match std::fs::read(&path) {
            Ok(b) => Ok(Some(serde_json::from_slice(&b)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Create or replace meta.json atomically (write tmp + rename).
    pub fn put_meta(&self, meta: &Meta) -> Result<()> {
        let dir = self.dir(&meta.id)?;
        let _g = self.write.lock().unwrap();
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join("meta.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(meta)?)?;
        std::fs::rename(tmp, dir.join("meta.json"))?;
        Ok(())
    }

    pub fn messages(&self, id: &str) -> Result<Vec<Message>> {
        let path = self.dir(id)?.join("messages.jsonl");
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        Ok(text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| match serde_json::from_str(l) {
                Ok(m) => Some(m),
                Err(e) => {
                    tracing::warn!("{}: skipping bad line: {e}", path.display());
                    None
                }
            })
            .collect())
    }

    pub fn append(&self, id: &str, msg: &Message) -> Result<()> {
        let dir = self.dir(id)?;
        let _g = self.write.lock().unwrap();
        if !dir.join("meta.json").exists() {
            bail!("conversation {id} does not exist");
        }
        let mut line = serde_json::to_vec(msg)?;
        line.push(b'\n');
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("messages.jsonl"))?
            .write_all(&line)?;
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<bool> {
        let dir = self.dir(id)?;
        let _g = self.write.lock().unwrap();
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
}
