//! Phase state machine for tracking the current activity phase of a worker.
//!
//! The phase is inferred heuristically from tool usage patterns, task text
//! keywords, and external signals (escape protocol, idle timeout, failures).
//! It drives the advisory boost matrix, control plane watcher activation,
//! and distillation context selection.

use crate::learning::Phase;

use std::collections::VecDeque;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum entries retained in the phase transition history.
const HISTORY_CAP: usize = 20;

/// Rolling window size for tool dominance detection.
const RECENT_TOOLS_CAP: usize = 5;

/// Fraction of classifiable recent tools that must map to a candidate phase
/// before the state machine commits to that phase.
const DOMINANCE_THRESHOLD: f64 = 0.60;

/// Seconds of inactivity before the state machine transitions to Halt.
const IDLE_HALT_SECS: u64 = 300;

/// Consecutive failures required to force a transition into Diagnose.
const FAILURE_DIAGNOSE_THRESHOLD: u32 = 2;

// ---------------------------------------------------------------------------
// PhaseState
// ---------------------------------------------------------------------------

/// Tracks the current activity phase, transition history, and failure count.
///
/// Call [`PhaseState::detect_phase`] on every tool invocation. Hard-override
/// conditions (failures, escape, idle) always win. Tool and keyword heuristics
/// apply next, gated by a dominance check over the recent tool window.
#[derive(Debug, Clone)]
pub struct PhaseState {
    current: Phase,
    previous: Phase,
    entered_at: Instant,
    history: VecDeque<(Phase, Instant)>,
    consecutive_failures: u32,
    recent_tools: VecDeque<String>,
}

impl PhaseState {
    /// Create a new phase state starting at Explore.
    pub fn new() -> Self {
        Self {
            current: Phase::Explore,
            previous: Phase::Explore,
            entered_at: Instant::now(),
            history: VecDeque::new(),
            consecutive_failures: 0,
            recent_tools: VecDeque::new(),
        }
    }

    /// Current activity phase.
    pub fn current(&self) -> Phase {
        self.current
    }

    /// Phase prior to the most recent transition.
    pub fn previous(&self) -> Phase {
        self.previous
    }

    /// Number of consecutive failures recorded without an intervening clear.
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// How long the current phase has been active.
    pub fn time_in_phase(&self) -> Duration {
        self.entered_at.elapsed()
    }

    /// Number of entries in the transition history (capped at [`HISTORY_CAP`]).
    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    /// Record a tool or task failure, incrementing the consecutive counter.
    pub fn record_failure(&mut self) {
        self.consecutive_failures += 1;
    }

    /// Reset the consecutive failure counter after a successful step.
    pub fn clear_failures(&mut self) {
        self.consecutive_failures = 0;
    }

    /// Transition to a new phase, archiving the current one in history.
    ///
    /// Transitions to the same phase are ignored to keep the entered_at
    /// timestamp stable and avoid noise in the history log.
    pub fn transition(&mut self, new_phase: Phase) {
        if new_phase == self.current {
            return;
        }
        self.previous = self.current;
        self.history.push_back((self.current, self.entered_at));
        if self.history.len() > HISTORY_CAP {
            self.history.pop_front();
        }
        self.current = new_phase;
        self.entered_at = Instant::now();
    }

    /// Update phase based on tool usage and contextual signals.
    ///
    /// Priority order:
    /// 1. Hard overrides — failures, escape, idle — always win.
    /// 2. Tool classification using `tool_name` and `args_summary`.
    /// 3. Keyword signals from `task_text`.
    /// 4. Dominance check — the candidate must account for >60 % of the
    ///    classifiable entries in the recent tool window.
    ///
    /// If `is_failure` is true the consecutive failure counter is incremented
    /// before override conditions are evaluated, so two back-to-back failures
    /// in successive detect_phase calls will trigger Diagnose.
    pub fn detect_phase(
        &mut self,
        tool_name: &str,
        args_summary: Option<&str>,
        task_text: Option<&str>,
        is_failure: bool,
        escape_triggered: bool,
        last_activity_secs: u64,
    ) {
        if is_failure {
            self.consecutive_failures += 1;
        }

        // Hard overrides — checked before tool tracking or heuristics.
        if self.consecutive_failures >= FAILURE_DIAGNOSE_THRESHOLD {
            self.transition(Phase::Diagnose);
            return;
        }
        if escape_triggered {
            self.transition(Phase::Escalate);
            return;
        }
        if last_activity_secs >= IDLE_HALT_SECS {
            self.transition(Phase::Halt);
            return;
        }

        // Append to the rolling tool window.
        self.recent_tools.push_back(tool_name.to_string());
        if self.recent_tools.len() > RECENT_TOOLS_CAP {
            self.recent_tools.pop_front();
        }

        // Tool classification takes precedence over keyword signals.
        let candidate = classify_tool(tool_name, args_summary)
            .or_else(|| task_text.and_then(classify_task_text));

        let Some(candidate_phase) = candidate else {
            return;
        };

        // Commit only when the candidate dominates the recent tool window.
        if self.phase_dominates(candidate_phase) {
            self.transition(candidate_phase);
        }
    }

