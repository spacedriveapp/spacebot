//! Control plane watchers for the learning system.
//!
//! Watchers inspect a `LearningState` snapshot and emit `WatcherAction`s when
//! they detect problematic patterns — repeated errors, stagnant confidence,
//! file thrashing, scope drift, and missing validation.  `ControlPlane` runs
//! all registered watchers and reports which fired.

use crate::learning::types::{Phase, WatcherSeverity};

use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// WatcherAction
// ---------------------------------------------------------------------------

/// A single watcher firing: who fired, how severe, and what to do about it.
#[derive(Debug, Clone)]
pub struct WatcherAction {
    /// Name of the watcher that produced this action.
    pub watcher_name: &'static str,
    /// How severe the signal is.
    pub severity: WatcherSeverity,
    /// Human-readable description of what was detected.
    pub message: String,
    /// Optional concrete next step the agent should take.
    pub suggested_action: Option<String>,
}

// ---------------------------------------------------------------------------
// LearningState
// ---------------------------------------------------------------------------

/// Snapshot of learning metrics evaluated by the control plane each step.
///
/// Callers build this from the running episode/step state and pass it to
/// `ControlPlane::evaluate`.  All fields default to safe zero-values so
/// partial construction via `..Default::default()` is always valid.
#[derive(Debug, Clone)]
pub struct LearningState {
    /// How many consecutive steps produced an identical error message.
    pub consecutive_same_errors: u32,
    /// How many steps have passed without any new evidence being recorded.
    pub steps_without_new_evidence: u32,
    /// Number of times each file path has been edited in the current episode.
    pub file_edit_counts: HashMap<String, u32>,
    /// Running record of per-step confidence scores (oldest first).
    pub confidence_history: Vec<f64>,
    /// The task description as originally stated.
    pub original_task: Option<String>,
    /// The task scope as currently understood (may drift over time).
    pub current_task_scope: Option<String>,
    /// Whether at least one validation step has been executed.
    pub has_validation: bool,
    /// Current activity phase.
    pub phase: Phase,
    /// Total number of watcher actions that have fired across all steps so far.
    pub watcher_firing_count: u32,
}

