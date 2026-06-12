//! One-time migration of per-agent project data to the instance database.
//!
//! Scans each agent's per-agent DB for rows in the `projects`,
//! `project_repos`, and `project_worktrees` tables and inserts them into the
//! shared instance database. Deduplicates projects by `root_path` — the first
//! agent encountered for a given path wins. Migration is idempotent via a
//! marker file.

use anyhow::Context as _;
use sqlx::{Row as _, SqlitePool};

use std::path::{Path, PathBuf};

const MIGRATION_MARKER: &str = ".projects_migrated";

/// Migrate all per-agent projects/repos/worktrees to the instance database.
pub async fn migrate_legacy_projects(
    instance_dir: &Path,
    instance_pool: &SqlitePool,
) -> anyhow::Result<()> {
    let data_dir = instance_dir.join("data");
    let marker_path = data_dir.join(MIGRATION_MARKER);

    if marker_path.exists() {
        tracing::debug!("instance project migration already completed, skipping");
        return Ok(());
    }

    let agents_dir = instance_dir.join("agents");
    if !agents_dir.exists() {
        tracing::debug!("no agents directory found, skipping project migration");
        write_marker(&marker_path)?;
        return Ok(());
    }

    let mut total_projects = 0u64;
    let mut total_repos = 0u64;
    let mut total_worktrees = 0u64;

    let entries = std::fs::read_dir(&agents_dir)
        .with_context(|| format!("failed to read agents directory: {}", agents_dir.display()))?;

    for entry in entries {
        let entry = entry.context("failed to read agents directory entry")?;
        let agent_dir = entry.path();
        if !agent_dir.is_dir() {
            continue;
        }

        let agent_id = agent_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown")
            .to_string();

        // Try new name first (agent.db), fall back to legacy (spacebot.db).
        let db_path = find_agent_db(&agent_dir);
        let Some(db_path) = db_path else { continue };

        match migrate_agent_projects(&db_path, instance_pool).await {
            Ok((projects, repos, worktrees)) => {
                if projects + repos + worktrees > 0 {
                    tracing::info!(
                        agent_id,
                        projects,
                        repos,
                        worktrees,
                        "migrated legacy projects to instance database"
                    );
                }
                total_projects += projects;
                total_repos += repos;
                total_worktrees += worktrees;
            }
            Err(error) => {
                tracing::error!(
                    agent_id,
                    %error,
                    "failed to migrate projects for agent — migration incomplete"
                );
                return Err(error);
            }
        }
    }

    write_marker(&marker_path)?;

    if total_projects + total_repos + total_worktrees > 0 {
        tracing::info!(
            total_projects,
            total_repos,
            total_worktrees,
            "legacy project migration complete"
        );
    }

    Ok(())
}

fn find_agent_db(agent_dir: &Path) -> Option<PathBuf> {
    let data_dir = agent_dir.join("data");
    let new_db = data_dir.join("agent.db");
    if new_db.exists() {
        return Some(new_db);
    }
    let legacy_db = data_dir.join("spacebot.db");
    if legacy_db.exists() {
        return Some(legacy_db);
    }
    None
}

