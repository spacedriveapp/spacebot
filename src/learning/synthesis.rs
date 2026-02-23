//! Tiered synthesis with emission budget for the learning advisory system.
//!
//! Two synthesis tiers: programmatic template formatting (Tier 1) and
//! AI-driven paragraph synthesis (Tier 2). The emission budget tracks
//! delivery rate and tier-2 reliability to prevent advisory noise and
//! disable expensive synthesis when it consistently falls back.

use crate::learning::AuthorityLevel;
use crate::llm::{LlmManager, RoutingConfig, SpacebotModel};

use rig::agent::AgentBuilder;
use rig::completion::{CompletionModel, Prompt};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::time::Duration;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum items included per synthesis call (emission budget per event).
const MAX_ITEMS_PER_SYNTHESIS: usize = 2;

/// Rolling window capacity for emission rate tracking.
const EMISSION_WINDOW_SIZE: usize = 100;

/// Minimum tier-2 attempts required before the fallback rate can disable tier 2.
///
/// Below this sample size the rate is too noisy to act on.
const TIER2_MIN_SAMPLE: u32 = 80;

/// Tier-2 fallback rate above which AI synthesis is considered unreliable.
const TIER2_DISABLE_THRESHOLD: f64 = 0.55;

// ---------------------------------------------------------------------------
// Input types
// ---------------------------------------------------------------------------

/// A single advice item ready for synthesis.
#[derive(Debug, Clone)]
pub struct AdviceItem {
    /// The action or recommendation. Should lead with an imperative verb.
    pub advice_text: String,
    /// Supporting evidence for this advice.
    pub evidence: String,
    /// Authority level assigned by the advisory pipeline.
    pub authority_level: AuthorityLevel,
    /// Source category (e.g. "pattern", "rule", "observation").
    pub source_type: String,
}

/// Input bundle for a synthesis call.
#[derive(Debug, Clone)]
pub struct SynthesisInput {
    /// Advice items to synthesize, ordered by relevance from the caller.
    pub advice_items: Vec<AdviceItem>,
    /// Short context summary injected into tier-2 prompts.
    pub context_summary: String,
}

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

/// Which synthesis tier produced the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SynthesisTier {
    /// Programmatic template-based formatting.
    Tier1,
    /// AI-driven paragraph synthesis.
    Tier2,
}

impl std::fmt::Display for SynthesisTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tier1 => write!(f, "tier1"),
            Self::Tier2 => write!(f, "tier2"),
        }
    }
}

/// Output from a synthesis call.
#[derive(Debug, Clone)]
pub struct SynthesisOutput {
    /// The formatted advisory text ready for delivery.
    pub formatted: String,
    /// Which tier produced this output.
    pub tier_used: SynthesisTier,
    /// Number of items included in the synthesis.
    pub items_included: usize,
}

// ---------------------------------------------------------------------------
// Tier 1: programmatic synthesis
// ---------------------------------------------------------------------------

