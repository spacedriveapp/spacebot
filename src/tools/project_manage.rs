//! Project management tool for channel and branch processes.
//!
//! Supports actions: list, create, scan, add_repo, create_worktree, remove_worktree, disk_usage.

use crate::projects::git;
use crate::projects::store::{
    CreateProjectInput, CreateRepoInput, CreateWorktreeInput, ProjectStatus, ProjectStore,
};

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ProjectManageTool {
    project_store: Arc<ProjectStore>,
}

impl ProjectManageTool {
    pub fn new(project_store: Arc<ProjectStore>) -> Self {
        Self { project_store }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("project_manage failed: {0}")]
pub struct ProjectManageError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProjectManageArgs {
    /// The operation to perform.
    pub action: String,
    /// Project ID — required for scan, add_repo, create_worktree, remove_worktree, disk_usage.
    #[serde(default)]
    pub project_id: Option<String>,
    /// Project name — used by create.
    #[serde(default)]
    pub name: Option<String>,
    /// Absolute path to the project root — used by create.
    #[serde(default)]
    pub root_path: Option<String>,
    /// Path to a repo directory — used by add_repo (absolute or relative to project root).
    #[serde(default)]
    pub repo_path: Option<String>,
    /// Repo ID — used by create_worktree to select the source repo.
    #[serde(default)]
    pub repo_id: Option<String>,
    /// Branch name — used by create_worktree.
    #[serde(default)]
    pub branch: Option<String>,
    /// Custom worktree directory name — used by create_worktree (defaults to branch name).
    #[serde(default)]
    pub worktree_name: Option<String>,
    /// Worktree ID — used by remove_worktree.
    #[serde(default)]
    pub worktree_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProjectManageOutput {
    pub success: bool,
    pub action: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl Tool for ProjectManageTool {
    const NAME: &'static str = "project_manage";

    type Error = ProjectManageError;
    type Args = ProjectManageArgs;
    type Output = ProjectManageOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: crate::prompts::text::get("tools/project_manage").to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "create", "scan", "add_repo", "create_worktree", "remove_worktree", "disk_usage"],
                        "description": "The operation to perform"
                    },
                    "project_id": {
                        "type": "string",
                        "description": "Project ID — required for scan, add_repo, create_worktree, remove_worktree, disk_usage"
                    },
                    "name": {
                        "type": "string",
                        "description": "Project name — used by create"
                    },
                    "root_path": {
                        "type": "string",
                        "description": "Absolute path to the project root — used by create"
                    },
                    "repo_path": {
                        "type": "string",
                        "description": "Path to a repo directory within the project — used by add_repo"
                    },
                    "repo_id": {
                        "type": "string",
                        "description": "Repo ID — used by create_worktree to select the source repo"
                    },
                    "branch": {
                        "type": "string",
                        "description": "Branch name — used by create_worktree"
                    },
                    "worktree_name": {
                        "type": "string",
                        "description": "Custom worktree directory name — used by create_worktree (defaults to branch name)"
                    },
                    "worktree_id": {
                        "type": "string",
                        "description": "Worktree ID — used by remove_worktree"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        match args.action.as_str() {
            "list" => self.handle_list().await,
            "create" => self.handle_create(args).await,
            "scan" => self.handle_scan(args).await,
            "add_repo" => self.handle_add_repo(args).await,
            "create_worktree" => self.handle_create_worktree(args).await,
            "remove_worktree" => self.handle_remove_worktree(args).await,
            "disk_usage" => self.handle_disk_usage(args).await,
            other => Ok(ProjectManageOutput {
                success: false,
                action: other.to_string(),
                message: format!(
                    "Unknown action '{other}'. Valid actions: list, create, scan, add_repo, \
                     create_worktree, remove_worktree, disk_usage"
                ),
                data: None,
            }),
        }
    }
}

impl ProjectManageTool {
    async fn handle_list(&self) -> Result<ProjectManageOutput, ProjectManageError> {
        let projects = self
            .project_store
            .list_projects(Some(ProjectStatus::Active))
            .await
            .map_err(|error| ProjectManageError(format!("failed to list projects: {error}")))?;

        let mut results = Vec::with_capacity(projects.len());
        for project in &projects {
            let repos = self
                .project_store
                .list_repos(&project.id)
                .await
                .unwrap_or_default();
            let worktrees = self
                .project_store
                .list_worktrees_with_repos(&project.id)
                .await
                .unwrap_or_default();

            results.push(serde_json::json!({
                "id": project.id,
                "name": project.name,
                "root_path": project.root_path,
                "description": project.description,
                "tags": project.tags,
                "repo_count": repos.len(),
                "worktree_count": worktrees.len(),
                "repos": repos.iter().map(|repo| serde_json::json!({
                    "id": repo.id,
                    "name": repo.name,
                    "path": repo.path,
                    "default_branch": repo.default_branch,
                    "remote_url": repo.remote_url,
                })).collect::<Vec<_>>(),
                "worktrees": worktrees.iter().map(|worktree| serde_json::json!({
                    "id": worktree.worktree.id,
                    "name": worktree.worktree.name,
                    "path": worktree.worktree.path,
                    "branch": worktree.worktree.branch,
                    "repo_name": worktree.repo_name,
                })).collect::<Vec<_>>(),
            }));
        }

        let count = results.len();
        Ok(ProjectManageOutput {
            success: true,
            action: "list".to_string(),
            message: format!("{count} active project(s)"),
            data: Some(serde_json::json!(results)),
        })
    }

