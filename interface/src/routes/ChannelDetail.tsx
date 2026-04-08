import {useCallback, useEffect, useMemo, useRef, useState} from "react";
import {Link} from "@tanstack/react-router";
import {useQuery} from "@tanstack/react-query";
import {
	api,
	type ChannelInfo,
	type TimelineItem,
	type TimelineBranchRun,
	type TimelineWorkerRun,
	type TranscriptStep,
} from "@/api/client";
import {
	isOpenCodeWorker,
	type ChannelLiveState,
	type ActiveWorker,
	type ActiveBranch,
} from "@/hooks/useChannelLiveState";
import {useLiveContext} from "@/hooks/useLiveContext";
import {LiveDuration} from "@/components/LiveDuration";
import {Markdown} from "@/components/Markdown";
import {PromptInspectModal} from "@/components/PromptInspectModal";
import {pairTranscriptSteps, ToolCall} from "@/components/ToolCall";
import {formatTimestamp, platformIcon, platformColor} from "@/lib/format";
import {Button} from "@spacedrive/primitives";
import {X, Code} from "@phosphor-icons/react";

interface ChannelDetailProps {
	agentId: string;
	channelId: string;
	channel: ChannelInfo | undefined;
	liveState: ChannelLiveState | undefined;
	onLoadMore: () => void;
}

function CancelButton({
	onClick,
	className,
}: {
	onClick: () => void;
	className?: string;
}) {
	const [cancelling, setCancelling] = useState(false);
	return (
		<Button
			type="button"
			variant="ghost"
			size="icon"
			disabled={cancelling}
			onClick={(e) => {
				e.stopPropagation();
				setCancelling(true);
				onClick();
			}}
			className={`h-7 w-7 flex-shrink-0 text-ink-faint/50 hover:bg-status-error/15 hover:text-status-error ${className ?? ""}`}
			title="Cancel"
		>
			<X className="h-3.5 w-3.5" />
		</Button>
	);
}

function WorkerTranscriptView({
	workerId,
	agentId,
	isLive,
	liveTranscript,
}: {
	workerId: string;
	agentId: string;
	isLive: boolean;
	liveTranscript?: TranscriptStep[];
}) {
	const {data: detailData} = useQuery({
		queryKey: ["worker-detail", agentId, workerId],
		queryFn: () => api.workerDetail(agentId, workerId).catch(() => null),
	});

	const transcript = useMemo(() => {
		if (isLive) {
			return liveTranscript && liveTranscript.length > 0
				? liveTranscript
				: (detailData?.transcript ?? null);
		}
		return detailData?.transcript ?? null;
	}, [isLive, liveTranscript, detailData]);

	if (!transcript || transcript.length === 0) {
		if (isLive) {
			return (
				<div className="flex items-center gap-2 py-2 pl-4 text-tiny text-ink-faint">
					<span className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
					Waiting for first tool call...
				</div>
			);
		}
		return null;
	}

	const items = pairTranscriptSteps(transcript);

	return (
		<div className="mt-2 flex flex-col gap-2 pl-4">
			{items.map((item, index) =>
				item.kind === "text" ? (
					<div key={`text-${index}`} className="text-xs text-ink-dull">
						<Markdown>{item.text.replace(/ {3,}/g, "  ")}</Markdown>
					</div>
				) : (
					<ToolCall key={item.pair.id} pair={item.pair} />
				),
			)}
		</div>
	);
}

