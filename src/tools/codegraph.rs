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
use crate::codegraph::tools as cg_tools;

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
            secondary_files: Vec::new(),
            community,
            confidence,
        })
    }
}

// ---------------------------------------------------------------------------
// codegraph_context — 360° symbol view
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CodeGraphContextTool {
    manager: Arc<CodeGraphManager>,
}

impl CodeGraphContextTool {
    pub fn new(manager: Arc<CodeGraphManager>) -> Self {
        Self { manager }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("codegraph_context failed: {0}")]
pub struct CodeGraphContextError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeGraphContextArgs {
    /// Symbol name to look up (e.g. "start_indexing", "CodeGraphManager").
    pub name: String,
    /// Project ID to search within.
    pub project_id: String,
    /// Optional file path to disambiguate symbols with the same name.
    pub file_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CodeGraphContextOutput {
    pub found: bool,
    pub context: Value,
}

impl Tool for CodeGraphContextTool {
    const NAME: &'static str = "codegraph_context";
    type Error = CodeGraphContextError;
    type Args = CodeGraphContextArgs;
    type Output = CodeGraphContextOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Get a 360-degree view of a code symbol — all callers, callees, inheritance, which processes it participates in, and its file location. Use this to understand how a function/class connects to the rest of the codebase.".to_string(),
            parameters: schemars::schema_for!(CodeGraphContextArgs).to_value(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let db = self
            .manager
            .get_db(&args.project_id)
            .await
            .ok_or_else(|| CodeGraphContextError(format!("project '{}' not found", args.project_id)))?;

        let result = cg_tools::symbol_context(
            &db,
            &args.project_id,
            &args.name,
            args.file_path.as_deref(),
        )
        .await
        .map_err(|e| CodeGraphContextError(e.to_string()))?;

        match result {
            Some(ctx) => Ok(CodeGraphContextOutput {
                found: true,
                context: serde_json::to_value(ctx).unwrap_or_default(),
            }),
            None => Ok(CodeGraphContextOutput {
                found: false,
                context: serde_json::json!({"error": format!("Symbol '{}' not found", args.name)}),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// codegraph_impact — blast radius analysis
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CodeGraphImpactTool {
    manager: Arc<CodeGraphManager>,
}

impl CodeGraphImpactTool {
    pub fn new(manager: Arc<CodeGraphManager>) -> Self {
        Self { manager }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("codegraph_impact failed: {0}")]
pub struct CodeGraphImpactError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeGraphImpactArgs {
    /// Name of the function, class, or method to analyze.
    pub target: String,
    /// Project ID to search within.
    pub project_id: String,
    /// Direction: "upstream" (what depends on this) or "downstream" (what this depends on).
    #[serde(default = "default_upstream")]
    pub direction: String,
    /// Maximum traversal depth (default: 3).
    #[serde(default = "default_depth")]
    pub max_depth: u32,
}

fn default_upstream() -> String {
    "upstream".to_string()
}

fn default_depth() -> u32 {
    3
}

#[derive(Debug, Serialize)]
pub struct CodeGraphImpactOutput {
    pub found: bool,
    pub impact: Value,
}

impl Tool for CodeGraphImpactTool {
    const NAME: &'static str = "codegraph_impact";
    type Error = CodeGraphImpactError;
    type Args = CodeGraphImpactArgs;
    type Output = CodeGraphImpactOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Analyze the blast radius of changing a code symbol. Shows what would break at each depth level (d=1: direct callers WILL BREAK, d=2: indirect deps LIKELY AFFECTED, d=3: transitive MAY NEED TESTING), affected execution flows, and risk level (LOW/MEDIUM/HIGH/CRITICAL). Run this BEFORE modifying any function or class.".to_string(),
            parameters: schemars::schema_for!(CodeGraphImpactArgs).to_value(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let db = self
            .manager
            .get_db(&args.project_id)
            .await
            .ok_or_else(|| CodeGraphImpactError(format!("project '{}' not found", args.project_id)))?;

        let result = cg_tools::impact_analysis(
            &db,
            &args.project_id,
            &args.target,
            &args.direction,
            args.max_depth,
        )
        .await
        .map_err(|e| CodeGraphImpactError(e.to_string()))?;

        match result {
            Some(impact) => Ok(CodeGraphImpactOutput {
                found: true,
                impact: serde_json::to_value(impact).unwrap_or_default(),
            }),
            None => Ok(CodeGraphImpactOutput {
                found: false,
                impact: serde_json::json!({"error": format!("Symbol '{}' not found", args.target)}),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// codegraph_detect_changes — map git diff to affected graph
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CodeGraphDetectChangesTool {
    manager: Arc<CodeGraphManager>,
}

impl CodeGraphDetectChangesTool {
    pub fn new(manager: Arc<CodeGraphManager>) -> Self {
        Self { manager }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("codegraph_detect_changes failed: {0}")]
pub struct CodeGraphDetectChangesError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeGraphDetectChangesArgs {
    /// Project ID to analyze.
    pub project_id: String,
    /// What to analyze: "unstaged" (default), "staged", "all", or "compare".
    #[serde(default = "default_scope")]
    pub scope: String,
    /// Branch/commit for "compare" scope (e.g. "main").
    pub base_ref: Option<String>,
}

fn default_scope() -> String {
    "unstaged".to_string()
}

#[derive(Debug, Serialize)]
pub struct CodeGraphDetectChangesOutput {
    pub result: Value,
}

impl Tool for CodeGraphDetectChangesTool {
    const NAME: &'static str = "codegraph_detect_changes";
    type Error = CodeGraphDetectChangesError;
    type Args = CodeGraphDetectChangesArgs;
    type Output = CodeGraphDetectChangesOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Analyze uncommitted git changes and find affected code graph symbols and execution flows. Maps git diff to indexed symbols, then traces which processes are impacted. Use this to understand the scope of your changes before committing.".to_string(),
            parameters: schemars::schema_for!(CodeGraphDetectChangesArgs).to_value(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let project = self
            .manager
            .get_project(&args.project_id)
            .await
            .ok_or_else(|| CodeGraphDetectChangesError(format!("project '{}' not found", args.project_id)))?;

        let db = self
            .manager
            .get_db(&args.project_id)
            .await
            .ok_or_else(|| CodeGraphDetectChangesError("database not available".to_string()))?;

        let result = cg_tools::detect_changes(
            &db,
            &args.project_id,
            &project.root_path,
            &args.scope,
            args.base_ref.as_deref(),
        )
        .await
        .map_err(|e| CodeGraphDetectChangesError(e.to_string()))?;

        Ok(CodeGraphDetectChangesOutput {
            result: serde_json::to_value(result).unwrap_or_default(),
        })
    }
}

// ---------------------------------------------------------------------------
// codegraph_cypher — raw Cypher query
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CodeGraphCypherTool {
    manager: Arc<CodeGraphManager>,
}

impl CodeGraphCypherTool {
    pub fn new(manager: Arc<CodeGraphManager>) -> Self {
        Self { manager }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("codegraph_cypher failed: {0}")]
pub struct CodeGraphCypherError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeGraphCypherArgs {
    /// Cypher query to execute (read-only — CREATE/DELETE/SET are rejected).
    pub query: String,
    /// Project ID (used to open the correct database).
    pub project_id: String,
}

#[derive(Debug, Serialize)]
pub struct CodeGraphCypherOutput {
    pub result: Value,
}

impl Tool for CodeGraphCypherTool {
    const NAME: &'static str = "codegraph_cypher";
    type Error = CodeGraphCypherError;
    type Args = CodeGraphCypherArgs;
    type Output = CodeGraphCypherOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Execute a raw read-only Cypher query against the code knowledge graph. Use for complex structural queries that the other codegraph tools don't cover. Node labels: File, Folder, Function, Class, Method, Interface, Struct, Enum, Trait, Impl, Type, etc. Edge type stored as CodeRelation.type property (CALLS, IMPORTS, EXTENDS, IMPLEMENTS, CONTAINS, DEFINES, HAS_METHOD, MEMBER_OF, STEP_IN_PROCESS).".to_string(),
            parameters: schemars::schema_for!(CodeGraphCypherArgs).to_value(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let db = self
            .manager
            .get_db(&args.project_id)
            .await
            .ok_or_else(|| CodeGraphCypherError(format!("project '{}' not found", args.project_id)))?;

        let result = cg_tools::cypher_query(&db, &args.query)
            .await
            .map_err(|e| CodeGraphCypherError(e.to_string()))?;

        Ok(CodeGraphCypherOutput {
            result: serde_json::to_value(result).unwrap_or_default(),
        })
    }
}

// ---------------------------------------------------------------------------
// codegraph_rename — multi-file coordinated rename
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CodeGraphRenameTool {
    manager: Arc<CodeGraphManager>,
}

impl CodeGraphRenameTool {
    pub fn new(manager: Arc<CodeGraphManager>) -> Self {
        Self { manager }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("codegraph_rename failed: {0}")]
pub struct CodeGraphRenameError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeGraphRenameArgs {
    /// Current name of the symbol to rename.
    pub symbol_name: String,
    /// New name for the symbol.
    pub new_name: String,
    /// Project ID to search within.
    pub project_id: String,
    /// Optional file path to disambiguate symbols with the same name.
    pub file_path: Option<String>,
    /// Preview edits without applying (default: true). Set to false to apply.
    #[serde(default = "default_dry_run")]
    pub dry_run: bool,
}

fn default_dry_run() -> bool {
    true
}

#[derive(Debug, Serialize)]
pub struct CodeGraphRenameOutput {
    pub found: bool,
    pub result: Value,
}

impl Tool for CodeGraphRenameTool {
    const NAME: &'static str = "codegraph_rename";
    type Error = CodeGraphRenameError;
    type Args = CodeGraphRenameArgs;
    type Output = CodeGraphRenameOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Rename a code symbol across all files using the knowledge graph + text search. Graph-based edits are high confidence; text_search edits need manual review. Defaults to dry_run=true (preview only). Set dry_run=false to apply the rename.".to_string(),
            parameters: schemars::schema_for!(CodeGraphRenameArgs).to_value(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let project = self
            .manager
            .get_project(&args.project_id)
            .await
            .ok_or_else(|| CodeGraphRenameError(format!("project '{}' not found", args.project_id)))?;

        let db = self
            .manager
            .get_db(&args.project_id)
            .await
            .ok_or_else(|| CodeGraphRenameError("database not available".to_string()))?;

        let result = cg_tools::rename_symbol(
            &db,
            &args.project_id,
            &project.root_path,
            &args.symbol_name,
            &args.new_name,
            args.file_path.as_deref(),
            args.dry_run,
        )
        .await
        .map_err(|e| CodeGraphRenameError(e.to_string()))?;

        match result {
            Some(r) => Ok(CodeGraphRenameOutput {
                found: true,
                result: serde_json::to_value(r).unwrap_or_default(),
            }),
            None => Ok(CodeGraphRenameOutput {
                found: false,
                result: serde_json::json!({"error": format!("Symbol '{}' not found", args.symbol_name)}),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// codegraph_route_map — show API route → handler mappings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CodeGraphRouteMapTool {
    manager: Arc<CodeGraphManager>,
}

impl CodeGraphRouteMapTool {
    pub fn new(manager: Arc<CodeGraphManager>) -> Self {
        Self { manager }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("codegraph_route_map failed: {0}")]
pub struct CodeGraphRouteMapError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeGraphRouteMapArgs {
    /// Project ID to search within.
    pub project_id: String,
    /// Optional route path filter (e.g. "/api/graph"). Omit for all routes.
    pub route: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CodeGraphRouteMapOutput {
    pub result: Value,
}

impl Tool for CodeGraphRouteMapTool {
    const NAME: &'static str = "codegraph_route_map";
    type Error = CodeGraphRouteMapError;
    type Args = CodeGraphRouteMapArgs;
    type Output = CodeGraphRouteMapOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Show API route mappings: which functions handle which HTTP endpoints. Returns route path, HTTP method, handler function name, and file location. Use to understand the API surface of a project.".to_string(),
            parameters: schemars::schema_for!(CodeGraphRouteMapArgs).to_value(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let db = self.manager.get_db(&args.project_id).await
            .ok_or_else(|| CodeGraphRouteMapError(format!("project '{}' not found", args.project_id)))?;
        let result = cg_tools::route_map(&db, &args.project_id, args.route.as_deref()).await
            .map_err(|e| CodeGraphRouteMapError(e.to_string()))?;
        Ok(CodeGraphRouteMapOutput { result: serde_json::to_value(result).unwrap_or_default() })
    }
}

// ---------------------------------------------------------------------------
// codegraph_tool_map — show tool/MCP definitions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CodeGraphToolMapTool {
    manager: Arc<CodeGraphManager>,
}

impl CodeGraphToolMapTool {
    pub fn new(manager: Arc<CodeGraphManager>) -> Self {
        Self { manager }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("codegraph_tool_map failed: {0}")]
pub struct CodeGraphToolMapError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeGraphToolMapArgs {
    /// Project ID to search within.
    pub project_id: String,
    /// Optional tool name filter. Omit for all tools.
    pub tool: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CodeGraphToolMapOutput {
    pub result: Value,
}

impl Tool for CodeGraphToolMapTool {
    const NAME: &'static str = "codegraph_tool_map";
    type Error = CodeGraphToolMapError;
    type Args = CodeGraphToolMapArgs;
    type Output = CodeGraphToolMapOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Show MCP/RPC tool definitions: which functions handle which tools, and where they're defined. Returns tool name, handler function, and file location.".to_string(),
            parameters: schemars::schema_for!(CodeGraphToolMapArgs).to_value(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let db = self.manager.get_db(&args.project_id).await
            .ok_or_else(|| CodeGraphToolMapError(format!("project '{}' not found", args.project_id)))?;
        let result = cg_tools::tool_map(&db, &args.project_id, args.tool.as_deref()).await
            .map_err(|e| CodeGraphToolMapError(e.to_string()))?;
        Ok(CodeGraphToolMapOutput { result: serde_json::to_value(result).unwrap_or_default() })
    }
}

// ---------------------------------------------------------------------------
// codegraph_api_impact — pre-change impact report for a route
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CodeGraphApiImpactTool {
    manager: Arc<CodeGraphManager>,
}

impl CodeGraphApiImpactTool {
    pub fn new(manager: Arc<CodeGraphManager>) -> Self {
        Self { manager }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("codegraph_api_impact failed: {0}")]
pub struct CodeGraphApiImpactError(String);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeGraphApiImpactArgs {
    /// Project ID to analyze.
    pub project_id: String,
    /// Route path to analyze (e.g. "/api/graph/stats").
    pub route: Option<String>,
    /// Handler file path (alternative to route for lookup).
    pub file: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CodeGraphApiImpactOutput {
    pub found: bool,
    pub result: Value,
}

impl Tool for CodeGraphApiImpactTool {
    const NAME: &'static str = "codegraph_api_impact";
    type Error = CodeGraphApiImpactError;
    type Args = CodeGraphApiImpactArgs;
    type Output = CodeGraphApiImpactOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Pre-change impact report for an API route handler. Combines route mapping with blast-radius analysis to show: the handler function, what calls it, what processes are affected, and the risk level. Use before modifying an API endpoint.".to_string(),
            parameters: schemars::schema_for!(CodeGraphApiImpactArgs).to_value(),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let db = self.manager.get_db(&args.project_id).await
            .ok_or_else(|| CodeGraphApiImpactError(format!("project '{}' not found", args.project_id)))?;
        let result = cg_tools::api_impact(
            &db, &args.project_id, args.route.as_deref(), args.file.as_deref(),
        ).await.map_err(|e| CodeGraphApiImpactError(e.to_string()))?;
        match result {
            Some(r) => Ok(CodeGraphApiImpactOutput {
                found: true,
                result: serde_json::to_value(r).unwrap_or_default(),
            }),
            None => Ok(CodeGraphApiImpactOutput {
                found: false,
                result: serde_json::json!({"error": "No matching route found"}),
            }),
        }
    }
}
