//! File tools the model can call: list / read / write / edit text files inside
//! the conversation's file root. Writes are applied directly; each result
//! carries a unified diff so the change can be seen (and reversed by hand).

use crate::config::expand_tilde;
use crate::files::MAX_BYTES;
use crate::provider::{ToolCall, ToolSpec};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};

/// Entries returned by `list_dir` at most.
const MAX_LIST: usize = 500;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolResult {
    pub call_id: String,
    /// Text sent back to the model.
    pub output: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub error: bool,
    /// Unified diff of a write/edit (UI only, not sent to the model).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

pub fn specs() -> Vec<ToolSpec> {
    let path = json!({ "type": "string", "description": "Path relative to the root directory" });
    vec![
        ToolSpec {
            name: "list_dir".into(),
            description: "List entries of a directory (directories end with '/').".into(),
            parameters: json!({
                "type": "object",
                "properties": { "path": { "type": "string", "description": "Directory relative to the root; default \".\"" } },
            }),
        },
        ToolSpec {
            name: "read_file".into(),
            description: "Read a UTF-8 text file. Optional 1-based inclusive line range.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": path,
                    "start_line": { "type": "integer" },
                    "end_line": { "type": "integer" },
                },
                "required": ["path"],
            }),
        },
        ToolSpec {
            name: "write_file".into(),
            description: "Create or overwrite a text file with the given content. Creates parent directories.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "path": path, "content": { "type": "string" } },
                "required": ["path", "content"],
            }),
        },
        ToolSpec {
            name: "edit_file".into(),
            description: "Replace one exact occurrence of old_string with new_string in a text file. old_string must occur exactly once; include enough context to make it unique.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": path,
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                },
                "required": ["path", "old_string", "new_string"],
            }),
        },
    ]
}

/// Runs one call. Failures become an error result for the model to see.
pub fn run(root: &str, call: &ToolCall) -> ToolResult {
    let res = serde_json::from_str::<Value>(if call.arguments.trim().is_empty() {
        "{}"
    } else {
        &call.arguments
    })
    .context("arguments are not valid JSON")
    .and_then(|args| dispatch(&expand_tilde(root.trim()), &call.name, &args));
    let out = match res {
        Ok((output, diff)) => ToolResult {
            call_id: call.id.clone(),
            output,
            error: false,
            diff,
        },
        Err(e) => ToolResult {
            call_id: call.id.clone(),
            output: format!("error: {e:#}"),
            error: true,
            diff: None,
        },
    };
    tracing::info!(
        "tool {}({}) error={}",
        call.name,
        short(&call.arguments),
        out.error
    );
    return out;
}

fn short(s: &str) -> String {
    let mut t: String = s.chars().take(120).collect();
    if t.len() < s.len() {
        t.push('…');
    }
    t
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("missing string argument `{key}`"))
}

fn dispatch(root: &Path, name: &str, args: &Value) -> Result<(String, Option<String>)> {
    match name {
        "list_dir" => {
            let p = args.get("path").and_then(Value::as_str).unwrap_or(".");
            Ok((list_dir(&jail(root, p)?)?, None))
        }
        "read_file" => {
            let p = jail(root, str_arg(args, "path")?)?;
            let line = |k: &str| args.get(k).and_then(Value::as_u64).map(|n| n as usize);
            Ok((read_file(&p, line("start_line"), line("end_line"))?, None))
        }
        "write_file" => {
            let rel = str_arg(args, "path")?;
            let (out, diff) = write_file(&jail(root, rel)?, rel, str_arg(args, "content")?)?;
            Ok((out, Some(diff)))
        }
        "edit_file" => {
            let rel = str_arg(args, "path")?;
            let (out, diff) = edit_file(
                &jail(root, rel)?,
                rel,
                str_arg(args, "old_string")?,
                str_arg(args, "new_string")?,
            )?;
            Ok((out, Some(diff)))
        }
        _ => bail!("unknown tool `{name}`"),
    }
}