function LiveBranchRunItem({
	item,
	live,
	channelId,
}: {
	item: TimelineBranchRun;
	live: ActiveBranch;
	channelId: string;
}) {
	const displayTool = live.currentTool ?? live.lastTool;
	return (
		<div className="flex gap-3 px-3 py-2">
			<span className="flex-shrink-0 pt-0.5 text-tiny text-ink-faint">
				{formatTimestamp(new Date(item.started_at).getTime())}
			</span>
			<div className="min-w-0 flex-1">
				<div className="rounded-md bg-accent/10 px-3 py-2">
					<div className="flex min-w-0 items-center gap-2">
						<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
						<span className="text-sm font-medium text-accent-faint">Branch</span>
						<span className="min-w-0 flex-1 truncate text-sm text-ink-dull">
							{item.description}
						</span>
						<CancelButton
							className="ml-auto"
							onClick={() => {
								api
									.cancelProcess(channelId, "branch", item.id)
									.catch(console.warn);
							}}
						/>
					</div>
					<div className="mt-1 flex items-center gap-3 pl-4 text-tiny text-ink-faint">
						<LiveDuration startMs={live.startedAt} />
						{displayTool && (
							<span
								className={
									live.currentTool ? "text-accent/70" : "text-accent/40"
								}
							>
								{displayTool}
							</span>
						)}
						{live.toolCalls > 0 && <span>{live.toolCalls} tool calls</span>}
					</div>
				</div>
			</div>
		</div>
	);
}

function LiveWorkerRunItem({
	item,
	live,
	channelId,
	agentId,
}: {
	item: TimelineWorkerRun;
	live: ActiveWorker;
	channelId: string;
	agentId: string;
}) {
	const [expanded, setExpanded] = useState(true);
	const oc = isOpenCodeWorker(live);
	const {liveTranscripts} = useLiveContext();

	return (
		<div className="flex gap-3 px-3 py-2">
			<span className="flex-shrink-0 pt-0.5 text-tiny text-ink-faint">
				{formatTimestamp(new Date(item.started_at).getTime())}
			</span>
			<div className="min-w-0 flex-1">
				<div
					className={`w-full rounded-md px-3 py-2 transition-colors ${
						oc
							? "bg-ink-faint/10 hover:bg-ink-faint/15"
							: "bg-status-warning/10 hover:bg-status-warning/15"
					}`}
				>
					<div className="flex min-w-0 items-center gap-2 overflow-hidden">
						<button
							type="button"
							onClick={() => setExpanded(!expanded)}
							className="min-w-0 flex-1 text-left"
						>
							<div className="flex min-w-0 items-center gap-2 overflow-hidden">
								<div
									className={`h-2 w-2 animate-pulse rounded-full ${oc ? "bg-ink-faint" : "bg-status-warning"}`}
								/>
								<span
									className={`text-sm font-medium ${oc ? "text-ink-dull" : "text-status-warning"}`}
								>
									Worker
								</span>
								<span
									className={`min-w-0 flex-1 text-sm text-ink-dull ${
										expanded ? "whitespace-normal break-words" : "truncate"
									}`}
								>
									{item.task}
								</span>
								<span className="flex-shrink-0 text-tiny leading-5 text-ink-faint">
									{expanded ? "▾" : "▸"}
								</span>
							</div>
						</button>
						<Link
							to="/agents/$agentId/workers"
							params={{agentId}}
							search={{worker: item.id}}
							className={`flex-shrink-0 rounded border px-1.5 py-0.5 text-tiny font-medium transition-colors ${
								oc
									? "border-ink-faint/30 text-ink-dull hover:border-ink-faint/60 hover:bg-ink-faint/15"
									: "border-status-warning/30 text-status-warning hover:border-status-warning/60 hover:bg-status-warning/15"
							}`}
						>
							Open
						</Link>
						<CancelButton
							onClick={() => {
								api
									.cancelProcess(channelId, "worker", item.id)
									.catch(console.warn);
							}}
						/>
					</div>
				</div>
				{expanded && (
					<>
						<div className="mt-1 flex min-w-0 items-center gap-3 overflow-hidden pl-4 text-tiny text-ink-faint">
							<LiveDuration startMs={live.startedAt} />
							<span className="truncate">{live.status}</span>
							{live.currentTool && (
								<span
									className={`truncate ${oc ? "text-ink-faint/70" : "text-status-warning/70"}`}
								>
									{live.currentTool}
								</span>
							)}
							{live.toolCalls > 0 && <span>{live.toolCalls} tool calls</span>}
						</div>
						<WorkerTranscriptView
							workerId={item.id}
							agentId={agentId}
							isLive
							liveTranscript={liveTranscripts[item.id]}
						/>
					</>
				)}
			</div>
		</div>
	);
}

