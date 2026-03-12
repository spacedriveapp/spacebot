//! Tool loop detection for the agent execution loop.

use crate::ProcessType;

use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};

const POLL_TOOLS: &[&str] = &["shell"];
const HISTORY_CAPACITY: usize = 30;

/// Tools that read changing state and are expected to be called repeatedly with
/// identical arguments across a session. `browser_snapshot` always takes `{}`
/// but returns different content after each page mutation. These tools get the
/// poll multiplier on their consecutive-call threshold and are excluded from
/// ping-pong detection patterns (since alternating snapshot→click is the normal
/// browser workflow, not a stuck loop).
const OBSERVATION_TOOLS: &[&str] = &["browser_snapshot", "browser_tab_list"];

/// Avoids hashing multi-MB tool outputs while still catching identical short
/// results. Large outputs that differ only in the tail (growing log files,
/// etc.) won't hash-match, which is correct — the tool is returning new data.
const RESULT_HASH_TRUNCATION: usize = 1_000;

#[derive(Debug, Clone)]
pub struct LoopGuardConfig {
    pub warn_threshold: u32,
    pub block_threshold: u32,
    pub global_circuit_breaker: u32,
    pub poll_multiplier: u32,
    pub outcome_warn_threshold: u32,
    pub outcome_block_threshold: u32,
    pub ping_pong_min_repeats: u32,
    pub max_warnings_per_call: u32,
}

impl LoopGuardConfig {
    pub fn for_process(process_type: ProcessType) -> Self {
        match process_type {
            // Workers do repetitive tool work (build, test, fix cycles).
            // Generous thresholds to avoid interfering with legitimate iteration.
            ProcessType::Worker => Self {
                warn_threshold: 4,
                block_threshold: 7,
                global_circuit_breaker: 80,
                poll_multiplier: 3,
                outcome_warn_threshold: 3,
                outcome_block_threshold: 4,
                ping_pong_min_repeats: 4,
                max_warnings_per_call: 3,
            },
            // Branches think and recall, moderate iteration expected.
            ProcessType::Branch => Self {
                warn_threshold: 3,
                block_threshold: 5,
                global_circuit_breaker: 40,
                poll_multiplier: 2,
                outcome_warn_threshold: 2,
                outcome_block_threshold: 3,
                ping_pong_min_repeats: 3,
                max_warnings_per_call: 2,
            },
            // Cortex chat is an interactive session with memory, docs, and
            // worker tools — needs headroom similar to branches.
            ProcessType::Cortex => Self {
                warn_threshold: 3,
                block_threshold: 5,
                global_circuit_breaker: 40,
                poll_multiplier: 2,
                outcome_warn_threshold: 2,
                outcome_block_threshold: 3,
                ping_pong_min_repeats: 3,
                max_warnings_per_call: 2,
            },
            // Channels and compactors: minimal tool loops expected.
            ProcessType::Channel | ProcessType::Compactor => Self {
                warn_threshold: 2,
                block_threshold: 4,
                global_circuit_breaker: 20,
                poll_multiplier: 2,
                outcome_warn_threshold: 2,
                outcome_block_threshold: 3,
                ping_pong_min_repeats: 3,
                max_warnings_per_call: 2,
            },
        }
    }
}

/// Maps to Rig hook actions: `Allow` -> `Continue`, `Block` -> `Skip` (message
/// becomes the tool result the LLM sees), `CircuitBreak` -> `Terminate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopGuardVerdict {
    Allow,
    Block(String),
    CircuitBreak(String),
}

