//! Code Graph Memory System.
//!
//! A Layer 2 memory layer that indexes codebases into queryable knowledge
//! graphs. When a project is added, a 10-phase AST parsing and graph
//! construction pipeline fires automatically. The graph captures every
//! function, class, import, call relationship, inheritance chain, and
//! execution flow, stored in LadybugDB.

pub mod config;
pub mod db;
pub mod embeddings_table;
pub mod events;
pub mod eviction;
pub mod graph_queries;
pub mod lang;
pub mod langmap;
pub mod manager;
pub mod pipeline;
pub mod schema;
pub mod search;
pub mod search_augmentation;
pub mod semantic;
pub mod tools;
pub mod types;
pub mod watcher;

pub use manager::CodeGraphManager;
pub use types::{
    CodeGraphConfig, CommunityInfo, FilesForTaskResult, GraphEdge, GraphNode, GraphSearchResult,
    ImpactResult, IndexLogEntry, IndexStatus, NodeLabel, PipelinePhase, PipelineProgress,
    PipelineStats, ProcessInfo, RegisteredProject,
};
