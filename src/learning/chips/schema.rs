//! Chip definition validation.
//!
//! Validates `ChipDefinition` structs against required field constraints and
//! advisory quality checks. Strictness controls whether advisory issues are
//! collected as warnings, promoted to errors, or ignored entirely.

use super::runtime::ChipDefinition;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// ValidationStrictness
// ---------------------------------------------------------------------------

/// Controls how strictly chip definitions are validated.
///
/// Required field checks (empty id, name, triggers, human_benefit,
/// harm_avoidance) always produce errors regardless of strictness. Advisory
/// checks vary by level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStrictness {
    /// Collect all issues. Valid as long as there are no hard errors;
    /// warnings do not affect the valid flag.
    Warn,
    /// Same as Warn but emits `tracing::warn!` for each advisory issue.
    Block,
    /// Warnings are promoted to errors. Any advisory issue makes the
    /// definition invalid.
    Strict,
    /// Only check required fields. Advisory checks are skipped entirely.
    Error,
}

impl ValidationStrictness {
    /// Parse from a string, defaulting to `Warn` for unrecognised values.
    pub fn from_str_lossy(value: &str) -> Self {
        match value {
            "warn" => Self::Warn,
            "block" => Self::Block,
            "strict" => Self::Strict,
            "error" => Self::Error,
            _ => Self::Warn,
        }
    }
}

impl std::fmt::Display for ValidationStrictness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Warn => write!(f, "warn"),
            Self::Block => write!(f, "block"),
            Self::Strict => write!(f, "strict"),
            Self::Error => write!(f, "error"),
        }
    }
}

// ---------------------------------------------------------------------------
// ValidationResult
// ---------------------------------------------------------------------------

/// Outcome of validating a chip definition against a given strictness level.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    /// Whether the definition is considered valid under the applied strictness.
    pub valid: bool,
    /// Hard errors that must be resolved before the chip can be used.
    pub errors: Vec<String>,
    /// Advisory issues collected under Warn and Block modes.
    pub warnings: Vec<String>,
}

