//! Evolving intelligence learning system.
//!
//! Event-driven pipeline that observes agent behavior and builds structured
//! episodes for self-improvement. Runs as an async loop alongside the cortex,
//! writing to a dedicated `learning.db` per agent.

mod config;
pub(crate) mod contradiction;
pub(crate) mod distillation;
mod engine;
pub(crate) mod feedback;
pub(crate) mod meta;
pub(crate) mod outcome;
pub(crate) mod patches;
pub(crate) mod ralph;
pub(crate) mod retriever;
pub(crate) mod signals;
mod store;
mod types;

pub use config::LearningConfig;
pub use engine::spawn_learning_loop;
pub use store::LearningStore;
pub use types::*;

use thiserror::Error;

/// Learning system errors.
#[derive(Debug, Error)]
pub enum LearningError {
    #[error("learning database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("learning engine error: {0}")]
    Engine(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
