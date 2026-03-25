//! Project CRUD storage (SQLite).

use crate::error::Result;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Row as _, SqlitePool};

// Enums

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    Active,
    Archived,
}

impl ProjectStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectStatus::Active => "active",
            ProjectStatus::Archived => "archived",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(ProjectStatus::Active),
            "archived" => Some(ProjectStatus::Archived),
            _ => None,
        }
    }
}

impl std::fmt::Display for ProjectStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Per-project settings overrides. Each field is optional — `None` means
/// "inherit from the agent-level `ProjectsConfig`". Stored as JSON in the
/// `settings` column of the `projects` table.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_worktrees: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_name_template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_create_worktrees: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_discover_repos: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_discover_worktrees: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_usage_warning_threshold: Option<u64>,
}

// Domain types

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Project {
    pub id: String,
    pub agent_id: String,
    pub name: String,
    pub description: String,
    pub icon: String,
    pub tags: Vec<String>,
    pub root_path: String,
    pub settings: Value,
    pub status: ProjectStatus,
    pub created_at: String,
    pub updated_at: String,
}

impl Project {
    /// Deserialize the `settings` JSON blob into a typed `ProjectSettings`.
    /// Returns defaults if the blob is empty or malformed.
    pub fn typed_settings(&self) -> ProjectSettings {
        serde_json::from_value(self.settings.clone()).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProjectRepo {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub path: String,
    pub remote_url: String,
    pub default_branch: String,
    /// Currently checked-out branch (may differ from `default_branch`).
    pub current_branch: Option<String>,
    pub description: String,
    pub disk_usage_bytes: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProjectWorktree {
    pub id: String,
    pub project_id: String,
    pub repo_id: String,
    pub name: String,
    pub path: String,
    pub branch: String,
    pub created_by: String,
    pub disk_usage_bytes: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

/// Full project with nested repos and worktrees for API responses.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProjectWithRelations {
    #[serde(flatten)]
    pub project: Project,
    pub repos: Vec<ProjectRepo>,
    pub worktrees: Vec<ProjectWorktreeWithRepo>,
}

/// Worktree with the source repo name resolved.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProjectWorktreeWithRepo {
    #[serde(flatten)]
    pub worktree: ProjectWorktree,
    pub repo_name: String,
}

// Input types

#[derive(Debug, Clone)]
pub struct CreateProjectInput {
    pub agent_id: String,
    pub name: String,
    pub description: String,
    pub icon: String,
    pub tags: Vec<String>,
    pub root_path: String,
    pub settings: Value,
}

#[derive(Debug, Clone, Default)]
pub struct UpdateProjectInput {
    pub name: Option<String>,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub tags: Option<Vec<String>>,
    pub settings: Option<Value>,
    pub status: Option<ProjectStatus>,
}

#[derive(Debug, Clone)]
pub struct CreateRepoInput {
    pub project_id: String,
    pub name: String,
    pub path: String,
    pub remote_url: String,
    pub default_branch: String,
    pub current_branch: Option<String>,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct CreateWorktreeInput {
    pub project_id: String,
    pub repo_id: String,
    pub name: String,
    pub path: String,
    pub branch: String,
    pub created_by: String,
}

// Store

#[derive(Debug, Clone)]
pub struct ProjectStore {
    pool: SqlitePool,
}

impl ProjectStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    // -- Projects -----------------------------------------------------------

    pub async fn create_project(&self, input: CreateProjectInput) -> Result<Project> {
        let id = uuid::Uuid::new_v4().to_string();
        let tags_json = serde_json::to_string(&input.tags).context("failed to serialize tags")?;
        let settings_json =
            serde_json::to_string(&input.settings).context("failed to serialize settings")?;

        sqlx::query(
            r#"
            INSERT INTO projects (id, agent_id, name, description, icon, tags, root_path, settings, status)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'active')
            "#,
        )
        .bind(&id)
        .bind(&input.agent_id)
        .bind(&input.name)
        .bind(&input.description)
        .bind(&input.icon)
        .bind(&tags_json)
        .bind(&input.root_path)
        .bind(&settings_json)
        .execute(&self.pool)
        .await
        .context("failed to insert project")?;

        Ok(self
            .get_project(&input.agent_id, &id)
            .await?
            .context("project not found after insert")?)
    }

    pub async fn get_project(&self, agent_id: &str, project_id: &str) -> Result<Option<Project>> {
        let row = sqlx::query("SELECT * FROM projects WHERE id = ? AND agent_id = ?")
            .bind(project_id)
            .bind(agent_id)
            .fetch_optional(&self.pool)
            .await
            .context("failed to fetch project")?;

        row.map(|r| row_to_project(&r)).transpose()
    }

    pub async fn list_projects(
        &self,
        agent_id: &str,
        status: Option<ProjectStatus>,
    ) -> Result<Vec<Project>> {
        let rows = if let Some(status) = status {
            sqlx::query(
                "SELECT * FROM projects WHERE agent_id = ? AND status = ? ORDER BY updated_at DESC",
            )
            .bind(agent_id)
            .bind(status.as_str())
            .fetch_all(&self.pool)
            .await
            .context("failed to list projects")?
        } else {
            sqlx::query("SELECT * FROM projects WHERE agent_id = ? ORDER BY updated_at DESC")
                .bind(agent_id)
                .fetch_all(&self.pool)
                .await
                .context("failed to list projects")?
        };

        rows.iter().map(row_to_project).collect()
    }

    pub async fn update_project(
        &self,
        agent_id: &str,
        project_id: &str,
        input: UpdateProjectInput,
    ) -> Result<Option<Project>> {
        let existing = self.get_project(agent_id, project_id).await?;
        let Some(existing) = existing else {
            return Ok(None);
        };

        let name = input.name.unwrap_or(existing.name);
        let description = input.description.unwrap_or(existing.description);
        let icon = input.icon.unwrap_or(existing.icon);
        let tags = input.tags.unwrap_or(existing.tags);
        let tags_json = serde_json::to_string(&tags).context("failed to serialize tags")?;
        let settings = input.settings.unwrap_or(existing.settings);
        let settings_json =
            serde_json::to_string(&settings).context("failed to serialize settings")?;
        let status = input.status.unwrap_or(existing.status);

        sqlx::query(
            r#"
            UPDATE projects
            SET name = ?, description = ?, icon = ?, tags = ?, settings = ?,
                status = ?, updated_at = CURRENT_TIMESTAMP
            WHERE id = ? AND agent_id = ?
            "#,
        )
        .bind(&name)
        .bind(&description)
        .bind(&icon)
        .bind(&tags_json)
        .bind(&settings_json)
        .bind(status.as_str())
        .bind(project_id)
        .bind(agent_id)
        .execute(&self.pool)
        .await
        .context("failed to update project")?;

        self.get_project(agent_id, project_id).await
    }

    pub async fn delete_project(&self, agent_id: &str, project_id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM projects WHERE id = ? AND agent_id = ?")
            .bind(project_id)
            .bind(agent_id)
            .execute(&self.pool)
            .await
            .context("failed to delete project")?;

        Ok(result.rows_affected() > 0)
    }

    /// Load a project with all its repos and worktrees.
    pub async fn get_project_with_relations(
        &self,
        agent_id: &str,
        project_id: &str,
    ) -> Result<Option<ProjectWithRelations>> {
        let Some(project) = self.get_project(agent_id, project_id).await? else {
            return Ok(None);
        };
        let repos = self.list_repos(project_id).await?;
        let worktrees = self.list_worktrees_with_repos(project_id).await?;
        Ok(Some(ProjectWithRelations {
            project,
            repos,
            worktrees,
        }))
    }

    // -- Repos --------------------------------------------------------------

    pub async fn create_repo(&self, input: CreateRepoInput) -> Result<ProjectRepo> {
        let id = uuid::Uuid::new_v4().to_string();

        sqlx::query(
            r#"
            INSERT INTO project_repos (id, project_id, name, path, remote_url, default_branch, current_branch, description)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&id)
        .bind(&input.project_id)
        .bind(&input.name)
        .bind(&input.path)
        .bind(&input.remote_url)
        .bind(&input.default_branch)
        .bind(&input.current_branch)
        .bind(&input.description)
        .execute(&self.pool)
        .await
        .context("failed to insert repo")?;

        Ok(self
            .get_repo(&id)
            .await?
            .context("repo not found after insert")?)
    }

    pub async fn get_repo(&self, repo_id: &str) -> Result<Option<ProjectRepo>> {
        let row = sqlx::query("SELECT * FROM project_repos WHERE id = ?")
            .bind(repo_id)
            .fetch_optional(&self.pool)
            .await
            .context("failed to fetch repo")?;

        row.map(|r| row_to_repo(&r)).transpose()
    }

    pub async fn list_repos(&self, project_id: &str) -> Result<Vec<ProjectRepo>> {
        let rows =
            sqlx::query("SELECT * FROM project_repos WHERE project_id = ? ORDER BY name ASC")
                .bind(project_id)
                .fetch_all(&self.pool)
                .await
                .context("failed to list repos")?;

        rows.iter().map(row_to_repo).collect()
    }

    pub async fn delete_repo(&self, repo_id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM project_repos WHERE id = ?")
            .bind(repo_id)
            .execute(&self.pool)
            .await
            .context("failed to delete repo")?;

        Ok(result.rows_affected() > 0)
    }

    /// Find a repo by its relative path within a project.
    pub async fn get_repo_by_path(
        &self,
        project_id: &str,
        path: &str,
    ) -> Result<Option<ProjectRepo>> {
        let row = sqlx::query("SELECT * FROM project_repos WHERE project_id = ? AND path = ?")
            .bind(project_id)
            .bind(path)
            .fetch_optional(&self.pool)
            .await
            .context("failed to fetch repo by path")?;

        row.map(|r| row_to_repo(&r)).transpose()
    }

    // -- Worktrees ----------------------------------------------------------

    pub async fn create_worktree(&self, input: CreateWorktreeInput) -> Result<ProjectWorktree> {
        let id = uuid::Uuid::new_v4().to_string();

        sqlx::query(
            r#"
            INSERT INTO project_worktrees (id, project_id, repo_id, name, path, branch, created_by)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&id)
        .bind(&input.project_id)
        .bind(&input.repo_id)
        .bind(&input.name)
        .bind(&input.path)
        .bind(&input.branch)
        .bind(&input.created_by)
        .execute(&self.pool)
        .await
        .context("failed to insert worktree")?;

        Ok(self
            .get_worktree(&id)
            .await?
            .context("worktree not found after insert")?)
    }

    pub async fn get_worktree(&self, worktree_id: &str) -> Result<Option<ProjectWorktree>> {
        let row = sqlx::query("SELECT * FROM project_worktrees WHERE id = ?")
            .bind(worktree_id)
            .fetch_optional(&self.pool)
            .await
            .context("failed to fetch worktree")?;

        row.map(|r| row_to_worktree(&r)).transpose()
    }

    pub async fn list_worktrees(&self, project_id: &str) -> Result<Vec<ProjectWorktree>> {
        let rows =
            sqlx::query("SELECT * FROM project_worktrees WHERE project_id = ? ORDER BY name ASC")
                .bind(project_id)
                .fetch_all(&self.pool)
                .await
                .context("failed to list worktrees")?;

        rows.iter().map(row_to_worktree).collect()
    }

    /// List worktrees with the source repo name resolved via JOIN.
    pub async fn list_worktrees_with_repos(
        &self,
        project_id: &str,
    ) -> Result<Vec<ProjectWorktreeWithRepo>> {
        let rows = sqlx::query(
            r#"
            SELECT w.*, r.name AS repo_name
            FROM project_worktrees w
            JOIN project_repos r ON w.repo_id = r.id
            WHERE w.project_id = ?
            ORDER BY w.name ASC
            "#,
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list worktrees with repos")?;

        rows.iter()
            .map(|r| {
                let worktree = row_to_worktree(r)?;
                let repo_name: String = r.try_get("repo_name").context("missing repo_name")?;
                Ok(ProjectWorktreeWithRepo {
                    worktree,
                    repo_name,
                })
            })
            .collect()
    }

    pub async fn delete_worktree(&self, worktree_id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM project_worktrees WHERE id = ?")
            .bind(worktree_id)
            .execute(&self.pool)
            .await
            .context("failed to delete worktree")?;

        Ok(result.rows_affected() > 0)
    }

    /// Find a worktree by its relative path within a project.
    pub async fn get_worktree_by_path(
        &self,
        project_id: &str,
        path: &str,
    ) -> Result<Option<ProjectWorktree>> {
        let row = sqlx::query("SELECT * FROM project_worktrees WHERE project_id = ? AND path = ?")
            .bind(project_id)
            .bind(path)
            .fetch_optional(&self.pool)
            .await
            .context("failed to fetch worktree by path")?;

        row.map(|r| row_to_worktree(&r)).transpose()
    }

    /// Update the current_branch for a repo (e.g. after a scan detects a checkout change).
    pub async fn update_repo_current_branch(
        &self,
        repo_id: &str,
        current_branch: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE project_repos SET current_branch = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
        )
        .bind(current_branch)
        .bind(repo_id)
        .execute(&self.pool)
        .await
        .context("failed to update repo current_branch")?;
        Ok(())
    }

    /// Update cached disk usage for a repo.
    pub async fn set_repo_disk_usage(&self, repo_id: &str, bytes: i64) -> Result<()> {
        sqlx::query("UPDATE project_repos SET disk_usage_bytes = ? WHERE id = ?")
            .bind(bytes)
            .bind(repo_id)
            .execute(&self.pool)
            .await
            .context("failed to update repo disk usage")?;
        Ok(())
    }

    /// Update cached disk usage for a worktree.
    pub async fn set_worktree_disk_usage(&self, worktree_id: &str, bytes: i64) -> Result<()> {
        sqlx::query("UPDATE project_worktrees SET disk_usage_bytes = ? WHERE id = ?")
            .bind(bytes)
            .bind(worktree_id)
            .execute(&self.pool)
            .await
            .context("failed to update worktree disk usage")?;
        Ok(())
    }

    /// List worktrees belonging to a specific repo.
    pub async fn list_worktrees_for_repo(&self, repo_id: &str) -> Result<Vec<ProjectWorktree>> {
        let rows =
            sqlx::query("SELECT * FROM project_worktrees WHERE repo_id = ? ORDER BY name ASC")
                .bind(repo_id)
                .fetch_all(&self.pool)
                .await
                .context("failed to list worktrees for repo")?;

        rows.iter().map(row_to_worktree).collect()
    }
}

// Row mapping helpers

fn row_to_project(row: &sqlx::sqlite::SqliteRow) -> Result<Project> {
    let tags_raw: String = row.try_get("tags").context("missing tags")?;
    let tags: Vec<String> = serde_json::from_str(&tags_raw).unwrap_or_default();

    let settings_raw: String = row.try_get("settings").context("missing settings")?;
    let settings: Value =
        serde_json::from_str(&settings_raw).unwrap_or(Value::Object(Default::default()));

    let status_raw: String = row.try_get("status").context("missing status")?;
    let status = ProjectStatus::parse(&status_raw).unwrap_or(ProjectStatus::Active);

    Ok(Project {
        id: row.try_get("id").context("missing id")?,
        agent_id: row.try_get("agent_id").context("missing agent_id")?,
        name: row.try_get("name").context("missing name")?,
        description: row.try_get("description").context("missing description")?,
        icon: row.try_get("icon").context("missing icon")?,
        tags,
        root_path: row.try_get("root_path").context("missing root_path")?,
        settings,
        status,
        created_at: row.try_get("created_at").context("missing created_at")?,
        updated_at: row.try_get("updated_at").context("missing updated_at")?,
    })
}

fn row_to_repo(row: &sqlx::sqlite::SqliteRow) -> Result<ProjectRepo> {
    Ok(ProjectRepo {
        id: row.try_get("id").context("missing id")?,
        project_id: row.try_get("project_id").context("missing project_id")?,
        name: row.try_get("name").context("missing name")?,
        path: row.try_get("path").context("missing path")?,
        remote_url: row.try_get("remote_url").context("missing remote_url")?,
        default_branch: row
            .try_get("default_branch")
            .context("missing default_branch")?,
        current_branch: row.try_get("current_branch").unwrap_or(None),
        description: row.try_get("description").context("missing description")?,
        disk_usage_bytes: row.try_get("disk_usage_bytes").unwrap_or(None),
        created_at: row.try_get("created_at").context("missing created_at")?,
        updated_at: row.try_get("updated_at").context("missing updated_at")?,
    })
}

fn row_to_worktree(row: &sqlx::sqlite::SqliteRow) -> Result<ProjectWorktree> {
    Ok(ProjectWorktree {
        id: row.try_get("id").context("missing id")?,
        project_id: row.try_get("project_id").context("missing project_id")?,
        repo_id: row.try_get("repo_id").context("missing repo_id")?,
        name: row.try_get("name").context("missing name")?,
        path: row.try_get("path").context("missing path")?,
        branch: row.try_get("branch").context("missing branch")?,
        created_by: row.try_get("created_by").context("missing created_by")?,
        disk_usage_bytes: row.try_get("disk_usage_bytes").unwrap_or(None),
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

    #[tokio::test]
    async fn create_and_list_project() {
        let pool = setup_pool().await;
        let store = ProjectStore::new(pool);

        let project = store
            .create_project(CreateProjectInput {
                agent_id: "agent-1".into(),
                name: "Spacebot".into(),
                description: "The Spacebot monorepo".into(),
                icon: "".into(),
                tags: vec!["rust".into(), "agent".into()],
                root_path: "/home/user/Projects/spacebot".into(),
                settings: Value::Object(Default::default()),
            })
            .await
            .expect("failed to create project");

        assert_eq!(project.name, "Spacebot");
        assert_eq!(project.tags, vec!["rust", "agent"]);
        assert_eq!(project.status, ProjectStatus::Active);

        let projects = store
            .list_projects("agent-1", None)
            .await
            .expect("failed to list projects");
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, project.id);
    }

    #[tokio::test]
    async fn create_repo_and_worktree() {
        let pool = setup_pool().await;
        let store = ProjectStore::new(pool);

        let project = store
            .create_project(CreateProjectInput {
                agent_id: "agent-1".into(),
                name: "Test".into(),
                description: String::new(),
                icon: String::new(),
                tags: vec![],
                root_path: "/tmp/test-project".into(),
                settings: Value::Object(Default::default()),
            })
            .await
            .expect("failed to create project");

        let repo = store
            .create_repo(CreateRepoInput {
                project_id: project.id.clone(),
                name: "spacebot".into(),
                path: "spacebot".into(),
                remote_url: "https://github.com/spacedriveapp/spacebot.git".into(),
                default_branch: "main".into(),
                current_branch: Some("feat/projects".into()),
                description: "Core agent".into(),
            })
            .await
            .expect("failed to create repo");

        assert_eq!(repo.name, "spacebot");

        let worktree = store
            .create_worktree(CreateWorktreeInput {
                project_id: project.id.clone(),
                repo_id: repo.id.clone(),
                name: "feat-projects".into(),
                path: "feat-projects".into(),
                branch: "feat/projects".into(),
                created_by: "user".into(),
            })
            .await
            .expect("failed to create worktree");

        assert_eq!(worktree.branch, "feat/projects");
        assert_eq!(worktree.created_by, "user");

        let with_repos = store
            .list_worktrees_with_repos(&project.id)
            .await
            .expect("failed to list worktrees with repos");
        assert_eq!(with_repos.len(), 1);
        assert_eq!(with_repos[0].repo_name, "spacebot");
    }

    #[tokio::test]
    async fn update_project_status() {
        let pool = setup_pool().await;
        let store = ProjectStore::new(pool);

        let project = store
            .create_project(CreateProjectInput {
                agent_id: "agent-1".into(),
                name: "Test".into(),
                description: String::new(),
                icon: String::new(),
                tags: vec![],
                root_path: "/tmp/test".into(),
                settings: Value::Object(Default::default()),
            })
            .await
            .expect("failed to create project");

        let updated = store
            .update_project(
                "agent-1",
                &project.id,
                UpdateProjectInput {
                    status: Some(ProjectStatus::Archived),
                    ..Default::default()
                },
            )
            .await
            .expect("failed to update project")
            .expect("project not found");

        assert_eq!(updated.status, ProjectStatus::Archived);

        // Filtering by active should return empty.
        let active = store
            .list_projects("agent-1", Some(ProjectStatus::Active))
            .await
            .expect("failed to list");
        assert!(active.is_empty());
    }

    #[tokio::test]
    async fn delete_project_cascades() {
        let pool = setup_pool().await;
        let store = ProjectStore::new(pool);

        let project = store
            .create_project(CreateProjectInput {
                agent_id: "agent-1".into(),
                name: "Test".into(),
                description: String::new(),
                icon: String::new(),
                tags: vec![],
                root_path: "/tmp/cascade-test".into(),
                settings: Value::Object(Default::default()),
            })
            .await
            .expect("failed to create project");

        let repo = store
            .create_repo(CreateRepoInput {
                project_id: project.id.clone(),
                name: "repo".into(),
                path: "repo".into(),
                remote_url: String::new(),
                default_branch: "main".into(),
                current_branch: None,
                description: String::new(),
            })
            .await
            .expect("failed to create repo");

        store
            .create_worktree(CreateWorktreeInput {
                project_id: project.id.clone(),
                repo_id: repo.id.clone(),
                name: "wt".into(),
                path: "wt".into(),
                branch: "feat/x".into(),
                created_by: "agent".into(),
            })
            .await
            .expect("failed to create worktree");

        let deleted = store
            .delete_project("agent-1", &project.id)
            .await
            .expect("failed to delete project");
        assert!(deleted);

        // Repos and worktrees should be gone via CASCADE.
        let repos = store
            .list_repos(&project.id)
            .await
            .expect("failed to list repos");
        assert!(repos.is_empty());

        let worktrees = store
            .list_worktrees(&project.id)
            .await
            .expect("failed to list worktrees");
        assert!(worktrees.is_empty());
    }

    #[tokio::test]
    async fn duplicate_root_path_rejected() {
        let pool = setup_pool().await;
        let store = ProjectStore::new(pool);

        store
            .create_project(CreateProjectInput {
                agent_id: "agent-1".into(),
                name: "First".into(),
                description: String::new(),
                icon: String::new(),
                tags: vec![],
                root_path: "/tmp/unique-path".into(),
                settings: Value::Object(Default::default()),
            })
            .await
            .expect("failed to create first project");

        let result = store
            .create_project(CreateProjectInput {
                agent_id: "agent-1".into(),
                name: "Second".into(),
                description: String::new(),
                icon: String::new(),
                tags: vec![],
                root_path: "/tmp/unique-path".into(),
                settings: Value::Object(Default::default()),
            })
            .await;

        assert!(result.is_err(), "duplicate root_path should fail");
    }
}
