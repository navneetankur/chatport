//! On-disk storage.
//!
//! data_dir/conversations/<group>/<id>-<label>/meta.json      mutable settings, rewritten atomically
//! data_dir/conversations/<group>/<id>-<label>/messages.jsonl append-only message tree
//!
//! `<group>` is the group name, or `_ungrouped`. `<label>` is the title made
//! safe for a folder name, so listing chats needs no file reads. A chat's
//! folder is found by its `<id>-` prefix; the path is looked up again on
//! every operation, and moves happen under the write lock, so a move during
//! a streaming reply is safe. A read racing a move may fail once.

use crate::provider::{CachePolicy, ReasoningEffort, Role, ToolCall, Usage};
use crate::tools::ToolResult;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
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
    /// Name of the config API key to use. None = config default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_name: Option<String>,
    /// Directory `@relative/path` attachments resolve against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_root: Option<String>,
    /// Offer file tools (inside `file_root`) to the model.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tools: bool,
    /// Currently shown branch tip. None = last appended message.
    #[serde(default)]
    pub leaf: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Attachment {
    pub name: String,
    pub content: String,
}

/// One model request that ended in tool calls, plus their results.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolRound {
    /// Text the model wrote before calling the tools.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content: String,
    pub calls: Vec<ToolCall>,
    pub results: Vec<ToolResult>,
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
    /// Assistant: tool rounds before the final `content`, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_rounds: Vec<ToolRound>,
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
    /// Non-fatal problem, e.g. the model called tools while they were off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
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
            tool_rounds: vec![],
            model: None,
            requested_model: None,
            provider_name: None,
            usage: None,
            partial: false,
            error: None,
            warning: None,
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

/// Folder holding chats that are in no group. Not allowed as a group name.
pub const UNGROUPED: &str = "_ungrouped";
const LABEL_MAX_BYTES: usize = 150;
const GROUP_MAX_BYTES: usize = 100;

/// One sidebar row, built from folder names and file times only.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Summary {
    pub id: String,
    /// None = ungrouped.
    pub group: Option<String>,
    pub title: String,
    /// mtime of messages.jsonl (meta.json for a chat with no messages), ms.
    pub updated_at: u64,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Check a user-chosen group name.
pub fn check_group(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        bail!("group name is empty");
    }
    if name != name.trim() {
        bail!("group name has leading or trailing spaces");
    }
    if name.len() > GROUP_MAX_BYTES {
        bail!("group name is longer than {GROUP_MAX_BYTES} bytes");
    }
    if name.starts_with('.') {
        bail!("group name can't start with '.'");
    }
    if name == UNGROUPED {
        bail!("'{UNGROUPED}' is reserved");
    }
    if name
        .chars()
        .any(|c| c == '/' || c == '\\' || c.is_control())
    {
        bail!("group name can't contain '/', '\\' or control characters");
    }
    Ok(())
}

/// Title as a folder-name part: '/' becomes '∕', control chars become spaces,
/// cut to LABEL_MAX_BYTES on a char boundary.
pub fn label_for(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        let c = match c {
            '/' => '∕',
            c if c.is_control() => ' ',
            c => c,
        };
        if out.len() + c.len_utf8() > LABEL_MAX_BYTES {
            break;
        }
        out.push(c);
    }
    return out;
}

fn folder_name(id: &str, title: &str) -> String {
    format!("{id}-{}", label_for(title))
}

fn mtime_ms(path: &Path) -> Option<u64> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as u64)
}

fn is_not_found(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound
}

/// Remove `dir` if it is an empty group folder. Non-empty is fine.
fn remove_if_empty(dir: &Path) {
    if dir.file_name().is_some_and(|n| n == UNGROUPED) {
        return;
    }
    match std::fs::remove_dir(dir) {
        Ok(()) => log::info!("removed empty group {}", dir.display()),
        Err(e) if e.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
        Err(e) => log::warn!("removing {}: {e}", dir.display()),
    }
}

