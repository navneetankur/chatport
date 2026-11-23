//! `llmjob batch` / `get` / `ls` / `rm`: OpenRouter batches with one request
//! per input file.
//!
//! data_dir/llmjob/jobs/<short>.json          job record (batch id, names, status)
//! data_dir/llmjob/jobs/<short>.results.json  raw final batch, once finished
//!
//! Once the final batch is cached locally the upstream copy is deleted, so
//! `get` can be repeated offline.

use super::Ctx;
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
    dir: PathBuf,
}

impl Jobs {
    pub fn open(data_dir: &Path) -> Result<Self> {
        let dir = data_dir.join("llmjob").join("jobs");
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        return Ok(Self { dir });
    }

    fn record(&self, short: &str) -> PathBuf {
        self.dir.join(format!("{short}.json"))
    }

    fn results(&self, short: &str) -> PathBuf {
        self.dir.join(format!("{short}.results.json"))
    }

    pub fn put(&self, job: &Job) -> Result<()> {
        let tmp = self.dir.join(format!("{}.json.tmp", job.short));
        std::fs::write(&tmp, serde_json::to_vec_pretty(job)?)?;
        std::fs::rename(&tmp, self.record(&job.short))?;
        return Ok(());
    }

    pub fn all(&self) -> Result<Vec<Job>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(&self.dir)? {
            let name = e?.file_name();
            let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".json")) else {
                continue;
            };
            if stem.contains('.') {
                continue; // results / tmp
            }
            match self.load(stem) {
                Ok(j) => out.push(j),
                Err(e) => log::warn!("skipping job {stem}: {e:#}"),
            }
        }
        out.sort_by_key(|j| j.created_at);
        return Ok(out);
    }

    fn load(&self, short: &str) -> Result<Job> {
        let b = std::fs::read(self.record(short))?;
        return Ok(serde_json::from_slice(&b)?);
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
                if !self.record(&s).exists() {
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
    let key = ctx.config.key_for(job.key_name.as_deref())?.to_string();
    let cached = jobs.results(&job.short);
    let v: Value = loop {
        if let Ok(b) = std::fs::read(&cached) {
            break serde_json::from_slice(&b).context("reading cached results")?;
        }
        let (status, progress) = match ctx.provider.batch_raw(&key, &job.batch_id).await? {
            Some(v) => match parse_batch_all(&v) {
                BatchState::Pending(info) => {
                    let counts = &v["request_counts"];
                    let progress = format!(
                        "{}: {} ({}/{} done, {} failed)",
                        job.short,
                        info.status,
                        counts["completed"],
                        counts["total"],
                        counts["failed"]
                    );
                    (info.status, progress)
                }
                _ => {
                    std::fs::write(&cached, serde_json::to_vec(&v)?)?;
                    // Results are kept locally now; free the upstream copy.
                    if let Err(e) = ctx.provider.delete_batch(&key, &job.batch_id).await {
                        log::warn!("deleting batch {} upstream: {e:#}", job.batch_id);
                    }
                    break v;
                }
            },
            None if now_ms().saturating_sub(job.created_at) < VISIBLE_AFTER_MS => {
                ("submitted".to_string(), format!("{}: submitted", job.short))
            }
            None => bail!(
                "batch {} not found upstream (deleted, or older than 30 days)",
                job.batch_id
            ),
        };
        eprintln!("{progress}");
        if status != job.status {
            job.status = status;
            jobs.put(&job)?;
        }
        if !wait {
            return Ok(EXIT_PENDING);
        }
        tokio::time::sleep(WAIT_GAP).await;
    };
    return write_results(&jobs, job, &v, out);
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

/// `llmjob rm`: deletes upstream (unless already gone) and the local record.
pub async fn rm(ctx: &Ctx, q: &str) -> Result<i32> {
    let jobs = Jobs::open(&ctx.data_dir)?;
    let job = jobs.find(q)?;
    let cached = jobs.results(&job.short);
    if !cached.exists() {
        let key = ctx.config.key_for(job.key_name.as_deref())?;
        ctx.provider
            .delete_batch(key, &job.batch_id)
            .await
            .context("deleting upstream (a running batch can't be cancelled or deleted)")?;
    }
    if let Err(e) = std::fs::remove_file(&cached)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        return Err(e.into());
    }
    std::fs::remove_file(jobs.record(&job.short))?;
    eprintln!("removed {}", job.short);
    return Ok(0);
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
        std::fs::write(jobs.results("abc"), "{}").unwrap();
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
