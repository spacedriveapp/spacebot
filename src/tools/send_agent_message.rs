//! Assign a task to another agent through the communication graph.
//!
//! When called, creates a task in the target agent's task store (skipping
//! `pending_approval` for agent-delegated tasks) and logs a system message
//! in the link channel between the two agents. The calling agent's turn ends
//! immediately — the result will be delivered when the target agent's cortex
//! picks up and completes the task.

use crate::conversation::history::ConversationLogger;
use crate::links::AgentLink;
use crate::tasks::TaskStore;
use crate::tools::SkipFlag;

use arc_swap::ArcSwap;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Tool for delegating tasks to other agents through the agent communication graph.
///
/// Resolves the target agent by ID or name, validates the link exists and permits
/// this direction, creates a task in the target agent's task store, and logs the
/// delegation in the link channel. The calling agent's turn ends after delegation.
#[derive(Clone)]
pub struct SendAgentMessageTool {
    agent_id: crate::AgentId,
    links: Arc<ArcSwap<Vec<AgentLink>>>,
    /// Map of known agent IDs to display names, for resolving targets.
    agent_names: Arc<HashMap<String, String>>,
    /// Global task store shared across all agents.
    task_store: Arc<TaskStore>,
    /// Per-agent conversation logger for writing link channel audit records.
    conversation_logger: ConversationLogger,
    /// Per-turn skip flag. When set after delegation, the channel turn ends immediately.
    skip_flag: Option<SkipFlag>,
    /// The originating channel (conversation_id) where the user request came from.
    /// Set per-turn so task completion notifications route back to the right place.
    originating_channel: Option<String>,
    working_memory: Option<Arc<crate::memory::WorkingMemoryStore>>,
}

impl std::fmt::Debug for SendAgentMessageTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendAgentMessageTool")
            .field("agent_id", &self.agent_id)
            .finish_non_exhaustive()
    }
}

impl SendAgentMessageTool {
    pub fn new(
        agent_id: crate::AgentId,
        links: Arc<ArcSwap<Vec<AgentLink>>>,
        agent_names: Arc<HashMap<String, String>>,
        task_store: Arc<TaskStore>,
        conversation_logger: ConversationLogger,
    ) -> Self {
        Self {
            agent_id,
            links,
            agent_names,
            task_store,
            conversation_logger,
            skip_flag: None,
            originating_channel: None,
            working_memory: None,
        }
    }

    /// Set the per-turn skip flag so the channel turn ends after delegation.
    pub fn with_skip_flag(mut self, flag: SkipFlag) -> Self {
        self.skip_flag = Some(flag);
        self
    }

    /// Set the originating channel for this turn so task completion notifications
    /// route back to the conversation where the user asked for the work.
    pub fn with_originating_channel(mut self, channel_id: String) -> Self {
        self.originating_channel = Some(channel_id);
        self
    }

    pub fn with_working_memory(mut self, store: Arc<crate::memory::WorkingMemoryStore>) -> Self {
        self.working_memory = Some(store);
        self
    }

    /// Resolve an agent target string to an agent ID.
    /// Checks both IDs and display names (case-insensitive).
    fn resolve_agent_id(&self, target: &str) -> Option<String> {
        // Direct ID match
        if self.agent_names.contains_key(target) {
            return Some(target.to_string());
        }

        // Name match (case-insensitive)
        let target_lower = target.to_lowercase();
        for (agent_id, name) in self.agent_names.iter() {
            if name.to_lowercase() == target_lower {
                return Some(agent_id.clone());
            }
        }

        None
    }
}

/// Error type for send_agent_message tool.
#[derive(Debug, thiserror::Error)]
#[error("SendAgentMessage failed: {0}")]
pub struct SendAgentMessageError(String);

/// Arguments for send_agent_message tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SendAgentMessageArgs {
    /// Target agent ID or name.
    pub target: String,
    /// The task to assign. First sentence is used as the task title;
    /// full content becomes the task description.
    pub message: String,
}

/// Output from send_agent_message tool.
#[derive(Debug, Serialize)]
pub struct SendAgentMessageOutput {
    pub success: bool,
    pub target_agent: String,
    pub task_number: Option<i64>,
    pub message: String,
}

impl Tool for SendAgentMessageTool {
    const NAME: &'static str = "send_agent_message";