impl Store {
    pub fn open(data_dir: PathBuf) -> Result<Self> {
        let root = data_dir.join("conversations");
        std::fs::create_dir_all(root.join(UNGROUPED))
            .with_context(|| format!("creating {}", root.display()))?;
        let store = Self {
            root,
            write: Mutex::new(()),
        };
        store.migrate()?;
        return Ok(store);
    }

    /// Old layout `conversations/<id>/` -> `_ungrouped/<id>-<label>/`.
    fn migrate(&self) -> Result<()> {
        for e in std::fs::read_dir(&self.root)? {
            let path = e?.path();
            let meta_path = path.join("meta.json");
            if !meta_path.is_file() {
                continue;
            }
            let meta: Meta = serde_json::from_slice(&std::fs::read(&meta_path)?)
                .with_context(|| format!("migrating {}", meta_path.display()))?;
            let to = self
                .root
                .join(UNGROUPED)
                .join(folder_name(&meta.id, &meta.title));
            std::fs::rename(&path, &to)
                .with_context(|| format!("moving {} to {}", path.display(), to.display()))?;
            log::info!("migrated {} -> {}", path.display(), to.display());
        }
        Ok(())
    }

    /// Group folder names, including UNGROUPED.
    fn group_dirs(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(&self.root)? {
            let e = e?;
            let Ok(name) = e.file_name().into_string() else {
                continue;
            };
            if !name.starts_with('.') && e.file_type()?.is_dir() {
                out.push(name);
            }
        }
        Ok(out)
    }

