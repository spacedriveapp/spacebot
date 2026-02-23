//! Memory gate with 5-signal importance scoring.
//!
//! Computes a composite importance score from five signals — impact, novelty,
//! surprise, recurrence, and irreversibility — and gates memory persistence
//! against a configurable threshold.

use crate::learning::ImportanceSignals;

use std::collections::HashSet;

// ---------------------------------------------------------------------------
// Core scoring
// ---------------------------------------------------------------------------

/// Compute a composite importance score from all five signals.
///
/// Weights are intentionally asymmetric: irreversibility and surprise carry
/// the most weight because high-stakes or unexpected events are the most
/// valuable to retain. The sum can exceed 1.0 before clamping, which lets
/// a single dominant signal push past weaker peers.
pub fn score_importance(signals: &ImportanceSignals) -> f64 {
    (signals.impact * 0.3
        + signals.novelty * 0.2
        + signals.surprise * 0.3
        + signals.recurrence * 0.2
        + signals.irreversibility * 0.4)
        .min(1.0)
}

/// Return true if the signal composite clears the threshold.
pub fn passes_memory_gate(signals: &ImportanceSignals, threshold: f64) -> bool {
    score_importance(signals) >= threshold
}

// ---------------------------------------------------------------------------
// Per-signal computation helpers
// ---------------------------------------------------------------------------

/// Estimate impact from whether the episode unblocked forward progress.
pub fn compute_impact(progress_made: bool) -> f64 {
    if progress_made { 0.8 } else { 0.2 }
}

/// Estimate novelty as the complement of best word-overlap with known insights.
///
/// Uses Jaccard similarity over whitespace-split word sets. A content string
/// with no overlap against any existing insight scores the maximum default of
/// 0.8 rather than 1.0, reflecting that genuine novelty is rare. When there
/// are no existing insights the same default applies.
pub fn compute_novelty(content: &str, existing_insights: &[&str]) -> f64 {
    if existing_insights.is_empty() {
        return 0.8;
    }

    let content_words: HashSet<&str> = content.split_whitespace().collect();

    let best_overlap = existing_insights
        .iter()
        .map(|insight| {
            let insight_words: HashSet<&str> = insight.split_whitespace().collect();
            let intersection = content_words.intersection(&insight_words).count();
            let union = content_words.union(&insight_words).count();
            if union == 0 {
                0.0
            } else {
                intersection as f64 / union as f64
            }
        })
        .fold(0.0_f64, |accumulated, overlap| accumulated.max(overlap));

    (1.0 - best_overlap).clamp(0.0, 1.0)
}

/// Extract surprise from an optional explicit level, defaulting to moderate.
pub fn compute_surprise(surprise_level: Option<f64>) -> f64 {
    surprise_level.unwrap_or(0.3)
}

/// Estimate recurrence from how many times this pattern has appeared recently.
///
/// The step function ensures low-frequency observations carry no recurrence
/// signal — three or more occurrences are needed before it starts contributing.
pub fn compute_recurrence(count: u32) -> f64 {
    match count {
        0..=2 => 0.0,
        3 => 0.5,
        4 => 0.7,
        _ => 0.9,
    }
}

/// Estimate irreversibility from the tool invoked and the current phase.
///
/// Shell and execution tools represent state changes in the environment that
/// cannot be undone by further tool calls. The Execute and Validate phases
/// carry elevated risk relative to planning or exploration phases.
pub fn compute_irreversibility(tool_name: &str, phase: &str) -> f64 {
    if matches!(tool_name, "shell" | "exec") || tool_name.contains("write") {
        return 0.8;
    }
    match phase {
        "execute" | "validate" => 0.6,
        _ => 0.3,
    }
}

// ---------------------------------------------------------------------------
// MemoryGate
// ---------------------------------------------------------------------------

/// Result of evaluating the memory gate against a set of signals.
#[derive(Debug, Clone, Copy)]
pub struct GateResult {
    /// Composite importance score in [0.0, 1.0].
    pub score: f64,
    /// True if the score meets or exceeds the gate threshold.
    pub passed: bool,
}

/// Stateful gate that evaluates importance signals against a fixed threshold.
#[derive(Debug, Clone)]
pub struct MemoryGate {
    threshold: f64,
}

impl MemoryGate {
    /// Create a gate with the given importance threshold.
    pub fn new(threshold: f64) -> Self {
        Self { threshold }
    }

