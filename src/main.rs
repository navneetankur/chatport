#![allow(clippy::needless_return)] // house style: explicit return in multi-statement fns

mod api;
mod auth;
mod config;
mod files;
mod provider;
mod store;
mod tree;
mod web;

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;

pub struct AppState {
    pub store: store::Store,
    pub provider: Arc<dyn provider::Provider>,
    pub config: config::Config,
    /// In-flight generations: conv id -> (generation id, cancel sender).
    pub running: api::Running,
}

struct Args {
    config: Option<PathBuf>,
    key: Option<String>,
}

fn parse_args() -> Result<Args> {
    let mut args = std::env::args().skip(1);
    let mut out = Args {
        config: None,
        key: None,
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" | "-c" => {
                out.config = Some(PathBuf::from(args.next().context("--config needs a path")?))
            }
            "--key" => {
                out.key = Some(
                    args.next()
                        .context("--key needs a value")?
                        .trim()
                        .to_string(),
                )
            }
            "--help" | "-h" => {
                println!(
                    "usage: chatport [--config PATH] [--key KEY]\n  \
                     config default: ~/.config/chatport/config.toml\n  \
                     --key / OPENROUTER_API_KEY override the default config key"
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
    let mut cfg = config::load(args.config)?;
    let override_key = args
        .key
        .filter(|k| !k.is_empty())
        .or_else(|| std::env::var("OPENROUTER_API_KEY").ok());
    cfg.resolve_keys(override_key)?;
    tracing::info!(
        "API keys: {:?} (default {:?})",
        cfg.key_names(),
        cfg.default_key.as_deref().unwrap_or_default()
    );
    let store = store::Store::open(config::expand_tilde(&cfg.data_dir))?;
    let state = Arc::new(AppState {
        store,
        provider: Arc::new(provider::openrouter::OpenRouter::new(
            cfg.key_for(None)?.to_string(),
        )),
        config: cfg.clone(),
        running: Default::default(),
    });

    let mut app = api::router(state).fallback(web::static_handler);
    match &cfg.auth {
        Some(a) => {
            let exp = auth::Expected::new(&a.user, &a.password);
            app = app.layer(axum::middleware::from_fn_with_state(exp, auth::require));
            tracing::info!("basic auth enabled for user {:?}", a.user);
        }
        None => tracing::warn!("no [auth] in config: anyone on the network can use this"),
    }

    let listener = tokio::net::TcpListener::bind(&cfg.bind)
        .await
        .with_context(|| format!("binding {}", cfg.bind))?;
    tracing::info!("listening on http://{}", cfg.bind);
    axum::serve(listener, app).await?;
    Ok(())
}
