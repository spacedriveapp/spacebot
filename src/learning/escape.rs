//! Escape protocol: structured escalation when the agent is stuck, looping, or out of budget.
//!
//! `EscapeProtocol` tracks attempt history and health metrics, decides when escalation is
//! warranted, and produces a structured `EscalationRequest` along with a learning artifact
//! that captures what was tried and why it did or did not work.

use crate::learning::types::{AttemptSummary, EscalationRequest, EscalationType, RequestType};
use crate::llm::{LlmManager, RoutingConfig};
use crate::ProcessType;

use rig::agent::AgentBuilder;
use rig::completion::{CompletionModel, Prompt as _};
use serde::{Deserialize, Serialize};

use std::sync::Arc;

// ---------------------------------------------------------------------------
// Trigger thresholds
// ---------------------------------------------------------------------------

/// Watcher firings at or above this count indicate a loop condition.
const LOOP_FIRING_THRESHOLD: u32 = 3;

/// Confidence below this value, combined with insufficient evidence, triggers escalation.
const LOW_CONFIDENCE_THRESHOLD: f64 = 0.3;

/// Minimum evidence count required to suppress a low-confidence trigger.
const MIN_EVIDENCE_FOR_CONFIDENCE: u32 = 2;

/// Time budget percentage above which escalation is triggered.
const BUDGET_TRIGGER_THRESHOLD: f64 = 0.80;

// ---------------------------------------------------------------------------
// EscapeProtocol
// ---------------------------------------------------------------------------

/// Tracks agent health signals and produces structured escalation when warranted.
///
/// Callers update metrics each turn via `update_metrics`, record attempt
/// summaries via `record_attempt`, and record raw evidence strings via
/// `record_evidence`. When `should_trigger` returns true, call `execute`
/// (or `execute_with_llm` for richer hypotheses) to produce an `EscapeResult`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscapeProtocol {
    watcher_firing_count: u32,
    confidence: f64,
    evidence_count: u32,
    time_budget_used_pct: f64,
    attempts: Vec<AttemptSummary>,
    evidence_gathered: Vec<String>,
}

/// Output of the escape protocol: a structured escalation request plus a
/// learning artifact that records what was tried and what was learned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscapeResult {
    pub escalation: EscalationRequest,
    pub learning_artifact: String,
}

impl EscapeProtocol {
    /// Create a fresh protocol with zeroed metrics and empty history.
    pub fn new() -> Self {
        Self {
            watcher_firing_count: 0,
            confidence: 1.0,
            evidence_count: 0,
            time_budget_used_pct: 0.0,
            attempts: Vec::new(),
            evidence_gathered: Vec::new(),
        }
    }

    /// Whether current metrics satisfy at least one trigger condition.
    ///
    /// Triggers when:
    /// - Watcher has fired 3+ times (loop detection)
    /// - Confidence is below 0.3 with fewer than 2 evidence items (uncertain with no footing)
    /// - Time budget is more than 80% consumed
    pub fn should_trigger(&self) -> bool {
        self.watcher_firing_count >= LOOP_FIRING_THRESHOLD
            || (self.confidence < LOW_CONFIDENCE_THRESHOLD
                && self.evidence_count < MIN_EVIDENCE_FOR_CONFIDENCE)
            || self.time_budget_used_pct > BUDGET_TRIGGER_THRESHOLD
    }

    /// Record a single attempt with a human-readable description, outcome, and tool names.
    pub fn record_attempt(
        &mut self,
        description: impl Into<String>,
        outcome: impl Into<String>,
        tools_used: Vec<String>,
    ) {
        self.attempts.push(AttemptSummary {
            description: description.into(),
            outcome: outcome.into(),
            tools_used,
        });
    }

    /// Append a raw evidence string — a tool output snippet, test result, or observation.
    pub fn record_evidence(&mut self, evidence: String) {
        self.evidence_gathered.push(evidence);
        // Keep evidence_count in sync when evidence is added through this path.
        // Callers that use update_metrics directly will override evidence_count there.
        self.evidence_count = self.evidence_gathered.len() as u32;
    }

