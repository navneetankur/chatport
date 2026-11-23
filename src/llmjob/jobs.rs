//! `llmjob batch` / `get` / `ls` / `rm`: OpenRouter batches with one request
//! per input file.
//!
//! data_dir/llmjob/jobs/pending/<short>.json       submitted, not fetched yet
//! data_dir/llmjob/jobs/done/<short>.json          fetched (record moved here)
//! data_dir/llmjob/jobs/done/<short>.results.json  raw final batch
//!
//! Listing in-progress jobs = listing `pending/`; fetched ones are never read
//! for it. "Fetched" = the results file exists (written before the record
//! moves, so a crash in between is repaired on the next listing).
//!
//! Once the final batch is cached locally the upstream copy is deleted, so
//! `get` can be repeated offline.

use super::Ctx;
use crate::config::Config;
use crate::provider::openrouter::{BatchState, build_multi_batch_body, parse_batch_all};
use crate::provider::{BatchReply, ChatRequest, Provider};
use crate::store::now_ms;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A fresh batch can 404 for a few seconds after submit.
const VISIBLE_AFTER_MS: u64 = 120_000;
const WAIT_GAP: Duration = Duration::from_secs(60);
/// Name of the single request when no `--dir` is given.
const SINGLE: &str = "output";
/// Exit code of `get` while the batch is still running.
pub const EXIT_PENDING: i32 = 2;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Job {
    pub short: String,
    pub batch_id: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_name: Option<String>,
    pub created_at: u64,
    /// Name of each request (input file name), in submit order. Request i
    /// is sent as custom_id `r<i>`: providers restrict custom_id characters.
    pub names: Vec<String>,
    /// Last known status.
    pub status: String,
}

pub struct Jobs {
    pending: PathBuf,
    done: PathBuf,
}

/// Job ids (file stems) in `dir`: `<short>.json`, skipping results and tmp files.
fn stems(dir: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let name = e?.file_name();
        if let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".json"))
            && !stem.contains('.')
        {
            out.push(stem.to_string());
        }
    }
    return Ok(out);
}

