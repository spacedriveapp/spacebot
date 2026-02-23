//! Chip runtime: event routing, YAML loading, observation handling.
//!
//! A `ChipRuntime` holds a set of `ChipDefinition`s loaded from YAML files on
//! disk. At runtime it routes incoming events to matching chips, records
//! per-chip observations in `chip_observations`, and tracks aggregate chip
//! state in `chip_state`.

use crate::learning::{LearningStore, RiskLevel};

use anyhow::{Context as _, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

use std::path::Path;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Specifies which events or domains activate a chip.
///
/// All non-`None` fields are AND-combined: every field present must match.
/// A trigger where every field is `None` matches all events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChipTrigger {
    /// Exact equality match against the incoming event type string.
    pub event: Option<String>,
    /// Exact equality match against the domain label attached to the event.
    pub domain: Option<String>,
    /// Regex pattern matched against the event type string.
    pub pattern: Option<String>,
}

impl ChipTrigger {
    /// Create a trigger with at least one condition set (`event = "tool_call"`).
    ///
    /// Used primarily in tests and as a canonical non-empty trigger.
    #[cfg(test)]
    pub fn with_conditions() -> Self {
        Self {
            event: Some("tool_call".into()),
            domain: None,
            pattern: None,
        }
    }

    /// Create a trigger where every field is `None`, matching all events.
    ///
    /// The schema validator treats this as advisory: an all-None trigger
    /// is valid but noisy.
    #[cfg(test)]
    pub fn without_conditions() -> Self {
        Self {
            event: None,
            domain: None,
            pattern: None,
        }
    }

    /// Returns `true` when at least one field is `Some`, meaning the trigger
    /// is selective rather than unconditional.
    pub fn has_conditions(&self) -> bool {
        self.event.is_some() || self.domain.is_some() || self.pattern.is_some()
    }
}

/// Describes a single field the chip should capture per observation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationSpec {
    pub field_name: String,
    pub field_type: String,
    pub description: String,
}

impl Default for ObservationSpec {
    fn default() -> Self {
        Self {
            field_name: "value".into(),
            field_type: "string".into(),
            description: String::new(),
        }
    }
}

/// Controls automatic deprecation and quality floors for a chip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionConfig {
    /// Automatically deprecate the chip when floors are not met after
    /// sufficient observations have accumulated.
    #[serde(default)]
    pub auto_deprecate: bool,
    /// Minimum observation count before insights are scored.
    #[serde(default = "default_min_observations")]
    pub min_observations: u64,
    /// Minimum confidence an insight must reach before promotion.
    #[serde(default = "default_min_confidence")]
    pub min_confidence: f64,
}

fn default_min_observations() -> u64 {
    10
}

fn default_min_confidence() -> f64 {
    0.5
}

impl Default for EvolutionConfig {
    fn default() -> Self {
        Self {
            auto_deprecate: false,
            min_observations: default_min_observations(),
            min_confidence: default_min_confidence(),
        }
    }
}

/// A YAML-defined domain chip: a focused learning module for one knowledge area.
///
/// Chips are loaded from `chips/*.yaml` at startup and matched against
/// incoming events by the runtime. Each chip defines what to observe, what
/// counts as success, and how its insights should evolve over time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChipDefinition {
    /// Stable identifier used in all database tables.
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// One or more triggers that activate this chip.
    pub triggers: Vec<ChipTrigger>,
    /// Fields this chip expects to receive per observation.
    pub observations: Vec<ObservationSpec>,
    /// Conditions that constitute a successful outcome for this domain.
    pub success_criteria: Vec<String>,
    /// How this chip's insights benefit users.
    pub human_benefit: String,
    /// Situations where this chip's guidance should be suppressed or softened.
    pub harm_avoidance: Vec<String>,
    /// Risk classification used to gate promotion and advisory authority.
    pub risk_level: RiskLevel,
    /// Auto-deprecation and quality-floor configuration.
    pub evolution: EvolutionConfig,
}

/// A single observation row from `chip_observations`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChipObservation {
    pub id: String,
    pub chip_id: String,
    pub field_name: String,
    pub field_type: String,
    pub value: String,
    pub episode_id: Option<String>,
    pub created_at: String,
}

/// Aggregate state for a chip, tracked in `chip_state`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChipState {
    pub chip_id: String,
    pub observation_count: i64,
    pub success_rate: f64,
    pub status: String,
    pub confidence: f64,
}

// ---------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------

