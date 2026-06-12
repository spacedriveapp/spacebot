import type {WorkerRunInfo} from "@/api/client";

/** A worker with agent metadata attached for cross-agent views. */
export interface OrchestrationWorker extends WorkerRunInfo {
	agent_id: string;
	agent_name: string;
	/** Overridden from SSE live state when available. */
	live_tool_calls?: number;
}

export interface WorktreeGroup {
	key: string;
	name: string;
	directory: string | null;
	workers: OrchestrationWorker[];
}

export interface ProjectGroup {
	key: string;
	name: string;
	worktrees: WorktreeGroup[];
	count: number;
}

export interface DirectoryMatch {
	projectId: string;
	projectName: string;
	worktreeName: string;
}