    type Error = SendAgentMessageError;
    type Args = SendAgentMessageArgs;
    type Output = SendAgentMessageOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/send_agent_message").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "The target agent's ID or name."
                    },
                    "message": {
                        "type": "string",
                        "description": "The task to assign. First sentence becomes the title; full content is the description."
                    }
                },
                "required": ["target", "message"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        tracing::info!(
            from = %self.agent_id,
            target = %args.target,
            message_len = args.message.len(),
            "send_agent_message tool called"
        );

        // Resolve target agent ID (could be name or ID)
        let target_agent_id = self.resolve_agent_id(&args.target).ok_or_else(|| {
            SendAgentMessageError(format!(
                "unknown agent '{}'. Check your organization context for available agents.",
                args.target
            ))
        })?;

        // Look up the link between sending agent and target
        let links = self.links.load();
        let link = crate::links::find_link_between(&links, &self.agent_id, &target_agent_id)
            .ok_or_else(|| {
                SendAgentMessageError(format!(
                    "no communication link exists between you and agent '{}'.",
                    args.target
                ))
            })?;

        // Check direction: if the link is one_way, only from_agent can initiate
        let sending_agent_id = self.agent_id.as_ref();
        let is_to_agent = link.to_agent_id == sending_agent_id;

        if link.direction == crate::links::LinkDirection::OneWay && is_to_agent {
            return Err(SendAgentMessageError(format!(
                "the link to agent '{}' is one-way and you cannot initiate messages.",
                args.target
            )));
        }

        let receiving_agent_id = if link.from_agent_id == sending_agent_id {
            &link.to_agent_id
        } else {
            &link.from_agent_id
        };

        let target_display = self
            .agent_names
            .get(receiving_agent_id)
            .cloned()
            .unwrap_or_else(|| receiving_agent_id.to_string());

        // Extract title from the message: first sentence or first 120 chars.
        let title = extract_task_title(&args.message);

        // Build task metadata with delegation context.
        let metadata = serde_json::json!({
            "delegated_by": sending_agent_id,
            "delegating_agent_id": sending_agent_id,
            "originating_channel": self.originating_channel,
        });

        // Create the task in the global store with cross-agent assignment.
        // Agent-delegated tasks skip pending_approval and go straight to ready.
        let task = self
            .task_store
            .create(crate::tasks::CreateTaskInput {
                owner_agent_id: sending_agent_id.to_string(),
                assigned_agent_id: receiving_agent_id.to_string(),
                title: title.clone(),
                description: Some(args.message.clone()),
                status: crate::tasks::TaskStatus::Ready,
                priority: crate::tasks::TaskPriority::Medium,
                subtasks: Vec::new(),
                metadata,
                source_memory_id: None,
                created_by: format!("agent:{}", sending_agent_id),
            })
            .await
            .map_err(|error| {
                SendAgentMessageError(format!(
                    "failed to create task on agent '{}': {error}",
                    target_display
                ))
            })?;

        let task_number = task.task_number;

        // Log delegation record in the link channel (system message).
        let sender_display = self
            .agent_names
            .get(sending_agent_id)
            .cloned()
            .unwrap_or_else(|| sending_agent_id.to_string());
        let link_channel_id = link.channel_id_for(sending_agent_id);

        self.conversation_logger.log_system_message(
            &link_channel_id,
            &format!(
                "{sender_display} assigned task #{task_number} to {target_display}: \"{title}\""
            ),
        );

        // Also log to the receiver's side of the link channel.
        let receiver_link_channel_id = link.channel_id_for(receiving_agent_id);
        self.conversation_logger.log_system_message(
            &receiver_link_channel_id,
            &format!(
                "{sender_display} assigned task #{task_number} to {target_display}: \"{title}\""
            ),
        );

        // End the current turn immediately after delegation.
        if let Some(ref flag) = self.skip_flag {
            flag.store(true, Ordering::Relaxed);
        }

        tracing::info!(
            from = %self.agent_id,
            to = %receiving_agent_id,
            task_number,
            "task delegated to target agent"
        );

        if let Some(working_memory) = &self.working_memory {
            working_memory
                .emit(
                    crate::memory::WorkingMemoryEventType::Outcome,
                    format!("Delegated task #{task_number} to {target_display}"),
                )
                .importance(0.7)
                .record();
        }

        Ok(SendAgentMessageOutput {
            success: true,
            target_agent: target_display,
            task_number: Some(task_number),
            message: format!(
                "Task #{task_number} assigned. The target agent's cortex will pick it up and execute it autonomously. \
                 You will be notified when it completes."
            ),
        })
    }
}

