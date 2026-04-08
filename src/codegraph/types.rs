//! Core types for the Code Graph Memory System.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Node labels
// ---------------------------------------------------------------------------

/// All supported graph node labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NodeLabel {
    // Structural
    Project,
    Package,
    Module,
    Folder,
    File,
    // Code entities
    Class,
    Function,
    Method,
    Variable,
    Parameter,
    Interface,
    Enum,
    Decorator,
    Import,
    Type,
    // Language-specific
    Struct,
    Macro,
    Trait,
    Impl,
    Namespace,
    TypeAlias,
    Const,
    Record,
    Template,
    // Semantic (AI-optimized)
    Community,
    Process,
    // Documentation & testing
    Section,
    Test,
}

impl NodeLabel {
    /// Cypher label string for this node type.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Project => "Project",
            Self::Package => "Package",
            Self::Module => "Module",
            Self::Folder => "Folder",
            Self::File => "File",
            Self::Class => "Class",
            Self::Function => "Function",
            Self::Method => "Method",
            Self::Variable => "Variable",
            Self::Parameter => "Parameter",
            Self::Interface => "Interface",
            Self::Enum => "Enum",
            Self::Decorator => "Decorator",
            Self::Import => "Import",
            Self::Type => "Type",
            Self::Struct => "Struct",
            Self::Macro => "MacroDef",
            Self::Trait => "Trait",
            Self::Impl => "Impl",
            Self::Namespace => "Namespace",
            Self::TypeAlias => "TypeAlias",
            Self::Const => "Const",
            Self::Record => "Record",
            Self::Template => "Template",
            Self::Community => "Community",
            Self::Process => "Process",
            Self::Section => "Section",
            Self::Test => "Test",
        }
    }
}

impl std::fmt::Display for NodeLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Edge types
// ---------------------------------------------------------------------------

/// All supported graph edge (relationship) types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EdgeType {
    Contains,
    Defines,
    Calls,
    Imports,
    Extends,
    Implements,
    Inherits,
    Overrides,
    HasMethod,
    HasProperty,
    Accesses,
    Uses,
    HasParameter,
    Decorates,
    MemberOf,
    StepInProcess,
    TestedBy,
}

impl EdgeType {
    /// Cypher relationship label string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Contains => "CONTAINS",
            Self::Defines => "DEFINES",
            Self::Calls => "CALLS",
            Self::Imports => "IMPORTS",
            Self::Extends => "EXTENDS",
            Self::Implements => "IMPLEMENTS",
            Self::Inherits => "INHERITS",
            Self::Overrides => "OVERRIDES",
            Self::HasMethod => "HAS_METHOD",
            Self::HasProperty => "HAS_PROPERTY",
            Self::Accesses => "ACCESSES",
            Self::Uses => "USES",
            Self::HasParameter => "HAS_PARAMETER",
            Self::Decorates => "DECORATES",
            Self::MemberOf => "MEMBER_OF",
            Self::StepInProcess => "STEP_IN_PROCESS",
            Self::TestedBy => "TESTED_BY",
        }
    }
}

impl std::fmt::Display for EdgeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Graph node
// ---------------------------------------------------------------------------

/// A node in the code graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    /// Internal graph node ID (assigned by LadybugDB).
    pub id: i64,
    /// Unique qualified name (e.g. `src/auth.ts::AuthService::login`).
    pub qualified_name: String,
    /// Human-readable short name.
    pub name: String,
    /// Node label / kind.
    pub label: NodeLabel,
    /// Source file path relative to project root.
    pub source_file: Option<String>,
    /// Start line in the source file (1-based).
    pub line_start: Option<u32>,
    /// End line in the source file (1-based).
    pub line_end: Option<u32>,
    /// Project this node belongs to.
    pub project_id: String,
    /// Who created this node: `"indexer"` or `"agent"`.
    pub source: String,
    /// If source is `"agent"`, which agent wrote it.
    pub written_by: Option<String>,
    /// Arbitrary properties (language-specific metadata, etc.).
    #[serde(default)]
    pub properties: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Graph edge