    /// Replace all tracked health metrics in one call.
    ///
    /// This is typically called once per turn from the control plane before
    /// checking `should_trigger`.
    pub fn update_metrics(
        &mut self,
        watcher_firings: u32,
        confidence: f64,
        evidence_count: u32,
        time_budget_pct: f64,
    ) {
        self.watcher_firing_count = watcher_firings;
        self.confidence = confidence;
        self.evidence_count = evidence_count;
        self.time_budget_used_pct = time_budget_pct;
    }

    /// Produce a template-based `EscapeResult` without calling an LLM.
    ///
    /// The escalation type, request type, and suggested hypotheses are
    /// derived directly from the tracked metrics and attempt history.
    /// Use this as the fast/offline path; `execute_with_llm` produces
    /// higher-quality hypotheses when a model is available.
    pub fn execute(&self) -> EscapeResult {
        let escalation_type = self.classify_escalation_type();
        let request_type = self.classify_request_type();
        let suggested_options = self.template_hypotheses(&escalation_type);
        let current_hypothesis = self.derive_current_hypothesis();
        let learning_artifact = self.generate_learning_artifact();

        let escalation = EscalationRequest {
            escalation_type,
            request_type,
            attempts: self.attempts.clone(),
            evidence_gathered: self.evidence_gathered.clone(),
            current_hypothesis,
            suggested_options,
        };

        EscapeResult {
            escalation,
            learning_artifact,
        }
    }

    /// Produce an `EscapeResult` with LLM-generated hypotheses.
    ///
    /// Builds a one-shot Rig agent using the branch model from `routing`,
    /// prompts it with a structured summary of current state, and parses
    /// the response as three ranked hypotheses. Falls back to `execute()`
    /// on any error so callers always receive a valid result.
    pub async fn execute_with_llm(
        &self,
        llm_manager: &Arc<LlmManager>,
        routing: &RoutingConfig,
        agent_id: &str,
    ) -> EscapeResult {
        match self.try_llm_hypotheses(llm_manager, routing, agent_id).await {
            Ok(hypotheses) => {
                let escalation_type = self.classify_escalation_type();
                let request_type = self.classify_request_type();
                let current_hypothesis = self.derive_current_hypothesis();
                let learning_artifact = self.generate_learning_artifact();

                let escalation = EscalationRequest {
                    escalation_type,
                    request_type,
                    attempts: self.attempts.clone(),
                    evidence_gathered: self.evidence_gathered.clone(),
                    current_hypothesis,
                    suggested_options: hypotheses,
                };

                EscapeResult {
                    escalation,
                    learning_artifact,
                }
            }
            Err(error) => {
                tracing::warn!(%error, "LLM hypothesis generation failed, falling back to template");
                self.execute()
            }
        }
    }

    /// Generate a learning artifact from attempts and metrics.
    ///
    /// The artifact is a compact text block suitable for injecting into
    /// the memory system or passing as context to the next session.
    pub fn generate_learning_artifact(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        parts.push(format!(
            "## Escape Protocol — Learning Artifact\n\
             Trigger: {}\n\
             Confidence: {:.0}% | Evidence: {} items | Budget used: {:.0}% | Watcher firings: {}",
            self.classify_escalation_type_label(),
            self.confidence * 100.0,
            self.evidence_count,
            self.time_budget_used_pct * 100.0,
            self.watcher_firing_count,
        ));

        if !self.attempts.is_empty() {
            parts.push("\n### Attempts".to_string());
            for (index, attempt) in self.attempts.iter().enumerate() {
                let tools = if attempt.tools_used.is_empty() {
                    "none".to_string()
                } else {
                    attempt.tools_used.join(", ")
                };
                parts.push(format!(
                    "{}. {} → {} (tools: {})",
                    index + 1,
                    attempt.description,
                    attempt.outcome,
                    tools,
                ));
            }
        }

        if !self.evidence_gathered.is_empty() {
            parts.push("\n### Evidence gathered".to_string());
            for evidence in &self.evidence_gathered {
                parts.push(format!("- {evidence}"));
            }
        }

        parts.join("\n")
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    fn classify_escalation_type(&self) -> EscalationType {
        // Budget check takes priority — the most objective signal.
        if self.time_budget_used_pct > BUDGET_TRIGGER_THRESHOLD {
            EscalationType::Budget
        } else if self.watcher_firing_count >= LOOP_FIRING_THRESHOLD {
            EscalationType::Loop
        } else if self.confidence < LOW_CONFIDENCE_THRESHOLD {
            EscalationType::Confidence
        } else {
            EscalationType::Unknown
        }
    }

    fn classify_escalation_type_label(&self) -> &'static str {
        match self.classify_escalation_type() {
            EscalationType::Budget => "budget exhausted",
            EscalationType::Loop => "loop detected",
            EscalationType::Confidence => "low confidence",
            EscalationType::Blocked => "blocked",
            EscalationType::Unknown => "unknown",
        }
    }