    /// Whether `phase` accounts for more than [`DOMINANCE_THRESHOLD`] of the
    /// classifiable entries in the recent tool window.
    ///
    /// Ambiguous tools (shell, exec without args) are excluded from both the
    /// numerator and denominator. When no classifiable tools are present the
    /// check always passes — there is no evidence against the candidate.
    fn phase_dominates(&self, phase: Phase) -> bool {
        let classified: Vec<Phase> = self
            .recent_tools
            .iter()
            .filter_map(|tool| classify_tool_by_name(tool))
            .collect();

        if classified.is_empty() {
            // No classifiable tools yet — allow the candidate through.
            return true;
        }

        let matching = classified.iter().filter(|&&p| p == phase).count();
        (matching as f64 / classified.len() as f64) > DOMINANCE_THRESHOLD
    }
}

impl Default for PhaseState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tool and keyword classification helpers
// ---------------------------------------------------------------------------

/// Shell commands that indicate read-only exploration.
const READ_ONLY_COMMANDS: &[&str] = &["rg", "ls", "find", "cat", "grep", "head", "tail"];

/// Test runner invocations that indicate a validation phase.
const TEST_COMMANDS: &[&str] = &["cargo test", "pytest", "npm test", "go test"];

/// Classify a single tool call into a phase using both name and args.
///
/// Returns `None` for ambiguous invocations (e.g., bare `shell` with no
/// recognisable pattern in `args_summary`).
fn classify_tool(tool_name: &str, args_summary: Option<&str>) -> Option<Phase> {
    let name = tool_name.trim().to_ascii_lowercase();
    let args = args_summary.unwrap_or("").to_ascii_lowercase();

    // Unambiguous by name.
    match name.as_str() {
        "web_search" | "browser" => return Some(Phase::Explore),
        _ => {}
    }

    // File write tools — write appears in the tool name itself.
    if name.contains("write") {
        return Some(Phase::Execute);
    }

    // Shell and exec are classified by their args.
    if name == "shell" || name == "exec" {
        // Test commands take precedence over read-only patterns.
        if TEST_COMMANDS.iter().any(|cmd| args.contains(cmd)) {
            return Some(Phase::Validate);
        }
        // Match the leading token of the command against read-only commands.
        let first_token = args.split_whitespace().next().unwrap_or("");
        if READ_ONLY_COMMANDS.contains(&first_token) {
            return Some(Phase::Explore);
        }
        return None;
    }

    None
}

/// Classify a tool by name only, used when scanning the recent tool window.
///
/// Less precise than [`classify_tool`] because args are not stored in the
/// window, but sufficient for trend detection across the rolling buffer.
fn classify_tool_by_name(tool_name: &str) -> Option<Phase> {
    let name = tool_name.trim().to_ascii_lowercase();
    match name.as_str() {
        "web_search" | "browser" => Some(Phase::Explore),
        name if name.contains("write") => Some(Phase::Execute),
        // shell/exec are ambiguous without args — excluded from dominance scoring.
        _ => None,
    }
}

/// Derive a phase from keywords in `task_text`.
///
/// Returns `None` when no keyword matches, so the caller can fall back to
/// other signals. Checked in specificity order — first match wins.
fn classify_task_text(task_text: &str) -> Option<Phase> {
    let lower = task_text.to_ascii_lowercase();

    if lower.contains("refactor") || lower.contains("simplify") || lower.contains("clean") {
        return Some(Phase::Simplify);
    }
    if lower.contains("consolidate") || lower.contains("summary") {
        return Some(Phase::Consolidate);
    }
    if lower.contains("plan") || lower.contains("approach") || lower.contains("design") {
        return Some(Phase::Plan);
    }

    None
}

// ---------------------------------------------------------------------------
// Phase boost matrix
// ---------------------------------------------------------------------------