    async fn handle_create(
        &self,
        args: ProjectManageArgs,
    ) -> Result<ProjectManageOutput, ProjectManageError> {
        let name = args
            .name
            .ok_or_else(|| ProjectManageError("'name' is required for create".into()))?;
        let root_path = args
            .root_path
            .ok_or_else(|| ProjectManageError("'root_path' is required for create".into()))?;

        let root = Path::new(&root_path);
        if !root.is_dir() {
            return Ok(ProjectManageOutput {
                success: false,
                action: "create".to_string(),
                message: format!("Directory does not exist: {root_path}"),
                data: None,
            });
        }

        let project = self
            .project_store
            .create_project(CreateProjectInput {
                name: name.clone(),
                description: String::new(),
                icon: String::new(),
                tags: Vec::new(),
                root_path: root_path.clone(),
                settings: serde_json::json!({}),
            })
            .await
            .map_err(|error| ProjectManageError(format!("failed to create project: {error}")))?;

        // Auto-discover repos
        let discovered = git::discover_repos(root).await.unwrap_or_default();
        let mut repo_count = 0;
        let mut worktree_count = 0;

        for repo_info in &discovered {
            let result = self
                .project_store
                .create_repo(CreateRepoInput {
                    project_id: project.id.clone(),
                    name: repo_info.name.clone(),
                    path: repo_info.relative_path.clone(),
                    remote_url: repo_info.remote_url.clone(),
                    default_branch: repo_info.default_branch.clone(),
                    current_branch: repo_info.current_branch.clone(),
                    description: String::new(),
                })
                .await;

            if let Ok(repo) = result {
                repo_count += 1;

                // Discover worktrees for this repo
                let is_root_repo = repo_info.relative_path == ".";
                let repo_abs_path = root.join(&repo_info.relative_path);
                if let Ok(worktrees) = git::list_worktrees(&repo_abs_path).await {
                    for worktree_info in worktrees {
                        let worktree_name = worktree_info
                            .path
                            .file_name()
                            .map(|name| name.to_string_lossy().to_string())
                            .unwrap_or_else(|| worktree_info.branch.replace('/', "-"));

                        // For root repos (path == "."), worktrees live in the parent
                        // directory. Store as `../name` relative to root.
                        let relative_path = if is_root_repo {
                            let parent = root.parent();
                            parent
                                .and_then(|p| worktree_info.path.strip_prefix(p).ok())
                                .map(|p| format!("../{}", p.to_string_lossy()))
                                .unwrap_or_else(|| worktree_info.path.to_string_lossy().to_string())
                        } else {
                            worktree_info
                                .path
                                .strip_prefix(root)
                                .map(|path| path.to_string_lossy().to_string())
                                .unwrap_or_else(|_| worktree_name.clone())
                        };

                        let worktree_result = self
                            .project_store
                            .create_worktree(CreateWorktreeInput {
                                project_id: project.id.clone(),
                                repo_id: repo.id.clone(),
                                name: worktree_name,
                                path: relative_path,
                                branch: worktree_info.branch.clone(),
                                created_by: "user".to_string(),
                            })
                            .await;
                        if worktree_result.is_ok() {
                            worktree_count += 1;
                        }
                    }
                }
            }
        }

        Ok(ProjectManageOutput {
            success: true,
            action: "create".to_string(),
            message: format!(
                "Created project '{name}' at {root_path} — discovered {repo_count} repo(s), \
                 {worktree_count} worktree(s)"
            ),
            data: Some(serde_json::json!({
                "project_id": project.id,
                "repo_count": repo_count,
                "worktree_count": worktree_count,
            })),
        })
    }

