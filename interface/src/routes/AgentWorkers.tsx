import {useState, useMemo, useEffect, useCallback, useRef} from "react";
import {useQuery, useQueryClient} from "@tanstack/react-query";
import {useNavigate, useSearch} from "@tanstack/react-router";
import {motion} from "framer-motion";
import {Markdown} from "@/components/Markdown";
import {
	api,
	type WorkerRunInfo,
	type WorkerDetailResponse,
	type TranscriptStep,
	type ActionContent,
} from "@/api/client";
import {Badge} from "@/ui/Badge";
import {formatTimeAgo, formatDuration} from "@/lib/format";
import {LiveDuration} from "@/components/LiveDuration";
import {useLiveContext} from "@/hooks/useLiveContext";
import {cx} from "@/ui/utils";

const STATUS_FILTERS = ["all", "running", "done", "failed"] as const;
type StatusFilter = (typeof STATUS_FILTERS)[number];

const KNOWN_STATUSES = new Set(["running", "done", "failed"]);

function normalizeStatus(status: string): string {
	if (KNOWN_STATUSES.has(status)) return status;
	// Legacy rows where set_status text overwrote the state enum.
	// If it has a completed_at it finished, otherwise it was interrupted.
	return "failed";
}

function statusBadgeVariant(status: string) {
	switch (status) {
		case "running":
			return "amber" as const;
		case "failed":
			return "red" as const;
		default:
			return "outline" as const;
	}
}

function workerTypeBadgeVariant(workerType: string) {
	return workerType === "opencode" ? ("accent" as const) : ("outline" as const);
}

function durationBetween(start: string, end: string | null): string {
	if (!end) return "";
	const seconds = Math.floor(
		(new Date(end).getTime() - new Date(start).getTime()) / 1000,
	);
	return formatDuration(seconds);
}

