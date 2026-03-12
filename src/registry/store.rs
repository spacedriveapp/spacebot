//! SQLite-backed registry store for auto-discovered GitHub repositories.

use crate::error::Result;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use sqlx::{Row as _, SqlitePool};

/// A GitHub repository tracked in the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryRepo {
    pub id: String,
    pub agent_id: String,
    pub owner: String,
    pub name: String,
    pub full_name: String,
    pub description: String,
    pub default_branch: String,
    pub is_archived: bool,
    pub is_fork: bool,
    pub visibility: String,
    pub language: Option<String>,
    pub local_path: Option<String>,
    pub clone_url: String,
    pub ssh_url: String,
    /// Per-repo model override (e.g., "anthropic/claude-sonnet-4").
    pub worker_model: Option<String>,
    /// Whether the agent should process events for this repo.
    pub enabled: bool,
    /// Link to the existing projects table.
    pub project_id: Option<String>,
    pub last_synced_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Input for upserting a repo discovered from `gh repo list`.
#[derive(Debug, Clone)]
pub struct UpsertRepoInput {
    pub agent_id: String,
    pub owner: String,
    pub name: String,
    pub full_name: String,
    pub description: String,
    pub default_branch: String,
    pub is_archived: bool,
    pub is_fork: bool,
    pub visibility: String,
    pub language: Option<String>,
    pub clone_url: String,
    pub ssh_url: String,
}

/// SQLite-backed registry store.
#[derive(Debug, Clone)]
pub struct RegistryStore {
    pool: SqlitePool,
}