    async fn handle_scan(
        &self,
        args: ProjectManageArgs,
    ) -> Result<ProjectManageOutput, ProjectManageError> {
        let project_id = args
            .project_id
            .ok_or_else(|| ProjectManageError("'project_id' is required for scan".into()))?;

        let project = self
            .project_store
            .get_project(&project_id)
            .await
            .map_err(|error| ProjectManageError(format!("failed to get project: {error}")))?
            .ok_or_else(|| ProjectManageError(format!("project not found: {project_id}")))?;

        let root = Path::new(&project.root_path);

        // Discover repos on disk
        let discovered = git::discover_repos(root).await.unwrap_or_default();
        let existing_repos = self
            .project_store
            .list_repos(&project_id)
            .await
            .unwrap_or_default();

        let mut new_repos = 0;
        let mut new_worktrees = 0;

        for repo_info in &discovered {
            if let Some(existing) = existing_repos
                .iter()
                .find(|r| r.path == repo_info.relative_path)
            {
                // Refresh current_branch on existing repos.
                let _ = self
                    .project_store
                    .update_repo_current_branch(&existing.id, repo_info.current_branch.as_deref())
                    .await;
                continue;
            }

            let result = self
                .project_store
                .create_repo(CreateRepoInput {
                    project_id: project_id.clone(),
                    name: repo_info.name.clone(),
                    path: repo_info.relative_path.clone(),
                    remote_url: repo_info.remote_url.clone(),
                    default_branch: repo_info.default_branch.clone(),
                    current_branch: repo_info.current_branch.clone(),
                    description: String::new(),
                })
                .await;

            if result.is_ok() {
                new_repos += 1;
            }
        }

        // Re-discover worktrees for all repos
        let all_repos = self
            .project_store
            .list_repos(&project_id)
            .await
            .unwrap_or_default();

        for repo in &all_repos {
            let repo_abs_path = root.join(&repo.path);
            let is_root_repo = repo.path == ".";
            if let Ok(worktrees) = git::list_worktrees(&repo_abs_path).await {
                let existing_worktrees = self
                    .project_store
                    .list_worktrees_for_repo(&repo.id)
                    .await
                    .unwrap_or_default();

                let existing_worktree_branches: std::collections::HashSet<&str> =
                    existing_worktrees
                        .iter()
                        .map(|worktree| worktree.branch.as_str())
                        .collect();

                for worktree_info in worktrees {
                    if existing_worktree_branches.contains(worktree_info.branch.as_str()) {
                        continue;
                    }

                    let worktree_name = worktree_info
                        .path
                        .file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_else(|| worktree_info.branch.replace('/', "-"));

                    let relative_path = if is_root_repo {
                        let parent = root.parent();
                        parent
                            .and_then(|p| worktree_info.path.strip_prefix(p).ok())
                            .map(|p| format!("../{}", p.to_string_lossy()))
                            .unwrap_or_else(|| worktree_info.path.to_string_lossy().to_string())
                    } else {
                        worktree_info
                            .path
                            .strip_prefix(root)
                            .map(|path| path.to_string_lossy().to_string())
                            .unwrap_or_else(|_| worktree_name.clone())
                    };

                    let result = self
                        .project_store
                        .create_worktree(CreateWorktreeInput {
                            project_id: project_id.clone(),
                            repo_id: repo.id.clone(),
                            name: worktree_name,
                            path: relative_path,
                            branch: worktree_info.branch.clone(),
                            created_by: "user".to_string(),
                        })
                        .await;
                    if result.is_ok() {
                        new_worktrees += 1;
                    }
                }
            }
        }

        Ok(ProjectManageOutput {
            success: true,
            action: "scan".to_string(),
            message: format!(
                "Scanned project '{}' — found {new_repos} new repo(s), {new_worktrees} new \
                 worktree(s)",
                project.name
            ),
            data: Some(serde_json::json!({
                "new_repos": new_repos,
                "new_worktrees": new_worktrees,
            })),
        })
    }

