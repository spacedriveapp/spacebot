//! Safety policy for domain chips.
//!
//! Evaluates chip definitions against content safety rules, risk-based
//! approval requirements, and ethical dimension scoring. The policy layer
//! runs before insights are promoted into the cognitive layer.

use crate::learning::RiskLevel;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Content patterns that indicate an insight or chip definition is unsafe
/// regardless of risk level or other scoring dimensions.
pub(crate) const BLOCKED_PATTERNS: &[&str] = &[
    "deceptive",
    "manipulate",
    "coerce",
    "exploit",
    "harass",
    "weaponize",
    "mislead",
];

/// Amount subtracted from the ethics score for each sensitive keyword found
/// in content (personal, private, sensitive).
const SENSITIVE_PENALTY: f64 = 0.3;

/// Amount added to the ethics score for each benefit keyword found in content
/// (benefit, helpful, safe).
const BENEFIT_BONUS: f64 = 0.1;

/// Minimum acceptable ethics dimension score for a chip's `human_benefit` field.
/// Fields scoring below this threshold are flagged in safety validation.
const MINIMUM_BENEFIT_ETHICS_SCORE: f64 = 0.5;

// ---------------------------------------------------------------------------
// PolicyResult
// ---------------------------------------------------------------------------

/// Outcome of a policy check on chip content.
///
/// Callers should match exhaustively so that any future variant additions
/// become compile errors rather than silent handling gaps.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PolicyResult {
    /// Content passed all policy checks and may be promoted immediately.
    Allowed,
    /// Content matched a blocked pattern and must not be used.
    Blocked(String),
    /// Content passed basic checks but the risk level demands human review
    /// before the insight is promoted.
    RequiresApproval(String),
    /// Content passed basic checks. Log the event for auditing but allow
    /// promotion without blocking.
    LogOnly(String),
}

// ---------------------------------------------------------------------------
// Public functions
// ---------------------------------------------------------------------------

/// Returns `true` if `content` (compared case-insensitively) contains any
/// entry from `BLOCKED_PATTERNS`.
pub(crate) fn is_blocked(content: &str) -> bool {
    let lowered = content.to_lowercase();
    BLOCKED_PATTERNS.iter().any(|pattern| lowered.contains(pattern))
}

/// Derive a policy decision for `content` based on `risk_level`.
///
/// Blocked patterns take precedence over the risk-level classification — even
/// Low-risk content is rejected when it contains a blocked term.
pub(crate) fn check_risk_level(risk_level: &RiskLevel, content: &str) -> PolicyResult {
    if is_blocked(content) {
        return PolicyResult::Blocked(format!(
            "content contains a prohibited pattern: {:?}",
            first_blocked_pattern(content),
        ));
    }

    match risk_level {
        RiskLevel::Low => PolicyResult::Allowed,
        RiskLevel::Medium => PolicyResult::LogOnly(format!(
            "medium-risk content logged for audit: {:.80}",
            content
        )),
        RiskLevel::High => PolicyResult::RequiresApproval(format!(
            "high-risk content requires human approval before promotion: {:.80}",
            content
        )),
    }
}

/// Validate the safety fields of a chip definition, returning a list of
/// human-readable issues. An empty list means all checks passed.
///
/// Validates `human_benefit` for substance, blocked patterns, and a minimum
/// ethics dimension score. Validates each `harm_avoidance` entry for
/// non-emptiness.
pub(crate) fn validate_chip_safety(
    human_benefit: &str,
    harm_avoidance: &[String],
) -> Vec<String> {
    let mut issues = Vec::new();

    if human_benefit.trim().is_empty() {
        issues.push("human_benefit must not be empty".to_string());
    } else {
        if is_blocked(human_benefit) {
            issues.push(format!(
                "human_benefit contains a prohibited pattern: {:?}",
                first_blocked_pattern(human_benefit),
            ));
        }

        let ethics_score = check_ethics_dimension(human_benefit);
        if ethics_score < MINIMUM_BENEFIT_ETHICS_SCORE {
            issues.push(format!(
                "human_benefit ethics score {ethics_score:.2} is below the minimum {MINIMUM_BENEFIT_ETHICS_SCORE:.2}"
            ));
        }
    }

    if harm_avoidance.is_empty() {
        issues.push("harm_avoidance must contain at least one entry".to_string());
    } else {
        for (index, entry) in harm_avoidance.iter().enumerate() {
            if entry.trim().is_empty() {
                issues.push(format!("harm_avoidance[{index}] must not be empty"));
            }
        }
    }

    issues
}

