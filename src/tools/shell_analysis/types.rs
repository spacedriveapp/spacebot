//! Types describing shell command analysis results.

use serde::Serialize;

/// Semantic category of a shell command.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandCategory {
    Search,
    Read,
    List,
    Write,
    Destructive,
    Network,
    Silent,
    Other,
}

/// Risk level for command execution.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Safe,
    Caution,
    Dangerous,
}

/// Estimated duration for UX decisions.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DurationHint {
    Fast,
    Medium,
    Long,
}

/// Detected pattern types that influence execution safety.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PatternType {
    CommandSubstitution,
    ProcessSubstitution,
    ObfuscatedFlag,
    GitCommitMessage,
    IfsInjection,
    Newline,
    CarriageReturn,
    ProcEnvironAccess,
    EnvExfiltration,
    OutsideWorkspacePath,
}

/// A detected shell pattern that influenced the final analysis.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DetectedPattern {
    pub pattern_type: PatternType,
    pub description: String,
    pub position: Option<usize>,
}

/// Complete analysis result for a shell command.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CommandAnalysis {
    pub category: CommandCategory,
    pub risk_level: RiskLevel,
    pub duration_hint: DurationHint,
    pub patterns: Vec<DetectedPattern>,
    pub requires_confirmation: bool,
    pub confirmation_reason: Option<String>,
    pub collapsed_by_default: bool,
    pub expects_no_output: bool,
}
