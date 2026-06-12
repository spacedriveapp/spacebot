//! Terminal completion tool for memory persistence branches.

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryPersistenceTerminalOutcome {
    Saved { saved_memory_ids: Vec<String> },
    NoMemories { reason: String },
}

#[derive(Debug, Default)]
pub struct MemoryPersistenceContractState {
    saved_memory_ids: Mutex<BTreeSet<String>>,
    terminal_outcome: Mutex<Option<MemoryPersistenceTerminalOutcome>>,
}

impl MemoryPersistenceContractState {
    pub fn record_saved_memory_id(&self, memory_id: impl Into<String>) {
        if let Ok(mut saved_memory_ids) = self.saved_memory_ids.lock() {
            saved_memory_ids.insert(memory_id.into());
        }
    }

    pub fn saved_memory_ids(&self) -> Vec<String> {
        self.saved_memory_ids
            .lock()
            .map(|saved_memory_ids| saved_memory_ids.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
    }

    pub fn set_terminal_outcome(&self, outcome: MemoryPersistenceTerminalOutcome) {
        if let Ok(mut terminal_outcome) = self.terminal_outcome.lock() {
            *terminal_outcome = Some(outcome);
        }
    }

    pub fn terminal_outcome(&self) -> Option<MemoryPersistenceTerminalOutcome> {
        self.terminal_outcome
            .lock()
            .ok()
            .and_then(|terminal_outcome| terminal_outcome.clone())
    }

    pub fn has_terminal_outcome(&self) -> bool {
        self.terminal_outcome
            .lock()
            .ok()
            .map(|guard| guard.is_some())
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone)]
pub struct MemoryPersistenceCompleteTool {
    state: Arc<MemoryPersistenceContractState>,
    working_memory: Option<Arc<crate::memory::WorkingMemoryStore>>,
    channel_id: Option<String>,
}

impl MemoryPersistenceCompleteTool {
    pub fn new(state: Arc<MemoryPersistenceContractState>) -> Self {
        Self {
            state,
            working_memory: None,
            channel_id: None,
        }
    }

    /// Enable working memory event writing for extracted events.
    pub fn with_working_memory(
        mut self,
        store: Arc<crate::memory::WorkingMemoryStore>,
        channel_id: Option<String>,
    ) -> Self {
        self.working_memory = Some(store);
        self.channel_id = channel_id;
        self
    }
}

#[derive(Debug, thiserror::Error)]
#[error("memory_persistence_complete failed: {0}")]
pub struct MemoryPersistenceCompleteError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryPersistenceCompleteArgs {
    /// Terminal branch outcome. Use "saved" when at least one memory was
    /// saved in this run, otherwise use "no_memories".
    pub outcome: String,
    /// Required when outcome is "saved". Must exactly match the memory IDs
    /// returned by successful memory_save tool calls in this run.
    #[serde(default)]
    pub saved_memory_ids: Vec<String>,
    /// Required when outcome is "no_memories". Explain briefly why nothing
    /// was worth saving.
    #[serde(default)]
    pub reason: Option<String>,
    /// Optional events extracted from the conversation. Each event becomes a
    /// working memory entry for temporal context.
    #[serde(default)]
    pub events: Vec<WorkingMemoryEventInput>,
}

/// A single event extracted by the persistence branch for the working memory log.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct WorkingMemoryEventInput {
    /// Event type: "decision", "user_correction", "decision_revised", "deadline_set",
    /// "blocked_on", "constraint", "outcome", "error", or "system".
    pub event_type: String,
    /// One-line summary of the event.
    pub summary: String,
    /// Importance score (0.0-1.0). Defaults to 0.5.
    #[serde(default = "default_importance")]
    pub importance: f32,
}

fn default_importance() -> f32 {
    0.5
}

#[derive(Debug, Serialize)]
pub struct MemoryPersistenceCompleteOutput {
    pub success: bool,
    pub outcome: String,
    pub saved_memory_ids: Vec<String>,
    pub reason: Option<String>,
}

impl Tool for MemoryPersistenceCompleteTool {
    const NAME: &'static str = "memory_persistence_complete";