/// Held behind `Arc<Mutex<>>` on `SpacebotHook`. Persists across tool calls
/// within one Rig `agent.prompt()` invocation, reset at prompt boundaries.
pub struct LoopGuard {
    config: LoopGuardConfig,
    /// Consecutive call count per `(tool, args)` hash. Reset to 0 for all
    /// hashes except the current one whenever a *different* call is made.
    /// This means `browser_snapshot` (empty args) can be called 50 times
    /// across a session as long as other tools run in between — but 7 calls
    /// in a row with nothing else is a real loop.
    call_counts: HashMap<String, u32>,
    /// The call hash of the most recent `check()`. Used to detect whether the
    /// current call is a continuation of the same repeated sequence.
    last_call_hash: Option<String>,
    total_calls: u32,
    outcome_counts: HashMap<String, u32>,
    // Call hashes poisoned by outcome detection — next check() auto-blocks.
    blocked_outcomes: HashSet<String>,
    recent_calls: VecDeque<String>,
    warnings_emitted: HashMap<String, u32>,
    hash_to_tool: HashMap<String, String>,
}

impl LoopGuard {
    pub fn new(config: LoopGuardConfig) -> Self {
        Self {
            config,
            call_counts: HashMap::new(),
            last_call_hash: None,
            total_calls: 0,
            outcome_counts: HashMap::new(),
            blocked_outcomes: HashSet::new(),
            recent_calls: VecDeque::with_capacity(HISTORY_CAPACITY),
            warnings_emitted: HashMap::new(),
            hash_to_tool: HashMap::new(),
        }
    }

    pub fn reset(&mut self) {
        self.call_counts.clear();
        self.last_call_hash = None;
        self.total_calls = 0;
        self.outcome_counts.clear();
        self.blocked_outcomes.clear();
        self.recent_calls.clear();
        self.warnings_emitted.clear();
        self.hash_to_tool.clear();
    }

    pub fn check(&mut self, tool_name: &str, args: &str) -> LoopGuardVerdict {
        self.total_calls += 1;

        if self.total_calls > self.config.global_circuit_breaker {
            return LoopGuardVerdict::CircuitBreak(format!(
                "Circuit breaker: exceeded {} total tool calls in this loop. \
                 The agent appears to be stuck.",
                self.config.global_circuit_breaker
            ));
        }

        let call_hash = Self::compute_call_hash(tool_name, args);
        self.hash_to_tool
            .entry(call_hash.clone())
            .or_insert_with(|| tool_name.to_string());

        if self.recent_calls.len() >= HISTORY_CAPACITY {
            self.recent_calls.pop_front();
        }
        self.recent_calls.push_back(call_hash.clone());

        if self.blocked_outcomes.contains(&call_hash) {
            return LoopGuardVerdict::Block(format!(
                "Blocked: tool '{}' is returning identical results repeatedly. \
                 The current approach is not working — try something different.",
                tool_name
            ));
        }

        // Reset consecutive counts when the call hash changes. A different
        // tool call in between means the model is doing real work, not looping.
        // This prevents observation tools like `browser_snapshot` (which always
        // have the same empty args) from being permanently blocked after N
        // total uses across a long session.
        let is_same_as_last = self.last_call_hash.as_ref() == Some(&call_hash);
        if !is_same_as_last {
            self.call_counts.clear();
            // Reset per-hash warnings so the model gets fresh warnings if it
            // starts a new loop with this tool later. Retain ping-pong warning
            // counters so the max_warnings_per_call escalation still works for
            // alternating patterns detected across tool switches.
            self.warnings_emitted
                .retain(|key, _| key.starts_with("pingpong_"));
        }
        self.last_call_hash = Some(call_hash.clone());

        let count = self.call_counts.entry(call_hash.clone()).or_insert(0);
        *count += 1;
        let count_value = *count;

        let is_poll = Self::is_poll_call(tool_name, args);
        let multiplier = if is_poll {
            self.config.poll_multiplier
        } else {
            1
        };
        let effective_warn = self.config.warn_threshold * multiplier;
        let effective_block = self.config.block_threshold * multiplier;

        if count_value >= effective_block {
            return LoopGuardVerdict::Block(format!(
                "Blocked: tool '{}' called {} times with identical parameters. \
                 Try a different approach or different parameters.",
                tool_name, count_value
            ));
        }

        // Warn threshold — escalates to block after max_warnings_per_call.
        if count_value >= effective_warn {
            let warning_count = self.warnings_emitted.entry(call_hash.clone()).or_insert(0);
            *warning_count += 1;
            if *warning_count > self.config.max_warnings_per_call {
                return LoopGuardVerdict::Block(format!(
                    "Blocked: tool '{}' called {} times with identical parameters \
                     (warnings exhausted). Try a different approach.",
                    tool_name, count_value
                ));
            }
            return LoopGuardVerdict::Block(format!(
                "Warning: tool '{}' has been called {} times with identical parameters. \
                 Consider trying a different approach or different parameters.",
                tool_name, count_value
            ));
        }

        // Ping-pong detection runs even if individual hash counts are low.
        if let Some(ping_pong_message) = self.detect_ping_pong() {
            let repeats = self.count_ping_pong_repeats();
            if repeats >= self.config.ping_pong_min_repeats {
                return LoopGuardVerdict::Block(ping_pong_message);
            }
            // Below min_repeats, send a warning via skip.
            let warning_count = self
                .warnings_emitted
                .entry(format!("pingpong_{call_hash}"))
                .or_insert(0);
            *warning_count += 1;
            if *warning_count <= self.config.max_warnings_per_call {
                return LoopGuardVerdict::Block(ping_pong_message);
            }
        }

        LoopGuardVerdict::Allow
    }

