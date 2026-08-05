import {useQueries, useQuery} from "@tanstack/react-query";
import {useMemo} from "react";
import {api} from "@/api/client";

export interface RepoChoice {
	repoId: string;
	repoName: string;
	projectId: string;
	projectName: string;
	/** Where the repo lives, for the reader deciding which `web` this is. */
	path: string;
	defaultBranch: string;
}

/**
 * Every repo across every project, as something a picker can offer.
 *
 * Repos only exist on the per-project detail endpoint, so this is one query per
 * project — the same keys `useBindingNames` uses, so the two share a cache
 * rather than doubling the requests.
 *
 * This exists because a command step *must* name a repo: it runs in exactly the
 * directory its binding resolves to, and a step with no binding has no
 * directory and is refused at launch rather than silently defaulting to the
 * workspace. Until now nothing in the workflow editor could set one.
 */
export function useRepoChoices(): {choices: RepoChoice[]; isLoading: boolean} {
	const {data: projectList, isLoading: projectsLoading} = useQuery({
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

	const choices = useMemo<RepoChoice[]>(() => {
		const out: RepoChoice[] = [];
		for (const query of detailQueries) {
			const detail = query.data;
			if (!detail) continue;
			for (const repo of detail.repos ?? []) {
				out.push({
					repoId: repo.id,
					repoName: repo.name || repo.path,
					projectId: detail.id,
					projectName: detail.name || detail.id,
					path: repo.path,
					defaultBranch: repo.default_branch,
				});
			}
		}
		return out.sort(
			(a, b) =>
				a.projectName.localeCompare(b.projectName) ||
				a.repoName.localeCompare(b.repoName),
		);
		// detailQueries is a new array each render; depend on the resolved data.
		// eslint-disable-next-line react-hooks/exhaustive-deps
	}, [detailQueries.map((q) => q.dataUpdatedAt).join(",")]);

	return {
		choices,
		isLoading: projectsLoading || detailQueries.some((q) => q.isLoading),
	};
}
