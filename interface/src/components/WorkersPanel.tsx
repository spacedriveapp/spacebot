import {useState, useMemo, useCallback} from "react";
import {useQuery} from "@tanstack/react-query";
import {useQueries} from "@tanstack/react-query";
import {Queue, MagnifyingGlass, CaretLeft, Copy, Check, XCircle} from "@phosphor-icons/react";
import {
	CircleButton,
	PopoverRoot,
	PopoverTrigger,
	PopoverContent,
} from "@spacedrive/primitives";
import {
	api,
	type WorkerDetailResponse,
	type TranscriptStep,
	type OpenCodePart,
} from "@/api/client";
import {useLiveContext} from "@/hooks/useLiveContext";
import {formatTimeAgo} from "@/lib/format";
import {LiveDuration} from "@/components/LiveDuration";
import {WorkerDetail, type LiveWorker} from "@/routes/AgentWorkers";
import {cx} from "class-variance-authority";

type Tab = "interactive" | "history";

// ─── Public button (used in Sidebar and portal header) ─────────────────────

export function WorkersPanelButton() {
	const [open, setOpen] = useState(false);
	const {activeWorkers} = useLiveContext();
	const activeCount = Object.keys(activeWorkers).length;

	return (
		<PopoverRoot open={open} onOpenChange={setOpen}>
			<PopoverTrigger asChild>
				<CircleButton
					icon={Queue}
					title="Workers"
					variant={activeCount > 0 ? "active" : "default"}
				/>
			</PopoverTrigger>
			<PopoverContent
				align="start"
				side="right"
				sideOffset={8}
				collisionPadding={16}
				className="w-[420px] p-0"
			>
				<WorkersPanelContent />
			</PopoverContent>
		</PopoverRoot>
	);
}

// ─── Panel content ──────────────────────────────────────────────────────────