    async fn handle_add_repo(
        &self,
        args: ProjectManageArgs,
    ) -> Result<ProjectManageOutput, ProjectManageError> {
        let project_id = args
            .project_id
            .ok_or_else(|| ProjectManageError("'project_id' is required for add_repo".into()))?;
        let repo_path = args
            .repo_path
            .ok_or_else(|| ProjectManageError("'repo_path' is required for add_repo".into()))?;

        let project = self
            .project_store
            .get_project(&project_id)
            .await
            .map_err(|error| ProjectManageError(format!("failed to get project: {error}")))?
            .ok_or_else(|| ProjectManageError(format!("project not found: {project_id}")))?;

        let root = Path::new(&project.root_path);

        // Resolve to absolute path — must end up inside the project root.
        let abs_path = if Path::new(&repo_path).is_absolute() {
            std::path::PathBuf::from(&repo_path)
        } else {
            root.join(&repo_path)
        };

        // Canonicalize to resolve any `..` and ensure the path is within the project root.
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let canonical_abs = abs_path.canonicalize().unwrap_or_else(|_| abs_path.clone());
        if !canonical_abs.starts_with(&canonical_root) {
            return Ok(ProjectManageOutput {
                success: false,
                action: "add_repo".to_string(),
                message: "repo path must be within the project root".to_string(),
                data: None,
            });
        }

        if !abs_path.is_dir() {
            return Ok(ProjectManageOutput {
                success: false,
                action: "add_repo".to_string(),
                message: format!("Directory does not exist: {}", abs_path.display()),
                data: None,
            });
        }

        if !abs_path.join(".git").exists() {
            return Ok(ProjectManageOutput {
                success: false,
                action: "add_repo".to_string(),
                message: format!("Not a git repository: {}", abs_path.display()),
                data: None,
            });
        }

        // Compute relative path from project root. When the repo IS the project
        // root (single-repo project), use "." as the canonical relative path.
        let relative_path = canonical_abs
            .strip_prefix(&canonical_root)
            .map(|path| {
                let s = path.to_string_lossy().to_string();
                if s.is_empty() { ".".to_string() } else { s }
            })
            .unwrap_or_else(|_| repo_path.clone());

        let name = abs_path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| relative_path.clone());

        // Discover a single repo to get remote URL and default branch.
        // We re-use discover_repos on the parent — if the parent is the project
        // root, we match by name. Otherwise fall back to defaults.
        let discovered = git::discover_repos(Path::new(&project.root_path))
            .await
            .unwrap_or_default();
        let repo_meta = discovered
            .iter()
            .find(|repo| repo.relative_path == relative_path);

        let remote_url = repo_meta
            .map(|meta| meta.remote_url.clone())
            .unwrap_or_default();
        let default_branch = repo_meta
            .map(|meta| meta.default_branch.clone())
            .unwrap_or_else(|| "main".to_string());
        let current_branch = repo_meta.and_then(|meta| meta.current_branch.clone());

        let repo = self
            .project_store
            .create_repo(CreateRepoInput {
                project_id: project_id.clone(),
                name: name.clone(),
                path: relative_path,
                remote_url,
                default_branch,
                current_branch,
                description: String::new(),
            })
            .await
            .map_err(|error| ProjectManageError(format!("failed to add repo: {error}")))?;

        Ok(ProjectManageOutput {
            success: true,
            action: "add_repo".to_string(),
            message: format!("Added repo '{name}' to project '{}'", project.name),
            data: Some(serde_json::json!({ "repo_id": repo.id })),
        })
    }