    /// If the same `(tool_name, args, result)` triple is seen enough times,
    /// the call hash is poisoned so the next `check()` blocks before execution.
    pub fn record_outcome(&mut self, tool_name: &str, args: &str, result: &str) {
        let outcome_hash = Self::compute_outcome_hash(tool_name, args, result);
        let call_hash = Self::compute_call_hash(tool_name, args);

        let count = self.outcome_counts.entry(outcome_hash).or_insert(0);
        *count += 1;
        let count_value = *count;

        if count_value >= self.config.outcome_block_threshold {
            // Poison the call hash so the next check() auto-blocks.
            self.blocked_outcomes.insert(call_hash);
            tracing::debug!(
                tool_name,
                outcome_count = count_value,
                "loop guard: outcome block threshold reached, poisoning call hash"
            );
        } else if count_value >= self.config.outcome_warn_threshold {
            tracing::debug!(
                tool_name,
                outcome_count = count_value,
                "loop guard: identical outcome detected"
            );
        }
    }

    fn detect_ping_pong(&self) -> Option<String> {
        let history: Vec<&String> = self.recent_calls.iter().collect();
        let length = history.len();

        // Check for pattern of length 2 (A-B-A-B-A-B) — need 6 entries.
        if length >= 6 {
            let tail = &history[length - 6..];
            let a = tail[0];
            let b = tail[1];
            if a != b && tail[2] == a && tail[3] == b && tail[4] == a && tail[5] == b {
                let tool_a = self.resolve_tool_name(a);
                let tool_b = self.resolve_tool_name(b);

                // Observation tools alternate with action tools as part of
                // normal workflow (snapshot→click→snapshot→click). Don't flag
                // these patterns as ping-pong.
                if Self::involves_observation_tool(&tool_a, &tool_b) {
                    return None;
                }

                return Some(format!(
                    "Ping-pong detected: tools '{}' and '{}' are alternating \
                     repeatedly. Break the cycle by trying a different approach.",
                    tool_a, tool_b
                ));
            }
        }

        // Check for pattern of length 3 (A-B-C-A-B-C-A-B-C) — need 9 entries.
        if length >= 9 {
            let tail = &history[length - 9..];
            let a = tail[0];
            let b = tail[1];
            let c = tail[2];
            // Ensure they're not all the same (that's just repetition).
            if !(a == b && b == c)
                && tail[3] == a
                && tail[4] == b
                && tail[5] == c
                && tail[6] == a
                && tail[7] == b
                && tail[8] == c
            {
                let tool_a = self.resolve_tool_name(a);
                let tool_b = self.resolve_tool_name(b);
                let tool_c = self.resolve_tool_name(c);

                if Self::involves_observation_tool(&tool_a, &tool_b)
                    || Self::involves_observation_tool(&tool_a, &tool_c)
                    || Self::involves_observation_tool(&tool_b, &tool_c)
                {
                    return None;
                }

                return Some(format!(
                    "Ping-pong detected: tools '{}', '{}', '{}' are cycling \
                     repeatedly. Break the cycle by trying a different approach.",
                    tool_a, tool_b, tool_c
                ));
            }
        }

        None
    }

