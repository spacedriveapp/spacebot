//! Code graph tools for agent use.
//!
//! Provides read/write tools for querying, searching, and navigating
//! the code graph. These tools are added to worker and branch tool servers.

use std::sync::Arc;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::codegraph::CodeGraphManager;

// ---------------------------------------------------------------------------
// codegraph_query — search the code graph
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CodeGraphQueryTool {
    manager: Arc<CodeGraphManager>,
}

impl CodeGraphQueryTool {
    pub fn new(manager: Arc<CodeGraphManager>) -> Self {
        Self { manager }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("codegraph_query failed: {0}")]
pub struct CodeGraphQueryError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeGraphQueryArgs {
    /// Natural language query to search the code graph.
    pub query: String,
    /// Project ID to search within. Use codegraph_list_projects to get available IDs.
    pub project_id: String,
    /// Maximum results to return (default: 20, max: 100).
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    20
}

#[derive(Debug, Serialize)]
pub struct CodeGraphQueryOutput {
    pub success: bool,
    pub results: Vec<Value>,
    pub total: usize,
}

impl Tool for CodeGraphQueryTool {
    const NAME: &'static str = "codegraph_query";

    type Error = CodeGraphQueryError;
    type Args = CodeGraphQueryArgs;
    type Output = CodeGraphQueryOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Search the code graph for symbols, functions, classes, and other code entities. Returns ranked results with file locations and context.".to_string(),
            parameters: schemars::schema_for!(CodeGraphQueryArgs).to_value(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let db = self
            .manager
            .get_db(&args.project_id)
            .await
            .ok_or_else(|| {
                CodeGraphQueryError(format!("project '{}' not found", args.project_id))
            })?;

        let limit = args.limit.min(100);

        let results = crate::codegraph::search::hybrid_search(
            &args.project_id,
            &args.query,
            limit,
            &db,
        )
        .await
        .map_err(|e| CodeGraphQueryError(e.to_string()))?;

        let total = results.len();
        let json_results: Vec<Value> = results
            .iter()
            .map(|r| serde_json::to_value(r).unwrap_or_default())
            .collect();

        Ok(CodeGraphQueryOutput {
            success: true,
            results: json_results,
            total,
        })
    }
}

// ---------------------------------------------------------------------------
// codegraph_list_projects — list all indexed projects
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CodeGraphListProjectsTool {
    manager: Arc<CodeGraphManager>,
}

impl CodeGraphListProjectsTool {
    pub fn new(manager: Arc<CodeGraphManager>) -> Self {
        Self { manager }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("codegraph_list_projects failed: {0}")]
pub struct CodeGraphListProjectsError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeGraphListProjectsArgs {}

#[derive(Debug, Serialize)]
pub struct CodeGraphListProjectsOutput {
    pub projects: Vec<Value>,
}

impl Tool for CodeGraphListProjectsTool {
    const NAME: &'static str = "codegraph_list_projects";

    type Error = CodeGraphListProjectsError;
    type Args = CodeGraphListProjectsArgs;
    type Output = CodeGraphListProjectsOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "List all indexed projects with their status, stats, and available graph data.".to_string(),
            parameters: schemars::schema_for!(CodeGraphListProjectsArgs).to_value(),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        let projects = self.manager.list_projects().await;
        let json_projects: Vec<Value> = projects
            .iter()
            .map(|p| serde_json::to_value(p).unwrap_or_default())
            .collect();

        Ok(CodeGraphListProjectsOutput {
            projects: json_projects,
        })
    }
}

// ---------------------------------------------------------------------------
// codegraph_get_files_for_task — targeted file list for a task
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CodeGraphGetFilesForTaskTool {
    manager: Arc<CodeGraphManager>,
}

impl CodeGraphGetFilesForTaskTool {
    pub fn new(manager: Arc<CodeGraphManager>) -> Self {
        Self { manager }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("codegraph_get_files_for_task failed: {0}")]
pub struct CodeGraphGetFilesError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeGraphGetFilesArgs {
    /// Description of the task to find relevant files for.
    pub task_description: String,
    /// Project ID to search within.
    pub project_id: String,
    /// Maximum primary files to return (default: 20).
    #[serde(default = "default_max_files")]
    pub max_files: usize,
    /// Include test files in results (default: true).
    #[serde(default = "default_true")]
    pub include_tests: bool,
}

fn default_max_files() -> usize {
    20
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize)]
pub struct CodeGraphGetFilesOutput {
    pub primary_files: Vec<String>,
    pub secondary_files: Vec<String>,
    pub community: Option<String>,
    pub confidence: f64,
}

impl Tool for CodeGraphGetFilesForTaskTool {
    const NAME: &'static str = "codegraph_get_files_for_task";

    type Error = CodeGraphGetFilesError;
    type Args = CodeGraphGetFilesArgs;
    type Output = CodeGraphGetFilesOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Given a task description, query the code graph to identify which files need to be read or modified. Returns primary files (direct matches) and secondary files (callers/importers). Use this BEFORE dispatching a worker to provide it with a targeted file list.".to_string(),
            parameters: schemars::schema_for!(CodeGraphGetFilesArgs).to_value(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let db = self
            .manager
            .get_db(&args.project_id)
            .await
            .ok_or_else(|| {
                CodeGraphGetFilesError(format!("project '{}' not found", args.project_id))
            })?;

        // Search for symbols matching the task description.
        let results = crate::codegraph::search::hybrid_search(
            &args.project_id,
            &args.task_description,
            args.max_files,
            &db,
        )
        .await
        .map_err(|e| CodeGraphGetFilesError(e.to_string()))?;

        // Extract unique file paths from results.
        let primary_files: Vec<String> = results
            .iter()
            .filter_map(|r| r.source_file.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .take(args.max_files)
            .collect();

        // Community from top result.
        let community = results.first().and_then(|r| r.community.clone());

        // Confidence based on top result score.
        let confidence = results.first().map(|r| r.score).unwrap_or(0.0);

        Ok(CodeGraphGetFilesOutput {
            primary_files,
            secondary_files: Vec::new(), // Will be populated via graph traversal later.
            community,
            confidence,
        })
    }
}
