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

// ---------------------------------------------------------------------------
// Phase (used by phase state machine, advisory gating, control plane)
// ---------------------------------------------------------------------------

/// Activity phase detected from tool usage and task signals.
///
/// The phase state machine tracks the current phase and drives the advisory
/// boost matrix, control plane watcher activation, and distillation context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Explore,
    Plan,
    Execute,
    Validate,
    Consolidate,
    Diagnose,
    Simplify,
    Escalate,
    Halt,
}

impl Phase {
    /// Parse from a string, defaulting to Explore.
    pub fn from_str_lossy(value: &str) -> Self {
        match value.to_lowercase().as_str() {
            "explore" => Self::Explore,
            "plan" => Self::Plan,
            "execute" => Self::Execute,
            "validate" | "testing" => Self::Validate,
            "consolidate" => Self::Consolidate,
            "diagnose" | "debugging" => Self::Diagnose,
            "simplify" => Self::Simplify,
            "escalate" => Self::Escalate,
            "halt" => Self::Halt,
            _ => Self::Explore,
        }
    }
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Explore => write!(f, "explore"),
            Self::Plan => write!(f, "plan"),
            Self::Execute => write!(f, "execute"),
            Self::Validate => write!(f, "validate"),
            Self::Consolidate => write!(f, "consolidate"),
            Self::Diagnose => write!(f, "diagnose"),
            Self::Simplify => write!(f, "simplify"),
            Self::Escalate => write!(f, "escalate"),
            Self::Halt => write!(f, "halt"),
        }
    }
}

// ---------------------------------------------------------------------------
// Advisory authority levels
// ---------------------------------------------------------------------------

/// The five advisory authority levels, from most to least severe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AuthorityLevel {
    Silent,
    Whisper,
    Note,
    Warning,
    Block,
}

impl AuthorityLevel {
    /// Score threshold for this authority level.
    pub fn threshold(&self) -> f64 {
        match self {
            Self::Block => 0.95,
            Self::Warning => 0.80,
            Self::Note => 0.48,
            Self::Whisper => 0.30,
            Self::Silent => 0.0,
        }
    }

    /// Determine authority level from a score.
    pub fn from_score(score: f64) -> Self {
        if score >= 0.95 {
            Self::Block
        } else if score >= 0.80 {
            Self::Warning
        } else if score >= 0.48 {
            Self::Note
        } else if score >= 0.30 {
            Self::Whisper
        } else {
            Self::Silent
        }
    }

    /// Whether this level requires agreement gating (2+ independent sources).
    pub fn requires_agreement(&self) -> bool {
        matches!(self, Self::Warning)
    }
}

impl std::fmt::Display for AuthorityLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Block => write!(f, "BLOCK"),
            Self::Warning => write!(f, "WARNING"),
            Self::Note => write!(f, "NOTE"),
            Self::Whisper => write!(f, "WHISPER"),
            Self::Silent => write!(f, "SILENT"),
        }
    }
}

// ---------------------------------------------------------------------------
// Watcher severity (Control Plane)
// ---------------------------------------------------------------------------

/// Severity of a control plane watcher action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatcherSeverity {
    /// Advisory only — logged but not enforced.
    Warning,
    /// Prevents the action from proceeding.
    Block,
    /// Forces a specific action (e.g., run validation).
    Force,
}

impl std::fmt::Display for WatcherSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Warning => write!(f, "warning"),
            Self::Block => write!(f, "block"),
            Self::Force => write!(f, "force"),
        }
    }
}

// ---------------------------------------------------------------------------
// Evidence types
// ---------------------------------------------------------------------------

/// Type of evidence captured by the evidence store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceType {
    ToolOutput,
    Diff,
    TestResult,
    ErrorTrace,
    Deploy,
    UserFlagged,
}

impl EvidenceType {
    /// Default retention period in hours for this evidence type.
    pub fn retention_hours(&self) -> i64 {
        match self {
            Self::ToolOutput => 72,
            Self::Diff => 168,
            Self::TestResult => 168,
            Self::ErrorTrace => 168,
            Self::Deploy => 720,
            Self::UserFlagged => 0, // permanent
        }
    }

    /// Parse from a string.
    pub fn from_str_lossy(value: &str) -> Self {
        match value {
            "tool_output" => Self::ToolOutput,
            "diff" => Self::Diff,
            "test_result" => Self::TestResult,
            "error_trace" => Self::ErrorTrace,
            "deploy" => Self::Deploy,
            "user_flagged" => Self::UserFlagged,
            _ => Self::ToolOutput,
        }
    }
}

