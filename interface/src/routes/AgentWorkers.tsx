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
	type OpenCodePart,
} from "@/api/client";
import {Badge} from "@/ui/Badge";
import {formatTimeAgo, formatDuration} from "@/lib/format";
import {LiveDuration} from "@/components/LiveDuration";
import {useLiveContext} from "@/hooks/useLiveContext";
import {cx} from "@/ui/utils";

/** RFC 4648 base64url encoding (no padding), matching OpenCode's directory encoding. */
export function base64UrlEncode(value: string): string {
	const bytes = new TextEncoder().encode(value);
	const binary = Array.from(bytes, (b) => String.fromCharCode(b)).join("");
	return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=/g, "");
}

const STATUS_FILTERS = ["all", "running", "idle", "done", "failed"] as const;
type StatusFilter = (typeof STATUS_FILTERS)[number];

const KNOWN_STATUSES = new Set(["running", "idle", "done", "failed"]);

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
		case "idle":
			return "blue" as const;
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
	const {activeWorkers, workerEventVersion, liveTranscripts, liveOpenCodeParts} = useLiveContext();

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
	const scopedActiveWorkers = useMemo(() => {
		const entries = Object.entries(activeWorkers).filter(
			([, worker]) => worker.agentId === agentId,
		);
		return Object.fromEntries(entries);
	}, [activeWorkers, agentId]);

	// Merge live SSE state onto the API-returned list.
	// Workers that exist in SSE state but haven't hit the DB yet
	// are synthesized and prepended so they appear instantly.
	const mergedWorkers: WorkerRunInfo[] = useMemo(() => {
		const dbIds = new Set(workers.map((w) => w.id));

		// Overlay live state onto existing DB rows
		const merged = workers.map((worker) => {
			const live = scopedActiveWorkers[worker.id];
			if (!live) return worker;
			return {
				...worker,
				status: live.isIdle ? "idle" : "running",
				live_status: live.status,
				tool_calls: live.toolCalls,
			};
		});

		// Synthesize entries for workers only known via SSE (not in DB yet)
		const synthetic: WorkerRunInfo[] = Object.values(scopedActiveWorkers)
			.filter((w) => !dbIds.has(w.id))
			.map((live) => ({
				id: live.id,
				task: live.task,
				status: live.isIdle ? "idle" : "running",
				worker_type: live.workerType ?? "builtin",
				channel_id: live.channelId ?? null,
				channel_name: null,
				started_at: new Date(live.startedAt).toISOString(),
				completed_at: null,
				has_transcript: false,
				live_status: live.status,
				tool_calls: live.toolCalls,
				opencode_port: null,
				interactive: live.interactive,
			}));

		return [...synthetic, ...merged];
	}, [workers, scopedActiveWorkers]);

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
		const live = selectedWorkerId ? scopedActiveWorkers[selectedWorkerId] : null;

		if (detailData) {
			// DB data exists — overlay live status if worker is still running
			if (!live) return detailData;
			return { ...detailData, status: live.isIdle ? "idle" : "running" };
		}

		// No DB data yet — synthesize from SSE state
		if (!live) return null;
		return {
			id: live.id,
			task: live.task,
			result: null,
			status: live.isIdle ? "idle" : "running",
			worker_type: live.workerType ?? "builtin",
			channel_id: live.channelId ?? null,
			channel_name: null,
			started_at: new Date(live.startedAt).toISOString(),
			completed_at: null,
			transcript: null,
			tool_calls: live.toolCalls,
			opencode_session_id: null,
			opencode_port: null,
			interactive: live.interactive,
		};
	}, [detailData, scopedActiveWorkers, selectedWorkerId]);

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
								liveWorker={scopedActiveWorkers[worker.id]}
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
						liveWorker={scopedActiveWorkers[selectedWorkerId]}
						liveTranscript={liveTranscripts[selectedWorkerId]}
						liveOpenCodeParts={liveOpenCodeParts[selectedWorkerId]}
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
	isIdle: boolean;
	interactive: boolean;
	workerType: string;
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
	const isLive = worker.status === "running" || !!liveWorker;
	const isIdle = liveWorker?.isIdle ?? worker.status === "idle";
	const isInteractive = liveWorker?.interactive ?? worker.interactive;
	const displayStatus = isIdle ? "idle" : isLive ? "running" : normalizeStatus(worker.status);
	const toolCalls = liveWorker?.toolCalls ?? worker.tool_calls;

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
				<div className="flex items-center gap-1.5">
					{isInteractive && (
						<Badge variant="outline" size="sm">
							interactive
						</Badge>
					)}
					<Badge
						variant={statusBadgeVariant(displayStatus)}
						size="sm"
						className={!isLive && worker.status === "done" ? "hover:border-app-line hover:text-ink-dull" : undefined}
					>
						{isLive && !isIdle && (
							<span className="h-1.5 w-1.5 animate-pulse rounded-full bg-current" />
						)}
						{displayStatus}
					</Badge>
				</div>
			</div>
			<div className="flex items-center gap-2 text-tiny text-ink-faint">
				{worker.channel_name && (
					<span className="truncate">{worker.channel_name}</span>
				)}
				{worker.channel_name && <span>·</span>}
				<span>{worker.worker_type}</span>
				<span>·</span>
				{isLive && !isIdle ? (
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

type DetailTab = "opencode" | "transcript";

function WorkerDetail({
	detail,
	liveWorker,
	liveTranscript,
	liveOpenCodeParts,
}: {
	detail: WorkerDetailResponse;
	liveWorker?: LiveWorker;
	liveTranscript?: TranscriptStep[];
	liveOpenCodeParts?: Map<string, OpenCodePart>;
}) {
	const isLive = detail.status === "running" || !!liveWorker;
	const isIdle = liveWorker?.isIdle ?? detail.status === "idle";
	const duration = durationBetween(detail.started_at, detail.completed_at);
	const displayStatus = liveWorker?.status;
	const currentTool = liveWorker?.currentTool;
	const toolCalls = liveWorker?.toolCalls ?? detail.tool_calls ?? 0;

	const isOpenCode = detail.worker_type === "opencode";
	const hasOpenCodeEmbed =
		isOpenCode &&
		detail.opencode_port != null &&
		detail.opencode_session_id != null;

	// Convert the insertion-ordered Map to an array for rendering
	const openCodeParts: OpenCodePart[] = useMemo(
		() => (liveOpenCodeParts ? Array.from(liveOpenCodeParts.values()) : []),
		[liveOpenCodeParts],
	);

	const [activeTab, setActiveTab] = useState<DetailTab>(
		hasOpenCodeEmbed ? "opencode" : "transcript",
	);

	// Reset tab when switching workers
	useEffect(() => {
		setActiveTab(hasOpenCodeEmbed ? "opencode" : "transcript");
	}, [detail.id, hasOpenCodeEmbed]);

	// Use persisted transcript if available, otherwise fall back to live SSE transcript.
	// Strip the final action step if it duplicates the result text shown above.
	const rawTranscript = detail.transcript ?? (isLive ? liveTranscript : null);
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

	// Auto-scroll to latest transcript step for running workers (not idle)
	const isRunning = isLive && !isIdle;
	useEffect(() => {
		if (isRunning && activeTab === "transcript" && transcriptRef.current) {
			transcriptRef.current.scrollTop = transcriptRef.current.scrollHeight;
		}
	}, [isRunning, activeTab, transcript?.length]);

	return (
		<div className="flex h-full flex-col">
			{/* Header */}
			<div className="flex flex-col gap-2 border-b border-app-line/50 bg-app-darkBox/20 px-6 py-4">
				<div className="flex items-start justify-between gap-3">
					<TaskText text={detail.task} />
					<div className="flex items-center gap-2">
						{isLive && detail.channel_id && (
							<CancelWorkerButton
								channelId={detail.channel_id}
								workerId={detail.id}
							/>
						)}
						{detail.interactive && (
							<Badge variant="outline" size="sm">
								interactive
							</Badge>
						)}
						<Badge
							variant={workerTypeBadgeVariant(detail.worker_type)}
							size="sm"
						>
							{detail.worker_type}
						</Badge>
						<Badge
							variant={statusBadgeVariant(
								isIdle ? "idle" : isLive ? "running" : normalizeStatus(detail.status),
							)}
							size="sm"
						>
							{isLive && !isIdle && (
								<span className="h-1.5 w-1.5 animate-pulse rounded-full bg-current" />
							)}
							{isIdle ? "idle" : isLive ? "running" : normalizeStatus(detail.status)}
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
					) : isIdle ? (
						<span className="text-blue-500">Idle — waiting for follow-up</span>
					) : (
						duration && <span>{duration}</span>
					)}
					{!isLive && <span>{formatTimeAgo(detail.started_at)}</span>}
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

			{/* Tab bar (only for OpenCode workers with embed data) */}
			{hasOpenCodeEmbed && (
				<div className="flex border-b border-app-line/50">
					<button
						onClick={() => setActiveTab("opencode")}
						className={cx(
							"px-4 py-2 text-xs font-medium transition-colors",
							activeTab === "opencode"
								? "border-b-2 border-accent text-accent"
								: "text-ink-faint hover:text-ink-dull",
						)}
					>
						OpenCode
					</button>
					<button
						onClick={() => setActiveTab("transcript")}
						className={cx(
							"px-4 py-2 text-xs font-medium transition-colors",
							activeTab === "transcript"
								? "border-b-2 border-accent text-accent"
								: "text-ink-faint hover:text-ink-dull",
						)}
					>
						Transcript
					</button>
				</div>
			)}

			{/* Content */}
			{activeTab === "opencode" && hasOpenCodeEmbed ? (
				<OpenCodeEmbed
					port={detail.opencode_port!}
					sessionId={detail.opencode_session_id!}
				/>
			) : (
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

					{/* OpenCode live parts (for running/idle OpenCode workers) */}
					{isOpenCode && isLive && openCodeParts.length > 0 ? (
						<div className="px-6 py-4">
							<h3 className="mb-3 text-tiny font-medium uppercase tracking-wider text-ink-faint">
								{isIdle ? "Transcript" : "Live Transcript"}
							</h3>
							<div className="flex flex-col gap-3">
								{openCodeParts.map((part) => (
									<motion.div
										key={part.id}
										initial={{opacity: 0, y: 6}}
										animate={{opacity: 1, y: 0}}
										transition={{duration: 0.2, ease: "easeOut"}}
									>
										<OpenCodePartView part={part} />
									</motion.div>
								))}
								{isRunning && currentTool && (
									<div className="flex items-center gap-2 py-2 text-tiny text-accent">
										<span className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
										Running {currentTool}...
									</div>
								)}
								{isIdle && (
									<div className="flex items-center gap-2 py-2 text-tiny text-blue-500">
										Waiting for follow-up input...
									</div>
								)}
							</div>
						</div>
					) : transcript && transcript.length > 0 ? (
						<div className="px-6 py-4">
							<h3 className="mb-3 text-tiny font-medium uppercase tracking-wider text-ink-faint">
								{isLive && !isIdle ? "Live Transcript" : "Transcript"}
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
								{isIdle && (
									<div className="flex items-center gap-2 py-2 text-tiny text-blue-500">
										Waiting for follow-up input...
									</div>
								)}
							</div>
						</div>
					) : liveWorker && !isIdle ? (
						<div className="flex flex-col items-center justify-center gap-2 py-12 text-ink-faint">
							<div className="h-2 w-2 animate-pulse rounded-full bg-amber-500" />
							<p className="text-xs">Waiting for first tool call...</p>
						</div>
					) : (
						<div className="px-6 py-8 text-center text-xs text-ink-faint">
							No transcript available for this worker
						</div>
					)}
				</div>
			)}
		</div>
	);
}

