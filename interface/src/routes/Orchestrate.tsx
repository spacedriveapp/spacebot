import {useState, useMemo, useEffect, useRef} from "react";
import {useQuery, useQueryClient, useQueries} from "@tanstack/react-query";
import {api, type WorkerRunInfo} from "@/api/client";
import {OpenCodeEmbed} from "@/components/OpenCodeEmbed";
import {LiveDuration} from "@/components/LiveDuration";
import {Badge} from "@/ui/Badge";
import {useLiveContext} from "@/hooks/useLiveContext";
import {cx} from "@/ui/utils";

// ── Types ────────────────────────────────────────────────────────────────

/** A worker with agent metadata attached for cross-agent views. */
interface OrchestrationWorker extends WorkerRunInfo {
	agent_id: string;
	agent_name: string;
	/** Overridden from SSE live state when available. */
	live_tool_calls?: number;
}

interface ProjectGroup {
	/** Project name, or "Ungrouped" for workers without a project. */
	name: string;
	projectId: string | null;
	workers: OrchestrationWorker[];
}

// ── Filters ──────────────────────────────────────────────────────────────

type GroupBy = "project" | "agent";

// ── Component ────────────────────────────────────────────────────────────

export function Orchestrate() {
	const [groupBy, setGroupBy] = useState<GroupBy>("project");
	const [agentFilter, setAgentFilter] = useState<string | null>(null);
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

	// -- Apply agent filter --
	const filteredWorkers = useMemo(
		() => agentFilter ? allWorkers.filter((w) => w.agent_id === agentFilter) : allWorkers,
		[allWorkers, agentFilter],
	);

	// -- Group workers --
	const groups: ProjectGroup[] = useMemo(() => {
		if (groupBy === "agent") {
			const byAgent = new Map<string, OrchestrationWorker[]>();
			for (const worker of filteredWorkers) {
				const key = worker.agent_id;
				const list = byAgent.get(key) ?? [];
				list.push(worker);
				byAgent.set(key, list);
			}
			return Array.from(byAgent.entries()).map(([agentId, workers]) => ({
				name: workers[0]?.agent_name ?? agentId,
				projectId: null,
				workers,
			}));
		}

		// Group by project
		const byProject = new Map<string, OrchestrationWorker[]>();
		for (const worker of filteredWorkers) {
			const key = worker.project_name ?? "Ungrouped";
			const list = byProject.get(key) ?? [];
			list.push(worker);
			byProject.set(key, list);
		}

		// Sort: named projects first (alphabetical), "Ungrouped" last
		const entries = Array.from(byProject.entries());
		entries.sort(([a], [b]) => {
			if (a === "Ungrouped") return 1;
			if (b === "Ungrouped") return -1;
			return a.localeCompare(b);
		});

		return entries.map(([name, workers]) => ({
			name,
			projectId: workers[0]?.project_id ?? null,
			workers,
		}));
	}, [filteredWorkers, groupBy]);

	const isLoading = workerQueries.some((query) => query.isLoading);

	// -- Unique agents with active workers (for filter dropdown) --
	const activeAgents = useMemo(() => {
		const seen = new Map<string, string>();
		for (const worker of allWorkers) {
			if (!seen.has(worker.agent_id)) {
				seen.set(worker.agent_id, worker.agent_name);
			}
		}
		return Array.from(seen.entries()).map(([id, name]) => ({id, name}));
	}, [allWorkers]);

	return (
		<div className="flex h-full flex-col">
			{/* Toolbar */}
			<div className="flex items-center gap-3 border-b border-app-line px-4 py-2">
				{/* Group by toggle */}
				<div className="flex items-center gap-1.5">
					<span className="text-tiny font-medium text-ink-faint">Group by</span>
					<div className="flex rounded-md border border-app-line">
						<button
							onClick={() => setGroupBy("project")}
							className={cx(
								"px-2.5 py-1 text-tiny font-medium transition-colors",
								groupBy === "project"
									? "bg-app-selected text-ink"
									: "text-ink-faint hover:text-ink",
							)}
						>
							Project
						</button>
						<button
							onClick={() => setGroupBy("agent")}
							className={cx(
								"px-2.5 py-1 text-tiny font-medium transition-colors",
								groupBy === "agent"
									? "bg-app-selected text-ink"
									: "text-ink-faint hover:text-ink",
							)}
						>
							Agent
						</button>
					</div>
				</div>

				{/* Agent filter */}
				{activeAgents.length > 1 && (
					<div className="flex items-center gap-1.5">
						<span className="text-tiny font-medium text-ink-faint">Agent</span>
						<select
							value={agentFilter ?? ""}
							onChange={(event) => setAgentFilter(event.target.value || null)}
							className="rounded-md border border-app-line bg-transparent px-2 py-1 text-tiny text-ink outline-none"
						>
							<option value="">All</option>
							{activeAgents.map(({id, name}) => (
								<option key={id} value={id}>{name}</option>
							))}
						</select>
					</div>
				)}

				{/* Worker count */}
				<div className="ml-auto text-tiny text-ink-faint">
					{filteredWorkers.length} active worker{filteredWorkers.length !== 1 ? "s" : ""}
				</div>
			</div>

			{/* Content */}
			<div className="flex-1 overflow-y-auto">
				{isLoading && filteredWorkers.length === 0 ? (
					<div className="flex h-full items-center justify-center">
						<div className="flex items-center gap-2 text-xs text-ink-faint">
							<span className="h-2 w-2 animate-pulse rounded-full bg-accent" />
							Loading workers...
						</div>
					</div>
				) : filteredWorkers.length === 0 ? (
					<EmptyState />
				) : (
					<div className="flex flex-col gap-0.5 p-2">
						{groups.map((group) => (
							<ProjectTrack key={group.name} group={group} />
						))}
					</div>
				)}
			</div>
		</div>
	);
}

