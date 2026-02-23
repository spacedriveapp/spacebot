//! Implicit outcome tracking: links advisory advice to tool-call outcomes.

use super::store::LearningStore;

use std::collections::HashMap;

/// In-flight advice record awaiting outcome resolution.
#[derive(Debug, Clone)]
pub struct PendingAdvice {
    pub id: String,
    pub advice_text: String,
    pub source_id: Option<String>,
    pub tool_name: Option<String>,
    pub episode_id: Option<String>,
    pub created_at: std::time::Instant,
}

/// Tracks advice issued before tool calls and resolves outcomes after completion.
pub(crate) struct FeedbackTracker {
    /// call_id or episode_id → pending advice
    pending: HashMap<String, Vec<PendingAdvice>>,
    ttl_secs: u64,
}

impl FeedbackTracker {
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            pending: HashMap::new(),
            ttl_secs,
        }
    }

    /// Record advice given before a tool call.
    pub fn record_advice(
        &mut self,
        key: &str,  // call_id or episode_id
        advice_text: String,
        source_id: Option<String>,
        tool_name: Option<String>,
        episode_id: Option<String>,
    ) {
        let advice = PendingAdvice {
            id: uuid::Uuid::new_v4().to_string(),
            advice_text,
            source_id,
            tool_name,
            episode_id,
            created_at: std::time::Instant::now(),
        };
        self.pending.entry(key.to_owned()).or_default().push(advice);
    }

    /// Resolve pending advice after a tool call completes.
    pub async fn resolve_outcome(
        &mut self,
        store: &LearningStore,
        key: &str,
        success: bool,
    ) -> anyhow::Result<()> {
        let Some(advice_list) = self.pending.remove(key) else {
            return Ok(());
        };

        let outcome = if success { "followed" } else { "unhelpful" };

        for advice in advice_list {
            if advice.created_at.elapsed().as_secs() > self.ttl_secs {
                continue; // expired
            }
            sqlx::query(
                "INSERT INTO implicit_feedback (id, advice_text, source_id, tool_name, episode_id, outcome, created_at, resolved_at) \
                 VALUES (?, ?, ?, ?, ?, ?, datetime('now'), datetime('now'))",
            )
            .bind(&advice.id)
            .bind(&advice.advice_text)
            .bind(&advice.source_id)
            .bind(&advice.tool_name)
            .bind(&advice.episode_id)
            .bind(outcome)
            .execute(store.pool())
            .await?;
        }
        Ok(())
    }

    /// Purge expired pending advice entries.
    pub fn purge_expired(&mut self) {
        self.pending.retain(|_key, advice_list| {
            advice_list.retain(|advice| advice.created_at.elapsed().as_secs() <= self.ttl_secs);
            !advice_list.is_empty()
        });
    }
}
