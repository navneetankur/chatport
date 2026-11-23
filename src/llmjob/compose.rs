//! Building prompt text from files: empty-ish parts skipped, trailing
//! whitespace trimmed, parts joined by a blank line, files optionally wrapped
//! in `<file name="…">` tags.

use anyhow::{Context, Result, bail};
use std::io::Read;
use std::path::Path;

/// One piece of a prompt. `name` is set for files (used by `--headers`).
#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    pub name: Option<String>,
    pub text: String,
}

impl Part {
    pub fn text(text: String) -> Self {
        Self { name: None, text }
    }
}

/// The joined prompt, or None when every part is empty or whitespace.
pub fn join(parts: &[Part], headers: bool) -> Option<String> {
    let kept: Vec<String> = parts
        .iter()
        .filter(|p| !p.text.trim().is_empty())
        .map(|p| {
            let text = p.text.trim_end();
            match (&p.name, headers) {
                (Some(n), true) => {
                    format!(
                        "<file name=\"{}\">\n{text}\n</file>",
                        n.replace('"', "&quot;")
                    )
                }
                _ => text.to_string(),
            }
        })
        .collect();
    if kept.is_empty() {
        return None;
    }
    return Some(kept.join("\n\n"));
}

/// Reads prompt files. `-` is stdin, which can be read only once.
#[derive(Default)]
pub struct Reader {
    stdin_used: bool,
}

impl Reader {
    /// `path` as given; `base` resolves relative paths (None = current dir).
    pub fn read(&mut self, path: &str, base: Option<&Path>) -> Result<Part> {
        if path == "-" {
            if self.stdin_used {
                bail!("stdin (`-`) given more than once");
            }
            self.stdin_used = true;
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .context("reading stdin")?;
            return Ok(Part {
                name: Some("stdin".into()),
                text,
            });
        }
        let mut p = crate::config::expand_tilde(path);
        if let Some(b) = base.filter(|_| p.is_relative()) {
            p = b.join(p);
        }
        let text =
            std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        return Ok(Part {
            name: Some(path.to_string()),
            text,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(name: &str, text: &str) -> Part {
        Part {
            name: Some(name.into()),
            text: text.into(),
        }
    }

    #[test]
    fn joins_with_blank_line_and_skips_empty() {
        let parts = [
            f("a.md", "# A\nbody\n\n\n"),
            f("empty.md", " \n\t\n"),
            Part::text("question".into()),
        ];
        assert_eq!(
            join(&parts, false).as_deref(),
            Some("# A\nbody\n\nquestion")
        );
        assert_eq!(join(&[f("e", "  ")], false), None);
        assert_eq!(join(&[], true), None);
    }

    #[test]
    fn headers_wrap_files_only() {
        let parts = [f("x \"y\".txt", "hi\n"), Part::text("q".into())];
        assert_eq!(
            join(&parts, true).as_deref(),
            Some("<file name=\"x &quot;y&quot;.txt\">\nhi\n</file>\n\nq")
        );
    }

    #[test]
    fn relative_paths_use_base() {
        let dir = std::env::temp_dir().join(format!("llmjob-test-{}", crate::store::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("p.md"), "x").unwrap();
        let mut r = Reader::default();
        let p = r.read("p.md", Some(&dir)).unwrap();
        assert_eq!((p.name.as_deref(), p.text.as_str()), (Some("p.md"), "x"));
        let abs = dir.join("p.md");
        assert_eq!(r.read(abs.to_str().unwrap(), None).unwrap().text, "x");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