export function AgentWorkers({agentId}: {agentId: string}) {
	const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");
	const [search, setSearch] = useState("");
	const queryClient = useQueryClient();
	const navigate = useNavigate();
	const routeSearch = useSearch({strict: false}) as {worker?: string};
	const selectedWorkerId = routeSearch.worker ?? null;
	const {activeWorkers, workerEventVersion, liveTranscripts} = useLiveContext();

	// Invalidate worker queries when SSE events fire
	const prevVersion = useRef(workerEventVersion);
	useEffect(() => {
		if (workerEventVersion !== prevVersion.current) {
			prevVersion.current = workerEventVersion;
			queryClient.invalidateQueries({queryKey: ["workers", agentId]});
			if (selectedWorkerId) {
				queryClient.invalidateQueries({
					queryKey: ["worker-detail", agentId, selectedWorkerId],
				});
			}
		}
	}, [workerEventVersion, agentId, selectedWorkerId, queryClient]);

	// List query
	const {data: listData} = useQuery({
		queryKey: ["workers", agentId, statusFilter],
		queryFn: () =>
			api.workersList(agentId, {
				limit: 200,
				status: statusFilter === "all" ? undefined : statusFilter,
			}),
		refetchInterval: 10_000,
	});

	// Detail query (only when a worker is selected).
	// Returns null instead of throwing on 404 — the worker may not be in the DB
	// yet while it's still visible via SSE state.
	const {data: detailData} = useQuery({
		queryKey: ["worker-detail", agentId, selectedWorkerId],
		queryFn: () =>
			selectedWorkerId
				? api.workerDetail(agentId, selectedWorkerId).catch(() => null)
				: Promise.resolve(null),
		enabled: !!selectedWorkerId,
	});

	const workers = listData?.workers ?? [];
	const total = listData?.total ?? 0;

	// Merge live SSE state onto the API-returned list.
	// Workers that exist in SSE state but haven't hit the DB yet
	// are synthesized and prepended so they appear instantly.
	const mergedWorkers: WorkerRunInfo[] = useMemo(() => {
		const dbIds = new Set(workers.map((w) => w.id));

		// Overlay live state onto existing DB rows
		const merged = workers.map((worker) => {
			const live = activeWorkers[worker.id];
			if (!live) return worker;
			return {
				...worker,
				status: "running",
				live_status: live.status,
				tool_calls: live.toolCalls,
			};
		});

		// Synthesize entries for workers only known via SSE (not in DB yet)
		const synthetic: WorkerRunInfo[] = Object.values(activeWorkers)
			.filter((w) => !dbIds.has(w.id))
			.map((live) => ({
				id: live.id,
				task: live.task,
				status: "running",
				worker_type: "builtin",
				channel_id: live.channelId ?? null,
				channel_name: null,
				started_at: new Date(live.startedAt).toISOString(),
				completed_at: null,
				has_transcript: false,
				live_status: live.status,
				tool_calls: live.toolCalls,
			}));

		return [...synthetic, ...merged];
	}, [workers, activeWorkers]);

	// Client-side task text search filter
	const filteredWorkers = useMemo(() => {
		if (!search.trim()) return mergedWorkers;
		const term = search.toLowerCase();
		return mergedWorkers.filter((w) => w.task.toLowerCase().includes(term));
	}, [mergedWorkers, search]);

	// Build detail view: prefer DB data, fall back to synthesized live state.
	// Running workers that haven't hit the DB yet still get a full detail view
	// from SSE state + live transcript.
	const mergedDetail: WorkerDetailResponse | null = useMemo(() => {
		const live = selectedWorkerId ? activeWorkers[selectedWorkerId] : null;

		if (detailData) {
			// DB data exists — overlay live status if worker is still running
			if (!live) return detailData;
			return { ...detailData, status: "running" };
		}

		// No DB data yet — synthesize from SSE state
		if (!live) return null;
		return {
			id: live.id,
			task: live.task,
			result: null,
			status: "running",
			worker_type: "builtin",
			channel_id: live.channelId ?? null,
			channel_name: null,
			started_at: new Date(live.startedAt).toISOString(),
			completed_at: null,
			transcript: null,
			tool_calls: live.toolCalls,
		};
	}, [detailData, activeWorkers, selectedWorkerId]);

	const selectWorker = useCallback(
		(workerId: string | null) => {
			navigate({
				to: `/agents/${agentId}/workers`,
				search: workerId ? {worker: workerId} : {},
				replace: true,
			} as any);
		},
		[navigate, agentId],
	);

	return (
		<div className="flex h-full">
			{/* Left column: worker list */}
			<div className="flex w-[360px] flex-shrink-0 flex-col border-r border-app-line/50">
				{/* Toolbar */}
				<div className="flex items-center gap-3 border-b border-app-line/50 bg-app-darkBox/20 px-4 py-2.5">
					<input
						type="text"
						placeholder="Search tasks..."
						value={search}
						onChange={(e) => setSearch(e.target.value)}
						className="h-7 flex-1 rounded-md border border-app-line/50 bg-app-input px-2.5 text-xs text-ink placeholder:text-ink-faint focus:border-accent/50 focus:outline-none"
					/>
					<span className="text-tiny text-ink-faint">{total}</span>
				</div>

				{/* Status filter pills */}
				<div className="flex items-center gap-1.5 border-b border-app-line/50 px-4 py-2">
					{STATUS_FILTERS.map((filter) => (
						<button
							key={filter}
							onClick={() => setStatusFilter(filter)}
							className={cx(
								"rounded-full px-2.5 py-0.5 text-tiny font-medium transition-colors",
								statusFilter === filter
									? "bg-accent/15 text-accent"
									: "text-ink-faint hover:bg-app-hover hover:text-ink-dull",
							)}
						>
							{filter.charAt(0).toUpperCase() + filter.slice(1)}
						</button>
					))}
				</div>

				{/* Worker list */}
				<div className="flex-1 overflow-y-auto">
					{filteredWorkers.length === 0 ? (
						<div className="flex h-32 items-center justify-center">
							<p className="text-xs text-ink-faint">No workers found</p>
						</div>
					) : (
						filteredWorkers.map((worker) => (
							<WorkerCard
								key={worker.id}
								worker={worker}
								liveWorker={activeWorkers[worker.id]}
								selected={worker.id === selectedWorkerId}
								onClick={() => selectWorker(worker.id)}
							/>
						))
					)}
				</div>
			</div>

			{/* Right column: detail view */}
			<div className="flex flex-1 flex-col overflow-hidden">
				{selectedWorkerId && mergedDetail ? (
					<WorkerDetail
						detail={mergedDetail}
						liveWorker={activeWorkers[selectedWorkerId]}
						liveTranscript={liveTranscripts[selectedWorkerId]}
					/>
				) : (
					<div className="flex flex-1 items-center justify-center">
						<p className="text-sm text-ink-faint">
							Select a worker to view details
						</p>
					</div>
				)}
			</div>
		</div>
	);
}