impl Default for LearningState {
    fn default() -> Self {
        Self {
            consecutive_same_errors: 0,
            steps_without_new_evidence: 0,
            file_edit_counts: HashMap::new(),
            confidence_history: Vec::new(),
            original_task: None,
            current_task_scope: None,
            has_validation: false,
            phase: Phase::Explore,
            watcher_firing_count: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Watcher infrastructure
// ---------------------------------------------------------------------------

/// A pure function that checks one condition and optionally fires an action.
pub type WatcherFn = fn(&LearningState) -> Option<WatcherAction>;

/// A named watcher with its check function and declared severity.
///
/// The `severity` field mirrors what the check function will embed in any
/// `WatcherAction` it returns, allowing callers to inspect the watcher
/// registry without running every check.
pub struct Watcher {
    pub name: &'static str,
    pub check: WatcherFn,
    pub severity: WatcherSeverity,
}

// ---------------------------------------------------------------------------
// ControlPlane
// ---------------------------------------------------------------------------

/// Runs all registered watchers against a `LearningState` and collects fired actions.
pub struct ControlPlane {
    watchers: Vec<Watcher>,
}

impl ControlPlane {
    /// Create a new control plane pre-loaded with the six built-in watchers.
    pub fn new() -> Self {
        Self {
            watchers: vec![
                Watcher {
                    name: "check_repeat_error",
                    check: check_repeat_error,
                    severity: WatcherSeverity::Block,
                },
                Watcher {
                    name: "check_no_new_info",
                    check: check_no_new_info,
                    severity: WatcherSeverity::Warning,
                },
                Watcher {
                    name: "check_diff_thrash",
                    check: check_diff_thrash,
                    severity: WatcherSeverity::Block,
                },
                Watcher {
                    name: "check_confidence_stagnation",
                    check: check_confidence_stagnation,
                    severity: WatcherSeverity::Warning,
                },
                Watcher {
                    name: "check_scope_creep",
                    check: check_scope_creep,
                    severity: WatcherSeverity::Warning,
                },
                Watcher {
                    name: "check_validation_gap",
                    check: check_validation_gap,
                    severity: WatcherSeverity::Force,
                },
            ],
        }
    }

    /// Run every registered watcher and return the actions that fired.
    ///
    /// Watchers that return `None` are silently skipped.  Order matches
    /// registration order so callers can rely on stable output ordering.
    pub fn evaluate(&self, state: &LearningState) -> Vec<WatcherAction> {
        self.watchers
            .iter()
            .filter_map(|watcher| (watcher.check)(state))
            .collect()
    }

    /// Return `true` if any watcher would emit a `Block`-severity action.
    pub fn has_block(&self, state: &LearningState) -> bool {
        self.evaluate(state)
            .iter()
            .any(|action| action.severity == WatcherSeverity::Block)
    }
}

impl Default for ControlPlane {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Built-in watcher functions
// ---------------------------------------------------------------------------

/// Fire when the same error has occurred on two or more consecutive steps.
///
/// Identical errors in a row are a strong signal of a tight loop.  Blocking
/// forces the agent to adopt a different strategy before continuing.
pub fn check_repeat_error(state: &LearningState) -> Option<WatcherAction> {
    if state.consecutive_same_errors >= 2 {
        Some(WatcherAction {
            watcher_name: "check_repeat_error",
            severity: WatcherSeverity::Block,
            message: format!(
                "the same error has repeated {} consecutive times",
                state.consecutive_same_errors,
            ),
            suggested_action: Some(
                "Try a different approach before retrying the same operation.".to_string(),
            ),
        })
    } else {
        None
    }
}

/// Fire when too many steps have passed without recording any new evidence.
///
/// Extended stretches without new evidence suggest the agent is spinning
/// without making observable progress.
pub fn check_no_new_info(state: &LearningState) -> Option<WatcherAction> {
    if state.steps_without_new_evidence >= 10 {
        Some(WatcherAction {
            watcher_name: "check_no_new_info",
            severity: WatcherSeverity::Warning,
            message: format!(
                "{} steps have passed without any new evidence being recorded",
                state.steps_without_new_evidence,
            ),
            suggested_action: Some(
                "Gather fresh evidence before continuing.".to_string(),
            ),
        })
    } else {
        None
    }
}

/// Fire when any single file has been edited three or more times.
///
/// Repeated edits to the same file in one episode indicate thrashing — the
/// agent is cycling through the same patch without converging.
pub fn check_diff_thrash(state: &LearningState) -> Option<WatcherAction> {
    state
        .file_edit_counts
        .iter()
        .find(|(_, count)| **count >= 3)
        .map(|(path, count)| WatcherAction {
            watcher_name: "check_diff_thrash",
            severity: WatcherSeverity::Block,
            message: format!(
                "file '{}' has been edited {} times in this episode",
                path, count,
            ),
            suggested_action: Some(
                "Stop re-editing the same file and reconsider the approach from scratch."
                    .to_string(),
            ),
        })
}

/// Fire when the last five confidence scores are flat and all below 0.5.
///
/// Confidence that neither rises nor falls over five steps, while remaining
/// low, indicates the agent has plateaued without making meaningful progress.
pub fn check_confidence_stagnation(state: &LearningState) -> Option<WatcherAction> {
    let history = &state.confidence_history;
    if history.len() < 5 {
        return None;
    }

    let last_five = &history[history.len() - 5..];

    let min = last_five.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = last_five.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

    // Flat means all values within 0.05 of each other.
    let is_flat = (max - min) <= 0.05;
    // Low means every score is below the confidence midpoint.
    let is_low = last_five.iter().all(|&score| score < 0.5);

    if is_flat && is_low {
        Some(WatcherAction {
            watcher_name: "check_confidence_stagnation",
            severity: WatcherSeverity::Warning,
            message: format!(
                "confidence has been flat ({:.2}–{:.2}) and low for the last 5 steps",
                min, max,
            ),
            suggested_action: Some(
                "Seek new information or escalate rather than continuing on the current path."
                    .to_string(),
            ),
        })
    } else {
        None
    }
}

/// Fire when the current task scope has drifted far from the original task.
///
/// Jaccard similarity below 0.3 on word-level overlap is a reliable signal
/// that the agent has wandered outside the intended work boundary.
pub fn check_scope_creep(state: &LearningState) -> Option<WatcherAction> {
    let (Some(original), Some(current)) =
        (state.original_task.as_deref(), state.current_task_scope.as_deref())
    else {
        return None;
    };

    let original_words = word_set(original);
    let current_words = word_set(current);
    let similarity = jaccard_similarity(&original_words, &current_words);

    if similarity < 0.3 {
        Some(WatcherAction {
            watcher_name: "check_scope_creep",
            severity: WatcherSeverity::Warning,
            message: format!(
                "current task scope has drifted from the original (word overlap {:.0}%)",
                similarity * 100.0,
            ),
            suggested_action: Some(
                "Re-anchor to the original task or explicitly confirm the new scope with the user."
                    .to_string(),
            ),
        })
    } else {
        None
    }
}

/// Fire when no validation has been run in any phase that warrants it.
///
/// Explore and Plan phases precede any execution, so missing validation
/// is expected there.  Every other phase should have at least one validation
/// step before the episode concludes.
pub fn check_validation_gap(state: &LearningState) -> Option<WatcherAction> {
    let exempt = matches!(state.phase, Phase::Explore | Phase::Plan);
    if !state.has_validation && !exempt {
        Some(WatcherAction {
            watcher_name: "check_validation_gap",
            severity: WatcherSeverity::Force,
            message: format!(
                "no validation has been run and the current phase is '{}'",
                state.phase,
            ),
            suggested_action: Some("Run validation".to_string()),
        })
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract unique lowercase words from a text string.
fn word_set(text: &str) -> HashSet<String> {
    text.split_whitespace()
        .map(|word| word.to_lowercase())
        .collect()
}

/// Jaccard similarity: |intersection| / |union|.
///
/// Returns 1.0 when both sets are empty (considered identical).
fn jaccard_similarity(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    let union_size = a.union(b).count();
    if union_size == 0 {
        return 1.0;
    }
    let intersection_size = a.intersection(b).count();
    intersection_size as f64 / union_size as f64
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- check_repeat_error -------------------------------------------------

    #[test]
    fn repeat_error_fires_at_threshold() {
        let state = LearningState {
            consecutive_same_errors: 2,
            ..Default::default()
        };
        let action = check_repeat_error(&state).expect("should fire");
        assert_eq!(action.severity, WatcherSeverity::Block);
        assert_eq!(action.watcher_name, "check_repeat_error");
    }

    #[test]
    fn repeat_error_fires_above_threshold() {
        let state = LearningState {
            consecutive_same_errors: 5,
            ..Default::default()
        };
        assert!(check_repeat_error(&state).is_some());
    }

    #[test]
    fn repeat_error_silent_below_threshold() {
        let state = LearningState {
            consecutive_same_errors: 1,
            ..Default::default()
        };
        assert!(check_repeat_error(&state).is_none());
    }

    #[test]
    fn repeat_error_silent_at_zero() {
        assert!(check_repeat_error(&LearningState::default()).is_none());
    }

    // --- check_no_new_info --------------------------------------------------

    #[test]
    fn no_new_info_fires_at_threshold() {
        let state = LearningState {
            steps_without_new_evidence: 10,
            ..Default::default()
        };
        let action = check_no_new_info(&state).expect("should fire");
        assert_eq!(action.severity, WatcherSeverity::Warning);
        assert_eq!(action.watcher_name, "check_no_new_info");
    }

    #[test]
    fn no_new_info_fires_above_threshold() {
        let state = LearningState {
            steps_without_new_evidence: 25,
            ..Default::default()
        };
        assert!(check_no_new_info(&state).is_some());
    }

    #[test]
    fn no_new_info_silent_below_threshold() {
        let state = LearningState {
            steps_without_new_evidence: 9,
            ..Default::default()
        };
        assert!(check_no_new_info(&state).is_none());
    }

    // --- check_diff_thrash --------------------------------------------------

    #[test]
    fn diff_thrash_fires_when_file_edited_three_times() {
        let mut counts = HashMap::new();
        counts.insert("src/main.rs".to_string(), 3);
        let state = LearningState {
            file_edit_counts: counts,
            ..Default::default()
        };
        let action = check_diff_thrash(&state).expect("should fire");
        assert_eq!(action.severity, WatcherSeverity::Block);
        assert_eq!(action.watcher_name, "check_diff_thrash");
        assert!(action.message.contains("src/main.rs"));
    }

    #[test]
    fn diff_thrash_fires_when_file_edited_more_than_three_times() {
        let mut counts = HashMap::new();
        counts.insert("lib.rs".to_string(), 7);
        let state = LearningState {
            file_edit_counts: counts,
            ..Default::default()
        };
        assert!(check_diff_thrash(&state).is_some());
    }

    #[test]
    fn diff_thrash_silent_when_all_files_below_threshold() {
        let mut counts = HashMap::new();
        counts.insert("src/main.rs".to_string(), 2);
        counts.insert("src/lib.rs".to_string(), 1);
        let state = LearningState {
            file_edit_counts: counts,
            ..Default::default()
        };
        assert!(check_diff_thrash(&state).is_none());
    }

    #[test]
    fn diff_thrash_silent_with_no_edits() {
        assert!(check_diff_thrash(&LearningState::default()).is_none());
    }

    // --- check_confidence_stagnation ----------------------------------------

    #[test]
    fn confidence_stagnation_fires_when_flat_and_low() {
        // Spread = 0.04 (< 0.05), all values < 0.5.
        let state = LearningState {
            confidence_history: vec![0.3, 0.31, 0.32, 0.33, 0.34],
            ..Default::default()
        };
        let action = check_confidence_stagnation(&state).expect("should fire");
        assert_eq!(action.severity, WatcherSeverity::Warning);
        assert_eq!(action.watcher_name, "check_confidence_stagnation");
    }

    #[test]
    fn confidence_stagnation_fires_uses_last_five_only() {
        // First entry is high/volatile; last 5 are flat and low.
        let state = LearningState {
            confidence_history: vec![0.9, 0.3, 0.31, 0.32, 0.33, 0.34],
            ..Default::default()
        };
        assert!(check_confidence_stagnation(&state).is_some());
    }

    #[test]
    fn confidence_stagnation_silent_when_values_are_high() {
        // Flat but above 0.5 — not stagnating badly enough to warn.
        let state = LearningState {
            confidence_history: vec![0.7, 0.71, 0.72, 0.73, 0.74],
            ..Default::default()
        };
        assert!(check_confidence_stagnation(&state).is_none());
    }

    #[test]
    fn confidence_stagnation_silent_when_spread_is_large() {
        // Low values but with a wide spread — agent is still moving.
        let state = LearningState {
            confidence_history: vec![0.1, 0.2, 0.3, 0.4, 0.49],
            ..Default::default()
        };
        assert!(check_confidence_stagnation(&state).is_none());
    }

    #[test]
    fn confidence_stagnation_silent_with_fewer_than_five_entries() {
        let state = LearningState {
            confidence_history: vec![0.3, 0.31, 0.32],
            ..Default::default()
        };
        assert!(check_confidence_stagnation(&state).is_none());
    }

    // --- check_scope_creep --------------------------------------------------

    #[test]
    fn scope_creep_fires_when_overlap_is_low() {
        let state = LearningState {
            original_task: Some("fix the authentication bug in login module".to_string()),
            current_task_scope: Some(
                "rewrite the entire database schema and payment processing pipeline".to_string(),
            ),
            ..Default::default()
        };
        let action = check_scope_creep(&state).expect("should fire");
        assert_eq!(action.severity, WatcherSeverity::Warning);
        assert_eq!(action.watcher_name, "check_scope_creep");
    }

    #[test]
    fn scope_creep_silent_when_overlap_is_high() {
        let state = LearningState {
            original_task: Some("fix the authentication bug in the login module".to_string()),
            current_task_scope: Some(
                "debug the authentication error in the login module handler".to_string(),
            ),
            ..Default::default()
        };
        assert!(check_scope_creep(&state).is_none());
    }

    #[test]
    fn scope_creep_silent_when_original_task_absent() {
        let state = LearningState {
            current_task_scope: Some("rewrite the payment system".to_string()),
            ..Default::default()
        };
        assert!(check_scope_creep(&state).is_none());
    }

    #[test]
    fn scope_creep_silent_when_current_scope_absent() {
        let state = LearningState {
            original_task: Some("fix the login bug".to_string()),
            ..Default::default()
        };
        assert!(check_scope_creep(&state).is_none());
    }

    #[test]
    fn scope_creep_silent_when_both_absent() {
        assert!(check_scope_creep(&LearningState::default()).is_none());
    }

    // --- check_validation_gap -----------------------------------------------

    #[test]
    fn validation_gap_fires_in_execute_phase_without_validation() {
        let state = LearningState {
            has_validation: false,
            phase: Phase::Execute,
            ..Default::default()
        };
        let action = check_validation_gap(&state).expect("should fire");
        assert_eq!(action.severity, WatcherSeverity::Force);
        assert_eq!(action.watcher_name, "check_validation_gap");
        assert_eq!(action.suggested_action.as_deref(), Some("Run validation"));
    }

    #[test]
    fn validation_gap_fires_in_diagnose_phase_without_validation() {
        let state = LearningState {
            has_validation: false,
            phase: Phase::Diagnose,
            ..Default::default()
        };
        assert!(check_validation_gap(&state).is_some());
    }

    #[test]
    fn validation_gap_silent_when_validation_present() {
        let state = LearningState {
            has_validation: true,
            phase: Phase::Execute,
            ..Default::default()
        };
        assert!(check_validation_gap(&state).is_none());
    }

    #[test]
    fn validation_gap_silent_in_explore_phase() {
        let state = LearningState {
            has_validation: false,
            phase: Phase::Explore,
            ..Default::default()
        };
        assert!(check_validation_gap(&state).is_none());
    }

    #[test]
    fn validation_gap_silent_in_plan_phase() {
        let state = LearningState {
            has_validation: false,
            phase: Phase::Plan,
            ..Default::default()
        };
        assert!(check_validation_gap(&state).is_none());
    }

    // --- ControlPlane integration -------------------------------------------

    #[test]
    fn evaluate_returns_all_fired_actions() {
        let plane = ControlPlane::new();

        let mut counts = HashMap::new();
        counts.insert("hot_file.rs".to_string(), 4);

        let state = LearningState {
            consecutive_same_errors: 3,
            file_edit_counts: counts,
            has_validation: false,
            phase: Phase::Execute,
            ..Default::default()
        };

        let actions = plane.evaluate(&state);
        // repeat_error (block) + diff_thrash (block) + validation_gap (force)
        assert!(actions.len() >= 3);
        let names: Vec<&str> = actions.iter().map(|a| a.watcher_name).collect();
        assert!(names.contains(&"check_repeat_error"));
        assert!(names.contains(&"check_diff_thrash"));
        assert!(names.contains(&"check_validation_gap"));
    }

    #[test]
    fn evaluate_returns_empty_for_clean_state() {
        let plane = ControlPlane::new();
        let state = LearningState {
            has_validation: true,
            phase: Phase::Execute,
            ..Default::default()
        };
        assert!(plane.evaluate(&state).is_empty());
    }

    #[test]
    fn has_block_true_when_block_watcher_fires() {
        let plane = ControlPlane::new();
        let state = LearningState {
            consecutive_same_errors: 2,
            ..Default::default()
        };
        assert!(plane.has_block(&state));
    }

    #[test]
    fn has_block_false_when_only_warnings_fire() {
        let plane = ControlPlane::new();
        // Only no_new_info (Warning) should fire.
        let state = LearningState {
            steps_without_new_evidence: 10,
            has_validation: true,
            phase: Phase::Explore,
            ..Default::default()
        };
        assert!(!plane.has_block(&state));
    }

    #[test]
    fn has_block_false_for_clean_state() {
        let plane = ControlPlane::new();
        let state = LearningState {
            has_validation: true,
            ..Default::default()
        };
        assert!(!plane.has_block(&state));
    }

    // --- Helpers ------------------------------------------------------------

    #[test]
    fn jaccard_identical_sets_returns_one() {
        let a: HashSet<String> = ["apple", "banana"].iter().map(|s| s.to_string()).collect();
        let b = a.clone();
        assert!((jaccard_similarity(&a, &b) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_disjoint_sets_returns_zero() {
        let a: HashSet<String> = ["apple"].iter().map(|s| s.to_string()).collect();
        let b: HashSet<String> = ["orange"].iter().map(|s| s.to_string()).collect();
        assert!((jaccard_similarity(&a, &b)).abs() < f64::EPSILON);
    }

    #[test]
    fn jaccard_empty_sets_returns_one() {
        let a: HashSet<String> = HashSet::new();
        let b: HashSet<String> = HashSet::new();
        assert!((jaccard_similarity(&a, &b) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn word_set_lowercases_and_deduplicates() {
        let set = word_set("The the THE fox");
        assert_eq!(set.len(), 2); // "the" and "fox"
        assert!(set.contains("the"));
        assert!(set.contains("fox"));
    }
}
