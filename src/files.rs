//! Server-side files referenced as `@path` in the composer.

use crate::config::expand_tilde;
use crate::store::Attachment;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// Files above this are rejected (they'd blow up the prompt anyway).
pub const MAX_BYTES: u64 = 256 * 1024;

/// `/abs` and `~/x` stand alone; anything else is relative to `root`.
pub fn resolve(root: Option<&str>, path: &str) -> Result<PathBuf> {
    if path.is_empty() {
        bail!("empty path");
    }
    if path == "~" || path.starts_with("~/") || Path::new(path).is_absolute() {
        return Ok(expand_tilde(if path == "~" { "~/" } else { path }));
    }
    let Some(root) = root.filter(|r| !r.trim().is_empty()) else {
        bail!("relative path but no file root set (chat settings)");
    };
    return Ok(expand_tilde(root.trim()).join(path));
}

/// Reads a text file as an attachment named by `name` (the path as typed).
pub fn read_text(path: &Path, name: &str) -> Result<Attachment> {
    let md = std::fs::metadata(path).with_context(|| format!("{}", path.display()))?;
    if md.is_dir() {
        bail!("{} is a directory", path.display());
    }
    if md.len() > MAX_BYTES {
        bail!(
            "{} is too large ({} KB, max {} KB)",
            path.display(),
            md.len() / 1024,
            MAX_BYTES / 1024
        );
    }
    let bytes = std::fs::read(path).with_context(|| format!("{}", path.display()))?;
    let Ok(content) = String::from_utf8(bytes) else {
        bail!("{} is not UTF-8 text", path.display());
    };
    if content.contains('\0') {
        bail!("{} looks binary", path.display());
    }
    return Ok(Attachment {
        name: name.to_string(),
        content,
        hash: None,
        size: None,
        path: Some(path.display().to_string()),
        missing: false,
    });
}

/// Autocomplete entries returned at most.
const MAX_ENTRIES: usize = 50;

#[derive(serde::Serialize, Debug, PartialEq)]
pub struct Entry {
    /// Full completion: the typed directory part + entry name (+ `/` for dirs).
    pub path: String,
    pub dir: bool,
}

/// Entries of the directory part of `prefix` whose names start with the rest.
/// Dot entries are listed only when the name part starts with `.`.
/// Directories first, then by name; capped at `MAX_ENTRIES`.
pub fn list(root: Option<&str>, prefix: &str) -> Result<Vec<Entry>> {
    let (dir_part, name_part) = match prefix.rfind('/') {
        Some(i) => prefix.split_at(i + 1),
        None if prefix == "~" => ("~/", ""),
        None => ("", prefix),
    };
    let dir = match dir_part {
        "" => match root.filter(|r| !r.trim().is_empty()) {
            Some(r) => expand_tilde(r.trim()),
            None => bail!("no file root set (chat settings)"),
        },
        d => resolve(root, d)?,
    };
    let mut out = Vec::new();
    for e in std::fs::read_dir(&dir).with_context(|| format!("{}", dir.display()))? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with(name_part) || (name.starts_with('.') && !name_part.starts_with('.')) {
            continue;
        }
        // Follows symlinks, so a link to a directory completes as one.
        let is_dir = std::fs::metadata(e.path()).is_ok_and(|m| m.is_dir());
        let slash = if is_dir { "/" } else { "" };
        out.push(Entry {
            path: format!("{dir_part}{name}{slash}"),
            dir: is_dir,
        });
    }
    out.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.path.cmp(&b.path)));
    out.truncate(MAX_ENTRIES);
    return Ok(out);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_completes() {
        let dir = std::env::temp_dir().join(format!("chatport-ls-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for f in ["a.md", "ab.txt", ".hidden", "sub/x.md"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        let root = dir.to_str().unwrap();
        let paths = |p: &str| -> Vec<String> {
            list(Some(root), p)
                .unwrap()
                .into_iter()
                .map(|e| e.path)
                .collect()
        };
        assert_eq!(paths(""), vec!["sub/", "a.md", "ab.txt"]);
        assert_eq!(paths("a"), vec!["a.md", "ab.txt"]);
        assert_eq!(paths("."), vec![".hidden"]);
        assert_eq!(paths("sub/"), vec!["sub/x.md"]);
        let abs = format!("{root}/s");
        assert_eq!(
            list(None, &abs).unwrap(),
            vec![Entry {
                path: format!("{root}/sub/"),
                dir: true
            }]
        );
        assert!(list(None, "a").is_err());
        assert!(list(Some(root), "missing/").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolve_rules() {
        assert_eq!(resolve(None, "/etc/x").unwrap(), PathBuf::from("/etc/x"));
        assert_eq!(
            resolve(Some("/r"), "a/b.md").unwrap(),
            PathBuf::from("/r/a/b.md")
        );
        assert!(resolve(None, "a.md").is_err());
        assert!(resolve(Some("  "), "a.md").is_err());
        assert!(resolve(Some("/r"), "").is_err());
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(
                resolve(None, "~/n.md").unwrap(),
                Path::new(&home).join("n.md")
            );
        }
    }

    #[test]
    fn read_text_checks() {
        let dir = std::env::temp_dir().join(format!("chatport-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ok = dir.join("ok.md");
        std::fs::write(&ok, "hello").unwrap();
        let a = read_text(&ok, "ok.md").unwrap();
        assert_eq!((a.name.as_str(), a.content.as_str()), ("ok.md", "hello"));
        let bin = dir.join("bin");
        std::fs::write(&bin, [0xff, 0x00, 0x01]).unwrap();
        assert!(read_text(&bin, "bin").is_err());
        let big = dir.join("big");
        std::fs::write(&big, vec![b'a'; MAX_BYTES as usize + 1]).unwrap();
        assert!(read_text(&big, "big").is_err());
        assert!(read_text(&dir, "dir").is_err());
        assert!(read_text(&dir.join("missing"), "m").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
