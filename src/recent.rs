//! Recently used models, newest first, in `<data_dir>/state.json`.
//!
//! Records the model id the user picked (e.g. `openrouter/free`), not the
//! one a router resolved it to. Kept outside the conversation folders so
//! the default model for a new chat needs no meta.json reads.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

/// How many distinct models the list keeps.
pub const MAX_RECENT: usize = 20;

#[derive(Serialize, Deserialize, Default)]
struct State {
    #[serde(default)]
    recent_models: Vec<String>,
}

pub struct Recent {
    path: PathBuf,
    list: Mutex<Vec<String>>,
}

/// Moves `model` to the front, dropping its older entry and anything past MAX_RECENT.
/// Returns false when the list was already in that order.
fn bump(list: &mut Vec<String>, model: &str) -> bool {
    if list.first().map(String::as_str) == Some(model) {
        return false;
    }
    list.retain(|m| m != model);
    list.insert(0, model.to_string());
    list.truncate(MAX_RECENT);
    return true;
}

impl Recent {
    /// A missing file is an empty list; an unreadable one is an error.
    pub fn open(data_dir: PathBuf) -> Result<Self> {
        let path = data_dir.join("state.json");
        let state: State = match std::fs::read(&path) {
            Ok(b) => {
                serde_json::from_slice(&b).with_context(|| format!("parsing {}", path.display()))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        return Ok(Self {
            path,
            list: Mutex::new(state.recent_models),
        });
    }

    pub fn models(&self) -> Vec<String> {
        self.list.lock().unwrap().clone()
    }

    pub fn latest(&self) -> Option<String> {
        self.list.lock().unwrap().first().cloned()
    }

    /// Records a use of `model` and rewrites state.json atomically.
    pub fn record(&self, model: &str) -> Result<()> {
        let mut list = self.list.lock().unwrap();
        if !bump(&mut list, model) {
            return Ok(());
        }
        let state = State {
            recent_models: list.clone(),
        };
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&state)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("renaming to {}", self.path.display()))?;
        return Ok(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn bump_moves_to_front_and_dedupes() {
        let mut l = v(&["a", "b", "c"]);
        assert!(bump(&mut l, "c"));
        assert_eq!(l, v(&["c", "a", "b"]));
        assert!(!bump(&mut l, "c"));
        assert!(bump(&mut l, "d"));
        assert_eq!(l, v(&["d", "c", "a", "b"]));
    }

    #[test]
    fn bump_caps_length() {
        let mut l: Vec<String> = (0..MAX_RECENT).map(|i| i.to_string()).collect();
        bump(&mut l, "new");
        assert_eq!(l.len(), MAX_RECENT);
        assert_eq!(l[0], "new");
        assert_eq!(l.last().unwrap(), &(MAX_RECENT - 2).to_string());
    }

    #[test]
    fn record_persists() {
        let dir = std::env::temp_dir().join(format!("chatport-recent-{}", crate::store::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let r = Recent::open(dir.clone()).unwrap();
        assert_eq!(r.latest(), None);
        r.record("x/a").unwrap();
        r.record("x/b").unwrap();
        r.record("x/a").unwrap();
        let r2 = Recent::open(dir.clone()).unwrap();
        assert_eq!(r2.models(), v(&["x/a", "x/b"]));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