impl ValidationResult {
    fn new() -> Self {
        Self {
            valid: true,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn add_error(&mut self, message: impl Into<String>) {
        self.errors.push(message.into());
        self.valid = false;
    }

    fn add_warning(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate a single chip definition under the given strictness.
///
/// Required field violations always produce errors. Advisory issues (empty
/// observations, empty success criteria, non-canonical ID characters, and
/// unconditional triggers) are handled according to `strictness`.
pub fn validate_chip(chip: &ChipDefinition, strictness: ValidationStrictness) -> ValidationResult {
    let mut result = ValidationResult::new();

    // Required fields — errors regardless of strictness.
    if chip.id.is_empty() {
        result.add_error("id must not be empty");
    }
    if chip.name.is_empty() {
        result.add_error("name must not be empty");
    }
    if chip.triggers.is_empty() {
        result.add_error("triggers must not be empty");
    }
    if chip.human_benefit.is_empty() {
        result.add_error("human_benefit must not be empty");
    }
    if chip.harm_avoidance.is_empty() {
        result.add_error("harm_avoidance must not be empty");
    }
    // risk_level is always present — it's an enum with no empty state.

    // Advisory checks are omitted entirely in Error mode.
    if matches!(strictness, ValidationStrictness::Error) {
        return result;
    }

    // Collect advisory issues before distributing them by strictness.
    let mut advisory: Vec<String> = Vec::new();

    if chip.observations.is_empty() {
        advisory.push("No observations defined".into());
    }
    if chip.success_criteria.is_empty() {
        advisory.push("No success criteria defined".into());
    }
    // Only flag the ID format when the ID itself is non-empty; an empty id is
    // already reported as a required-field error above.
    if !chip.id.is_empty() && !is_valid_chip_id(&chip.id) {
        advisory.push("ID should be alphanumeric with underscores".into());
    }
    // A trigger where every field is None matches all events unconditionally,
    // which is almost never intentional and can produce noisy advisories.
    for trigger in &chip.triggers {
        if !trigger.has_conditions() {
            advisory.push("Trigger has no conditions, will match everything".into());
        }
    }

    match strictness {
        ValidationStrictness::Warn => {
            for message in advisory {
                result.add_warning(message);
            }
        }
        ValidationStrictness::Block => {
            for message in advisory {
                tracing::warn!(chip_id = %chip.id, warning = %message, "chip validation warning");
                result.add_warning(message);
            }
        }
        ValidationStrictness::Strict => {
            // Warnings become errors; no warnings are recorded separately.
            for message in advisory {
                result.add_error(message);
            }
        }
        // Already returned early above.
        ValidationStrictness::Error => {}
    }

    result
}

/// Validate a slice of chip definitions, returning (chip_id, result) pairs.
///
/// Preserves input order. Each chip is validated independently; a failure in
/// one does not affect the others.
pub fn validate_chips(
    chips: &[ChipDefinition],
    strictness: ValidationStrictness,
) -> Vec<(String, ValidationResult)> {
    chips
        .iter()
        .map(|chip| (chip.id.clone(), validate_chip(chip, strictness)))
        .collect()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns true when the ID consists solely of ASCII alphanumerics and `_`.
///
/// Spaces, hyphens, dots, and other special characters all fail this check.
fn is_valid_chip_id(id: &str) -> bool {
    id.chars().all(|character| character.is_ascii_alphanumeric() || character == '_')
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::runtime::{ChipDefinition, ChipTrigger, EvolutionConfig, ObservationSpec};
    use crate::learning::RiskLevel;

    fn test_observation() -> ObservationSpec {
        ObservationSpec {
            field_name: "result".into(),
            field_type: "string".into(),
            description: "The tool result.".into(),
        }
    }

    fn test_evolution() -> EvolutionConfig {
        EvolutionConfig {
            auto_deprecate: false,
            min_observations: 10,
            min_confidence: 0.5,
        }
    }

    // Satisfies all required fields; advisory issues are absent so the chip
    // passes clean in every mode including Strict.
    fn fully_valid_chip() -> ChipDefinition {
        ChipDefinition {
            id: "full_chip".into(),
            name: "Full Chip".into(),
            triggers: vec![ChipTrigger::with_conditions()],
            observations: vec![test_observation()],
            success_criteria: vec!["no regressions introduced".into()],
            human_benefit: "Guides toward safer file operations".into(),
            harm_avoidance: vec!["never overwrites without backup".into()],
            risk_level: RiskLevel::Low,
            evolution: test_evolution(),
        }
    }

    // Satisfies required fields only; advisory issues (no observations, no
    // success criteria) are present, but valid under Warn/Block/Error modes.
    fn minimal_chip() -> ChipDefinition {
        ChipDefinition {
            id: "test_chip".into(),
            name: "Test Chip".into(),
            triggers: vec![ChipTrigger::with_conditions()],
            observations: Vec::new(),
            success_criteria: Vec::new(),
            human_benefit: "Reduces repeated mistakes".into(),
            harm_avoidance: vec!["does not expose secrets".into()],
            risk_level: RiskLevel::Low,
            evolution: test_evolution(),
        }
    }

    // -----------------------------------------------------------------------
    // Strictness modes
    // -----------------------------------------------------------------------

    #[test]
    fn test_warn_mode_valid_with_warnings() {
        let chip = minimal_chip(); // no observations, no success_criteria
        let result = validate_chip(&chip, ValidationStrictness::Warn);

        assert!(result.valid, "warn mode: advisory issues should not fail validation");
        assert!(result.errors.is_empty());
        assert_eq!(result.warnings.len(), 2, "expected two advisory warnings");
    }

    #[test]
    fn test_block_mode_valid_with_warnings() {
        let chip = minimal_chip();
        let result = validate_chip(&chip, ValidationStrictness::Block);

        assert!(result.valid, "block mode: advisory issues should not fail validation");
        assert!(result.errors.is_empty());
        assert!(!result.warnings.is_empty());
    }

    #[test]
    fn test_strict_mode_promotes_warnings_to_errors() {
        let chip = minimal_chip(); // no observations, no success_criteria = 2 advisory
        let result = validate_chip(&chip, ValidationStrictness::Strict);

        assert!(!result.valid, "strict mode: advisory issues must fail validation");
        assert_eq!(result.errors.len(), 2);
        assert!(result.warnings.is_empty(), "strict mode must not record warnings separately");
    }

    #[test]
    fn test_error_mode_skips_advisory_checks() {
        let chip = minimal_chip();
        let result = validate_chip(&chip, ValidationStrictness::Error);

        assert!(result.valid, "error mode: advisory checks are skipped");
        assert!(result.errors.is_empty());
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn test_fully_valid_chip_passes_all_modes() {
        let chip = fully_valid_chip();

        for strictness in [
            ValidationStrictness::Warn,
            ValidationStrictness::Block,
            ValidationStrictness::Strict,
            ValidationStrictness::Error,
        ] {
            let result = validate_chip(&chip, strictness);
            assert!(result.valid, "fully valid chip should pass under {strictness}");
            assert!(result.errors.is_empty());
            assert!(result.warnings.is_empty());
        }
    }

    // -----------------------------------------------------------------------
    // Required field errors
    // -----------------------------------------------------------------------

    #[test]
    fn test_empty_id_always_errors() {
        let mut chip = minimal_chip();
        chip.id = String::new();

        for strictness in [
            ValidationStrictness::Warn,
            ValidationStrictness::Block,
            ValidationStrictness::Strict,
            ValidationStrictness::Error,
        ] {
            let result = validate_chip(&chip, strictness);
            assert!(!result.valid, "empty id must be invalid under {strictness}");
            assert!(
                result.errors.iter().any(|error| error.contains("id")),
                "expected id error under {strictness}, got: {:?}",
                result.errors,
            );
        }
    }

    #[test]
    fn test_empty_name_always_errors() {
        let mut chip = minimal_chip();
        chip.name = String::new();

        let result = validate_chip(&chip, ValidationStrictness::Warn);
        assert!(!result.valid);
        assert!(result.errors.iter().any(|error| error.contains("name")));
    }

    #[test]
    fn test_empty_triggers_always_errors() {
        let mut chip = minimal_chip();
        chip.triggers = Vec::new();

        let result = validate_chip(&chip, ValidationStrictness::Warn);
        assert!(!result.valid);
        assert!(result.errors.iter().any(|error| error.contains("triggers")));
    }

    #[test]
    fn test_empty_human_benefit_always_errors() {
        let mut chip = minimal_chip();
        chip.human_benefit = String::new();

        let result = validate_chip(&chip, ValidationStrictness::Warn);
        assert!(!result.valid);
        assert!(result.errors.iter().any(|error| error.contains("human_benefit")));
    }

    #[test]
    fn test_empty_harm_avoidance_always_errors() {
        let mut chip = minimal_chip();
        chip.harm_avoidance = Vec::new();

        let result = validate_chip(&chip, ValidationStrictness::Warn);
        assert!(!result.valid);
        assert!(result.errors.iter().any(|error| error.contains("harm_avoidance")));
    }

    #[test]
    fn test_multiple_required_field_errors_accumulate() {
        let mut chip = minimal_chip();
        chip.id = String::new();
        chip.name = String::new();
        chip.triggers = Vec::new();

        let result = validate_chip(&chip, ValidationStrictness::Warn);
        assert!(!result.valid);
        assert_eq!(result.errors.len(), 3);
    }

    // -----------------------------------------------------------------------
    // Advisory checks
    // -----------------------------------------------------------------------

    #[test]
    fn test_empty_observations_warns() {
        let mut chip = fully_valid_chip();
        chip.observations = Vec::new();

        let result = validate_chip(&chip, ValidationStrictness::Warn);
        assert!(result.valid);
        assert!(result.warnings.iter().any(|w| w.contains("observations")));
    }

    #[test]
    fn test_empty_success_criteria_warns() {
        let mut chip = fully_valid_chip();
        chip.success_criteria = Vec::new();

        let result = validate_chip(&chip, ValidationStrictness::Warn);
        assert!(result.valid);
        assert!(result.warnings.iter().any(|w| w.contains("success criteria")));
    }

    #[test]
    fn test_id_with_spaces_warns() {
        let mut chip = minimal_chip();
        chip.id = "my chip".into();

        let result = validate_chip(&chip, ValidationStrictness::Warn);
        assert!(result.valid);
        assert!(result.warnings.iter().any(|w| w.contains("alphanumeric")));
    }

    #[test]
    fn test_id_with_special_characters_warns() {
        let mut chip = minimal_chip();
        chip.id = "my-chip!".into();

        let result = validate_chip(&chip, ValidationStrictness::Warn);
        assert!(result.valid);
        assert!(result.warnings.iter().any(|w| w.contains("alphanumeric")));
    }

    #[test]
    fn test_id_with_hyphens_fails_strict() {
        let mut chip = minimal_chip();
        chip.id = "my-chip".into();

        let result = validate_chip(&chip, ValidationStrictness::Strict);
        assert!(!result.valid);
        assert!(result.errors.iter().any(|e| e.contains("alphanumeric")));
    }

    #[test]
    fn test_valid_id_characters_do_not_warn() {
        let mut chip = minimal_chip();
        chip.id = "my_chip_123".into();

        let result = validate_chip(&chip, ValidationStrictness::Warn);
        assert!(!result.warnings.iter().any(|w| w.contains("alphanumeric")));
    }

    #[test]
    fn test_unconditioned_trigger_warns() {
        let mut chip = minimal_chip();
        chip.triggers = vec![ChipTrigger::without_conditions()];

        let result = validate_chip(&chip, ValidationStrictness::Warn);
        assert!(result.valid);
        assert!(result.warnings.iter().any(|w| w.contains("no conditions")));
    }

    #[test]
    fn test_unconditioned_trigger_fails_strict() {
        let mut chip = minimal_chip();
        chip.triggers = vec![ChipTrigger::without_conditions()];

        let result = validate_chip(&chip, ValidationStrictness::Strict);
        assert!(!result.valid);
        assert!(result.errors.iter().any(|e| e.contains("no conditions")));
    }

    #[test]
    fn test_multiple_triggers_each_checked_independently() {
        let mut chip = minimal_chip();
        chip.triggers = vec![
            ChipTrigger::with_conditions(),
            ChipTrigger::without_conditions(),
            ChipTrigger::without_conditions(),
        ];

        let result = validate_chip(&chip, ValidationStrictness::Warn);
        assert!(result.valid);
        // One warning per unconditioned trigger.
        assert_eq!(
            result.warnings.iter().filter(|w| w.contains("no conditions")).count(),
            2,
        );
    }

    #[test]
    fn test_pattern_only_trigger_is_conditioned() {
        let mut chip = minimal_chip();
        chip.triggers = vec![ChipTrigger {
            event: None,
            domain: None,
            pattern: Some(r"^tool_".into()),
        }];

        let result = validate_chip(&chip, ValidationStrictness::Warn);
        assert!(!result.warnings.iter().any(|w| w.contains("no conditions")));
    }

    // -----------------------------------------------------------------------
    // validate_chips (batch)
    // -----------------------------------------------------------------------

    #[test]
    fn test_validate_chips_returns_paired_results() {
        let mut second = fully_valid_chip();
        second.id = "second_chip".into();
        second.observations = Vec::new();

        let chips = vec![fully_valid_chip(), second];
        let results = validate_chips(&chips, ValidationStrictness::Warn);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "full_chip");
        assert_eq!(results[1].0, "second_chip");
        assert!(results[0].1.valid);
        assert!(results[1].1.valid);
        assert!(results[1].1.warnings.iter().any(|w| w.contains("observations")));
    }

    #[test]
    fn test_validate_chips_empty_slice() {
        let results = validate_chips(&[], ValidationStrictness::Strict);
        assert!(results.is_empty());
    }

    #[test]
    fn test_validate_chips_failure_does_not_affect_others() {
        let mut bad = fully_valid_chip();
        bad.id = "bad_chip".into();
        bad.name = String::new(); // required field error

        let chips = vec![fully_valid_chip(), bad];
        let results = validate_chips(&chips, ValidationStrictness::Warn);

        assert!(results[0].1.valid, "first chip should still pass");
        assert!(!results[1].1.valid, "second chip must fail");
    }

    // -----------------------------------------------------------------------
    // from_str_lossy / Display
    // -----------------------------------------------------------------------

    #[test]
    fn test_from_str_lossy_known_values() {
        assert_eq!(ValidationStrictness::from_str_lossy("warn"), ValidationStrictness::Warn);
        assert_eq!(ValidationStrictness::from_str_lossy("block"), ValidationStrictness::Block);
        assert_eq!(ValidationStrictness::from_str_lossy("strict"), ValidationStrictness::Strict);
        assert_eq!(ValidationStrictness::from_str_lossy("error"), ValidationStrictness::Error);
    }

    #[test]
    fn test_from_str_lossy_unknown_defaults_to_warn() {
        assert_eq!(ValidationStrictness::from_str_lossy("unknown"), ValidationStrictness::Warn);
        assert_eq!(ValidationStrictness::from_str_lossy(""), ValidationStrictness::Warn);
        // Variants are case-sensitive.
        assert_eq!(ValidationStrictness::from_str_lossy("WARN"), ValidationStrictness::Warn);
    }

    #[test]
    fn test_display_round_trips() {
        for strictness in [
            ValidationStrictness::Warn,
            ValidationStrictness::Block,
            ValidationStrictness::Strict,
            ValidationStrictness::Error,
        ] {
            assert_eq!(
                ValidationStrictness::from_str_lossy(&strictness.to_string()),
                strictness,
                "round-trip failed for {strictness}",
            );
        }
    }
}
