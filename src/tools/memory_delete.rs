//! Memory delete tool for branches.
//!
//! Soft-deletes a memory by setting its `forgotten` flag. The memory stays in
//! the database but is excluded from all search and recall operations.

use crate::config::RuntimeConfig;
use crate::memory::Memory;
use crate::memory::MemorySearch;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Tool for soft-deleting memories.
#[derive(Debug, Clone)]
pub struct MemoryDeleteTool {
    memory_search: Arc<MemorySearch>,
    runtime_config: Option<Arc<RuntimeConfig>>,
}

impl MemoryDeleteTool {
    /// Create a new memory delete tool.
    pub fn new(memory_search: Arc<MemorySearch>) -> Self {
        Self {
            memory_search,
            runtime_config: None,
        }
    }

    /// Create a memory delete tool with runtime warm-recall cache support.
    pub fn with_runtime(
        memory_search: Arc<MemorySearch>,
        runtime_config: Arc<RuntimeConfig>,
    ) -> Self {
        Self {
            memory_search,
            runtime_config: Some(runtime_config),
        }
    }

    fn evict_from_warm_cache(runtime_config: &RuntimeConfig, memory_id: &str) -> bool {
        let current_memories = runtime_config.warm_recall_memories.load();
        let refreshed_at_unix_ms = *runtime_config
            .warm_recall_refreshed_at_unix_ms
            .load()
            .as_ref();
        let (updated_memories, refreshed_at_unix_ms, removed) = remove_warm_cache_memory_by_id(
            current_memories.as_ref(),
            memory_id,
            refreshed_at_unix_ms,
        );
        if !removed {
            return false;
        }

        runtime_config
            .warm_recall_memories
            .store(Arc::new(updated_memories));
        // Eviction is a partial mutation, not a full refresh. Keep the existing timestamp.
        runtime_config
            .warm_recall_refreshed_at_unix_ms
            .store(Arc::new(refreshed_at_unix_ms));
        true
    }
}

struct InflightForgetGuard {
    counts: Arc<std::sync::Mutex<BTreeMap<String, usize>>>,
    memory_id: String,
}

impl InflightForgetGuard {
    fn new(runtime_config: &RuntimeConfig, memory_id: &str) -> Self {
        Self::from_counts(
            Arc::clone(&runtime_config.warm_recall_inflight_forget_counts),
            memory_id,
        )
    }

    fn from_counts(
        counts: Arc<std::sync::Mutex<BTreeMap<String, usize>>>,
        memory_id: &str,
    ) -> Self {
        {
            let mut counts = lock_inflight_counts(&counts);
            *counts.entry(memory_id.to_string()).or_insert(0) += 1;
        }

        Self {
            counts,
            memory_id: memory_id.to_string(),
        }
    }
}

impl Drop for InflightForgetGuard {
    fn drop(&mut self) {
        let mut counts = lock_inflight_counts(&self.counts);
        if let Some(count) = counts.get_mut(&self.memory_id) {
            if *count <= 1 {
                counts.remove(&self.memory_id);
            } else {
                *count -= 1;
            }
        }
    }
}

fn lock_inflight_counts(
    counts: &std::sync::Mutex<BTreeMap<String, usize>>,
) -> std::sync::MutexGuard<'_, BTreeMap<String, usize>> {
    match counts.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!("warm recall inflight forget counts lock poisoned; recovering state");
            poisoned.into_inner()
        }
    }
}

fn remove_warm_cache_memory_by_id(
    memories: &[Memory],
    memory_id: &str,
    refreshed_at_unix_ms: Option<i64>,
) -> (Vec<Memory>, Option<i64>, bool) {
    let mut removed = false;
    let mut updated = Vec::with_capacity(memories.len());
    for memory in memories {
        if memory.id == memory_id {
            removed = true;
            continue;
        }
        updated.push(memory.clone());
    }
    (updated, refreshed_at_unix_ms, removed)
}

/// Error type for memory delete tool.
#[derive(Debug, thiserror::Error)]
#[error("Memory delete failed: {0}")]
pub struct MemoryDeleteError(String);

/// Arguments for memory delete tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemoryDeleteArgs {
    /// The ID of the memory to forget.
    pub memory_id: String,
    /// Brief reason for forgetting this memory (for audit purposes).
    pub reason: Option<String>,
}

