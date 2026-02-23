//! Chip-to-cognitive merger pipeline.
//!
//! Translates raw chip scoring outputs into validated [`MergedLearning`]
//! records ready for insertion into Layer 2 of the learning system.
//! Responsibilities:
//!
//! - Stripping chip-specific markup and telemetry lines from content.
//! - Enforcing quality floors on cognitive value, actionability, and
//!   transferability scores.
//! - Detecting and suppressing duplicate-content churn via a sliding window
//!   over word-overlap ratios.
//! - Mapping domain strings to [`InsightCategory`] values for downstream
//!   promotion logic.

use crate::learning::meta::InsightCategory;

use serde_json;

use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Minimum word-overlap ratio (Jaccard) that classifies two content strings as
/// near-duplicates. At 0.60 most restatements are caught while leaving room
/// for similar but meaningfully distinct insights.
const DUPLICATE_OVERLAP_THRESHOLD: f64 = 0.60;

/// Number of consecutive merge attempts tracked in the duplicate-ratio window.
const DUPLICATE_WINDOW_SIZE: usize = 10;

/// Fraction of the duplicate window that must be duplicates before a cooldown
/// is engaged. Above 0.80 the pipeline is clearly churning on the same content.
const DUPLICATE_CHURN_RATIO: f64 = 0.80;

/// How long to suppress merges once duplicate churn is detected.
const COOLDOWN_DURATION: Duration = Duration::from_secs(30 * 60);

/// Maximum number of recent content strings kept for duplicate detection.
const RECENT_CONTENTS_CAPACITY: usize = 100;

/// Minimum `cognitive_value` score for a raw insight to pass quality floors.
const QUALITY_FLOOR_COGNITIVE_VALUE: f64 = 0.35;

/// Minimum `actionability` score for a raw insight to pass quality floors.
const QUALITY_FLOOR_ACTIONABILITY: f64 = 0.25;

/// Minimum `transferability` score for a raw insight to pass quality floors.
const QUALITY_FLOOR_TRANSFERABILITY: f64 = 0.20;

/// Minimum content length in characters (after stripping) to pass quality
/// floors. Keeps single-sentence fragments from entering the cognitive layer.
const QUALITY_FLOOR_CONTENT_LENGTH: usize = 28;

// ── RawChipInsight ────────────────────────────────────────────────────────────

/// A raw scoring output emitted by a domain chip before merger processing.
#[derive(Debug, Clone)]
pub struct RawChipInsight {
    /// Stable identifier for the chip that produced this insight.
    pub chip_id: String,
    /// Raw content string, possibly containing chip markup and telemetry lines.
    pub content: String,
    /// JSON object with individual dimension scores (e.g. `cognitive_value`,
    /// `actionability`, `transferability`).
    pub scores_json: String,
    /// Pre-summed total of all dimension scores for quick pre-filtering and
    /// confidence normalisation.
    pub total_score: f64,
    /// Knowledge domain the chip operates in (e.g. `"coding"`, `"research"`).
    pub domain: String,
}

// ── MergedLearning ────────────────────────────────────────────────────────────

/// A chip insight that has passed all merger quality gates and is ready for
/// Layer 2 insertion.
#[derive(Debug, Clone)]
pub struct MergedLearning {
    /// Knowledge category mapped from the originating chip's domain.
    pub category: InsightCategory,
    /// Cleaned content with chip prefixes and telemetry lines removed.
    pub content: String,
    /// Confidence derived from `total_score / 6.0`, clamped to `[0.0, 1.0]`.
    pub confidence: f64,
    /// Type label of the originating artefact (always `"chip_insight"` for
    /// pipeline output).
    pub source_type: String,
    /// Identifier of the chip that produced the original raw insight.
    pub source_id: String,
}

// ── MergerPipeline ────────────────────────────────────────────────────────────

