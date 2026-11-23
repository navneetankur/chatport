//! Attachment blobs, shared by all chats.
//!
//! data_dir/attachments/<sha256>   content, written once, never modified
//! data_dir/attachments.jsonl      append-only log: one line per attach, pin, unpin or hide
//!
//! Messages store `{name, hash, size}`; the content is filled back in when a
//! chat is loaded. Old messages with inline `content` and no hash keep working.
//! Every line carries the full state of its hash, so the newest line per hash
//! is the truth: a re-attach copies `pinned` from the previous newest line and
//! clears `hidden`; pin/unpin/hide append a copy of the newest line with the
//! flag changed (keeping `at`, so they don't count as a use). Re-attaching touches the
//! blob's mtime, which `gc` uses as a grace period for blobs written moments
//! before their message is appended.

use crate::store::{Attachment, Store, now_ms};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

/// Blobs younger than this are never collected (their message may not be appended yet).
pub const GC_GRACE: Duration = Duration::from_secs(3600);

/// One line of attachments.jsonl, also a picker entry.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub hash: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default)]
    pub size: u64,
    /// When attached (ms).
    #[serde(default)]
    pub at: u64,
    /// Conversation id and its title at the time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_name: Option<String>,
    /// Server path for `@path` files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// Dropped from the picker until attached again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Picker {
    /// Most recently used first.
    pub pinned: Vec<Entry>,
    /// Most recently used first; pinned and hidden ones left out.
    pub recent: Vec<Entry>,
}

pub struct Attachments {
    dir: PathBuf,
    log: PathBuf,
    lock: Mutex<()>,
}

pub fn hash_of(content: &str) -> String {
    let d = Sha256::digest(content.as_bytes());
    let mut s = String::with_capacity(64);
    for b in d {
        s.push_str(&format!("{b:02x}"));
    }
    return s;
}

fn valid_hash(h: &str) -> bool {
    h.len() == 64
        && h.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Lines of a log, newest last. A broken line (e.g. a torn last write) is skipped.
fn parse_log(text: &str, path: &Path) -> Vec<Entry> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| match serde_json::from_str(l) {
            Ok(e) => Some(e),
            Err(e) => {
                log::warn!("{}: skipping bad line: {e}", path.display());
                None
            }
        })
        .collect()
}

/// Bytes read per step by `find_newest`.
const TAIL_CHUNK: u64 = 4096;

/// Newest line for `hash`, reading backwards `chunk` bytes at a time, so a
/// recently used file (near the end after compaction) costs one small read.
/// A line cut by a chunk boundary is carried into the next (earlier) step.
fn find_newest<F: Read + Seek>(f: &mut F, hash: &str, chunk: u64) -> Result<Option<Entry>> {
    let mut pos = f.seek(std::io::SeekFrom::End(0))?;
    let mut carry: Vec<u8> = Vec::new();
    while pos > 0 {
        let start = pos.saturating_sub(chunk);
        let mut buf = vec![0; (pos - start) as usize];
        f.seek(std::io::SeekFrom::Start(start))?;
        f.read_exact(&mut buf)?;
        buf.extend_from_slice(&carry);
        // Before the first newline may be the tail of a line starting earlier.
        let cut = match (start, buf.iter().position(|&b| b == b'\n')) {
            (0, _) => 0,
            (_, Some(i)) => i + 1,
            (_, None) => buf.len(),
        };
        for line in buf[cut..].split(|&b| b == b'\n').rev() {
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            match serde_json::from_slice::<Entry>(line) {
                Ok(e) if e.hash == hash => return Ok(Some(e)),
                Ok(_) => {}
                Err(e) => log::warn!("attachments log: skipping bad line: {e}"),
            }
        }
        buf.truncate(cut);
        carry = buf;
        pos = start;
    }
    return Ok(None);
}

/// Newest line per hash, most recently used first.
fn latest(lines: &[Entry]) -> Vec<Entry> {
    let mut seen = HashSet::new();
    let mut out: Vec<Entry> = lines
        .iter()
        .rev()
        .filter(|e| seen.insert(e.hash.as_str()))
        .cloned()
        .collect();
    out.sort_by_key(|e| std::cmp::Reverse(e.at));
    return out;
}

/// Newest line per hash among `keep`, in log order (oldest first).
fn compact(lines: &[Entry], keep: &HashSet<String>) -> Vec<Entry> {
    let mut seen = HashSet::new();
    let mut out: Vec<Entry> = lines
        .iter()
        .rev()
        .filter(|e| keep.contains(&e.hash) && seen.insert(e.hash.clone()))
        .cloned()
        .collect();
    out.reverse();
    return out;
}