    async fn handle_create_worktree(
        &self,
        args: ProjectManageArgs,
    ) -> Result<ProjectManageOutput, ProjectManageError> {
        let project_id = args.project_id.ok_or_else(|| {
            ProjectManageError("'project_id' is required for create_worktree".into())
        })?;
        let repo_id = args.repo_id.ok_or_else(|| {
            ProjectManageError("'repo_id' is required for create_worktree".into())
        })?;
        let branch = args
            .branch
            .ok_or_else(|| ProjectManageError("'branch' is required for create_worktree".into()))?;

        let project = self
            .project_store
            .get_project(&project_id)
            .await
            .map_err(|error| ProjectManageError(format!("failed to get project: {error}")))?
            .ok_or_else(|| ProjectManageError(format!("project not found: {project_id}")))?;

        let repo = self
            .project_store
            .get_repo(&repo_id)
            .await
            .map_err(|error| ProjectManageError(format!("failed to get repo: {error}")))?
            .ok_or_else(|| ProjectManageError(format!("repo not found: {repo_id}")))?;

        // Verify the repo belongs to this project.
        if repo.project_id != project_id {
            return Ok(ProjectManageOutput {
                success: false,
                action: "create_worktree".to_string(),
                message: format!("repo '{repo_id}' does not belong to project '{project_id}'"),
                data: None,
            });
        }

        let worktree_dir_name = args
            .worktree_name
            .unwrap_or_else(|| branch.replace('/', "-"));

        // Sanitize the worktree name — must be a single path segment, no traversal.
        if worktree_dir_name.is_empty()
            || worktree_dir_name.contains('/')
            || worktree_dir_name.contains('\\')
            || worktree_dir_name == ".."
            || worktree_dir_name == "."
        {
            return Ok(ProjectManageOutput {
                success: false,
                action: "create_worktree".to_string(),
                message: format!(
                    "invalid worktree name '{worktree_dir_name}': must be a single directory name"
                ),
                data: None,
            });
        }

        let root = Path::new(&project.root_path);
        let repo_abs_path = root.join(&repo.path);
        let is_single_repo = repo.path == ".";

        // For single-repo projects, place worktrees in the parent directory
        // (as siblings of the repo). For multi-repo projects, place them
        // inside the project root.
        let (worktree_abs_path, worktree_db_path) = if is_single_repo {
            let parent = root.parent().ok_or_else(|| {
                ProjectManageError("single-repo project root has no parent directory".into())
            })?;
            (
                parent.join(&worktree_dir_name),
                format!("../{worktree_dir_name}"),
            )
        } else {
            (root.join(&worktree_dir_name), worktree_dir_name.clone())
        };

        // Create the git worktree (branch from HEAD of the repo)
        git::create_worktree(&repo_abs_path, &worktree_abs_path, &branch, None)
            .await
            .map_err(|error| ProjectManageError(format!("git worktree add failed: {error}")))?;

        // Register in the database
        let worktree = self
            .project_store
            .create_worktree(CreateWorktreeInput {
                project_id: project_id.clone(),
                repo_id: repo_id.clone(),
                name: worktree_dir_name.clone(),
                path: worktree_db_path,
                branch: branch.clone(),
                created_by: "agent".to_string(),
            })
            .await
            .map_err(|error| {
                ProjectManageError(format!("failed to register worktree in database: {error}"))
            })?;

        Ok(ProjectManageOutput {
            success: true,
            action: "create_worktree".to_string(),
            message: format!(
                "Created worktree '{worktree_dir_name}' (branch: {branch}) from repo '{}' at {}",
                repo.name,
                worktree_abs_path.display()
            ),
            data: Some(serde_json::json!({
                "worktree_id": worktree.id,
                "path": worktree_abs_path.to_string_lossy(),
            })),
        })
    }