    fn classify_request_type(&self) -> RequestType {
        if self.attempts.is_empty() {
            // Haven't tried anything yet — just need information.
            RequestType::Info
        } else if self.evidence_gathered.is_empty() {
            // Tried things but gathered nothing concrete — need help.
            RequestType::Help
        } else {
            // Have attempts and evidence — ready for a decision.
            RequestType::Decision
        }
    }

    fn derive_current_hypothesis(&self) -> Option<String> {
        // Use the most recent attempt's description as the working hypothesis.
        self.attempts
            .last()
            .map(|attempt| attempt.description.clone())
    }

    /// Three template hypotheses ranked by testability (most testable first).
    fn template_hypotheses(&self, escalation_type: &EscalationType) -> Vec<String> {
        match escalation_type {
            EscalationType::Budget => vec![
                "Identify the minimum subset of remaining work that satisfies the original goal \
                 and complete only that."
                    .to_string(),
                "Checkpoint current progress and hand off remaining tasks with a clear \
                 continuation plan."
                    .to_string(),
                "Re-scope the task with the requester: confirm which deliverables are \
                 optional before proceeding."
                    .to_string(),
            ],
            EscalationType::Loop => {
                let loop_description = self
                    .attempts
                    .last()
                    .map(|attempt| attempt.description.as_str())
                    .unwrap_or("the current action");

                vec![
                    format!(
                        "Verify whether the precondition for '{loop_description}' is actually \
                         satisfied before retrying."
                    ),
                    "Try a structurally different approach: if the current tool is failing, \
                     replace it rather than retrying with modified arguments."
                        .to_string(),
                    "Collect diagnostic output from the last failure and treat it as a new \
                     starting point rather than repeating the same action."
                        .to_string(),
                ]
            }
            EscalationType::Confidence => vec![
                "Run a targeted probe — a single, cheap, falsifiable test — to confirm or \
                 refute the most uncertain assumption."
                    .to_string(),
                "List the unknowns explicitly and gather evidence for the highest-impact one \
                 first."
                    .to_string(),
                "Ask the requester to clarify the success criteria so confidence can be \
                 grounded in a concrete definition of done."
                    .to_string(),
            ],
            EscalationType::Blocked | EscalationType::Unknown => vec![
                "Identify the specific dependency or constraint that is preventing progress \
                 and check whether it can be bypassed."
                    .to_string(),
                "Switch to a parallel sub-task that does not depend on the blocker, returning \
                 to the blocked path once it clears."
                    .to_string(),
                "Escalate the blocker with a clear description of what is needed, who can \
                 provide it, and the impact of the delay."
                    .to_string(),
            ],
        }
    }

    /// Call an LLM to generate three ranked hypotheses. Returns an error on any failure.
    async fn try_llm_hypotheses(
        &self,
        llm_manager: &Arc<LlmManager>,
        routing: &RoutingConfig,
        agent_id: &str,
    ) -> anyhow::Result<Vec<String>> {
        let model_name = routing.resolve(ProcessType::Branch, None).to_string();
        let model = crate::llm::SpacebotModel::make(llm_manager, &model_name)
            .with_context(agent_id, "escape")
            .with_routing(routing.clone());

        let preamble = "You are an expert debugging assistant. Given a summary of what an AI \
            agent has tried and the signals that triggered an escalation, produce exactly three \
            hypotheses about what to try next, ranked from most to least testable. \
            Return only a numbered list: 1. ... 2. ... 3. ... — no other text.";

        let agent = AgentBuilder::new(model).preamble(preamble).build();

        let prompt = self.build_llm_prompt();
        let response = agent.prompt(&prompt).await?;

        let hypotheses = parse_numbered_list(&response);
        if hypotheses.len() < 3 {
            anyhow::bail!(
                "LLM returned {} hypotheses, expected 3; falling back to template",
                hypotheses.len()
            );
        }

        Ok(hypotheses.into_iter().take(3).collect())
    }

