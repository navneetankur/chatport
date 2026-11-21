mod api;
mod config;
mod provider;
mod store;
mod tree;

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;

pub struct AppState {
    pub store: store::Store,
    pub provider: Arc<dyn provider::Provider>,
    pub config: config::Config,
}

struct Args {
    config: Option<PathBuf>,
    key: Option<String>,
}

fn parse_args() -> Result<Args> {
    let mut args = std::env::args().skip(1);
    let mut out = Args { config: None, key: None };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" | "-c" => out.config = Some(PathBuf::from(args.next().context("--config needs a path")?)),
            "--key" => out.key = Some(args.next().context("--key needs a value")?.trim().to_string()),
            "--help" | "-h" => {
                println!(
                    "usage: chat [--config PATH] [--key KEY]\n  \
                     config default: ~/.config/chat/config.toml\n  \
                     key priority: --key > OPENROUTER_API_KEY > config"
                );
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
    }
    Ok(out)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = parse_args()?;
    let cfg = config::load(args.config)?;
    let key = args
        .key
        .filter(|k| !k.is_empty())
        .or_else(|| cfg.openrouter_api_key.clone())
        .context("no API key: use --key, OPENROUTER_API_KEY, or openrouter_api_key in config")?;
    let store = store::Store::open(config::expand_tilde(&cfg.data_dir))?;
    let state = Arc::new(AppState {
        store,
        provider: Arc::new(provider::openrouter::OpenRouter::new(key)),
        config: cfg.clone(),
    });

    // TODO(milestone 3): serve embedded Svelte build here.
    let app = api::router(state).fallback(|| async { "chat: frontend not built yet; API under /api" });

    let listener = tokio::net::TcpListener::bind(&cfg.bind).await.with_context(|| format!("binding {}", cfg.bind))?;
    tracing::info!("listening on http://{}", cfg.bind);
    axum::serve(listener, app).await?;
    Ok(())
}
