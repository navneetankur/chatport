//! The upstream half of an OpenRouter batch, shared by chatport conversations
//! (`batch.rs`) and llmjob jobs (`llmjob/jobs.rs`): the stored reference,
//! submit, one look upstream, the key guard, freeing the upstream copy.
//! Where a finished batch goes (conversation tree / job files) stays with each.

use crate::attachments::hash_of;
use crate::provider::openrouter::{BatchState, parse_batch_all};
use crate::provider::{BatchInfo, ChatRequest, Provider};
use crate::store::now_ms;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

/// A fresh batch can 404 for a while after submit.
pub const VISIBLE_AFTER: Duration = Duration::from_secs(120);

/// What is kept of a submitted batch. Flattened into chatport's
/// `PendingBatch` and llmjob's `Job`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct BatchRef {
    pub id: String,
    /// Key name the batch was billed to (None = default key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_name: Option<String>,
    /// sha256 hex of the key it was submitted with. Upstream answers 404, not
    /// 401, for a batch another account owns, so a wrong key would look like
    /// "gone".
    pub key_hash: String,
    /// Unix ms.
    pub submitted_at: u64,
}

impl BatchRef {
    pub fn age(&self) -> Duration {
        Duration::from_millis(now_ms().saturating_sub(self.submitted_at))
    }

    /// Refuses a key other than the submit key (`WrongKey`).
    pub fn verify_key(&self, key: &str) -> Result<()> {
        if self.key_hash != hash_of(key) {
            return Err(WrongKey.into());
        }
        return Ok(());
    }
}

/// The key about to be used is not the one the batch was submitted with.
/// Retrying cannot help: pollers stop on it.
#[derive(Debug)]
pub struct WrongKey;

impl std::fmt::Display for WrongKey {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("sent with another key; use the key (--keyfile / config) it was sent with")
    }
}

impl std::error::Error for WrongKey {}

/// custom_id of request `i` (providers restrict custom_id characters, so
/// names are kept locally).
pub fn custom_id(i: usize) -> String {
    format!("r{i}")
}

/// Submits `reqs` (same model, same key) as one batch, request i as
/// `custom_id(i)`.
pub async fn submit(
    provider: &dyn Provider,
    key_name: Option<String>,
    reqs: Vec<ChatRequest>,
) -> Result<(BatchRef, BatchInfo)> {
    let key = reqs.first().context("nothing to submit")?.key.clone();
    let sent: Vec<(String, ChatRequest)> = reqs
        .into_iter()
        .enumerate()
        .map(|(i, r)| (custom_id(i), r))
        .collect();
    let (id, info) = provider.submit_batch(&key, &sent).await?;
    let b = BatchRef {
        id,
        key_name,
        key_hash: hash_of(&key),
        submitted_at: now_ms(),
    };
    return Ok((b, info));
}

/// One look upstream.
#[derive(Debug)]
pub enum Look {
    /// Not visible upstream yet (younger than `VISIBLE_AFTER`).
    Submitted,
    /// Running. `raw` for progress counts.
    Pending { info: BatchInfo, raw: Value },
    /// Finished: completed, failed, expired or cancelled. The raw batch.
    Done(Value),
    /// Unknown upstream past `VISIBLE_AFTER` (deleted, or older than 30 days).
    Gone,
}

/// Asks upstream once with `key`; `WrongKey` (before any request) if it is
/// not the submit key.
pub async fn look(provider: &dyn Provider, b: &BatchRef, key: &str) -> Result<Look> {
    b.verify_key(key)?;
    let look = match provider.batch_raw(key, &b.id).await? {
        None if b.age() < VISIBLE_AFTER => Look::Submitted,
        None => Look::Gone,
        Some(v) => match parse_batch_all(&v) {
            BatchState::Pending(info) => Look::Pending { info, raw: v },
            _ => Look::Done(v),
        },
    };
    return Ok(look);
}

/// Frees the upstream copy. Only once the result is stored locally; best
/// effort (an undeleted batch expires upstream after 30 days).
pub async fn delete(provider: &dyn Provider, b: &BatchRef, key: &str) {
    if let Err(e) = provider.delete_batch(key, &b.id).await {
        log::warn!("deleting batch {} upstream: {e:#}", b.id);
    }
}

pub fn gone_msg(b: &BatchRef) -> String {
    format!(
        "batch {} not found upstream (deleted, or older than 30 days)",
        b.id
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bref(key_hash: String) -> BatchRef {
        BatchRef {
            id: "b".into(),
            key_name: None,
            key_hash,
            submitted_at: 0,
        }
    }

    #[test]
    fn key_hash_guards_upstream_calls() {
        let b = bref(hash_of("k-right"));
        assert!(b.verify_key("k-right").is_ok());
        let e = b.verify_key("k-wrong").unwrap_err();
        assert!(e.downcast_ref::<WrongKey>().is_some());
    }

    #[test]
    fn round_trips() {
        let b = bref("h".into());
        let back: BatchRef = serde_json::from_str(&serde_json::to_string(&b).unwrap()).unwrap();
        assert_eq!(back, b);
    }
}
