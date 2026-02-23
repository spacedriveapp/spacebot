//! Policy patches: convert high-confidence distillations into behavioral rules.

use super::store::LearningStore;

use serde::{Deserialize, Serialize};

/// What triggers a policy patch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TriggerType {
    ErrorCount,
    PhaseEntry,
    PatternMatch,
    ConfidenceDrop,
}

impl std::fmt::Display for TriggerType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ErrorCount => write!(f, "ERROR_COUNT"),
            Self::PhaseEntry => write!(f, "PHASE_ENTRY"),
            Self::PatternMatch => write!(f, "PATTERN_MATCH"),
            Self::ConfidenceDrop => write!(f, "CONFIDENCE_DROP"),
        }
    }
}

/// What action to take when a patch fires.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ActionType {
    EmitWarning,
    RequireStep,
    AddConstraint,
    ForceValidation,
}

impl std::fmt::Display for ActionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmitWarning => write!(f, "EMIT_WARNING"),
            Self::RequireStep => write!(f, "REQUIRE_STEP"),
            Self::AddConstraint => write!(f, "ADD_CONSTRAINT"),
            Self::ForceValidation => write!(f, "FORCE_VALIDATION"),
        }
    }
}

/// A policy patch that enforces learned behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyPatch {
    pub id: String,
    pub source_distillation_id: String,
    pub trigger_type: TriggerType,
    pub trigger_config: serde_json::Value,
    pub action_type: ActionType,
    pub action_config: serde_json::Value,
    pub enabled: bool,
}

/// In-memory error counter for ERROR_COUNT trigger evaluation.
pub(crate) struct ErrorCounter {
    counts: std::collections::HashMap<String, u32>,  // process_id -> consecutive errors
}

impl ErrorCounter {
    pub fn new() -> Self {
        Self { counts: std::collections::HashMap::new() }
    }

    pub fn record_error(&mut self, process_id: &str) -> u32 {
        let count = self.counts.entry(process_id.to_owned()).or_insert(0);
        *count += 1;
        *count
    }

    pub fn record_success(&mut self, process_id: &str) {
        self.counts.remove(process_id);
    }

    pub fn get_count(&self, process_id: &str) -> u32 {
        self.counts.get(process_id).copied().unwrap_or(0)
    }
}

/// Result of evaluating policy patches against an event.
#[derive(Debug, Clone)]
pub struct PatchAction {
    pub patch_id: String,
    pub action_type: ActionType,
    pub action_config: serde_json::Value,
}

/// Check all enabled patches against current state. Returns actions to take.
pub fn evaluate_patches(
    patches: &[PolicyPatch],
    error_counter: &ErrorCounter,
    process_id: &str,
    tool_name: Option<&str>,
    _file_edit_counts: Option<&std::collections::HashMap<String, u32>>,
) -> Vec<PatchAction> {
    let mut actions = Vec::new();
    for patch in patches {
        if !patch.enabled {
            continue;
        }
        let triggered = match patch.trigger_type {
            TriggerType::ErrorCount => {
                let threshold = patch.trigger_config.get("threshold")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(2) as u32;
                error_counter.get_count(process_id) >= threshold
            }
            TriggerType::PatternMatch => {
                if let Some(pattern) = patch.trigger_config.get("pattern").and_then(|v| v.as_str()) {
                    tool_name.is_some_and(|name| name.contains(pattern))
                } else {
                    false
                }
            }
            TriggerType::PhaseEntry | TriggerType::ConfidenceDrop => {
                false // Future implementation
            }
        };
        if triggered {
            actions.push(PatchAction {
                patch_id: patch.id.clone(),
                action_type: patch.action_type.clone(),
                action_config: patch.action_config.clone(),
            });
        }
    }
    actions
}

/// Create default policy patches on first run.
pub async fn ensure_defaults(store: &LearningStore) -> anyhow::Result<()> {
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM policy_patches")
        .fetch_one(store.pool())
        .await?;
    if count.0 > 0 {
        return Ok(());
    }

    // "Two Failures Rule"
    save_patch(store, &PolicyPatch {
        id: uuid::Uuid::new_v4().to_string(),
        source_distillation_id: "builtin:two_failures".into(),
        trigger_type: TriggerType::ErrorCount,
        trigger_config: serde_json::json!({"threshold": 2}),
        action_type: ActionType::RequireStep,
        action_config: serde_json::json!({"message": "Run diagnostic before retrying"}),
        enabled: true,
    }).await?;

    // "File Thrash Prevention"
    save_patch(store, &PolicyPatch {
        id: uuid::Uuid::new_v4().to_string(),
        source_distillation_id: "builtin:file_thrash".into(),
        trigger_type: TriggerType::PatternMatch,
        trigger_config: serde_json::json!({"pattern": "file", "same_file_threshold": 3}),
        action_type: ActionType::EmitWarning,
        action_config: serde_json::json!({"message": "Consider a different approach"}),
        enabled: true,
    }).await?;

    tracing::info!("created default policy patches");
    Ok(())
}

/// Save a policy patch to the database.
pub async fn save_patch(store: &LearningStore, patch: &PolicyPatch) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO policy_patches (id, source_distillation_id, trigger_type, trigger_config, \
         action_type, action_config, enabled, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, datetime('now'))",
    )
    .bind(&patch.id)
    .bind(&patch.source_distillation_id)
    .bind(patch.trigger_type.to_string())
    .bind(patch.trigger_config.to_string())
    .bind(patch.action_type.to_string())
    .bind(patch.action_config.to_string())
    .bind(patch.enabled)
    .execute(store.pool())
    .await?;
    Ok(())
}

/// Load all enabled policy patches.
pub async fn load_enabled(store: &LearningStore) -> anyhow::Result<Vec<PolicyPatch>> {
    let rows: Vec<(String, String, String, String, String, String, bool)> = sqlx::query_as(
        "SELECT id, source_distillation_id, trigger_type, trigger_config, action_type, action_config, enabled \
         FROM policy_patches WHERE enabled = 1",
    )
    .fetch_all(store.pool())
    .await?;

    let mut patches = Vec::with_capacity(rows.len());
    for (id, source, trigger_type_str, trigger_config_str, action_type_str, action_config_str, enabled) in rows {
        let trigger_type = match trigger_type_str.as_str() {
            "ERROR_COUNT" => TriggerType::ErrorCount,
            "PHASE_ENTRY" => TriggerType::PhaseEntry,
            "PATTERN_MATCH" => TriggerType::PatternMatch,
            "CONFIDENCE_DROP" => TriggerType::ConfidenceDrop,
            _ => continue,
        };
        let action_type = match action_type_str.as_str() {
            "EMIT_WARNING" => ActionType::EmitWarning,
            "REQUIRE_STEP" => ActionType::RequireStep,
            "ADD_CONSTRAINT" => ActionType::AddConstraint,
            "FORCE_VALIDATION" => ActionType::ForceValidation,
            _ => continue,
        };
        patches.push(PolicyPatch {
            id,
            source_distillation_id: source,
            trigger_type,
            trigger_config: serde_json::from_str(&trigger_config_str).unwrap_or_default(),
            action_type,
            action_config: serde_json::from_str(&action_config_str).unwrap_or_default(),
            enabled,
        });
    }
    Ok(patches)
}
