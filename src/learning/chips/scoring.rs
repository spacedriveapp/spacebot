//! Six-dimensional insight scoring for domain chips.
//!
//! Scores each chip insight on cognitive value, outcome linkage, uniqueness,
//! actionability, transferability, and domain relevance. The weighted total
//! drives promotion tier assignment (long-term, working, session, or discard).

use crate::learning::PromotionTier;

use regex::Regex;
use serde::{Deserialize, Serialize};

use std::collections::HashSet;
use std::sync::LazyLock;

// ---------------------------------------------------------------------------
// Dimension weights — must sum to 1.0
// ---------------------------------------------------------------------------

/// Weight for the cognitive value dimension.
const WEIGHT_COGNITIVE_VALUE: f64 = 0.30;

/// Weight for the outcome linkage dimension.
const WEIGHT_OUTCOME_LINKAGE: f64 = 0.20;

/// Weight for the uniqueness dimension.
const WEIGHT_UNIQUENESS: f64 = 0.15;

/// Weight for the actionability dimension.
const WEIGHT_ACTIONABILITY: f64 = 0.15;

/// Weight for the transferability dimension.
const WEIGHT_TRANSFERABILITY: f64 = 0.10;

/// Weight for the domain relevance dimension.
const WEIGHT_DOMAIN_RELEVANCE: f64 = 0.10;

// ---------------------------------------------------------------------------
// Cognitive value keyword modifiers
// ---------------------------------------------------------------------------

/// Words that signal above-average insight quality. Each match adds 0.05.
const COGNITIVE_BOOST_KEYWORDS: &[&str] = &[
    "crucial",
    "critical",
    "fundamental",
    "key",
    "important",
    "insight",
    "breakthrough",
    "surprising",
    "unexpected",
    "significant",
];

/// Words that signal low cognitive density. Each match subtracts 0.05.
const COGNITIVE_REDUCE_KEYWORDS: &[&str] = &[
    "simple",
    "obvious",
    "trivial",
    "basic",
    "straightforward",
    "just",
    "merely",
    "only",
];

// ---------------------------------------------------------------------------
// Transferability reasoning markers
// ---------------------------------------------------------------------------

/// Causal and conditional connectives that indicate portable reasoning.
const REASONING_MARKERS: &[&str] = &["because", "when", "if", "then", "therefore"];

// ---------------------------------------------------------------------------
// Primitive patterns — operational / low-value content
// ---------------------------------------------------------------------------

/// Regex patterns that identify operational content with low cognitive value.
///
/// Content matching any of these patterns is likely a status confirmation,
/// error dump, or tool invocation record rather than a durable insight.
pub(crate) static PRIMITIVE_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // Shell command prompts: "$ cargo build", "$ ls -la"
        Regex::new(r"(?im)^\s*\$\s+\S+").expect("hardcoded regex"),
        // File I/O confirmations: "read file", "wrote file", "created path"
        Regex::new(r"(?i)\b(read|wrote|created|deleted|updated)\s+(file|directory|path|dir)\b")
            .expect("hardcoded regex"),
        // Error / exception / traceback markers.
        // No trailing \b because these tokens end in ":" or a space, both of
        // which are non-word characters, so a word boundary assertion fails.
        Regex::new(r"(?i)\b(error:|exception:|traceback:|stack\s+trace:|panicked\s+at)")
            .expect("hardcoded regex"),
        // Build / compile status lines: "compiling 5 crates", "linking debug"
        Regex::new(r"(?i)\b(compiling|linking|bundling|building)\s+\d+").expect("hardcoded regex"),
        // HTTP method log lines: "GET /api/users", "POST https://..."
        Regex::new(r"(?i)\b(GET|POST|PUT|DELETE|PATCH)\s+https?://").expect("hardcoded regex"),
        // Package installation: "installing tokio v1.38", "downloaded serde 1.0"
        Regex::new(r"(?i)\b(installing|downloaded|fetching)\s+\S+\s+v?[\d.]+")
            .expect("hardcoded regex"),
        // Bare "done / completed / finished" status messages
        Regex::new(r"(?i)\b(done|completed|finished|succeeded)\s*\.?\s*$").expect("hardcoded regex"),
        // Tool execution confirmations: "ran cargo", "executed migration"
        Regex::new(r"(?i)^(ran|executed|called|invoked)\s+\S+").expect("hardcoded regex"),
    ]
});

// ---------------------------------------------------------------------------
// Valuable patterns — cognitive / high-value content
// ---------------------------------------------------------------------------

