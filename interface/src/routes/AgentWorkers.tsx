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
import {ToolCall, pairTranscriptSteps, openCodePartToPair} from "@/components/ToolCall";
import {Badge} from "@/ui/Badge";
import {formatTimeAgo, formatDuration} from "@/lib/format";
import {LiveDuration} from "@/components/LiveDuration";
import {useLiveContext} from "@/hooks/useLiveContext";
import {cx} from "@/ui/utils";
import {badgeStyles} from "@/ui/Badge";
import {ProviderIcon} from "@/lib/providerIcons";
import { getApiBaseUrl, assetUrl } from "@/lib/backend";

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
				selected ? "bg-app-hover/30" : "",
			)}
		>
			<div className="flex items-center justify-between gap-2">
				<p className={cx("min-w-0 flex-1 truncate text-xs font-medium", selected ? "text-ink" : "text-ink-dull")}>
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
					<Badge
						variant="outline"
						size="sm"
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

	// For live workers, prefer the SSE-accumulated transcript when it has content
	// (it's the most up-to-date). Fall back to detail.transcript which may come
	// from the DB or the server-side live cache (survives page refreshes).
	// For completed workers, always use the persisted DB transcript.
	const rawTranscript = isLive
		? (liveTranscript && liveTranscript.length > 0 ? liveTranscript : detail.transcript ?? null)
		: detail.transcript ?? null;
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
								directory={detail.directory}
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
							<span className="text-blue-500">Idle — waiting for follow-up</span>
						) : (
							duration && <span>{duration}</span>
						)}
						{!isLive && <span>{formatTimeAgo(detail.started_at)}</span>}
						{toolCalls > 0 && (
							<span>{toolCalls} tool calls</span>
						)}
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
			{activeTab === "opencode" && hasOpenCodeEmbed ? (
				<OpenCodeEmbed
					port={detail.opencode_port!}
					sessionId={detail.opencode_session_id!}
					directory={detail.directory}
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
											<div className="text-xs text-ink">
												<Markdown>{item.text}</Markdown>
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
		fetch(`${getApiBaseUrl()}/opencode/${port}/session/${sessionId}`, {
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
		? `${getApiBaseUrl()}/opencode/${port}/${base64UrlEncode(directory)}/session/${sessionId}`
		: `${getApiBaseUrl()}/opencode/${port}`;

	return (
		<a
			href={href}
			target="_blank"
			rel="noopener noreferrer"
			className={cx(badgeStyles({ variant: "outline", size: "sm" }), "w-fit")}
		>
			<ProviderIcon provider="opencode-zen" size={12} className="text-current" />
			OpenCode ::{port}
		</a>
	);
}

/**
 * Cache for the OpenCode embed assets. Once loaded, the JS module and CSS
 * text are reused across mounts so we don't re-fetch on every tab switch.
 */
let embedAssetsPromise: Promise<{
	mountOpenCode: (el: HTMLElement, config: { serverUrl: string; initialRoute?: string }) => {
		dispose: () => void;
		navigate: (route: string) => void;
	};
	cssText: string;
}> | null = null;

function loadScript(src: string): Promise<void> {
	return new Promise((resolve, reject) => {
		// Don't add the same script twice
		if (document.querySelector(`script[src="${src}"]`)) {
			resolve();
			return;
		}
		const script = document.createElement("script");
		script.type = "module";
		script.src = src;
		script.onload = () => resolve();
		script.onerror = () => reject(new Error(`Failed to load script: ${src}`));
		document.head.appendChild(script);
	});
}

function loadEmbedAssets() {
	if (embedAssetsPromise) return embedAssetsPromise;
	embedAssetsPromise = (async () => {
		// Load the manifest to find hashed asset filenames
		const manifestRes = await fetch(assetUrl("/opencode-embed/manifest.json"));
		if (!manifestRes.ok) throw new Error("Failed to load opencode-embed manifest");
		const manifest: { js: string; css: string } = await manifestRes.json();

		// Guard against path traversal or unexpected values from the manifest
		const validAssetPath = (p: unknown): p is string =>
			typeof p === "string" && p.startsWith("assets/") && !p.includes("..");
		if (!validAssetPath(manifest.js) || !validAssetPath(manifest.css)) {
			throw new Error("Invalid asset paths in opencode-embed manifest");
		}

		// Load JS via <script> tag (required for /public files in Vite dev)
		// and CSS via fetch (to inject into Shadow DOM) in parallel
		const [, cssRes] = await Promise.all([
			loadScript(assetUrl(`/opencode-embed/${manifest.js}`)),
			fetch(assetUrl(`/opencode-embed/${manifest.css}`)),
		]);

		if (!cssRes.ok) throw new Error("Failed to load opencode-embed CSS");
		const cssText = await cssRes.text();

		// The embed entry attaches mountOpenCode to window.__opencode_embed__
		const embedApi = (window as any).__opencode_embed__;
		if (!embedApi?.mountOpenCode) {
			throw new Error("OpenCode embed module did not export mountOpenCode");
		}

		return { mountOpenCode: embedApi.mountOpenCode, cssText };
	})();
	// If loading fails, clear the cache so the next attempt retries
	embedAssetsPromise.catch(() => { embedAssetsPromise = null; });
	return embedAssetsPromise;
}

function OpenCodeEmbed({
	port,
	sessionId,
	directory: initialDirectory,
}: {
	port: number;
	sessionId: string;
	directory: string | null;
}) {
	const [state, setState] = useState<"loading" | "ready" | "error">("loading");
	const [errorMessage, setErrorMessage] = useState<string | null>(null);
	const hostRef = useRef<HTMLDivElement>(null);
	const handleRef = useRef<{ dispose: () => void; navigate: (route: string) => void } | null>(null);

	// Route through the Spacebot proxy so it works for hosted/Tailscale
	// users, not just local dev. The proxy handles forwarding to the
	// actual OpenCode instance. In local dev the Vite proxy forwards
	// /api/* to the Rust backend at 19898; in production it's same-origin.
	const serverUrl = `${getApiBaseUrl()}/opencode/${port}`;

	// Discover the event directory from the OpenCode server.
	// OpenCode tags SSE events with Instance.directory (the process CWD),
	// which may differ from the session's directory field. The SPA subscribes
	// to events by directory, so we must use the event-tagged directory in
	// the route or live updates won't work.
	const [eventDirectory, setEventDirectory] = useState<string | null>(null);

	// Reset when the worker changes (props point to a different OpenCode instance)
	useEffect(() => {
		setEventDirectory(null);
	}, [port, sessionId]);

	useEffect(() => {
		const controller = new AbortController();

		(async () => {
			try {
				// Strategy 1: Probe the SSE stream briefly. The first non-heartbeat
				// event carries the actual Instance.directory.
				// Use a separate controller for the probe timeout so aborting the
				// probe doesn't poison the fallback fetch or the unmount controller.
				const probeController = new AbortController();
				const onUnmount = () => probeController.abort();
				controller.signal.addEventListener("abort", onUnmount);

				const sseRes = await fetch(`${serverUrl}/global/event`, {
					headers: { Accept: "text/event-stream" },
					signal: probeController.signal,
				});
				if (!sseRes.ok || !sseRes.body) throw new Error("SSE probe failed");

				const reader = sseRes.body.pipeThrough(new TextDecoderStream()).getReader();
				const timeout = setTimeout(() => probeController.abort(), 8000);
				let buffer = "";

				while (!probeController.signal.aborted) {
					const { done, value } = await reader.read();
					if (done) break;
					buffer += value;

					// Parse SSE lines
					const lines = buffer.split("\n");
					buffer = lines.pop() ?? "";
					for (const line of lines) {
						if (!line.startsWith("data: ")) continue;
						try {
							const event = JSON.parse(line.slice(6));
							if (event.directory && event.directory !== "global") {
								setEventDirectory(event.directory);
								clearTimeout(timeout);
								reader.cancel();
								return;
							}
						} catch { /* not JSON, skip */ }
					}
				}
				clearTimeout(timeout);
			} catch {
				// If SSE probe fails (aborted, timeout, etc.), fall back to
				// the session API directory — unless the component unmounted.
				if (!controller.signal.aborted) {
					try {
						const res = await fetch(`${serverUrl}/session/${sessionId}`, {
							signal: controller.signal,
						});
						if (res.ok) {
							const session = await res.json();
							if (session?.directory) setEventDirectory(session.directory);
						}
					} catch { /* ignore */ }
				}
			}
		})();

		return () => controller.abort();
	}, [serverUrl, sessionId]);

	// Use the discovered event directory, fall back to the prop directory
	const resolvedDirectory = eventDirectory ?? initialDirectory;

	// Build the initial route for the memory router
	const initialRoute = resolvedDirectory
		? `/${base64UrlEncode(resolvedDirectory)}/session/${sessionId}`
		: "/";

	// Mount the OpenCode SPA into a Shadow DOM
	useEffect(() => {
		const host = hostRef.current;
		if (!host) return;

		let disposed = false;

		(async () => {
			try {
				setState("loading");

				// Pre-seed OpenCode layout preferences so it starts with a clean
				// chat-only view (sidebar, terminal, file tree, review all closed).
				const layoutKey = "opencode.global.dat:layout";
				if (!localStorage.getItem(layoutKey)) {
					localStorage.setItem(layoutKey, JSON.stringify({
						sidebar: { opened: false, width: 344, workspaces: {}, workspacesDefault: false },
						terminal: { height: 280, opened: false },
						review: { diffStyle: "split", panelOpened: false },
						fileTree: { opened: false, width: 344, tab: "changes" },
						session: { width: 600 },
						mobileSidebar: { opened: false },
						sessionTabs: {},
						sessionView: {},
						handoff: {},
					}));
				}

				// First check if the OpenCode server is reachable
				const healthRes = await fetch(`${serverUrl}/global/health`);
				if (!healthRes.ok) throw new Error("OpenCode server not reachable");

				// Load the embed assets (cached after first load)
				const { mountOpenCode, cssText } = await loadEmbedAssets();

				if (disposed) return;

				// Create Shadow DOM for CSS isolation
				const shadow = host.shadowRoot ?? host.attachShadow({ mode: "open" });

				// Clear any previous content
				shadow.innerHTML = "";

				// Inject the OpenCode CSS into the shadow root
				const style = document.createElement("style");
				style.textContent = cssText;
				shadow.appendChild(style);

				// Hide the sidebar, mobile sidebar, and top bar in embedded
				// mode — we only want the session/chat view.
				const overrides = document.createElement("style");
				overrides.textContent = `
					[data-component="sidebar-nav-desktop"],
					[data-component="sidebar-nav-mobile"],
					header:has(#opencode-titlebar-center),
					[data-session-title] { display: none !important; }
					main { border: none !important; border-radius: 0 !important; }
				`;
				shadow.appendChild(overrides);

				// Create the mount point inside the shadow.
				// Apply the base styles that OpenCode normally gets from
				// <body class="text-12-regular antialiased overflow-hidden">
				// and ensure rem units resolve correctly by setting font-size
				// on the shadow host (rem in Shadow DOM still resolves against
				// document <html>, but this establishes the inherited font-size
				// for em/% units used by child elements).
				host.style.fontSize = "16px";
				const mountDiv = document.createElement("div");
				mountDiv.id = "opencode-root";
				mountDiv.style.cssText = "display:flex;flex-direction:column;height:100%;width:100%;overflow:hidden;font-size:13px;line-height:150%;-webkit-font-smoothing:antialiased;";
				shadow.appendChild(mountDiv);

				// Inject a copy of OpenCode's CSS into the document <head>
				// so that Kobalte portals (dropdowns, dialogs, toasts) that
				// escape the Shadow DOM into document.body still get styled.
				// We scope it to avoid polluting Spacebot's own styles by
				// wrapping in a layer.
				let portalStyle = document.getElementById("opencode-portal-css");
				if (!portalStyle) {
					portalStyle = document.createElement("style");
					portalStyle.id = "opencode-portal-css";
					// Use a CSS layer so OpenCode's global resets (*, html, body)
					// don't override Spacebot's styles. Portal elements from
					// Kobalte will pick up the right vars because they inherit
					// from :root where the CSS custom properties are set.
					portalStyle.textContent = `@layer opencode-portals {\n${cssText}\n}`;
					document.head.appendChild(portalStyle);
				}

				// Mount the SolidJS app
				const handle = mountOpenCode(mountDiv, {
					serverUrl,
					initialRoute,
				});

				handleRef.current = handle;
				setState("ready");
			} catch (error) {
				if (!disposed) {
					setState("error");
					setErrorMessage(error instanceof Error ? error.message : "Unknown error");
				}
			}
		})();

		return () => {
			disposed = true;
			// Dispose the SolidJS app on unmount
			if (handleRef.current) {
				handleRef.current.dispose();
				handleRef.current = null;
			}
			// Clean up shadow DOM content
			if (host.shadowRoot) {
				host.shadowRoot.innerHTML = "";
			}
			// Remove portal CSS from document head
			const portalStyle = document.getElementById("opencode-portal-css");
			if (portalStyle) portalStyle.remove();
		};
	}, [serverUrl, initialRoute]);

	// Navigate the embedded app when the route changes
	useEffect(() => {
		if (handleRef.current && resolvedDirectory) {
			const route = `/${base64UrlEncode(resolvedDirectory)}/session/${sessionId}`;
			handleRef.current.navigate(route);
		}
	}, [resolvedDirectory, sessionId]);

	// Always render the host div so the ref is available for the mount
	// effect. Overlay loading/error states on top.
	return (
		<div className="relative flex-1 overflow-hidden">
			<div
				ref={hostRef}
				className="absolute inset-0"
				style={{ contain: "strict" }}
			/>
			{state === "loading" && (
				<div className="absolute inset-0 flex items-center justify-center bg-app-darkBox/80">
					<div className="flex items-center gap-2 text-xs text-ink-faint">
						<span className="h-2 w-2 animate-pulse rounded-full bg-accent" />
						Loading OpenCode...
					</div>
				</div>
			)}
			{state === "error" && (
				<div className="absolute inset-0 flex flex-col items-center justify-center gap-2 bg-app-darkBox/80 text-ink-faint">
					<p className="text-xs">Failed to load OpenCode</p>
					<p className="text-tiny">
						{errorMessage || "The server may have been stopped. Try the Transcript tab for available data."}
					</p>
				</div>
			)}
		</div>
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
			onMouseEnter={() => { if (isTruncated) setHovered(true); }}
			onMouseLeave={() => setHovered(false)}
		>
			<p
				ref={textRef}
				className="truncate text-sm font-medium text-ink-dull"
			>
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