/// Loads chip definitions from YAML files and routes events to matching chips.
#[derive(Debug)]
pub struct ChipRuntime {
    chips: Vec<ChipDefinition>,
    store: Arc<LearningStore>,
}

impl ChipRuntime {
    /// Create a runtime with no chips loaded.
    pub fn new(store: Arc<LearningStore>) -> Self {
        Self {
            chips: Vec::new(),
            store,
        }
    }

    /// Load all `*.yaml` / `*.yml` files from `dir`, parse each as a
    /// `ChipDefinition`, validate its structure, and add it to the runtime.
    ///
    /// Logs a warning and skips files that fail to parse or validate rather
    /// than aborting the whole load, so one bad chip cannot block the others.
    /// Returns the count of successfully loaded chips.
    pub fn load_chips_from_dir(&mut self, dir: &Path) -> Result<usize> {
        let mut loaded = 0usize;

        let entries = std::fs::read_dir(dir)
            .with_context(|| format!("failed to read chip directory {}", dir.display()))?;

        for entry in entries {
            let entry = entry.context("failed to read directory entry")?;
            let path = entry.path();

            let is_yaml = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext == "yaml" || ext == "yml")
                .unwrap_or(false);

            if !is_yaml {
                continue;
            }

            let contents = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) => {
                    tracing::warn!(%error, path = %path.display(), "failed to read chip file — skipping");
                    continue;
                }
            };

            let chip: ChipDefinition = match serde_yaml::from_str(&contents) {
                Ok(definition) => definition,
                Err(error) => {
                    tracing::warn!(%error, path = %path.display(), "failed to parse chip YAML — skipping");
                    continue;
                }
            };

            // Validate via the schema module's structural checks.
            if let Err(error) = validate_chip(&chip) {
                tracing::warn!(%error, chip_id = %chip.id, path = %path.display(), "chip validation failed — skipping");
                continue;
            }

            tracing::info!(chip_id = %chip.id, name = %chip.name, "loaded chip definition");
            self.chips.push(chip);
            loaded += 1;
        }

        Ok(loaded)
    }

    /// Add a chip definition directly, bypassing YAML loading.
    ///
    /// Useful for programmatically constructed chips and tests. Callers are
    /// responsible for pre-validating the definition.
    pub fn add_chip(&mut self, chip: ChipDefinition) {
        self.chips.push(chip);
    }

    /// Return all chips whose triggers match `event_type` and `domain`.
    ///
    /// A chip matches if any one of its triggers matches (OR across triggers).
    /// Within a single trigger all specified fields must match (AND logic).
    pub fn route_event<'a>(
        &'a self,
        event_type: &str,
        domain: Option<&str>,
    ) -> Vec<&'a ChipDefinition> {
        self.chips
            .iter()
            .filter(|chip| {
                chip.triggers
                    .iter()
                    .any(|trigger| matches_trigger(trigger, event_type, domain))
            })
            .collect()
    }

    /// Insert a row into `chip_observations`.
    pub async fn record_observation(
        &self,
        chip_id: &str,
        field_name: &str,
        field_type: &str,
        value: &str,
        episode_id: Option<&str>,
    ) -> Result<()> {
        let id = uuid::Uuid::new_v4().to_string();

        sqlx::query(
            r#"
            INSERT INTO chip_observations
                (id, chip_id, field_name, field_type, value, episode_id, created_at)
            VALUES
                (?, ?, ?, ?, ?, ?, datetime('now'))
            "#,
        )
        .bind(&id)
        .bind(chip_id)
        .bind(field_name)
        .bind(field_type)
        .bind(value)
        .bind(episode_id)
        .execute(self.store.pool())
        .await
        .context("failed to insert chip observation")?;

        Ok(())
    }

    /// Retrieve recent observations for a chip, newest first.
    pub async fn get_observations(
        &self,
        chip_id: &str,
        limit: i64,
    ) -> Result<Vec<ChipObservation>> {
        let rows: Vec<(String, String, String, String, String, Option<String>, String)> =
            sqlx::query_as(
                r#"
                SELECT id, chip_id, field_name, field_type, value, episode_id, created_at
                FROM chip_observations
                WHERE chip_id = ?
                ORDER BY created_at DESC
                LIMIT ?
                "#,
            )
            .bind(chip_id)
            .bind(limit)
            .fetch_all(self.store.pool())
            .await
            .context("failed to fetch chip observations")?;

        let observations = rows
            .into_iter()
            .map(
                |(id, chip_id, field_name, field_type, value, episode_id, created_at)| {
                    ChipObservation {
                        id,
                        chip_id,
                        field_name,
                        field_type,
                        value,
                        episode_id,
                        created_at,
                    }
                },
            )
            .collect();

        Ok(observations)
    }

    /// Upsert `chip_state`, incrementing `observation_count` by `delta` and
    /// stamping `last_triggered_at`. The count floor is zero — negative deltas
    /// cannot drive the count below zero.
    pub async fn update_chip_state(
        &self,
        chip_id: &str,
        observation_count_delta: i64,
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO chip_state
                (chip_id, observation_count, success_rate, status, confidence,
                 last_triggered_at, created_at, updated_at)
            VALUES
                (?, MAX(0, ?), 0.5, 'active', 0.5, datetime('now'), datetime('now'), datetime('now'))
            ON CONFLICT(chip_id) DO UPDATE SET
                observation_count = MAX(0, observation_count + excluded.observation_count),
                last_triggered_at = excluded.last_triggered_at,
                updated_at         = excluded.updated_at
            "#,
        )
        .bind(chip_id)
        .bind(observation_count_delta)
        .execute(self.store.pool())
        .await
        .context("failed to upsert chip state")?;

        Ok(())
    }

    /// Retrieve the current state row for a chip, or `None` if it has never
    /// been triggered.
    pub async fn get_chip_state(&self, chip_id: &str) -> Result<Option<ChipState>> {
        let row: Option<(String, i64, f64, String, f64)> = sqlx::query_as(
            r#"
            SELECT chip_id, observation_count, success_rate, status, confidence
            FROM chip_state
            WHERE chip_id = ?
            "#,
        )
        .bind(chip_id)
        .fetch_optional(self.store.pool())
        .await
        .context("failed to fetch chip state")?;

        Ok(row.map(
            |(chip_id, observation_count, success_rate, status, confidence)| ChipState {
                chip_id,
                observation_count,
                success_rate,
                status,
                confidence,
            },
        ))
    }
}