function BranchRunItem({item}: {item: TimelineBranchRun}) {
	const [expanded, setExpanded] = useState(true);

	return (
		<div className="flex gap-3 px-3 py-2">
			<span className="flex-shrink-0 pt-0.5 text-tiny text-ink-faint">
				{formatTimestamp(new Date(item.started_at).getTime())}
			</span>
			<div className="min-w-0 flex-1">
				<button
					type="button"
					onClick={() => setExpanded(!expanded)}
					className="w-full rounded-md bg-accent/10 px-3 py-2 text-left hover:bg-accent/15"
				>
					<div className="flex min-w-0 items-start gap-2">
						<span className="inline-flex flex-shrink-0 items-center gap-2 self-start">
							<span className="h-2 w-2 rounded-full bg-accent/50" />
							<span className="text-sm font-medium text-accent-faint">
								Branch
							</span>
						</span>
						<span
							className={`min-w-0 flex-1 text-sm text-ink-dull ${
								expanded ? "whitespace-normal break-words" : "truncate"
							}`}
						>
							{item.description}
						</span>
						{item.conclusion && (
							<span className="flex-shrink-0 self-start text-tiny leading-5 text-ink-faint">
								{expanded ? "▾" : "▸"}
							</span>
						)}
					</div>
				</button>
				{expanded && item.conclusion && (
					<div className="mt-1 rounded-md border border-accent/10 bg-accent/5 px-3 py-2">
						<div className="text-sm text-ink-dull">
							<Markdown className="break-words">
								{item.conclusion}
							</Markdown>
						</div>
					</div>
				)}
			</div>
		</div>
	);
}

function WorkerRunItem({
	item,
	agentId,
}: {
	item: TimelineWorkerRun;
	agentId: string;
}) {
	const [expanded, setExpanded] = useState(true);
	const oc = isOpenCodeWorker({task: item.task});

	return (
		<div className="flex gap-3 px-3 py-2">
			<span className="flex-shrink-0 pt-0.5 text-tiny text-ink-faint">
				{formatTimestamp(new Date(item.started_at).getTime())}
			</span>
			<div className="min-w-0 flex-1">
				<div
					className={`w-full rounded-md px-3 py-2 transition-colors ${
						oc
							? "bg-ink-faint/10 hover:bg-ink-faint/15"
							: "bg-status-warning/10 hover:bg-status-warning/15"
					}`}
				>
					<div className="flex min-w-0 items-center gap-2 overflow-hidden">
						<button
							type="button"
							onClick={() => setExpanded(!expanded)}
							className="min-w-0 flex-1 text-left"
						>
							<div className="flex min-w-0 items-center gap-2 overflow-hidden">
								<div
									className={`h-2 w-2 rounded-full ${oc ? "bg-ink-faint/50" : "bg-status-warning/50"}`}
								/>
								<span
									className={`text-sm font-medium ${oc ? "text-ink-dull" : "text-status-warning"}`}
								>
									Worker
								</span>
								<span
									className={`min-w-0 flex-1 text-sm text-ink-dull ${
										expanded ? "whitespace-normal break-words" : "truncate"
									}`}
								>
									{item.task}
								</span>
								<span className="flex-shrink-0 text-tiny leading-5 text-ink-faint">
									{expanded ? "▾" : "▸"}
								</span>
							</div>
						</button>
						<Link
							to="/agents/$agentId/workers"
							params={{agentId}}
							search={{worker: item.id}}
							className={`flex-shrink-0 rounded border px-1.5 py-0.5 text-tiny font-medium transition-colors ${
								oc
									? "border-ink-faint/30 text-ink-dull hover:border-ink-faint/60 hover:bg-ink-faint/15"
									: "border-status-warning/30 text-status-warning hover:border-status-warning/60 hover:bg-status-warning/15"
							}`}
						>
							Open
						</Link>
					</div>
				</div>
				{expanded && (
					<>
						<WorkerTranscriptView
							workerId={item.id}
							agentId={agentId}
							isLive={false}
						/>
						{item.result && (
							<div
								className={`mt-2 rounded-md border px-3 py-2 ${
									oc
										? "border-ink-faint/10 bg-ink-faint/5"
										: "border-status-warning/10 bg-status-warning/5"
								}`}
							>
								<div className="text-sm text-ink-dull">
									<Markdown className="break-words">
										{item.result}
									</Markdown>
								</div>
							</div>
						)}
					</>
				)}
			</div>
		</div>
	);
}