impl RegistryStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert or update a discovered repo. Preserves user-set overrides
    /// (worker_model, enabled) on conflict.
    pub async fn upsert_repo(&self, input: UpsertRepoInput) -> Result<RegistryRepo> {
        let id = uuid::Uuid::new_v4().to_string();

        sqlx::query(
            r#"
            INSERT INTO registry_repos (id, agent_id, owner, name, full_name, description,
                default_branch, is_archived, is_fork, visibility, language, clone_url, ssh_url,
                last_synced_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
            ON CONFLICT (agent_id, full_name) DO UPDATE SET
                description = excluded.description,
                default_branch = excluded.default_branch,
                is_archived = excluded.is_archived,
                is_fork = excluded.is_fork,
                visibility = excluded.visibility,
                language = excluded.language,
                clone_url = excluded.clone_url,
                ssh_url = excluded.ssh_url,
                last_synced_at = CURRENT_TIMESTAMP,
                updated_at = CURRENT_TIMESTAMP
            "#,
        )
        .bind(&id)
        .bind(&input.agent_id)
        .bind(&input.owner)
        .bind(&input.name)
        .bind(&input.full_name)
        .bind(&input.description)
        .bind(&input.default_branch)
        .bind(input.is_archived)
        .bind(input.is_fork)
        .bind(&input.visibility)
        .bind(&input.language)
        .bind(&input.clone_url)
        .bind(&input.ssh_url)
        .execute(&self.pool)
        .await
        .context("failed to upsert registry repo")?;

        Ok(self
            .get_by_full_name(&input.agent_id, &input.full_name)
            .await?
            .context("repo not found after upsert")?)
    }

    /// Get a repo by its full name (e.g., "marcmantei/ChargePilot-Launch").
    pub async fn get_by_full_name(
        &self,
        agent_id: &str,
        full_name: &str,
    ) -> Result<Option<RegistryRepo>> {
        let row = sqlx::query(
            "SELECT * FROM registry_repos WHERE agent_id = ? AND full_name = ?",
        )
        .bind(agent_id)
        .bind(full_name)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch registry repo by full_name")?;

        row.map(|r| row_to_registry_repo(&r)).transpose()
    }

    /// Get a repo by its ID.
    pub async fn get_by_id(&self, id: &str) -> Result<Option<RegistryRepo>> {
        let row = sqlx::query("SELECT * FROM registry_repos WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .context("failed to fetch registry repo by id")?;

        row.map(|r| row_to_registry_repo(&r)).transpose()
    }

    /// List all repos for an agent, optionally filtering by enabled status.
    pub async fn list_repos(
        &self,
        agent_id: &str,
        enabled_only: bool,
    ) -> Result<Vec<RegistryRepo>> {
        let rows = if enabled_only {
            sqlx::query(
                "SELECT * FROM registry_repos WHERE agent_id = ? AND enabled = 1 ORDER BY full_name ASC",
            )
            .bind(agent_id)
            .fetch_all(&self.pool)
            .await
            .context("failed to list enabled registry repos")?
        } else {
            sqlx::query(
                "SELECT * FROM registry_repos WHERE agent_id = ? ORDER BY full_name ASC",
            )
            .bind(agent_id)
            .fetch_all(&self.pool)
            .await
            .context("failed to list registry repos")?
        };

        rows.iter().map(row_to_registry_repo).collect()
    }

    /// Update per-repo overrides.
    pub async fn set_overrides(
        &self,
        agent_id: &str,
        full_name: &str,
        worker_model: Option<Option<String>>,
        enabled: Option<bool>,
    ) -> Result<Option<RegistryRepo>> {
        let existing = self.get_by_full_name(agent_id, full_name).await?;
        let Some(existing) = existing else {
            return Ok(None);
        };

        let worker_model = match worker_model {
            Some(v) => v,
            None => existing.worker_model,
        };
        let enabled = enabled.unwrap_or(existing.enabled);

        sqlx::query(
            r#"
            UPDATE registry_repos
            SET worker_model = ?, enabled = ?, updated_at = CURRENT_TIMESTAMP
            WHERE agent_id = ? AND full_name = ?
            "#,
        )
        .bind(&worker_model)
        .bind(enabled)
        .bind(agent_id)
        .bind(full_name)
        .execute(&self.pool)
        .await
        .context("failed to update registry repo overrides")?;

        self.get_by_full_name(agent_id, full_name).await
    }

    /// Link a registry repo to an existing project.
    pub async fn link_project(
        &self,
        agent_id: &str,
        full_name: &str,
        project_id: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE registry_repos SET project_id = ?, updated_at = CURRENT_TIMESTAMP WHERE agent_id = ? AND full_name = ?",
        )
        .bind(project_id)
        .bind(agent_id)
        .bind(full_name)
        .execute(&self.pool)
        .await
        .context("failed to link registry repo to project")?;
        Ok(())
    }

    /// Set the local_path for a repo (after cloning).
    pub async fn set_local_path(
        &self,
        agent_id: &str,
        full_name: &str,
        local_path: &str,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE registry_repos SET local_path = ?, updated_at = CURRENT_TIMESTAMP WHERE agent_id = ? AND full_name = ?",
        )
        .bind(local_path)
        .bind(agent_id)
        .bind(full_name)
        .execute(&self.pool)
        .await
        .context("failed to set registry repo local_path")?;
        Ok(())
    }

    /// Mark repos not seen in the latest sync as archived.
    pub async fn mark_absent_as_archived(
        &self,
        agent_id: &str,
        seen_full_names: &[String],
    ) -> Result<u64> {
        if seen_full_names.is_empty() {
            return Ok(0);
        }

        // Build a parameterized IN clause
        let placeholders: Vec<&str> = seen_full_names.iter().map(|_| "?").collect();
        let in_clause = placeholders.join(", ");
        let query = format!(
            "UPDATE registry_repos SET is_archived = 1, updated_at = CURRENT_TIMESTAMP \
             WHERE agent_id = ? AND full_name NOT IN ({}) AND is_archived = 0",
            in_clause
        );

        let mut q = sqlx::query(&query).bind(agent_id);
        for name in seen_full_names {
            q = q.bind(name);
        }

        let result = q
            .execute(&self.pool)
            .await
            .context("failed to mark absent repos as archived")?;

        Ok(result.rows_affected())
    }

    /// Get the full names of all non-archived repos for an agent.
    pub async fn get_non_archived_names(&self, agent_id: &str) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT full_name FROM registry_repos WHERE agent_id = ? AND is_archived = 0",
        )
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch non-archived repo names")?;

        rows.iter()
            .map(|r| r.try_get("full_name").context("missing full_name"))
            .collect::<std::result::Result<Vec<String>, _>>()
            .map_err(Into::into)
    }

    /// Delete a repo from the registry.
    pub async fn delete_repo(&self, agent_id: &str, full_name: &str) -> Result<bool> {
        let result = sqlx::query(
            "DELETE FROM registry_repos WHERE agent_id = ? AND full_name = ?",
        )
        .bind(agent_id)
        .bind(full_name)
        .execute(&self.pool)
        .await
        .context("failed to delete registry repo")?;

        Ok(result.rows_affected() > 0)
    }
}

