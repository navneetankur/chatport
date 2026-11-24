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
//!
//! llmjob batch jobs (`data_dir/llmjob/jobs/`) share the poller under keys
//! `llmjob:<short>`: found by the startup scan, and by an inotify watch on
//! `llmjob/jobs/pending/` for jobs submitted while the server runs
//! (`POST api/llmjob/rescan` does a scan by hand). Same gaps, by job age.
//! A finished job is cached for `llmjob get`; nothing is written elsewhere.
//!
//! The upstream half (record, submit, one look, key guard) is shared with
//! llmjob in `upstream`; this file is the conversation side.
//!
//! The poller sleeps until the earliest due check; `schedule`/`unschedule`
//! wake it (`AppState.batch_wake`) so it re-reads the earliest time. With
//! nothing pending it sleeps until the next submit.

pub mod upstream;

use crate::AppState;
use crate::api::{R, St, bad, context, ev, load_meta, record_model, save_reply};
use crate::provider::openrouter::parse_batch;
use crate::provider::{BatchInfo, BatchReply, ChatRequest};
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
use upstream::{Look, WrongKey};

/// First automatic check after submit.
const FIRST_GAP: Duration = Duration::from_secs(300);

/// Poll-map key prefix of llmjob jobs (conversation ids are plain alphanumerics).
const JOB_PREFIX: &str = "llmjob:";

/// Next automatic check per conversation with a pending batch (or llmjob job).
pub type Polls = Mutex<HashMap<String, Instant>>;

/// Shared by everything that saves or rewrites a pending batch, so a reply is
/// saved once even when ⟳ and the poller finish the same batch together.
pub type Lock = tokio::sync::Mutex<()>;

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
        // Stores a permit if the poller is busy, so the change is never missed.
        s.batch_wake.notify_one();
    }
}