function TimelineEntry({
	item,
	liveWorkers,
	liveBranches,
	channelId,
	agentId,
}: {
	item: TimelineItem;
	liveWorkers: Record<string, ActiveWorker>;
	liveBranches: Record<string, ActiveBranch>;
	channelId: string;
	agentId: string;
}) {
	switch (item.type) {
		case "message":
			return (
				<div
					className={`flex gap-3 rounded-md px-3 py-2 ${
						item.role === "user" ? "bg-app-dark-box/30" : ""
					}`}
				>
					<span className="flex-shrink-0 pt-0.5 text-tiny text-ink-faint">
						{formatTimestamp(new Date(item.created_at).getTime())}
					</span>
					<div className="min-w-0 flex-1">
						<span
							className={`text-sm font-medium ${
								item.role === "user" ? "text-accent-faint" : "text-status-success"
							}`}
						>
							{item.role === "user"
								? (item.sender_name ?? "user")
								: (item.sender_name ?? "bot")}
						</span>
						<div className="mt-0.5 text-sm text-ink-dull">
							<Markdown>{item.content}</Markdown>
						</div>
					</div>
				</div>
			);
		case "branch_run": {
			const live = liveBranches[item.id];
			if (live)
				return (
					<LiveBranchRunItem
						item={item as TimelineBranchRun}
						live={live}
						channelId={channelId}
					/>
				);
			return <BranchRunItem item={item as TimelineBranchRun} />;
		}
		case "worker_run": {
			const live = liveWorkers[item.id];
			if (live)
				return (
					<LiveWorkerRunItem
						item={item as TimelineWorkerRun}
						live={live}
						channelId={channelId}
						agentId={agentId}
					/>
				);
			return (
				<WorkerRunItem item={item as TimelineWorkerRun} agentId={agentId} />
			);
		}
	}
}