/// Extract a task title from the message content.
/// Uses the first sentence (up to first `.`, `!`, or `?`) or truncates at 120 chars.
fn extract_task_title(message: &str) -> String {
    let first_line = message.lines().next().unwrap_or(message);

    // Find the first sentence-ending punctuation
    if let Some(position) = first_line.find(['.', '!', '?']) {
        let title = &first_line[..=position];
        if title.len() <= 120 {
            return title.trim().to_string();
        }
    }

    // Fall back to first 120 chars
    if first_line.len() <= 120 {
        first_line.trim().to_string()
    } else {
        let boundary = first_line.floor_char_boundary(120);
        format!("{}...", first_line[..boundary].trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::links::{AgentLink, LinkDirection, LinkKind};
    use crate::memory::working::WorkingMemoryEvent;
    use crate::memory::{WorkingMemoryEventType, WorkingMemoryStore};
    use arc_swap::ArcSwap;
    use chrono_tz::Tz;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::collections::HashMap;
    use std::time::Duration;

    async fn wait_for_single_event(store: &WorkingMemoryStore) -> WorkingMemoryEvent {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let events = store
                    .get_recent_events(10, 0.0)
                    .await
                    .expect("working memory query");
                if let Some(event) = events.into_iter().next() {
                    break event;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed out waiting for working memory event")
    }

    #[tokio::test]
    async fn send_agent_message_emits_outcome_event() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("sqlite connect");
        sqlx::query(
            r#"
            CREATE TABLE tasks (
                id TEXT PRIMARY KEY,
                task_number INTEGER NOT NULL UNIQUE,
                title TEXT NOT NULL,
                description TEXT,
                status TEXT NOT NULL,
                priority TEXT NOT NULL,
                owner_agent_id TEXT NOT NULL,
                assigned_agent_id TEXT NOT NULL,
                subtasks TEXT NOT NULL,
                metadata TEXT NOT NULL DEFAULT '{}',
                source_memory_id TEXT,
                worker_id TEXT,
                created_by TEXT NOT NULL,
                approved_at TEXT,
                approved_by TEXT,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                completed_at TEXT
            )
            "#,
        )
        .execute(&pool)
        .await
        .expect("tasks schema should be created");
        sqlx::query(
            "CREATE TABLE task_number_seq (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                next_number INTEGER NOT NULL DEFAULT 1
            )",
        )
        .execute(&pool)
        .await
        .expect("task_number_seq should be created");
        sqlx::query(
            "CREATE TABLE conversation_messages (
                id TEXT PRIMARY KEY,
                channel_id TEXT NOT NULL,
                role TEXT NOT NULL,
                sender_name TEXT,
                sender_id TEXT,
                content TEXT NOT NULL,
                metadata TEXT,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
            )",
        )
        .execute(&pool)
        .await
        .expect("conversation messages schema should be created");
        sqlx::query("INSERT INTO task_number_seq (id, next_number) VALUES (1, 1)")
            .execute(&pool)
            .await
            .expect("sequence seed should be inserted");

        let task_store = Arc::new(TaskStore::new(pool.clone()));
        let conversation_logger = ConversationLogger::new(pool.clone());
        let working_memory_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("sqlite connect");
        sqlx::migrate!("./migrations")
            .run(&working_memory_pool)
            .await
            .expect("working memory migrations");
        let working_memory = WorkingMemoryStore::new(working_memory_pool, Tz::UTC);

        let links = Arc::new(ArcSwap::from_pointee(vec![AgentLink {
            from_agent_id: "planner".to_string(),
            to_agent_id: "executor".to_string(),
            direction: LinkDirection::TwoWay,
            kind: LinkKind::Peer,
        }]));
        let agent_names = Arc::new(HashMap::from([
            ("planner".to_string(), "Planner".to_string()),
            ("executor".to_string(), "Executor".to_string()),
        ]));

        let tool = SendAgentMessageTool::new(
            crate::AgentId::from("planner"),
            links,
            agent_names,
            task_store,
            conversation_logger,
        )
        .with_working_memory(working_memory.clone());

        let output = tool
            .call(SendAgentMessageArgs {
                target: "executor".to_string(),
                message: "Implement the working-memory renderer. Include tests.".to_string(),
            })
            .await
            .expect("send agent message should succeed");

        assert!(output.success);

        let event = wait_for_single_event(&working_memory).await;
        assert_eq!(event.event_type, WorkingMemoryEventType::Outcome);
        assert_eq!(event.summary, "Delegated task #1 to Executor");
    }
}
