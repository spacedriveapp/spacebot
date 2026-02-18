//! StatusBlock: Live status snapshot for channels.

use crate::{BranchId, ProcessEvent, ProcessId, WorkerId};
use chrono::{DateTime, Utc};

/// Live status block injected into channel context.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct StatusBlock {
    /// Currently running branches.
    pub active_branches: Vec<BranchStatus>,
    /// Currently running workers.
    pub active_workers: Vec<WorkerStatus>,
    /// Recently completed work.
    pub completed_items: Vec<CompletedItem>,
}

/// Status of an active branch.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BranchStatus {
    pub id: BranchId,
    pub started_at: DateTime<Utc>,
    pub description: String,
}

/// Status of an active worker.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkerStatus {
    pub id: WorkerId,
    pub task: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub notify_on_complete: bool,
    pub tool_calls: usize,
}

/// Recently completed work item.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CompletedItem {
    pub id: String,
    pub item_type: CompletedItemType,
    pub description: String,
    pub completed_at: DateTime<Utc>,
    pub result_summary: String,
}

/// Type of completed item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum CompletedItemType {
    Branch,
    Worker,
}

impl StatusBlock {
    /// Create a new empty status block.
    pub fn new() -> Self {
        Self::default()
    }

    /// Update from a process event.
    pub fn update(&mut self, event: &ProcessEvent) {
        match event {
            ProcessEvent::WorkerStatus {
                worker_id, status, ..
            } => {
                // Update existing worker or add new one
                if let Some(worker) = self.active_workers.iter_mut().find(|w| w.id == *worker_id) {
                    worker.status.clone_from(status);
                }
            }
            ProcessEvent::WorkerComplete {
                worker_id,
                result,
                notify,
                ..
            } => {
                // Remove from active, add to completed
                if let Some(pos) = self.active_workers.iter().position(|w| w.id == *worker_id) {
                    let worker = self.active_workers.remove(pos);

                    if *notify {
                        self.completed_items.push(CompletedItem {
                            id: worker_id.to_string(),
                            item_type: CompletedItemType::Worker,
                            description: worker.task,
                            completed_at: Utc::now(),
                            result_summary: result.clone(),
                        });
                    }
                }
            }
            ProcessEvent::ToolCompleted {
                process_id: ProcessId::Worker(worker_id),
                ..
            } => {
                if let Some(worker) = self.active_workers.iter_mut().find(|w| w.id == *worker_id) {
                    worker.tool_calls += 1;
                }
            }
            ProcessEvent::BranchResult {
                branch_id,
                conclusion,
                ..
            } => {
                // Remove from active branches, add to completed
                if let Some(pos) = self.active_branches.iter().position(|b| b.id == *branch_id) {
                    let branch = self.active_branches.remove(pos);
                    self.completed_items.push(CompletedItem {
                        id: branch_id.to_string(),
                        item_type: CompletedItemType::Branch,
                        description: branch.description,
                        completed_at: Utc::now(),
                        result_summary: conclusion.clone(),
                    });
                }

                // Keep only last 10 completed items
                if self.completed_items.len() > 10 {
                    self.completed_items.remove(0);
                }
            }
            ProcessEvent::BranchFailed {
                branch_id, error, ..
            } => {
                // Remove from active branches, add to completed with error summary.
                if let Some(pos) = self.active_branches.iter().position(|b| b.id == *branch_id) {
                    let branch = self.active_branches.remove(pos);
                    self.completed_items.push(CompletedItem {
                        id: branch_id.to_string(),
                        item_type: CompletedItemType::Branch,
                        description: branch.description,
                        completed_at: Utc::now(),
                        result_summary: format!("Branch failed: {error}"),
                    });
                }

                // Keep only last 10 completed items
                if self.completed_items.len() > 10 {
                    self.completed_items.remove(0);
                }
            }
            _ => {}
        }
    }

    /// Add a new active branch.
    pub fn add_branch(&mut self, id: BranchId, description: impl Into<String>) {
        self.active_branches.push(BranchStatus {
            id,
            started_at: Utc::now(),
            description: description.into(),
        });
    }

    /// Add a new active worker.
    pub fn add_worker(&mut self, id: WorkerId, task: impl Into<String>, notify_on_complete: bool) {
        self.active_workers.push(WorkerStatus {
            id,
            task: task.into(),
            status: "starting".to_string(),
            started_at: Utc::now(),
            notify_on_complete,
            tool_calls: 0,
        });
    }

    /// Render the status block as a string for context injection.
    pub fn render(&self) -> String {
        let mut output = String::new();

        // Active workers
        if !self.active_workers.is_empty() {
            output.push_str("## Active Workers\n");
            for worker in &self.active_workers {
                let tool_calls_str = if worker.tool_calls > 0 {
                    format!(", {} tool calls", worker.tool_calls)
                } else {
                    String::new()
                };
                output.push_str(&format!(
                    "- [{}] {} ({}{}): {}\n",
                    worker.id,
                    worker.task,
                    worker.started_at.format("%H:%M"),
                    tool_calls_str,
                    worker.status
                ));
            }
            output.push('\n');
        }

        // Active branches
        if !self.active_branches.is_empty() {
            output.push_str("## Active Branches\n");
            for branch in &self.active_branches {
                output.push_str(&format!(
                    "- [{}] {} (started {})\n",
                    branch.id,
                    branch.description,
                    branch.started_at.format("%H:%M:%S")
                ));
            }
            output.push('\n');
        }

        // Recently completed
        if !self.completed_items.is_empty() {
            output.push_str("## Recently Completed\n");
            for item in self.completed_items.iter().rev().take(5) {
                let type_str = match item.item_type {
                    CompletedItemType::Branch => "branch",
                    CompletedItemType::Worker => "worker",
                };
                // Truncate long results to keep the status block manageable
                let summary = if item.result_summary.len() > 500 {
                    let end = item.result_summary.floor_char_boundary(500);
                    format!("{}...", &item.result_summary[..end])
                } else {
                    item.result_summary.clone()
                };
                output.push_str(&format!(
                    "- [{}] {}: {}\n",
                    type_str, item.description, summary,
                ));
            }
            output.push('\n');
        }

        output
    }

    /// Check if a worker is active.
    pub fn is_worker_active(&self, worker_id: WorkerId) -> bool {
        self.active_workers.iter().any(|w| w.id == worker_id)
    }

    /// Get the number of active branches.
    pub fn active_branch_count(&self) -> usize {
        self.active_branches.len()
    }
}
