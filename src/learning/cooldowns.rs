//! Cooldown mechanisms and suppression rules for learning advisories.
//!
//! `CooldownManager` tracks per-tool and per-advice cooldown windows, within-cycle
//! deduplication, content-hash deduplication, and context-driven obviousness rules.
//! Every advisory candidate passes through `is_suppressed` before being emitted;
//! callers record successful emissions with `record_emission` so future candidates
//! can be compared against prior output.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::time::Instant;

/// Window in seconds within which a read event suppresses a read-before-write advisory.
const READ_BEFORE_WRITE_WINDOW_SECS: u64 = 120;

/// TTL in seconds for content-hash entries used to suppress duplicate advisory wording.
///
/// One hour covers multiple bulletin cycles without being so long that genuinely
/// re-relevant advice stays hidden after context changes.
const TEXT_HASH_TTL_SECS: u64 = 3600;

/// Reason an advisory candidate was suppressed instead of being emitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuppressionReason {
    /// The same tool produced an advisory within the per-tool cooldown window.
    ToolCooldown,
    /// The same advice ID was emitted within the per-advice cooldown window.
    AdviceCooldown,
    /// The same advice ID was already emitted during the current bulletin cycle.
    CycleDedup,
    /// Advisory text with this content hash was emitted recently.
    TextDedup,
    /// The advisory is obvious from surrounding context and would add noise.
    ObviousFromContext(String),
}

/// Tracks cooldowns, deduplication state, and suppression rules for learning advisories.
///
/// A single `CooldownManager` is owned per bulletin cycle runner. It is reset between
/// cycles with `reset_cycle` and cleaned up periodically with `cleanup_expired`.
pub struct CooldownManager {
    /// Last time any advisory was emitted for a given tool name.
    tool_cooldowns: HashMap<String, Instant>,
    /// Last time a given advice ID was emitted, across all tools.
    advice_cooldowns: HashMap<String, Instant>,
    /// Advice IDs emitted during the current bulletin cycle; cleared by `reset_cycle`.
    emitted_this_cycle: HashSet<String>,
    /// Maps a content hash to the last time that exact advisory text was emitted.
    ///
    /// Prevents the same wording from surfacing repeatedly even when the advice ID
    /// changes (e.g., two chips producing semantically identical advice).
    advice_text_hashes: HashMap<u64, Instant>,
    /// Tracks files that were recently read, enabling read-before-write suppression.
    recent_file_reads: HashMap<String, Instant>,
}

impl CooldownManager {
    /// Creates a new `CooldownManager` with all tracking state empty.
    pub fn new() -> Self {
        Self {
            tool_cooldowns: HashMap::new(),
            advice_cooldowns: HashMap::new(),
            emitted_this_cycle: HashSet::new(),
            advice_text_hashes: HashMap::new(),
            recent_file_reads: HashMap::new(),
        }
    }

    /// Returns the suppression reason if the advisory should be skipped, or `None` if
    /// it should be emitted.
    ///
    /// Checks are ordered from cheapest to most context-dependent so common cases
    /// (tool cooldown) short-circuit before the more expensive obviousness checks.
    pub fn is_suppressed(
        &self,
        advice_id: &str,
        tool_name: &str,
        advice_text: &str,
        phase: &str,
        cooldown_per_tool_secs: u64,
        cooldown_per_advice_secs: u64,
    ) -> Option<SuppressionReason> {
        if let Some(last_tool_time) = self.tool_cooldowns.get(tool_name) {
            if last_tool_time.elapsed().as_secs() < cooldown_per_tool_secs {
                return Some(SuppressionReason::ToolCooldown);
            }
        }

        if let Some(last_advice_time) = self.advice_cooldowns.get(advice_id) {
            if last_advice_time.elapsed().as_secs() < cooldown_per_advice_secs {
                return Some(SuppressionReason::AdviceCooldown);
            }
        }

        if self.emitted_this_cycle.contains(advice_id) {
            return Some(SuppressionReason::CycleDedup);
        }

        let text_hash = Self::hash_text(advice_text);
        if let Some(last_hash_time) = self.advice_text_hashes.get(&text_hash) {
            if last_hash_time.elapsed().as_secs() < TEXT_HASH_TTL_SECS {
                return Some(SuppressionReason::TextDedup);
            }
        }

        if let Some(reason) = self.is_obvious(advice_text, tool_name, phase) {
            return Some(SuppressionReason::ObviousFromContext(reason));
        }

        None
    }