/// Format advice items as action-first lines joined by newlines.
///
/// Each line follows the pattern `"{advice_text} — {evidence}"`.
/// At most `MAX_ITEMS_PER_SYNTHESIS` items are included regardless of how
/// many are provided — the excess is silently dropped.
pub fn synthesize_tier1(items: &[AdviceItem]) -> String {
    items
        .iter()
        .take(MAX_ITEMS_PER_SYNTHESIS)
        .map(|item| format!("{} — {}", item.advice_text, item.evidence))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Tier 2: AI synthesis
// ---------------------------------------------------------------------------

/// Attempt AI-driven synthesis within the given time budget.
///
/// Builds a short-lived agent using the worker model from the routing config
/// and asks it to combine the items into a coherent, action-first paragraph.
/// Returns `None` on any error or timeout so callers can fall back to tier 1
/// without propagating failures.
pub async fn synthesize_tier2(
    items: &[AdviceItem],
    context: &str,
    llm_manager: &Arc<LlmManager>,
    routing: &RoutingConfig,
    agent_id: &str,
    budget_ms: u64,
) -> Option<String> {
    let model_name = routing.worker.clone();
    let model = SpacebotModel::make(llm_manager, &model_name)
        .with_context(agent_id, "synthesis")
        .with_routing(routing.clone());

    let agent = AgentBuilder::new(model)
        .preamble(
            "You are an advisory synthesis engine. Combine these advice items into a \
             coherent, action-first paragraph. Be concise.",
        )
        .default_max_turns(1)
        .build();

    let prompt_text = build_tier2_prompt(items, context);

    let synthesis_future = async move { agent.prompt(&prompt_text).await };
    match tokio::time::timeout(Duration::from_millis(budget_ms), synthesis_future).await {
        Ok(Ok(response)) => Some(response),
        Ok(Err(error)) => {
            tracing::debug!(%error, "tier-2 synthesis failed, falling back to tier 1");
            None
        }
        Err(_elapsed) => {
            tracing::debug!(budget_ms, "tier-2 synthesis timed out, falling back to tier 1");
            None
        }
    }
}

/// Build the prompt text sent to the tier-2 synthesis agent.
fn build_tier2_prompt(items: &[AdviceItem], context: &str) -> String {
    let mut prompt = String::new();

    if !context.is_empty() {
        prompt.push_str("Context: ");
        prompt.push_str(context);
        prompt.push_str("\n\n");
    }

    prompt.push_str("Advice items:\n");
    for (index, item) in items.iter().enumerate() {
        prompt.push_str(&format!(
            "{}. [{}] {} (evidence: {})\n",
            index + 1,
            item.authority_level,
            item.advice_text,
            item.evidence,
        ));
    }

    prompt
}

// ---------------------------------------------------------------------------
// Top-level synthesize
// ---------------------------------------------------------------------------

/// Produce a synthesis output, selecting the appropriate tier automatically.
///
/// Filters the input to the top two items by authority level. When
/// `allow_tier2` is set and any item is WARNING or above, tier-2 is attempted
/// within `budget_ms`. Any failure falls through transparently to tier 1.
pub async fn synthesize(
    input: &SynthesisInput,
    llm_manager: &Arc<LlmManager>,
    routing: &RoutingConfig,
    agent_id: &str,
    budget_ms: u64,
    allow_tier2: bool,
) -> SynthesisOutput {
    // Sort descending by authority level to surface the highest-priority items.
    let mut ranked = input.advice_items.clone();
    ranked.sort_by(|a, b| b.authority_level.cmp(&a.authority_level));
    let top_items: Vec<AdviceItem> = ranked.into_iter().take(MAX_ITEMS_PER_SYNTHESIS).collect();

    let has_elevated_authority = top_items
        .iter()
        .any(|item| item.authority_level >= AuthorityLevel::Warning);

    if allow_tier2 && has_elevated_authority {
        if let Some(formatted) = synthesize_tier2(
            &top_items,
            &input.context_summary,
            llm_manager,
            routing,
            agent_id,
            budget_ms,
        )
        .await
        {
            return SynthesisOutput {
                items_included: top_items.len(),
                formatted,
                tier_used: SynthesisTier::Tier2,
            };
        }
    }

    let formatted = synthesize_tier1(&top_items);
    SynthesisOutput {
        items_included: top_items.len(),
        formatted,
        tier_used: SynthesisTier::Tier1,
    }
}

// ---------------------------------------------------------------------------
// EmissionBudget
// ---------------------------------------------------------------------------

/// Tracks advisory delivery rate and tier-2 synthesis reliability.
///
/// The rolling window records whether each of the last `EMISSION_WINDOW_SIZE`
/// advisory events produced an emission. The tier-2 counters accumulate over
/// the lifetime of the budget and are checked against a minimum sample size
/// before making a reliability determination.
#[derive(Debug)]
pub struct EmissionBudget {
    /// Maximum emissions allowed per individual event call (burst guard).
    pub max_per_event: usize,
    /// Lower bound on the acceptable emission rate.
    pub target_rate_min: f64,
    /// Upper bound on the acceptable emission rate.
    pub target_rate_max: f64,
    /// Rolling window of emission outcomes. `true` means an emission occurred.
    recent_emissions: VecDeque<bool>,
    /// Number of tier-2 attempts that fell back to tier 1.
    tier2_fallback_count: u32,
    /// Total tier-2 attempts recorded.
    tier2_total_count: u32,
}

impl EmissionBudget {
    /// Create a new emission budget with the given rate targets.
    pub fn new(max_per_event: usize, target_rate_min: f64, target_rate_max: f64) -> Self {
        Self {
            max_per_event,
            target_rate_min,
            target_rate_max,
            recent_emissions: VecDeque::with_capacity(EMISSION_WINDOW_SIZE),
            tier2_fallback_count: 0,
            tier2_total_count: 0,
        }
    }

    /// Whether the current emission rate is within the target window.
    ///
    /// Returns `true` unconditionally until the rolling window fills up,
    /// because a short history is too noisy to enforce rate limiting against.
    pub fn can_emit(&self) -> bool {
        if self.recent_emissions.len() < EMISSION_WINDOW_SIZE {
            return true;
        }
        let rate = self.current_rate();
        rate >= self.target_rate_min && rate <= self.target_rate_max
    }

    /// Record an emission outcome into the rolling window.
    ///
    /// Evicts the oldest entry when the window is at capacity.
    pub fn record_emission(&mut self, emitted: bool) {
        if self.recent_emissions.len() == EMISSION_WINDOW_SIZE {
            self.recent_emissions.pop_front();
        }
        self.recent_emissions.push_back(emitted);
    }

    /// Fraction of events in the rolling window that produced an emission.
    ///
    /// Returns `0.0` when the window is empty.
    pub fn current_rate(&self) -> f64 {
        if self.recent_emissions.is_empty() {
            return 0.0;
        }
        let emitted_count = self.recent_emissions.iter().filter(|&&emitted| emitted).count();
        emitted_count as f64 / self.recent_emissions.len() as f64
    }

    /// Whether tier-2 synthesis should be disabled due to excessive fallbacks.
    ///
    /// Requires at least `TIER2_MIN_SAMPLE` recorded attempts before drawing
    /// a conclusion. Below that threshold, tier 2 is always considered viable.
    pub fn should_disable_tier2(&self) -> bool {
        if self.tier2_total_count < TIER2_MIN_SAMPLE {
            return false;
        }
        let fallback_rate = self.tier2_fallback_count as f64 / self.tier2_total_count as f64;
        fallback_rate > TIER2_DISABLE_THRESHOLD
    }

    /// Record the outcome of a tier-2 synthesis attempt.
    ///
    /// Pass `fell_back = true` when tier 2 failed and the caller used tier 1.
    pub fn record_tier2_attempt(&mut self, fell_back: bool) {
        self.tier2_total_count += 1;
        if fell_back {
            self.tier2_fallback_count += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_item(advice_text: &str, evidence: &str, authority_level: AuthorityLevel) -> AdviceItem {
        AdviceItem {
            advice_text: advice_text.to_string(),
            evidence: evidence.to_string(),
            authority_level,
            source_type: "test".to_string(),
        }
    }

    // --- Tier 1 synthesis ---

    #[test]
    fn tier1_formats_action_and_evidence() {
        let items = vec![make_item(
            "Run tests before merging",
            "3 regressions introduced without coverage",
            AuthorityLevel::Warning,
        )];
        let result = synthesize_tier1(&items);
        assert_eq!(
            result,
            "Run tests before merging — 3 regressions introduced without coverage"
        );
    }

    #[test]
    fn tier1_joins_two_items_with_newline() {
        let items = vec![
            make_item("Check types first", "type errors blocked 2 tasks", AuthorityLevel::Note),
            make_item(
                "Commit incrementally",
                "large commits caused merge conflicts",
                AuthorityLevel::Warning,
            ),
        ];
        let result = synthesize_tier1(&items);
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("Check types first"));
        assert!(lines[1].contains("Commit incrementally"));
    }

    #[test]
    fn tier1_caps_output_at_two_items() {
        let items = vec![
            make_item("Action A", "evidence A", AuthorityLevel::Block),
            make_item("Action B", "evidence B", AuthorityLevel::Warning),
            make_item("Action C", "evidence C", AuthorityLevel::Note),
        ];
        let result = synthesize_tier1(&items);
        assert_eq!(result.lines().count(), 2);
        assert!(!result.contains("Action C"), "third item must not appear in output");
    }

    #[test]
    fn tier1_empty_input_returns_empty_string() {
        assert_eq!(synthesize_tier1(&[]), "");
    }

    #[test]
    fn tier1_single_item_has_no_trailing_newline() {
        let items = vec![make_item("Do the thing", "because reasons", AuthorityLevel::Note)];
        let result = synthesize_tier1(&items);
        assert!(!result.ends_with('\n'));
    }

    // --- EmissionBudget: can_emit ---

    #[test]
    fn emission_budget_allows_emit_on_empty_window() {
        let budget = EmissionBudget::new(2, 0.1, 0.8);
        assert!(budget.can_emit());
    }

    #[test]
    fn emission_budget_allows_emit_while_window_filling() {
        let mut budget = EmissionBudget::new(2, 0.1, 0.8);
        for _ in 0..(EMISSION_WINDOW_SIZE - 1) {
            budget.record_emission(true);
        }
        // Window is not yet full: rate enforcement is suspended.
        assert!(budget.can_emit());
    }

    #[test]
    fn emission_budget_blocks_when_rate_exceeds_max() {
        let mut budget = EmissionBudget::new(2, 0.0, 0.5);
        // All true → rate = 1.0, above max 0.5.
        for _ in 0..EMISSION_WINDOW_SIZE {
            budget.record_emission(true);
        }
        assert!(!budget.can_emit());
    }

    #[test]
    fn emission_budget_blocks_when_rate_below_min() {
        let mut budget = EmissionBudget::new(2, 0.5, 0.9);
        // All false → rate = 0.0, below min 0.5.
        for _ in 0..EMISSION_WINDOW_SIZE {
            budget.record_emission(false);
        }
        assert!(!budget.can_emit());
    }

    #[test]
    fn emission_budget_allows_emit_when_rate_in_range() {
        let mut budget = EmissionBudget::new(2, 0.4, 0.6);
        // Alternate true/false → rate = 0.5, within [0.4, 0.6].
        for i in 0..EMISSION_WINDOW_SIZE {
            budget.record_emission(i % 2 == 0);
        }
        assert!(budget.can_emit());
    }

    // --- EmissionBudget: current_rate ---

    #[test]
    fn emission_budget_current_rate_is_zero_when_empty() {
        let budget = EmissionBudget::new(2, 0.1, 0.8);
        assert_eq!(budget.current_rate(), 0.0);
    }

    #[test]
    fn emission_budget_computes_rate_correctly() {
        let mut budget = EmissionBudget::new(2, 0.1, 0.9);
        budget.record_emission(true);
        budget.record_emission(false);
        budget.record_emission(true);
        // 2 out of 3 emitted.
        let expected = 2.0 / 3.0;
        assert!((budget.current_rate() - expected).abs() < f64::EPSILON);
    }

    // --- EmissionBudget: rolling window eviction ---

    #[test]
    fn emission_budget_rolling_window_evicts_oldest_entry() {
        let mut budget = EmissionBudget::new(1, 0.0, 1.0);
        // Fill the window entirely with `true`.
        for _ in 0..EMISSION_WINDOW_SIZE {
            budget.record_emission(true);
        }
        assert_eq!(budget.current_rate(), 1.0);
        // Push one `false`. The oldest `true` must be evicted.
        budget.record_emission(false);
        assert_eq!(budget.recent_emissions.len(), EMISSION_WINDOW_SIZE);
        let expected = 99.0 / 100.0;
        assert!((budget.current_rate() - expected).abs() < f64::EPSILON);
    }

    // --- EmissionBudget: tier-2 reliability ---

    #[test]
    fn tier2_not_disabled_below_minimum_sample() {
        let mut budget = EmissionBudget::new(2, 0.1, 0.9);
        // 79 attempts — one below the threshold.
        for _ in 0..79 {
            budget.record_tier2_attempt(true);
        }
        assert!(!budget.should_disable_tier2());
    }

    #[test]
    fn tier2_disabled_when_fallback_rate_exceeds_threshold() {
        let mut budget = EmissionBudget::new(2, 0.1, 0.9);
        // 80 fallbacks out of 80 → rate 1.0 > 0.55.
        for _ in 0..80 {
            budget.record_tier2_attempt(true);
        }
        assert!(budget.should_disable_tier2());
    }

    #[test]
    fn tier2_not_disabled_when_fallback_rate_is_acceptable() {
        let mut budget = EmissionBudget::new(2, 0.1, 0.9);
        // 30 fallbacks then 50 successes → rate 0.375 < 0.55.
        for _ in 0..30 {
            budget.record_tier2_attempt(true);
        }
        for _ in 0..50 {
            budget.record_tier2_attempt(false);
        }
        assert!(!budget.should_disable_tier2());
    }

    #[test]
    fn tier2_disabled_exactly_at_threshold_boundary() {
        let mut budget = EmissionBudget::new(2, 0.1, 0.9);
        // 45 fallbacks, 35 successes out of 80 → rate = 0.5625 > 0.55.
        for _ in 0..45 {
            budget.record_tier2_attempt(true);
        }
        for _ in 0..35 {
            budget.record_tier2_attempt(false);
        }
        assert!(budget.should_disable_tier2());
    }
}