/// `rel` joined to `root`, refusing anything that ends up outside it:
/// absolute paths, `..`, and symlinks pointing out (checked on the deepest
/// existing ancestor, so not-yet-created files work).
pub fn jail(root: &Path, rel: &str) -> Result<PathBuf> {
    let rel_path = Path::new(rel);
    for c in rel_path.components() {
        match c {
            Component::Normal(_) | Component::CurDir => {}
            _ => bail!("path must be relative to the root, without `..`: {rel}"),
        }
    }
    let root = root
        .canonicalize()
        .with_context(|| format!("file root {}", root.display()))?;
    let full = root.join(rel_path);
    let mut existing = full.as_path();
    while !existing.exists() {
        existing = existing.parent().context("no existing ancestor")?;
    }
    if !existing.canonicalize()?.starts_with(&root) {
        bail!("{rel} is outside the file root");
    }
    return Ok(full);
}

fn list_dir(dir: &Path) -> Result<String> {
    let mut names = Vec::new();
    for e in std::fs::read_dir(dir).with_context(|| format!("{}", dir.display()))? {
        let e = e?;
        let mut n = e.file_name().to_string_lossy().into_owned();
        if std::fs::metadata(e.path()).is_ok_and(|m| m.is_dir()) {
            n.push('/');
        }
        names.push(n);
    }
    names.sort();
    let total = names.len();
    names.truncate(MAX_LIST);
    let mut out = names.join("\n");
    if total > MAX_LIST {
        out.push_str(&format!("\n… ({} more)", total - MAX_LIST));
    }
    if out.is_empty() {
        out = "(empty)".into();
    }
    return Ok(out);
}

/// Existing text file content, or None if it doesn't exist.
fn read_existing(p: &Path) -> Result<Option<String>> {
    if !p.exists() {
        return Ok(None);
    }
    let a = crate::files::read_text(p, "")?;
    return Ok(Some(a.content));
}

fn read_file(p: &Path, start: Option<usize>, end: Option<usize>) -> Result<String> {
    let text = read_existing(p)?.with_context(|| format!("{} does not exist", p.display()))?;
    if start.is_none() && end.is_none() {
        return Ok(text);
    }
    let lines: Vec<&str> = text.lines().collect();
    let s = start.unwrap_or(1).max(1);
    let e = end.unwrap_or(lines.len()).min(lines.len());
    if s > e {
        return Ok(format!(
            "(no lines in range; file has {} lines)",
            lines.len()
        ));
    }
    return Ok(lines[s - 1..e].join("\n"));
}

fn check_size(content: &str) -> Result<()> {
    if content.len() as u64 > MAX_BYTES {
        bail!(
            "content too large ({} KB, max {} KB)",
            content.len() / 1024,
            MAX_BYTES / 1024
        );
    }
    Ok(())
}

fn unified(rel: &str, old: &str, new: &str) -> String {
    similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{rel}"), &format!("b/{rel}"))
        .to_string()
}

fn write_file(p: &Path, rel: &str, content: &str) -> Result<(String, String)> {
    check_size(content)?;
    if p.is_dir() {
        bail!("{rel} is a directory");
    }
    let old = read_existing(p)?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(p, content).with_context(|| format!("writing {}", p.display()))?;
    let verb = if old.is_some() { "updated" } else { "created" };
    let diff = unified(rel, old.as_deref().unwrap_or(""), content);
    return Ok((format!("{verb} {rel} ({} bytes)", content.len()), diff));
}

fn edit_file(p: &Path, rel: &str, old_s: &str, new_s: &str) -> Result<(String, String)> {
    if old_s.is_empty() {
        bail!("old_string is empty; use write_file to create a file");
    }
    let text = read_existing(p)?.with_context(|| format!("{rel} does not exist"))?;
    match text.matches(old_s).count() {
        1 => {}
        0 => bail!("old_string not found in {rel}"),
        n => bail!("old_string occurs {n} times in {rel}; add context to make it unique"),
    }
    let new_text = text.replacen(old_s, new_s, 1);
    check_size(&new_text)?;
    std::fs::write(p, &new_text).with_context(|| format!("writing {}", p.display()))?;
    return Ok((format!("edited {rel}"), unified(rel, &text, &new_text)));
}

#[cfg(test)]
mod tests;