/// Output from memory delete tool.
#[derive(Debug, Serialize)]
pub struct MemoryDeleteOutput {
    /// Whether the memory was found and forgotten.
    pub forgotten: bool,
    /// Description of what happened.
    pub message: String,
}

impl Tool for MemoryDeleteTool {
    const NAME: &'static str = "memory_delete";

    type Error = MemoryDeleteError;
    type Args = MemoryDeleteArgs;
    type Output = MemoryDeleteOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/memory_delete").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "memory_id": {
                        "type": "string",
                        "description": "The ID of the memory to forget (from memory_recall results)"
                    },
                    "reason": {
                        "type": "string",
                        "description": "Optional reason for forgetting this memory"
                    }
                },
                "required": ["memory_id"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> std::result::Result<Self::Output, Self::Error> {
        let store = self.memory_search.store();

        // Verify the memory exists first
        let memory = store
            .load(&args.memory_id)
            .await
            .map_err(|e| MemoryDeleteError(format!("Failed to look up memory: {e}")))?;

        let Some(memory) = memory else {
            return Ok(MemoryDeleteOutput {
                forgotten: false,
                message: format!("No memory found with ID: {}", args.memory_id),
            });
        };

        if memory.forgotten {
            let removed_from_warm_cache = if let Some(runtime_config) = self.runtime_config.as_ref()
            {
                let _warm_recall_cache_guard = runtime_config.warm_recall_cache_lock.lock().await;
                let removed =
                    MemoryDeleteTool::evict_from_warm_cache(runtime_config, &args.memory_id);
                if removed {
                    runtime_config
                        .warm_recall_cache_epoch
                        .fetch_add(1, Ordering::SeqCst);
                }
                removed
            } else {
                false
            };
            return Ok(MemoryDeleteOutput {
                forgotten: false,
                message: format!(
                    "Memory {} is already forgotten. Warm cache evicted: {}.",
                    args.memory_id, removed_from_warm_cache
                ),
            });
        }

        let reason_suffix = args
            .reason
            .as_deref()
            .map(|r| format!(" Reason: {r}"))
            .unwrap_or_default();

        let (was_forgotten, removed_from_warm_cache) =
            if let Some(runtime_config) = self.runtime_config.as_ref() {
                let runtime_config = Arc::clone(runtime_config);
                let memory_search = Arc::clone(&self.memory_search);
                let memory_id = args.memory_id.clone();
                let inflight_forget_guard = InflightForgetGuard::new(&runtime_config, &memory_id);
                let forget_and_evict_task = tokio::spawn(async move {
                    let store = memory_search.store();
                    let was_forgotten = store.forget(&memory_id).await.map_err(|error| {
                        MemoryDeleteError(format!("Failed to forget memory: {error}"))
                    })?;

                    let mut removed_from_warm_cache = false;
                    if was_forgotten {
                        let _warm_recall_cache_guard =
                            runtime_config.warm_recall_cache_lock.lock().await;
                        removed_from_warm_cache =
                            MemoryDeleteTool::evict_from_warm_cache(&runtime_config, &memory_id);
                        runtime_config
                            .warm_recall_cache_epoch
                            .fetch_add(1, Ordering::SeqCst);
                    } else {
                        drop(inflight_forget_guard);
                    }

                    Ok::<(bool, bool), MemoryDeleteError>((was_forgotten, removed_from_warm_cache))
                });

                forget_and_evict_task.await.map_err(|error| {
                    MemoryDeleteError(format!("Forget and eviction task failed: {error}"))
                })??
            } else {
                let was_forgotten = store
                    .forget(&args.memory_id)
                    .await
                    .map_err(|e| MemoryDeleteError(format!("Failed to forget memory: {e}")))?;
                (was_forgotten, false)
            };

        if was_forgotten {
            #[cfg(feature = "metrics")]
            crate::telemetry::Metrics::global()
                .memory_updates_total
                .with_label_values(&["unknown", "forget"])
                .inc();

            tracing::info!(
                memory_id = %args.memory_id,
                memory_type = %memory.memory_type,
                reason = ?args.reason,
                removed_from_warm_cache,
                "memory forgotten"
            );

            let preview = memory.content.lines().next().unwrap_or("(empty)");
            Ok(MemoryDeleteOutput {
                forgotten: true,
                message: format!(
                    "Forgotten [{type}] memory: \"{preview}\".{reason_suffix}",
                    type = memory.memory_type,
                    preview = truncate(preview, 80),
                ),
            })
        } else {
            Ok(MemoryDeleteOutput {
                forgotten: false,
                message: format!("Failed to forget memory {}.", args.memory_id),
            })
        }
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..s.floor_char_boundary(max)]
    }
}

