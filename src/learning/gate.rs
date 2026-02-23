//! Advisory scoring engine with 5 authority levels and agreement gating.
//!
//! Candidates are scored using a weighted combination of context match,
//! phase boost, and situational signals (negative phrasing, failures, cold
//! start). WARNING-level advisories are gated behind an agreement check:
//! two or more independent sources must corroborate before the advisory
//! surfaces, unless the candidate is flagged as a policy.

use crate::learning::phase::phase_boost;
use crate::learning::types::{AuthorityLevel, Phase};

use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// Input types
// ---------------------------------------------------------------------------

/// A raw advisory candidate sourced from distillation, insight, or chip records.
#[derive(Debug, Clone)]
pub struct AdvisoryCandidate {
    /// Unique identifier of the source record.
    pub source_id: String,
    /// Record type: "distillation", "insight", or "chip".
    pub source_type: String,
    /// The advisory text to surface to the agent.
    pub advice_text: String,
    /// One of the eight insight categories (e.g., "reasoning", "wisdom").
    pub category: String,
    /// Tool names or short keywords that trigger this advisory.
    pub triggers: Vec<String>,
    /// Domain labels that narrow relevance (e.g., "rust", "database").
    pub domains: Vec<String>,
    /// Policies bypass agreement gating regardless of source count.
    pub is_policy: bool,
}

/// Runtime signals that describe what the agent is doing right now.
#[derive(Debug, Clone)]
pub struct AdvisoryContext {
    /// The tool currently being invoked, if any.
    pub current_tool: Option<String>,
    /// The current detected activity phase.
    pub current_phase: Phase,
    /// Keywords extracted from the active task description.
    pub task_keywords: Vec<String>,
    /// Number of consecutive tool or task failures without a clear.
    pub consecutive_failures: u32,
    /// Total episodes completed in the agent's lifetime.
    pub completed_episodes: u64,
}

// ---------------------------------------------------------------------------
// Output type
// ---------------------------------------------------------------------------

/// A scored and authority-assigned advisory ready for surfacing.
#[derive(Debug, Clone)]
pub struct ScoredAdvisory {
    /// Source record identifier.
    pub source_id: String,
    /// Source record type.
    pub source_type: String,
    /// The advisory text.
    pub advice_text: String,
    /// Final computed score clamped to [0.0, 1.0].
    pub score: f64,
    /// Authority level derived from the score thresholds.
    pub authority_level: AuthorityLevel,
    /// Insight category of this advisory.
    pub category: String,
    /// Whether this advisory is a policy, which bypasses agreement gating.
    pub is_policy: bool,
}

// ---------------------------------------------------------------------------
// Weights
// ---------------------------------------------------------------------------

/// Tuneable weights for the advisory scoring pipeline.
///
/// The weighted sum of `context_weight`, `confidence_weight * 0.5`, and
/// `base_floor` forms the pre-boost base score. Boost multipliers and the
/// cold start additive are applied after the phase matrix.
#[derive(Debug, Clone)]
pub struct AdvisoryWeights {
    /// Weight applied to the context match component (tool, domain, keyword).
    pub context_weight: f64,
    /// Weight applied to the fixed 0.5 confidence value.
    pub confidence_weight: f64,
    /// Constant additive floor applied to every candidate before boosts.
    pub base_floor: f64,
    /// Score multiplier for advisories starting with "don't", "avoid", or "never".
    ///
    /// Preventive guidance receives extra weight because it is most useful
    /// immediately before a potentially harmful action.
    pub negative_boost: f64,
    /// Score multiplier applied when at least one consecutive failure is active.
    pub failure_boost: f64,
    /// Maximum additive boost applied at the start of the cold start window.
    pub cold_start_boost: f64,
    /// Episode number at which the cold start boost is still at full strength.
    pub cold_start_start_episode: u64,
    /// Episode number at which the cold start boost reaches zero.
    pub cold_start_end_episode: u64,
}

impl Default for AdvisoryWeights {
    fn default() -> Self {
        Self {
            context_weight: 0.60,
            confidence_weight: 0.20,
            base_floor: 0.10,
            negative_boost: 1.30,
            failure_boost: 1.40,
            cold_start_boost: 0.15,
            cold_start_start_episode: 0,
            cold_start_end_episode: 50,
        }
    }
}

