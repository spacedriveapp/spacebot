//! Pre-execution analysis for shell commands.

mod analyzer;
mod categorizer;
mod parser;
mod security;
mod types;

pub(crate) use analyzer::ShellAnalyzer;
pub use types::{
    CommandAnalysis, CommandCategory, DetectedPattern, DurationHint, PatternType, RiskLevel,
};