// ---------------------------------------------------------------------------
// Trigger matching
// ---------------------------------------------------------------------------

/// Return `true` if `trigger` matches the given event type and optional domain.
///
/// AND logic: every non-`None` field in the trigger must match. The regex
/// pattern is compiled on every call; invalid patterns log a warning and
/// are treated as non-matching. Patterns are validated at chip load time
/// (see `validate_chip`), so compilation errors here indicate a bug.
fn matches_trigger(trigger: &ChipTrigger, event_type: &str, domain: Option<&str>) -> bool {
    if let Some(required_event) = &trigger.event {
        if required_event != event_type {
            return false;
        }
    }

    if let Some(required_domain) = &trigger.domain {
        match domain {
            Some(actual_domain) if actual_domain == required_domain => {}
            _ => return false,
        }
    }

    if let Some(pattern) = &trigger.pattern {
        match Regex::new(pattern) {
            Ok(regex) => {
                if !regex.is_match(event_type) {
                    return false;
                }
            }
            Err(error) => {
                // Patterns are validated at load time, so this path means
                // a chip was added without going through `load_chips_from_dir`.
                tracing::warn!(%error, pattern, "chip trigger has invalid regex — treating as non-match");
                return false;
            }
        }
    }

    true
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Run structural validation on a chip definition before it enters the runtime.
///
/// Validates required fields and pre-compiles regex patterns so that
/// `matches_trigger` is guaranteed to succeed for all loaded chips.
fn validate_chip(chip: &ChipDefinition) -> Result<()> {
    if chip.id.is_empty() {
        anyhow::bail!("can't load chip: id is empty");
    }

    if chip.name.is_empty() {
        anyhow::bail!("can't load chip '{}': name is empty", chip.id);
    }

    if chip.triggers.is_empty() {
        anyhow::bail!(
            "can't load chip '{}': must have at least one trigger",
            chip.id
        );
    }

    // Pre-compile regex patterns so route_event never encounters invalid ones.
    for trigger in &chip.triggers {
        if let Some(pattern) = &trigger.pattern {
            Regex::new(pattern).with_context(|| {
                format!(
                    "can't load chip '{}': invalid regex pattern '{}'",
                    chip.id, pattern
                )
            })?;
        }
    }

    if chip.evolution.min_confidence < 0.0 || chip.evolution.min_confidence > 1.0 {
        anyhow::bail!(
            "can't load chip '{}': min_confidence must be in [0.0, 1.0], got {}",
            chip.id,
            chip.evolution.min_confidence
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Trigger matching — pure functions, no database needed
    // -----------------------------------------------------------------------

    fn trigger(event: Option<&str>, domain: Option<&str>, pattern: Option<&str>) -> ChipTrigger {
        ChipTrigger {
            event: event.map(str::to_string),
            domain: domain.map(str::to_string),
            pattern: pattern.map(str::to_string),
        }
    }

    #[test]
    fn all_none_trigger_matches_everything() {
        let t = trigger(None, None, None);
        assert!(matches_trigger(&t, "tool_call", None));
        assert!(matches_trigger(&t, "tool_call", Some("rust")));
        assert!(matches_trigger(&t, "anything", Some("whatever")));
    }

    #[test]
    fn event_exact_match_required() {
        let t = trigger(Some("tool_call"), None, None);
        assert!(matches_trigger(&t, "tool_call", None));
        assert!(!matches_trigger(&t, "message_sent", None));
    }

    #[test]
    fn domain_exact_match_required() {
        let t = trigger(None, Some("rust"), None);
        assert!(matches_trigger(&t, "tool_call", Some("rust")));
        assert!(!matches_trigger(&t, "tool_call", Some("python")));
        // Missing domain never matches a trigger that requires one.
        assert!(!matches_trigger(&t, "tool_call", None));
    }

    #[test]
    fn event_and_domain_both_must_match() {
        let t = trigger(Some("tool_call"), Some("rust"), None);
        assert!(matches_trigger(&t, "tool_call", Some("rust")));
        assert!(!matches_trigger(&t, "tool_call", Some("python")));
        assert!(!matches_trigger(&t, "message_sent", Some("rust")));
    }

    #[test]
    fn pattern_regex_match() {
        let t = trigger(None, None, Some(r"^tool_"));
        assert!(matches_trigger(&t, "tool_call", None));
        assert!(matches_trigger(&t, "tool_result", None));
        assert!(!matches_trigger(&t, "message_sent", None));
    }

    #[test]
    fn pattern_combined_with_domain() {
        let t = trigger(None, Some("rust"), Some(r"^tool_"));
        assert!(matches_trigger(&t, "tool_call", Some("rust")));
        assert!(!matches_trigger(&t, "tool_call", Some("python")));
        assert!(!matches_trigger(&t, "message_sent", Some("rust")));
    }

    #[test]
    fn invalid_regex_does_not_match() {
        // An invalid pattern is treated as a non-match rather than a panic.
        let t = trigger(None, None, Some(r"[invalid"));
        assert!(!matches_trigger(&t, "tool_call", None));
    }

    // -----------------------------------------------------------------------
    // Chip loading — uses a real temp-file LearningStore
    // -----------------------------------------------------------------------

    fn minimal_chip_yaml(id: &str) -> String {
        format!(
            r#"id: {id}
name: "Test Chip"
triggers:
  - event: "tool_call"
observations:
  - field_name: "result"
    field_type: "string"
    description: "The tool result."
success_criteria:
  - "Tool returned successfully."
human_benefit: "Helps the user."
harm_avoidance:
  - "Do not run destructive commands."
risk_level: "low"
evolution:
  auto_deprecate: false
  min_observations: 5
  min_confidence: 0.6
"#
        )
    }

    fn bare_chip(id: &str, event: &str) -> ChipDefinition {
        ChipDefinition {
            id: id.into(),
            name: format!("{id} chip"),
            triggers: vec![ChipTrigger {
                event: Some(event.into()),
                domain: None,
                pattern: None,
            }],
            observations: Vec::new(),
            success_criteria: Vec::new(),
            human_benefit: String::new(),
            harm_avoidance: Vec::new(),
            risk_level: RiskLevel::Low,
            evolution: EvolutionConfig {
                auto_deprecate: false,
                min_observations: 10,
                min_confidence: 0.5,
            },
        }
    }

    async fn setup_runtime() -> (ChipRuntime, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("learning.db");
        let store = LearningStore::connect(&db_path).await.unwrap();
        let runtime = ChipRuntime::new(store);
        (runtime, dir)
    }

    #[tokio::test]
    async fn load_chips_from_dir_counts_yaml_files() {
        let (mut runtime, _dir) = setup_runtime().await;
        let chip_dir = tempfile::tempdir().unwrap();

        // Write two valid chip files.
        for id in ["chip_a", "chip_b"] {
            std::fs::write(
                chip_dir.path().join(format!("{id}.yaml")),
                minimal_chip_yaml(id),
            )
            .unwrap();
        }

        // Write a non-YAML file that should be ignored.
        std::fs::write(chip_dir.path().join("notes.txt"), "ignore me").unwrap();

        let count = runtime.load_chips_from_dir(chip_dir.path()).unwrap();

        assert_eq!(count, 2);
        assert_eq!(runtime.chips.len(), 2);
    }

    #[tokio::test]
    async fn load_chips_from_dir_skips_invalid_yaml() {
        let (mut runtime, _dir) = setup_runtime().await;
        let chip_dir = tempfile::tempdir().unwrap();

        std::fs::write(
            chip_dir.path().join("good.yaml"),
            minimal_chip_yaml("good_chip"),
        )
        .unwrap();

        // Deliberately malformed YAML.
        std::fs::write(
            chip_dir.path().join("bad.yaml"),
            "id: !!invalid-yaml-here [[[",
        )
        .unwrap();

        let count = runtime.load_chips_from_dir(chip_dir.path()).unwrap();

        // Only the valid chip is loaded; the broken file is skipped.
        assert_eq!(count, 1);
        assert_eq!(runtime.chips[0].id, "good_chip");
    }

    #[tokio::test]
    async fn load_chips_from_dir_skips_chip_with_no_triggers() {
        let (mut runtime, _dir) = setup_runtime().await;
        let chip_dir = tempfile::tempdir().unwrap();

        // A chip with an empty triggers list must fail validation.
        let yaml = r#"id: no_triggers
name: "No Triggers"
triggers: []
observations: []
success_criteria: []
human_benefit: ""
harm_avoidance: []
risk_level: "low"
evolution:
  auto_deprecate: false
  min_observations: 5
  min_confidence: 0.5
"#;
        std::fs::write(chip_dir.path().join("no_triggers.yaml"), yaml).unwrap();

        let count = runtime.load_chips_from_dir(chip_dir.path()).unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn route_event_returns_matching_chips() {
        let (mut runtime, _dir) = setup_runtime().await;

        runtime.add_chip(bare_chip("chip_tool", "tool_call"));
        runtime.add_chip(bare_chip("chip_msg", "message_sent"));

        let matched = runtime.route_event("tool_call", None);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].id, "chip_tool");

        let matched = runtime.route_event("message_sent", None);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].id, "chip_msg");

        let matched = runtime.route_event("unknown_event", None);
        assert!(matched.is_empty());
    }

    #[tokio::test]
    async fn route_event_can_match_multiple_chips() {
        let (mut runtime, _dir) = setup_runtime().await;

        // Two chips both triggered by "tool_call".
        runtime.add_chip(bare_chip("chip_a", "tool_call"));
        runtime.add_chip(bare_chip("chip_b", "tool_call"));

        let matched = runtime.route_event("tool_call", None);
        assert_eq!(matched.len(), 2);
    }

    #[tokio::test]
    async fn record_and_get_observation_roundtrip() {
        let (runtime, _dir) = setup_runtime().await;

        runtime
            .record_observation("chip_a", "error_count", "integer", "3", Some("ep-1"))
            .await
            .unwrap();

        let observations = runtime.get_observations("chip_a", 10).await.unwrap();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].chip_id, "chip_a");
        assert_eq!(observations[0].field_name, "error_count");
        assert_eq!(observations[0].value, "3");
        assert_eq!(observations[0].episode_id.as_deref(), Some("ep-1"));
    }

    #[tokio::test]
    async fn update_and_get_chip_state() {
        let (runtime, _dir) = setup_runtime().await;

        // Initial upsert creates the row.
        runtime.update_chip_state("chip_x", 5).await.unwrap();
        let state = runtime.get_chip_state("chip_x").await.unwrap().unwrap();
        assert_eq!(state.observation_count, 5);
        assert_eq!(state.status, "active");

        // Second upsert increments the count.
        runtime.update_chip_state("chip_x", 3).await.unwrap();
        let state = runtime.get_chip_state("chip_x").await.unwrap().unwrap();
        assert_eq!(state.observation_count, 8);
    }

    #[tokio::test]
    async fn get_chip_state_returns_none_for_unknown_chip() {
        let (runtime, _dir) = setup_runtime().await;
        let state = runtime.get_chip_state("nonexistent").await.unwrap();
        assert!(state.is_none());
    }
}
