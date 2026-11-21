use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
    #[serde(default)]
    pub openrouter_api_key: Option<String>,
    #[serde(default = "default_model")]
    pub default_model: String,
    /// OpenRouter `provider` routing object, passed through as-is.
    #[serde(default)]
    pub default_provider: Option<Value>,
}

fn default_bind() -> String {
    "0.0.0.0:8787".into()
}
fn default_data_dir() -> String {
    "~/.local/share/chat".into()
}
fn default_model() -> String {
    "anthropic/claude-sonnet-4.5".into()
}

pub fn expand_tilde(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => Path::new(&home).join(rest),
        _ => PathBuf::from(p),
    }
}

/// Explicit path must exist; default path is optional.
pub fn load(explicit: Option<PathBuf>) -> Result<Config> {
    let (path, required) = match explicit {
        Some(p) => (p, true),
        None => (expand_tilde("~/.config/chat/config.toml"), false),
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if !required && e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut cfg: Config =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if let Ok(k) = std::env::var("OPENROUTER_API_KEY") {
        if !k.is_empty() {
            cfg.openrouter_api_key = Some(k);
        }
    }
    Ok(cfg)
}