    /// Current folder of a chat, or None.
    fn find(&self, id: &str) -> Result<Option<PathBuf>> {
        if !valid_id(id) {
            bail!("invalid conversation id");
        }
        let prefix = format!("{id}-");
        for g in self.group_dirs()? {
            for e in std::fs::read_dir(self.root.join(&g))? {
                let e = e?;
                if e.file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with(&prefix))
                {
                    return Ok(Some(e.path()));
                }
            }
        }
        Ok(None)
    }

    fn dir(&self, id: &str) -> Result<PathBuf> {
        self.find(id)?
            .with_context(|| format!("conversation {id} does not exist"))
    }

    pub fn list(&self) -> Result<Vec<Summary>> {
        let mut out = Vec::new();
        for g in self.group_dirs()? {
            let group = (g != UNGROUPED).then(|| g.clone());
            for e in std::fs::read_dir(self.root.join(&g))? {
                let e = e?;
                let Ok(name) = e.file_name().into_string() else {
                    continue;
                };
                let Some((id, label)) = name.split_once('-').filter(|(id, _)| valid_id(id)) else {
                    log::warn!("skipping {}: not <id>-<label>", e.path().display());
                    continue;
                };
                let path = e.path();
                let updated_at = mtime_ms(&path.join("messages.jsonl"))
                    .or_else(|| mtime_ms(&path.join("meta.json")))
                    .unwrap_or(0);
                out.push(Summary {
                    id: id.to_string(),
                    group: group.clone(),
                    title: label.to_string(),
                    updated_at,
                });
            }
        }
        out.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        Ok(out)
    }

    /// Group of a chat (None = ungrouped). Err if the chat doesn't exist.
    pub fn group_of(&self, id: &str) -> Result<Option<String>> {
        let dir = self.dir(id)?;
        let g = dir
            .parent()
            .and_then(Path::file_name)
            .and_then(|n| n.to_str())
            .context("chat folder has no group")?;
        Ok((g != UNGROUPED).then(|| g.to_string()))
    }

    pub fn meta(&self, id: &str) -> Result<Option<Meta>> {
        let Some(dir) = self.find(id)? else {
            return Ok(None);
        };
        match std::fs::read(dir.join("meta.json")) {
            Ok(b) => Ok(Some(serde_json::from_slice(&b)?)),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Create or replace meta.json atomically (write tmp + rename). A new chat
    /// goes to UNGROUPED; a changed title renames the folder.
    pub fn put_meta(&self, meta: &Meta) -> Result<()> {
        let _g = self.write.lock().unwrap();
        let name = folder_name(&meta.id, &meta.title);
        let dir = match self.find(&meta.id)? {
            Some(old) => {
                let new = old.with_file_name(&name);
                if new != old {
                    std::fs::rename(&old, &new)
                        .with_context(|| format!("renaming {}", old.display()))?;
                }
                new
            }
            None => {
                let new = self.root.join(UNGROUPED).join(&name);
                std::fs::create_dir_all(&new)?;
                new
            }
        };
        let tmp = dir.join("meta.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(meta)?)?;
        std::fs::rename(tmp, dir.join("meta.json"))?;
        Ok(())
    }

    pub fn messages(&self, id: &str) -> Result<Vec<Message>> {
        let Some(dir) = self.find(id)? else {
            return Ok(vec![]);
        };
        let path = dir.join("messages.jsonl");
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if is_not_found(&e) => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        Ok(text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| match serde_json::from_str(l) {
                Ok(m) => Some(m),
                Err(e) => {
                    log::warn!("{}: skipping bad line: {e}", path.display());
                    None
                }
            })
            .collect())
    }

    pub fn append(&self, id: &str, msg: &Message) -> Result<()> {
        let _g = self.write.lock().unwrap();
        let dir = self.dir(id)?;
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
        let _g = self.write.lock().unwrap();
        let Some(dir) = self.find(id)? else {
            return Ok(false);
        };
        std::fs::remove_dir_all(&dir)?;
        if let Some(parent) = dir.parent() {
            remove_if_empty(parent);
        }
        Ok(true)
    }

    /// Move a chat into `group` (created if needed), or ungroup it with None.
    /// The old group folder is removed when it becomes empty.
    pub fn move_to(&self, id: &str, group: Option<&str>) -> Result<()> {
        if let Some(g) = group {
            check_group(g)?;
        }
        let _g = self.write.lock().unwrap();
        let old = self.dir(id)?;
        let target_dir = self.root.join(group.unwrap_or(UNGROUPED));
        let new = target_dir.join(old.file_name().context("chat folder has no name")?);
        if new == old {
            return Ok(());
        }
        std::fs::create_dir_all(&target_dir)?;
        std::fs::rename(&old, &new).with_context(|| format!("moving {}", old.display()))?;
        if let Some(parent) = old.parent() {
            remove_if_empty(parent);
        }
        Ok(())
    }

    pub fn rename_group(&self, old: &str, new: &str) -> Result<()> {
        check_group(old)?;
        check_group(new)?;
        let _g = self.write.lock().unwrap();
        let (from, to) = (self.root.join(old), self.root.join(new));
        if !from.is_dir() {
            bail!("group {old:?} does not exist");
        }
        if to.exists() {
            bail!("group {new:?} already exists");
        }
        std::fs::rename(&from, &to)?;
        Ok(())
    }

    /// Delete a group. `with_chats` deletes its chats too, otherwise they
    /// move to UNGROUPED.
    pub fn delete_group(&self, name: &str, with_chats: bool) -> Result<()> {
        check_group(name)?;
        let _g = self.write.lock().unwrap();
        let dir = self.root.join(name);
        if !dir.is_dir() {
            bail!("group {name:?} does not exist");
        }
        if with_chats {
            std::fs::remove_dir_all(&dir)?;
            return Ok(());
        }
        for e in std::fs::read_dir(&dir)? {
            let e = e?;
            let to = self.root.join(UNGROUPED).join(e.file_name());
            std::fs::rename(e.path(), &to)
                .with_context(|| format!("moving {}", e.path().display()))?;
        }
        std::fs::remove_dir(&dir)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