// ---------------------------------------------------------------------------

/// An edge (relationship) in the code graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    /// Source node ID.
    pub from_id: i64,
    /// Target node ID.
    pub to_id: i64,
    /// Relationship type.
    pub edge_type: EdgeType,
    /// Confidence score for this relationship (0.0–1.0).
    pub confidence: f32,
    /// Project this edge belongs to.
    pub project_id: String,
    /// Who created this edge: `"indexer"` or `"agent"`.
    pub source: String,
    /// Arbitrary properties.
    #[serde(default)]
    pub properties: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Pipeline types
// ---------------------------------------------------------------------------

/// Phases of the 10-phase indexing pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PipelinePhase {
    /// Phase 1: Walk filesystem, respect .gitignore, skip binaries.
    Extracting,
    /// Phase 2: Create structural nodes (Folder, File, Package, Module, Project).
    Structure,
    /// Phase 3: tree-sitter AST parse, extract symbol nodes.
    Parsing,
    /// Phase 4: Resolve import/require/use statements.
    Imports,
    /// Phase 5: Resolve call-sites with confidence scoring.
    Calls,
    /// Phase 6: Resolve extends/implements/inherits.
    Heritage,
    /// Phase 7: Leiden community detection.
    Communities,
    /// Phase 8: Score entry-points, trace call chains.
    Processes,
    /// Phase 9: Optional LLM labels (skipped if >50k nodes).
    Enriching,
    /// Phase 10: Write meta.json, fire events, start watcher.
    Complete,
}

impl PipelinePhase {
    /// 1-based phase number.
    pub fn number(&self) -> u8 {
        match self {
            Self::Extracting => 1,
            Self::Structure => 2,
            Self::Parsing => 3,
            Self::Imports => 4,
            Self::Calls => 5,
            Self::Heritage => 6,
            Self::Communities => 7,
            Self::Processes => 8,
            Self::Enriching => 9,
            Self::Complete => 10,
        }
    }

    /// Human-readable name for display.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Extracting => "Extracting files",
            Self::Structure => "Building structure",
            Self::Parsing => "Parsing AST",
            Self::Imports => "Resolving imports",
            Self::Calls => "Resolving calls",
            Self::Heritage => "Resolving heritage",
            Self::Communities => "Detecting communities",
            Self::Processes => "Tracing processes",
            Self::Enriching => "Enriching labels",
            Self::Complete => "Completing index",
        }
    }
}

impl std::fmt::Display for PipelinePhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/10 — {}", self.number(), self.display_name())
    }
}

/// Live progress of an indexing pipeline run.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PipelineProgress {
    /// Current phase.
    pub phase: PipelinePhase,
    /// Progress within the current phase (0.0–1.0).
    pub phase_progress: f32,
    /// Human-readable status message.
    pub message: String,
    /// Accumulated stats so far.
    pub stats: PipelineStats,
}

/// Statistics from an indexing run.
#[derive(Debug, Clone, Default, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PipelineStats {
    pub files_found: u64,
    pub files_parsed: u64,
    pub files_skipped: u64,
    pub nodes_created: u64,
    pub edges_created: u64,
    pub communities_detected: u64,
    pub processes_traced: u64,
    pub errors: u64,
}

// ---------------------------------------------------------------------------
// Index status
// ---------------------------------------------------------------------------

/// Overall indexing status for a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IndexStatus {
    /// Not yet indexed.
    Pending,
    /// Pipeline is running.
    Indexing,
    /// Successfully indexed and up-to-date.
    Indexed,
    /// Files changed since last index.
    Stale,
    /// Indexing failed.
    Error,
}

impl std::fmt::Display for IndexStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Indexing => write!(f, "indexing"),
            Self::Indexed => write!(f, "indexed"),
            Self::Stale => write!(f, "stale"),
            Self::Error => write!(f, "error"),
        }
    }
}

// ---------------------------------------------------------------------------
// Project registry
// ---------------------------------------------------------------------------