impl std::fmt::Display for EvidenceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ToolOutput => write!(f, "tool_output"),
            Self::Diff => write!(f, "diff"),
            Self::TestResult => write!(f, "test_result"),
            Self::ErrorTrace => write!(f, "error_trace"),
            Self::Deploy => write!(f, "deploy"),
            Self::UserFlagged => write!(f, "user_flagged"),
        }
    }
}

// ---------------------------------------------------------------------------
// Truth status (Truth Ledger)
// ---------------------------------------------------------------------------

/// Status of a claim in the truth ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TruthStatus {
    Claim,
    Fact,
    Rule,
    Stale,
    Contradicted,
}

impl TruthStatus {
    pub fn from_str_lossy(value: &str) -> Self {
        match value {
            "claim" => Self::Claim,
            "fact" => Self::Fact,
            "rule" => Self::Rule,
            "stale" => Self::Stale,
            "contradicted" => Self::Contradicted,
            _ => Self::Claim,
        }
    }
}

impl std::fmt::Display for TruthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Claim => write!(f, "claim"),
            Self::Fact => write!(f, "fact"),
            Self::Rule => write!(f, "rule"),
            Self::Stale => write!(f, "stale"),
            Self::Contradicted => write!(f, "contradicted"),
        }
    }
}

/// Evidence strength for a truth ledger entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceLevel {
    None,
    Weak,
    Strong,
}

impl EvidenceLevel {
    pub fn from_str_lossy(value: &str) -> Self {
        match value {
            "none" => Self::None,
            "weak" => Self::Weak,
            "strong" => Self::Strong,
            _ => Self::None,
        }
    }
}

impl std::fmt::Display for EvidenceLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "none"),
            Self::Weak => write!(f, "weak"),
            Self::Strong => write!(f, "strong"),
        }
    }
}

// ---------------------------------------------------------------------------
// Chip types
// ---------------------------------------------------------------------------

/// Risk level for a domain chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

impl std::fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Low => write!(f, "low"),
            Self::Medium => write!(f, "medium"),
            Self::High => write!(f, "high"),
        }
    }
}

/// Promotion tier for chip insights.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionTier {
    LongTerm,
    Working,
    Session,
    Discard,
}

impl PromotionTier {
    /// Determine promotion tier from total insight score.
    pub fn from_score(score: f64) -> Self {
        if score >= 0.75 {
            Self::LongTerm
        } else if score >= 0.50 {
            Self::Working
        } else if score >= 0.30 {
            Self::Session
        } else {
            Self::Discard
        }
    }
}

impl std::fmt::Display for PromotionTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LongTerm => write!(f, "long_term"),
            Self::Working => write!(f, "working"),
            Self::Session => write!(f, "session"),
            Self::Discard => write!(f, "discard"),
        }
    }
}

// ---------------------------------------------------------------------------
// Escalation types (Escape Protocol)
// ---------------------------------------------------------------------------

/// Type of escalation when the escape protocol triggers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationType {
    Budget,
    Loop,
    Confidence,
    Blocked,
    Unknown,
}

/// What the escalation is requesting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestType {
    Info,
    Decision,
    Help,
    Review,
}

/// Summary of a single attempt made before escalation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptSummary {
    pub description: String,
    pub outcome: String,
    pub tools_used: Vec<String>,
}

/// Structured output from the escape protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationRequest {
    pub escalation_type: EscalationType,
    pub request_type: RequestType,
    pub attempts: Vec<AttemptSummary>,
    pub evidence_gathered: Vec<String>,
    pub current_hypothesis: Option<String>,
    pub suggested_options: Vec<String>,
}

// ---------------------------------------------------------------------------
// Importance signals (Memory Gate)
// ---------------------------------------------------------------------------

/// The five signals used to compute memory importance.
#[derive(Debug, Clone, Default)]
pub struct ImportanceSignals {
    /// Did this unblock progress? (0.0 - 1.0)
    pub impact: f64,
    /// Word overlap with existing insights < 0.5 (0.0 - 1.0)
    pub novelty: f64,
    /// From episode/step surprise_level (0.0 - 1.0)
    pub surprise: f64,
    /// Seen 3+ times in recent episodes (0.0 - 1.0)
    pub recurrence: f64,
    /// High-stakes tool or Execute/Validate phase (0.0 - 1.0)
    pub irreversibility: f64,
}
