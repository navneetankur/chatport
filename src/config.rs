use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
    /// Named OpenRouter keys; a conversation picks one by name.
    #[serde(default)]
    pub keys: Vec<NamedKey>,
    /// Name of the key used when a conversation names none. Default: first entry.
    #[serde(default)]
    pub default_key: Option<String>,
    #[serde(default = "default_model")]
    pub default_model: String,
    /// OpenRouter `provider` routing object, passed through as-is.
    #[serde(default)]
    pub default_provider: Option<Value>,
    /// Directory `@relative/path` attachments resolve against in new chats.
    #[serde(default)]
    pub default_file_root: Option<String>,
    /// HTTP Basic auth; off when absent.
    #[serde(default)]
    pub auth: Option<Auth>,
}

#[derive(Deserialize, Clone)]
pub struct Auth {
    pub user: String,
    pub password: String,
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auth")
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize, Clone)]
pub struct NamedKey {
    pub name: String,
    pub key: String,
}

impl std::fmt::Debug for NamedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NamedKey")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl Config {
    /// Applies a `--key` / env override to the default key (creating a key named
    /// "default" if none are configured), then checks names are unique and the
    /// default exists. Afterwards `default_key` is always `Some`.
    pub fn resolve_keys(&mut self, override_key: Option<String>) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for k in &self.keys {
            if !seen.insert(k.name.as_str()) {
                bail!("duplicate key name {:?}", k.name);
            }
        }
        let default = match (&self.default_key, self.keys.first()) {
            (Some(d), _) => d.clone(),
            (None, Some(first)) => first.name.clone(),
            (None, None) => "default".to_string(),
        };
        let override_key = override_key.filter(|k| !k.is_empty());
        let no_keys = self.keys.is_empty();
        match (
            self.keys.iter_mut().find(|k| k.name == default),
            override_key,
        ) {
            (Some(k), Some(o)) => k.key = o,
            (Some(_), None) => {}
            (None, Some(o)) => self.keys.push(NamedKey {
                name: default.clone(),
                key: o,
            }),
            (None, None) if no_keys => {
                bail!("no API key: use --key, OPENROUTER_API_KEY, or [[keys]] in config")
            }
            (None, None) => bail!("default_key {default:?} is not in [[keys]]"),
        }
        self.default_key = Some(default);
        return Ok(());
    }

    /// Key for a conversation: its named key, or the default when unset.
    pub fn key_for(&self, name: Option<&str>) -> Result<&str> {
        let name = name.or(self.default_key.as_deref()).unwrap_or_default();
        return self
            .keys
            .iter()
            .find(|k| k.name == name)
            .map(|k| k.key.as_str())
            .with_context(|| format!("API key {name:?} is not in config"));
    }

    pub fn key_names(&self) -> Vec<&str> {
        self.keys.iter().map(|k| k.name.as_str()).collect()
    }
}

fn default_bind() -> String {
    "0.0.0.0:8787".into()
}
fn default_data_dir() -> String {
    "~/.local/share/chatport".into()
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
        None => (expand_tilde("~/.config/chatport/config.toml"), false),
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if !required && e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let cfg: Config =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(toml_text: &str) -> Config {
        toml::from_str(toml_text).unwrap()
    }
    const TWO: &str = r#"
        [[keys]]
        name = "a"
        key = "ka"
        [[keys]]
        name = "b"
        key = "kb"
    "#;

    #[test]
    fn default_is_first_key() {
        let mut c = cfg(TWO);
        c.resolve_keys(None).unwrap();
        assert_eq!(c.key_for(None).unwrap(), "ka");
        assert_eq!(c.key_for(Some("b")).unwrap(), "kb");
        assert!(c.key_for(Some("zz")).is_err());
    }

    #[test]
    fn override_replaces_default_only() {
        let mut c = cfg(&format!("default_key = \"b\"\n{TWO}"));
        c.resolve_keys(Some("ko".into())).unwrap();
        assert_eq!(c.key_for(None).unwrap(), "ko");
        assert_eq!(c.key_for(Some("a")).unwrap(), "ka");
    }

    #[test]
    fn override_without_keys_creates_default() {
        let mut c = cfg("");
        c.resolve_keys(Some("ko".into())).unwrap();
        assert_eq!(c.key_names(), vec!["default"]);
        assert_eq!(c.key_for(None).unwrap(), "ko");
    }

    #[test]
    fn errors() {
        assert!(cfg("").resolve_keys(None).is_err());
        assert!(
            cfg(&format!("default_key = \"x\"\n{TWO}"))
                .resolve_keys(None)
                .is_err()
        );
        let dup = "[[keys]]\nname=\"a\"\nkey=\"1\"\n[[keys]]\nname=\"a\"\nkey=\"2\"";
        assert!(cfg(dup).resolve_keys(None).is_err());
    }
}
