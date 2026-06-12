import {basename, normalizePath} from "./paths";
import type {
	DirectoryMatch,
	OrchestrationWorker,
	ProjectGroup,
	WorktreeGroup,
} from "./types";

export function groupWorkersByProjectAndWorktree(
	workers: OrchestrationWorker[],
	directoryIndex: Map<string, DirectoryMatch>,
): ProjectGroup[] {
	const projects = new Map<string, Map<string, WorktreeGroup>>();
	// Track resolved project names per key
	const projectNames = new Map<string, string>();

	for (const worker of workers) {
		// Try API-linked project first, then resolve via directory lookup
		let projectKey = worker.project_id ?? "";
		let projectName = worker.project_name ?? "";
		let worktreeName = "";

		if (worker.directory) {
			const match = directoryIndex.get(normalizePath(worker.directory));
			if (match) {
				projectKey = match.projectId;
				projectName = match.projectName;
				worktreeName = match.worktreeName;
			}
		}

		if (!projectKey) {
			projectKey = "__ungrouped__";
			projectName = "Ungrouped";
		}
		projectNames.set(projectKey, projectName);

		const worktreeKey = worktreeName || worker.directory || "__no_worktree__";
		const worktreeDisplay =
			worktreeName ||
			(worker.directory ? basename(worker.directory) : "No worktree");

		let worktrees = projects.get(projectKey);
		if (!worktrees) {
			worktrees = new Map();
			projects.set(projectKey, worktrees);
		}

		let group = worktrees.get(worktreeKey);
		if (!group) {
			group = {
				key: worktreeKey,
				name: worktreeDisplay,
				directory: worker.directory ?? null,
				workers: [],
			};
			worktrees.set(worktreeKey, group);
		}
		group.workers.push(worker);
	}

	const result: ProjectGroup[] = [];
	for (const [projectKey, worktrees] of projects) {
		const name = projectNames.get(projectKey) || projectKey;
		const wtList = [...worktrees.values()].sort((a, b) => {
			// "main" worktree first, then alpha
			if (a.name === "main") return -1;
			if (b.name === "main") return 1;
			return a.name.localeCompare(b.name);
		});
		const count = wtList.reduce((sum, wt) => sum + wt.workers.length, 0);
		result.push({key: projectKey, name, worktrees: wtList, count});
	}

	result.sort((a, b) => {
		if (a.key === "__ungrouped__") return 1;
		if (b.key === "__ungrouped__") return -1;
		return a.name.localeCompare(b.name);
	});

	return result;
}