fn ignore_not_found(r: std::io::Result<()>) -> Result<()> {
    match r {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

impl Jobs {
    pub fn open(data_dir: &Path) -> Result<Self> {
        let root = data_dir.join("llmjob").join("jobs");
        let (pending, done) = (root.join("pending"), root.join("done"));
        for d in [&pending, &done] {
            std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
        }
        return Ok(Self { pending, done });
    }

    fn pending_rec(&self, short: &str) -> PathBuf {
        self.pending.join(format!("{short}.json"))
    }

    fn done_rec(&self, short: &str) -> PathBuf {
        self.done.join(format!("{short}.json"))
    }

    /// Where the record of `short` lives now.
    fn record(&self, short: &str) -> PathBuf {
        let d = self.done_rec(short);
        if d.exists() {
            d
        } else {
            self.pending_rec(short)
        }
    }

    fn results(&self, short: &str) -> PathBuf {
        self.done.join(format!("{short}.results.json"))
    }

    pub fn put(&self, job: &Job) -> Result<()> {
        let target = self.record(&job.short);
        let tmp = target.with_extension(format!("json.tmp{}", std::process::id()));
        std::fs::write(&tmp, serde_json::to_vec_pretty(job)?)?;
        std::fs::rename(&tmp, target)?;
        return Ok(());
    }

    /// Marks a job fetched: results written atomically, then the record moved
    /// to done/. Safe when the CLI and the server both do it.
    fn finish(&self, short: &str, v: &Value) -> Result<()> {
        let results = self.results(short);
        let tmp = results.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&tmp, serde_json::to_vec(v)?)?;
        std::fs::rename(&tmp, &results)?;
        return ignore_not_found(std::fs::rename(
            self.pending_rec(short),
            self.done_rec(short),
        ));
    }

    /// Every job, oldest first.
    pub fn all(&self) -> Result<Vec<Job>> {
        let mut ids = stems(&self.done)?;
        ids.extend(
            stems(&self.pending)?
                .into_iter()
                .filter(|s| !self.done_rec(s).exists()),
        );
        return Ok(self.load_all(ids));
    }

    /// Jobs not fetched yet, oldest first. Reads only `pending/`.
    pub fn pending(&self) -> Result<Vec<Job>> {
        let mut ids = Vec::new();
        for s in stems(&self.pending)? {
            if self.results(&s).exists() {
                // Fetched, but the move to done/ didn't happen (crash): finish it.
                if let Err(e) = std::fs::rename(self.pending_rec(&s), self.done_rec(&s)) {
                    log::warn!("moving job {s} to done: {e}");
                }
                continue;
            }
            ids.push(s);
        }
        return Ok(self.load_all(ids));
    }

    fn load_all(&self, ids: Vec<String>) -> Vec<Job> {
        let mut out = Vec::new();
        for s in ids {
            match self.load(&s) {
                Ok(j) => out.push(j),
                Err(e) => log::warn!("skipping job {s}: {e:#}"),
            }
        }
        out.sort_by_key(|j| j.created_at);
        return out;
    }

    fn load(&self, short: &str) -> Result<Job> {
        let b = std::fs::read(self.record(short))?;
        return Ok(serde_json::from_slice(&b)?);
    }

    /// Removes a job's record and results.
    fn remove(&self, short: &str) -> Result<()> {
        ignore_not_found(std::fs::remove_file(self.results(short)))?;
        ignore_not_found(std::fs::remove_file(self.done_rec(short)))?;
        ignore_not_found(std::fs::remove_file(self.pending_rec(short)))?;
        return Ok(());
    }

    /// By unique prefix, or `last` = newest.
    pub fn find(&self, q: &str) -> Result<Job> {
        let all = self.all()?;
        if q == "last" {
            return all.into_iter().next_back().context("no batch jobs");
        }
        let mut hits: Vec<Job> = all.into_iter().filter(|j| j.short.starts_with(q)).collect();
        match hits.len() {
            0 => bail!("no batch job {q:?}"),
            1 => return Ok(hits.remove(0)),
            n => bail!("{n} batch jobs start with {q:?}"),
        }
    }

    /// A short id not in use: 3 chars, longer if crowded.
    fn new_short(&self) -> String {
        const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
        let mut len = 3;
        loop {
            for _ in 0..20 {
                // Low bits of a v7 uuid are random.
                let mut r = uuid::Uuid::now_v7().as_u128();
                let s: String = (0..len)
                    .map(|_| {
                        let c = ALPHABET[(r % ALPHABET.len() as u128) as usize] as char;
                        r /= ALPHABET.len() as u128;
                        c
                    })
                    .collect();
                if !self.pending_rec(&s).exists() && !self.done_rec(&s).exists() {
                    return s;
                }
            }
            len += 1;
        }
    }
}

/// `llmjob batch`: one request per input (each a full ChatRequest).
pub async fn submit(ctx: &Ctx, reqs: Vec<(String, ChatRequest)>) -> Result<i32> {
    let jobs = Jobs::open(&ctx.data_dir)?;
    let (_, first) = reqs.first().context("nothing to submit")?;
    let (model, key) = (first.model.clone(), first.key.clone());
    let mut names = Vec::with_capacity(reqs.len());
    let mut sent = Vec::with_capacity(reqs.len());
    for (i, (name, req)) in reqs.into_iter().enumerate() {
        names.push(name);
        sent.push((custom_id(i), req));
    }
    let body = build_multi_batch_body(&sent)?;
    let (batch_id, info) = ctx.provider.submit_batch_body(&key, &body).await?;
    let job = Job {
        short: jobs.new_short(),
        batch_id,
        model,
        key_name: ctx.key_name.clone(),
        created_at: now_ms(),
        names,
        status: info.status,
    };
    jobs.put(&job)?;
    println!("{}", job.short);
    return Ok(0);
}

/// One look at a job.
enum Check {
    /// Finished (completed or failed); raw batch cached locally.
    Done(Value),
    /// Still running: progress line for stderr.
    Pending(String),
    /// Unknown upstream past the grace period (deleted, or older than 30 days).
    Gone,
}

/// Reads the cache, or asks upstream once. A finished batch gets cached and
/// its upstream copy deleted; a pending one updates the record's status.
async fn check(
    config: &Config,
    provider: &dyn Provider,
    jobs: &Jobs,
    job: &mut Job,
) -> Result<Check> {
    let cached = jobs.results(&job.short);
    if let Ok(b) = std::fs::read(&cached) {
        return Ok(Check::Done(
            serde_json::from_slice(&b).context("reading cached results")?,
        ));
    }
    let key = config.key_for(job.key_name.as_deref())?.to_string();
    let (status, progress) = match provider.batch_raw(&key, &job.batch_id).await? {
        Some(v) => match parse_batch_all(&v) {
            BatchState::Pending(info) => {
                let counts = &v["request_counts"];
                let progress = format!(
                    "{}: {} ({}/{} done, {} failed)",
                    job.short, info.status, counts["completed"], counts["total"], counts["failed"]
                );
                (info.status, progress)
            }
            _ => {
                jobs.finish(&job.short, &v)?;
                // Results are kept locally now; free the upstream copy.
                if let Err(e) = provider.delete_batch(&key, &job.batch_id).await {
                    log::warn!("deleting batch {} upstream: {e:#}", job.batch_id);
                }
                return Ok(Check::Done(v));
            }
        },
        None if now_ms().saturating_sub(job.created_at) < VISIBLE_AFTER_MS => {
            ("submitted".to_string(), format!("{}: submitted", job.short))
        }
        None => return Ok(Check::Gone),
    };
    if status != job.status {
        job.status = status;
        jobs.put(job)?;
    }
    return Ok(Check::Pending(progress));
}

/// `llmjob get`.
pub async fn get(ctx: &Ctx, q: &str, out: Option<&Path>, wait: bool) -> Result<i32> {
    let jobs = Jobs::open(&ctx.data_dir)?;
    let mut job = jobs.find(q)?;
    if out.is_none() && job.names.len() > 1 {
        bail!(
            "job {} has {} outputs: use --dir DIR",
            job.short,
            job.names.len()
        );
    }
    let v = loop {
        match check(&ctx.config, &ctx.provider, &jobs, &mut job).await? {
            Check::Done(v) => break v,
            Check::Pending(progress) => eprintln!("{progress}"),
            Check::Gone => bail!("{}", gone_msg(&job)),
        }
        if !wait {
            return Ok(EXIT_PENDING);
        }
        tokio::time::sleep(WAIT_GAP).await;
    };
    return write_results(&jobs, job, &v, out);
}

fn gone_msg(job: &Job) -> String {
    format!(
        "{}: batch {} not found upstream (deleted, or older than 30 days)",
        job.short, job.batch_id
    )
}

/// Server poller: unfetched jobs as (short id, created_at ms).
pub(crate) fn unfetched(data_dir: &Path) -> Result<Vec<(String, u64)>> {
    let jobs = Jobs::open(data_dir)?;
    return Ok(jobs
        .pending()?
        .into_iter()
        .map(|j| (j.short, j.created_at))
        .collect());
}

/// The `pending/` folder (created if missing), for the server's watcher.
pub(crate) fn pending_dir(data_dir: &Path) -> Result<PathBuf> {
    return Ok(Jobs::open(data_dir)?.pending);
}

/// created_at of `short` if it is still in `pending/` and not fetched.
pub(crate) fn pending_created_at(data_dir: &Path, short: &str) -> Result<Option<u64>> {
    let jobs = Jobs::open(data_dir)?;
    if !jobs.pending_rec(short).exists() || jobs.results(short).exists() {
        return Ok(None);
    }
    return Ok(Some(jobs.load(short)?.created_at));
}

/// Server poller: one check of job `short`. Some(created_at) = still running,
/// check again later; None = nothing left to do (fetched, removed or gone).
pub(crate) async fn poll_once(
    config: &Config,
    provider: &dyn Provider,
    data_dir: &Path,
    short: &str,
) -> Result<Option<u64>> {
    let jobs = Jobs::open(data_dir)?;
    let Ok(mut job) = jobs.load(short) else {
        return Ok(None); // removed with `llmjob rm`
    };
    match check(config, provider, &jobs, &mut job).await? {
        Check::Done(v) => {
            let state = match parse_batch_all(&v) {
                BatchState::Failed(_) => "failed",
                _ => "completed",
            };
            set_status(&jobs, &mut job, state)?;
            log::info!("llmjob {short}: {state}, cached");
            return Ok(None);
        }
        Check::Pending(progress) => {
            log::info!("{progress}");
            return Ok(Some(job.created_at));
        }
        Check::Gone => {
            log::warn!("{}", gone_msg(&job));
            return Ok(None);
        }
    }
}

/// `llmjob get --all`: checks every unfetched job once and caches the
/// finished ones (no outputs written). Exit 2 if any is still running, 1 if
/// any check failed.
pub async fn get_all(ctx: &Ctx) -> Result<i32> {
    let jobs = Jobs::open(&ctx.data_dir)?;
    let (mut fetched, mut running, mut failed) = (0, 0, 0);
    for mut job in jobs.pending()? {
        match check(&ctx.config, &ctx.provider, &jobs, &mut job).await {
            Ok(Check::Done(v)) => {
                let state = match parse_batch_all(&v) {
                    BatchState::Failed(_) => "failed",
                    _ => "completed",
                };
                set_status(&jobs, &mut job, state)?;
                eprintln!("{}: {state}, cached", job.short);
                fetched += 1;
            }
            Ok(Check::Pending(progress)) => {
                eprintln!("{progress}");
                running += 1;
            }
            Ok(Check::Gone) => {
                eprintln!("{}", gone_msg(&job));
                failed += 1;
            }
            Err(e) => {
                eprintln!("{}: {e:#}", job.short);
                failed += 1;
            }
        }
    }
    eprintln!("fetched {fetched}, still running {running}, failed {failed}");
    return Ok(match (failed, running) {
        (0, 0) => 0,
        (0, _) => EXIT_PENDING,
        _ => 1,
    });
}

/// Writes outputs of a finished batch. Exit 1 if anything failed.
fn write_results(jobs: &Jobs, mut job: Job, v: &Value, out: Option<&Path>) -> Result<i32> {
    let (replies, usage) = match parse_batch_all(v) {
        BatchState::Done { replies, usage } => (replies, usage),
        BatchState::Failed(e) => {
            set_status(jobs, &mut job, "failed")?;
            eprintln!("{}: {e}", job.short);
            return Ok(1);
        }
        BatchState::Pending(_) => bail!("cached batch is not finished"),
    };
    set_status(jobs, &mut job, "completed")?;
    let mut failed = 0;
    let mut seen = std::collections::HashSet::new();
    for (id, r) in &replies {
        let Some(name) = (0..job.names.len())
            .find(|&i| custom_id(i) == *id)
            .map(|i| &job.names[i])
        else {
            log::warn!("ignoring result for unknown request {id:?}");
            continue;
        };
        seen.insert(name.as_str());
        failed += usize::from(!emit(name, r, out)?);
    }
    for name in job.names.iter().filter(|n| !seen.contains(n.as_str())) {
        eprintln!("{name}: no result");
        failed += 1;
    }
    let cost = usage
        .and_then(|u| u.cost)
        .map(|c| format!(", cost ${c:.6}"))
        .unwrap_or_default();
    eprintln!(
        "{}: {} ok, {failed} failed{cost}",
        job.short,
        job.names.len() - failed
    );
    return Ok(if failed > 0 { 1 } else { 0 });
}

fn custom_id(i: usize) -> String {
    format!("r{i}")
}

fn set_status(jobs: &Jobs, job: &mut Job, status: &str) -> Result<()> {
    if job.status != status {
        job.status = status.to_string();
        jobs.put(job)?;
    }
    return Ok(());
}

/// One result: to `out/<name>` (`out/<name>.err` on error) or stdout. True = ok.
fn emit(name: &str, r: &BatchReply, out: Option<&Path>) -> Result<bool> {
    let err = match (&r.error, r.content.trim().is_empty()) {
        (Some(e), _) => Some(e.clone()),
        (None, true) => Some("empty reply".to_string()),
        (None, false) => None,
    };
    let Some(dir) = out else {
        match &err {
            Some(e) => eprintln!("{name}: {e}"),
            None => print!("{}", with_newline(&r.content)),
        }
        return Ok(err.is_none());
    };
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    match &err {
        Some(e) => {
            std::fs::write(dir.join(format!("{name}.err")), format!("{e}\n"))?;
            eprintln!("{name}: {e}");
        }
        None => std::fs::write(dir.join(name), with_newline(&r.content))?,
    }
    return Ok(err.is_none());
}

fn with_newline(s: &str) -> String {
    if s.ends_with('\n') {
        s.to_string()
    } else {
        format!("{s}\n")
    }
}

/// `llmjob ls`.
pub fn ls(ctx: &Ctx) -> Result<i32> {
    let jobs = Jobs::open(&ctx.data_dir)?;
    for j in jobs.all()? {
        let age = now_ms().saturating_sub(j.created_at) / 60_000;
        let age = match age {
            m if m < 60 => format!("{m}m"),
            m if m < 48 * 60 => format!("{}h", m / 60),
            m => format!("{}d", m / 1440),
        };
        println!(
            "{:<5} {:<11} {:>4} req  {:>4} ago  {}",
            j.short,
            j.status,
            j.names.len(),
            age,
            j.model
        );
    }
    return Ok(0);
}

/// What `rm` did with one job.
#[derive(Debug, PartialEq)]
enum Removed {
    Yes,
    Running,
    Unfetched,
}

/// `llmjob rm`: the named jobs, or all. Running batches are skipped (no
/// cancel exists); finished-but-unfetched ones only with `force`. Exit 1 if
/// anything was skipped.
pub async fn rm(ctx: &Ctx, qs: &[String], all: bool, force: bool) -> Result<i32> {
    let jobs = Jobs::open(&ctx.data_dir)?;
    // Resolve every id first, so a typo removes nothing.
    let targets = if all {
        jobs.all()?
    } else {
        qs.iter()
            .map(|q| jobs.find(q))
            .collect::<Result<Vec<_>>>()?
    };
    let (mut removed, mut running, mut unfetched, mut failed) = (0, 0, 0, 0);
    for job in &targets {
        match rm_one(ctx, &jobs, job, force).await {
            Ok(Removed::Yes) => removed += 1,
            Ok(Removed::Running) => {
                eprintln!("{}: still running, skipped (can't cancel)", job.short);
                running += 1;
            }
            Ok(Removed::Unfetched) => {
                eprintln!(
                    "{}: finished but not fetched, skipped (`get` it, or --force)",
                    job.short
                );
                unfetched += 1;
            }
            Err(e) => {
                eprintln!("{}: {e:#}", job.short);
                failed += 1;
            }
        }
    }
    eprintln!(
        "removed {removed}, skipped {running} running, {unfetched} unfetched, {failed} failed"
    );
    return Ok(if running + unfetched + failed > 0 {
        1
    } else {
        0
    });
}

async fn rm_one(ctx: &Ctx, jobs: &Jobs, job: &Job, force: bool) -> Result<Removed> {
    let cached = jobs.results(&job.short);
    if !cached.exists() {
        let key = ctx.config.key_for(job.key_name.as_deref())?;
        match ctx.provider.batch_raw(key, &job.batch_id).await? {
            None if now_ms().saturating_sub(job.created_at) < VISIBLE_AFTER_MS => {
                return Ok(Removed::Running);
            }
            // Gone upstream: nothing left to lose.
            None => {}
            Some(v) => match parse_batch_all(&v) {
                BatchState::Pending(_) => return Ok(Removed::Running),
                _ if !force => return Ok(Removed::Unfetched),
                _ => ctx.provider.delete_batch(key, &job.batch_id).await?,
            },
        }
    }
    jobs.remove(&job.short)?;
    return Ok(Removed::Yes);
}

/// Name of the request when there is no input dir.
pub fn single_name() -> String {
    SINGLE.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_by_prefix_and_last() {
        let d = std::env::temp_dir().join(format!("llmjob-jobs-{}", crate::store::new_id()));
        let jobs = Jobs::open(&d).unwrap();
        let job = |short: &str, at: u64| Job {
            short: short.into(),
            batch_id: format!("b-{short}"),
            model: "m:batch".into(),
            key_name: None,
            created_at: at,
            names: vec!["output".into()],
            status: "validating".into(),
        };
        jobs.put(&job("abc", 2)).unwrap();
        jobs.put(&job("abd", 1)).unwrap();
        assert_eq!(jobs.pending().unwrap().len(), 2);
        jobs.finish("abc", &serde_json::json!({})).unwrap();
        assert!(jobs.done_rec("abc").exists() && !jobs.pending_rec("abc").exists());
        let pending: Vec<String> = jobs
            .pending()
            .unwrap()
            .into_iter()
            .map(|j| j.short)
            .collect();
        assert_eq!(pending, ["abd"]);
        // Second finish (CLI and server both fetched): fine.
        jobs.finish("abc", &serde_json::json!({})).unwrap();
        // Crash between results and move: repaired by the next listing.
        std::fs::write(jobs.results("abd"), "{}").unwrap();
        assert!(jobs.pending().unwrap().is_empty());
        assert!(jobs.done_rec("abd").exists());
        assert_eq!(jobs.all().unwrap().len(), 2);
        assert_eq!(jobs.find("abc").unwrap().batch_id, "b-abc");
        assert_eq!(jobs.find("last").unwrap().short, "abc");
        assert!(jobs.find("ab").is_err());
        assert!(jobs.find("zz").is_err());
        let s = jobs.new_short();
        assert_eq!(s.len(), 3);
        assert!(!s.contains(['0', '1', 'i', 'l', 'o']));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn writes_outputs_and_errors() {
        let d = std::env::temp_dir().join(format!("llmjob-out-{}", crate::store::new_id()));
        let jobs = Jobs::open(&d).unwrap();
        let job = Job {
            short: "xyz".into(),
            batch_id: "b".into(),
            model: "m".into(),
            key_name: None,
            created_at: 0,
            names: vec!["a.txt".into(), "b.txt".into(), "c.txt".into()],
            status: "in_progress".into(),
        };
        jobs.put(&job).unwrap();
        let v = serde_json::json!({ "status": "completed", "results": [
            { "custom_id": "r0", "error": null, "response": { "body": {
                "choices": [{ "message": { "content": "A" } }] } } },
            { "custom_id": "r1", "error": { "message": "nope" } },
            { "custom_id": "r9", "error": null, "response": { "body": {
                "choices": [{ "message": { "content": "X" } }] } } }
        ]});
        let out = d.join("out");
        let code = write_results(&jobs, job, &v, Some(&out)).unwrap();
        assert_eq!(code, 1);
        assert_eq!(std::fs::read_to_string(out.join("a.txt")).unwrap(), "A\n");
        assert_eq!(
            std::fs::read_to_string(out.join("b.txt.err")).unwrap(),
            "nope\n"
        );
        assert!(!d.join("evil").exists());
        assert_eq!(jobs.find("xyz").unwrap().status, "completed");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