interface LiveWorker {
	id: string;
	task: string;
	status: string;
	startedAt: number;
	toolCalls: number;
	currentTool: string | null;
}

function WorkerCard({
	worker,
	liveWorker,
	selected,
	onClick,
}: {
	worker: WorkerRunInfo;
	liveWorker?: LiveWorker;
	selected: boolean;
	onClick: () => void;
}) {
	const isRunning = worker.status === "running" || !!liveWorker;
	const displayStatus = liveWorker?.status ?? worker.live_status;
	const toolCalls = liveWorker?.toolCalls ?? worker.tool_calls;
	const currentTool = liveWorker?.currentTool;

	return (
		<button
			onClick={onClick}
			className={cx(
				"flex w-full flex-col gap-1 border-b border-app-line/30 px-4 py-3 text-left transition-colors",
				selected ? "bg-app-selected/50" : "",
			)}
		>
			<div className="flex items-start justify-between gap-2">
				<p className={cx("line-clamp-2 flex-1 text-xs font-medium", selected ? "text-ink" : "text-ink-dull")}>
					{worker.task}
				</p>
				<Badge
					variant={statusBadgeVariant(isRunning ? "running" : worker.status)}
					size="sm"
					className={!isRunning && worker.status === "done" ? "hover:border-app-line hover:text-ink-dull" : undefined}
				>
					{isRunning && (
						<span className="h-1.5 w-1.5 animate-pulse rounded-full bg-current" />
					)}
					{isRunning ? "running" : normalizeStatus(worker.status)}
				</Badge>
			</div>
			<div className="flex items-center gap-2 text-tiny text-ink-faint">
				{worker.channel_name && (
					<span className="truncate">{worker.channel_name}</span>
				)}
				{worker.channel_name && <span>·</span>}
				<span>{worker.worker_type}</span>
				<span>·</span>
				{isRunning ? (
					<LiveDuration
						startMs={
							liveWorker?.startedAt ??
							new Date(worker.started_at).getTime()
						}
					/>
				) : (
					<span>{formatTimeAgo(worker.started_at)}</span>
				)}
				{toolCalls > 0 && (
					<>
						<span>·</span>
						<span>{toolCalls} tools</span>
					</>
				)}
			</div>
		</button>
	);
}

