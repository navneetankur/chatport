//! Batch replies (`<slug>:batch` models): submit, refresh (manual ⟳ and the
//! background poller), forget.
//!
//! A conversation has at most one pending batch, stored in `meta.pending_batch`.
//! The reply is appended to `messages.jsonl` only once it has arrived.
//!
//! Poll schedule: the first check is 5 min after submit; after that the gap
//! equals the batch's age (checks at 5, 10, 20, 40 min, …), capped at
//! `[batch] poll_interval`. It depends only on `submitted_at`, so it survives
//! restarts. Every pending batch is checked once at startup.

use crate::AppState;
use crate::api::{R, St, bad, context, ev, load_meta, record_model, save_reply};
use crate::provider::{BatchInfo, BatchPoll, BatchReply, ChatRequest};
use crate::store::{Message, Meta, PendingBatch, now_ms};
use crate::tree;
use anyhow::{Result, anyhow};
use axum::Json;
use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::{Stream, StreamExt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;
use tokio_stream::wrappers::UnboundedReceiverStream;

/// A fresh batch can 404 for a few seconds after submit.
const VISIBLE_AFTER: Duration = Duration::from_secs(120);
/// First automatic check after submit.
const FIRST_GAP: Duration = Duration::from_secs(300);
/// How often the poller looks for due batches.
const TICK: Duration = Duration::from_secs(60);

/// Next automatic check per conversation with a pending batch.
pub type Polls = Mutex<HashMap<String, Instant>>;

/// Shared by everything that saves or rewrites a pending batch, so a reply is
/// saved once even when ⟳ and the poller finish the same batch together.
pub type Lock = tokio::sync::Mutex<()>;

fn age_of(p: &PendingBatch) -> Duration {
    Duration::from_millis(now_ms().saturating_sub(p.submitted_at))
}

/// Gap until the next check of a batch this old: doubling from 5 min, capped.
fn next_gap(age: Duration, cap: Duration) -> Duration {
    age.max(FIRST_GAP).min(cap)
}

fn schedule(s: &AppState, conv_id: &str, gap: Duration) {
    if !s.config.batch.poll_interval.is_zero() {
        s.batch_polls
            .lock()
            .unwrap()
            .insert(conv_id.to_string(), Instant::now() + gap);
    }
}

fn unschedule(s: &AppState, conv_id: &str) {
    s.batch_polls.lock().unwrap().remove(conv_id);
}

/// Submits the reply as a one-request batch. SSE: optional `user_saved`, then
/// `batch {pending}` on success, or `error` + `done {assistant msg}` on failure.
/// Tools are not offered: a batch is a single round.
pub fn submit(
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
    tokio::spawn(async move {
        let conv_id = meta.id.clone();
        let submitted = async {
            let key = s.config.key_for(meta.key_name.as_deref())?.to_string();
            let req = ChatRequest {
                key,
                model: meta.model.clone(),
                system: meta.system.clone(),
                messages: context(&tree::path(&msgs, &user.id), false),
                options: meta.provider.clone(),
                cache: meta.cache,
                reasoning_effort: meta.reasoning_effort,
                tools: vec![],
                session_id: meta.id.clone(),
            };
            let (id, info) = s.provider.submit_batch(&req).await?;
            let pending = PendingBatch {
                id,
                user_id: user.id.clone(),
                model: meta.model.clone(),
                key_name: meta.key_name.clone(),
                submitted_at: now_ms(),
                info,
            };
            let _g = s.batch_lock.lock().await;
            // Re-read meta: settings may have been patched meanwhile.
            let mut m = s
                .store
                .meta(&conv_id)?
                .ok_or_else(|| anyhow!("conversation vanished"))?;
            m.pending_batch = Some(pending.clone());
            m.updated_at = now_ms();
            s.store.put_meta(&m)?;
            anyhow::Ok(pending)
        };
        match submitted.await {
            Ok(p) => {
                log::info!("{conv_id}: waiting on batch {}", p.id);
                schedule(
                    &s,
                    &conv_id,
                    next_gap(Duration::ZERO, s.config.batch.poll_interval),
                );
                let _ = tx.send(ev("batch", &p));
            }
            Err(e) => {
                let mut asst = Message::assistant(user.id.clone(), meta.model.clone());
                asst.error = Some(format!("batch submit failed: {e:#}"));
                asst.partial = true;
                if let Err(e) = save_reply(&s, &conv_id, &asst) {
                    log::error!("saving reply in {conv_id}: {e:#}");
                }
                let _ = tx.send(ev("error", json!({ "message": asst.error })));
                let _ = tx.send(ev("done", &asst));
            }
        }
    });
    Sse::new(UnboundedReceiverStream::new(rx).map(Ok)).keep_alive(KeepAlive::default())
}

/// The assistant message for a finished batch.
fn batch_message(p: &PendingBatch, r: BatchReply) -> Message {
    let mut asst = Message::assistant(p.user_id.clone(), p.model.clone());
    if let Some(m) = r.model {
        record_model(&mut asst, &p.model, m);
    }
    asst.provider_name = r.provider;
    asst.usage = r.usage;
    asst.content = r.content;
    asst.reasoning = r.reasoning;
    asst.partial = r.error.is_some();
    asst.error = r.error;
    if !r.tool_calls.is_empty() {
        asst.warning = Some(format!(
            "model tried to call {}, but tools are not run in batch mode",
            r.tool_calls.join(", ")
        ));
    }
    return asst;
}

#[derive(Debug)]
pub enum Refreshed {
    /// Nothing pending (never was, or already handled).
    None,
    /// Still running; the pending batch with its latest info.
    Pending(PendingBatch),
    /// Reply saved by this call.
    Saved(Message),
}

/// Checks the pending batch of `conv_id` once. Finished (incl. failed, expired,
/// or gone upstream) → reply appended, leaf set, pending cleared, upstream
/// copy deleted in the background.
pub async fn refresh(s: &Arc<AppState>, conv_id: &str) -> Result<Refreshed> {
    let Some(p) = s.store.meta(conv_id)?.and_then(|m| m.pending_batch) else {
        return Ok(Refreshed::None);
    };
    let key = s.config.key_for(p.key_name.as_deref())?.to_string();
    let (reply, delete_upstream) = match s.provider.poll_batch(&key, &p.id).await? {
        BatchPoll::Pending(info) => return update_info(s, conv_id, p, info).await,
        BatchPoll::NotFound if age_of(&p) < VISIBLE_AFTER => {
            let info = BatchInfo {
                status: "submitted".into(),
                ..p.info.clone()
            };
            return Ok(Refreshed::Pending(PendingBatch { info, ..p }));
        }
        BatchPoll::NotFound => {
            let r = BatchReply {
                error: Some(format!(
                    "batch {} not found upstream (deleted, or older than 30 days)",
                    p.id
                )),
                ..Default::default()
            };
            (r, false)
        }
        BatchPoll::Done(r) => (r, true),
    };
    let _g = s.batch_lock.lock().await;
    let Some(mut meta) = s.store.meta(conv_id)? else {
        return Ok(Refreshed::None);
    };
    if meta.pending_batch.as_ref().map(|b| &b.id) != Some(&p.id) {
        // Someone else saved it (⟳ racing the poller) or it was forgotten.
        return Ok(Refreshed::None);
    }
    let asst = batch_message(&p, reply);
    s.store.append(conv_id, &asst)?;
    meta.pending_batch = None;
    meta.leaf = Some(asst.id.clone());
    meta.updated_at = now_ms();
    s.store.put_meta(&meta)?;
    unschedule(s, conv_id);
    log::info!("{conv_id}: batch {} done, reply {}", p.id, asst.id);
    if delete_upstream {
        let s = s.clone();
        tokio::spawn(async move {
            if let Err(e) = s.provider.delete_batch(&key, &p.id).await {
                log::warn!("deleting batch {}: {e:#}", p.id);
            }
        });
    }
    return Ok(Refreshed::Saved(asst));
}

/// Persists new status/provider/deadline so a reload shows them.
async fn update_info(
    s: &AppState,
    conv_id: &str,
    p: PendingBatch,
    info: BatchInfo,
) -> Result<Refreshed> {
    if info != p.info {
        let _g = s.batch_lock.lock().await;
        if let Some(mut meta) = s.store.meta(conv_id)?
            && let Some(b) = meta.pending_batch.as_mut().filter(|b| b.id == p.id)
        {
            b.info = info.clone();
            s.store.put_meta(&meta)?;
        }
    }
    return Ok(Refreshed::Pending(PendingBatch { info, ..p }));
}

/// ⟳. Body: `{status, provider?, expires_at?}` while running, `{status:
/// "done", message}` when this call saved the reply, `{status: "done"}` when
/// nothing is pending any more (e.g. the poller got there first: reload).
pub async fn refresh_handler(State(s): St, Path(id): Path<String>) -> R<Json<Value>> {
    load_meta(&s, &id)?;
    let body = match refresh(&s, &id).await? {
        Refreshed::None => json!({ "status": "done" }),
        Refreshed::Pending(p) => json!(p.info),
        Refreshed::Saved(m) => json!({ "status": "done", "message": m }),
    };
    Ok(Json(body))
}

/// Drops the pending batch locally. OpenRouter has no cancel: a batch still
/// running keeps running and is billed; only a finished one can be deleted.
pub async fn forget_handler(State(s): St, Path(id): Path<String>) -> R<Json<Value>> {
    let p = {
        let _g = s.batch_lock.lock().await;
        let mut meta = load_meta(&s, &id)?;
        let p = meta
            .pending_batch
            .take()
            .ok_or_else(|| bad("no pending batch"))?;
        meta.updated_at = now_ms();
        s.store.put_meta(&meta)?;
        p
    };
    unschedule(&s, &id);
    let deleted = match s.config.key_for(p.key_name.as_deref()) {
        Ok(key) => s.provider.delete_batch(key, &p.id).await,
        Err(e) => Err(e),
    };
    if let Err(e) = &deleted {
        log::warn!("{id}: forgot batch {} without deleting it: {e:#}", p.id);
    }
    Ok(Json(json!({ "upstream_deleted": deleted.is_ok() })))
}

/// Starts the background poller unless `[batch] poll_interval` is 0.
pub fn start_poller(s: Arc<AppState>) {
    let cap = s.config.batch.poll_interval;
    if cap.is_zero() {
        log::info!("batch auto-poll off");
        return;
    }
    tokio::spawn(async move {
        match scan(&s) {
            Ok(n) => log::info!("batch auto-poll every ≤{cap:?}; {n} pending at startup"),
            Err(e) => log::warn!("batch auto-poll: startup scan failed: {e:#}"),
        }
        let mut tick = tokio::time::interval(TICK);
        loop {
            tick.tick().await;
            poll_due(&s, cap).await;
        }
    });
}

/// Schedules every conversation with a pending batch for an immediate check.
fn scan(s: &AppState) -> Result<usize> {
    let mut n = 0;
    for c in s.store.list()? {
        match s.store.meta(&c.id) {
            Ok(Some(m)) if m.pending_batch.is_some() => {
                schedule(s, &c.id, Duration::ZERO);
                n += 1;
            }
            Ok(_) => {}
            Err(e) => log::warn!("batch scan: {}: {e:#}", c.id),
        }
    }
    return Ok(n);
}

async fn poll_due(s: &Arc<AppState>, cap: Duration) {
    let now = Instant::now();
    let due: Vec<String> = s
        .batch_polls
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, at)| **at <= now)
        .map(|(id, _)| id.clone())
        .collect();
    for id in due {
        match refresh(s, &id).await {
            Ok(Refreshed::Pending(p)) => {
                let gap = next_gap(age_of(&p), cap);
                log::info!("auto-poll {id}: {}, next check in {gap:?}", p.info.status);
                schedule(s, &id, gap);
            }
            Ok(Refreshed::Saved(_)) => log::info!("auto-poll {id}: reply saved"),
            Ok(Refreshed::None) => unschedule(s, &id),
            Err(e) => {
                // Includes a deleted conversation's folder: retry later anyway;
                // the next refresh sees no meta and unschedules.
                log::warn!("auto-poll {id}: {e:#}");
                schedule(s, &id, cap.min(FIRST_GAP.max(TICK)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaps_double_from_five_minutes_then_cap() {
        let m = |x: u64| Duration::from_secs(x * 60);
        let cap = m(60);
        // Check times since submit: 5, 10, 20, 40, 80 (capped gap 60 → 100), …
        let mut t = next_gap(Duration::ZERO, cap);
        let mut times = vec![t];
        for _ in 0..5 {
            t += next_gap(t, cap);
            times.push(t);
        }
        assert_eq!(times, [m(5), m(10), m(20), m(40), m(80), m(140)]);
        // Cap below the first gap wins.
        assert_eq!(next_gap(Duration::ZERO, m(2)), m(2));
    }
}
