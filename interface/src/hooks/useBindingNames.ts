import { useQueries, useQuery } from "@tanstack/react-query";
import { useMemo } from "react";
import { api } from "@/api/client";
import type { BindingNames } from "@/components/tasks/RepoChip";

/**
 * Resolve project / repo / worktree ids to display names for the task board.
 *
 * Loaded once for the whole board and passed down, rather than each card
 * fetching its own — a 200-task board would otherwise fan out into hundreds of
 * requests. Repos and worktrees only exist on the per-project detail endpoint,
 * so this issues one query per project.
 */
export function useBindingNames(): { names: BindingNames; isLoading: boolean } {
	const { data: projectList, isLoading: projectsLoading } = useQuery({
		queryKey: ["projects", "all"],
		queryFn: () => api.listProjects(),
		staleTime: 60_000,
	});

	const projects = useMemo(() => projectList?.projects ?? [], [projectList]);

	const detailQueries = useQueries({
		queries: projects.map((project) => ({
			queryKey: ["project", project.id],
			queryFn: () => api.getProject(project.id),
			staleTime: 60_000,
		})),
	});

	const names = useMemo<BindingNames>(() => {
		const projectMap = new Map<string, string>();
		const repoMap = new Map<string, string>();
		const worktreeMap = new Map<string, string>();

		for (const project of projects) {
			projectMap.set(project.id, project.name || project.id);
		}
		for (const query of detailQueries) {
			const detail = query.data;
			if (!detail) continue;
			for (const repo of detail.repos ?? []) {
				repoMap.set(repo.id, repo.name || repo.path);
			}
			for (const worktree of detail.worktrees ?? []) {
				// Worktrees are branch-scoped, so the branch is the useful label.
				worktreeMap.set(worktree.id, worktree.branch || worktree.name);
			}
		}

		return { projects: projectMap, repos: repoMap, worktrees: worktreeMap };
		// detailQueries is a new array each render; depend on the resolved data.
		// eslint-disable-next-line react-hooks/exhaustive-deps
	}, [projects, detailQueries.map((q) => q.dataUpdatedAt).join(",")]);

	return {
		names,
		isLoading: projectsLoading || detailQueries.some((q) => q.isLoading),
	};
}
