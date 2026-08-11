//! Session chronicle inspection.
//!
//! Lets a process walk its own channel's chronology: list the checkpoint
//! index, open one checkpoint's summary, or expand a checkpoint's coverage
//! back into raw transcript.
//!
//! Scope and capability come from the constructor, never from tool arguments.
//! A channel is built a `Metadata` tool — bounded, already-summarized text —
//! while a branch gets `Expand` as well, keeping raw transcript out of the
//! channel's context. Because both are injected, a narrower tool can be handed
//! to a process without changing the argument surface or the prompt.

use crate::conversation::chronicle::{ChronicleCheckpoint, ChronicleStore};

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What a holder of the tool may do with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChronicleCapability {
    /// List the index and open checkpoint summaries.
    Metadata,
    /// Everything `Metadata` allows, plus expanding a checkpoint back into
    /// raw messages.
    Expand,
}

impl ChronicleCapability {
    fn allows_expand(self) -> bool {
        matches!(self, ChronicleCapability::Expand)
    }
}

/// Maximum checkpoints a single `list` returns.
const MAX_LIST_LIMIT: i64 = 50;
const DEFAULT_LIST_LIMIT: i64 = 20;

#[derive(Debug, Clone)]
pub struct ChronicleTool {
    store: ChronicleStore,
    /// The only channel this tool can read. Cross-channel recall is
    /// `channel_recall`'s job.
    channel_id: String,
    capability: ChronicleCapability,
    expand_limit: i64,
}