/// Regex patterns that identify cognitive content with high insight value.
///
/// Content matching these patterns tends to contain durable reasoning,
/// generalizations, or causal explanations worth promoting.
pub(crate) static VALUABLE_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // Realization / discovery: "realized that", "discovered that", "learned that"
        Regex::new(r"(?i)\b(realized|discovered|found\s+that|learned\s+that)\b")
            .expect("hardcoded regex"),
        // Causal explanation: "because X", "therefore Y", "which means", "this means"
        Regex::new(r"(?i)\b(because|therefore|which\s+means|this\s+means)\b")
            .expect("hardcoded regex"),
        // Pattern / principle identification
        Regex::new(r"(?i)\b(pattern|principle|rule\s+of\s+thumb|heuristic)\b")
            .expect("hardcoded regex"),
        // Root cause and problem framing
        Regex::new(r"(?i)\b(root\s+cause|key\s+issue|the\s+fix\s+is|the\s+problem\s+was)\b")
            .expect("hardcoded regex"),
        // Preventive / future-oriented insights
        Regex::new(r"(?i)\b(to\s+avoid|to\s+prevent|in\s+the\s+future|next\s+time)\b")
            .expect("hardcoded regex"),
        // Comparative and tradeoff reasoning
        Regex::new(r"(?i)\b(better\s+than|more\s+efficient|trade.?off|versus)\b")
            .expect("hardcoded regex"),
        // Generalizations used as principles: "always prefer", "never store"
        Regex::new(r"(?i)\b(always|never)\s+\w").expect("hardcoded regex"),
        // Mental models: abstractions, invariants, assumptions
        Regex::new(r"(?i)\b(abstraction|invariant|assumption|mental\s+model)\b")
            .expect("hardcoded regex"),
        // Cross-domain applicability
        Regex::new(r"(?i)\b(applies\s+to|generalizes|can\s+be\s+used|transferable)\b")
            .expect("hardcoded regex"),
        // Explicit insight framing
        Regex::new(r"(?i)\b(key\s+insight|important\s+to\s+note|worth\s+remembering|crucial\s+observation)\b")
            .expect("hardcoded regex"),
    ]
});

// ---------------------------------------------------------------------------
// Action verbs used for actionability scoring
// ---------------------------------------------------------------------------

/// Verb set used to measure how densely a piece of content directs behavior.
///
/// Ratio of action-verb tokens to total tokens approximates how much of the
/// insight is an instruction versus a description.
static ACTION_VERBS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "use",
        "apply",
        "check",
        "run",
        "implement",
        "create",
        "build",
        "test",
        "avoid",
        "ensure",
        "verify",
        "configure",
        "set",
        "update",
        "add",
        "remove",
        "deploy",
        "refactor",
        "optimize",
        "debug",
        "monitor",
        "validate",
        "review",
        "document",
        "automate",
        "integrate",
        "enable",
        "disable",
        "install",
        "migrate",
        "handle",
        "catch",
        "retry",
        "cache",
        "batch",
        "throttle",
        "clean",
        "fix",
        "improve",
        "extend",
        "wrap",
        "inject",
        "export",
        "import",
        "register",
        "subscribe",
        "publish",
        "prefer",
        "replace",
        "initialize",
    ]
    .into_iter()
    .collect()
});

// ---------------------------------------------------------------------------
// InsightScores
// ---------------------------------------------------------------------------

/// Six-dimensional quality scores for a single chip insight.
///
/// Each dimension is a value in [0.0, 1.0]. Call [`total`] to get the
/// weighted sum, which drives [`determine_promotion_tier`].
///
/// [`total`]: InsightScores::total
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsightScores {
    /// Richness of the concept relative to operational noise. Weight: 0.30.
    pub cognitive_value: f64,
    /// Correlation to a known episode outcome. Weight: 0.20.
    pub outcome_linkage: f64,
    /// Novelty relative to recent chip insights measured by word-set overlap. Weight: 0.15.
    pub uniqueness: f64,
    /// Density of actionable directives (action-verb ratio). Weight: 0.15.
    pub actionability: f64,
    /// Presence of reasoning structure that travels across domains. Weight: 0.10.
    pub transferability: f64,
    /// Relevance to the chip's configured domain. Weight: 0.10.
    pub domain_relevance: f64,
}

