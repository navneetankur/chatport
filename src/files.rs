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
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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