async fn migrate_agent_projects(
    db_path: &Path,
    instance_pool: &SqlitePool,
) -> anyhow::Result<(u64, u64, u64)> {
    let url = format!("sqlite:{}?mode=ro", db_path.display());
    let agent_pool = SqlitePool::connect(&url)
        .await
        .with_context(|| format!("failed to connect to agent database: {}", db_path.display()))?;

    let table_exists: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'projects'",
    )
    .fetch_one(&agent_pool)
    .await
    .unwrap_or(false);

    if !table_exists {
        agent_pool.close().await;
        return Ok((0, 0, 0));
    }

    // Read columns — handle both old schema (agent_id) and new schema (post-migration).
    let rows = sqlx::query("SELECT * FROM projects")
        .fetch_all(&agent_pool)
        .await
        .context("failed to read legacy projects")?;

    let mut migrated_projects = 0u64;
    let mut migrated_repos = 0u64;
    let mut migrated_worktrees = 0u64;

    for row in &rows {
        let id: String = row.try_get("id").context("missing project id")?;
        let root_path: String = row.try_get("root_path").context("missing root_path")?;

        // Skip if this project (by id) or root_path already exists in instance DB.
        let already_exists: bool =
            sqlx::query_scalar("SELECT COUNT(*) > 0 FROM projects WHERE id = ? OR root_path = ?")
                .bind(&id)
                .bind(&root_path)
                .fetch_one(instance_pool)
                .await
                .unwrap_or(false);

        if already_exists {
            continue;
        }

        let name: String = row.try_get("name").unwrap_or_default();
        let description: String = row.try_get("description").unwrap_or_default();
        let icon: String = row.try_get("icon").unwrap_or_default();
        let tags: String = row.try_get("tags").unwrap_or_else(|_| "[]".to_string());
        let logo_path: Option<String> = row.try_get("logo_path").unwrap_or(None);
        let settings: String = row.try_get("settings").unwrap_or_else(|_| "{}".to_string());
        let status: String = row
            .try_get("status")
            .unwrap_or_else(|_| "active".to_string());
        let created_at: String = row.try_get("created_at").unwrap_or_default();
        let updated_at: String = row.try_get("updated_at").unwrap_or_default();

        sqlx::query(
            "INSERT INTO projects (id, name, description, icon, tags, root_path, logo_path, settings, status, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&name)
        .bind(&description)
        .bind(&icon)
        .bind(&tags)
        .bind(&root_path)
        .bind(&logo_path)
        .bind(&settings)
        .bind(&status)
        .bind(&created_at)
        .bind(&updated_at)
        .execute(instance_pool)
        .await
        .with_context(|| format!("failed to insert migrated project: {id}"))?;

        migrated_projects += 1;

        // Copy repos for this project.
        let repo_rows = sqlx::query("SELECT * FROM project_repos WHERE project_id = ?")
            .bind(&id)
            .fetch_all(&agent_pool)
            .await
            .context("failed to read legacy repos")?;

        for rr in &repo_rows {
            let repo_id: String = rr.try_get("id").context("missing repo id")?;
            let exists: bool =
                sqlx::query_scalar("SELECT COUNT(*) > 0 FROM project_repos WHERE id = ?")
                    .bind(&repo_id)
                    .fetch_one(instance_pool)
                    .await
                    .unwrap_or(false);
            if exists {
                continue;
            }

            sqlx::query(
                "INSERT INTO project_repos (id, project_id, name, path, remote_url, default_branch, current_branch, description, disk_usage_bytes, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&repo_id)
            .bind(&id)
            .bind(rr.try_get::<String, _>("name").unwrap_or_default())
            .bind(rr.try_get::<String, _>("path").unwrap_or_default())
            .bind(rr.try_get::<String, _>("remote_url").unwrap_or_default())
            .bind(rr.try_get::<String, _>("default_branch").unwrap_or_else(|_| "main".to_string()))
            .bind(rr.try_get::<Option<String>, _>("current_branch").unwrap_or(None))
            .bind(rr.try_get::<String, _>("description").unwrap_or_default())
            .bind(rr.try_get::<Option<i64>, _>("disk_usage_bytes").unwrap_or(None))
            .bind(rr.try_get::<String, _>("created_at").unwrap_or_default())
            .bind(rr.try_get::<String, _>("updated_at").unwrap_or_default())
            .execute(instance_pool)
            .await
            .with_context(|| format!("failed to insert migrated repo: {repo_id}"))?;

            migrated_repos += 1;
        }

        // Copy worktrees for this project.
        let wt_rows = sqlx::query("SELECT * FROM project_worktrees WHERE project_id = ?")
            .bind(&id)
            .fetch_all(&agent_pool)
            .await
            .context("failed to read legacy worktrees")?;

        for wr in &wt_rows {
            let wt_id: String = wr.try_get("id").context("missing worktree id")?;
            let exists: bool =
                sqlx::query_scalar("SELECT COUNT(*) > 0 FROM project_worktrees WHERE id = ?")
                    .bind(&wt_id)
                    .fetch_one(instance_pool)
                    .await
                    .unwrap_or(false);
            if exists {
                continue;
            }

            sqlx::query(
                "INSERT INTO project_worktrees (id, project_id, repo_id, name, path, branch, created_by, disk_usage_bytes, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&wt_id)
            .bind(&id)
            .bind(wr.try_get::<String, _>("repo_id").unwrap_or_default())
            .bind(wr.try_get::<String, _>("name").unwrap_or_default())
            .bind(wr.try_get::<String, _>("path").unwrap_or_default())
            .bind(wr.try_get::<String, _>("branch").unwrap_or_default())
            .bind(wr.try_get::<String, _>("created_by").unwrap_or_else(|_| "user".to_string()))
            .bind(wr.try_get::<Option<i64>, _>("disk_usage_bytes").unwrap_or(None))
            .bind(wr.try_get::<String, _>("created_at").unwrap_or_default())
            .bind(wr.try_get::<String, _>("updated_at").unwrap_or_default())
            .execute(instance_pool)
            .await
            .with_context(|| format!("failed to insert migrated worktree: {wt_id}"))?;

            migrated_worktrees += 1;
        }
    }

    agent_pool.close().await;

    Ok((migrated_projects, migrated_repos, migrated_worktrees))
}

fn write_marker(path: &Path) -> anyhow::Result<()> {
    std::fs::write(path, "migrated")
        .with_context(|| format!("failed to write migration marker: {}", path.display()))
}