impl ChronicleTool {
    pub fn new(
        store: ChronicleStore,
        channel_id: impl Into<String>,
        capability: ChronicleCapability,
        expand_limit: i64,
    ) -> Self {
        Self {
            store,
            channel_id: channel_id.into(),
            capability,
            expand_limit,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Chronicle lookup failed: {0}")]
pub struct ChronicleError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ChronicleArgs {
    /// "list" (default), "open", or "expand".
    #[serde(default = "default_action")]
    pub action: String,
    /// Checkpoint sequence number. Required for "open" and "expand".
    #[serde(default)]
    pub checkpoint: Option<i64>,
    /// Maximum checkpoints to list, or maximum raw messages to expand.
    #[serde(default)]
    pub limit: Option<i64>,
    /// Cursor for "expand": continue after this message sequence number, as
    /// returned by the previous page.
    #[serde(default)]
    pub after: Option<i64>,
}

fn default_action() -> String {
    "list".to_string()
}

#[derive(Debug, Serialize)]
pub struct ChronicleOutput {
    pub action: String,
    pub channel_id: String,
    pub summary: String,
}

impl Tool for ChronicleTool {
    const NAME: &'static str = "chronicle";

    type Error = ChronicleError;
    type Args = ChronicleArgs;
    type Output = ChronicleOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let mut actions = vec!["list", "open"];
        if self.capability.allows_expand() {
            actions.push("expand");
        }

        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/chronicle").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": actions,
                        "default": "list",
                        "description": if self.capability.allows_expand() {
                            "\"list\" for the checkpoint index, \"open\" for one checkpoint's full summary, \"expand\" for the raw messages a checkpoint covers."
                        } else {
                            "\"list\" for the checkpoint index, \"open\" for one checkpoint's full summary. Expanding into raw transcript requires a branch."
                        }
                    },
                    "checkpoint": {
                        "type": "integer",
                        "description": "Checkpoint sequence number, as shown by \"list\". Required for \"open\" and \"expand\"."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "For \"list\": checkpoints to return (default 20, max 50). For \"expand\": raw messages per page, capped by the configured expand limit."
                    },
                    "after": {
                        "type": "integer",
                        "description": "Continue an expansion after this message sequence number, as returned by the previous page."
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        match args.action.as_str() {
            "list" => self.list(args.limit).await,
            "open" => self.open(args.checkpoint).await,
            "expand" => self.expand(args.checkpoint, args.limit, args.after).await,
            other => Err(ChronicleError(format!(
                "Unknown action \"{other}\". Use \"list\", \"open\"{}.",
                if self.capability.allows_expand() {
                    ", or \"expand\""
                } else {
                    ""
                }
            ))),
        }
    }
}

impl ChronicleTool {
    fn output(&self, action: &str, summary: String) -> ChronicleOutput {
        ChronicleOutput {
            action: action.to_string(),
            channel_id: self.channel_id.clone(),
            summary,
        }
    }

    async fn list(&self, limit: Option<i64>) -> Result<ChronicleOutput, ChronicleError> {
        let limit = limit.unwrap_or(DEFAULT_LIST_LIMIT).clamp(1, MAX_LIST_LIMIT);

        let stats =
            self.store.stats(&self.channel_id).await.map_err(|error| {
                ChronicleError(format!("Failed to read chronicle stats: {error}"))
            })?;

        // Every level, not just level 0: after a rollup absorbs a run of
        // checkpoints the rollup is the entry the agent should see first, and
        // the children remain reachable by opening it.
        let checkpoints = self
            .store
            .list_all_levels(&self.channel_id, limit)
            .await
            .map_err(|error| ChronicleError(format!("Failed to list checkpoints: {error}")))?;

        if checkpoints.is_empty() {
            return Ok(self.output(
                "list",
                "This channel has no chronicle checkpoints yet. Everything that has happened is \
                 still in raw context."
                    .to_string(),
            ));
        }

        // A rolled-up checkpoint is already represented by its rollup, so it is
        // hidden from the index. The rollup is the entry, and the children are
        // reachable through `open`.
        let visible: Vec<&ChronicleCheckpoint> = checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.rolled_up_into.is_none())
            .collect();

        let mut summary = format!(
            "## Session Chronicle — {} checkpoints, {} messages logged\n\n\
             Showing {} entries ({} most recent checkpoints requested). {} messages since the \
             last checkpoint are still in raw context.\n\n",
            stats.checkpoint_count,
            stats.total_messages,
            visible.len(),
            limit,
            stats.unsummarized_messages
        );

        for checkpoint in &visible {
            summary.push_str(&format!(
                "- **#{}** {}{} — {} → {} · {} messages · {}\n",
                checkpoint.seq,
                if checkpoint.level > 0 {
                    "[rollup] "
                } else {
                    ""
                },
                checkpoint.title,
                checkpoint.covers_from_at.format("%Y-%m-%d %H:%M"),
                checkpoint.covers_to_at.format("%Y-%m-%d %H:%M"),
                checkpoint.message_count,
                checkpoint.kind.as_str(),
            ));
        }

        summary.push_str("\nOpen one with `action: \"open\", checkpoint: <seq>`.");
        Ok(self.output("list", summary))
    }

    async fn open(&self, seq: Option<i64>) -> Result<ChronicleOutput, ChronicleError> {
        let checkpoint = self.require_checkpoint(seq, "open").await?;
        let mut summary = render_checkpoint(&checkpoint);

        // A rollup is only trustworthy if you can see what it stands for, so
        // opening one lists the checkpoints it covers. Those rows are never
        // deleted; each can still be opened or expanded on its own.
        if checkpoint.level > 0 {
            let children = self
                .store
                .children_of(&checkpoint.id)
                .await
                .map_err(|error| {
                    ChronicleError(format!("Failed to read rollup children: {error}"))
                })?;

            if children.is_empty() {
                summary.push_str("\n_This rollup has no recorded children._\n");
            } else {
                summary.push_str(&format!(
                    "\n### Covers {} checkpoint(s)\n\n",
                    children.len()
                ));
                for child in &children {
                    summary.push_str(&format!(
                        "- **#{}** {} — {} → {} · {} messages\n",
                        child.seq,
                        child.title,
                        child.covers_from_at.format("%Y-%m-%d %H:%M"),
                        child.covers_to_at.format("%Y-%m-%d %H:%M"),
                        child.message_count,
                    ));
                }
                summary.push_str(
                    "\nOpen any of them for its own summary, or expand it for raw messages.\n",
                );
            }
        }

        Ok(self.output("open", summary))
    }

    async fn expand(
        &self,
        seq: Option<i64>,
        limit: Option<i64>,
        after: Option<i64>,
    ) -> Result<ChronicleOutput, ChronicleError> {
        if !self.capability.allows_expand() {
            return Err(ChronicleError(
                "Expanding a checkpoint into raw transcript is not available here. Open the \
                 checkpoint summary instead, or branch to inspect the raw messages."
                    .to_string(),
            ));
        }

        let checkpoint = self.require_checkpoint(seq, "expand").await?;
        let limit = limit
            .unwrap_or(self.expand_limit)
            .clamp(1, self.expand_limit);

        let from = checkpoint.start_boundary();
        let to = checkpoint.end_boundary();

        let messages = self
            .store
            .messages_in_range(&self.channel_id, from, to, after, limit)
            .await
            .map_err(|error| ChronicleError(format!("Failed to expand checkpoint: {error}")))?;

        let start = after.unwrap_or(from.seq);
        let read_so_far = (start - from.seq).max(0) + messages.len() as i64;
        let mut summary = format!(
            "## Raw transcript for checkpoint #{} — {}\n\n{} → {} · showing {} of {} covered \
             messages\n\n",
            checkpoint.seq,
            checkpoint.title,
            checkpoint.covers_from_at.format("%Y-%m-%d %H:%M"),
            checkpoint.covers_to_at.format("%Y-%m-%d %H:%M"),
            messages.len(),
            checkpoint.message_count,
        );
        summary.push_str(&crate::agent::chronicle::render_log_transcript(&messages));

        // A checkpoint may cover more messages than one page returns. Hand back
        // the cursor to continue from rather than suggesting a larger limit
        // that would just be clamped again.
        match messages.last().and_then(|message| message.seq) {
            Some(cursor) if cursor < to.seq => {
                let remaining = checkpoint.message_count - read_so_far;
                summary.push_str(&format!(
                    "\n[{} more message(s) in this span. Continue with \
                     `action: \"expand\", checkpoint: {}, after: {cursor}`.]\n",
                    remaining.max(0),
                    checkpoint.seq,
                ));
            }
            _ => {}
        }

        Ok(self.output("expand", summary))
    }

    async fn require_checkpoint(
        &self,
        seq: Option<i64>,
        action: &str,
    ) -> Result<ChronicleCheckpoint, ChronicleError> {
        let Some(seq) = seq else {
            return Err(ChronicleError(format!(
                "\"{action}\" needs a `checkpoint` sequence number. Run `action: \"list\"` first \
                 to see which checkpoints exist."
            )));
        };

        self.store
            .get_by_seq(&self.channel_id, seq)
            .await
            .map_err(|error| ChronicleError(format!("Failed to read checkpoint: {error}")))?
            .ok_or_else(|| ChronicleError(format!("No checkpoint #{seq} in this channel.")))
    }
}

fn render_checkpoint(checkpoint: &ChronicleCheckpoint) -> String {
    let mut output = format!(
        "## Checkpoint #{} — {}\n\n**Covers:** {} → {} ({} messages)\n**Kind:** {}\n",
        checkpoint.seq,
        checkpoint.title,
        checkpoint.covers_from_at.format("%Y-%m-%d %H:%M"),
        checkpoint.covers_to_at.format("%Y-%m-%d %H:%M"),
        checkpoint.message_count,
        checkpoint.kind.as_str(),
    );

    if checkpoint.rolled_up_into.is_some() {
        output.push_str("**Rolled up:** covered by a higher-level summary\n");
    }

    output.push_str(&format!("\n{}\n", checkpoint.summary));
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::chronicle::{CheckpointKind, ChronicleBoundary, NewCheckpoint};
    use chrono::{DateTime, Utc};

    async fn store_with_checkpoint() -> ChronicleStore {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("pool");
        sqlx::raw_sql(include_str!(
            "../../migrations/20260211000002_conversations.sql"
        ))
        .execute(&pool)
        .await
        .expect("conversations migration");
        sqlx::raw_sql(include_str!(
            "../../migrations/20260809000004_session_chronicles.sql"
        ))
        .execute(&pool)
        .await
        .expect("chronicle migration");
        sqlx::raw_sql(include_str!(
            "../../migrations/20260810000001_conversation_message_seq.sql"
        ))
        .execute(&pool)
        .await
        .expect("seq migration");

        for (index, id) in ["m1", "m2", "m3"].iter().enumerate() {
            sqlx::query(
                "INSERT INTO conversation_messages (id, channel_id, role, content, created_at, seq) \
                 VALUES (?, 'ch', 'user', 'hello', ?, \
                    COALESCE((SELECT MAX(seq) FROM conversation_messages WHERE channel_id = 'ch'), 0) + 1)",
            )
            .bind(id)
            .bind(format!("2026-08-01 00:00:0{index}"))
            .execute(&pool)
            .await
            .expect("insert");
        }

        let store = ChronicleStore::new(pool);
        let at = |value: &str| {
            DateTime::parse_from_rfc3339(value)
                .unwrap()
                .with_timezone(&Utc)
        };
        store
            .commit(NewCheckpoint {
                channel_id: "ch".into(),
                level: 0,
                kind: CheckpointKind::Interval,
                title: "First span".into(),
                summary: "They said hello three times.".into(),
                covers_from: ChronicleBoundary::origin(),
                covers_to: ChronicleBoundary::new(3),
                covers_from_at: at("2026-08-01T00:00:00Z"),
                covers_to_at: at("2026-08-01T00:00:02Z"),
                covers_from_message_id: None,
                covers_to_message_id: Some("m3".into()),
                message_count: 3,
                token_estimate: 5,
                rolls_up_from_seq: None,
                rolls_up_to_seq: None,
                model: None,
            })
            .await
            .expect("commit");

        store
    }

    fn tool(store: ChronicleStore, capability: ChronicleCapability) -> ChronicleTool {
        ChronicleTool::new(store, "ch", capability, 100)
    }

    #[tokio::test]
    async fn list_reports_the_index() {
        let tool = tool(store_with_checkpoint().await, ChronicleCapability::Metadata);
        let output = tool.list(None).await.expect("list");
        assert!(output.summary.contains("#1"));
        assert!(output.summary.contains("First span"));
        assert!(output.summary.contains("1 checkpoints"));
    }

    #[tokio::test]
    async fn open_returns_the_full_summary() {
        let tool = tool(store_with_checkpoint().await, ChronicleCapability::Metadata);
        let output = tool.open(Some(1)).await.expect("open");
        assert!(output.summary.contains("They said hello three times."));
    }

    #[tokio::test]
    async fn metadata_capability_refuses_expand() {
        let tool = tool(store_with_checkpoint().await, ChronicleCapability::Metadata);
        let error = tool
            .expand(Some(1), None, None)
            .await
            .expect_err("must refuse");
        assert!(error.to_string().contains("not available here"));
    }

    #[tokio::test]
    async fn expand_capability_returns_raw_messages() {
        let tool = tool(store_with_checkpoint().await, ChronicleCapability::Expand);
        let output = tool.expand(Some(1), None, None).await.expect("expand");
        assert!(output.summary.contains("Raw transcript for checkpoint #1"));
        assert_eq!(
            output.summary.matches("hello").count(),
            3,
            "every covered message is expanded"
        );
    }

    #[tokio::test]
    async fn expand_pages_forward_with_a_cursor_instead_of_a_bigger_limit() {
        let tool = ChronicleTool::new(
            store_with_checkpoint().await,
            "ch",
            ChronicleCapability::Expand,
            2,
        );

        let first = tool.expand(Some(1), None, None).await.expect("expand");
        assert_eq!(first.summary.matches("hello").count(), 2);
        assert!(
            first.summary.contains("after: 2"),
            "a partial page hands back the cursor to continue from: {}",
            first.summary
        );

        // Continuing from the cursor reaches the rest of the span, which a
        // clamped `limit` never could.
        let second = tool.expand(Some(1), None, Some(2)).await.expect("expand");
        assert_eq!(second.summary.matches("hello").count(), 1);
        assert!(
            !second.summary.contains("more message(s)"),
            "the final page is not advertised as partial"
        );
    }

    #[tokio::test]
    async fn actions_are_scoped_to_the_constructed_channel() {
        // A checkpoint exists under "ch"; a tool built for another channel
        // cannot reach it, and no argument can widen the scope.
        let tool = ChronicleTool::new(
            store_with_checkpoint().await,
            "other-channel",
            ChronicleCapability::Expand,
            100,
        );
        assert!(tool.open(Some(1)).await.is_err());
        assert!(tool.expand(Some(1), None, None).await.is_err());
        let listed = tool.list(None).await.expect("list");
        assert!(listed.summary.contains("no chronicle checkpoints yet"));
    }

    #[tokio::test]
    async fn open_without_a_checkpoint_explains_how_to_find_one() {
        let tool = tool(store_with_checkpoint().await, ChronicleCapability::Metadata);
        let error = tool.open(None).await.expect_err("must ask for a seq");
        assert!(error.to_string().contains("list"));
    }

    #[tokio::test]
    async fn definition_hides_expand_without_the_capability() {
        let store = store_with_checkpoint().await;
        let metadata = tool(store.clone(), ChronicleCapability::Metadata)
            .definition(String::new())
            .await;
        let actions = metadata.parameters["properties"]["action"]["enum"]
            .as_array()
            .expect("enum")
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>();
        assert_eq!(actions, vec!["list", "open"]);

        let expand = tool(store, ChronicleCapability::Expand)
            .definition(String::new())
            .await;
        assert!(
            expand.parameters["properties"]["action"]["enum"]
                .as_array()
                .expect("enum")
                .iter()
                .any(|value| value.as_str() == Some("expand"))
        );
    }
}