    /// Build the input prompt for `try_llm_hypotheses`.
    fn build_llm_prompt(&self) -> String {
        let mut lines: Vec<String> = Vec::new();

        lines.push(format!(
            "Escalation type: {}\n\
             Confidence: {:.0}% | Evidence items: {} | Budget used: {:.0}% | Watcher firings: {}",
            self.classify_escalation_type_label(),
            self.confidence * 100.0,
            self.evidence_count,
            self.time_budget_used_pct * 100.0,
            self.watcher_firing_count,
        ));

        if !self.attempts.is_empty() {
            lines.push("\nAttempts so far:".to_string());
            for (index, attempt) in self.attempts.iter().enumerate() {
                lines.push(format!(
                    "{}. {} → {}",
                    index + 1,
                    attempt.description,
                    attempt.outcome,
                ));
            }
        }

        if !self.evidence_gathered.is_empty() {
            lines.push("\nEvidence gathered:".to_string());
            for evidence in self.evidence_gathered.iter().take(5) {
                lines.push(format!("- {evidence}"));
            }
            if self.evidence_gathered.len() > 5 {
                lines.push(format!(
                    "  … and {} more items",
                    self.evidence_gathered.len() - 5
                ));
            }
        }

        lines.push(
            "\nProvide three hypotheses ranked by testability (most testable first).".to_string(),
        );

        lines.join("\n")
    }
}

