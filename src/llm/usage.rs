//! Token usage tracking: extended usage extraction, in-memory accumulation,
//! and database persistence.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

/// Extended token usage extracted from raw provider responses.
/// Captures fields that rig's `completion::Usage` does not carry.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExtendedUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
}

impl ExtendedUsage {
    /// Extract extended usage from an Anthropic response body.
    pub fn from_anthropic_body(body: &serde_json::Value) -> Self {
        let usage = &body["usage"];
        let input_tokens = usage["input_tokens"].as_u64().unwrap_or(0);
        let output_tokens = usage["output_tokens"].as_u64().unwrap_or(0);
        let cache_read_tokens = usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
        let cache_write_tokens = usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);
        // Anthropic doesn't have reasoning_tokens in the same way, but check anyway.
        let reasoning_tokens = usage["reasoning_tokens"].as_u64().unwrap_or(0);

        Self {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
        }
    }

    /// Extract extended usage from an OpenAI-compatible response body.
    pub fn from_openai_body(body: &serde_json::Value) -> Self {
        let usage = &body["usage"];
        let prompt_tokens = usage["prompt_tokens"].as_u64().unwrap_or(0);
        let output_tokens = usage["completion_tokens"].as_u64().unwrap_or(0);

        let cache_read_tokens = usage["prompt_tokens_details"]["cached_tokens"]
            .as_u64()
            .unwrap_or(0);
        let cache_write_tokens = usage["prompt_tokens_details"]["cache_write_tokens"]
            .as_u64()
            .unwrap_or(0);
        // Non-cached input = total prompt minus cached portions.
        let input_tokens = prompt_tokens
            .saturating_sub(cache_read_tokens)
            .saturating_sub(cache_write_tokens);

        let reasoning_tokens = usage["completion_tokens_details"]["reasoning_tokens"]
            .as_u64()
            .unwrap_or(0);

        Self {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
        }
    }
}

/// Cost classification for a usage record.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostStatus {
    Estimated,
    #[default]
    Included,
    Unknown,
}

impl CostStatus {
    fn as_str(&self) -> &'static str {
        match self {
            CostStatus::Estimated => "estimated",
            CostStatus::Included => "included",
            CostStatus::Unknown => "unknown",
        }
    }

    /// Return the more conservative of two statuses.
    /// Unknown > Estimated > Included.
    fn most_conservative(self, other: CostStatus) -> CostStatus {
        match (self, other) {
            (CostStatus::Unknown, _) | (_, CostStatus::Unknown) => CostStatus::Unknown,
            (CostStatus::Estimated, _) | (_, CostStatus::Estimated) => CostStatus::Estimated,
            _ => CostStatus::Included,
        }
    }
}

impl std::fmt::Display for CostStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Accumulates token usage across multiple API calls within a single process
/// invocation. Thread-safe interior mutability via the `Mutex` wrapper on the
/// caller side.
#[derive(Debug, Default)]
pub struct UsageAccumulator {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub request_count: u32,
    pub estimated_cost_usd: f64,
    pub cost_status: CostStatus,
    /// Track per-model usage for the `model` column. We record the most-used
    /// model (by request count) when flushing.
    model_requests: std::collections::HashMap<String, u32>,
    provider: Option<String>,
}

