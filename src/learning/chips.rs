//! Domain chips: specialized learning modules per knowledge domain.
//!
//! Chips are YAML-defined processors that observe specific event patterns,
//! collect domain-specific observations, and produce insights scored on six
//! dimensions. Insights passing quality floors are merged into the cognitive
//! layer (Layer 2) via the merger pipeline.

pub(crate) mod evolution;
pub(crate) mod merger;
pub(crate) mod policy;
pub(crate) mod runtime;
pub(crate) mod schema;
pub(crate) mod scoring;