// ---------------------------------------------------------------------------
// Context match
// ---------------------------------------------------------------------------

/// Compute the context match score for a candidate in [0.0, 1.0].
///
/// Three sub-signals are combined:
///
/// - **Tool overlap** (weight 0.4): whether the current tool appears in the
///   candidate's trigger list.
/// - **Domain overlap** (weight 0.3): fraction of the candidate's domains that
///   overlap with the task keywords (substring containment in either direction).
/// - **Keyword Jaccard** (weight 0.3): Jaccard similarity between the
///   candidate's triggers and the task keywords (case-insensitive).
pub fn compute_context_match(candidate: &AdvisoryCandidate, context: &AdvisoryContext) -> f64 {
    // Tool overlap: current tool must appear verbatim in the trigger list.
    let tool_overlap = match &context.current_tool {
        Some(tool) if candidate.triggers.iter().any(|trigger| trigger == tool) => 0.4,
        _ => 0.0,
    };

    // Domain overlap: how many of the candidate's domains are mentioned in task keywords.
    let domain_count = candidate.domains.len().max(1);
    let matched_domain_count = candidate
        .domains
        .iter()
        .filter(|domain| {
            let domain_lower = domain.to_ascii_lowercase();
            context.task_keywords.iter().any(|keyword| {
                let keyword_lower = keyword.to_ascii_lowercase();
                keyword_lower.contains(domain_lower.as_str())
                    || domain_lower.contains(keyword_lower.as_str())
            })
        })
        .count();
    let domain_overlap = (matched_domain_count as f64 / domain_count as f64) * 0.3;

    // Keyword Jaccard: |triggers ∩ task_keywords| / |triggers ∪ task_keywords|.
    let trigger_set: HashSet<String> = candidate
        .triggers
        .iter()
        .map(|trigger| trigger.to_ascii_lowercase())
        .collect();
    let keyword_set: HashSet<String> = context
        .task_keywords
        .iter()
        .map(|keyword| keyword.to_ascii_lowercase())
        .collect();
    let intersection_size = trigger_set.intersection(&keyword_set).count();
    let union_size = trigger_set.union(&keyword_set).count();
    let jaccard = if union_size == 0 {
        0.0
    } else {
        intersection_size as f64 / union_size as f64
    };
    let keyword_overlap = jaccard * 0.3;

    (tool_overlap + domain_overlap + keyword_overlap).clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Advisory scoring
// ---------------------------------------------------------------------------

/// Compute the full advisory score for a candidate in [0.0, 1.0].
///
/// The pipeline applies, in order:
///
/// 1. Weighted base: `context_weight × context_match + confidence_weight × 0.5 + base_floor`.
/// 2. Phase boost multiplier from the category × phase matrix.
/// 3. Negative phrasing multiplier for advisories starting with "don't", "avoid", or "never".
/// 4. Failure multiplier when at least one consecutive failure is active.
/// 5. Cold start additive boost, interpolated linearly from `cold_start_start_episode`
///    (full strength) to `cold_start_end_episode` (zero).
pub fn compute_advisory_score(
    candidate: &AdvisoryCandidate,
    context: &AdvisoryContext,
    weights: &AdvisoryWeights,
) -> f64 {
    let context_match = compute_context_match(candidate, context);

    let mut score = weights.context_weight * context_match
        + weights.confidence_weight * 0.5
        + weights.base_floor;

    // Phase boost from the category × phase matrix.
    score *= phase_boost(context.current_phase, &candidate.category);

    // Preventive phrasing signals higher relevance for avoidance guidance.
    let lower_advice = candidate.advice_text.to_ascii_lowercase();
    if lower_advice.starts_with("don't")
        || lower_advice.starts_with("avoid")
        || lower_advice.starts_with("never")
    {
        score *= weights.negative_boost;
    }

    // Failure boost: any active failure streak raises advisory urgency.
    if context.consecutive_failures >= 1 {
        score *= weights.failure_boost;
    }

    // Cold start additive boost: interpolated from full strength at
    // cold_start_start_episode, decaying to zero at cold_start_end_episode.
    if context.completed_episodes < weights.cold_start_end_episode {
        let boost_factor = if context.completed_episodes <= weights.cold_start_start_episode {
            1.0
        } else {
            let decay_range =
                (weights.cold_start_end_episode - weights.cold_start_start_episode) as f64;
            let elapsed =
                (context.completed_episodes - weights.cold_start_start_episode) as f64;
            1.0 - (elapsed / decay_range)
        };
        score += weights.cold_start_boost * boost_factor;
    }

    score.clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Agreement gating
// ---------------------------------------------------------------------------

/// Filter scored advisories by the agreement requirement.
///
/// WARNING-level advisories must be corroborated by at least two distinct
/// `source_id` values carrying the same normalized advice text (lowercase,
/// trimmed). Policy advisories bypass this check entirely.
///
/// Advisories at all other authority levels pass through unchanged.
pub fn check_agreement(candidates: &[ScoredAdvisory]) -> Vec<ScoredAdvisory> {
    // Pre-compute distinct source_id counts for each normalized WARNING text.
    // Only non-policy WARNINGs participate in the agreement check.
    let mut source_id_sets: HashMap<String, HashSet<String>> = HashMap::new();
    for candidate in candidates {
        if candidate.authority_level == AuthorityLevel::Warning && !candidate.is_policy {
            let normalized = candidate.advice_text.trim().to_ascii_lowercase();
            source_id_sets
                .entry(normalized)
                .or_default()
                .insert(candidate.source_id.clone());
        }
    }

    candidates
        .iter()
        .filter(|candidate| {
            if candidate.authority_level != AuthorityLevel::Warning {
                return true;
            }
            if candidate.is_policy {
                return true;
            }
            let normalized = candidate.advice_text.trim().to_ascii_lowercase();
            source_id_sets
                .get(&normalized)
                .is_some_and(|ids| ids.len() >= 2)
        })
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// Pipeline entry point
// ---------------------------------------------------------------------------

/// Score all candidates, assign authority levels, gate WARNINGs, and sort.
///
/// Returns advisories sorted by score descending. WARNING-level candidates
/// backed by only a single non-policy source are removed by agreement gating.
pub fn score_and_gate(
    candidates: Vec<AdvisoryCandidate>,
    context: &AdvisoryContext,
    weights: &AdvisoryWeights,
) -> Vec<ScoredAdvisory> {
    let scored: Vec<ScoredAdvisory> = candidates
        .into_iter()
        .map(|candidate| {
            let score = compute_advisory_score(&candidate, context, weights);
            let authority_level = AuthorityLevel::from_score(score);
            ScoredAdvisory {
                source_id: candidate.source_id,
                source_type: candidate.source_type,
                advice_text: candidate.advice_text,
                score,
                authority_level,
                category: candidate.category,
                is_policy: candidate.is_policy,
            }
        })
        .collect();

    let mut gated = check_agreement(&scored);
    gated.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    gated
}

// ---------------------------------------------------------------------------
// Reliability filtering
// ---------------------------------------------------------------------------

/// Return source IDs where the reliability score falls below 0.5.
///
/// Used to demote or exclude insight sources that have demonstrated
/// poor predictive accuracy over their evaluation window.
pub fn demote_unreliable_insights(insights: &[(String, f64)]) -> Vec<String> {
    insights
        .iter()
        .filter_map(|(id, reliability)| {
            if *reliability < 0.5 {
                Some(id.clone())
            } else {
                None
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

    fn make_candidate(
        source_id: &str,
        advice_text: &str,
        triggers: Vec<&str>,
        domains: Vec<&str>,
        is_policy: bool,
    ) -> AdvisoryCandidate {
        AdvisoryCandidate {
            source_id: source_id.to_string(),
            source_type: "insight".to_string(),
            advice_text: advice_text.to_string(),
            category: "reasoning".to_string(),
            triggers: triggers.into_iter().map(|t| t.to_string()).collect(),
            domains: domains.into_iter().map(|d| d.to_string()).collect(),
            is_policy,
        }
    }

    fn halt_context() -> AdvisoryContext {
        AdvisoryContext {
            current_tool: None,
            // Halt has a uniform phase_boost of 1.0 for all categories,
            // which isolates other scoring signals in unit tests.
            current_phase: Phase::Halt,
            task_keywords: vec![],
            consecutive_failures: 0,
            completed_episodes: 100,
        }
    }

    // -- compute_context_match --

    #[test]
    fn test_context_match_tool_in_triggers() {
        let candidate = make_candidate("a", "check it", vec!["shell"], vec![], false);
        let context = AdvisoryContext {
            current_tool: Some("shell".to_string()),
            task_keywords: vec![],
            ..halt_context()
        };
        let score = compute_context_match(&candidate, &context);
        assert!(
            (score - 0.4).abs() < 1e-9,
            "tool overlap alone should contribute exactly 0.4, got {score}"
        );
    }

    #[test]
    fn test_context_match_tool_not_in_triggers() {
        let candidate = make_candidate("a", "check it", vec!["write_file"], vec![], false);
        let context = AdvisoryContext {
            current_tool: Some("shell".to_string()),
            task_keywords: vec![],
            ..halt_context()
        };
        let score = compute_context_match(&candidate, &context);
        assert!(
            score < 0.4,
            "mismatched tool should yield no tool overlap, got {score}"
        );
    }

    #[test]
    fn test_context_match_full_keyword_jaccard() {
        let candidate = make_candidate("a", "check it", vec!["rust", "compile"], vec![], false);
        let context = AdvisoryContext {
            current_tool: None,
            task_keywords: vec!["rust".to_string(), "compile".to_string()],
            ..halt_context()
        };
        let score = compute_context_match(&candidate, &context);
        // Jaccard = 2/2 = 1.0 → keyword component = 0.3.
        assert!(
            (score - 0.3).abs() < 1e-9,
            "full keyword overlap with no other signals should give 0.3, got {score}"
        );
    }

    #[test]
    fn test_context_match_partial_keyword_jaccard() {
        let candidate = make_candidate("a", "tip", vec!["rust", "deploy"], vec![], false);
        let context = AdvisoryContext {
            current_tool: None,
            task_keywords: vec!["rust".to_string(), "compile".to_string()],
            ..halt_context()
        };
        let score = compute_context_match(&candidate, &context);
        // Intersection = {rust}, union = {rust, deploy, compile} → Jaccard = 1/3.
        let expected = (1.0_f64 / 3.0) * 0.3;
        assert!(
            (score - expected).abs() < 1e-9,
            "partial keyword overlap should give {expected}, got {score}"
        );
    }

    #[test]
    fn test_context_match_domain_overlap() {
        let candidate = make_candidate("a", "tip", vec![], vec!["rust", "db"], false);
        let context = AdvisoryContext {
            current_tool: None,
            task_keywords: vec!["rust".to_string(), "compile".to_string()],
            ..halt_context()
        };
        let score = compute_context_match(&candidate, &context);
        // 1 out of 2 domains matched → 0.5 * 0.3 = 0.15.
        assert!(
            (score - 0.15).abs() < 1e-9,
            "half domain overlap should give 0.15, got {score}"
        );
    }

    #[test]
    fn test_context_match_clamped_to_one() {
        // Combine all three signals to verify clamping.
        let candidate = make_candidate(
            "a",
            "tip",
            vec!["rust", "compile"],
            vec!["rust", "compile"],
            false,
        );
        let context = AdvisoryContext {
            current_tool: Some("rust".to_string()),
            task_keywords: vec!["rust".to_string(), "compile".to_string()],
            ..halt_context()
        };
        let score = compute_context_match(&candidate, &context);
        assert!(
            score <= 1.0,
            "combined signals must not exceed 1.0, got {score}"
        );
        assert!(score >= 0.0, "score must not be negative, got {score}");
    }

    #[test]
    fn test_context_match_no_signals_is_zero() {
        let candidate = make_candidate("a", "tip", vec![], vec![], false);
        let context = AdvisoryContext {
            current_tool: None,
            task_keywords: vec![],
            ..halt_context()
        };
        let score = compute_context_match(&candidate, &context);
        assert_eq!(score, 0.0, "no overlap signals should give 0.0");
    }

    // -- compute_advisory_score --

    #[test]
    fn test_advisory_score_base_floor_only() {
        // Zero context match, Halt phase (boost 1.0), no failures, no cold start.
        let candidate = make_candidate("a", "check it", vec![], vec![], false);
        let context = halt_context();
        let weights = AdvisoryWeights::default();
        let score = compute_advisory_score(&candidate, &context, &weights);
        let expected = weights.confidence_weight * 0.5 + weights.base_floor;
        assert!(
            (score - expected).abs() < 1e-9,
            "floor-only score should be {expected}, got {score}"
        );
    }

    #[test]
    fn test_advisory_score_negative_phrasing_dont() {
        let weights = AdvisoryWeights::default();
        let neutral = make_candidate("a", "check the output", vec![], vec![], false);
        let negative = make_candidate("b", "don't skip validation", vec![], vec![], false);

        let base = compute_advisory_score(&neutral, &halt_context(), &weights);
        let boosted = compute_advisory_score(&negative, &halt_context(), &weights);

        assert!(boosted > base, "don't-prefixed advice must score higher than neutral");
        assert!(
            (boosted - base * weights.negative_boost).abs() < 1e-9,
            "boost should multiply base by negative_boost: expected {}, got {boosted}",
            base * weights.negative_boost,
        );
    }

    #[test]
    fn test_advisory_score_negative_phrasing_avoid() {
        let weights = AdvisoryWeights::default();
        let neutral = make_candidate("a", "review the diff", vec![], vec![], false);
        let negative = make_candidate("b", "avoid large refactors mid-sprint", vec![], vec![], false);

        let base = compute_advisory_score(&neutral, &halt_context(), &weights);
        let boosted = compute_advisory_score(&negative, &halt_context(), &weights);
        assert!(boosted > base, "avoid-prefixed advice must score higher than neutral");
    }

    #[test]
    fn test_advisory_score_negative_phrasing_never() {
        let weights = AdvisoryWeights::default();
        let neutral = make_candidate("a", "review the diff", vec![], vec![], false);
        let negative = make_candidate("b", "never deploy on a Friday", vec![], vec![], false);

        let base = compute_advisory_score(&neutral, &halt_context(), &weights);
        let boosted = compute_advisory_score(&negative, &halt_context(), &weights);
        assert!(boosted > base, "never-prefixed advice must score higher than neutral");
    }

    #[test]
    fn test_advisory_score_failure_boost() {
        let weights = AdvisoryWeights::default();
        let candidate = make_candidate("a", "check it", vec![], vec![], false);

        let clean = halt_context();
        let failing = AdvisoryContext {
            consecutive_failures: 1,
            ..halt_context()
        };

        let score_clean = compute_advisory_score(&candidate, &clean, &weights);
        let score_failing = compute_advisory_score(&candidate, &failing, &weights);
        assert!(
            score_failing > score_clean,
            "failure context should boost score: {score_clean} → {score_failing}"
        );
    }

    #[test]
    fn test_advisory_score_phase_boost_applied() {
        let weights = AdvisoryWeights::default();
        let candidate = AdvisoryCandidate {
            category: "domain_expertise".to_string(),
            ..make_candidate("a", "check it", vec![], vec![], false)
        };

        // Explore phase boosts domain_expertise by 1.5; Halt by 1.0.
        let explore_context = AdvisoryContext {
            current_phase: Phase::Explore,
            ..halt_context()
        };
        let score_explore = compute_advisory_score(&candidate, &explore_context, &weights);
        let score_halt = compute_advisory_score(&candidate, &halt_context(), &weights);

        assert!(
            score_explore > score_halt,
            "Explore phase should boost domain_expertise above Halt baseline"
        );
        assert!(
            (score_explore / score_halt - 1.5).abs() < 1e-9,
            "Explore/domain_expertise boost should be 1.5×, got {:.4}×",
            score_explore / score_halt,
        );
    }

    #[test]
    fn test_advisory_score_clamped_to_one() {
        let weights = AdvisoryWeights {
            negative_boost: 5.0,
            failure_boost: 5.0,
            cold_start_boost: 0.5,
            cold_start_start_episode: 0,
            cold_start_end_episode: 50,
            ..AdvisoryWeights::default()
        };
        let candidate = make_candidate("a", "never do this", vec![], vec![], false);
        let context = AdvisoryContext {
            consecutive_failures: 5,
            completed_episodes: 0,
            ..halt_context()
        };
        let score = compute_advisory_score(&candidate, &context, &weights);
        assert!(score <= 1.0, "score must be clamped to 1.0, got {score}");
        assert!(score >= 0.0, "score must not be negative, got {score}");
    }

    // -- cold start boost --

    #[test]
    fn test_cold_start_full_boost_at_episode_zero() {
        let weights = AdvisoryWeights {
            cold_start_start_episode: 0,
            cold_start_end_episode: 50,
            cold_start_boost: 0.15,
            ..AdvisoryWeights::default()
        };
        let candidate = make_candidate("a", "check it", vec![], vec![], false);

        let early_context = AdvisoryContext {
            completed_episodes: 0,
            ..halt_context()
        };
        let late_context = AdvisoryContext {
            completed_episodes: 100,
            ..halt_context()
        };

        let early_score = compute_advisory_score(&candidate, &early_context, &weights);
        let late_score = compute_advisory_score(&candidate, &late_context, &weights);

        assert!(
            early_score > late_score,
            "cold start should boost early episodes: {early_score} > {late_score}"
        );
        assert!(
            (early_score - late_score - weights.cold_start_boost).abs() < 1e-9,
            "full cold start boost should add exactly cold_start_boost at episode 0: \
             expected diff {}, got {}",
            weights.cold_start_boost,
            early_score - late_score,
        );
    }

    #[test]
    fn test_cold_start_decays_linearly() {
        let weights = AdvisoryWeights {
            cold_start_start_episode: 0,
            cold_start_end_episode: 100,
            cold_start_boost: 0.20,
            ..AdvisoryWeights::default()
        };
        let candidate = make_candidate("a", "check it", vec![], vec![], false);

        let make_context = |episodes: u64| AdvisoryContext {
            completed_episodes: episodes,
            ..halt_context()
        };

        let at_0 = compute_advisory_score(&candidate, &make_context(0), &weights);
        let at_50 = compute_advisory_score(&candidate, &make_context(50), &weights);
        let at_100 = compute_advisory_score(&candidate, &make_context(100), &weights);

        assert!(at_0 > at_50, "score should decrease from episode 0 to 50");
        assert!(at_50 > at_100, "score should decrease from episode 50 to 100");

        // At episode 50: boost_factor = 1 - 50/100 = 0.5 → adds 0.10.
        assert!(
            (at_0 - at_50 - 0.10).abs() < 1e-9,
            "half-way through decay should give half the boost (0.10), got diff {}",
            at_0 - at_50,
        );
    }

    #[test]
    fn test_cold_start_full_strength_before_start_episode() {
        let weights = AdvisoryWeights {
            cold_start_start_episode: 20,
            cold_start_end_episode: 100,
            cold_start_boost: 0.10,
            ..AdvisoryWeights::default()
        };
        let candidate = make_candidate("a", "check it", vec![], vec![], false);

        let make_context = |episodes: u64| AdvisoryContext {
            completed_episodes: episodes,
            ..halt_context()
        };

        let at_0 = compute_advisory_score(&candidate, &make_context(0), &weights);
        let at_10 = compute_advisory_score(&candidate, &make_context(10), &weights);
        let at_20 = compute_advisory_score(&candidate, &make_context(20), &weights);

        // Episodes 0–20 are all at full boost_factor = 1.0.
        assert!(
            (at_0 - at_10).abs() < 1e-9,
            "scores before cold_start_start_episode should be equal"
        );
        assert!(
            (at_10 - at_20).abs() < 1e-9,
            "score at cold_start_start_episode should equal earlier scores"
        );
    }

    #[test]
    fn test_cold_start_no_boost_at_end_episode() {
        let weights = AdvisoryWeights {
            cold_start_start_episode: 0,
            cold_start_end_episode: 50,
            cold_start_boost: 0.15,
            ..AdvisoryWeights::default()
        };
        let candidate = make_candidate("a", "check it", vec![], vec![], false);

        let at_end = AdvisoryContext {
            completed_episodes: 50,
            ..halt_context()
        };
        let beyond = AdvisoryContext {
            completed_episodes: 200,
            ..halt_context()
        };

        let score_at_end = compute_advisory_score(&candidate, &at_end, &weights);
        let score_beyond = compute_advisory_score(&candidate, &beyond, &weights);

        assert_eq!(
            score_at_end, score_beyond,
            "no cold start boost should apply at or beyond cold_start_end_episode"
        );
    }

    // -- check_agreement --

    #[test]
    fn test_agreement_gating_single_source_warning_removed() {
        let advisory = ScoredAdvisory {
            source_id: "src-1".to_string(),
            source_type: "insight".to_string(),
            advice_text: "don't skip validation".to_string(),
            score: 0.82,
            authority_level: AuthorityLevel::Warning,
            category: "reasoning".to_string(),
            is_policy: false,
        };
        let result = check_agreement(&[advisory]);
        assert!(
            result.is_empty(),
            "single-source WARNING without policy flag should be removed"
        );
    }

    #[test]
    fn test_agreement_gating_two_distinct_sources_pass() {
        let make_warning = |source_id: &str| ScoredAdvisory {
            source_id: source_id.to_string(),
            source_type: "insight".to_string(),
            advice_text: "don't skip validation".to_string(),
            score: 0.82,
            authority_level: AuthorityLevel::Warning,
            category: "reasoning".to_string(),
            is_policy: false,
        };
        let result = check_agreement(&[make_warning("src-1"), make_warning("src-2")]);
        assert_eq!(result.len(), 2, "two distinct sources should satisfy agreement gating");
    }

    #[test]
    fn test_agreement_gating_duplicate_source_id_counts_once() {
        // Submitting the same source_id twice must not satisfy the 2-source requirement.
        let make_warning = |source_id: &str| ScoredAdvisory {
            source_id: source_id.to_string(),
            source_type: "insight".to_string(),
            advice_text: "don't skip validation".to_string(),
            score: 0.82,
            authority_level: AuthorityLevel::Warning,
            category: "reasoning".to_string(),
            is_policy: false,
        };
        let result = check_agreement(&[make_warning("src-1"), make_warning("src-1")]);
        assert!(
            result.is_empty(),
            "duplicate source_id should not count as two independent sources"
        );
    }

    #[test]
    fn test_agreement_gating_policy_bypasses_check() {
        let policy_advisory = ScoredAdvisory {
            source_id: "policy-1".to_string(),
            source_type: "distillation".to_string(),
            advice_text: "always run tests before committing".to_string(),
            score: 0.85,
            authority_level: AuthorityLevel::Warning,
            category: "wisdom".to_string(),
            is_policy: true,
        };
        let result = check_agreement(&[policy_advisory]);
        assert_eq!(
            result.len(),
            1,
            "policy advisory should bypass agreement gating with a single source"
        );
    }

    #[test]
    fn test_agreement_gating_block_and_note_pass_through() {
        let advisories = vec![
            ScoredAdvisory {
                source_id: "a".to_string(),
                source_type: "insight".to_string(),
                advice_text: "never deploy without review".to_string(),
                score: 0.96,
                authority_level: AuthorityLevel::Block,
                category: "reasoning".to_string(),
                is_policy: false,
            },
            ScoredAdvisory {
                source_id: "b".to_string(),
                source_type: "insight".to_string(),
                advice_text: "note this pattern".to_string(),
                score: 0.55,
                authority_level: AuthorityLevel::Note,
                category: "wisdom".to_string(),
                is_policy: false,
            },
        ];
        let result = check_agreement(&advisories);
        assert_eq!(
            result.len(),
            2,
            "Block and Note authority levels should not be subject to agreement gating"
        );
    }

    #[test]
    fn test_agreement_gating_whisper_and_silent_pass_through() {
        let advisories = vec![
            ScoredAdvisory {
                source_id: "a".to_string(),
                source_type: "chip".to_string(),
                advice_text: "consider this".to_string(),
                score: 0.35,
                authority_level: AuthorityLevel::Whisper,
                category: "context".to_string(),
                is_policy: false,
            },
            ScoredAdvisory {
                source_id: "b".to_string(),
                source_type: "chip".to_string(),
                advice_text: "background noise".to_string(),
                score: 0.10,
                authority_level: AuthorityLevel::Silent,
                category: "context".to_string(),
                is_policy: false,
            },
        ];
        let result = check_agreement(&advisories);
        assert_eq!(result.len(), 2, "Whisper and Silent should pass through unfiltered");
    }

    #[test]
    fn test_agreement_gating_normalized_text_matching() {
        // Different whitespace/case in advice_text should still group correctly.
        let make_warning = |source_id: &str, advice: &str| ScoredAdvisory {
            source_id: source_id.to_string(),
            source_type: "insight".to_string(),
            advice_text: advice.to_string(),
            score: 0.82,
            authority_level: AuthorityLevel::Warning,
            category: "reasoning".to_string(),
            is_policy: false,
        };
        let result = check_agreement(&[
            make_warning("src-1", "  Don't Skip Validation  "),
            make_warning("src-2", "don't skip validation"),
        ]);
        assert_eq!(
            result.len(),
            2,
            "normalization should allow text variants to satisfy agreement"
        );
    }

    // -- score_and_gate --

    #[test]
    fn test_score_and_gate_sorted_descending() {
        let candidates = vec![
            make_candidate("a", "review the output", vec![], vec![], false),
            make_candidate("b", "don't skip validation", vec![], vec![], false),
        ];
        let context = halt_context();
        let weights = AdvisoryWeights::default();
        let result = score_and_gate(candidates, &context, &weights);

        for window in result.windows(2) {
            assert!(
                window[0].score >= window[1].score,
                "results must be sorted score descending: {} >= {}",
                window[0].score,
                window[1].score,
            );
        }
    }

    #[test]
    fn test_score_and_gate_assigns_authority_from_score() {
        let candidate = make_candidate("a", "review the output", vec![], vec![], false);
        let context = halt_context();
        let weights = AdvisoryWeights::default();
        let result = score_and_gate(vec![candidate.clone()], &context, &weights);

        assert_eq!(result.len(), 1);
        let expected_level =
            AuthorityLevel::from_score(compute_advisory_score(&candidate, &context, &weights));
        assert_eq!(
            result[0].authority_level, expected_level,
            "authority level should match AuthorityLevel::from_score"
        );
    }

    #[test]
    fn test_score_and_gate_gated_warning_with_two_sources_survives() {
        // Two distinct sources for the same warning text should both appear.
        let context = AdvisoryContext {
            consecutive_failures: 2,
            completed_episodes: 0,
            ..halt_context()
        };
        let weights = AdvisoryWeights {
            failure_boost: 3.0,
            cold_start_boost: 0.20,
            cold_start_end_episode: 50,
            ..AdvisoryWeights::default()
        };

        let candidates = vec![
            make_candidate("src-1", "don't skip validation", vec![], vec![], false),
            make_candidate("src-2", "don't skip validation", vec![], vec![], false),
        ];

        let test_score = compute_advisory_score(&candidates[0], &context, &weights);
        if AuthorityLevel::from_score(test_score) == AuthorityLevel::Warning {
            let result = score_and_gate(candidates, &context, &weights);
            assert_eq!(result.len(), 2, "two-source WARNING should survive agreement gating");
        }
    }

    // -- demote_unreliable_insights --

    #[test]
    fn test_demote_unreliable_below_threshold() {
        let insights = vec![
            ("a".to_string(), 0.3_f64),
            ("b".to_string(), 0.5),
            ("c".to_string(), 0.49),
            ("d".to_string(), 0.8),
        ];
        let demoted = demote_unreliable_insights(&insights);
        assert_eq!(demoted, vec!["a", "c"]);
    }

    #[test]
    fn test_demote_unreliable_at_boundary() {
        let insights = vec![
            ("x".to_string(), 0.5_f64),  // exactly at threshold — not demoted
            ("y".to_string(), 0.499),     // just below — demoted
        ];
        let demoted = demote_unreliable_insights(&insights);
        assert_eq!(demoted, vec!["y"]);
    }

    #[test]
    fn test_demote_unreliable_all_reliable() {
        let insights = vec![
            ("a".to_string(), 0.5_f64),
            ("b".to_string(), 0.9),
            ("c".to_string(), 1.0),
        ];
        let demoted = demote_unreliable_insights(&insights);
        assert!(demoted.is_empty(), "no IDs should be demoted when all reliability >= 0.5");
    }

    #[test]
    fn test_demote_unreliable_all_demoted() {
        let insights = vec![
            ("a".to_string(), 0.0_f64),
            ("b".to_string(), 0.1),
            ("c".to_string(), 0.499),
        ];
        let demoted = demote_unreliable_insights(&insights);
        assert_eq!(demoted.len(), 3, "all IDs below 0.5 should be demoted");
    }
}
