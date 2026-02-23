//! Evidence store for tool outputs, diffs, test results, and error traces.
//!
//! Evidence records are time-bounded snapshots of raw tool activity attached
//! to learning episodes. Retention is type-driven: `UserFlagged` evidence is
//! permanent, `Deploy` evidence lives for 30 days, everything else expires
//! sooner. Cleanup skips records that belong to episodes still in progress.

use crate::learning::{EvidenceType, LearningStore};

use anyhow::{Context as _, Result};
use chrono::Utc;
use uuid::Uuid;

use std::sync::Arc;

// ---------------------------------------------------------------------------
// EvidenceRecord
// ---------------------------------------------------------------------------

/// A single row from the evidence table, returned by query methods.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EvidenceRecord {
    pub id: String,
    pub evidence_type: String,
    pub content: String,
    pub episode_id: Option<String>,
    pub tool_name: Option<String>,
    pub compressed: bool,
    pub created_at: String,
}

// ---------------------------------------------------------------------------
// EvidenceStore
// ---------------------------------------------------------------------------

/// Writes, retrieves, and expires evidence records in learning.db.
#[derive(Debug, Clone)]
pub struct EvidenceStore {
    store: Arc<LearningStore>,
}

impl EvidenceStore {
    pub fn new(store: Arc<LearningStore>) -> Self {
        Self { store }
    }

    /// Store a single piece of evidence and return its generated ID.
    ///
    /// `retention_hours == 0` (e.g. `UserFlagged`) means the record never
    /// expires; the sentinel value `9999-12-31` is used so the cleanup query
    /// always works uniformly.
    pub async fn store_evidence(
        &self,
        evidence_type: &EvidenceType,
        content: &str,
        episode_id: Option<&str>,
        tool_name: Option<&str>,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let retention_hours = evidence_type.retention_hours();

        let expires_at = if retention_hours == 0 {
            "9999-12-31".to_string()
        } else {
            let expiry = Utc::now() + chrono::Duration::hours(retention_hours);
            expiry.format("%Y-%m-%d %H:%M:%S").to_string()
        };

        let evidence_type_str = evidence_type.to_string();

        sqlx::query(
            r#"
            INSERT INTO evidence
                (id, evidence_type, content, episode_id, tool_name,
                 retention_hours, compressed, expires_at, created_at)
            VALUES
                (?, ?, ?, ?, ?, ?, 0, ?, datetime('now'))
            "#,
        )
        .bind(&id)
        .bind(&evidence_type_str)
        .bind(content)
        .bind(episode_id)
        .bind(tool_name)
        .bind(retention_hours)
        .bind(&expires_at)
        .execute(self.store.pool())
        .await
        .context("failed to insert evidence record")?;

        Ok(id)
    }

    /// Classify tool activity into an `EvidenceType` without LLM involvement.
    ///
    /// Failure always wins — any failed tool call becomes an error trace
    /// regardless of the tool or arguments. Within successful calls, command
    /// content takes precedence: test runners and deployment commands are
    /// detected by keyword matching in `args_summary`, file-mutating tools
    /// produce diffs, and everything else is generic tool output.
    pub fn classify_evidence(
        tool_name: &str,
        args_summary: Option<&str>,
        is_failure: bool,
    ) -> EvidenceType {
        if is_failure {
            return EvidenceType::ErrorTrace;
        }

        let tool_lower = tool_name.to_lowercase();
        let summary_lower = args_summary.unwrap_or("").to_lowercase();

        // Shell and exec tools: inspect the command for test/deploy signals.
        if tool_lower == "shell" || tool_lower == "exec" {
            let is_test_command = [
                "cargo test",
                "pytest",
                " test ",
                "jest",
                "rspec",
                "mocha",
                "vitest",
                "go test",
                "npm test",
                "yarn test",
            ]
            .iter()
            .any(|pattern| summary_lower.contains(pattern))
                // Also catch bare "test" at start of command (e.g. "test -f ...").
                || summary_lower.starts_with("test");

            if is_test_command {
                return EvidenceType::TestResult;
            }

            let is_deploy_command = [
                "deploy",
                "kubectl",
                "helm ",
                "terraform",
                "docker push",
                "heroku push",
                "ansible",
                "flyctl",
                "railway up",
                "vercel deploy",
            ]
            .iter()
            .any(|pattern| summary_lower.contains(pattern));

            if is_deploy_command {
                return EvidenceType::Deploy;
            }
        }

        // File tools that mutate content produce diffs.
        let is_file_write = tool_lower.contains("write")
            || tool_lower.contains("patch")
            || tool_lower.contains("edit")
            || (tool_lower.contains("file")
                && (summary_lower.contains("write")
                    || summary_lower.contains("patch")
                    || summary_lower.contains("create")));

        if is_file_write {
            return EvidenceType::Diff;
        }

        EvidenceType::ToolOutput
    }

