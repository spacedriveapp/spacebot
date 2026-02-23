//! Evolving intelligence learning system.
//!
//! Event-driven pipeline that observes agent behavior and builds structured
//! episodes for self-improvement. Runs as an async loop alongside the cortex,
//! writing to a dedicated `learning.db` per agent.

// -- Milestone 1: Foundation --
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

// -- Milestone 3: Advisory Gating (Layer 3) --
pub(crate) mod cooldowns;
pub(crate) mod gate;
pub(crate) mod packets;
pub(crate) mod prefetch;
pub(crate) mod quarantine;
pub(crate) mod synthesis;
pub(crate) mod tuner;

// -- Milestone 3: Cross-Cutting --
pub(crate) mod control;
pub(crate) mod escape;
pub(crate) mod evidence;
pub(crate) mod importance;
pub(crate) mod phase;
pub(crate) mod truth;
pub(crate) mod tuneables;

// -- Milestone 3: Domain Chips (Layer 4) --
pub(crate) mod chips;

// -- Milestone 4: Introspection & Metrics --
pub(crate) mod commands;
pub(crate) mod metrics;

// -- Observatory --
pub mod observatory;

pub use commands::{LearningCommand, parse_learning_command, handle_learning_command};
pub use config::LearningConfig;
pub use engine::spawn_learning_loop;
pub use metrics::MetricsCalculator;
pub use store::LearningStore;
pub use tuneables::TuneableStore;
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
