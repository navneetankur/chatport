#![allow(clippy::needless_return)] // house style: explicit return in multi-statement fns

pub mod api;
pub mod attachments;
pub mod auth;
pub mod batch;
pub mod config;
pub mod files;
pub mod listen;
pub mod provider;
pub mod recent;
pub mod store;
pub mod tools;
pub mod tree;
pub mod web;

use std::sync::Arc;

pub struct AppState {
    pub store: store::Store,
    /// Attachment blobs, use log and pins.
    pub attachments: attachments::Attachments,
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