/// Score content on the ethics dimension in the range \[0.0, 1.0\].
///
/// Scoring rules (applied in order):
/// - Blocked patterns short-circuit to exactly 0.0.
/// - Start at 1.0.
/// - Each sensitive keyword present (`personal`, `private`, `sensitive`)
///   subtracts 0.3.
/// - Each benefit keyword present (`benefit`, `helpful`, `safe`) adds 0.1.
/// - The result is clamped to \[0.0, 1.0\].
///
/// Comparisons are case-insensitive.
pub(crate) fn check_ethics_dimension(content: &str) -> f64 {
    let lowered = content.to_lowercase();

    if BLOCKED_PATTERNS.iter().any(|pattern| lowered.contains(pattern)) {
        return 0.0;
    }

    const SENSITIVE_KEYWORDS: &[&str] = &["personal", "private", "sensitive"];
    const BENEFIT_KEYWORDS: &[&str] = &["benefit", "helpful", "safe"];

    let mut score = 1.0_f64;

    for keyword in SENSITIVE_KEYWORDS {
        if lowered.contains(keyword) {
            score -= SENSITIVE_PENALTY;
        }
    }

    for keyword in BENEFIT_KEYWORDS {
        if lowered.contains(keyword) {
            score += BENEFIT_BONUS;
        }
    }

    score.clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Return the first blocked pattern found in `content` (case-insensitive),
/// or `"unknown"` if none match.
fn first_blocked_pattern(content: &str) -> &'static str {
    let lowered = content.to_lowercase();
    BLOCKED_PATTERNS
        .iter()
        .find(|pattern| lowered.contains(*pattern))
        .copied()
        .unwrap_or("unknown")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- is_blocked ---

    #[test]
    fn test_is_blocked_exact_pattern() {
        assert!(is_blocked("this is deceptive content"));
    }

    #[test]
    fn test_is_blocked_case_insensitive() {
        assert!(is_blocked("MANIPULATE the user"));
        assert!(is_blocked("Coerce them into agreement"));
        assert!(is_blocked("WEAPONIZE the data"));
    }

    #[test]
    fn test_is_blocked_pattern_embedded_in_longer_word() {
        // "exploit" is present inside "exploitation" — still blocked
        assert!(is_blocked("data exploitation at scale"));
    }

    #[test]
    fn test_is_blocked_clean_content_is_false() {
        assert!(!is_blocked("helpful and beneficial recipe suggestions"));
        assert!(!is_blocked("summarise meeting notes for the user"));
    }

    #[test]
    fn test_is_blocked_every_pattern_in_constant() {
        for pattern in BLOCKED_PATTERNS {
            assert!(
                is_blocked(pattern),
                "BLOCKED_PATTERNS entry {:?} should be detected by is_blocked",
                pattern
            );
        }
    }

    // --- check_risk_level ---

    #[test]
    fn test_check_risk_level_low_clean_content_is_allowed() {
        let result = check_risk_level(&RiskLevel::Low, "provide helpful cooking tips");
        assert_eq!(result, PolicyResult::Allowed);
    }

    #[test]
    fn test_check_risk_level_medium_clean_content_is_log_only() {
        let result = check_risk_level(&RiskLevel::Medium, "record user dietary preferences");
        assert!(
            matches!(result, PolicyResult::LogOnly(_)),
            "expected LogOnly for medium-risk clean content, got {result:?}"
        );
    }

    #[test]
    fn test_check_risk_level_high_clean_content_requires_approval() {
        let result = check_risk_level(&RiskLevel::High, "analyse transaction history");
        assert!(
            matches!(result, PolicyResult::RequiresApproval(_)),
            "expected RequiresApproval for high-risk clean content, got {result:?}"
        );
    }

    #[test]
    fn test_check_risk_level_blocked_pattern_overrides_low_risk() {
        let result = check_risk_level(&RiskLevel::Low, "deceptive marketing copy");
        assert!(
            matches!(result, PolicyResult::Blocked(_)),
            "expected Blocked even for Low risk when content is blocked, got {result:?}"
        );
    }

    #[test]
    fn test_check_risk_level_blocked_pattern_overrides_high_risk() {
        // Blocked takes priority — RequiresApproval must not be returned.
        let result = check_risk_level(&RiskLevel::High, "weaponize user location data");
        assert!(
            matches!(result, PolicyResult::Blocked(_)),
            "expected Blocked even for High risk when content is blocked, got {result:?}"
        );
    }

    #[test]
    fn test_check_risk_level_blocked_message_names_the_pattern() {
        let result = check_risk_level(&RiskLevel::Medium, "attempt to harass users");
        let PolicyResult::Blocked(message) = result else {
            panic!("expected Blocked");
        };
        assert!(
            message.contains("harass"),
            "expected blocked message to identify the matched pattern, got: {message}"
        );
    }

    // --- validate_chip_safety ---

    #[test]
    fn test_validate_chip_safety_valid_inputs_produce_no_issues() {
        let issues = validate_chip_safety(
            "help users find safe and beneficial recipes",
            &[
                "avoid allergen exposure".to_string(),
                "do not retain private user data".to_string(),
            ],
        );
        assert!(issues.is_empty(), "expected no issues, got: {issues:?}");
    }

    #[test]
    fn test_validate_chip_safety_empty_benefit_is_flagged() {
        let issues = validate_chip_safety("", &["prevent harm".to_string()]);
        assert!(
            issues.iter().any(|issue| issue.contains("human_benefit")),
            "expected issue mentioning human_benefit for empty input, got: {issues:?}"
        );
    }

    #[test]
    fn test_validate_chip_safety_whitespace_only_benefit_is_flagged() {
        let issues = validate_chip_safety("   ", &["prevent harm".to_string()]);
        assert!(
            issues.iter().any(|issue| issue.contains("human_benefit")),
            "expected issue mentioning human_benefit for whitespace-only input, got: {issues:?}"
        );
    }

    #[test]
    fn test_validate_chip_safety_blocked_benefit_is_flagged() {
        let issues = validate_chip_safety(
            "mislead the user for engagement",
            &["avoid data leaks".to_string()],
        );
        assert!(
            issues.iter().any(|issue| issue.contains("prohibited pattern")),
            "expected prohibited-pattern issue for blocked human_benefit, got: {issues:?}"
        );
    }

    #[test]
    fn test_validate_chip_safety_empty_harm_avoidance_is_flagged() {
        let issues = validate_chip_safety("provide helpful cooking tips", &[]);
        assert!(
            issues.iter().any(|issue| issue.contains("harm_avoidance")),
            "expected issue mentioning harm_avoidance for empty slice, got: {issues:?}"
        );
    }

    #[test]
    fn test_validate_chip_safety_blank_harm_avoidance_entry_is_flagged() {
        let issues = validate_chip_safety(
            "provide helpful cooking tips",
            &["valid entry".to_string(), "  ".to_string()],
        );
        assert!(
            issues.iter().any(|issue| issue.contains("harm_avoidance[1]")),
            "expected issue for blank entry at index 1, got: {issues:?}"
        );
    }

    #[test]
    fn test_validate_chip_safety_low_ethics_score_is_flagged() {
        // Three sensitive keywords drive score to 0.1, below the 0.5 minimum.
        let issues = validate_chip_safety(
            "access personal private sensitive records",
            &["log all access attempts".to_string()],
        );
        assert!(
            issues.iter().any(|issue| issue.contains("ethics score")),
            "expected ethics-score issue for low-scoring human_benefit, got: {issues:?}"
        );
    }

    // --- check_ethics_dimension ---

    #[test]
    fn test_check_ethics_dimension_neutral_content_is_one() {
        let score = check_ethics_dimension("provide recipe recommendations");
        assert!(
            (score - 1.0).abs() < f64::EPSILON,
            "expected 1.0 for content with no keywords, got {score}"
        );
    }

    #[test]
    fn test_check_ethics_dimension_blocked_content_is_zero() {
        let score = check_ethics_dimension("weaponize user data");
        assert_eq!(score, 0.0, "expected exactly 0.0 for blocked content");
    }

    #[test]
    fn test_check_ethics_dimension_single_sensitive_keyword_reduces_score() {
        let score = check_ethics_dimension("access personal records");
        assert!(
            (score - 0.7).abs() < 1e-9,
            "expected 0.7 (1.0 - 0.3) for one sensitive keyword, got {score}"
        );
    }

    #[test]
    fn test_check_ethics_dimension_single_benefit_keyword_is_clamped() {
        // 1.0 + 0.1 = 1.1, clamped to 1.0
        let score = check_ethics_dimension("this is helpful");
        assert!(
            (score - 1.0).abs() < f64::EPSILON,
            "expected clamped 1.0 after benefit bonus, got {score}"
        );
    }

    #[test]
    fn test_check_ethics_dimension_all_three_sensitive_keywords() {
        // 1.0 - 0.3 - 0.3 - 0.3 = 0.1
        let score = check_ethics_dimension("personal private sensitive data handling");
        assert!(
            (score - 0.1).abs() < 1e-9,
            "expected 0.1 (1.0 - 3×0.3) for three sensitive keywords, got {score}"
        );
    }

    #[test]
    fn test_check_ethics_dimension_mixed_keywords_accumulate() {
        // "personal" (-0.3) + "helpful" (+0.1) = 0.8
        let score = check_ethics_dimension("helpful tips for personal development");
        assert!(
            (score - 0.8).abs() < 1e-9,
            "expected 0.8 for one sensitive and one benefit keyword, got {score}"
        );
    }

    #[test]
    fn test_check_ethics_dimension_score_never_below_zero() {
        // Contrived content that would go negative without clamping.
        let score = check_ethics_dimension("personal private sensitive information");
        assert!(
            score >= 0.0,
            "ethics score must not go below 0.0, got {score}"
        );
    }

    #[test]
    fn test_check_ethics_dimension_score_never_above_one() {
        let score = check_ethics_dimension("benefit helpful safe approach");
        assert!(
            score <= 1.0,
            "ethics score must not exceed 1.0, got {score}"
        );
    }

    #[test]
    fn test_check_ethics_dimension_case_insensitive() {
        let lower = check_ethics_dimension("helpful guidance");
        let upper = check_ethics_dimension("HELPFUL GUIDANCE");
        assert!(
            (lower - upper).abs() < f64::EPSILON,
            "ethics scoring must be case-insensitive (lower={lower}, upper={upper})"
        );
    }

    #[test]
    fn test_check_ethics_dimension_blocked_short_circuits_before_keywords() {
        // Even with benefit keywords present, a blocked pattern returns 0.0.
        let score = check_ethics_dimension("helpful safe content that tries to manipulate");
        assert_eq!(
            score, 0.0,
            "blocked pattern must short-circuit to 0.0 regardless of benefit keywords"
        );
    }
}