impl InsightScores {
    /// Weighted sum of all six dimensions.
    ///
    /// Result is in [0.0, 1.0] because dimension weights sum to 1.0 and every
    /// dimension is clamped to [0.0, 1.0] at scoring time.
    pub fn total(&self) -> f64 {
        self.cognitive_value * WEIGHT_COGNITIVE_VALUE
            + self.outcome_linkage * WEIGHT_OUTCOME_LINKAGE
            + self.uniqueness * WEIGHT_UNIQUENESS
            + self.actionability * WEIGHT_ACTIONABILITY
            + self.transferability * WEIGHT_TRANSFERABILITY
            + self.domain_relevance * WEIGHT_DOMAIN_RELEVANCE
    }
}

// ---------------------------------------------------------------------------
// score_insight
// ---------------------------------------------------------------------------

/// Score a single insight candidate across all six dimensions.
///
/// - `content` — the insight text to score.
/// - `has_outcome` — whether the enclosing episode has a known outcome.
/// - `recent_insights` — the chip's most recent promoted insights, used for
///   Jaccard uniqueness comparison.
pub(crate) fn score_insight(
    content: &str,
    has_outcome: bool,
    recent_insights: &[&str],
) -> InsightScores {
    InsightScores {
        cognitive_value: score_cognitive_value(content),
        outcome_linkage: score_outcome_linkage(has_outcome),
        uniqueness: score_uniqueness(content, recent_insights),
        actionability: score_actionability(content),
        transferability: score_transferability(content),
        domain_relevance: 0.7,
    }
}

// ---------------------------------------------------------------------------
// Dimension implementations
// ---------------------------------------------------------------------------

/// Score cognitive value of content by pattern matching and keyword modifiers.
fn score_cognitive_value(content: &str) -> f64 {
    // Primitive patterns signal operational noise — score low regardless of
    // whether valuable patterns also match (error messages can contain causal
    // language but still aren't insights).
    let base = if PRIMITIVE_PATTERNS.iter().any(|pattern| pattern.is_match(content)) {
        0.1
    } else if VALUABLE_PATTERNS.iter().any(|pattern| pattern.is_match(content)) {
        0.8
    } else {
        0.5
    };

    let lowercased = content.to_lowercase();

    let boost: f64 = COGNITIVE_BOOST_KEYWORDS
        .iter()
        .filter(|&&keyword| lowercased.contains(keyword))
        .count() as f64
        * 0.05;

    let reduction: f64 = COGNITIVE_REDUCE_KEYWORDS
        .iter()
        .filter(|&&keyword| lowercased.contains(keyword))
        .count() as f64
        * 0.05;

    (base + boost - reduction).clamp(0.0, 1.0)
}

/// Score outcome linkage: higher when the enclosing episode has a known result.
fn score_outcome_linkage(has_outcome: bool) -> f64 {
    if has_outcome {
        0.7
    } else {
        0.3
    }
}

/// Score uniqueness via Jaccard word-set overlap against recent insights.
///
/// Normalizing to lowercase before tokenization prevents "Use" and "use" from
/// counting as distinct tokens.
fn score_uniqueness(content: &str, recent_insights: &[&str]) -> f64 {
    if recent_insights.is_empty() {
        return 1.0;
    }

    let content_words = word_set(content);

    let best_overlap = recent_insights
        .iter()
        .map(|insight| jaccard_similarity(&content_words, &word_set(insight)))
        .fold(0.0_f64, f64::max);

    // Near-duplicate content (>0.8 overlap) receives a floor score rather than
    // zero so that a slightly rephrased insight isn't completely discarded.
    if best_overlap > 0.8 {
        0.1
    } else {
        1.0 - best_overlap
    }
}

/// Score actionability as the ratio of action-verb tokens to total tokens.
fn score_actionability(content: &str) -> f64 {
    let words: Vec<&str> = content.split_whitespace().collect();
    let total = words.len();

    if total == 0 {
        return 0.0;
    }

    let action_count = words
        .iter()
        .filter(|&&word| ACTION_VERBS.contains(word.to_lowercase().trim_matches(|char: char| !char.is_alphabetic())))
        .count();

    (action_count as f64 / total as f64).clamp(0.0, 1.0)
}

/// Score transferability by counting distinct causal/conditional reasoning markers.
///
/// Each unique marker found adds 0.1 to a 0.3 base, capped at 0.9. Deduplication
/// prevents a single repeated marker from inflating the score.
fn score_transferability(content: &str) -> f64 {
    let lowercased = content.to_lowercase();

    let marker_count = REASONING_MARKERS
        .iter()
        .filter(|&&marker| lowercased.contains(marker))
        .count();

    (0.3 + marker_count as f64 * 0.1).min(0.9)
}

