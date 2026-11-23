#![allow(clippy::needless_return)] // house style: explicit return in multi-statement fns

mod api;
mod auth;
mod batch;
mod config;
mod files;
mod listen;
mod provider;
mod recent;
mod store;
mod tools;
mod tree;
mod web;

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;

pub struct AppState {
    pub store: store::Store,
    /// Recently picked models (state.json).
    pub recent: recent::Recent,
    pub provider: Arc<dyn provider::Provider>,
    pub config: config::Config,
    /// In-flight generations: conv id -> (generation id, cancel sender).
    pub running: api::Running,
    /// Next automatic check per conversation with a pending batch.
    pub batch_polls: batch::Polls,
    pub batch_lock: batch::Lock,
    /// Wakes the batch poller when `batch_polls` changes.
    pub batch_wake: tokio::sync::Notify,
}

struct Args {
    config: Option<PathBuf>,
    keyfile: Option<PathBuf>,
    bind: Option<String>,
}

fn parse_args() -> Result<Args> {
    let mut args = std::env::args().skip(1);
    let mut out = Args {
        config: None,
        keyfile: None,
        bind: None,
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" | "-c" => {
                out.config = Some(PathBuf::from(args.next().context("--config needs a path")?))
            }
            "--keyfile" => {
                out.keyfile = Some(config::expand_tilde(
                    &args.next().context("--keyfile needs a path")?,
                ))
            }
            "--bind" => out.bind = Some(args.next().context("--bind needs an address")?),
            // Keys on the command line are visible to every user via ps / /proc.
            "--key" => anyhow::bail!("--key was removed (visible in ps); use --keyfile PATH"),
            "--help" | "-h" => {
                println!(
                    "usage: chatport [--config PATH] [--keyfile PATH] [--bind ADDR]\n  \
                     config default: ~/.config/chatport/config.toml\n  \
                     --keyfile / OPENROUTER_API_KEY override the default config key\n  \
                     --bind overrides the config bind (systemd socket activation still wins)"
                );
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
    }
    Ok(out)
}

/// The key from `path`, surrounding whitespace trimmed. Empty is an error.
fn read_keyfile(path: &std::path::Path) -> Result<String> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading keyfile {}", path.display()))?;
    let key = text.trim();
    if key.is_empty() {
        anyhow::bail!("keyfile {} is empty", path.display());
    }
    return Ok(key.to_string());
}

fn main() -> Result<()> {
    // SAFETY: no threads exist yet; the tokio runtime is built below.
    let act = unsafe { listen::take_activation() };
    return tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(act));
}

async fn run(act: listen::Activation) -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = parse_args()?;
    let mut cfg = config::load(args.config)?;
    let override_key = match &args.keyfile {
        Some(p) => Some(read_keyfile(p)?),
        None => std::env::var("OPENROUTER_API_KEY").ok(),
    };
    cfg.resolve_keys(override_key)?;
    if let Some(b) = args.bind {
        log::info!("bind {b:?} from --bind overrides config {:?}", cfg.bind);
        cfg.bind = b;
    }
    log::info!(
        "API keys: {:?} (default {:?})",
        cfg.key_names(),
        cfg.default_key.as_deref().unwrap_or_default()
    );
    let store = store::Store::open(config::expand_tilde(&cfg.data_dir))?;
    let recent = recent::Recent::open(config::expand_tilde(&cfg.data_dir))?;
    let state = Arc::new(AppState {
        store,
        recent,
        provider: Arc::new(provider::openrouter::OpenRouter::new(
            cfg.key_for(None)?.to_string(),
        )?),
        config: cfg.clone(),
        running: Default::default(),
        batch_polls: Default::default(),
        batch_lock: Default::default(),
        batch_wake: Default::default(),
    });
    batch::start_poller(state.clone());

    let mut app = api::router(state).fallback(web::static_handler);
    match &cfg.auth {
        Some(a) => {
            let exp = auth::Expected::new(&a.user, &a.password);
            app = app.layer(axum::middleware::from_fn_with_state(exp, auth::require));
            log::info!("basic auth enabled for user {:?}", a.user);
        }
        None => log::warn!("no [auth] in config: anyone on the network can use this"),
    }

    let listener = listen::listener(act, &cfg.bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