    /// Returns true if exactly one tool is an observation tool. Observation
    /// tools (like `browser_snapshot`) naturally alternate with action tools
    /// as part of normal workflow (snapshot→click) and should not be flagged
    /// as ping-pong. Two observation tools alternating (snapshot↔tab_list)
    /// is still suspicious and should be caught.
    fn involves_observation_tool(tool_a: &str, tool_b: &str) -> bool {
        OBSERVATION_TOOLS.contains(&tool_a) ^ OBSERVATION_TOOLS.contains(&tool_b)
    }

    fn count_ping_pong_repeats(&self) -> u32 {
        let history: Vec<&String> = self.recent_calls.iter().collect();
        let length = history.len();

        if length >= 4 {
            let a = history[length - 2];
            let b = history[length - 1];
            if a != b {
                let mut repeats: u32 = 0;
                let mut index = length;
                while index >= 2 {
                    index -= 2;
                    if history[index] == a && history[index + 1] == b {
                        repeats += 1;
                    } else {
                        break;
                    }
                }
                if repeats >= 2 {
                    return repeats;
                }
            }
        }

        if length >= 6 {
            let a = history[length - 3];
            let b = history[length - 2];
            let c = history[length - 1];
            if !(a == b && b == c) {
                let mut repeats: u32 = 0;
                let mut index = length;
                while index >= 3 {
                    index -= 3;
                    if history[index] == a && history[index + 1] == b && history[index + 2] == c {
                        repeats += 1;
                    } else {
                        break;
                    }
                }
                if repeats >= 2 {
                    return repeats;
                }
            }
        }

        0
    }

    fn resolve_tool_name(&self, hash: &str) -> String {
        self.hash_to_tool
            .get(hash)
            .cloned()
            .unwrap_or_else(|| "unknown".to_string())
    }

    // Poll/observation tools get relaxed thresholds because they're expected
    // to be called repeatedly (checking build output, watching deployment
    // status, re-snapshotting browser state after page mutations).
    fn is_poll_call(tool_name: &str, args: &str) -> bool {
        // Observation tools are always considered poll calls — they read
        // changing state with identical args every time.
        if OBSERVATION_TOOLS.contains(&tool_name) {
            return true;
        }

        if POLL_TOOLS.contains(&tool_name) {
            let args_lower = args.to_lowercase();
            if args.len() < 200
                && (args_lower.contains("status")
                    || args_lower.contains("poll")
                    || args_lower.contains("wait")
                    || args_lower.contains("watch")
                    || args_lower.contains("tail")
                    || args_lower.contains("\"ps ")
                    || args_lower.contains("jobs")
                    || args_lower.contains("pgrep")
                    || args_lower.contains("docker ps")
                    || args_lower.contains("kubectl get"))
            {
                return true;
            }
        }
        false
    }

    fn compute_call_hash(tool_name: &str, args: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(tool_name.as_bytes());
        hasher.update(b"|");
        hasher.update(args.as_bytes());
        hex::encode(hasher.finalize())
    }