// Row mapping

fn row_to_registry_repo(row: &sqlx::sqlite::SqliteRow) -> Result<RegistryRepo> {
    Ok(RegistryRepo {
        id: row.try_get("id").context("missing id")?,
        agent_id: row.try_get("agent_id").context("missing agent_id")?,
        owner: row.try_get("owner").context("missing owner")?,
        name: row.try_get("name").context("missing name")?,
        full_name: row.try_get("full_name").context("missing full_name")?,
        description: row.try_get("description").context("missing description")?,
        default_branch: row
            .try_get("default_branch")
            .context("missing default_branch")?,
        is_archived: row
            .try_get::<bool, _>("is_archived")
            .context("missing is_archived")?,
        is_fork: row
            .try_get::<bool, _>("is_fork")
            .context("missing is_fork")?,
        visibility: row
            .try_get("visibility")
            .context("missing visibility")?,
        language: row.try_get("language").unwrap_or(None),
        local_path: row.try_get("local_path").unwrap_or(None),
        clone_url: row.try_get("clone_url").context("missing clone_url")?,
        ssh_url: row.try_get("ssh_url").context("missing ssh_url")?,
        worker_model: row.try_get("worker_model").unwrap_or(None),
        enabled: row
            .try_get::<bool, _>("enabled")
            .context("missing enabled")?,
        project_id: row.try_get("project_id").unwrap_or(None),
        last_synced_at: row.try_get("last_synced_at").unwrap_or(None),
        created_at: row.try_get("created_at").context("missing created_at")?,
        updated_at: row.try_get("updated_at").context("missing updated_at")?,
    })
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("failed to create in-memory pool");
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("failed to run migrations");
        pool
    }

    fn sample_input() -> UpsertRepoInput {
        UpsertRepoInput {
            agent_id: "main".into(),
            owner: "marcmantei".into(),
            name: "ChargePilot-Launch".into(),
            full_name: "marcmantei/ChargePilot-Launch".into(),
            description: "ChargePilot launch site".into(),
            default_branch: "main".into(),
            is_archived: false,
            is_fork: false,
            visibility: "private".into(),
            language: Some("TypeScript".into()),
            clone_url: "https://github.com/marcmantei/ChargePilot-Launch.git".into(),
            ssh_url: "git@github.com:marcmantei/ChargePilot-Launch.git".into(),
        }
    }

    #[tokio::test]
    async fn upsert_and_retrieve() {
        let pool = setup_pool().await;
        let store = RegistryStore::new(pool);

        let repo = store.upsert_repo(sample_input()).await.unwrap();
        assert_eq!(repo.full_name, "marcmantei/ChargePilot-Launch");
        assert_eq!(repo.language.as_deref(), Some("TypeScript"));
        assert!(repo.enabled);
        assert!(!repo.is_archived);

        // Retrieve by full name
        let found = store
            .get_by_full_name("main", "marcmantei/ChargePilot-Launch")
            .await
            .unwrap()
            .expect("should find repo");
        assert_eq!(found.id, repo.id);
    }

    #[tokio::test]
    async fn upsert_preserves_overrides() {
        let pool = setup_pool().await;
        let store = RegistryStore::new(pool);

        store.upsert_repo(sample_input()).await.unwrap();

        // Set a worker_model override
        store
            .set_overrides(
                "main",
                "marcmantei/ChargePilot-Launch",
                Some(Some("anthropic/claude-sonnet-4".into())),
                Some(false),
            )
            .await
            .unwrap();

        // Upsert again (simulating re-sync)
        let mut input = sample_input();
        input.description = "Updated description".into();
        store.upsert_repo(input).await.unwrap();

        // Overrides should be preserved
        let repo = store
            .get_by_full_name("main", "marcmantei/ChargePilot-Launch")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            repo.worker_model.as_deref(),
            Some("anthropic/claude-sonnet-4")
        );
        assert!(!repo.enabled);
        assert_eq!(repo.description, "Updated description");
    }

    #[tokio::test]
    async fn list_repos_filter() {
        let pool = setup_pool().await;
        let store = RegistryStore::new(pool);

        store.upsert_repo(sample_input()).await.unwrap();

        let mut input2 = sample_input();
        input2.name = "other-repo".into();
        input2.full_name = "marcmantei/other-repo".into();
        store.upsert_repo(input2).await.unwrap();

        // Disable one
        store
            .set_overrides("main", "marcmantei/other-repo", None, Some(false))
            .await
            .unwrap();

        let all = store.list_repos("main", false).await.unwrap();
        assert_eq!(all.len(), 2);

        let enabled = store.list_repos("main", true).await.unwrap();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].full_name, "marcmantei/ChargePilot-Launch");
    }

    #[tokio::test]
    async fn mark_absent_as_archived() {
        let pool = setup_pool().await;
        let store = RegistryStore::new(pool);

        store.upsert_repo(sample_input()).await.unwrap();

        let mut input2 = sample_input();
        input2.name = "removed-repo".into();
        input2.full_name = "marcmantei/removed-repo".into();
        store.upsert_repo(input2).await.unwrap();

        // Only ChargePilot-Launch was seen in this sync
        let archived = store
            .mark_absent_as_archived(
                "main",
                &["marcmantei/ChargePilot-Launch".into()],
            )
            .await
            .unwrap();
        assert_eq!(archived, 1);

        let repo = store
            .get_by_full_name("main", "marcmantei/removed-repo")
            .await
            .unwrap()
            .unwrap();
        assert!(repo.is_archived);
    }

    #[tokio::test]
    async fn delete_repo() {
        let pool = setup_pool().await;
        let store = RegistryStore::new(pool);

        store.upsert_repo(sample_input()).await.unwrap();
        let deleted = store
            .delete_repo("main", "marcmantei/ChargePilot-Launch")
            .await
            .unwrap();
        assert!(deleted);

        let found = store
            .get_by_full_name("main", "marcmantei/ChargePilot-Launch")
            .await
            .unwrap();
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn link_project() {
        let pool = setup_pool().await;
        let store = RegistryStore::new(pool.clone());

        store.upsert_repo(sample_input()).await.unwrap();

        // Create a real project to link to (satisfies FK constraint).
        let project_store = crate::projects::ProjectStore::new(pool);
        let project = project_store
            .create_project(crate::projects::store::CreateProjectInput {
                agent_id: "main".into(),
                name: "ChargePilot".into(),
                description: String::new(),
                icon: String::new(),
                tags: vec![],
                root_path: "/home/sira/ChargePilot-Launch".into(),
                settings: serde_json::Value::Object(Default::default()),
            })
            .await
            .unwrap();

        store
            .link_project("main", "marcmantei/ChargePilot-Launch", Some(&project.id))
            .await
            .unwrap();

        let repo = store
            .get_by_full_name("main", "marcmantei/ChargePilot-Launch")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(repo.project_id.as_deref(), Some(project.id.as_str()));

        // Unlinking should also work.
        store
            .link_project("main", "marcmantei/ChargePilot-Launch", None)
            .await
            .unwrap();
        let repo = store
            .get_by_full_name("main", "marcmantei/ChargePilot-Launch")
            .await
            .unwrap()
            .unwrap();
        assert!(repo.project_id.is_none());
    }
}