function WorkerDetail({
	detail,
	liveWorker,
	liveTranscript,
}: {
	detail: WorkerDetailResponse;
	liveWorker?: LiveWorker;
	liveTranscript?: TranscriptStep[];
}) {
	const isRunning = detail.status === "running" || !!liveWorker;
	const duration = durationBetween(detail.started_at, detail.completed_at);
	const displayStatus = liveWorker?.status;
	const currentTool = liveWorker?.currentTool;
	const toolCalls = liveWorker?.toolCalls ?? detail.tool_calls ?? 0;
	// Use persisted transcript if available, otherwise fall back to live SSE transcript.
	// Strip the final action step if it duplicates the result text shown above.
	const rawTranscript = detail.transcript ?? (isRunning ? liveTranscript : null);
	const transcript = useMemo(() => {
		if (!rawTranscript || !detail.result) return rawTranscript;
		const last = rawTranscript[rawTranscript.length - 1];
		if (
			last?.type === "action" &&
			last.content.length === 1 &&
			last.content[0].type === "text" &&
			last.content[0].text.trim() === detail.result.trim()
		) {
			return rawTranscript.slice(0, -1);
		}
		return rawTranscript;
	}, [rawTranscript, detail.result]);
	const transcriptRef = useRef<HTMLDivElement>(null);

	// Auto-scroll to latest transcript step for running workers
	useEffect(() => {
		if (isRunning && transcriptRef.current) {
			transcriptRef.current.scrollTop = transcriptRef.current.scrollHeight;
		}
	}, [isRunning, transcript?.length]);

	return (
		<div className="flex h-full flex-col">
			{/* Header */}
			<div className="flex flex-col gap-2 border-b border-app-line/50 bg-app-darkBox/20 px-6 py-4">
				<div className="flex items-start justify-between gap-3">
					<TaskText text={detail.task} />
					<div className="flex items-center gap-2">
						{isRunning && detail.channel_id && (
							<CancelWorkerButton
								channelId={detail.channel_id}
								workerId={detail.id}
							/>
						)}
						<Badge
							variant={workerTypeBadgeVariant(detail.worker_type)}
							size="sm"
						>
							{detail.worker_type}
						</Badge>
						<Badge
							variant={statusBadgeVariant(
								isRunning ? "running" : normalizeStatus(detail.status),
							)}
							size="sm"
						>
							{isRunning && (
								<span className="h-1.5 w-1.5 animate-pulse rounded-full bg-current" />
							)}
							{isRunning ? "running" : normalizeStatus(detail.status)}
						</Badge>
					</div>
				</div>
				<div className="flex items-center gap-3 text-tiny text-ink-faint">
					{detail.channel_name && <span>{detail.channel_name}</span>}
					{isRunning ? (
						<span>
							Running for{" "}
							<LiveDuration
								startMs={
									liveWorker?.startedAt ??
									new Date(detail.started_at).getTime()
								}
							/>
						</span>
					) : (
						duration && <span>{duration}</span>
					)}
					{!isRunning && <span>{formatTimeAgo(detail.started_at)}</span>}
					{toolCalls > 0 && (
						<span>{toolCalls} tool calls</span>
					)}
				</div>
				{/* Live status bar for running workers */}
				{isRunning && (currentTool || displayStatus) && (
					<div className="flex items-center gap-2 text-tiny">
						{currentTool ? (
							<span className="text-accent">
								Running {currentTool}...
							</span>
						) : displayStatus ? (
							<span className="text-amber-500">{displayStatus}</span>
						) : null}
					</div>
				)}
			</div>

			{/* Content */}
			<div ref={transcriptRef} className="flex-1 overflow-y-auto">
				{/* Result section */}
				{detail.result && (
					<div className="border-b border-app-line/30 px-6 py-4">
						<h3 className="mb-2 text-tiny font-medium uppercase tracking-wider text-ink-faint">
							Result
						</h3>
						<div className="text-xs text-ink">
							<Markdown>{detail.result}</Markdown>
						</div>
					</div>
				)}

				{/* Transcript section */}
				{transcript && transcript.length > 0 ? (
					<div className="px-6 py-4">
						<h3 className="mb-3 text-tiny font-medium uppercase tracking-wider text-ink-faint">
							{isRunning ? "Live Transcript" : "Transcript"}
						</h3>
						<div className="flex flex-col gap-3">
							{transcript.map((step, index) => (
								<motion.div
									key={`${step.type}-${index}`}
									initial={{opacity: 0, y: 6}}
									animate={{opacity: 1, y: 0}}
									transition={{duration: 0.2, ease: "easeOut"}}
								>
									<TranscriptStepView step={step} />
								</motion.div>
							))}
							{isRunning && currentTool && (
								<div className="flex items-center gap-2 py-2 text-tiny text-accent">
									<span className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
									Running {currentTool}...
								</div>
							)}
						</div>
					</div>
				) : isRunning ? (
					<div className="flex flex-col items-center justify-center gap-2 py-12 text-ink-faint">
						<div className="h-2 w-2 animate-pulse rounded-full bg-amber-500" />
						<p className="text-xs">Waiting for first tool call...</p>
					</div>
				) : (
					<div className="px-6 py-8 text-center text-xs text-ink-faint">
						Full transcript not available for this worker
					</div>
				)}
			</div>
		</div>
	);
}