// ── Empty state ──────────────────────────────────────────────────────────

function EmptyState() {
	return (
		<div className="flex h-full flex-col items-center justify-center gap-3 text-ink-faint">
			<div className="flex h-12 w-12 items-center justify-center rounded-full border border-app-line">
				<svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
					<rect x="2" y="3" width="20" height="18" rx="2" />
					<line x1="9" y1="3" x2="9" y2="21" />
					<line x1="15" y1="3" x2="15" y2="21" />
				</svg>
			</div>
			<div className="text-center">
				<p className="text-sm font-medium">No active workers</p>
				<p className="mt-1 text-xs">
					OpenCode workers will appear here as columns when they're running.
				</p>
			</div>
		</div>
	);
}

// ── Project track (horizontal scrollable row of worker columns) ──────────

function ProjectTrack({group}: {group: ProjectGroup}) {
	return (
		<div className="flex flex-col">
			{/* Track header */}
			<div className="flex items-center gap-2 px-2 py-1.5">
				<h2 className="text-xs font-semibold text-ink">{group.name}</h2>
				<Badge variant="outline" size="sm">
					{group.workers.length}
				</Badge>
			</div>

			{/* Horizontal scroll container */}
			<div className="flex gap-2 overflow-x-auto pb-2 pl-2 pr-2">
				{group.workers.map((worker) => (
					<WorkerColumn key={worker.id} worker={worker} />
				))}
			</div>
		</div>
	);
}

// ── Worker column (fixed-width, contains header + OpenCode embed) ────────

function WorkerColumn({worker}: {worker: OrchestrationWorker}) {
	const isRunning = worker.status === "running";
	const isIdle = worker.status === "idle";
	const toolCalls = worker.live_tool_calls ?? worker.tool_calls;

	// Strip the [opencode] prefix from the task text for display
	const taskText = worker.task.replace(/^\[opencode\]\s*/i, "");

	return (
		<div className="flex h-[calc(100vh-9rem)] w-[560px] flex-shrink-0 flex-col overflow-hidden rounded-lg border border-app-line bg-app-darkBox">
			{/* Column header */}
			<div className="flex flex-col gap-1 border-b border-app-line px-3 py-2">
				<div className="flex items-center gap-2">
					{/* Status dot */}
					<span
						className={cx(
							"h-2 w-2 flex-shrink-0 rounded-full",
							isRunning && "animate-pulse bg-green-500",
							isIdle && "bg-yellow-500",
							!isRunning && !isIdle && "bg-ink-faint",
						)}
					/>
					{/* Task text */}
					<p className="min-w-0 flex-1 truncate text-xs font-medium text-ink" title={taskText}>
						{taskText}
					</p>
					{/* Cancel button for running workers */}
					{(isRunning || isIdle) && worker.channel_id && (
						<CancelButton channelId={worker.channel_id} workerId={worker.id} />
					)}
				</div>
				<div className="flex items-center gap-2 text-tiny text-ink-faint">
					<span>{worker.agent_name}</span>
					<span>&middot;</span>
					{isRunning ? (
						<LiveDuration startMs={new Date(worker.started_at).getTime()} />
					) : (
						<span>{worker.status}</span>
					)}
					{toolCalls > 0 && (
						<>
							<span>&middot;</span>
							<span>{toolCalls} tool{toolCalls !== 1 ? "s" : ""}</span>
						</>
					)}
				</div>
			</div>

			{/* OpenCode embed */}
			<div className="flex flex-1 flex-col overflow-hidden">
				<OpenCodeEmbed
					port={worker.opencode_port!}
					sessionId={worker.opencode_session_id ?? worker.id}
					directory={worker.directory ?? null}
				/>
			</div>
		</div>
	);
}

// ── Cancel button ────────────────────────────────────────────────────────

function CancelButton({
	channelId,
	workerId,
}: {
	channelId: string;
	workerId: string;
}) {
	const [cancelling, setCancelling] = useState(false);

	return (
		<button
			disabled={cancelling}
			onClick={(event) => {
				event.stopPropagation();
				setCancelling(true);
				api
					.cancelProcess(channelId, "worker", workerId)
					.catch(console.warn)
					.finally(() => setCancelling(false));
			}}
			className="flex-shrink-0 rounded-md border border-app-line px-1.5 py-0.5 text-tiny font-medium text-ink-dull transition-colors hover:border-red-500/50 hover:text-red-400 disabled:opacity-50"
			title="Cancel worker"
		>
			{cancelling ? "..." : "Cancel"}
		</button>
	);
}