impl Default for EscapeProtocol {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/// Parse lines of the form "1. text", "2. text", "3. text" from an LLM response.
fn parse_numbered_list(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            // Match "1.", "2.", "3." etc. at the start of a line.
            let after_number = trimmed
                .split_once('.')
                .and_then(|(prefix, rest)| {
                    if prefix.trim().parse::<usize>().is_ok() {
                        Some(rest.trim())
                    } else {
                        None
                    }
                })?;
            if after_number.is_empty() {
                None
            } else {
                Some(after_number.to_string())
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn protocol_with_metrics(
        firings: u32,
        confidence: f64,
        evidence_count: u32,
        budget_pct: f64,
    ) -> EscapeProtocol {
        let mut protocol = EscapeProtocol::new();
        protocol.update_metrics(firings, confidence, evidence_count, budget_pct);
        protocol
    }

    // --- should_trigger ---

    #[test]
    fn trigger_on_loop_firings() {
        let protocol = protocol_with_metrics(3, 0.9, 5, 0.5);
        assert!(protocol.should_trigger());
    }

    #[test]
    fn no_trigger_below_loop_threshold() {
        let protocol = protocol_with_metrics(2, 0.9, 5, 0.5);
        assert!(!protocol.should_trigger());
    }

    #[test]
    fn trigger_on_low_confidence_with_no_evidence() {
        let protocol = protocol_with_metrics(0, 0.2, 1, 0.5);
        assert!(protocol.should_trigger());
    }

    #[test]
    fn no_trigger_low_confidence_with_sufficient_evidence() {
        // confidence < 0.3 but evidence_count >= 2 — suppressed.
        let protocol = protocol_with_metrics(0, 0.2, 2, 0.5);
        assert!(!protocol.should_trigger());
    }

    #[test]
    fn trigger_on_budget_threshold() {
        let protocol = protocol_with_metrics(0, 0.9, 5, 0.81);
        assert!(protocol.should_trigger());
    }

    #[test]
    fn no_trigger_exactly_at_budget_boundary() {
        // > 0.80, not >= 0.80
        let protocol = protocol_with_metrics(0, 0.9, 5, 0.80);
        assert!(!protocol.should_trigger());
    }

    #[test]
    fn healthy_state_does_not_trigger() {
        let protocol = protocol_with_metrics(1, 0.8, 3, 0.6);
        assert!(!protocol.should_trigger());
    }

    // --- escalation type classification ---

    #[test]
    fn budget_escalation_type_takes_priority() {
        // All conditions met — budget wins.
        let protocol = protocol_with_metrics(3, 0.1, 0, 0.9);
        assert_eq!(protocol.classify_escalation_type(), EscalationType::Budget);
    }

    #[test]
    fn loop_escalation_type_when_budget_ok() {
        let protocol = protocol_with_metrics(3, 0.9, 5, 0.5);
        assert_eq!(protocol.classify_escalation_type(), EscalationType::Loop);
    }

    #[test]
    fn confidence_escalation_type_when_only_confidence_low() {
        let protocol = protocol_with_metrics(0, 0.1, 0, 0.5);
        assert_eq!(
            protocol.classify_escalation_type(),
            EscalationType::Confidence
        );
    }

    // --- request type classification ---

    #[test]
    fn info_request_when_no_attempts() {
        let protocol = EscapeProtocol::new();
        assert_eq!(protocol.classify_request_type(), RequestType::Info);
    }

    #[test]
    fn help_request_when_attempts_but_no_evidence() {
        let mut protocol = EscapeProtocol::new();
        protocol.record_attempt("tried foo", "failed", vec!["shell".to_string()]);
        assert_eq!(protocol.classify_request_type(), RequestType::Help);
    }

    #[test]
    fn decision_request_when_attempts_and_evidence() {
        let mut protocol = EscapeProtocol::new();
        protocol.record_attempt("tried foo", "partial", vec!["shell".to_string()]);
        protocol.record_evidence("test suite: 14 passed, 2 failed".to_string());
        assert_eq!(protocol.classify_request_type(), RequestType::Decision);
    }

    // --- template generation ---

    #[test]
    fn execute_produces_three_hypotheses() {
        let mut protocol = protocol_with_metrics(3, 0.9, 5, 0.5);
        protocol.record_attempt("ran tests", "partial", vec!["shell".to_string()]);
        let result = protocol.execute();
        assert_eq!(result.escalation.suggested_options.len(), 3);
    }

    #[test]
    fn execute_budget_type_mentions_scope() {
        let protocol = protocol_with_metrics(0, 0.9, 5, 0.9);
        let result = protocol.execute();
        assert_eq!(result.escalation.escalation_type, EscalationType::Budget);
        // All three hypotheses should be non-empty.
        for hypothesis in &result.escalation.suggested_options {
            assert!(!hypothesis.is_empty());
        }
    }

    #[test]
    fn learning_artifact_includes_attempt_details() {
        let mut protocol = EscapeProtocol::new();
        protocol.record_attempt(
            "ran cargo build",
            "failure",
            vec!["shell".to_string(), "file".to_string()],
        );
        protocol.record_evidence("error: linker not found".to_string());
        let artifact = protocol.generate_learning_artifact();
        assert!(artifact.contains("ran cargo build"));
        assert!(artifact.contains("error: linker not found"));
    }

    #[test]
    fn parse_numbered_list_extracts_three_items() {
        let text = "1. First hypothesis\n2. Second hypothesis\n3. Third hypothesis";
        let items = parse_numbered_list(text);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], "First hypothesis");
        assert_eq!(items[2], "Third hypothesis");
    }

    #[test]
    fn parse_numbered_list_ignores_non_numbered_lines() {
        let text = "Here are the hypotheses:\n1. First\nSome interstitial text\n2. Second\n3. Third";
        let items = parse_numbered_list(text);
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn record_evidence_increments_count() {
        let mut protocol = EscapeProtocol::new();
        assert_eq!(protocol.evidence_count, 0);
        protocol.record_evidence("first piece".to_string());
        assert_eq!(protocol.evidence_count, 1);
        protocol.record_evidence("second piece".to_string());
        assert_eq!(protocol.evidence_count, 2);
    }
}