    async fn handle_remove_worktree(
        &self,
        args: ProjectManageArgs,
    ) -> Result<ProjectManageOutput, ProjectManageError> {
        let project_id = args.project_id.ok_or_else(|| {
            ProjectManageError("'project_id' is required for remove_worktree".into())
        })?;
        let worktree_id = args.worktree_id.ok_or_else(|| {
            ProjectManageError("'worktree_id' is required for remove_worktree".into())
        })?;

        let project = self
            .project_store
            .get_project(&project_id)
            .await
            .map_err(|error| ProjectManageError(format!("failed to get project: {error}")))?
            .ok_or_else(|| ProjectManageError(format!("project not found: {project_id}")))?;

        let worktree = self
            .project_store
            .get_worktree(&worktree_id)
            .await
            .map_err(|error| ProjectManageError(format!("failed to get worktree: {error}")))?
            .ok_or_else(|| ProjectManageError(format!("worktree not found: {worktree_id}")))?;

        // Verify the worktree belongs to this project.
        if worktree.project_id != project_id {
            return Ok(ProjectManageOutput {
                success: false,
                action: "remove_worktree".to_string(),
                message: format!(
                    "worktree '{worktree_id}' does not belong to project '{project_id}'"
                ),
                data: None,
            });
        }

        let repo = self
            .project_store
            .get_repo(&worktree.repo_id)
            .await
            .map_err(|error| ProjectManageError(format!("failed to get repo: {error}")))?
            .ok_or_else(|| {
                ProjectManageError(format!("source repo not found: {}", worktree.repo_id))
            })?;

        let root = Path::new(&project.root_path);
        let repo_abs_path = root.join(&repo.path);
        let worktree_abs_path = root.join(&worktree.path);

        // Remove the git worktree — only delete the DB record if the directory
        // is already gone or git removal succeeds.
        if worktree_abs_path.exists() {
            git::remove_worktree(&repo_abs_path, &worktree_abs_path)
                .await
                .map_err(|error| {
                    ProjectManageError(format!("git worktree remove failed: {error}"))
                })?;
        }

        // Delete from database.
        self.project_store
            .delete_worktree(&worktree_id)
            .await
            .map_err(|error| {
                ProjectManageError(format!("failed to delete worktree from database: {error}"))
            })?;

        Ok(ProjectManageOutput {
            success: true,
            action: "remove_worktree".to_string(),
            message: format!(
                "Removed worktree '{}' (branch: {}) from project '{}'",
                worktree.name, worktree.branch, project.name
            ),
            data: None,
        })
    }

    async fn handle_disk_usage(
        &self,
        args: ProjectManageArgs,
    ) -> Result<ProjectManageOutput, ProjectManageError> {
        let project_id = args
            .project_id
            .ok_or_else(|| ProjectManageError("'project_id' is required for disk_usage".into()))?;

        let project = self
            .project_store
            .get_project(&project_id)
            .await
            .map_err(|error| ProjectManageError(format!("failed to get project: {error}")))?
            .ok_or_else(|| ProjectManageError(format!("project not found: {project_id}")))?;

        let root = Path::new(&project.root_path);
        if !root.is_dir() {
            return Ok(ProjectManageOutput {
                success: false,
                action: "disk_usage".to_string(),
                message: format!("Project root does not exist: {}", project.root_path),
                data: None,
            });
        }

        // Calculate disk usage per top-level entry (async IO, no symlink following).
        let mut entries = Vec::new();
        let mut total_bytes: u64 = 0;

        if let Ok(mut read_dir) = tokio::fs::read_dir(root).await {
            while let Ok(Some(entry)) = read_dir.next_entry().await {
                let metadata = match tokio::fs::symlink_metadata(entry.path()).await {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                // Skip symlinks entirely.
                if metadata.is_symlink() {
                    continue;
                }
                let is_dir = metadata.is_dir();
                let size = if is_dir {
                    dir_size_async(&entry.path()).await
                } else {
                    metadata.len()
                };
                total_bytes += size;
                entries.push(serde_json::json!({
                    "name": entry.file_name().to_string_lossy(),
                    "bytes": size,
                    "is_dir": is_dir,
                }));
            }
        }

        // Sort by size descending
        entries.sort_by(|a, b| {
            let size_a = a["bytes"].as_u64().unwrap_or(0);
            let size_b = b["bytes"].as_u64().unwrap_or(0);
            size_b.cmp(&size_a)
        });

        let human_total = format_bytes(total_bytes);

        Ok(ProjectManageOutput {
            success: true,
            action: "disk_usage".to_string(),
            message: format!("Project '{}' uses {human_total}", project.name),
            data: Some(serde_json::json!({
                "total_bytes": total_bytes,
                "total_human": human_total,
                "entries": entries,
            })),
        })
    }
}

/// Recursively calculate directory size in bytes (async, no symlink following).
async fn dir_size_async(path: &Path) -> u64 {
    let mut total: u64 = 0;
    let mut stack = vec![path.to_path_buf()];

    while let Some(current) = stack.pop() {
        let mut entries = match tokio::fs::read_dir(&current).await {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let metadata = match tokio::fs::symlink_metadata(entry.path()).await {
                Ok(m) => m,
                Err(_) => continue,
            };
            if metadata.is_dir() {
                stack.push(entry.path());
            } else if metadata.is_file() {
                total += metadata.len();
            }
            // Skip symlinks.
        }
    }
    total
}

/// Format bytes as a human-readable string.
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}
