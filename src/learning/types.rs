//! Data types for the learning system.

use serde::{Deserialize, Serialize};

/// Predicted or actual outcome of an episode or step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    Failure,
    Partial,
    Unknown,
}

impl Outcome {
    /// Parse from a string, defaulting to Unknown.
    pub fn from_str_lossy(value: &str) -> Self {
        match value {
            "success" => Self::Success,
            "failure" => Self::Failure,
            "partial" => Self::Partial,
            _ => Self::Unknown,
        }
    }
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Success => write!(f, "success"),
            Self::Failure => write!(f, "failure"),
            Self::Partial => write!(f, "partial"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}