// ---------------------------------------------------------------------------
// determine_promotion_tier
// ---------------------------------------------------------------------------

/// Map insight scores to a promotion tier using the weighted total.
pub(crate) fn determine_promotion_tier(scores: &InsightScores) -> PromotionTier {
    PromotionTier::from_score(scores.total())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Tokenize a string into a lowercase word set, stripping non-alphabetic
/// boundary characters so punctuation doesn't fragment tokens.
fn word_set(text: &str) -> HashSet<String> {
    text.split_whitespace()
        .map(|word| word.to_lowercase().trim_matches(|char: char| !char.is_alphabetic()).to_owned())
        .filter(|word| !word.is_empty())
        .collect()
}

/// Jaccard similarity between two word sets: |A ∩ B| / |A ∪ B|.
fn jaccard_similarity(set_a: &HashSet<String>, set_b: &HashSet<String>) -> f64 {
    let intersection = set_a.intersection(set_b).count();
    let union = set_a.union(set_b).count();

    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- InsightScores::total ------------------------------------------------

    #[test]
    fn total_uses_correct_weights() {
        let scores = InsightScores {
            cognitive_value: 1.0,
            outcome_linkage: 1.0,
            uniqueness: 1.0,
            actionability: 1.0,
            transferability: 1.0,
            domain_relevance: 1.0,
        };
        // All ones must give exactly 1.0 (weights sum to 1.0).
        assert!((scores.total() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn total_zero_when_all_dimensions_are_zero() {
        let scores = InsightScores {
            cognitive_value: 0.0,
            outcome_linkage: 0.0,
            uniqueness: 0.0,
            actionability: 0.0,
            transferability: 0.0,
            domain_relevance: 0.0,
        };
        assert_eq!(scores.total(), 0.0);
    }

    #[test]
    fn total_reflects_per_dimension_weight() {
        // Only cognitive_value is non-zero; its weight is 0.30.
        let scores = InsightScores {
            cognitive_value: 1.0,
            outcome_linkage: 0.0,
            uniqueness: 0.0,
            actionability: 0.0,
            transferability: 0.0,
            domain_relevance: 0.0,
        };
        assert!((scores.total() - 0.30).abs() < 1e-10);
    }

    // -- cognitive_value scoring ---------------------------------------------

    #[test]
    fn primitive_pattern_gives_low_cognitive_value() {
        // Shell prompt — primitive signal.
        let score = score_cognitive_value("$ cargo build --release");
        assert!(score <= 0.2, "expected low score for shell command, got {score}");
    }

    #[test]
    fn valuable_pattern_gives_high_cognitive_value() {
        let score = score_cognitive_value("I realized that locking before branching prevents the race.");
        assert!(score >= 0.7, "expected high score for insight language, got {score}");
    }

    #[test]
    fn neutral_content_gives_midrange_cognitive_value() {
        let score = score_cognitive_value("The cache is populated on first access.");
        assert!((0.3..=0.7).contains(&score), "expected midrange score, got {score}");
    }

    #[test]
    fn boost_keywords_raise_cognitive_value() {
        let baseline = score_cognitive_value("The cache stores results.");
        let boosted = score_cognitive_value("The cache stores results. This is crucial and significant.");
        assert!(boosted > baseline, "boost keywords should raise the score");
    }

    #[test]
    fn reduce_keywords_lower_cognitive_value() {
        let baseline = score_cognitive_value("The cache stores results.");
        let reduced = score_cognitive_value("The cache stores results. It is simple and obvious.");
        assert!(reduced < baseline, "reduce keywords should lower the score");
    }

    #[test]
    fn cognitive_value_clamped_to_unit_interval() {
        // Pile on every boost keyword — must not exceed 1.0.
        let content = "crucial critical fundamental key important insight breakthrough \
                       surprising unexpected significant";
        let score = score_cognitive_value(content);
        assert!(score <= 1.0, "cognitive value must not exceed 1.0, got {score}");

        // Pile on every reduce keyword — must not go below 0.0.
        let content = "simple obvious trivial basic straightforward just merely only";
        let score = score_cognitive_value(content);
        assert!(score >= 0.0, "cognitive value must not be negative, got {score}");
    }

    // -- outcome_linkage -----------------------------------------------------

    #[test]
    fn outcome_linkage_high_when_outcome_known() {
        assert_eq!(score_outcome_linkage(true), 0.7);
    }

    #[test]
    fn outcome_linkage_low_when_outcome_unknown() {
        assert_eq!(score_outcome_linkage(false), 0.3);
    }

    // -- uniqueness (Jaccard) ------------------------------------------------

    #[test]
    fn uniqueness_is_full_when_no_recent_insights() {
        let score = score_uniqueness("completely novel content", &[]);
        assert_eq!(score, 1.0);
    }

    #[test]
    fn uniqueness_is_low_for_near_duplicate() {
        let content = "always use connection pooling to reduce database overhead";
        let recent = ["always use connection pooling to reduce database overhead"];
        let score = score_uniqueness(content, &recent);
        // Identical content → Jaccard = 1.0 → should return the floor 0.1.
        assert_eq!(score, 0.1, "near-duplicate should get floor score, got {score}");
    }

    #[test]
    fn uniqueness_is_high_for_unrelated_content() {
        let content = "apply backpressure when the downstream queue is full";
        let recent = ["rotate encryption keys every ninety days"];
        let score = score_uniqueness(content, &recent);
        assert!(score > 0.8, "unrelated content should score high uniqueness, got {score}");
    }

    #[test]
    fn uniqueness_is_partial_for_overlapping_content() {
        let content = "use connection pooling for database performance";
        let recent = ["use connection pooling to manage database connections"];
        let score = score_uniqueness(content, &recent);
        // Some overlap but not near-duplicate.
        assert!((0.1..1.0).contains(&score), "partial overlap should give partial score, got {score}");
    }

    // -- actionability -------------------------------------------------------

    #[test]
    fn actionability_zero_for_empty_content() {
        assert_eq!(score_actionability(""), 0.0);
    }

    #[test]
    fn actionability_higher_for_directive_content() {
        let directive = "use a connection pool, configure the timeout, and validate the response";
        let descriptive = "the connection pool is initialized during startup";
        assert!(
            score_actionability(directive) > score_actionability(descriptive),
            "directive content should outscore descriptive content on actionability"
        );
    }

    #[test]
    fn actionability_clamped_to_unit_interval() {
        let content = "use apply check run implement create build test avoid ensure";
        let score = score_actionability(content);
        assert!((0.0..=1.0).contains(&score));
    }

    // -- transferability -----------------------------------------------------

    #[test]
    fn transferability_at_base_without_markers() {
        // "the cache stores results on first access" contains no reasoning marker
        // as a substring ("first" is f-i-r-s-t, not "if"; "the" is not "then").
        let score = score_transferability("the cache stores results on first access");
        assert!((score - 0.3).abs() < 1e-10, "no markers should give base 0.3, got {score}");
    }

    #[test]
    fn transferability_increases_per_unique_marker() {
        let score_one = score_transferability("retry because the network is unreliable");
        let score_two = score_transferability("if the request fails then retry because the network is unreliable");
        assert!(score_two > score_one, "more markers should give higher transferability");
    }

    #[test]
    fn transferability_caps_at_0_9() {
        // All five reasoning markers present: 0.3 + 5 × 0.1 = 0.8. The `.min(0.9)`
        // guard is a safety rail for future marker additions; with the current set
        // of five markers the ceiling is 0.8, safely below the cap.
        let content = "because when if then therefore this applies universally";
        let score = score_transferability(content);
        assert!(
            score <= 0.9,
            "transferability must not exceed the 0.9 cap, got {score}"
        );
        assert!(
            (score - 0.8).abs() < 1e-10,
            "all five markers should produce 0.8, got {score}"
        );
    }

    // -- domain_relevance ----------------------------------------------------

    #[test]
    fn domain_relevance_always_0_7() {
        let scores = score_insight("any content", false, &[]);
        assert_eq!(scores.domain_relevance, 0.7);
    }

    // -- score_insight (integration) -----------------------------------------

    #[test]
    fn score_insight_returns_all_dimensions() {
        let scores = score_insight(
            "I realized that retrying with exponential backoff prevents thundering herd because \
             all clients desynchronize after a failure.",
            true,
            &["use circuit breakers to protect downstream services"],
        );
        // Total should be well above the session tier floor (0.30).
        assert!(
            scores.total() > 0.30,
            "high-quality insight should exceed session floor, total = {}",
            scores.total()
        );
        assert_eq!(scores.outcome_linkage, 0.7);
        assert_eq!(scores.domain_relevance, 0.7);
    }

    #[test]
    fn score_insight_low_for_operational_noise() {
        let scores = score_insight(
            "$ cargo test --release\ndone",
            false,
            &[],
        );
        // Primitive content with no outcome should score low overall.
        assert!(
            scores.total() < 0.55,
            "operational noise should score below working tier, total = {}",
            scores.total()
        );
    }

    // -- determine_promotion_tier --------------------------------------------

    #[test]
    fn determines_long_term_tier_for_high_scores() {
        let scores = InsightScores {
            cognitive_value: 1.0,
            outcome_linkage: 1.0,
            uniqueness: 1.0,
            actionability: 1.0,
            transferability: 1.0,
            domain_relevance: 1.0,
        };
        assert_eq!(determine_promotion_tier(&scores), PromotionTier::LongTerm);
    }

    #[test]
    fn determines_discard_tier_for_low_scores() {
        let scores = InsightScores {
            cognitive_value: 0.0,
            outcome_linkage: 0.0,
            uniqueness: 0.0,
            actionability: 0.0,
            transferability: 0.0,
            domain_relevance: 0.0,
        };
        assert_eq!(determine_promotion_tier(&scores), PromotionTier::Discard);
    }

    #[test]
    fn determines_working_tier_for_midrange_scores() {
        // Construct scores whose total lands between 0.50 and 0.75.
        let scores = InsightScores {
            cognitive_value: 0.6,
            outcome_linkage: 0.6,
            uniqueness: 0.6,
            actionability: 0.6,
            transferability: 0.6,
            domain_relevance: 0.6,
        };
        assert_eq!(determine_promotion_tier(&scores), PromotionTier::Working);
    }

    #[test]
    fn determines_session_tier_for_borderline_scores() {
        // Target a total of ~0.40 — above discard (0.30) but below working (0.50).
        let scores = InsightScores {
            cognitive_value: 0.4,
            outcome_linkage: 0.4,
            uniqueness: 0.4,
            actionability: 0.4,
            transferability: 0.4,
            domain_relevance: 0.4,
        };
        let total = scores.total();
        assert!(
            (0.30..0.50).contains(&total),
            "expected session-tier total, got {total}"
        );
        assert_eq!(determine_promotion_tier(&scores), PromotionTier::Session);
    }

    // -- patterns sanity -----------------------------------------------------

    #[test]
    fn all_primitive_patterns_compile_and_match() {
        let samples = [
            "$ ls -la /tmp",
            "read file /etc/config.toml",
            "error: failed to open socket",
            "compiling 42 crates",
            "GET https://api.example.com/v1/users",
            "installing tokio v1.38.0",
            "done.",
            "ran cargo test",
        ];
        for (index, sample) in samples.iter().enumerate() {
            assert!(
                PRIMITIVE_PATTERNS[index].is_match(sample),
                "primitive pattern {index} did not match: {sample:?}"
            );
        }
    }

    #[test]
    fn all_valuable_patterns_compile_and_match() {
        let samples = [
            "I realized that backpressure prevents overload",
            "This works because the thread pool is bounded",
            "This is a pattern worth encoding as a principle",
            "The root cause was unbounded queue growth",
            "To avoid this in the future, add a circuit breaker",
            "The retry approach is better than failing immediately",
            "Always prefer idempotent operations at the boundary",
            "The invariant here is that readers never block writers",
            "This approach generalizes to any rate-limited API",
            "Key insight: locks should be held for the shortest possible duration",
        ];
        for (index, sample) in samples.iter().enumerate() {
            assert!(
                VALUABLE_PATTERNS[index].is_match(sample),
                "valuable pattern {index} did not match: {sample:?}"
            );
        }
    }

    // -- word_set / jaccard helpers ------------------------------------------

    #[test]
    fn jaccard_similarity_identical_sets_is_one() {
        let set_a = word_set("the quick brown fox");
        let set_b = word_set("the quick brown fox");
        assert!((jaccard_similarity(&set_a, &set_b) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn jaccard_similarity_disjoint_sets_is_zero() {
        let set_a = word_set("alpha beta gamma");
        let set_b = word_set("delta epsilon zeta");
        assert_eq!(jaccard_similarity(&set_a, &set_b), 0.0);
    }

    #[test]
    fn jaccard_similarity_empty_sets_is_zero() {
        assert_eq!(jaccard_similarity(&HashSet::new(), &HashSet::new()), 0.0);
    }

    #[test]
    fn word_set_is_case_insensitive() {
        let set_a = word_set("Use Connection Pooling");
        let set_b = word_set("use connection pooling");
        // Case-normalised sets must be identical.
        assert_eq!(jaccard_similarity(&set_a, &set_b), 1.0);
    }
}