export function WorkersPanelContent() {
	const [tab, setTab] = useState<Tab>("history");
	const [search, setSearch] = useState("");
	const [selectedWorker, setSelectedWorker] = useState<{
		workerId: string;
		agentId: string;
		channelId?: string;
	} | null>(null);

	const {activeWorkers, liveTranscripts, liveOpenCodeParts} = useLiveContext();

	// Agent list for name lookups + per-agent worker history queries
	const {data: agentsData} = useQuery({
		queryKey: ["agents"],
		queryFn: api.agents,
		staleTime: 30_000,
	});
	const agents = agentsData?.agents ?? [];

	const agentNameMap = useMemo(() => {
		const map: Record<string, string> = {};
		for (const a of agents) map[a.id] = a.display_name ?? a.id;
		return map;
	}, [agents]);

	// Per-agent worker queries for history tab
	const workerQueries = useQueries({
		queries: agents.map((agent) => ({
			queryKey: ["workers-panel", agent.id],
			queryFn: () => api.workersList(agent.id, {limit: 100}),
			staleTime: 15_000,
			refetchInterval: 15_000,
		})),
	});

	// History: all DB workers merged and sorted newest-first
	const historyWorkers = useMemo(() => {
		type Row = {
			id: string;
			task: string;
			status: string;
			worker_type: string;
			started_at: string;
			completed_at: string | null;
			channel_id?: string | null;
			tool_calls: number;
			agentId: string;
			agentName: string;
		};
		const merged: Row[] = [];
		for (let i = 0; i < agents.length; i++) {
			const data = workerQueries[i]?.data;
			if (!data) continue;
			for (const w of data.workers) {
				merged.push({
					id: w.id,
					task: w.task,
					status: w.status,
					worker_type: w.worker_type,
					started_at: w.started_at,
					completed_at: w.completed_at ?? null,
					channel_id: w.channel_id,
					tool_calls: w.tool_calls,
					agentId: agents[i].id,
					agentName: agentNameMap[agents[i].id] ?? agents[i].id,
				});
			}
		}
		return merged.sort(
			(a, b) =>
				new Date(b.started_at).getTime() - new Date(a.started_at).getTime(),
		);
	}, [workerQueries, agents, agentNameMap]);

	// Interactive: live SSE workers (running or idle)
	const interactiveWorkers = useMemo(
		() =>
			Object.values(activeWorkers).sort((a, b) => b.startedAt - a.startedAt),
		[activeWorkers],
	);

	const term = search.trim().toLowerCase();

	const filteredInteractive = useMemo(
		() =>
			term
				? interactiveWorkers.filter((w) => w.task.toLowerCase().includes(term))
				: interactiveWorkers,
		[interactiveWorkers, term],
	);

	const filteredHistory = useMemo(
		() =>
			term
				? historyWorkers.filter((w) => w.task.toLowerCase().includes(term))
				: historyWorkers,
		[historyWorkers, term],
	);

	return (
		<div className="relative overflow-hidden" style={{height: 500}}>
			{/* List view */}
			<div
				className="absolute inset-0 flex flex-col transition-transform duration-250 ease-in-out"
				style={{transform: selectedWorker ? "translateX(-100%)" : "translateX(0)"}}
			>
				{/* Search */}
				<div className="flex items-center gap-2 border-b border-app-line px-3 py-2.5">
					<MagnifyingGlass className="h-3.5 w-3.5 shrink-0 text-ink-faint" />
					<input
						type="text"
						placeholder="Search workers..."
						value={search}
						onChange={(e) => setSearch(e.target.value)}
						className="flex-1 bg-transparent text-sm text-ink placeholder:text-ink-faint outline-none"
						autoFocus
					/>
				</div>

				{/* Tabs */}
				<div className="flex border-b border-app-line">
					<button
						onClick={() => setTab("history")}
						className={cx(
							"flex-1 py-2 text-xs font-medium transition-colors",
							tab === "history"
								? "bg-app-hover/40 text-ink"
								: "text-ink-faint hover:text-ink-dull",
						)}
					>
						History
					</button>
					<button
						onClick={() => setTab("interactive")}
						className={cx(
							"flex-1 py-2 text-xs font-medium transition-colors",
							tab === "interactive"
								? "bg-app-hover/40 text-ink"
								: "text-ink-faint hover:text-ink-dull",
						)}
					>
						Interactive{interactiveWorkers.length > 0 ? ` · ${interactiveWorkers.length}` : ""}
					</button>
				</div>

				{/* List */}
				<div className="flex-1 overflow-y-auto">
					{tab === "interactive" ? (
						filteredInteractive.length === 0 ? (
							<div className="py-10 text-center text-sm text-ink-faint">
								No active workers
							</div>
						) : (
							filteredInteractive.map((w) => (
								<WorkerPanelRow
									key={w.id}
									task={w.task}
									agentName={agentNameMap[w.agentId] ?? w.agentId}
									status={w.isIdle ? "idle" : "running"}
									workerType={w.workerType}
									startedAt={new Date(w.startedAt).toISOString()}
									completedAt={null}
									isLive
									toolCalls={w.toolCalls}
									onClick={() =>
										setSelectedWorker({workerId: w.id, agentId: w.agentId, channelId: w.channelId})
									}
								/>
							))
						)
					) : filteredHistory.length === 0 ? (
						<div className="py-10 text-center text-sm text-ink-faint">
							No worker history
						</div>
					) : (
						filteredHistory.map((w) => (
							<WorkerPanelRow
								key={w.id}
								task={w.task}
								agentName={w.agentName}
								status={w.status}
								workerType={w.worker_type}
								startedAt={w.started_at}
								completedAt={w.completed_at}
								isLive={w.id in activeWorkers}
								toolCalls={w.tool_calls}
								onClick={() =>
									setSelectedWorker({workerId: w.id, agentId: w.agentId, channelId: activeWorkers[w.id]?.channelId ?? w.channel_id ?? undefined})
								}
							/>
						))
					)}
				</div>
			</div>

			{/* Detail view — slides in from the right */}
			<div
				className="absolute inset-0 flex flex-col transition-transform duration-250 ease-in-out"
				style={{transform: selectedWorker ? "translateX(0)" : "translateX(100%)"}}
			>
				{selectedWorker && (
					<WorkerDetailInline
						workerId={selectedWorker.workerId}
						agentId={selectedWorker.agentId}
						channelId={selectedWorker.channelId}
						liveWorker={activeWorkers[selectedWorker.workerId] as LiveWorker | undefined}
						liveTranscript={liveTranscripts[selectedWorker.workerId]}
						liveOpenCodeParts={liveOpenCodeParts[selectedWorker.workerId]}
						onBack={() => setSelectedWorker(null)}
					/>
				)}
			</div>
		</div>
	);
}

// ─── Compact worker row ─────────────────────────────────────────────────────