    fn compute_outcome_hash(tool_name: &str, args: &str, result: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(tool_name.as_bytes());
        hasher.update(b"|");
        hasher.update(args.as_bytes());
        hasher.update(b"|");
        let result_bytes = result.as_bytes();
        hasher.update(&result_bytes[..result_bytes.len().min(RESULT_HASH_TRUNCATION)]);
        hex::encode(hasher.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker_config() -> LoopGuardConfig {
        LoopGuardConfig::for_process(ProcessType::Worker)
    }

    fn channel_config() -> LoopGuardConfig {
        LoopGuardConfig::for_process(ProcessType::Channel)
    }

    #[test]
    fn allow_below_threshold() {
        let mut guard = LoopGuard::new(worker_config());
        let verdict = guard.check("shell", r#"{"command":"ls"}"#);
        assert_eq!(verdict, LoopGuardVerdict::Allow);
        let verdict = guard.check("shell", r#"{"command":"ls"}"#);
        assert_eq!(verdict, LoopGuardVerdict::Allow);
    }

    #[test]
    fn block_at_warn_threshold() {
        let mut guard = LoopGuard::new(worker_config());
        let args = r#"{"command":"cargo build"}"#;
        // Worker warn_threshold = 4, so calls 1-3 are Allow.
        for _ in 0..3 {
            assert_eq!(guard.check("shell", args), LoopGuardVerdict::Allow);
        }
        // Call 4 hits warn threshold — delivered as a Block/Skip with warning.
        let verdict = guard.check("shell", args);
        assert!(
            matches!(verdict, LoopGuardVerdict::Block(ref message) if message.contains("Warning"))
        );
    }

    #[test]
    fn block_at_block_threshold() {
        let mut guard = LoopGuard::new(worker_config());
        let args = r#"{"command":"cargo build"}"#;
        // Worker block_threshold = 7.
        for _ in 0..6 {
            guard.check("shell", args);
        }
        let verdict = guard.check("shell", args);
        assert!(
            matches!(verdict, LoopGuardVerdict::Block(ref message) if message.contains("Blocked"))
        );
    }

    #[test]
    fn different_params_no_collision() {
        let mut guard = LoopGuard::new(worker_config());
        for i in 0..20 {
            let args = format!(r#"{{"query":"query_{i}"}}"#);
            assert_eq!(guard.check("web_search", &args), LoopGuardVerdict::Allow);
        }
    }

    #[test]
    fn global_circuit_breaker() {
        let config = LoopGuardConfig {
            global_circuit_breaker: 5,
            warn_threshold: 100,
            block_threshold: 100,
            ..worker_config()
        };
        let mut guard = LoopGuard::new(config);
        for i in 0..5 {
            let args = format!(r#"{{"n":{i}}}"#);
            assert_eq!(guard.check("tool", &args), LoopGuardVerdict::Allow);
        }
        // Call 6 triggers circuit breaker (> 5).
        let verdict = guard.check("tool", r#"{"n":5}"#);
        assert!(matches!(verdict, LoopGuardVerdict::CircuitBreak(_)));
    }

    #[test]
    fn channel_has_stricter_thresholds_than_worker() {
        let channel = channel_config();
        let worker = worker_config();
        assert!(channel.warn_threshold < worker.warn_threshold);
        assert!(channel.block_threshold < worker.block_threshold);
        assert!(channel.global_circuit_breaker < worker.global_circuit_breaker);
    }

    #[test]
    fn outcome_repetition_poisons_call_hash() {
        let mut guard = LoopGuard::new(worker_config());
        let args = r#"{"query":"weather"}"#;
        let result = "sunny 72F";

        // Worker outcome_block_threshold = 4. Record 4 identical outcomes.
        for _ in 0..4 {
            guard.record_outcome("web_search", args, result);
        }

        // The next check() for this call hash should auto-block.
        let verdict = guard.check("web_search", args);
        assert!(
            matches!(verdict, LoopGuardVerdict::Block(ref message) if message.contains("identical results"))
        );
    }

    #[test]
    fn different_results_do_not_poison() {
        let mut guard = LoopGuard::new(worker_config());
        let args = r#"{"command":"cargo build"}"#;

        // Each call produces a different result — the agent is making progress.
        for i in 0..10 {
            guard.record_outcome("shell", args, &format!("error on line {i}"));
        }

        // Call hash should not be poisoned.
        let verdict = guard.check("shell", args);
        assert_eq!(verdict, LoopGuardVerdict::Allow);
    }

    #[test]
    fn ping_pong_ab_detection() {
        let config = LoopGuardConfig {
            warn_threshold: 100,
            block_threshold: 100,
            ping_pong_min_repeats: 3,
            ..worker_config()
        };
        let mut guard = LoopGuard::new(config);
        let args_a = r#"{"file":"a.txt"}"#;
        let args_b = r#"{"file":"b.txt"}"#;

        // A-B-A-B-A-B = 3 repeats of (A,B).
        for _ in 0..3 {
            guard.check("file", args_a);
            guard.check("file", args_b);
        }

        // On the 7th call, pattern should be detected.
        let verdict = guard.check("file", args_a);
        assert!(
            matches!(verdict, LoopGuardVerdict::Block(ref message) if message.contains("Ping-pong"))
                || matches!(verdict, LoopGuardVerdict::Allow),
            "Expected ping-pong detection or allow (pattern detected on prior call), got: {verdict:?}"
        );
    }

    #[test]
    fn ping_pong_abc_detection() {
        let config = LoopGuardConfig {
            warn_threshold: 100,
            block_threshold: 100,
            ping_pong_min_repeats: 3,
            ..worker_config()
        };
        let mut guard = LoopGuard::new(config);

        // A-B-C-A-B-C-A-B-C = 3 repeats of (A,B,C).
        for _ in 0..3 {
            guard.check("tool_a", r#"{"a":1}"#);
            guard.check("tool_b", r#"{"b":2}"#);
            guard.check("tool_c", r#"{"c":3}"#);
        }

        // The 10th call should detect the pattern.
        let verdict = guard.check("tool_a", r#"{"a":1}"#);
        assert!(
            matches!(verdict, LoopGuardVerdict::Block(ref message) if message.contains("Ping-pong")),
            "Expected ping-pong detection, got: {verdict:?}"
        );
    }

    #[test]
    fn no_false_ping_pong() {
        let mut guard = LoopGuard::new(worker_config());
        for i in 0..10 {
            let args = format!(r#"{{"n":{i}}}"#);
            let verdict = guard.check("tool", &args);
            assert_eq!(verdict, LoopGuardVerdict::Allow);
        }
    }

    #[test]
    fn poll_tool_relaxed_thresholds() {
        let mut guard = LoopGuard::new(worker_config());
        // Worker: warn=4, poll_multiplier=3, so effective warn=12.
        let args = r#"{"command":"docker ps --status running"}"#;

        // Calls 1-11 should all be Allow (below effective warn=12).
        for i in 1..=11 {
            let verdict = guard.check("shell", args);
            assert_eq!(
                verdict,
                LoopGuardVerdict::Allow,
                "Call {i} should be allowed for poll tool"
            );
        }

        // Call 12 should trigger a warning.
        let verdict = guard.check("shell", args);
        assert!(
            matches!(verdict, LoopGuardVerdict::Block(ref message) if message.contains("Warning")),
            "Expected warning at poll threshold, got: {verdict:?}"
        );
    }

    #[test]
    fn non_poll_shell_not_relaxed() {
        let mut guard = LoopGuard::new(worker_config());
        let args = r#"{"command":"echo hello"}"#;

        // Worker: warn=4, no poll multiplier for non-poll commands.
        for _ in 0..3 {
            assert_eq!(guard.check("shell", args), LoopGuardVerdict::Allow);
        }
        let verdict = guard.check("shell", args);
        assert!(matches!(verdict, LoopGuardVerdict::Block(_)));
    }

    #[test]
    fn reset_clears_all_state() {
        let mut guard = LoopGuard::new(worker_config());
        let args = r#"{"command":"ls"}"#;

        for _ in 0..3 {
            guard.check("shell", args);
        }
        guard.record_outcome("shell", args, "file1 file2");

        guard.reset();

        // After reset, the first call should be allowed again.
        assert_eq!(guard.check("shell", args), LoopGuardVerdict::Allow);
        assert_eq!(guard.total_calls, 1);
    }

    #[test]
    fn non_consecutive_identical_calls_allowed() {
        // This is the browser_snapshot bug fix: calling the same parameterless
        // tool many times across a session should be fine as long as other
        // tools run in between (the page state changes between snapshots).
        let mut guard = LoopGuard::new(worker_config());
        let snapshot_args = "{}";
        let click_args = r#"{"index": 3}"#;

        // Simulate 20 rounds of snapshot → click → snapshot → click...
        for _ in 0..20 {
            let verdict = guard.check("browser_snapshot", snapshot_args);
            assert_eq!(
                verdict,
                LoopGuardVerdict::Allow,
                "browser_snapshot should be allowed when interleaved with other tools"
            );
            let verdict = guard.check("browser_click", click_args);
            assert_eq!(verdict, LoopGuardVerdict::Allow);
        }
    }

    #[test]
    fn consecutive_identical_calls_still_blocked() {
        // Calling the same tool with the same args many times IN A ROW should
        // still trigger the block — that's a real loop.
        let mut guard = LoopGuard::new(worker_config());
        let args = r#"{"query":"same thing"}"#;

        // Worker warn_threshold = 4. Calls 1-3 are Allow, call 4 hits warn.
        for _ in 0..3 {
            assert_eq!(guard.check("web_search", args), LoopGuardVerdict::Allow);
        }
        // Call 4 hits warn threshold.
        let verdict = guard.check("web_search", args);
        assert!(matches!(verdict, LoopGuardVerdict::Block(_)));
    }

    #[test]
    fn observation_tool_consecutive_gets_relaxed_threshold() {
        // Observation tools like browser_snapshot get the poll multiplier even
        // when called consecutively, so they can handle legitimate sequences
        // of multiple snapshots.
        let mut guard = LoopGuard::new(worker_config());
        let args = "{}";

        // Worker warn_threshold=4, poll_multiplier=3, so effective warn=12.
        // Calls 1-11 should all be allowed.
        for i in 1..=11 {
            let verdict = guard.check("browser_snapshot", args);
            assert_eq!(
                verdict,
                LoopGuardVerdict::Allow,
                "Call {i} should be allowed for observation tool"
            );
        }
        // Call 12 should trigger a warning.
        let verdict = guard.check("browser_snapshot", args);
        assert!(
            matches!(verdict, LoopGuardVerdict::Block(ref message) if message.contains("Warning")),
            "Expected warning at relaxed threshold, got: {verdict:?}"
        );
    }

    #[test]
    fn warning_bucket_escalates_to_block() {
        let config = LoopGuardConfig {
            warn_threshold: 2,
            block_threshold: 100,
            max_warnings_per_call: 2,
            ..worker_config()
        };
        let mut guard = LoopGuard::new(config);
        let args = r#"{"x":1}"#;

        // Call 1: Allow.
        assert_eq!(guard.check("tool", args), LoopGuardVerdict::Allow);

        // Call 2: Block with warning (hits warn_threshold=2).
        let verdict = guard.check("tool", args);
        assert!(matches!(verdict, LoopGuardVerdict::Block(ref m) if m.contains("Warning")));

        // Call 3: Block with warning again.
        let verdict = guard.check("tool", args);
        assert!(matches!(verdict, LoopGuardVerdict::Block(ref m) if m.contains("Warning")));

        // Call 4: Block with "warnings exhausted" (max_warnings_per_call=2 exceeded).
        let verdict = guard.check("tool", args);
        assert!(
            matches!(verdict, LoopGuardVerdict::Block(ref m) if m.contains("warnings exhausted"))
        );
    }
}