fn to_lines(entries: &[Entry]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for e in entries {
        out.extend(serde_json::to_vec(e)?);
        out.push(b'\n');
    }
    return Ok(out);
}

impl Attachments {
    pub fn open(data_dir: &Path) -> Result<Self> {
        let dir = data_dir.join("attachments");
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        return Ok(Self {
            dir,
            log: data_dir.join("attachments.jsonl"),
            lock: Mutex::new(()),
        });
    }

    fn blob(&self, hash: &str) -> PathBuf {
        self.dir.join(hash)
    }

    /// Content of a blob.
    pub fn read(&self, hash: &str) -> Result<String> {
        if !valid_hash(hash) {
            bail!("invalid attachment hash {hash:?}");
        }
        let p = self.blob(hash);
        return std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()));
    }

    /// Writes the blob (or touches an existing one) and logs the use. Sets
    /// `a.hash` and `a.size`; `a.content` is kept. An attachment with only a
    /// hash (picked from the list) gets its content from the blob first.
    pub fn save(&self, a: &mut Attachment, chat: &str, chat_name: &str) -> Result<()> {
        if a.content.is_empty() {
            let Some(h) = a.hash.clone() else {
                bail!("attachment {:?} has neither content nor hash", a.name);
            };
            a.content = self
                .read(&h)
                .with_context(|| format!("attachment {:?}", a.name))?;
        }
        let hash = hash_of(&a.content);
        let p = self.blob(&hash);
        let _g = self.lock.lock().unwrap();
        let mut pinned = false;
        if p.exists() {
            pinned = self.newest(&hash)?.is_some_and(|e| e.pinned);
            // Fresh mtime keeps gc's grace period meaning "recently used".
            std::fs::File::options()
                .write(true)
                .open(&p)
                .and_then(|f| f.set_modified(SystemTime::now()))
                .with_context(|| format!("touching {}", p.display()))?;
        } else {
            let tmp = self.dir.join(format!("{hash}.tmp"));
            std::fs::write(&tmp, &a.content)
                .with_context(|| format!("writing {}", tmp.display()))?;
            std::fs::rename(&tmp, &p).with_context(|| format!("renaming to {}", p.display()))?;
            log::info!(
                "new attachment blob {hash} ({:?}, {} bytes)",
                a.name,
                a.content.len()
            );
        }
        a.hash = Some(hash.clone());
        a.size = Some(a.content.len() as u64);
        a.missing = false;
        self.append(&Entry {
            hash,
            name: a.name.clone(),
            size: a.content.len() as u64,
            at: now_ms(),
            chat: Some(chat.to_string()),
            chat_name: Some(chat_name.to_string()),
            path: a.path.clone(),
            pinned,
            hidden: false,
        })?;
        return Ok(());
    }

    /// Fills in `content` of a hash-only attachment. A missing blob is logged
    /// as an error and marked `missing`; the send path refuses such messages.
    pub fn hydrate(&self, a: &mut Attachment) {
        let Some(h) = &a.hash else {
            return;
        };
        if !a.content.is_empty() {
            return;
        }
        match self.read(h) {
            Ok(c) => a.content = c,
            Err(e) => {
                log::error!("attachment {:?} ({h}) is missing: {e:#}", a.name);
                a.missing = true;
            }
        }
    }

    fn append(&self, e: &Entry) -> Result<()> {
        let mut line = serde_json::to_vec(e)?;
        line.push(b'\n');
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .and_then(|mut f| f.write_all(&line))
            .with_context(|| format!("appending to {}", self.log.display()))?;
        return Ok(());
    }

    fn lines(&self) -> Result<Vec<Entry>> {
        match std::fs::read_to_string(&self.log) {
            Ok(t) => Ok(parse_log(&t, &self.log)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
            Err(e) => Err(e).with_context(|| format!("reading {}", self.log.display())),
        }
    }

    /// Newest line for `hash`, if any.
    fn newest(&self, hash: &str) -> Result<Option<Entry>> {
        let mut f = match std::fs::File::open(&self.log) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("opening {}", self.log.display())),
        };
        return find_newest(&mut f, hash, TAIL_CHUNK)
            .with_context(|| format!("reading {}", self.log.display()));
    }

    pub fn picker(&self) -> Result<Picker> {
        let (mut pinned, mut recent) = (Vec::new(), Vec::new());
        for e in latest(&self.lines()?) {
            if e.hidden || !self.blob(&e.hash).exists() {
                continue;
            }
            if e.pinned { &mut pinned } else { &mut recent }.push(e);
        }
        return Ok(Picker { pinned, recent });
    }

    /// Appends a copy of the newest line for `hash` with `change` applied,
    /// unless that changes nothing. Err for a hash never attached.
    fn restate(&self, hash: &str, change: impl FnOnce(&mut Entry)) -> Result<()> {
        let _g = self.lock.lock().unwrap();
        let Some(old) = self.newest(hash)? else {
            bail!("unknown attachment {hash}");
        };
        let mut e = old.clone();
        change(&mut e);
        if e == old {
            return Ok(());
        }
        return self.append(&e);
    }

    /// Pinning a hidden file un-hides it.
    pub fn pin(&self, hash: &str) -> Result<()> {
        self.restate(hash, |e| {
            e.pinned = true;
            e.hidden = false;
        })
    }

    pub fn unpin(&self, hash: &str) -> Result<()> {
        self.restate(hash, |e| e.pinned = false)
    }

    /// Drops `hash` from the picker (unpinning it too) until it is attached again.
    pub fn hide(&self, hash: &str) -> Result<()> {
        self.restate(hash, |e| {
            e.pinned = false;
            e.hidden = true;
        })
    }

    /// Lists orphan blobs (no message refers to them, not pinned); with `delete`,
    /// removes them and compacts the log. Prints a report to stdout.
    pub fn gc(&self, store: &Store, delete: bool) -> Result<()> {
        let mut live: HashSet<String> = HashSet::new();
        for s in store.list()? {
            for m in store.messages(&s.id)? {
                live.extend(m.attachments.into_iter().filter_map(|a| a.hash));
            }
        }
        live.extend(
            latest(&self.lines()?)
                .into_iter()
                .filter(|e| e.pinned)
                .map(|e| e.hash),
        );

        let now = SystemTime::now();
        let (mut orphans, mut orphan_bytes, mut skipped, mut kept) =
            (Vec::new(), 0u64, 0, HashSet::new());
        for e in std::fs::read_dir(&self.dir).with_context(|| format!("{}", self.dir.display()))? {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            if !valid_hash(&name) {
                println!("ignoring {} (not a blob)", e.path().display());
                continue;
            }
            if live.contains(&name) {
                kept.insert(name);
                continue;
            }
            let md = e.metadata()?;
            let age = now.duration_since(md.modified()?).unwrap_or_default();
            if age < GC_GRACE {
                println!(
                    "skipped {name} ({} min old, too recent)",
                    age.as_secs() / 60
                );
                skipped += 1;
                kept.insert(name);
                continue;
            }
            orphan_bytes += md.len();
            orphans.push((name, md.len()));
        }
        for (h, size) in &orphans {
            println!("orphan  {h} ({size} bytes)");
        }
        println!(
            "{} orphans ({:.1} KB), {} skipped as recent, {} in use",
            orphans.len(),
            orphan_bytes as f64 / 1024.0,
            skipped,
            live.len()
        );
        if !delete {
            println!(
                "dry run: pass --delete to remove orphans and compact {}",
                self.log.display()
            );
            return Ok(());
        }
        for (h, _) in &orphans {
            let p = self.blob(h);
            std::fs::remove_file(&p).with_context(|| format!("removing {}", p.display()))?;
        }
        self.compact_log(&kept)?;
        return Ok(());
    }

    /// Rewrites the log keeping the newest line per hash in `keep`. Lines a
    /// running server appends meanwhile are copied over before the rename.
    fn compact_log(&self, keep: &HashSet<String>) -> Result<()> {
        let mut f = match std::fs::File::open(&self.log) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e).with_context(|| format!("opening {}", self.log.display())),
        };
        let mut text = String::new();
        f.read_to_string(&mut text)?;
        let lines = parse_log(&text, &self.log);
        let out = compact(&lines, keep);
        let tmp = self.log.with_extension("jsonl.tmp");
        let mut bytes = to_lines(&out)?;
        // Appended since we read: keep verbatim.
        f.seek(std::io::SeekFrom::Start(text.len() as u64))?;
        f.read_to_end(&mut bytes)?;
        std::fs::write(&tmp, &bytes).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.log)
            .with_context(|| format!("renaming to {}", self.log.display()))?;
        println!(
            "compacted {}: {} → {} lines",
            self.log.display(),
            lines.len(),
            out.len()
        );
        return Ok(());
    }
}

#[cfg(test)]
mod tests;