#[cfg(test)]
mod tests {
    use super::{InflightForgetGuard, lock_inflight_counts, remove_warm_cache_memory_by_id};
    use crate::memory::{Memory, MemoryType};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    #[test]
    fn remove_warm_cache_memory_by_id_removes_matching_memory() {
        let keep = Memory::new("keep", MemoryType::Fact);
        let remove = Memory::new("remove", MemoryType::Fact);
        let refreshed_at_unix_ms = Some(11_000);

        let (updated, updated_refreshed_at_unix_ms, removed_flag) = remove_warm_cache_memory_by_id(
            &[keep.clone(), remove.clone()],
            &remove.id,
            refreshed_at_unix_ms,
        );

        assert!(removed_flag);
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].id, keep.id);
        assert_eq!(updated_refreshed_at_unix_ms, refreshed_at_unix_ms);
    }

    #[test]
    fn remove_warm_cache_memory_by_id_keeps_cache_when_id_missing() {
        let memory = Memory::new("keep", MemoryType::Fact);
        let refreshed_at_unix_ms = Some(22_000);

        let (updated, updated_refreshed_at_unix_ms, removed_flag) = remove_warm_cache_memory_by_id(
            std::slice::from_ref(&memory),
            "missing-id",
            refreshed_at_unix_ms,
        );

        assert!(!removed_flag);
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].id, memory.id);
        assert_eq!(updated_refreshed_at_unix_ms, refreshed_at_unix_ms);
    }

    #[test]
    fn remove_warm_cache_memory_by_id_compose_evictions_without_reintroducing() {
        let first = Memory::new("first", MemoryType::Fact);
        let second = Memory::new("second", MemoryType::Fact);
        let third = Memory::new("third", MemoryType::Fact);
        let refreshed_at_unix_ms = Some(33_000);

        let (after_first, after_first_refreshed_at_unix_ms, removed_first) =
            remove_warm_cache_memory_by_id(
                &[first.clone(), second.clone(), third.clone()],
                &first.id,
                refreshed_at_unix_ms,
            );
        let (after_second, after_second_refreshed_at_unix_ms, removed_second) =
            remove_warm_cache_memory_by_id(
                &after_first,
                &second.id,
                after_first_refreshed_at_unix_ms,
            );

        assert!(removed_first);
        assert!(removed_second);
        assert_eq!(after_second.len(), 1);
        assert_eq!(after_second[0].id, third.id);
        assert_eq!(after_second_refreshed_at_unix_ms, refreshed_at_unix_ms);
    }

    #[test]
    fn inflight_forget_guard_refcounts_same_memory_id() {
        let counts = Arc::new(Mutex::new(BTreeMap::new()));
        let guard_one = InflightForgetGuard::from_counts(Arc::clone(&counts), "memory-1");
        let guard_two = InflightForgetGuard::from_counts(Arc::clone(&counts), "memory-1");

        assert_eq!(
            lock_inflight_counts(&counts).get("memory-1").copied(),
            Some(2)
        );

        drop(guard_one);
        assert_eq!(
            lock_inflight_counts(&counts).get("memory-1").copied(),
            Some(1)
        );

        drop(guard_two);
        assert!(lock_inflight_counts(&counts).get("memory-1").is_none());
    }

    #[test]
    fn inflight_forget_guard_tracks_multiple_ids_independently() {
        let counts = Arc::new(Mutex::new(BTreeMap::new()));
        let guard_one = InflightForgetGuard::from_counts(Arc::clone(&counts), "memory-1");
        let guard_two = InflightForgetGuard::from_counts(Arc::clone(&counts), "memory-2");

        let snapshot = lock_inflight_counts(&counts).clone();
        assert_eq!(snapshot.get("memory-1"), Some(&1));
        assert_eq!(snapshot.get("memory-2"), Some(&1));

        drop(guard_one);
        assert!(lock_inflight_counts(&counts).get("memory-1").is_none());
        assert_eq!(
            lock_inflight_counts(&counts).get("memory-2").copied(),
            Some(1)
        );

        drop(guard_two);
        assert!(lock_inflight_counts(&counts).is_empty());
    }
}