impl UsageAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one API call's usage.
    pub fn add(&mut self, usage: ExtendedUsage, model: &str, provider: &str, cost: f64) {
        self.input_tokens += usage.input_tokens;
        self.output_tokens += usage.output_tokens;
        self.cache_read_tokens += usage.cache_read_tokens;
        self.cache_write_tokens += usage.cache_write_tokens;
        self.reasoning_tokens += usage.reasoning_tokens;
        self.request_count += 1;
        self.estimated_cost_usd += cost;

        // Determine cost status for this call.
        let call_status = if cost > 0.0 {
            CostStatus::Estimated
        } else {
            // Zero cost could mean included or unknown. We mark as estimated
            // when we have pricing info (cost == 0 is valid for zero-token calls).
            CostStatus::Estimated
        };
        self.cost_status = self.cost_status.most_conservative(call_status);

        *self.model_requests.entry(model.to_string()).or_insert(0) += 1;
        if self.provider.is_none() {
            self.provider = Some(provider.to_string());
        }
    }

    /// Returns true if any usage was recorded.
    pub fn has_usage(&self) -> bool {
        self.request_count > 0
    }

    /// The most-used model name across all calls.
    fn primary_model(&self) -> String {
        self.model_requests
            .iter()
            .max_by_key(|(_, count)| *count)
            .map(|(model, _)| model.clone())
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// Flush accumulated usage to the database.
    pub async fn flush(
        &self,
        pool: &SqlitePool,
        agent_id: &str,
        process_type: &str,
        conversation_id: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        if !self.has_usage() {
            return Ok(());
        }

        let model = self.primary_model();
        let provider = self.provider.as_deref().unwrap_or("unknown");
        let cost_status = self.cost_status.as_str();
        let cost = if self.cost_status == CostStatus::Unknown {
            None
        } else {
            Some(self.estimated_cost_usd)
        };

        sqlx::query(
            "INSERT INTO token_usage (
                agent_id, process_type, conversation_id, model, provider,
                input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                reasoning_tokens, request_count, estimated_cost_usd, cost_status
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(agent_id)
        .bind(process_type)
        .bind(conversation_id)
        .bind(&model)
        .bind(provider)
        .bind(self.input_tokens as i64)
        .bind(self.output_tokens as i64)
        .bind(self.cache_read_tokens as i64)
        .bind(self.cache_write_tokens as i64)
        .bind(self.reasoning_tokens as i64)
        .bind(self.request_count as i32)
        .bind(cost)
        .bind(cost_status)
        .execute(pool)
        .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accumulator_add() {
        let mut acc = UsageAccumulator::new();
        let usage = ExtendedUsage {
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_tokens: 200,
            cache_write_tokens: 100,
            reasoning_tokens: 50,
        };
        acc.add(usage, "claude-sonnet-4", "anthropic", 0.01);
        acc.add(usage, "claude-sonnet-4", "anthropic", 0.01);

        assert_eq!(acc.input_tokens, 2000);
        assert_eq!(acc.output_tokens, 1000);
        assert_eq!(acc.cache_read_tokens, 400);
        assert_eq!(acc.cache_write_tokens, 200);
        assert_eq!(acc.reasoning_tokens, 100);
        assert_eq!(acc.request_count, 2);
        assert!((acc.estimated_cost_usd - 0.02).abs() < 1e-10);
    }

    #[test]
    fn test_accumulator_no_usage_skips_flush() {
        let acc = UsageAccumulator::new();
        assert!(!acc.has_usage());
    }

    #[test]
    fn test_primary_model() {
        let mut acc = UsageAccumulator::new();
        let usage = ExtendedUsage::default();
        acc.add(usage, "claude-sonnet-4", "anthropic", 0.0);
        acc.add(usage, "claude-sonnet-4", "anthropic", 0.0);
        acc.add(usage, "gpt-4o", "openai", 0.0);
        assert_eq!(acc.primary_model(), "claude-sonnet-4");
    }

    #[test]
    fn test_cost_status_conservatism() {
        assert_eq!(
            CostStatus::Estimated.most_conservative(CostStatus::Unknown),
            CostStatus::Unknown
        );
        assert_eq!(
            CostStatus::Included.most_conservative(CostStatus::Estimated),
            CostStatus::Estimated
        );
        assert_eq!(
            CostStatus::Included.most_conservative(CostStatus::Included),
            CostStatus::Included
        );
    }

    #[test]
    fn test_anthropic_body_extraction() {
        let body = serde_json::json!({
            "usage": {
                "input_tokens": 1000,
                "output_tokens": 500,
                "cache_read_input_tokens": 200,
                "cache_creation_input_tokens": 100
            }
        });
        let usage = ExtendedUsage::from_anthropic_body(&body);
        assert_eq!(usage.input_tokens, 1000);
        assert_eq!(usage.output_tokens, 500);
        assert_eq!(usage.cache_read_tokens, 200);
        assert_eq!(usage.cache_write_tokens, 100);
    }

    #[test]
    fn test_openai_body_extraction() {
        let body = serde_json::json!({
            "usage": {
                "prompt_tokens": 1500,
                "completion_tokens": 500,
                "prompt_tokens_details": {
                    "cached_tokens": 300,
                    "cache_write_tokens": 200
                },
                "completion_tokens_details": {
                    "reasoning_tokens": 100
                }
            }
        });
        let usage = ExtendedUsage::from_openai_body(&body);
        assert_eq!(usage.input_tokens, 1000); // 1500 - 300 - 200
        assert_eq!(usage.output_tokens, 500);
        assert_eq!(usage.cache_read_tokens, 300);
        assert_eq!(usage.cache_write_tokens, 200);
        assert_eq!(usage.reasoning_tokens, 100);
    }
}
