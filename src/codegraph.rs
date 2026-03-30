//! Code Graph Memory System — Spacebot-native rebuild of GitNexus.
//!
//! A Layer 2 memory layer that indexes codebases into queryable knowledge
//! graphs. When a project is added, a 10-phase AST parsing and graph
//! construction pipeline fires automatically. The graph captures every
//! function, class, import, call relationship, inheritance chain, and
//! execution flow, stored in LadybugDB.

pub mod db;
pub mod events;
pub mod eviction;
pub mod languages;
pub mod manager;
pub mod pipeline;
pub mod schema;
pub mod search;
pub mod types;
pub mod watcher;

pub use manager::CodeGraphManager;
pub use types::{
    CodeGraphConfig, CommunityInfo, FilesForTaskResult, GraphEdge, GraphNode, GraphSearchResult,
    ImpactResult, IndexLogEntry, IndexStatus, NodeLabel, PipelinePhase, PipelineProgress,
    PipelineStats, ProcessInfo, ProjectMemory, RegisteredProject,
};