    /// Evaluate the signals and return a scored gate result.
    pub fn evaluate(&self, signals: &ImportanceSignals) -> GateResult {
        let score = score_importance(signals);
        GateResult {
            score,
            passed: score >= self.threshold,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn all_zero_signals() -> ImportanceSignals {
        ImportanceSignals {
            impact: 0.0,
            novelty: 0.0,
            surprise: 0.0,
            recurrence: 0.0,
            irreversibility: 0.0,
        }
    }

    fn all_one_signals() -> ImportanceSignals {
        ImportanceSignals {
            impact: 1.0,
            novelty: 1.0,
            surprise: 1.0,
            recurrence: 1.0,
            irreversibility: 1.0,
        }
    }

    // --- score_importance ---

    #[test]
    fn score_all_zeros_is_zero() {
        assert_eq!(score_importance(&all_zero_signals()), 0.0);
    }

    #[test]
    fn score_all_ones_clamps_to_one() {
        // Raw sum: 0.3+0.2+0.3+0.2+0.4 = 1.4, clamped to 1.0.
        assert_eq!(score_importance(&all_one_signals()), 1.0);
    }

    #[test]
    fn score_applies_correct_weights() {
        let signals = ImportanceSignals {
            impact: 1.0,
            novelty: 0.0,
            surprise: 0.0,
            recurrence: 0.0,
            irreversibility: 0.0,
        };
        assert!((score_importance(&signals) - 0.3).abs() < f64::EPSILON);

        let signals = ImportanceSignals {
            impact: 0.0,
            novelty: 0.0,
            surprise: 0.0,
            recurrence: 0.0,
            irreversibility: 1.0,
        };
        assert!((score_importance(&signals) - 0.4).abs() < f64::EPSILON);
    }

    #[test]
    fn passes_memory_gate_respects_threshold() {
        let signals = ImportanceSignals {
            impact: 1.0,
            novelty: 0.0,
            surprise: 0.0,
            recurrence: 0.0,
            irreversibility: 0.0,
        };
        // score = 0.3
        assert!(passes_memory_gate(&signals, 0.3));
        assert!(!passes_memory_gate(&signals, 0.31));
    }

    // --- compute_impact ---

    #[test]
    fn impact_high_when_progress_made() {
        assert_eq!(compute_impact(true), 0.8);
    }

    #[test]
    fn impact_low_when_no_progress() {
        assert_eq!(compute_impact(false), 0.2);
    }

    // --- compute_novelty ---

    #[test]
    fn novelty_defaults_when_no_existing_insights() {
        assert_eq!(compute_novelty("something brand new", &[]), 0.8);
    }

    #[test]
    fn novelty_is_one_minus_best_jaccard_overlap() {
        // "the cat sat" vs "the cat sat" → full overlap → novelty = 0.0.
        let novelty = compute_novelty("the cat sat", &["the cat sat"]);
        assert!(novelty < 0.01, "expected near-zero novelty, got {novelty}");
    }

    #[test]
    fn novelty_is_high_for_unrelated_content() {
        let novelty = compute_novelty("quantum entanglement", &["the cat sat on the mat"]);
        // No shared words → best_overlap = 0.0 → novelty = 1.0.
        assert!((novelty - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn novelty_uses_best_overlap_across_multiple_insights() {
        // One insight shares all words, the other shares none.
        let novelty = compute_novelty("hello world", &["goodbye moon", "hello world"]);
        // best_overlap = 1.0 → novelty = 0.0.
        assert!(novelty < 0.01, "expected near-zero novelty, got {novelty}");
    }

    #[test]
    fn novelty_partial_overlap() {
        // "a b c" vs "a b d": intersection={a,b}=2, union={a,b,c,d}=4
        // Jaccard = 2/4 = 0.5 → novelty = 0.5.
        let novelty = compute_novelty("a b c", &["a b d"]);
        assert!((novelty - 0.5).abs() < f64::EPSILON);
    }

    // --- compute_surprise ---

    #[test]
    fn surprise_returns_provided_value() {
        assert_eq!(compute_surprise(Some(0.9)), 0.9);
    }

    #[test]
    fn surprise_defaults_to_moderate() {
        assert_eq!(compute_surprise(None), 0.3);
    }

    // --- compute_recurrence ---

    #[test]
    fn recurrence_zero_for_fewer_than_three_occurrences() {
        assert_eq!(compute_recurrence(0), 0.0);
        assert_eq!(compute_recurrence(2), 0.0);
    }

    #[test]
    fn recurrence_steps_at_three_four_and_five_plus() {
        assert_eq!(compute_recurrence(3), 0.5);
        assert_eq!(compute_recurrence(4), 0.7);
        assert_eq!(compute_recurrence(5), 0.9);
        assert_eq!(compute_recurrence(100), 0.9);
    }

    // --- compute_irreversibility ---

    #[test]
    fn irreversibility_high_for_shell_and_exec() {
        assert_eq!(compute_irreversibility("shell", "explore"), 0.8);
        assert_eq!(compute_irreversibility("exec", "plan"), 0.8);
    }

    #[test]
    fn irreversibility_high_for_write_tools() {
        assert_eq!(compute_irreversibility("file_write", "explore"), 0.8);
        assert_eq!(compute_irreversibility("write_file", "plan"), 0.8);
    }

    #[test]
    fn irreversibility_elevated_for_execute_and_validate_phases() {
        assert_eq!(compute_irreversibility("memory_recall", "execute"), 0.6);
        assert_eq!(compute_irreversibility("memory_recall", "validate"), 0.6);
    }

    #[test]
    fn irreversibility_low_for_safe_tools_and_phases() {
        assert_eq!(compute_irreversibility("memory_recall", "explore"), 0.3);
        assert_eq!(compute_irreversibility("branch", "plan"), 0.3);
    }

    // --- MemoryGate ---

    #[test]
    fn gate_passes_when_score_meets_threshold() {
        let gate = MemoryGate::new(0.5);
        let signals = ImportanceSignals {
            impact: 1.0,
            novelty: 1.0,
            surprise: 0.0,
            recurrence: 0.0,
            irreversibility: 0.0,
        };
        // score = 0.3 + 0.2 = 0.5
        let result = gate.evaluate(&signals);
        assert!((result.score - 0.5).abs() < f64::EPSILON);
        assert!(result.passed);
    }

    #[test]
    fn gate_fails_when_score_below_threshold() {
        let gate = MemoryGate::new(0.8);
        let result = gate.evaluate(&all_zero_signals());
        assert_eq!(result.score, 0.0);
        assert!(!result.passed);
    }

    #[test]
    fn gate_result_is_copy() {
        let gate = MemoryGate::new(0.5);
        let result = gate.evaluate(&all_one_signals());
        let _copy = result; // GateResult is Copy; original still usable.
        assert_eq!(result.score, 1.0);
    }
}