/// Entry in the project registry (`.spacebot/codegraph/registry.json`).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RegisteredProject {
    pub project_id: String,
    pub name: String,
    #[schema(value_type = String)]
    pub root_path: PathBuf,
    pub status: IndexStatus,
    /// Current pipeline progress (only set when `status == Indexing`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<PipelineProgress>,
    /// Human-readable error message (only set when `status == Error`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Stats from the last completed index.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_index_stats: Option<PipelineStats>,
    /// When the last index completed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_indexed_at: Option<DateTime<Utc>>,
    /// Primary language detected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_language: Option<String>,
    /// Schema version of the graph database.
    pub schema_version: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Meta.json
// ---------------------------------------------------------------------------

/// Per-project metadata file persisted at `.spacebot/codegraph/<project_id>/meta.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub project_id: String,
    pub schema_version: u32,
    pub status: IndexStatus,
    /// Phase-by-phase timing from last run.
    #[serde(default)]
    pub phase_timings: HashMap<String, f64>,
    /// Last completed stats.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<PipelineStats>,
    pub last_indexed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Project memory (Layer 1 with project_id tag)
// ---------------------------------------------------------------------------

/// A project-scoped memory in the centralized store.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProjectMemory {
    pub id: String,
    pub project_id: String,
    pub memory_type: crate::memory::MemoryType,
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub last_verified_at: DateTime<Utc>,
    /// Relevance score (0.0–1.0), updated by stale eviction.
    pub relevance_score: f32,
    /// Who created this: `"indexer"`, `"cortex"`, `"agent"`, or `"user"`.
    pub source: String,
}

// ---------------------------------------------------------------------------
// Staleness check
// ---------------------------------------------------------------------------