function TaskText({text}: {text: string}) {
	const [expanded, setExpanded] = useState(false);

	return (
		<button
			onClick={() => setExpanded((v) => !v)}
			className="text-left text-sm font-medium text-ink-dull"
		>
			<p className={expanded ? undefined : "line-clamp-3"}>{text}</p>
		</button>
	);
}

function TranscriptStepView({step}: {step: TranscriptStep}) {
	if (step.type === "action") {
		return (
			<div className="flex flex-col gap-1.5">
				{step.content.map((content, index) => (
					<ActionContentView key={index} content={content} />
				))}
			</div>
		);
	}

	return <ToolResultView step={step} />;
}

function CancelWorkerButton({
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
			onClick={() => {
				setCancelling(true);
				api.cancelProcess(channelId, "worker", workerId).catch(console.warn);
			}}
			className="rounded-md border border-app-line px-2 py-0.5 text-tiny font-medium text-ink-dull transition-colors hover:border-ink-faint hover:text-ink disabled:opacity-50"
		>
			{cancelling ? "Cancelling..." : "Cancel"}
		</button>
	);
}

function ActionContentView({content}: {content: ActionContent}) {
	if (content.type === "text") {
		return (
			<div className="text-xs text-ink">
				<Markdown>{content.text}</Markdown>
			</div>
		);
	}

	return <ToolCallView content={content} />;
}

function ToolCallView({
	content,
}: {
	content: Extract<ActionContent, {type: "tool_call"}>;
}) {
	const [expanded, setExpanded] = useState(false);

	return (
		<div className="rounded-md border border-app-line/50 bg-app-darkBox/30">
			<button
				onClick={() => setExpanded(!expanded)}
				className="flex w-full items-center gap-2 px-3 py-2 text-left text-xs"
			>
				<span className="text-accent">&#9656;</span>
				<span className="font-medium text-ink-dull">{content.name}</span>
				{!expanded && (
					<span className="flex-1 truncate text-ink-faint">
						{content.args.slice(0, 80)}
					</span>
				)}
			</button>
			{expanded && (
				<pre className="max-h-60 overflow-auto border-t border-app-line/30 px-3 py-2 font-mono text-tiny text-ink-dull">
					{content.args}
				</pre>
			)}
		</div>
	);
}

function ToolResultView({
	step,
}: {
	step: Extract<TranscriptStep, {type: "tool_result"}>;
}) {
	const [expanded, setExpanded] = useState(false);
	const isLong = step.text.length > 300;
	const displayText =
		isLong && !expanded ? step.text.slice(0, 300) + "..." : step.text;

	return (
		<div className="rounded-md border border-app-line/30 bg-app-darkerBox/50">
			<div className="flex items-center gap-2 px-3 py-1.5">
				<span className="text-tiny text-emerald-500">&#10003;</span>
				{step.name && (
					<span className="text-tiny font-medium text-ink-faint">
						{step.name}
					</span>
				)}
			</div>
			<pre className="max-h-80 overflow-auto whitespace-pre-wrap px-3 pb-2 font-mono text-tiny text-ink-dull">
				{displayText}
			</pre>
			{isLong && (
				<button
					onClick={() => setExpanded(!expanded)}
					className="w-full border-t border-app-line/20 px-3 py-1 text-center text-tiny text-ink-faint hover:text-ink-dull"
				>
					{expanded ? "Collapse" : "Show full output"}
				</button>
			)}
		</div>
	);
}