fn unschedule(s: &AppState, conv_id: &str) {
    if s.batch_polls.lock().unwrap().remove(conv_id).is_some() {
        s.batch_wake.notify_one();
    }
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
                web_tools: false,
                session_id: meta.id.clone(),
            };
            let (batch, info) =
                upstream::submit(s.provider.as_ref(), meta.key_name.clone(), vec![req]).await?;
            let pending = PendingBatch {
                batch,
                user_id: user.id.clone(),
                model: meta.model.clone(),
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
                log::info!("{conv_id}: waiting on batch {}", p.batch.id);
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
/// copy deleted in the background. A key other than the submit key is a
/// `WrongKey` error and changes nothing.
pub async fn refresh(s: &Arc<AppState>, conv_id: &str) -> Result<Refreshed> {
    let Some(p) = s.store.meta(conv_id)?.and_then(|m| m.pending_batch) else {
        return Ok(Refreshed::None);
    };
    let key = s.config.key_for(p.batch.key_name.as_deref())?.to_string();
    let (reply, delete_upstream) = match upstream::look(s.provider.as_ref(), &p.batch, &key).await?
    {
        Look::Pending { info, .. } => return update_info(s, conv_id, p, info).await,
        Look::Submitted => {
            let info = BatchInfo {
                status: "submitted".into(),
                ..p.info.clone()
            };
            return Ok(Refreshed::Pending(PendingBatch { info, ..p }));
        }
        Look::Gone => {
            let r = BatchReply {
                error: Some(upstream::gone_msg(&p.batch)),
                ..Default::default()
            };
            (r, false)
        }
        Look::Done(v) => match parse_batch(&v) {
            Some(r) => (r, true),
            None => {
                // Completed, results not attached yet.
                let info = BatchInfo {
                    status: "finalizing".into(),
                    ..p.info.clone()
                };
                return update_info(s, conv_id, p, info).await;
            }
        },
    };
    let _g = s.batch_lock.lock().await;
    let Some(mut meta) = s.store.meta(conv_id)? else {
        return Ok(Refreshed::None);
    };
    if meta.pending_batch.as_ref().map(|b| &b.batch.id) != Some(&p.batch.id) {
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
    log::info!("{conv_id}: batch {} done, reply {}", p.batch.id, asst.id);
    if delete_upstream {
        let s = s.clone();
        tokio::spawn(async move { upstream::delete(s.provider.as_ref(), &p.batch, &key).await });
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
            && let Some(b) = meta
                .pending_batch
                .as_mut()
                .filter(|b| b.batch.id == p.batch.id)
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
    let deleted = match s.config.key_for(p.batch.key_name.as_deref()) {
        Ok(key) => s.provider.delete_batch(key, &p.batch.id).await,
        Err(e) => Err(e),
    };
    if let Err(e) = &deleted {
        log::warn!(
            "{id}: forgot batch {} without deleting it: {e:#}",
            p.batch.id
        );
    }
    Ok(Json(json!({ "upstream_deleted": deleted.is_ok() })))
}

fn age_since(created_at_ms: u64) -> Duration {
    Duration::from_millis(now_ms().saturating_sub(created_at_ms))
}

fn data_dir(s: &AppState) -> std::path::PathBuf {
    crate::config::expand_tilde(&s.config.data_dir)
}

/// Schedules llmjob job `short` by its age, unless it is scheduled already
/// (so repeats cause no burst of checks). True if it was added.
fn schedule_job(s: &AppState, short: &str, created_at: u64) -> bool {
    let key = format!("{JOB_PREFIX}{short}");
    if s.batch_polls.lock().unwrap().contains_key(&key) {
        return false;
    }
    let gap = next_gap(age_since(created_at), s.config.batch.poll_interval);
    schedule(s, &key, gap);
    log::info!("llmjob {short}: scheduled, first check in {gap:?}");
    return true;
}

/// `POST api/llmjob/rescan`: schedules every unfetched llmjob job not
/// scheduled yet. The watcher normally makes this unnecessary; kept for a UI
/// button and for setups where inotify misses changes.
pub async fn job_rescan_handler(State(s): St) -> R<Json<Value>> {
    if s.config.batch.poll_interval.is_zero() {
        return Ok(Json(json!({ "scheduled": 0, "auto_poll": false })));
    }
    let jobs = crate::llmjob::jobs::unfetched(&data_dir(&s)).map_err(|e| bad(&format!("{e:#}")))?;
    let added = jobs
        .into_iter()
        .filter(|(short, created)| schedule_job(&s, short, *created))
        .count();
    Ok(Json(json!({ "scheduled": added, "auto_poll": true })))
}

/// Watches `llmjob/jobs/pending/` (inotify): a job record written by the CLI
/// gets scheduled at once. Sleeps in the kernel while nothing happens.
fn start_job_watcher(s: Arc<AppState>) -> Result<()> {
    use inotify::{Inotify, WatchMask};
    let dir = crate::llmjob::jobs::pending_dir(&data_dir(&s))?;
    let ino = Inotify::init()?;
    // Records are written as tmp + rename (MOVED_TO); CREATE for anything else.
    ino.watches()
        .add(&dir, WatchMask::CREATE | WatchMask::MOVED_TO)?;
    let mut events = ino.into_event_stream([0u8; 4096])?;
    log::info!("watching {} for llmjob jobs", dir.display());
    tokio::spawn(async move {
        while let Some(ev) = events.next().await {
            let ev = match ev {
                Ok(ev) => ev,
                Err(e) => {
                    log::warn!("llmjob watcher stopped: {e}");
                    return;
                }
            };
            let Some(short) = ev
                .name
                .as_ref()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".json"))
                .filter(|n| !n.contains('.'))
            else {
                continue; // tmp or results files
            };
            match crate::llmjob::jobs::pending_created_at(&data_dir(&s), short) {
                Ok(Some(created)) => {
                    schedule_job(&s, short, created);
                }
                Ok(None) => {} // moved on (fetched / removed) already
                Err(e) => log::warn!("llmjob watcher: {short}: {e:#}"),
            }
        }
    });
    return Ok(());
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
        if let Err(e) = start_job_watcher(s.clone()) {
            log::warn!("llmjob watcher not started ({e:#}); new jobs wait for a restart or rescan");
        }
        loop {
            let next = s.batch_polls.lock().unwrap().values().min().copied();
            match next {
                Some(at) => tokio::select! {
                    _ = tokio::time::sleep_until(at) => poll_due(&s, cap).await,
                    _ = s.batch_wake.notified() => {}
                },
                None => s.batch_wake.notified().await,
            }
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
    for (short, _) in crate::llmjob::jobs::unfetched(&data_dir(s))? {
        schedule(s, &format!("{JOB_PREFIX}{short}"), Duration::ZERO);
        n += 1;
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
        if let Some(short) = id.strip_prefix(JOB_PREFIX) {
            poll_job(s, &id, short, cap).await;
            continue;
        }
        match refresh(s, &id).await {
            Ok(Refreshed::Pending(p)) => {
                let gap = next_gap(p.batch.age(), cap);
                log::info!("auto-poll {id}: {}, next check in {gap:?}", p.info.status);
                schedule(s, &id, gap);
            }
            Ok(Refreshed::Saved(_)) => log::info!("auto-poll {id}: reply saved"),
            Ok(Refreshed::None) => unschedule(s, &id),
            Err(e) if e.downcast_ref::<WrongKey>().is_some() => {
                // Same key next time: stop. A restart with the right key
                // rescans; ⟳ shows the error.
                log::warn!("auto-poll {id}: {e:#}; not polling it");
                unschedule(s, &id);
            }
            Err(e) => {
                // Includes a deleted conversation's folder: retry later anyway;
                // the next refresh sees no meta and unschedules.
                log::warn!("auto-poll {id}: {e:#}");
                schedule(s, &id, cap.min(FIRST_GAP));
            }
        }
    }
}

async fn poll_job(s: &Arc<AppState>, key: &str, short: &str, cap: Duration) {
    let r =
        crate::llmjob::jobs::poll_once(&s.config, s.provider.as_ref(), &data_dir(s), short).await;
    match r {
        Ok(Some(created)) => schedule(s, key, next_gap(age_since(created), cap)),
        Ok(None) => unschedule(s, key),
        Err(e) => {
            log::warn!("auto-poll {key}: {e:#}");
            schedule(s, key, cap.min(FIRST_GAP));
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
