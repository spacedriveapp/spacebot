import {useMemo, useEffect, useRef, useState} from "react";
import {useQuery, useQueryClient, useQueries} from "@tanstack/react-query";
import {ListBullets} from "@phosphor-icons/react";
import {api} from "@/api/client";
import {useLiveContext} from "@/hooks/useLiveContext";
import {useIsMobile} from "@/hooks/useMediaQuery";
import {Drawer} from "@/ui/Drawer";
import {
	EmptyState,
	WorkbenchSidebar,
	WorkerColumn,
	groupWorkersByProjectAndWorktree,
	normalizePath,
	resolvePath,
	type DirectoryMatch,
	type OrchestrationWorker,
} from "@/components/workbench";

export function Workbench() {
	const queryClient = useQueryClient();
	const {activeWorkers, workerEventVersion} = useLiveContext();

	// -- Fetch all agents --
	const {data: agentsData} = useQuery({
		queryKey: ["agents"],
		queryFn: () => api.agents(),
		staleTime: 30_000,
	});

	const agents = agentsData?.agents ?? [];

	// -- Fan-out: fetch active workers per agent --
	// We fetch both "running" and "idle" status workers separately then merge.
	const workerQueries = useQueries({
		queries: agents.flatMap((agent) =>
			(["running", "idle"] as const).map((status) => ({
				queryKey: ["orchestrate-workers", agent.id, status],
				queryFn: () => api.workersList(agent.id, {limit: 200, status}),
				refetchInterval: 10_000,
			})),
		),
	});

	// Invalidate when SSE events fire
	const prevVersion = useRef(workerEventVersion);
	useEffect(() => {
		if (workerEventVersion !== prevVersion.current) {
			prevVersion.current = workerEventVersion;
			queryClient.invalidateQueries({queryKey: ["orchestrate-workers"]});
		}
	}, [workerEventVersion, queryClient]);

	// -- Merge DB workers with SSE live state --
	const allWorkers: OrchestrationWorker[] = useMemo(() => {
		const result: OrchestrationWorker[] = [];
		const seenIds = new Set<string>();

		agents.forEach((agent, agentIndex) => {
			const agentName = agent.display_name ?? agent.id;
			// Each agent has 2 queries (running, idle) at indices agentIndex*2 and agentIndex*2+1
			for (let statusIdx = 0; statusIdx < 2; statusIdx++) {
				const queryIdx = agentIndex * 2 + statusIdx;
				const queryResult = workerQueries[queryIdx];
				const workers = queryResult?.data?.workers ?? [];

				for (const worker of workers) {
					if (seenIds.has(worker.id)) continue;
					seenIds.add(worker.id);

					// Only show OpenCode workers
					if (worker.worker_type !== "opencode") continue;
					// Must have a port to embed
					if (!worker.opencode_port) continue;

					const liveWorker = activeWorkers[worker.id];
					result.push({
						...worker,
						agent_id: agent.id,
						agent_name: agentName,
						status: liveWorker?.status ?? worker.status,
						live_tool_calls: liveWorker?.toolCalls,
					});
				}
			}

			// Also add SSE-only workers (started but not yet in DB)
			for (const [workerId, liveWorker] of Object.entries(activeWorkers)) {
				if (seenIds.has(workerId)) continue;
				if (liveWorker.agentId !== agent.id) continue;
				if (liveWorker.workerType !== "opencode") continue;
				seenIds.add(workerId);

				// SSE workers don't have opencode_port in activeWorkers.
				// They'll appear in the DB query on next refetch once persisted.
				// Skip for now — they'll show up shortly.
			}
		});

		return result;
	}, [agents, workerQueries, activeWorkers]);

	const filteredWorkers = allWorkers;
	const isLoading = workerQueries.some((query) => query.isLoading);

	// -- Fetch projects + worktrees to resolve worker.directory → project/worktree --
	const {data: projectsData} = useQuery({
		queryKey: ["workbench-projects"],
		queryFn: () => api.listProjects(),
		staleTime: 60_000,
	});
	const projects = projectsData?.projects ?? [];

	const projectDetailQueries = useQueries({
		queries: projects.map((project) => ({
			queryKey: ["workbench-project-detail", project.id],
			queryFn: () => api.getProject(project.id),
			staleTime: 60_000,
		})),
	});

	const directoryIndex = useMemo(() => {
		const index = new Map<string, DirectoryMatch>();
		projects.forEach((project, idx) => {
			const detail = projectDetailQueries[idx]?.data;
			if (!detail) return;
			// Main repo root acts as the default worktree
			if (project.root_path) {
				const abs = normalizePath(project.root_path);
				if (!index.has(abs)) {
					index.set(abs, {
						projectId: project.id,
						projectName: project.name,
						worktreeName: "main",
					});
				}
			}
			for (const worktree of detail.worktrees ?? []) {
				const abs = resolvePath(project.root_path, worktree.path);
				index.set(abs, {
					projectId: project.id,
					projectName: project.name,
					worktreeName: worktree.name,
				});
			}
		});
		return index;
	}, [projects, projectDetailQueries]);

	const columnRefs = useRef<Record<string, HTMLDivElement | null>>({});
	const scrollToWorker = (id: string) => {
		columnRefs.current[id]?.scrollIntoView({
			behavior: "smooth",
			inline: "start",
			block: "nearest",
		});
	};

	// Group by project → worktree
	const tree = useMemo(
		() => groupWorkersByProjectAndWorktree(filteredWorkers, directoryIndex),
		[filteredWorkers, directoryIndex],
	);

	const isMobile = useIsMobile();
	const [sidebarOpen, setSidebarOpen] = useState(false);

	const handleSelectWorker = (id: string) => {
		scrollToWorker(id);
		setSidebarOpen(false);
	};

	return (
		<div className="relative flex h-full gap-[10px] bg-sidebar pb-[10px] md:pr-[10px]">
			{!isMobile && (
				<WorkbenchSidebar
					tree={tree}
					totalCount={filteredWorkers.length}
					onSelectWorker={scrollToWorker}
				/>
			)}
			<div className="flex min-w-0 flex-1">
				{isLoading && filteredWorkers.length === 0 ? (
					<div className="flex h-full flex-1 items-center justify-center">
						<div className="flex items-center gap-2 text-xs text-ink-faint">
							<span className="h-2 w-2 animate-pulse rounded-full bg-accent" />
							Loading workers...
						</div>
					</div>
				) : filteredWorkers.length === 0 ? (
					<EmptyState />
				) : (
					<div className="flex flex-1 snap-x snap-mandatory gap-[10px] overflow-x-auto px-[10px] md:snap-none md:px-0">
						{filteredWorkers.map((worker) => (
							<div
								key={worker.id}
								ref={(el) => {
									columnRefs.current[worker.id] = el;
								}}
								className="flex h-full w-full flex-shrink-0 snap-center md:w-auto md:snap-none"
							>
								<WorkerColumn worker={worker} />
							</div>
						))}
					</div>
				)}
			</div>

			{isMobile && filteredWorkers.length > 0 && (
				<button
					type="button"
					aria-label="Workers list"
					onClick={() => setSidebarOpen(true)}
					className="fixed bottom-4 right-4 z-40 flex h-12 w-12 items-center justify-center rounded-full border border-app-line bg-app-box text-ink shadow-lg active:bg-app-selected"
				>
					<ListBullets size={20} weight="bold" />
				</button>
			)}

			{isMobile && (
				<Drawer
					open={sidebarOpen}
					onOpenChange={setSidebarOpen}
					side="right"
					ariaLabel="Workers"
				>
					<WorkbenchSidebar
						tree={tree}
						totalCount={filteredWorkers.length}
						onSelectWorker={handleSelectWorker}
						fillWidth
					/>
				</Drawer>
			)}
		</div>
	);
}