    /// Records that an advisory was emitted, updating all cooldown and dedup maps.
    pub fn record_emission(&mut self, advice_id: &str, tool_name: &str, advice_text: &str) {
        let now = Instant::now();
        self.tool_cooldowns.insert(tool_name.to_string(), now);
        self.advice_cooldowns.insert(advice_id.to_string(), now);
        self.emitted_this_cycle.insert(advice_id.to_string());
        let text_hash = Self::hash_text(advice_text);
        self.advice_text_hashes.insert(text_hash, now);
    }

    /// Clears within-cycle deduplication state. Call at the start of each new bulletin cycle.
    ///
    /// Per-tool and per-advice cooldown windows are not affected; those persist across
    /// cycles for their configured TTL.
    pub fn reset_cycle(&mut self) {
        self.emitted_this_cycle.clear();
    }

    /// Records that a file was read, enabling read-before-write obviousness suppression.
    pub fn record_file_read(&mut self, file_path: &str) {
        self.recent_file_reads.insert(file_path.to_string(), Instant::now());
    }

    /// Removes entries older than `max_age_secs` from all time-indexed maps.
    ///
    /// The within-cycle set is managed by `reset_cycle` and is not affected here.
    pub fn cleanup_expired(&mut self, max_age_secs: u64) {
        self.tool_cooldowns
            .retain(|_, instant| instant.elapsed().as_secs() < max_age_secs);
        self.advice_cooldowns
            .retain(|_, instant| instant.elapsed().as_secs() < max_age_secs);
        self.advice_text_hashes
            .retain(|_, instant| instant.elapsed().as_secs() < max_age_secs);
        self.recent_file_reads
            .retain(|_, instant| instant.elapsed().as_secs() < max_age_secs);
    }

    /// Returns a human-readable suppression explanation if the advisory is obvious from
    /// context, or `None` if it should proceed.
    ///
    /// Each rule targets a class of advice that would be noise because the agent has
    /// already demonstrated the correct behaviour or is in a phase where the advice
    /// does not apply.
    fn is_obvious(&self, advice_text: &str, tool_name: &str, phase: &str) -> Option<String> {
        let advice_lower = advice_text.to_lowercase();
        let tool_lower = tool_name.to_lowercase();

        // Read-before-write: if the agent already read a file mentioned in the advisory
        // within the tracking window, reminding it to read first is redundant.
        if tool_lower.contains("write") || tool_lower.contains("file") {
            for (file_path, last_read) in &self.recent_file_reads {
                if last_read.elapsed().as_secs() < READ_BEFORE_WRITE_WINDOW_SECS
                    && advice_text.contains(file_path.as_str())
                {
                    return Some(format!(
                        "{file_path} was read {:.0}s ago; read-before-write is already satisfied",
                        last_read.elapsed().as_secs_f64()
                    ));
                }
            }
        }

        // Deployment warnings are not actionable during Explore — the agent is gathering
        // information, not making changes, so surfacing them creates unnecessary alarm.
        if phase.eq_ignore_ascii_case("explore")
            && (advice_lower.contains("deploy") || advice_lower.contains("production"))
        {
            return Some(
                "deployment warnings are not applicable during the Explore phase".to_string(),
            );
        }

        // Meta-constraints like "plan first" are valuable at planning time but become
        // noise when emitted mid-execution against tools that have no planning affordance.
        let is_meta_constraint = advice_lower.contains("plan first")
            || advice_lower.contains("before implementing")
            || advice_lower.contains("define requirements");
        let is_planning_tool = tool_lower.contains("plan")
            || tool_lower.contains("think")
            || tool_lower.contains("brainstorm");
        if is_meta_constraint && !is_planning_tool {
            return Some(
                "meta-constraint advisories are suppressed on non-planning tools".to_string(),
            );
        }

        // Shell safety advisories (quoting, injection, escaping) only apply when the
        // agent is actually invoking shell commands. Surfacing them on file or browser
        // tools produces irrelevant noise.
        let is_shell_safety_advice = advice_lower.contains("shell")
            && (advice_lower.contains("quot")
                || advice_lower.contains("inject")
                || advice_lower.contains("escap"));
        let is_shell_tool = tool_lower.contains("shell")
            || tool_lower.contains("bash")
            || tool_lower.contains("exec");
        if is_shell_safety_advice && !is_shell_tool {
            return Some(format!(
                "shell safety advice is not applicable to the {tool_name} tool"
            ));
        }

        None
    }

