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
	type OpenCodePart,
} from "@/api/client";
import {
	ToolCall,
	pairTranscriptSteps,
	openCodePartToPair,
} from "@/components/ToolCall";
import {Badge, badgeVariants} from "@spacedrive/primitives";
import {formatTimeAgo, formatDuration} from "@/lib/format";
import {LiveDuration} from "@/components/LiveDuration";
import {useLiveContext} from "@/hooks/useLiveContext";
import {useIsMobile} from "@/hooks/useMediaQuery";
import {cx} from "class-variance-authority";
import {ProviderIcon} from "@/lib/providerIcons";
import {CaretLeft} from "@phosphor-icons/react";

import {OpenCodeEmbed, base64UrlEncode} from "@/components/OpenCodeEmbed";

const STATUS_FILTERS = ["all", "running", "idle", "done", "failed"] as const;
type StatusFilter = (typeof STATUS_FILTERS)[number];

const KNOWN_STATUSES = new Set(["running", "idle", "done", "failed"]);

/** Collapse runs of 3+ spaces to 2, preventing Markdown 4-space code blocks. */
function stripExcessWhitespace(text: string): string {
	return text.replace(/ {3,}/g, "  ");
}

function normalizeStatus(status: string): string {
	if (KNOWN_STATUSES.has(status)) return status;
	// Legacy rows where set_status text overwrote the state enum.
	// If it has a completed_at it finished, otherwise it was interrupted.
	return "failed";
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
	const {
		activeWorkers,
		workerEventVersion,
		liveTranscripts,
		liveOpenCodeParts,
	} = useLiveContext();

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
				opencode_session_id: null,
				directory: null,
				interactive: live.interactive,
				project_id: null,
				project_name: null,
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
		const live = selectedWorkerId
			? scopedActiveWorkers[selectedWorkerId]
			: null;

		if (detailData) {
			// DB data exists — overlay live status if worker is still running
			if (!live) return detailData;
			return {...detailData, status: live.isIdle ? "idle" : "running"};
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
			directory: null,
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

	const isMobile = useIsMobile();
	// On mobile, master-detail collapses to a single pane: list when no
	// worker is selected, detail (with back button) when one is.
	const showList = !isMobile || !selectedWorkerId;
	const showDetail = !isMobile || !!selectedWorkerId;

	return (
		<div className="flex h-full">
			{/* Left column: worker list */}
			{showList && (
				<div
					className={
						isMobile
							? "flex w-full flex-shrink-0 flex-col"
							: "flex w-[360px] flex-shrink-0 flex-col border-r border-app-line/50"
					}
				>
					{/* Toolbar */}
					<div className="flex items-center gap-3 border-b border-app-line/50 bg-app-dark-box/20 px-4 py-2.5">
						<input
							type="text"
							placeholder="Search tasks..."
							value={search}
							onChange={(e) => setSearch(e.target.value)}
							className="h-9 flex-1 rounded-md border border-app-line/50 bg-app-input px-2.5 text-xs text-ink placeholder:text-ink-faint focus:border-accent/50 focus:outline-none md:h-7"
						/>
						<span className="text-tiny text-ink-faint">{total}</span>
					</div>

					{/* Status filter pills */}
					<div className="flex items-center gap-1.5 overflow-x-auto border-b border-app-line/50 px-4 py-2">
						{STATUS_FILTERS.map((filter) => (
							<button
								key={filter}
								onClick={() => setStatusFilter(filter)}
								className={cx(
									"shrink-0 rounded-full px-2.5 py-0.5 text-tiny font-medium transition-colors",
									statusFilter === filter
										? "bg-app-hover text-ink"
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
			)}

			{/* Right column: detail view */}
			{showDetail && (
				<div className="flex flex-1 flex-col overflow-hidden">
					{isMobile && selectedWorkerId && (
						<div className="flex shrink-0 items-center gap-2 border-b border-app-line/50 px-2 py-1.5">
							<button
								type="button"
								aria-label="Back to worker list"
								onClick={() => selectWorker(null)}
								className="flex h-11 w-11 items-center justify-center rounded-lg text-ink hover:bg-app-box/60 active:bg-app-box/80"
							>
								<CaretLeft size={20} weight="regular" />
							</button>
							<span className="truncate text-xs text-ink-faint">
								Back to workers
							</span>
						</div>
					)}
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
			)}
		</div>
	);
}

export interface LiveWorker {
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
	const displayStatus = isIdle
		? "idle"
		: isLive
			? "running"
			: normalizeStatus(worker.status);
	const toolCalls = liveWorker?.toolCalls ?? worker.tool_calls;

	return (
		<button
			onClick={onClick}
			className={cx(
				"flex w-full flex-col gap-1 border-b border-app-line/30 px-4 py-3 text-left transition-colors",
				selected ? "bg-app-hover/30" : "",
			)}
		>
			<div className="flex items-center justify-between gap-2">
				<p
					className={cx(
						"min-w-0 flex-1 truncate text-xs font-medium",
						selected ? "text-ink" : "text-ink-dull",
					)}
				>
					{worker.task.replace(/^\[opencode]\s*/, "")}
				</p>
				<div className="flex shrink-0 items-center gap-1.5 pointer-events-none">
					{worker.worker_type === "opencode" ? (
						<Badge variant="outline" size="sm">
							OpenCode
						</Badge>
					) : isInteractive ? (
						<Badge variant="outline" size="sm">
							interactive
						</Badge>
					) : null}
					<Badge variant="outline" size="sm">
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
							liveWorker?.startedAt ?? new Date(worker.started_at).getTime()
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

export function WorkerDetail({
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
	const duration = durationBetween(
		detail.started_at,
		detail.completed_at ?? null,
	);
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

	// For live workers, prefer the SSE-accumulated transcript when it has content
	// (it's the most up-to-date). Fall back to detail.transcript which may come
	// from the DB or the server-side live cache (survives page refreshes).
	// For completed workers, always use the persisted DB transcript.
	const rawTranscript = isLive
		? liveTranscript && liveTranscript.length > 0
			? liveTranscript
			: (detail.transcript ?? null)
		: (detail.transcript ?? null);
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
			<div className="flex flex-col gap-2 border-b border-app-line/50 bg-app-dark-box/20 px-6 py-4">
				<div className="flex items-start justify-between gap-3">
					<TaskText text={detail.task} />
					{isLive && detail.channel_id && (
						<CancelWorkerButton
							channelId={detail.channel_id}
							workerId={detail.id}
						/>
					)}
				</div>
				<div className="flex items-center justify-between gap-3">
					<div className="flex items-center gap-3 text-tiny text-ink-faint">
						{detail.channel_name && <span>{detail.channel_name}</span>}
						{hasOpenCodeEmbed && detail.opencode_port && (
							<OpenCodeDirectLink
								port={detail.opencode_port}
								sessionId={detail.opencode_session_id!}
								directory={detail.directory ?? null}
							/>
						)}
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
							<span className="text-blue-500">
								Idle — waiting for follow-up
							</span>
						) : (
							duration && <span>{duration}</span>
						)}
						{!isLive && <span>{formatTimeAgo(detail.started_at)}</span>}
						{toolCalls > 0 && <span>{toolCalls} tool calls</span>}
					</div>
					{hasOpenCodeEmbed && (
						<div className="flex items-center gap-1 rounded-full border border-app-line/50 p-0.5">
							<button
								onClick={() => setActiveTab("opencode")}
								className={cx(
									"rounded-full px-2 py-0.5 text-tiny font-medium transition-colors",
									activeTab === "opencode"
										? "bg-app-hover/50 text-ink"
										: "text-ink-faint hover:text-ink-dull",
								)}
							>
								OpenCode
							</button>
							<button
								onClick={() => setActiveTab("transcript")}
								className={cx(
									"rounded-full px-2 py-0.5 text-tiny font-medium transition-colors",
									activeTab === "transcript"
										? "bg-app-hover/50 text-ink"
										: "text-ink-faint hover:text-ink-dull",
								)}
							>
								Transcript
							</button>
						</div>
					)}
				</div>
				{/* Live status bar for running workers */}
				{isRunning && (currentTool || displayStatus) && (
					<div className="flex items-center gap-2 text-tiny">
						{currentTool ? (
							<span className="text-accent">Running {currentTool}...</span>
						) : displayStatus ? (
							<span className="text-amber-500">{displayStatus}</span>
						) : null}
					</div>
				)}
			</div>

			{/* Content */}
			{activeTab === "opencode" && hasOpenCodeEmbed ? (
				<OpenCodeEmbed
					port={detail.opencode_port!}
					sessionId={detail.opencode_session_id!}
					directory={detail.directory ?? null}
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
								{pairTranscriptSteps(transcript).map((item, index) => (
									<motion.div
										key={item.kind === "tool" ? item.pair.id : `text-${index}`}
										initial={{opacity: 0, y: 6}}
										animate={{opacity: 1, y: 0}}
										transition={{duration: 0.2, ease: "easeOut"}}
									>
										{item.kind === "text" ? (
											<div className="text-xs text-ink-dull">
												<Markdown>{stripExcessWhitespace(item.text)}</Markdown>
											</div>
										) : (
											<ToolCall pair={item.pair} />
										)}
									</motion.div>
								))}
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

function OpenCodeDirectLink({
	port,
	sessionId,
	directory: initialDirectory,
}: {
	port: number;
	sessionId: string;
	directory: string | null;
}) {
	const [directory, setDirectory] = useState<string | null>(initialDirectory);

	// Reset when the worker changes (props point to a different OpenCode instance)
	useEffect(() => {
		setDirectory(initialDirectory);
	}, [port, sessionId, initialDirectory]);

	useEffect(() => {
		if (initialDirectory) return;
		// Fetch directory from the OpenCode session API as fallback.
		const controller = new AbortController();
		fetch(`/api/opencode/${port}/session/${sessionId}`, {
			signal: controller.signal,
		})
			.then((r) => (r.ok ? r.json() : null))
			.then((session) => {
				if (session?.directory) setDirectory(session.directory);
			})
			.catch(() => {});
		return () => controller.abort();
	}, [port, sessionId, initialDirectory]);

	const href = directory
		? `/api/opencode/${port}/${base64UrlEncode(directory)}/session/${sessionId}`
		: `/api/opencode/${port}`;

	return (
		<a
			href={href}
			target="_blank"
			rel="noopener noreferrer"
			className={cx(badgeVariants({variant: "outline", size: "sm"}), "w-fit")}
		>
			<ProviderIcon
				provider="opencode-zen"
				size={12}
				className="text-current"
			/>
			OpenCode ::{port}
		</a>
	);
}

function TaskText({text}: {text: string}) {
	const [hovered, setHovered] = useState(false);
	const containerRef = useRef<HTMLDivElement>(null);
	const textRef = useRef<HTMLParagraphElement>(null);
	const [isTruncated, setIsTruncated] = useState(false);

	useEffect(() => {
		const element = textRef.current;
		if (!element) return;
		setIsTruncated(element.scrollWidth > element.clientWidth);
	}, [text]);

	return (
		<div
			ref={containerRef}
			className="relative min-w-0 flex-1"
			onMouseEnter={() => {
				if (isTruncated) setHovered(true);
			}}
			onMouseLeave={() => setHovered(false)}
		>
			<p ref={textRef} className="truncate text-sm font-medium text-ink-dull">
				{text}
			</p>
			{hovered && (
				<div className="absolute left-0 top-full z-50 mt-1 max-h-60 max-w-lg overflow-y-auto rounded-lg border border-app-line/50 bg-app-box/95 px-4 py-3 shadow-xl backdrop-blur-sm">
					<p className="whitespace-pre-wrap break-words text-sm text-ink-dull">
						{text}
					</p>
				</div>
			)}
		</div>
	);
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
			return <ToolCall pair={openCodePartToPair(part)} />;
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