/// Result of a staleness evaluation on a project memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StalenessCheck {
    pub memory_id: String,
    pub project_id: String,
    pub check_type: StalenessCheckType,
    pub result: StalenessResult,
    pub reason: String,
    pub action: StalenessAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StalenessCheckType {
    CodeChange,
    Scheduled,
    Explicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StalenessResult {
    Current,
    Stale,
    Uncertain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StalenessAction {
    Keep,
    Update,
    Remove,
}

// ---------------------------------------------------------------------------
// Code graph configuration
// ---------------------------------------------------------------------------

/// Code graph settings (from Settings > Code Graph).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeGraphConfig {
    /// Auto-fire indexing when a project is added.
    #[serde(default = "default_true")]
    pub auto_index_on_add: bool,
    /// Enable file watcher for real-time updates.
    #[serde(default = "default_true")]
    pub real_time_watching: bool,
    /// Watcher debounce window in milliseconds.
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    /// Enable LLM enrichment labels in phase 9.
    #[serde(default)]
    pub llm_enrichment: bool,
    /// If set, only parse files matching these languages.
    #[serde(default)]
    pub language_filter: Vec<String>,
    /// Percentage delta threshold to trigger full Leiden re-clustering.
    #[serde(default = "default_re_index_threshold")]
    pub re_index_threshold: u8,
    /// Hours before auto full re-index due to staleness.
    #[serde(default = "default_staleness_threshold_hours")]
    pub staleness_threshold_hours: u32,
    /// Max call-chain depth for Process tracing.
    #[serde(default = "default_max_process_depth")]
    pub max_process_depth: u32,
    /// Max number of entry-point processes to trace.
    #[serde(default = "default_max_processes")]
    pub max_processes: u32,
    /// Max branching factor during BFS process tracing.
    #[serde(default = "default_max_process_branching")]
    pub max_process_branching: u32,
    /// Minimum call-chain length to persist a process.
    #[serde(default = "default_min_process_steps")]
    pub min_process_steps: u32,
    /// Minimum nodes per persisted community.
    #[serde(default = "default_community_min_size")]
    pub community_min_size: u32,
    /// Allow worker agent writes to the graph.
    #[serde(default = "default_true")]
    pub agent_writes_enabled: bool,
    /// Enable automatic stale memory eviction.
    #[serde(default = "default_true")]
    pub stale_eviction_enabled: bool,
    /// Scheduled stale eviction cadence in hours.
    #[serde(default = "default_stale_eviction_cadence_hours")]
    pub stale_eviction_cadence_hours: u32,
    /// Relevance score below which a memory is flagged for removal.
    #[serde(default = "default_stale_relevance_threshold")]
    pub stale_relevance_threshold: f32,
    /// Auto-skip semantic embeddings above this node count.
    #[serde(default = "default_node_embedding_skip_threshold")]
    pub node_embedding_skip_threshold: u64,
    /// Auto-generate AGENTS.md on index complete.
    #[serde(default = "default_true")]
    pub generate_agents_md: bool,
}

impl Default for CodeGraphConfig {
    fn default() -> Self {
        Self {
            auto_index_on_add: true,
            real_time_watching: true,
            debounce_ms: 500,
            llm_enrichment: false,
            language_filter: Vec::new(),
            re_index_threshold: 5,
            staleness_threshold_hours: 24,
            max_process_depth: 10,
            max_processes: 50,
            max_process_branching: 4,
            min_process_steps: 3,
            community_min_size: 3,
            agent_writes_enabled: true,
            stale_eviction_enabled: true,
            stale_eviction_cadence_hours: 24,
            stale_relevance_threshold: 0.2,
            node_embedding_skip_threshold: 50_000,
            generate_agents_md: true,
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_debounce_ms() -> u64 {
    500
}
fn default_re_index_threshold() -> u8 {
    5
}
fn default_staleness_threshold_hours() -> u32 {
    24
}
fn default_max_process_depth() -> u32 {
    10
}
fn default_max_processes() -> u32 {
    50
}
fn default_max_process_branching() -> u32 {
    4
}
fn default_min_process_steps() -> u32 {
    3
}
fn default_community_min_size() -> u32 {
    3
}
fn default_stale_eviction_cadence_hours() -> u32 {
    24
}
fn default_stale_relevance_threshold() -> f32 {
    0.2
}
fn default_node_embedding_skip_threshold() -> u64 {
    50_000
}

// ---------------------------------------------------------------------------
// Search results
// ---------------------------------------------------------------------------

/// A result from the hybrid BM25+semantic+RRF search.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct GraphSearchResult {
    pub node_id: i64,
    pub qualified_name: String,
    pub name: String,
    pub label: NodeLabel,
    pub source_file: Option<String>,
    pub line_start: Option<u32>,
    /// Fusion score from reciprocal rank fusion.
    pub score: f64,
    /// Which community this node belongs to.
    pub community: Option<String>,
    /// Snippet of surrounding code or content.
    pub snippet: Option<String>,
}

/// A community summary for the UI.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CommunityInfo {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub node_count: u64,
    pub file_count: u64,
    pub function_count: u64,
    /// Top symbols by centrality.
    pub key_symbols: Vec<String>,
}

/// An entry point / process node for the UI.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProcessInfo {
    pub id: String,
    pub entry_function: String,
    pub source_file: String,
    pub call_depth: u32,
    pub community: Option<String>,
    pub steps: Vec<String>,
}

/// Index log entry for the UI.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct IndexLogEntry {
    pub run_id: String,
    pub status: IndexStatus,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub current_phase: Option<PipelinePhase>,
    pub progress: Option<PipelineProgress>,
    pub stats: Option<PipelineStats>,
    pub error: Option<String>,
}

/// Result from `codegraph_get_files_for_task`.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct FilesForTaskResult {
    /// Direct symbol matches.
    pub primary_files: Vec<String>,
    /// Callers and importers.
    pub secondary_files: Vec<String>,
    /// Which community this task touches.
    pub community: Option<String>,
    /// Confidence in the file selection (0.0–1.0).
    pub confidence: f64,
}

/// Impact analysis result.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ImpactResult {
    pub symbol: String,
    pub depth: u32,
    pub upstream: Vec<ImpactNode>,
    pub downstream: Vec<ImpactNode>,
    pub risk_level: RiskLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ImpactNode {
    pub qualified_name: String,
    pub label: NodeLabel,
    pub source_file: String,
    pub distance: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}
