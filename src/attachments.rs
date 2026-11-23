//! Attachment blobs, shared by all chats.
//!
//! data_dir/attachments/<sha256>   content, written once, never modified
//! data_dir/attachments.jsonl      append-only use log (one line per attach, or a hide)
//! data_dir/pinned.json            pinned picker entries, newest pin first
//!
//! Messages store `{name, hash, size}`; the content is filled back in when a
//! chat is loaded. Old messages with inline `content` and no hash keep working.
//! The picker reads the log from the end: the newest line per hash wins, so a
//! re-attach moves a file to the top and un-hides it. Re-attaching touches the
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
    /// Hide marker: drops the hash from the picker until it is attached again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Picker {
    pub pinned: Vec<Entry>,
    /// Newest first, pinned and hidden ones left out.
    pub recent: Vec<Entry>,
}

pub struct Attachments {
    dir: PathBuf,
    log: PathBuf,
    pinned: PathBuf,
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

/// Newest line per hash, newest first. Hidden hashes and `skip` are left out.
fn latest(lines: &[Entry], skip: &HashSet<&str>) -> Vec<Entry> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for e in lines.iter().rev() {
        if !seen.insert(e.hash.as_str()) {
            continue;
        }
        if !e.hidden && !skip.contains(e.hash.as_str()) {
            out.push(e.clone());
        }
    }
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
            pinned: data_dir.join("pinned.json"),
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
        if p.exists() {
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

    fn read_pinned(&self) -> Result<Vec<Entry>> {
        match std::fs::read(&self.pinned) {
            Ok(b) => serde_json::from_slice(&b)
                .with_context(|| format!("parsing {}", self.pinned.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
            Err(e) => Err(e).with_context(|| format!("reading {}", self.pinned.display())),
        }
    }

    fn write_pinned(&self, list: &[Entry]) -> Result<()> {
        let tmp = self.pinned.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(list)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.pinned)
            .with_context(|| format!("renaming to {}", self.pinned.display()))?;
        return Ok(());
    }

    pub fn picker(&self) -> Result<Picker> {
        let pinned = self.read_pinned()?;
        let skip: HashSet<&str> = pinned.iter().map(|e| e.hash.as_str()).collect();
        let recent = latest(&self.lines()?, &skip)
            .into_iter()
            .filter(|e| self.blob(&e.hash).exists())
            .collect();
        return Ok(Picker { pinned, recent });
    }

    /// Pins `hash` with its newest log entry. Already pinned is fine.
    pub fn pin(&self, hash: &str) -> Result<()> {
        let _g = self.lock.lock().unwrap();
        let mut list = self.read_pinned()?;
        if list.iter().any(|e| e.hash == hash) {
            return Ok(());
        }
        let Some(e) = self
            .lines()?
            .into_iter()
            .rev()
            .find(|e| e.hash == hash && !e.hidden)
        else {
            bail!("unknown attachment {hash}");
        };
        list.insert(0, e);
        return self.write_pinned(&list);
    }

    /// Unpins `hash`. Returns whether it was pinned.
    pub fn unpin(&self, hash: &str) -> Result<bool> {
        let _g = self.lock.lock().unwrap();
        let mut list = self.read_pinned()?;
        let n = list.len();
        list.retain(|e| e.hash != hash);
        if list.len() == n {
            return Ok(false);
        }
        self.write_pinned(&list)?;
        return Ok(true);
    }

    /// Drops `hash` from the picker (unpinning it too) until it is attached again.
    pub fn hide(&self, hash: &str) -> Result<()> {
        if !valid_hash(hash) {
            bail!("invalid attachment hash {hash:?}");
        }
        self.unpin(hash)?;
        let _g = self.lock.lock().unwrap();
        return self.append(&Entry {
            hash: hash.to_string(),
            name: String::new(),
            size: 0,
            at: now_ms(),
            chat: None,
            chat_name: None,
            path: None,
            hidden: true,
        });
    }

    /// Lists orphan blobs (no message or pin refers to them); with `delete`,
    /// removes them and compacts the log. Prints a report to stdout.
    pub fn gc(&self, store: &Store, delete: bool) -> Result<()> {
        let mut live: HashSet<String> = HashSet::new();
        for s in store.list()? {
            for m in store.messages(&s.id)? {
                live.extend(m.attachments.into_iter().filter_map(|a| a.hash));
            }
        }
        live.extend(self.read_pinned()?.into_iter().map(|e| e.hash));

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