    type Error = MemoryPersistenceCompleteError;
    type Args = MemoryPersistenceCompleteArgs;
    type Output = MemoryPersistenceCompleteOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/memory_persistence_complete").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "outcome": {
                        "type": "string",
                        "enum": ["saved", "no_memories"],
                        "description": "Terminal memory persistence outcome for this run"
                    },
                    "saved_memory_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Required for outcome=saved. Must exactly match successful memory_save IDs from this run"
                    },
                    "reason": {
                        "type": "string",
                        "description": "Required for outcome=no_memories. Brief reason why no memories were saved"
                    },
                    "events": {
                        "type": "array",
                        "description": "Optional events extracted from the conversation for working memory",
                        "items": {
                            "type": "object",
                            "properties": {
                                "event_type": {
                                "type": "string",
                                    "enum": [
                                        "decision",
                                        "user_correction",
                                        "decision_revised",
                                        "deadline_set",
                                        "blocked_on",
                                        "constraint",
                                        "outcome",
                                        "error",
                                        "system"
                                    ],
                                    "description": "Type of event"
                                },
                                "summary": {
                                    "type": "string",
                                    "description": "One-line summary of the event"
                                },
                                "importance": {
                                    "type": "number",
                                    "description": "Importance score 0.0-1.0, defaults to 0.5"
                                }
                            },
                            "required": ["event_type", "summary"]
                        }
                    }
                },
                "required": ["outcome"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let outcome = args.outcome.trim();
        let recorded_ids = self.state.saved_memory_ids();
        let output = match outcome {
            "saved" => {
                if args.saved_memory_ids.is_empty() {
                    return Err(MemoryPersistenceCompleteError(
                        "outcome 'saved' requires non-empty saved_memory_ids".to_string(),
                    ));
                }
                if recorded_ids.is_empty() {
                    return Err(MemoryPersistenceCompleteError(
                        "outcome 'saved' is invalid because no successful memory_save calls were recorded in this run"
                            .to_string(),
                    ));
                }

                let provided_ids = args.saved_memory_ids;
                let provided_set = provided_ids.iter().cloned().collect::<BTreeSet<_>>();
                if provided_set.len() != provided_ids.len() {
                    return Err(MemoryPersistenceCompleteError(
                        "saved_memory_ids must not contain duplicates".to_string(),
                    ));
                }

                let recorded_set = recorded_ids.iter().cloned().collect::<BTreeSet<_>>();
                if provided_set != recorded_set {
                    return Err(MemoryPersistenceCompleteError(format!(
                        "saved_memory_ids mismatch: expected {:?}, got {:?}",
                        recorded_ids, provided_ids
                    )));
                }

                MemoryPersistenceCompleteOutput {
                    success: true,
                    outcome: "saved".to_string(),
                    saved_memory_ids: provided_ids,
                    reason: None,
                }
            }
            "no_memories" => {
                if !args.saved_memory_ids.is_empty() {
                    return Err(MemoryPersistenceCompleteError(
                        "outcome 'no_memories' must not include saved_memory_ids".to_string(),
                    ));
                }
                if !recorded_ids.is_empty() {
                    return Err(MemoryPersistenceCompleteError(format!(
                        "outcome 'no_memories' is invalid because successful memory_save calls were recorded: {:?}",
                        recorded_ids
                    )));
                }

                let reason = args.reason.unwrap_or_default();
                if reason.trim().len() < 3 {
                    return Err(MemoryPersistenceCompleteError(
                        "outcome 'no_memories' requires a short reason".to_string(),
                    ));
                }

                MemoryPersistenceCompleteOutput {
                    success: true,
                    outcome: "no_memories".to_string(),
                    saved_memory_ids: Vec::new(),
                    reason: Some(reason.trim().to_string()),
                }
            }
            _ => {
                return Err(MemoryPersistenceCompleteError(format!(
                    "invalid outcome '{outcome}'; expected 'saved' or 'no_memories'"
                )));
            }
        };

        // Write extracted events to working memory only after validation succeeds.
        if let Some(working_memory) = &self.working_memory {
            for event_input in &args.events {
                let event_type =
                    match crate::memory::WorkingMemoryEventType::parse(&event_input.event_type) {
                        Some(event_type) => event_type,
                        None => {
                            tracing::trace!(
                                raw_event_type = %event_input.event_type,
                                "unrecognized event_type, falling back to System"
                            );
                            crate::memory::WorkingMemoryEventType::System
                        }
                    };
                let importance = event_input.importance.clamp(0.0, 1.0);
                let mut builder = working_memory
                    .emit(event_type, &event_input.summary)
                    .importance(importance);
                if let Some(channel_id) = &self.channel_id {
                    builder = builder.channel(channel_id.clone());
                }
                builder.record();
            }
            if !args.events.is_empty() {
                tracing::info!(
                    event_count = args.events.len(),
                    "persistence branch extracted events into working memory"
                );
            }
        }

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn saved_outcome_rejects_fabricated_ids() {
        let state = Arc::new(MemoryPersistenceContractState::default());
        state.record_saved_memory_id("mem_real_1");
        let tool = MemoryPersistenceCompleteTool::new(state);

        let error = tool
            .call(MemoryPersistenceCompleteArgs {
                outcome: "saved".to_string(),
                saved_memory_ids: vec!["mem_fake".to_string()],
                reason: None,
                events: vec![],
            })
            .await
            .expect_err("fabricated ids should fail");

        assert!(error.to_string().contains("saved_memory_ids mismatch"));
    }

    #[tokio::test]
    async fn saved_outcome_accepts_exact_recorded_ids() {
        let state = Arc::new(MemoryPersistenceContractState::default());
        state.record_saved_memory_id("mem_1");
        state.record_saved_memory_id("mem_2");
        let tool = MemoryPersistenceCompleteTool::new(state);

        let output = tool
            .call(MemoryPersistenceCompleteArgs {
                outcome: "saved".to_string(),
                saved_memory_ids: vec!["mem_2".to_string(), "mem_1".to_string()],
                reason: None,
                events: vec![],
            })
            .await
            .expect("exact ids should pass");

        assert!(output.success);
        assert_eq!(output.outcome, "saved");
    }

    #[tokio::test]
    async fn no_memories_outcome_accepts_short_reason_without_saves() {
        let state = Arc::new(MemoryPersistenceContractState::default());
        let tool = MemoryPersistenceCompleteTool::new(state);

        let output = tool
            .call(MemoryPersistenceCompleteArgs {
                outcome: "no_memories".to_string(),
                saved_memory_ids: Vec::new(),
                reason: Some("No durable facts in recent turns".to_string()),
                events: vec![],
            })
            .await
            .expect("no_memories should pass with reason");

        assert!(output.success);
        assert_eq!(output.outcome, "no_memories");
    }

    #[tokio::test]
    async fn persists_conversational_events() {
        let state = Arc::new(MemoryPersistenceContractState::default());

        let pool = sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .expect("sqlite connect");
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("migrations");

        let working_memory = crate::memory::WorkingMemoryStore::new(pool, chrono_tz::Tz::UTC);
        let tool = MemoryPersistenceCompleteTool::new(state)
            .with_working_memory(working_memory.clone(), Some("test-channel".to_string()));

        tool.call(MemoryPersistenceCompleteArgs {
            outcome: "no_memories".to_string(),
            saved_memory_ids: Vec::new(),
            reason: Some("Nothing worth retaining".to_string()),
            events: vec![
                WorkingMemoryEventInput {
                    event_type: "user_correction".to_string(),
                    summary: "User corrected a payment split assumption".to_string(),
                    importance: 0.8,
                },
                WorkingMemoryEventInput {
                    event_type: "decision_revised".to_string(),
                    summary: "Decision changed after user feedback".to_string(),
                    importance: 0.8,
                },
            ],
        })
        .await
        .expect("persistence complete should pass");

        let events = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let events = working_memory
                    .get_events_for_channel("test-channel", 10)
                    .await
                    .expect("working memory query");
                if events.len() == 2 {
                    break events;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed out waiting for working memory events");
        assert_eq!(events.len(), 2);
        assert!(events.iter().any(|event| {
            event.event_type == crate::memory::WorkingMemoryEventType::UserCorrection
        }));
        assert!(events.iter().any(|event| {
            event.event_type == crate::memory::WorkingMemoryEventType::DecisionRevised
        }));
    }
}