function OpenCodeEmbed({port, sessionId}: {port: number; sessionId: string}) {
	const [state, setState] = useState<"loading" | "ready" | "error">("loading");

	useEffect(() => {
		setState("loading");
		const controller = new AbortController();

		fetch(`/api/opencode/${port}/global/health`, {signal: controller.signal})
			.then((response) => {
				setState(response.ok ? "ready" : "error");
			})
			.catch(() => {
				setState("error");
			});

		return () => controller.abort();
	}, [port, sessionId]);

	if (state === "loading") {
		return (
			<div className="flex flex-1 items-center justify-center">
				<div className="flex items-center gap-2 text-xs text-ink-faint">
					<span className="h-2 w-2 animate-pulse rounded-full bg-accent" />
					Connecting to OpenCode...
				</div>
			</div>
		);
	}

	if (state === "error") {
		return (
			<div className="flex flex-1 flex-col items-center justify-center gap-2 text-ink-faint">
				<p className="text-xs">OpenCode server is not reachable</p>
				<p className="text-tiny">
					The server may have been stopped. Try the Transcript tab for available data.
				</p>
			</div>
		);
	}

	// Build the iframe URL. OpenCode uses base64url-encoded directory paths
	// in its SPA routing. We load the root and let the app navigate — the
	// server knows its directory, and the session list will show this session.
	// Direct deep-linking: /api/opencode/{port}/{base64dir}/session/{sessionId}
	const iframeSrc = `/api/opencode/${port}/`;

	return (
		<iframe
			src={iframeSrc}
			className="h-full w-full flex-1 border-0"
			title="OpenCode"
			sandbox="allow-scripts allow-same-origin allow-forms allow-popups"
		/>
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
				api
					.cancelProcess(channelId, "worker", workerId)
					.catch(console.warn)
					.finally(() => setCancelling(false));
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

// -- OpenCode-native part renderers --

function OpenCodePartView({part}: {part: OpenCodePart}) {
	switch (part.type) {
		case "text":
			return (
				<div className="text-xs text-ink">
					<Markdown>{part.text}</Markdown>
				</div>
			);
		case "tool":
			return <OpenCodeToolPartView part={part} />;
		case "step_start":
			return (
				<div className="flex items-center gap-2 border-t border-app-line/20 pt-3">
					<div className="h-px flex-1 bg-app-line/30" />
					<span className="text-tiny text-ink-faint">Step</span>
					<div className="h-px flex-1 bg-app-line/30" />
				</div>
			);
		case "step_finish":
			return (
				<div className="flex items-center gap-2 border-b border-app-line/20 pb-3">
					<div className="h-px flex-1 bg-app-line/30" />
					<span className="text-tiny text-ink-faint">
						{part.reason ? `End: ${part.reason}` : "End step"}
					</span>
					<div className="h-px flex-1 bg-app-line/30" />
				</div>
			);
		default:
			return null;
	}
}

function OpenCodeToolPartView({
	part,
}: {
	part: Extract<OpenCodePart, {type: "tool"}>;
}) {
	const [expanded, setExpanded] = useState(false);
	const isRunning = part.status === "running";
	const isCompleted = part.status === "completed";
	const isError = part.status === "error";

	const statusIcon = isCompleted
		? "\u2713"
		: isError
			? "\u2717"
			: isRunning
				? "\u25B6"
				: "\u25CB";

	const statusColor = isCompleted
		? "text-emerald-500"
		: isError
			? "text-red-400"
			: isRunning
				? "text-accent"
				: "text-ink-faint";

	const title =
		(part.status === "running" || part.status === "completed")
			? (part as any).title
			: undefined;

	const input =
		(part.status === "running" || part.status === "completed")
			? (part as any).input
			: undefined;

	const output = part.status === "completed" ? (part as any).output : undefined;
	const error = part.status === "error" ? (part as any).error : undefined;

	return (
		<div className="rounded-md border border-app-line/50 bg-app-darkBox/30">
			<button
				onClick={() => setExpanded(!expanded)}
				className="flex w-full items-center gap-2 px-3 py-2 text-left text-xs"
			>
				<span className={cx(statusColor, isRunning ? "animate-pulse" : "")}>
					{statusIcon}
				</span>
				<span className="font-medium text-ink-dull">{part.tool}</span>
				{title && (
					<span className="flex-1 truncate text-ink-faint">{title}</span>
				)}
				{!title && !expanded && input && (
					<span className="flex-1 truncate text-ink-faint">
						{input.slice(0, 80)}
					</span>
				)}
				{isRunning && (
					<span className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
				)}
			</button>
			{expanded && (
				<div className="border-t border-app-line/30">
					{input && (
						<div className="border-b border-app-line/20 px-3 py-2">
							<p className="mb-1 text-tiny font-medium text-ink-faint">Input</p>
							<pre className="max-h-40 overflow-auto font-mono text-tiny text-ink-dull">
								{input}
							</pre>
						</div>
					)}
					{output && (
						<div className="px-3 py-2">
							<p className="mb-1 text-tiny font-medium text-ink-faint">Output</p>
							<pre className="max-h-60 overflow-auto whitespace-pre-wrap font-mono text-tiny text-ink-dull">
								{output.length > 500 ? output.slice(0, 500) + "..." : output}
							</pre>
						</div>
					)}
					{error && (
						<div className="px-3 py-2">
							<p className="mb-1 text-tiny font-medium text-red-400">Error</p>
							<pre className="font-mono text-tiny text-red-300">{error}</pre>
						</div>
					)}
				</div>
			)}
		</div>
	);
}