function WorkerPanelRow({
	task,
	agentName,
	status,
	workerType,
	startedAt,
	completedAt,
	isLive,
	toolCalls,
	onClick,
}: {
	task: string;
	agentName: string;
	status: string;
	workerType: string;
	startedAt: string;
	completedAt: string | null;
	isLive: boolean;
	toolCalls: number;
	onClick: () => void;
}) {
	const isRunning = isLive && status === "running";
	const isIdle = status === "idle";

	return (
		<button
			onClick={onClick}
			className="flex w-full flex-col gap-1 border-b border-app-line/30 px-4 py-3 text-left transition-colors hover:bg-app-hover/20"
		>
			<div className="flex items-start gap-2">
				{isRunning && (
					<span className="mt-1.5 h-1.5 w-1.5 shrink-0 animate-pulse rounded-full bg-accent" />
				)}
				{isIdle && (
					<span className="mt-1.5 h-1.5 w-1.5 shrink-0 rounded-full bg-blue-400" />
				)}
				{!isRunning && !isIdle && (
					<span
						className={cx(
							"mt-1.5 h-1.5 w-1.5 shrink-0 rounded-full",
							status === "done" ? "bg-status-success" : "bg-status-error",
						)}
					/>
				)}
				<p className="min-w-0 flex-1 truncate text-sm text-ink-dull">
					{task.replace(/^\[opencode\]\s*/, "")}
				</p>
			</div>
			<div className="flex items-center gap-2 pl-3.5 text-tiny text-ink-faint">
				<span className="font-medium text-ink-faint">{agentName}</span>
				<span>·</span>
				<span>{workerType === "opencode" ? "OpenCode" : workerType}</span>
				<span>·</span>
				{isRunning ? (
					<LiveDuration startMs={new Date(startedAt).getTime()} />
				) : (
					<span>{formatTimeAgo(startedAt)}</span>
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

// ─── Inline detail view (slides in within the popover) ────────────────────

function WorkerDetailInline({
	workerId,
	agentId,
	channelId,
	liveWorker,
	liveTranscript,
	liveOpenCodeParts,
	onBack,
}: {
	workerId: string;
	agentId: string;
	channelId?: string;
	liveWorker?: LiveWorker;
	liveTranscript?: TranscriptStep[];
	liveOpenCodeParts?: Map<string, OpenCodePart>;
	onBack: () => void;
}) {
	const {data: detailData} = useQuery({
		queryKey: ["worker-detail", agentId, workerId],
		queryFn: () => api.workerDetail(agentId, workerId).catch(() => null),
	});

	const detail: WorkerDetailResponse | null = useMemo(() => {
		if (detailData) {
			if (!liveWorker) return detailData;
			return {...detailData, status: liveWorker.isIdle ? "idle" : "running"};
		}
		if (!liveWorker) return null;
		return {
			id: liveWorker.id,
			task: liveWorker.task,
			result: null,
			status: liveWorker.isIdle ? "idle" : "running",
			worker_type: liveWorker.workerType ?? "builtin",
			channel_id: null,
			channel_name: null,
			started_at: new Date(liveWorker.startedAt).toISOString(),
			completed_at: null,
			transcript: null,
			tool_calls: liveWorker.toolCalls,
			opencode_session_id: null,
			opencode_port: null,
			interactive: liveWorker.interactive,
			directory: null,
		};
	}, [detailData, liveWorker]);

	const [copied, setCopied] = useState(false);
	const [cancelling, setCancelling] = useState(false);

	const isActive = !!liveWorker;
	const resolvedChannelId = channelId ?? detail?.channel_id ?? undefined;
	const canCancel = isActive && !!resolvedChannelId;

	const handleCancel = useCallback(async () => {
		if (!resolvedChannelId || cancelling) return;
		setCancelling(true);
		try {
			await api.cancelProcess(resolvedChannelId, "worker", workerId);
		} catch (e) {
			console.error("Failed to cancel worker:", e);
		} finally {
			setCancelling(false);
		}
	}, [resolvedChannelId, workerId, cancelling]);

	const copyTranscript = useCallback(() => {
		const steps = liveTranscript ?? detail?.transcript;
		if (!steps || steps.length === 0) return;

		const lines: string[] = [];
		if (detail?.task) lines.push(`# ${detail.task}\n`);
		for (const step of steps) {
			if (step.type === "user_text" || step.type === "system_text") {
				lines.push(step.text);
			} else if (step.type === "tool_result") {
				lines.push(`[${step.name}] ${step.text}`);
			} else if (step.type === "action") {
				for (const c of step.content) {
					if (c.type === "text") lines.push(c.text);
					else if (c.type === "tool_call") lines.push(`> ${c.name}(${c.args})`);
				}
			}
		}
		if (detail?.result) lines.push(`\n---\nResult: ${detail.result}`);

		navigator.clipboard.writeText(lines.join("\n")).then(() => {
			setCopied(true);
			setTimeout(() => setCopied(false), 2000);
		});
	}, [detail, liveTranscript]);

	return (
		<>
			{/* Back bar */}
			<div className="flex items-center gap-1 border-b border-app-line px-2 py-1.5">
				<CircleButton
					icon={CaretLeft}
					title="Back to workers"
					onClick={onBack}
					variant="default"
				/>
				<div className="flex-1" />
				{canCancel && (
					<CircleButton
						icon={XCircle}
						title="Cancel worker"
						onClick={handleCancel}
						variant="default"
						disabled={cancelling}
					/>
				)}
				<CircleButton
					icon={copied ? Check : Copy}
					title="Copy transcript"
					onClick={copyTranscript}
					variant="default"
				/>
			</div>

			{!detail ? (
				<div className="flex flex-1 items-center justify-center">
					<p className="text-sm text-ink-faint">Loading...</p>
				</div>
			) : (
				<div className="flex-1 overflow-hidden">
					<WorkerDetail
						detail={detail}
						liveWorker={liveWorker}
						liveTranscript={liveTranscript}
						liveOpenCodeParts={liveOpenCodeParts}
					/>
				</div>
			)}
		</>
	);
}