    /// Computes a stable 64-bit hash of `text` for content-based deduplication.
    fn hash_text(text: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        hasher.finish()
    }
}

impl Default for CooldownManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_manager() -> CooldownManager {
        CooldownManager::new()
    }

    // -----------------------------------------------------------------------
    // Baseline: fresh manager suppresses nothing
    // -----------------------------------------------------------------------

    #[test]
    fn new_manager_suppresses_nothing() {
        let manager = make_manager();
        let result =
            manager.is_suppressed("advice-1", "shell", "avoid rm -rf /", "execute", 60, 300);
        assert!(result.is_none());
    }

    // -----------------------------------------------------------------------
    // ToolCooldown
    // -----------------------------------------------------------------------

    #[test]
    fn tool_cooldown_suppresses_same_tool_within_window() {
        let mut manager = make_manager();
        manager.record_emission("advice-1", "shell", "avoid rm -rf /");

        // Immediately after emission the tool is on cooldown.
        let result =
            manager.is_suppressed("advice-2", "shell", "different advice text", "execute", 60, 300);
        assert_eq!(result, Some(SuppressionReason::ToolCooldown));
    }

    #[test]
    fn tool_cooldown_does_not_suppress_different_tool() {
        let mut manager = make_manager();
        manager.record_emission("advice-1", "shell", "avoid rm -rf /");

        // The file tool was never emitted so it has no cooldown.
        let result =
            manager.is_suppressed("advice-2", "file", "check the target path", "execute", 60, 300);
        assert!(result.is_none());
    }

    #[test]
    fn tool_cooldown_does_not_suppress_when_window_is_zero() {
        let mut manager = make_manager();
        manager.record_emission("advice-1", "shell", "avoid rm -rf /");

        // A zero-second window means the cooldown is always expired.
        let result =
            manager.is_suppressed("advice-2", "shell", "different advice text", "execute", 0, 0);
        // CycleDedup fires before ObviousFromContext; advice-2 is new so no cycle hit.
        // tool_cooldown_secs = 0 → elapsed (≥0) is never < 0, so no ToolCooldown.
        assert_ne!(result, Some(SuppressionReason::ToolCooldown));
    }

    // -----------------------------------------------------------------------
    // AdviceCooldown
    // -----------------------------------------------------------------------

    #[test]
    fn advice_cooldown_suppresses_same_id_within_window() {
        let mut manager = make_manager();
        manager.record_emission("safety-001", "file", "read before writing");

        // Different tool, same advice_id — per-advice cooldown fires.
        // Use tool_cooldown_secs = 0 to skip tool cooldown on the new tool.
        let result =
            manager.is_suppressed("safety-001", "exec", "read before writing", "execute", 0, 300);
        assert_eq!(result, Some(SuppressionReason::AdviceCooldown));
    }

    #[test]
    fn advice_cooldown_does_not_suppress_different_id() {
        let mut manager = make_manager();
        manager.record_emission("safety-001", "shell", "read before writing");

        let result =
            manager.is_suppressed("safety-002", "exec", "a different advisory", "execute", 0, 300);
        assert_ne!(result, Some(SuppressionReason::AdviceCooldown));
    }

    // -----------------------------------------------------------------------
    // CycleDedup
    // -----------------------------------------------------------------------

    #[test]
    fn cycle_dedup_suppresses_second_emission_in_same_cycle() {
        let mut manager = make_manager();
        manager.record_emission("tip-42", "shell", "use set -e in scripts");

        // Different tool, zero cooldowns — only the cycle dedup should fire.
        let result =
            manager.is_suppressed("tip-42", "exec", "use set -e variant text", "execute", 0, 0);
        assert_eq!(result, Some(SuppressionReason::CycleDedup));
    }

    #[test]
    fn reset_cycle_clears_cycle_dedup_state() {
        let mut manager = make_manager();
        manager.record_emission("tip-42", "shell", "use set -e in scripts");
        manager.reset_cycle();

        // After reset, cycle dedup no longer applies for this advice_id.
        let result =
            manager.is_suppressed("tip-42", "exec", "use set -e variant text", "execute", 0, 0);
        assert_ne!(result, Some(SuppressionReason::CycleDedup));
    }

    #[test]
    fn reset_cycle_preserves_tool_and_advice_cooldowns() {
        let mut manager = make_manager();
        manager.record_emission("tip-42", "shell", "use set -e in scripts");
        manager.reset_cycle();

        // Tool cooldown for "shell" was set by record_emission and persists across reset.
        let result =
            manager.is_suppressed("tip-42", "shell", "use set -e variant text", "execute", 300, 0);
        assert_eq!(result, Some(SuppressionReason::ToolCooldown));
    }

    // -----------------------------------------------------------------------
    // TextDedup
    // -----------------------------------------------------------------------

    #[test]
    fn text_dedup_suppresses_identical_advice_wording() {
        let mut manager = make_manager();
        manager.record_emission("advice-a", "shell", "always quote shell variables");

        // Different advice_id, different tool, zero cooldowns — only TextDedup fires.
        let result = manager.is_suppressed(
            "advice-b",
            "exec",
            "always quote shell variables",
            "execute",
            0,
            0,
        );
        assert_eq!(result, Some(SuppressionReason::TextDedup));
    }

    #[test]
    fn text_dedup_does_not_suppress_different_wording() {
        let mut manager = make_manager();
        manager.record_emission("advice-a", "shell", "always quote shell variables");

        let result = manager.is_suppressed(
            "advice-b",
            "exec",
            "prefer explicit error handling",
            "execute",
            0,
            0,
        );
        assert_ne!(result, Some(SuppressionReason::TextDedup));
    }

    // -----------------------------------------------------------------------
    // ObviousFromContext — deployment warning in Explore phase
    // -----------------------------------------------------------------------

    #[test]
    fn deployment_warning_suppressed_in_explore_phase() {
        let manager = make_manager();
        let result = manager.is_suppressed(
            "deploy-warn",
            "file",
            "avoid pushing untested changes to production",
            "Explore",
            0,
            0,
        );
        assert!(matches!(result, Some(SuppressionReason::ObviousFromContext(_))));
    }

    #[test]
    fn deployment_warning_not_suppressed_in_execute_phase() {
        let manager = make_manager();
        let result = manager.is_suppressed(
            "deploy-warn",
            "shell",
            "avoid pushing untested changes to production",
            "Execute",
            0,
            0,
        );
        assert!(result.is_none());
    }

    #[test]
    fn production_warning_suppressed_in_explore_phase() {
        let manager = make_manager();
        let result = manager.is_suppressed(
            "prod-warn",
            "shell",
            "do not modify production configuration directly",
            "explore",
            0,
            0,
        );
        assert!(matches!(result, Some(SuppressionReason::ObviousFromContext(_))));
    }

    // -----------------------------------------------------------------------
    // ObviousFromContext — meta-constraints on non-planning tools
    // -----------------------------------------------------------------------

    #[test]
    fn meta_constraint_suppressed_on_shell_tool() {
        let manager = make_manager();
        let result = manager.is_suppressed(
            "meta-001",
            "shell",
            "plan first before implementing this feature",
            "Execute",
            0,
            0,
        );
        assert!(matches!(result, Some(SuppressionReason::ObviousFromContext(_))));
    }

    #[test]
    fn meta_constraint_not_suppressed_on_planning_tool() {
        let manager = make_manager();
        let result = manager.is_suppressed(
            "meta-001",
            "think",
            "plan first before implementing this feature",
            "Execute",
            0,
            0,
        );
        assert!(result.is_none());
    }

    #[test]
    fn before_implementing_suppressed_on_exec_tool() {
        let manager = make_manager();
        let result = manager.is_suppressed(
            "meta-002",
            "exec",
            "define requirements before implementing the solution",
            "Execute",
            0,
            0,
        );
        assert!(matches!(result, Some(SuppressionReason::ObviousFromContext(_))));
    }

    // -----------------------------------------------------------------------
    // ObviousFromContext — shell safety advice on non-shell tool
    // -----------------------------------------------------------------------

    #[test]
    fn shell_safety_advice_suppressed_on_file_tool() {
        let manager = make_manager();
        let result = manager.is_suppressed(
            "shell-safety",
            "file_write",
            "always use shell quoting to prevent injection",
            "Execute",
            0,
            0,
        );
        assert!(matches!(result, Some(SuppressionReason::ObviousFromContext(_))));
    }

    #[test]
    fn shell_safety_advice_not_suppressed_on_shell_tool() {
        let manager = make_manager();
        let result = manager.is_suppressed(
            "shell-safety",
            "shell",
            "always use shell quoting to prevent injection",
            "Execute",
            0,
            0,
        );
        assert!(result.is_none());
    }

    #[test]
    fn shell_safety_advice_not_suppressed_on_exec_tool() {
        let manager = make_manager();
        let result = manager.is_suppressed(
            "shell-safety",
            "exec",
            "escape all shell arguments to prevent injection",
            "Execute",
            0,
            0,
        );
        assert!(result.is_none());
    }

    // -----------------------------------------------------------------------
    // ObviousFromContext — read-before-write
    // -----------------------------------------------------------------------

    #[test]
    fn read_before_write_suppressed_when_file_recently_read() {
        let mut manager = make_manager();
        manager.record_file_read("/etc/config.toml");

        let result = manager.is_suppressed(
            "rwcheck-001",
            "file_write",
            "read /etc/config.toml before making changes to it",
            "Execute",
            0,
            0,
        );
        assert!(matches!(result, Some(SuppressionReason::ObviousFromContext(_))));
    }

    #[test]
    fn read_before_write_not_suppressed_when_different_file_was_read() {
        let mut manager = make_manager();
        manager.record_file_read("/etc/other.toml");

        let result = manager.is_suppressed(
            "rwcheck-001",
            "file_write",
            "read /etc/config.toml before making changes to it",
            "Execute",
            0,
            0,
        );
        // /etc/config.toml is not in recent reads; advisory is not obvious.
        assert!(result.is_none());
    }

    #[test]
    fn read_before_write_not_suppressed_on_non_write_tool() {
        let mut manager = make_manager();
        manager.record_file_read("/etc/config.toml");

        // A shell tool does not trigger the write-path check.
        let result = manager.is_suppressed(
            "rwcheck-001",
            "shell",
            "read /etc/config.toml before making changes to it",
            "Execute",
            0,
            0,
        );
        // Shell tool doesn't match the write/file guard, so rule doesn't apply.
        assert!(result.is_none());
    }

    // -----------------------------------------------------------------------
    // cleanup_expired
    // -----------------------------------------------------------------------

    #[test]
    fn cleanup_expired_removes_all_entries_when_max_age_is_zero() {
        let mut manager = make_manager();
        manager.record_emission("old-advice", "shell", "some advisory text");
        manager.record_file_read("/tmp/scratch.txt");

        // max_age_secs = 0 means elapsed (≥ 0) is never < 0, so everything is pruned.
        manager.cleanup_expired(0);

        assert!(manager.tool_cooldowns.is_empty(), "tool cooldowns should be cleared");
        assert!(manager.advice_cooldowns.is_empty(), "advice cooldowns should be cleared");
        assert!(manager.advice_text_hashes.is_empty(), "text hashes should be cleared");
        assert!(manager.recent_file_reads.is_empty(), "file reads should be cleared");
    }

    #[test]
    fn cleanup_expired_retains_fresh_entries() {
        let mut manager = make_manager();
        manager.record_emission("fresh-advice", "shell", "some advisory text");
        manager.record_file_read("/tmp/scratch.txt");

        // A generous window retains everything that was just written.
        manager.cleanup_expired(3600);

        assert!(!manager.tool_cooldowns.is_empty());
        assert!(!manager.advice_cooldowns.is_empty());
        assert!(!manager.advice_text_hashes.is_empty());
        assert!(!manager.recent_file_reads.is_empty());
    }

    #[test]
    fn cleanup_expired_does_not_clear_cycle_set() {
        let mut manager = make_manager();
        manager.record_emission("tip-99", "shell", "use set -e");

        manager.cleanup_expired(0);

        // cleanup_expired does not touch emitted_this_cycle; reset_cycle does.
        assert!(manager.emitted_this_cycle.contains("tip-99"));
    }

    // -----------------------------------------------------------------------
    // hash_text
    // -----------------------------------------------------------------------

    #[test]
    fn hash_text_is_deterministic() {
        assert_eq!(
            CooldownManager::hash_text("hello world"),
            CooldownManager::hash_text("hello world")
        );
    }

    #[test]
    fn hash_text_differs_for_different_inputs() {
        assert_ne!(
            CooldownManager::hash_text("hello world"),
            CooldownManager::hash_text("goodbye world")
        );
    }

    #[test]
    fn hash_text_is_sensitive_to_whitespace() {
        assert_ne!(
            CooldownManager::hash_text("hello world"),
            CooldownManager::hash_text("helloworld")
        );
    }
}