/// Stateful pipeline that converts [`RawChipInsight`]s into [`MergedLearning`]
/// records.
///
/// Maintains a sliding duplicate-ratio window and a cooldown timer so that
/// bursts of near-identical content cannot flood the cognitive layer.
#[derive(Debug)]
pub struct MergerPipeline {
    /// Rolling record of whether each recent merge attempt was a near-duplicate.
    duplicate_ratio_window: VecDeque<bool>,
    /// When set, all merge attempts return `None` until this instant passes.
    cooldown_until: Option<Instant>,
    /// Running count of insights successfully merged since creation.
    total_merged: u64,
    /// Content strings of the last [`RECENT_CONTENTS_CAPACITY`] accepted
    /// insights, used for Jaccard word-overlap duplicate detection.
    recent_contents: VecDeque<String>,
}

impl MergerPipeline {
    /// Create a new pipeline with empty state and no active cooldown.
    pub fn new() -> Self {
        Self {
            duplicate_ratio_window: VecDeque::with_capacity(DUPLICATE_WINDOW_SIZE),
            cooldown_until: None,
            total_merged: 0,
            recent_contents: VecDeque::with_capacity(RECENT_CONTENTS_CAPACITY),
        }
    }

    /// Total number of insights successfully merged since this pipeline was
    /// created.
    pub fn total_merged(&self) -> u64 {
        self.total_merged
    }

    /// Whether the pipeline is currently in a duplicate-churn cooldown.
    pub fn is_in_cooldown(&self) -> bool {
        match self.cooldown_until {
            Some(deadline) => Instant::now() < deadline,
            None => false,
        }
    }