export function ChannelDetail({
	agentId,
	channelId,
	channel,
	liveState,
	onLoadMore,
}: ChannelDetailProps) {
	const timeline = liveState?.timeline ?? [];
	const hasMore = liveState?.hasMore ?? false;
	const loadingMore = liveState?.loadingMore ?? false;
	const isTyping = liveState?.isTyping ?? false;
	const workers = liveState?.workers ?? {};
	const branches = liveState?.branches ?? {};
	const activeWorkerCount = Object.keys(workers).length;
	const activeBranchCount = Object.keys(branches).length;
	const hasActivity = activeWorkerCount > 0 || activeBranchCount > 0;
	const [inspectOpen, setInspectOpen] = useState(false);

	const scrollRef = useRef<HTMLDivElement>(null);
	const sentinelRef = useRef<HTMLDivElement>(null);
	const lastLoadMoreAtRef = useRef(0);

	// Trigger load when the sentinel at the top of the timeline becomes visible
	const handleIntersection = useCallback(
		(entries: IntersectionObserverEntry[]) => {
			const entry = entries[0];
			if (!entry?.isIntersecting) {
				return;
			}
			const now = Date.now();
			if (now - lastLoadMoreAtRef.current < 800) {
				return;
			}
			if (hasMore && !loadingMore) {
				lastLoadMoreAtRef.current = now;
				onLoadMore();
			}
		},
		[hasMore, loadingMore, onLoadMore],
	);

	useEffect(() => {
		const sentinel = sentinelRef.current;
		if (!sentinel) return;
		const observer = new IntersectionObserver(handleIntersection, {
			root: scrollRef.current,
			rootMargin: "200px",
		});
		observer.observe(sentinel);
		return () => observer.disconnect();
	}, [handleIntersection]);

	return (
		<div className="flex h-full">
			{/* Main channel content */}
			<div className="flex flex-1 flex-col overflow-hidden">
				{/* Channel sub-header */}
				<div className="flex h-12 items-center gap-3 border-b border-app-line/50 bg-app-dark-box/20 px-6">
					<Link
						to="/agents/$agentId/channels"
						params={{agentId}}
						className="text-tiny text-ink-faint hover:text-ink-dull"
					>
						Channels
					</Link>
					<span className="text-ink-faint/50">/</span>
					<span className="text-sm font-medium text-ink">
						{channel?.display_name ?? channelId}
						{channel?.display_name && (
							<span className="ml-2 font-normal text-ink-faint text-tiny">
								{channelId}
							</span>
						)}
					</span>
					{channel && (
						<span
							className={`inline-flex items-center rounded-md px-1.5 py-0.5 text-tiny font-medium ${platformColor(channel.platform)}`}
						>
							{platformIcon(channel.platform)}
						</span>
					)}

					{/* Right side: activity indicators + typing + inspect */}
					<div className="ml-auto flex items-center gap-3">
						{hasActivity && (
							<div className="flex items-center gap-2">
								{activeWorkerCount > 0 && (
									<div className="flex items-center gap-1.5">
										<div className="h-1.5 w-1.5 animate-pulse rounded-full bg-status-warning" />
										<span className="text-tiny text-status-warning">
											{activeWorkerCount} worker
											{activeWorkerCount !== 1 ? "s" : ""}
										</span>
									</div>
								)}
								{activeBranchCount > 0 && (
									<div className="flex items-center gap-1.5">
										<div className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
										<span className="text-tiny text-accent-faint">
											{activeBranchCount} branch
											{activeBranchCount !== 1 ? "es" : ""}
										</span>
									</div>
								)}
							</div>
						)}
						{isTyping && (
							<div className="flex items-center gap-1">
								<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
								<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-accent [animation-delay:0.2s]" />
								<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-accent [animation-delay:0.4s]" />
								<span className="ml-1 text-tiny text-ink-faint">typing</span>
							</div>
						)}
						<Button
							aria-label="Inspect prompt"
							onClick={() => setInspectOpen(true)}
							variant="ghost"
							size="icon"
							title="Inspect prompt"
						>
							<Code className="h-4 w-4" />
						</Button>
					</div>
				</div>

				{/* Timeline — flex-col-reverse keeps scroll pinned to bottom */}
				<div
					ref={scrollRef}
					className="flex flex-1 flex-col-reverse overflow-y-auto"
				>
					<div className="flex flex-col gap-1 p-6">
						{/* Sentinel for infinite scroll — sits above the oldest item */}
						<div ref={sentinelRef} className="h-px" />
						{loadingMore && (
							<div className="flex justify-center py-3">
								<span className="text-tiny text-ink-faint">
									Loading older messages...
								</span>
							</div>
						)}
						{!hasMore && timeline.length > 0 && (
							<div className="flex justify-center py-3">
								<span className="text-tiny text-ink-faint/50">
									Beginning of conversation
								</span>
							</div>
						)}
						{timeline.length === 0 ? (
							<p className="text-sm text-ink-faint">No messages yet</p>
						) : (
							timeline.map((item) => (
								<TimelineEntry
									key={item.id}
									item={item}
									liveWorkers={workers}
									liveBranches={branches}
									channelId={channelId}
									agentId={agentId}
								/>
							))
						)}
						{isTyping && (
							<div className="flex gap-3 px-3 py-2">
								<span className="flex-shrink-0 pt-0.5 text-tiny text-ink-faint">
									{formatTimestamp(Date.now())}
								</span>
								<div className="flex items-center gap-1.5">
									<span className="text-sm font-medium text-status-success">
										bot
									</span>
									<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-ink-faint" />
									<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-ink-faint [animation-delay:0.2s]" />
									<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-ink-faint [animation-delay:0.4s]" />
								</div>
							</div>
						)}
					</div>
				</div>
			</div>

			<PromptInspectModal
				open={inspectOpen}
				onOpenChange={setInspectOpen}
				channelId={channelId}
			/>
		</div>
	);
}