/// Score multiplier for `category` advisories when the worker is in `phase`.
///
/// Applies to the six advisory categories: `self_awareness`, `wisdom`,
/// `reasoning`, `user_model`, `domain_expertise`, `context`. Returns `1.0`
/// for unknown category names so unrecognised categories are never suppressed.
pub fn phase_boost(phase: Phase, category: &str) -> f64 {
    // Columns: self_awareness, wisdom, reasoning, user_model, domain_expertise, context.
    let (sa, w, r, um, de, c) = match phase {
        Phase::Explore     => (1.0, 1.0, 1.2, 1.0, 1.5, 1.2),
        Phase::Plan        => (1.0, 1.5, 1.5, 1.0, 1.2, 1.0),
        Phase::Execute     => (1.2, 1.0, 1.0, 1.0, 1.2, 1.0),
        Phase::Validate    => (1.0, 1.0, 1.5, 1.0, 1.0, 1.0),
        Phase::Consolidate => (1.0, 1.2, 1.0, 1.0, 1.0, 1.0),
        Phase::Diagnose    => (1.5, 1.2, 1.5, 1.0, 1.2, 1.0),
        Phase::Simplify    => (1.2, 1.0, 1.2, 1.0, 1.0, 1.0),
        Phase::Escalate    => (1.0, 1.5, 1.0, 1.0, 1.0, 1.2),
        Phase::Halt        => (1.0, 1.0, 1.0, 1.0, 1.0, 1.0),
    };

    match category {
        "self_awareness"   => sa,
        "wisdom"           => w,
        "reasoning"        => r,
        "user_model"       => um,
        "domain_expertise" => de,
        "context"          => c,
        _                  => 1.0,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- PhaseState construction --

    #[test]
    fn test_initial_state() {
        let state = PhaseState::new();
        assert_eq!(state.current(), Phase::Explore);
        assert_eq!(state.previous(), Phase::Explore);
        assert_eq!(state.consecutive_failures(), 0);
    }

    #[test]
    fn test_default_matches_new() {
        let a = PhaseState::new();
        let b = PhaseState::default();
        assert_eq!(a.current(), b.current());
        assert_eq!(a.previous(), b.previous());
    }

    // -- Transition --

    #[test]
    fn test_transition_updates_current_and_previous() {
        let mut state = PhaseState::new();
        state.transition(Phase::Plan);
        assert_eq!(state.current(), Phase::Plan);
        assert_eq!(state.previous(), Phase::Explore);
    }

    #[test]
    fn test_transition_to_same_phase_is_noop() {
        let mut state = PhaseState::new();
        state.transition(Phase::Explore);
        // History remains empty — we started in Explore and never left.
        assert_eq!(state.history_len(), 0);
        assert_eq!(state.current(), Phase::Explore);
    }

    #[test]
    fn test_history_is_capped_at_20() {
        let mut state = PhaseState::new();
        // 22 distinct phases cycling through two patterns pushes well past cap.
        let phases = [
            Phase::Plan, Phase::Execute, Phase::Validate, Phase::Consolidate,
            Phase::Diagnose, Phase::Simplify, Phase::Escalate, Phase::Halt,
            Phase::Explore, Phase::Plan, Phase::Execute, Phase::Validate,
            Phase::Consolidate, Phase::Diagnose, Phase::Simplify, Phase::Escalate,
            Phase::Halt, Phase::Explore, Phase::Plan, Phase::Execute,
            Phase::Validate, Phase::Consolidate,
        ];
        for phase in phases {
            state.transition(phase);
        }
        assert!(state.history_len() <= HISTORY_CAP);
    }

    // -- Failure tracking --

    #[test]
    fn test_record_and_clear_failures() {
        let mut state = PhaseState::new();
        state.record_failure();
        assert_eq!(state.consecutive_failures(), 1);
        state.record_failure();
        assert_eq!(state.consecutive_failures(), 2);
        state.clear_failures();
        assert_eq!(state.consecutive_failures(), 0);
    }

    // -- Hard override conditions --

    #[test]
    fn test_two_failures_trigger_diagnose() {
        let mut state = PhaseState::new();
        state.record_failure();
        state.record_failure();
        state.detect_phase("shell", Some("rg src/"), None, false, false, 0);
        assert_eq!(state.current(), Phase::Diagnose);
    }

    #[test]
    fn test_is_failure_increments_counter() {
        let mut state = PhaseState::new();
        // Two detect_phase calls with is_failure=true should trip the threshold.
        state.detect_phase("shell", None, None, true, false, 0);
        state.detect_phase("shell", None, None, true, false, 0);
        assert_eq!(state.current(), Phase::Diagnose);
    }

    #[test]
    fn test_escape_triggers_escalate() {
        let mut state = PhaseState::new();
        state.detect_phase("shell", None, None, false, true, 0);
        assert_eq!(state.current(), Phase::Escalate);
    }

    #[test]
    fn test_idle_triggers_halt() {
        let mut state = PhaseState::new();
        state.detect_phase("shell", None, None, false, false, IDLE_HALT_SECS);
        assert_eq!(state.current(), Phase::Halt);
    }

    #[test]
    fn test_failure_override_beats_escape() {
        // Failures are checked first, so Diagnose wins over Escalate.
        let mut state = PhaseState::new();
        state.record_failure();
        state.record_failure();
        state.detect_phase("shell", None, None, false, true, 0);
        assert_eq!(state.current(), Phase::Diagnose);
    }

    // -- Tool pattern detection --

    #[test]
    fn test_web_search_signals_explore() {
        let mut state = PhaseState::new();
        // Single call — window has one classifiable entry (Explore), dominates.
        state.detect_phase("web_search", None, None, false, false, 0);
        assert_eq!(state.current(), Phase::Explore);
    }

    #[test]
    fn test_browser_signals_explore() {
        let mut state = PhaseState::new();
        state.detect_phase("browser", None, None, false, false, 0);
        assert_eq!(state.current(), Phase::Explore);
    }

    #[test]
    fn test_shell_read_only_signals_explore() {
        // shell/exec are ambiguous by name so the window has zero classifiable
        // entries; dominance check passes and the tool-arg candidate wins.
        for cmd in ["rg src/", "ls -la", "grep -r foo .", "find . -name '*.rs'",
                    "cat README.md", "head -n 20 foo.txt", "tail -f log.txt"]
        {
            let mut state = PhaseState::new();
            state.detect_phase("shell", Some(cmd), None, false, false, 0);
            assert_eq!(state.current(), Phase::Explore, "cmd={cmd}");
        }
    }

    #[test]
    fn test_shell_test_commands_signal_validate() {
        for cmd in ["cargo test --all", "pytest tests/", "npm test", "go test ./..."] {
            let mut state = PhaseState::new();
            state.detect_phase("shell", Some(cmd), None, false, false, 0);
            assert_eq!(state.current(), Phase::Validate, "cmd={cmd}");
        }
    }

    #[test]
    fn test_file_write_tool_signals_execute() {
        for name in ["write_file", "file_write", "write"] {
            let mut state = PhaseState::new();
            state.detect_phase(name, None, None, false, false, 0);
            assert_eq!(state.current(), Phase::Execute, "tool={name}");
        }
    }

    #[test]
    fn test_exec_test_command_signals_validate() {
        let mut state = PhaseState::new();
        state.detect_phase("exec", Some("pytest tests/unit"), None, false, false, 0);
        assert_eq!(state.current(), Phase::Validate);
    }

    // -- Keyword detection --

    #[test]
    fn test_task_text_plan_keywords() {
        for text in ["let's plan the approach", "design the architecture", "approach this problem"] {
            assert_eq!(classify_task_text(text), Some(Phase::Plan), "text={text}");
        }
    }

    #[test]
    fn test_task_text_simplify_keywords() {
        for text in ["refactor this module", "simplify the logic", "clean up the code"] {
            assert_eq!(classify_task_text(text), Some(Phase::Simplify), "text={text}");
        }
    }

    #[test]
    fn test_task_text_consolidate_keywords() {
        for text in ["consolidate the findings", "write a summary"] {
            assert_eq!(classify_task_text(text), Some(Phase::Consolidate), "text={text}");
        }
    }

    #[test]
    fn test_task_text_no_match_returns_none() {
        assert_eq!(classify_task_text("fix the bug"), None);
        assert_eq!(classify_task_text(""), None);
        assert_eq!(classify_task_text("run the tests"), None);
    }

    #[test]
    fn test_task_text_simplify_beats_plan_when_both_present() {
        // "simplify" is checked before "plan" — simplify wins.
        assert_eq!(
            classify_task_text("simplify and plan the approach"),
            Some(Phase::Simplify),
        );
    }

    // -- Dominance window --

    #[test]
    fn test_dominance_window_prevents_single_outlier_flip() {
        let mut state = PhaseState::new();
        // Establish Explore dominance with write_file calls so window is
        // full of Execute-classified tools, then try a single web_search.
        for _ in 0..4 {
            state.detect_phase("write_file", None, None, false, false, 0);
        }
        assert_eq!(state.current(), Phase::Execute);
        // One web_search should not flip — Execute still dominates (4/5 = 80 %).
        state.detect_phase("web_search", None, None, false, false, 0);
        assert_eq!(state.current(), Phase::Execute);
    }

    #[test]
    fn test_dominance_window_flips_when_majority_shifts() {
        let mut state = PhaseState::new();
        // Seed three web_search (Explore) then two write_file (Execute).
        state.detect_phase("web_search", None, None, false, false, 0); // [ws]
        state.detect_phase("web_search", None, None, false, false, 0); // [ws, ws]
        state.detect_phase("web_search", None, None, false, false, 0); // [ws, ws, ws]
        assert_eq!(state.current(), Phase::Explore);

        state.detect_phase("write_file", None, None, false, false, 0); // [ws, ws, ws, wf]
        state.detect_phase("write_file", None, None, false, false, 0); // [ws, ws, ws, wf, wf]
        // Explore: 3/5 = 60 % — NOT strictly greater than threshold.
        // Execute: 2/5 = 40 % — also below threshold.
        // Neither dominates, so no new transition.
        assert_eq!(state.current(), Phase::Explore);

        // One more write_file pushes window to [ws, ws, wf, wf, wf] → Execute 3/5 = 60 % — still not >.
        state.detect_phase("write_file", None, None, false, false, 0);
        // Still Explore because 3/5 is not strictly > 0.60.
        assert_eq!(state.current(), Phase::Explore);

        // Four write_files → [wf, wf, wf, wf, ws] Execute 4/5 = 80 % > 60 % → flip.
        state.detect_phase("write_file", None, None, false, false, 0);
        assert_eq!(state.current(), Phase::Execute);
    }

    // -- time_in_phase --

    #[test]
    fn test_time_in_phase_is_non_negative() {
        let state = PhaseState::new();
        assert!(state.time_in_phase() >= Duration::ZERO);
    }

    // -- phase_boost matrix --

    #[test]
    fn test_phase_boost_all_halt_categories_are_one() {
        let categories = [
            "self_awareness", "wisdom", "reasoning",
            "user_model", "domain_expertise", "context",
        ];
        for category in categories {
            assert_eq!(
                phase_boost(Phase::Halt, category), 1.0,
                "Halt/{category} should be 1.0",
            );
        }
    }

    #[test]
    fn test_phase_boost_spot_checks() {
        assert_eq!(phase_boost(Phase::Explore,     "domain_expertise"), 1.5);
        assert_eq!(phase_boost(Phase::Explore,     "context"),          1.2);
        assert_eq!(phase_boost(Phase::Plan,        "wisdom"),           1.5);
        assert_eq!(phase_boost(Phase::Plan,        "reasoning"),        1.5);
        assert_eq!(phase_boost(Phase::Execute,     "self_awareness"),   1.2);
        assert_eq!(phase_boost(Phase::Execute,     "domain_expertise"), 1.2);
        assert_eq!(phase_boost(Phase::Validate,    "reasoning"),        1.5);
        assert_eq!(phase_boost(Phase::Consolidate, "wisdom"),           1.2);
        assert_eq!(phase_boost(Phase::Diagnose,    "self_awareness"),   1.5);
        assert_eq!(phase_boost(Phase::Diagnose,    "reasoning"),        1.5);
        assert_eq!(phase_boost(Phase::Diagnose,    "wisdom"),           1.2);
        assert_eq!(phase_boost(Phase::Diagnose,    "domain_expertise"), 1.2);
        assert_eq!(phase_boost(Phase::Simplify,    "self_awareness"),   1.2);
        assert_eq!(phase_boost(Phase::Simplify,    "reasoning"),        1.2);
        assert_eq!(phase_boost(Phase::Escalate,    "wisdom"),           1.5);
        assert_eq!(phase_boost(Phase::Escalate,    "context"),          1.2);
    }

    #[test]
    fn test_phase_boost_neutral_columns_are_one() {
        // user_model should be 1.0 for every phase.
        for phase in [
            Phase::Explore, Phase::Plan, Phase::Execute, Phase::Validate,
            Phase::Consolidate, Phase::Diagnose, Phase::Simplify,
            Phase::Escalate, Phase::Halt,
        ] {
            assert_eq!(phase_boost(phase, "user_model"), 1.0, "{phase}/user_model");
        }
    }

    #[test]
    fn test_phase_boost_unknown_category_returns_one() {
        assert_eq!(phase_boost(Phase::Diagnose, "unknown_category"), 1.0);
        assert_eq!(phase_boost(Phase::Plan, ""),                     1.0);
    }
}