    /// Attempt to merge a raw chip insight into a [`MergedLearning`].
    ///
    /// Returns `None` when:
    /// - The pipeline is in a duplicate-churn cooldown.
    /// - Content fails quality floors (too short, or low cognitive value,
    ///   actionability, or transferability scores).
    /// - The cleaned content is a near-duplicate of a recently accepted insight.
    /// - Duplicate churn is detected on this call (> 80 % of the last 10
    ///   attempts were near-duplicates), which also engages a 30-minute cooldown.
    pub fn merge(&mut self, raw: &RawChipInsight) -> Option<MergedLearning> {
        if self.is_in_cooldown() {
            tracing::debug!(
                chip_id = %raw.chip_id,
                "merger pipeline in cooldown, skipping insight"
            );
            return None;
        }

        let cleaned_content = strip_telemetry(&strip_chip_prefix(&raw.content));

        if !check_quality_floors(&cleaned_content, &raw.scores_json) {
            tracing::debug!(
                chip_id = %raw.chip_id,
                content_len = cleaned_content.len(),
                "raw chip insight failed quality floors"
            );
            return None;
        }

        let is_duplicate = self.is_duplicate_content(&cleaned_content);
        self.record_in_window(is_duplicate);

        if self.is_churn_detected() {
            tracing::info!(
                chip_id = %raw.chip_id,
                cooldown_minutes = COOLDOWN_DURATION.as_secs() / 60,
                "duplicate churn detected, engaging cooldown"
            );
            self.cooldown_until = Some(Instant::now() + COOLDOWN_DURATION);
            return None;
        }

        if is_duplicate {
            tracing::debug!(
                chip_id = %raw.chip_id,
                "insight content is a near-duplicate, skipping"
            );
            return None;
        }

        self.push_recent_content(cleaned_content.clone());
        self.total_merged += 1;

        let confidence = (raw.total_score / 6.0).clamp(0.0, 1.0);
        let category = map_domain_to_category(&raw.domain);

        Some(MergedLearning {
            category,
            content: cleaned_content,
            confidence,
            source_type: "chip_insight".into(),
            source_id: raw.chip_id.clone(),
        })
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Return `true` when `content` has high word-overlap (Jaccard ≥
    /// [`DUPLICATE_OVERLAP_THRESHOLD`]) with any string in `recent_contents`.
    fn is_duplicate_content(&self, content: &str) -> bool {
        let content_words = word_set(content);
        if content_words.is_empty() {
            return false;
        }

        self.recent_contents.iter().any(|previous| {
            let previous_words = word_set(previous);
            if previous_words.is_empty() {
                return false;
            }
            let overlap = content_words.intersection(&previous_words).count();
            let union = content_words.union(&previous_words).count();
            if union == 0 {
                return false;
            }
            (overlap as f64 / union as f64) >= DUPLICATE_OVERLAP_THRESHOLD
        })
    }

    /// Push a duplicate flag into the sliding window, evicting the oldest entry
    /// when the window has reached [`DUPLICATE_WINDOW_SIZE`].
    fn record_in_window(&mut self, is_duplicate: bool) {
        if self.duplicate_ratio_window.len() >= DUPLICATE_WINDOW_SIZE {
            self.duplicate_ratio_window.pop_front();
        }
        self.duplicate_ratio_window.push_back(is_duplicate);
    }

    /// Return `true` when the window is full and the proportion of duplicate
    /// entries meets or exceeds [`DUPLICATE_CHURN_RATIO`].
    fn is_churn_detected(&self) -> bool {
        if self.duplicate_ratio_window.len() < DUPLICATE_WINDOW_SIZE {
            return false;
        }
        let duplicate_count = self
            .duplicate_ratio_window
            .iter()
            .filter(|&&is_duplicate| is_duplicate)
            .count();
        (duplicate_count as f64 / DUPLICATE_WINDOW_SIZE as f64) >= DUPLICATE_CHURN_RATIO
    }

    /// Add a content string to the recent-contents ring buffer, evicting the
    /// oldest entry when capacity is exhausted.
    fn push_recent_content(&mut self, content: String) {
        if self.recent_contents.len() >= RECENT_CONTENTS_CAPACITY {
            self.recent_contents.pop_front();
        }
        self.recent_contents.push_back(content);
    }
}

impl Default for MergerPipeline {
    fn default() -> Self {
        Self::new()
    }
}

// ── Free-standing helpers ─────────────────────────────────────────────────────

/// Remove a leading `[Anything]` chip-prefix bracket from `content`.
///
/// Only the first bracketed token at the very start of the string is stripped;
/// brackets that appear later in the content are left intact.
pub(crate) fn strip_chip_prefix(content: &str) -> String {
    let trimmed = content.trim_start();
    if trimmed.starts_with('[') {
        if let Some(close_pos) = trimmed.find(']') {
            return trimmed[close_pos + 1..].trim_start().to_string();
        }
    }
    trimmed.to_string()
}

/// Remove telemetry lines from `content`.
///
/// Lines whose trimmed text starts with `tool_name:`, `event_type:`, or `cwd:`
/// are filtered out. This covers the structured metadata that chips sometimes
/// prepend to their raw insight text.
pub(crate) fn strip_telemetry(content: &str) -> String {
    content
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with("tool_name:")
                && !trimmed.starts_with("event_type:")
                && !trimmed.starts_with("cwd:")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Return `true` when `content` and the scores in `scores_json` meet every
/// quality floor.
///
/// All four gates must pass simultaneously:
/// - `cognitive_value` ≥ 0.35
/// - `actionability` ≥ 0.25
/// - `transferability` ≥ 0.20
/// - Cleaned content length ≥ 28 characters
///
/// A missing or unparseable score field is treated as `0.0`, which fails the
/// corresponding floor.
pub(crate) fn check_quality_floors(content: &str, scores_json: &str) -> bool {
    if content.len() < QUALITY_FLOOR_CONTENT_LENGTH {
        return false;
    }

    let scores: serde_json::Value = match serde_json::from_str(scores_json) {
        Ok(value) => value,
        Err(_) => return false,
    };

    let cognitive_value = score_field(&scores, "cognitive_value");
    let actionability = score_field(&scores, "actionability");
    let transferability = score_field(&scores, "transferability");

    cognitive_value >= QUALITY_FLOOR_COGNITIVE_VALUE
        && actionability >= QUALITY_FLOOR_ACTIONABILITY
        && transferability >= QUALITY_FLOOR_TRANSFERABILITY
}

/// Map a domain string to the appropriate [`InsightCategory`].
///
/// | Domain          | Category                                 |
/// |-----------------|------------------------------------------|
/// | `"coding"`      | [`InsightCategory::DomainExpertise`]     |
/// | `"research"`    | [`InsightCategory::Reasoning`]           |
/// | `"communication"` | [`InsightCategory::Communication`]     |
/// | `"productivity"` | [`InsightCategory::SelfAwareness`]      |
/// | `"personal"`    | [`InsightCategory::UserModel`]           |
/// | anything else   | [`InsightCategory::Context`]             |
pub(crate) fn map_domain_to_category(domain: &str) -> InsightCategory {
    match domain {
        "coding" => InsightCategory::DomainExpertise,
        "research" => InsightCategory::Reasoning,
        "communication" => InsightCategory::Communication,
        "productivity" => InsightCategory::SelfAwareness,
        "personal" => InsightCategory::UserModel,
        _ => InsightCategory::Context,
    }
}

// ── Private utilities ─────────────────────────────────────────────────────────

/// Extract a numeric score from a JSON object by field name.
///
/// Returns `0.0` when the field is absent or not representable as `f64`, so
/// callers can compare directly against quality floor constants.
fn score_field(scores: &serde_json::Value, field: &str) -> f64 {
    scores
        .get(field)
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0)
}

/// Build a [`HashSet`] of lowercase alphabetic words from `text` for Jaccard
/// overlap comparison.
///
/// Single-character tokens are excluded to reduce noise from stray punctuation
/// and common abbreviations.
fn word_set(text: &str) -> HashSet<String> {
    text.split_whitespace()
        .map(|token| {
            token
                .chars()
                .filter(|character| character.is_alphabetic())
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|word| word.len() > 1)
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── strip_chip_prefix ─────────────────────────────────────────────────────

    #[test]
    fn test_strip_chip_prefix_removes_bracket() {
        assert_eq!(
            strip_chip_prefix("[CodingChip] Use early returns to reduce nesting"),
            "Use early returns to reduce nesting"
        );
    }

    #[test]
    fn test_strip_chip_prefix_no_bracket_unchanged() {
        assert_eq!(
            strip_chip_prefix("Plain insight without a prefix"),
            "Plain insight without a prefix"
        );
    }

    #[test]
    fn test_strip_chip_prefix_leading_whitespace_trimmed() {
        assert_eq!(strip_chip_prefix("   [Tag] Content"), "Content");
    }

    #[test]
    fn test_strip_chip_prefix_embedded_brackets_untouched() {
        assert_eq!(
            strip_chip_prefix("[Tag] Content with [inner] brackets"),
            "Content with [inner] brackets"
        );
    }

    #[test]
    fn test_strip_chip_prefix_unclosed_bracket_unchanged() {
        // No closing bracket — nothing is stripped.
        assert_eq!(
            strip_chip_prefix("[UnclosedBracket no closing"),
            "[UnclosedBracket no closing"
        );
    }

    #[test]
    fn test_strip_chip_prefix_empty_input() {
        assert_eq!(strip_chip_prefix(""), "");
    }

    #[test]
    fn test_strip_chip_prefix_bracket_only() {
        assert_eq!(strip_chip_prefix("[Tag]"), "");
    }

    // ── strip_telemetry ───────────────────────────────────────────────────────

    #[test]
    fn test_strip_telemetry_removes_tool_name_line() {
        let content = "tool_name: bash\nActual insight text here";
        assert_eq!(strip_telemetry(content), "Actual insight text here");
    }

    #[test]
    fn test_strip_telemetry_removes_event_type_line() {
        let content = "event_type: tool_call\nActual insight text here";
        assert_eq!(strip_telemetry(content), "Actual insight text here");
    }

    #[test]
    fn test_strip_telemetry_removes_cwd_line() {
        let content = "cwd: /Users/user/projects/spacebot\nActual insight text here";
        assert_eq!(strip_telemetry(content), "Actual insight text here");
    }

    #[test]
    fn test_strip_telemetry_removes_multiple_telemetry_lines() {
        let content =
            "tool_name: bash\nevent_type: tool_call\ncwd: /home/user\nActual insight content";
        assert_eq!(strip_telemetry(content), "Actual insight content");
    }

    #[test]
    fn test_strip_telemetry_preserves_non_telemetry_lines() {
        let content = "First real line\nSecond real line";
        assert_eq!(strip_telemetry(content), "First real line\nSecond real line");
    }

    #[test]
    fn test_strip_telemetry_preserves_inline_telemetry_keywords() {
        // The prefix must appear at the start of the line to be stripped.
        let content = "This mentions tool_name: bash but is not a telemetry line";
        assert_eq!(strip_telemetry(content), content);
    }

    #[test]
    fn test_strip_telemetry_empty_input() {
        assert_eq!(strip_telemetry(""), "");
    }

    #[test]
    fn test_strip_telemetry_all_telemetry_lines() {
        let content = "tool_name: bash\nevent_type: tool_result\ncwd: /tmp";
        assert_eq!(strip_telemetry(content), "");
    }

    // ── check_quality_floors ──────────────────────────────────────────────────

    fn passing_scores_json() -> String {
        r#"{"cognitive_value": 0.5, "actionability": 0.4, "transferability": 0.3}"#.into()
    }

    fn content_long_enough() -> &'static str {
        "Prefer small, focused functions with clear contracts"
    }

    #[test]
    fn test_quality_floors_pass_all_criteria_met() {
        assert!(check_quality_floors(content_long_enough(), &passing_scores_json()));
    }

    #[test]
    fn test_quality_floors_fail_content_too_short() {
        assert!(!check_quality_floors("Too short", &passing_scores_json()));
    }

    #[test]
    fn test_quality_floors_fail_low_cognitive_value() {
        let scores =
            r#"{"cognitive_value": 0.10, "actionability": 0.4, "transferability": 0.3}"#;
        assert!(!check_quality_floors(content_long_enough(), scores));
    }

    #[test]
    fn test_quality_floors_fail_low_actionability() {
        let scores =
            r#"{"cognitive_value": 0.5, "actionability": 0.10, "transferability": 0.3}"#;
        assert!(!check_quality_floors(content_long_enough(), scores));
    }

    #[test]
    fn test_quality_floors_fail_low_transferability() {
        let scores =
            r#"{"cognitive_value": 0.5, "actionability": 0.4, "transferability": 0.05}"#;
        assert!(!check_quality_floors(content_long_enough(), scores));
    }

    #[test]
    fn test_quality_floors_fail_invalid_json() {
        assert!(!check_quality_floors(content_long_enough(), "not-valid-json"));
    }

    #[test]
    fn test_quality_floors_missing_field_treated_as_zero() {
        // cognitive_value absent — defaults to 0.0, below the floor.
        let scores = r#"{"actionability": 0.4, "transferability": 0.3}"#;
        assert!(!check_quality_floors(content_long_enough(), scores));
    }

    #[test]
    fn test_quality_floors_exact_minimum_values_pass() {
        let scores = r#"{"cognitive_value": 0.35, "actionability": 0.25, "transferability": 0.20}"#;
        assert!(check_quality_floors(content_long_enough(), scores));
    }

    #[test]
    fn test_quality_floors_content_exactly_at_minimum_length() {
        // 28 characters exactly.
        let content = "12345678901234567890abcdefgh";
        assert_eq!(content.len(), 28);
        assert!(check_quality_floors(content, &passing_scores_json()));
    }

    #[test]
    fn test_quality_floors_content_one_below_minimum_length() {
        let content = "1234567890123456789abcdefgh";
        assert_eq!(content.len(), 27);
        assert!(!check_quality_floors(content, &passing_scores_json()));
    }

    // ── map_domain_to_category ────────────────────────────────────────────────

    #[test]
    fn test_domain_mapping_coding() {
        assert_eq!(
            map_domain_to_category("coding"),
            InsightCategory::DomainExpertise
        );
    }

    #[test]
    fn test_domain_mapping_research() {
        assert_eq!(map_domain_to_category("research"), InsightCategory::Reasoning);
    }

    #[test]
    fn test_domain_mapping_communication() {
        assert_eq!(
            map_domain_to_category("communication"),
            InsightCategory::Communication
        );
    }

    #[test]
    fn test_domain_mapping_productivity() {
        assert_eq!(
            map_domain_to_category("productivity"),
            InsightCategory::SelfAwareness
        );
    }

    #[test]
    fn test_domain_mapping_personal() {
        assert_eq!(map_domain_to_category("personal"), InsightCategory::UserModel);
    }

    #[test]
    fn test_domain_mapping_unknown_falls_back_to_context() {
        assert_eq!(map_domain_to_category("finance"), InsightCategory::Context);
        assert_eq!(map_domain_to_category(""), InsightCategory::Context);
        assert_eq!(map_domain_to_category("CODING"), InsightCategory::Context);
    }

    // ── word_set ──────────────────────────────────────────────────────────────

    #[test]
    fn test_word_set_basic_tokenisation() {
        let words = word_set("Hello world foo");
        assert!(words.contains("hello"));
        assert!(words.contains("world"));
        assert!(words.contains("foo"));
    }

    #[test]
    fn test_word_set_strips_punctuation() {
        let words = word_set("Hello, world!");
        assert!(words.contains("hello"));
        assert!(words.contains("world"));
    }

    #[test]
    fn test_word_set_lowercases_all_tokens() {
        let words = word_set("Rust PROGRAMMING");
        assert!(words.contains("rust"));
        assert!(words.contains("programming"));
    }

    #[test]
    fn test_word_set_excludes_single_char_tokens() {
        let words = word_set("a b c hello");
        assert!(!words.contains("a"));
        assert!(!words.contains("b"));
        assert!(!words.contains("c"));
        assert!(words.contains("hello"));
    }

    #[test]
    fn test_word_set_empty_string_is_empty() {
        assert!(word_set("").is_empty());
    }

    // ── MergerPipeline::is_duplicate_content ──────────────────────────────────

    #[test]
    fn test_pipeline_empty_recent_is_never_duplicate() {
        let pipeline = MergerPipeline::new();
        assert!(
            !pipeline.is_duplicate_content("Some brand new insight content about Rust errors"),
            "empty recent_contents must never classify as duplicate"
        );
    }

    #[test]
    fn test_pipeline_detects_near_duplicate() {
        let mut pipeline = MergerPipeline::new();
        let original = "Avoid deeply nested match arms by extracting helper functions";
        pipeline.push_recent_content(original.to_string());

        // Nine of ten words overlap — Jaccard is 0.9, well above the 0.6 threshold.
        // "always" is the only new word; all others appear in the original.
        assert!(
            pipeline.is_duplicate_content(
                "Avoid deeply nested match arms by extracting helper functions always"
            ),
            "high-overlap content should be flagged as duplicate"
        );
    }

    #[test]
    fn test_pipeline_does_not_flag_different_content() {
        let mut pipeline = MergerPipeline::new();
        pipeline.push_recent_content(
            "Use the builder pattern to construct complex configuration objects".to_string(),
        );

        assert!(
            !pipeline
                .is_duplicate_content("Prefer Option map over nested if-let chains for clean code"),
            "content with low word overlap must not be flagged"
        );
    }

    // ── MergerPipeline::merge ─────────────────────────────────────────────────

    fn passing_raw_insight(domain: &str) -> RawChipInsight {
        RawChipInsight {
            chip_id: "coding_chip".into(),
            content: "[CodingChip] Prefer small, focused functions with clear contracts".into(),
            scores_json: r#"{"cognitive_value": 0.7, "actionability": 0.6, "transferability": 0.5}"#.into(),
            total_score: 4.2,
            domain: domain.into(),
        }
    }

    #[test]
    fn test_merge_produces_merged_learning() {
        let mut pipeline = MergerPipeline::new();
        let result = pipeline.merge(&passing_raw_insight("coding"));

        assert!(result.is_some(), "valid insight must merge successfully");
        let merged = result.unwrap();
        assert_eq!(merged.category, InsightCategory::DomainExpertise);
        assert_eq!(
            merged.content,
            "Prefer small, focused functions with clear contracts"
        );
        assert_eq!(merged.source_type, "chip_insight");
        assert_eq!(merged.source_id, "coding_chip");
        assert!(
            merged.confidence > 0.0 && merged.confidence <= 1.0,
            "confidence must be in (0, 1], got {}",
            merged.confidence
        );
        assert_eq!(pipeline.total_merged(), 1);
    }

    #[test]
    fn test_merge_strips_chip_prefix_and_telemetry() {
        let mut pipeline = MergerPipeline::new();
        let raw = RawChipInsight {
            chip_id: "test_chip".into(),
            content:
                "[Tag] tool_name: bash\nPrefer small, focused functions with clear contracts in Rust"
                    .into(),
            scores_json:
                r#"{"cognitive_value": 0.7, "actionability": 0.6, "transferability": 0.5}"#.into(),
            total_score: 4.0,
            domain: "coding".into(),
        };

        let merged = pipeline.merge(&raw).unwrap();
        assert_eq!(
            merged.content,
            "Prefer small, focused functions with clear contracts in Rust"
        );
    }

    #[test]
    fn test_merge_returns_none_when_quality_fails() {
        let mut pipeline = MergerPipeline::new();
        let raw = RawChipInsight {
            chip_id: "chip".into(),
            content: "Too short".into(),
            scores_json:
                r#"{"cognitive_value": 0.7, "actionability": 0.6, "transferability": 0.5}"#.into(),
            total_score: 4.0,
            domain: "coding".into(),
        };

        assert!(pipeline.merge(&raw).is_none(), "short content must be rejected");
        assert_eq!(pipeline.total_merged(), 0);
    }

    #[test]
    fn test_merge_returns_none_during_cooldown() {
        let mut pipeline = MergerPipeline::new();
        // Force the pipeline into a future cooldown.
        pipeline.cooldown_until = Some(Instant::now() + Duration::from_secs(3600));

        assert!(
            pipeline.merge(&passing_raw_insight("coding")).is_none(),
            "pipeline in cooldown must return None"
        );
    }

    #[test]
    fn test_merge_does_not_increment_count_on_rejection() {
        let mut pipeline = MergerPipeline::new();
        let rejected = RawChipInsight {
            chip_id: "chip".into(),
            content: "Short".into(),
            scores_json:
                r#"{"cognitive_value": 0.7, "actionability": 0.6, "transferability": 0.5}"#.into(),
            total_score: 0.1,
            domain: "coding".into(),
        };
        pipeline.merge(&rejected);
        assert_eq!(pipeline.total_merged(), 0);
    }

    #[test]
    fn test_merge_duplicate_skipped_on_second_call() {
        let mut pipeline = MergerPipeline::new();
        let raw = passing_raw_insight("coding");

        assert!(pipeline.merge(&raw).is_some(), "first merge must succeed");

        // Identical content — overlap is 100 %.
        let duplicate = RawChipInsight {
            content: "[CodingChip] Prefer small, focused functions with clear contracts".into(),
            ..passing_raw_insight("coding")
        };
        assert!(
            pipeline.merge(&duplicate).is_none(),
            "duplicate content must not be merged a second time"
        );
        // Only the first merge is counted.
        assert_eq!(pipeline.total_merged(), 1);
    }

    #[test]
    fn test_merge_confidence_at_full_score() {
        let mut pipeline = MergerPipeline::new();
        let raw = RawChipInsight {
            total_score: 6.0,
            ..passing_raw_insight("coding")
        };

        let merged = pipeline.merge(&raw).unwrap();
        assert!(
            (merged.confidence - 1.0).abs() < f64::EPSILON,
            "total_score 6.0 should yield confidence 1.0, got {}",
            merged.confidence
        );
    }

    #[test]
    fn test_merge_confidence_clamped_above_maximum() {
        let mut pipeline = MergerPipeline::new();
        let raw = RawChipInsight {
            total_score: 12.0, // above the 6-dimension ceiling
            ..passing_raw_insight("coding")
        };

        let merged = pipeline.merge(&raw).unwrap();
        assert!(
            merged.confidence <= 1.0,
            "confidence must never exceed 1.0, got {}",
            merged.confidence
        );
    }

    #[test]
    fn test_merge_domain_routing() {
        let cases = [
            ("coding", InsightCategory::DomainExpertise),
            ("research", InsightCategory::Reasoning),
            ("communication", InsightCategory::Communication),
            ("productivity", InsightCategory::SelfAwareness),
            ("personal", InsightCategory::UserModel),
            ("unknown", InsightCategory::Context),
        ];

        for (domain, expected_category) in cases {
            let mut pipeline = MergerPipeline::new();
            let raw = RawChipInsight {
                content: format!(
                    "[Chip] Prefer small, focused functions with clear contracts in {domain}"
                ),
                ..passing_raw_insight(domain)
            };
            let merged = pipeline.merge(&raw).unwrap();
            assert_eq!(
                merged.category, expected_category,
                "domain '{domain}' should map to {expected_category:?}"
            );
        }
    }

    // ── is_in_cooldown ────────────────────────────────────────────────────────

    #[test]
    fn test_cooldown_false_when_none() {
        let pipeline = MergerPipeline::new();
        assert!(!pipeline.is_in_cooldown());
    }

    #[test]
    fn test_cooldown_true_when_future_deadline() {
        let mut pipeline = MergerPipeline::new();
        pipeline.cooldown_until = Some(Instant::now() + Duration::from_secs(3600));
        assert!(pipeline.is_in_cooldown());
    }

    #[test]
    fn test_cooldown_false_when_deadline_has_passed() {
        let mut pipeline = MergerPipeline::new();
        // Set the deadline to slightly in the past.
        pipeline.cooldown_until = Some(Instant::now() - Duration::from_secs(1));
        assert!(!pipeline.is_in_cooldown());
    }

    // ── churn detection ───────────────────────────────────────────────────────

    #[test]
    fn test_churn_not_detected_below_window_size() {
        let mut pipeline = MergerPipeline::new();
        for _ in 0..(DUPLICATE_WINDOW_SIZE - 1) {
            pipeline.record_in_window(true);
        }
        // Window is not yet full — churn must not trigger.
        assert!(
            !pipeline.is_churn_detected(),
            "churn should not trigger before the window is full"
        );
    }

    #[test]
    fn test_churn_detected_at_full_window_all_duplicates() {
        let mut pipeline = MergerPipeline::new();
        for _ in 0..DUPLICATE_WINDOW_SIZE {
            pipeline.record_in_window(true);
        }
        assert!(
            pipeline.is_churn_detected(),
            "100 % duplicate window must trigger churn detection"
        );
    }

    #[test]
    fn test_churn_detected_at_exact_threshold() {
        let mut pipeline = MergerPipeline::new();
        // 80 % of 10 = 8 duplicates — exactly at the threshold.
        let duplicate_count = (DUPLICATE_WINDOW_SIZE as f64 * DUPLICATE_CHURN_RATIO) as usize;
        let fresh_count = DUPLICATE_WINDOW_SIZE - duplicate_count;

        for _ in 0..fresh_count {
            pipeline.record_in_window(false);
        }
        for _ in 0..duplicate_count {
            pipeline.record_in_window(true);
        }
        assert!(
            pipeline.is_churn_detected(),
            "window at exact churn threshold must trigger detection"
        );
    }

    #[test]
    fn test_churn_not_detected_when_mostly_fresh() {
        let mut pipeline = MergerPipeline::new();
        // 2 duplicates out of 10 (20 %) — well below the 80 % threshold.
        for _ in 0..8 {
            pipeline.record_in_window(false);
        }
        for _ in 0..2 {
            pipeline.record_in_window(true);
        }
        assert!(!pipeline.is_churn_detected());
    }

    #[test]
    fn test_window_evicts_oldest_entry_when_full() {
        let mut pipeline = MergerPipeline::new();
        // Fill the window with duplicates.
        for _ in 0..DUPLICATE_WINDOW_SIZE {
            pipeline.record_in_window(true);
        }
        assert!(pipeline.is_churn_detected());

        // Push enough fresh entries to push all duplicates out.
        for _ in 0..DUPLICATE_WINDOW_SIZE {
            pipeline.record_in_window(false);
        }
        assert!(
            !pipeline.is_churn_detected(),
            "churn should clear once all duplicate entries are evicted"
        );
    }

    // ── Default ───────────────────────────────────────────────────────────────

    #[test]
    fn test_default_creates_empty_pipeline() {
        let pipeline = MergerPipeline::default();
        assert_eq!(pipeline.total_merged(), 0);
        assert!(!pipeline.is_in_cooldown());
    }
}