    /// Delete expired evidence, skipping records tied to active episodes.
    ///
    /// Active episodes may still reference their evidence for distillation or
    /// compaction, so only records with no episode or a completed episode are
    /// eligible for removal.
    pub async fn cleanup_expired(&self, active_episode_ids: &[&str]) -> Result<u64> {
        let rows_affected = if active_episode_ids.is_empty() {
            sqlx::query("DELETE FROM evidence WHERE expires_at < datetime('now')")
                .execute(self.store.pool())
                .await
                .context("failed to delete expired evidence")?
                .rows_affected()
        } else {
            // Build a parameterized NOT IN list. SQLite uses `?` for every
            // positional placeholder; we can't use a typed macro here because
            // the list length is dynamic.
            let placeholders = active_episode_ids
                .iter()
                .map(|_| "?")
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "DELETE FROM evidence \
                 WHERE expires_at < datetime('now') \
                 AND (episode_id IS NULL OR episode_id NOT IN ({placeholders}))"
            );
            let mut query = sqlx::query(&sql);
            for episode_id in active_episode_ids {
                query = query.bind(*episode_id);
            }
            query
                .execute(self.store.pool())
                .await
                .context("failed to delete expired evidence")?
                .rows_affected()
        };

        Ok(rows_affected)
    }

    /// Fetch all evidence records associated with an episode, oldest first.
    pub async fn get_for_episode(&self, episode_id: &str) -> Result<Vec<EvidenceRecord>> {
        let records = sqlx::query_as::<_, EvidenceRecord>(
            r#"
            SELECT id, evidence_type, content, episode_id, tool_name,
                   compressed, created_at
            FROM evidence
            WHERE episode_id = ?
            ORDER BY created_at ASC
            "#,
        )
        .bind(episode_id)
        .fetch_all(self.store.pool())
        .await
        .context("failed to fetch evidence for episode")?;

        Ok(records)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------------
    // classify_evidence — pure function, no database needed
    // ---------------------------------------------------------------------------

    #[test]
    fn test_classify_failure_always_wins() {
        // Even a clearly test-looking command becomes an error trace on failure.
        assert_eq!(
            EvidenceStore::classify_evidence("shell", Some("cargo test --all"), true),
            EvidenceType::ErrorTrace,
        );
        assert_eq!(
            EvidenceStore::classify_evidence("write_file", Some("patch src/main.rs"), true),
            EvidenceType::ErrorTrace,
        );
    }

    #[test]
    fn test_classify_test_commands() {
        for cmd in &["cargo test", "pytest tests/", "jest --watch", "rspec spec/", "go test ./..."] {
            assert_eq!(
                EvidenceStore::classify_evidence("shell", Some(cmd), false),
                EvidenceType::TestResult,
                "expected TestResult for shell with args '{cmd}'",
            );
        }
        assert_eq!(
            EvidenceStore::classify_evidence("exec", Some("npm test"), false),
            EvidenceType::TestResult,
        );
    }

    #[test]
    fn test_classify_deploy_commands() {
        for cmd in &["kubectl apply -f deploy.yaml", "helm upgrade myapp ./chart", "terraform apply", "docker push myrepo/app:latest"] {
            assert_eq!(
                EvidenceStore::classify_evidence("shell", Some(cmd), false),
                EvidenceType::Deploy,
                "expected Deploy for shell with args '{cmd}'",
            );
        }
    }

    #[test]
    fn test_classify_file_write() {
        assert_eq!(
            EvidenceStore::classify_evidence("write_file", None, false),
            EvidenceType::Diff,
        );
        assert_eq!(
            EvidenceStore::classify_evidence("edit_file", Some("src/lib.rs"), false),
            EvidenceType::Diff,
        );
        assert_eq!(
            EvidenceStore::classify_evidence("patch", Some("fix auth bug"), false),
            EvidenceType::Diff,
        );
        assert_eq!(
            EvidenceStore::classify_evidence("file", Some("write README.md"), false),
            EvidenceType::Diff,
        );
    }

    #[test]
    fn test_classify_tool_output_default() {
        assert_eq!(
            EvidenceStore::classify_evidence("shell", Some("ls -la"), false),
            EvidenceType::ToolOutput,
        );
        assert_eq!(
            EvidenceStore::classify_evidence("memory_recall", Some("query: auth"), false),
            EvidenceType::ToolOutput,
        );
        assert_eq!(
            EvidenceStore::classify_evidence("browser", None, false),
            EvidenceType::ToolOutput,
        );
    }

    #[test]
    fn test_classify_no_args_summary() {
        // No summary means we can only go on tool name.
        assert_eq!(
            EvidenceStore::classify_evidence("shell", None, false),
            EvidenceType::ToolOutput,
        );
    }

    // ---------------------------------------------------------------------------
    // Database-backed tests
    // ---------------------------------------------------------------------------

    async fn make_evidence_store() -> EvidenceStore {
        let path = std::env::temp_dir()
            .join(format!("spacebot-evidence-test-{}.db", Uuid::new_v4()));
        let store = LearningStore::connect(&path).await.unwrap();
        EvidenceStore::new(store)
    }

    #[tokio::test]
    async fn test_store_and_retrieve_evidence() {
        let evidence_store = make_evidence_store().await;
        let episode_id = Uuid::new_v4().to_string();

        let id = evidence_store
            .store_evidence(
                &EvidenceType::TestResult,
                "all 42 tests passed",
                Some(&episode_id),
                Some("shell"),
            )
            .await
            .unwrap();

        assert!(!id.is_empty());

        let records = evidence_store.get_for_episode(&episode_id).await.unwrap();
        assert_eq!(records.len(), 1);

        let record = &records[0];
        assert_eq!(record.id, id);
        assert_eq!(record.evidence_type, "test_result");
        assert_eq!(record.content, "all 42 tests passed");
        assert_eq!(record.episode_id.as_deref(), Some(episode_id.as_str()));
        assert_eq!(record.tool_name.as_deref(), Some("shell"));
        assert!(!record.compressed);
    }

    #[tokio::test]
    async fn test_store_without_episode() {
        let evidence_store = make_evidence_store().await;

        let id = evidence_store
            .store_evidence(&EvidenceType::ToolOutput, "some output", None, Some("exec"))
            .await
            .unwrap();

        assert!(!id.is_empty());

        // Records with no episode_id should not appear under any episode.
        let records = evidence_store
            .get_for_episode("nonexistent-episode")
            .await
            .unwrap();
        assert!(records.is_empty());
    }

    #[tokio::test]
    async fn test_get_for_episode_ordering() {
        let evidence_store = make_evidence_store().await;
        let episode_id = Uuid::new_v4().to_string();

        for content in &["first", "second", "third"] {
            evidence_store
                .store_evidence(
                    &EvidenceType::ToolOutput,
                    content,
                    Some(&episode_id),
                    None,
                )
                .await
                .unwrap();
        }

        let records = evidence_store.get_for_episode(&episode_id).await.unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].content, "first");
        assert_eq!(records[1].content, "second");
        assert_eq!(records[2].content, "third");
    }

    #[tokio::test]
    async fn test_cleanup_expired_no_active_episodes() {
        let evidence_store = make_evidence_store().await;

        // Insert an already-expired record by manually inserting with a past expires_at.
        sqlx::query(
            "INSERT INTO evidence (id, evidence_type, content, retention_hours, compressed, expires_at)
             VALUES (?, 'tool_output', 'stale output', 1, 0, '2000-01-01')",
        )
        .bind(Uuid::new_v4().to_string())
        .execute(evidence_store.store.pool())
        .await
        .unwrap();

        let deleted = evidence_store.cleanup_expired(&[]).await.unwrap();
        assert_eq!(deleted, 1);
    }

    #[tokio::test]
    async fn test_cleanup_expired_skips_active_episodes() {
        let evidence_store = make_evidence_store().await;
        let active_episode = Uuid::new_v4().to_string();
        let done_episode = Uuid::new_v4().to_string();

        // Both records are past their expiry, but one belongs to an active episode.
        for (episode_id, content) in &[
            (active_episode.as_str(), "active episode evidence"),
            (done_episode.as_str(), "done episode evidence"),
        ] {
            sqlx::query(
                "INSERT INTO evidence (id, evidence_type, content, episode_id, retention_hours, compressed, expires_at)
                 VALUES (?, 'tool_output', ?, ?, 1, 0, '2000-01-01')",
            )
            .bind(Uuid::new_v4().to_string())
            .bind(content)
            .bind(episode_id)
            .execute(evidence_store.store.pool())
            .await
            .unwrap();
        }

        // Only the done episode's record should be deleted.
        let deleted = evidence_store
            .cleanup_expired(&[active_episode.as_str()])
            .await
            .unwrap();
        assert_eq!(deleted, 1);

        // Active episode's record should still be there.
        let remaining = evidence_store.get_for_episode(&active_episode).await.unwrap();
        assert_eq!(remaining.len(), 1);
    }

    #[tokio::test]
    async fn test_user_flagged_uses_permanent_sentinel() {
        let evidence_store = make_evidence_store().await;

        evidence_store
            .store_evidence(&EvidenceType::UserFlagged, "important note", None, None)
            .await
            .unwrap();

        // Permanent evidence should survive a cleanup pass.
        let deleted = evidence_store.cleanup_expired(&[]).await.unwrap();
        assert_eq!(deleted, 0);
    }
}
