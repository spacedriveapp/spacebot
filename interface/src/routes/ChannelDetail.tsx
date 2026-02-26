import { useCallback, useEffect, useRef, useState } from "react";
import { Link } from "@tanstack/react-router";
import { AnimatePresence, motion } from "framer-motion";
import { api, type ChannelInfo, type TimelineItem, type TimelineBranchRun, type TimelineWorkerRun } from "@/api/client";
import type { ChannelLiveState, ActiveWorker, ActiveBranch } from "@/hooks/useChannelLiveState";
import { CortexChatPanel } from "@/components/CortexChatPanel";
import { LiveDuration } from "@/components/LiveDuration";
import { Markdown } from "@/components/Markdown";
import { formatTimestamp, platformIcon, platformColor } from "@/lib/format";
import { Button } from "@/ui";
import { Cancel01Icon, IdeaIcon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";

interface ChannelDetailProps {
	agentId: string;
	channelId: string;
	channel: ChannelInfo | undefined;
	liveState: ChannelLiveState | undefined;
	onLoadMore: () => void;
}

function CancelButton({ onClick, className }: { onClick: () => void; className?: string }) {
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
			className={`h-7 w-7 flex-shrink-0 text-ink-faint/50 hover:bg-red-500/15 hover:text-red-400 ${className ?? ""}`}
			title="Cancel"
		>
			<HugeiconsIcon icon={Cancel01Icon} className="h-3.5 w-3.5" />
		</Button>
	);
}

function LiveBranchRunItem({ item, live, channelId }: { item: TimelineBranchRun; live: ActiveBranch; channelId: string }) {
	const displayTool = live.currentTool ?? live.lastTool;
	return (
		<div className="flex gap-3 px-3 py-2">
			<span className="flex-shrink-0 pt-0.5 text-tiny text-ink-faint">
				{formatTimestamp(new Date(item.started_at).getTime())}
			</span>
			<div className="min-w-0 flex-1">
				<div className="rounded-md bg-violet-500/10 px-3 py-2">
					<div className="flex min-w-0 items-center gap-2">
						<div className="h-2 w-2 animate-pulse rounded-full bg-violet-400" />
						<span className="text-sm font-medium text-violet-300">Branch</span>
						<span className="min-w-0 flex-1 truncate text-sm text-ink-dull">{item.description}</span>
						<CancelButton className="ml-auto" onClick={() => { api.cancelProcess(channelId, "branch", item.id).catch(console.warn); }} />
					</div>
					<div className="mt-1 flex items-center gap-3 pl-4 text-tiny text-ink-faint">
						<LiveDuration startMs={live.startedAt} />
						{displayTool && (
							<span className={live.currentTool ? "text-violet-400/70" : "text-violet-400/40"}>{displayTool}</span>
						)}
						{live.toolCalls > 0 && (
							<span>{live.toolCalls} tool calls</span>
						)}
					</div>
				</div>
			</div>
		</div>
	);
}

function LiveWorkerRunItem({ item, live, channelId, agentId }: { item: TimelineWorkerRun; live: ActiveWorker; channelId: string; agentId: string }) {
	const [expanded, setExpanded] = useState(true);

	return (
		<div className="flex gap-3 px-3 py-2">
			<span className="flex-shrink-0 pt-0.5 text-tiny text-ink-faint">
				{formatTimestamp(new Date(item.started_at).getTime())}
			</span>
			<div className="min-w-0 flex-1">
				<div className="w-full rounded-md bg-amber-500/10 px-3 py-2 transition-colors hover:bg-amber-500/15">
					<div className="flex min-w-0 items-center gap-2 overflow-hidden">
						<button
							type="button"
							onClick={() => setExpanded(!expanded)}
							className="min-w-0 flex-1 text-left"
						>
							<div className="flex min-w-0 items-center gap-2 overflow-hidden">
								<div className="h-2 w-2 animate-pulse rounded-full bg-amber-400" />
								<span className="text-sm font-medium text-amber-300">Worker</span>
								<span className={`min-w-0 flex-1 text-sm text-ink-dull ${
									expanded ? "whitespace-normal break-words" : "truncate"
								}`}>{item.task}</span>
								<span className="flex-shrink-0 text-tiny leading-5 text-ink-faint">
									{expanded ? "▾" : "▸"}
								</span>
							</div>
						</button>
						<Link
							to="/agents/$agentId/workers"
							params={{ agentId }}
							search={{ worker: item.id }}
							className="flex-shrink-0 rounded border border-amber-400/30 px-1.5 py-0.5 text-tiny font-medium text-amber-300 transition-colors hover:border-amber-400/60 hover:bg-amber-500/15"
						>
							Open
						</Link>
						<CancelButton onClick={() => { api.cancelProcess(channelId, "worker", item.id).catch(console.warn); }} />
					</div>
				</div>
				{expanded && (
					<div className="mt-1 flex min-w-0 items-center gap-3 overflow-hidden pl-4 text-tiny text-ink-faint">
						<LiveDuration startMs={live.startedAt} />
						<span className="truncate">{live.status}</span>
						{live.currentTool && (
							<span className="truncate text-amber-400/70">{live.currentTool}</span>
						)}
						{live.toolCalls > 0 && (
							<span>{live.toolCalls} tool calls</span>
						)}
					</div>
				)}
			</div>
		</div>
	);
}

function BranchRunItem({ item }: { item: TimelineBranchRun }) {
	const [expanded, setExpanded] = useState(false);

	return (
		<div className="flex gap-3 px-3 py-2">
			<span className="flex-shrink-0 pt-0.5 text-tiny text-ink-faint">
				{formatTimestamp(new Date(item.started_at).getTime())}
			</span>
			<div className="min-w-0 flex-1">
				<Button
					type="button"
					onClick={() => setExpanded(!expanded)}
					variant="ghost"
					className="h-auto w-full justify-start rounded-md bg-violet-500/10 px-3 py-2 text-left hover:bg-violet-500/15"
				>
					<div className="flex min-w-0 items-start gap-2">
						<span className="inline-flex flex-shrink-0 items-center gap-2 self-start">
							<span className="h-2 w-2 rounded-full bg-violet-400/50" />
							<span className="text-sm font-medium text-violet-300">Branch</span>
						</span>
						<span className={`min-w-0 flex-1 text-sm text-ink-dull ${
							expanded ? "whitespace-normal break-words" : "truncate"
						}`}>
							{item.description}
						</span>
						{item.conclusion && (
							<span className="flex-shrink-0 self-start text-tiny leading-5 text-ink-faint">
								{expanded ? "▾" : "▸"}
							</span>
						)}
					</div>
				</Button>
				{expanded && item.conclusion && (
					<div className="mt-1 rounded-md border border-violet-500/10 bg-violet-500/5 px-3 py-2">
						<div className="text-sm text-ink-dull">
							<Markdown className="whitespace-pre-wrap break-words">{item.conclusion}</Markdown>
						</div>
					</div>
				)}
			</div>
		</div>
	);
}

function WorkerRunItem({ item, agentId }: { item: TimelineWorkerRun; agentId: string }) {
	const [expanded, setExpanded] = useState(false);

	return (
		<div className="flex gap-3 px-3 py-2">
			<span className="flex-shrink-0 pt-0.5 text-tiny text-ink-faint">
				{formatTimestamp(new Date(item.started_at).getTime())}
			</span>
			<div className="min-w-0 flex-1">
				<div className="w-full rounded-md bg-amber-500/10 px-3 py-2 transition-colors hover:bg-amber-500/15">
					<div className="flex min-w-0 items-center gap-2 overflow-hidden">
						<button
							type="button"
							onClick={() => {
								if (item.result) setExpanded(!expanded);
							}}
							className="min-w-0 flex-1 text-left"
						>
							<div className="flex min-w-0 items-center gap-2 overflow-hidden">
								<div className="h-2 w-2 rounded-full bg-amber-400/50" />
								<span className="text-sm font-medium text-amber-300">Worker</span>
								<span className={`min-w-0 flex-1 text-sm text-ink-dull ${
									expanded ? "whitespace-normal break-words" : "truncate"
								}`}>{item.task}</span>
								{item.result && (
									<span className="flex-shrink-0 text-tiny leading-5 text-ink-faint">
										{expanded ? "▾" : "▸"}
									</span>
								)}
							</div>
						</button>
						<Link
							to="/agents/$agentId/workers"
							params={{ agentId }}
							search={{ worker: item.id }}
							className="flex-shrink-0 rounded border border-amber-400/30 px-1.5 py-0.5 text-tiny font-medium text-amber-300 transition-colors hover:border-amber-400/60 hover:bg-amber-500/15"
						>
							Open
						</Link>
					</div>
				</div>
				{expanded && item.result && (
					<div className="mt-1 rounded-md border border-amber-500/10 bg-amber-500/5 px-3 py-2">
						<div className="text-sm text-ink-dull">
							<Markdown className="whitespace-pre-wrap break-words">{item.result}</Markdown>
						</div>
					</div>
				)}
			</div>
		</div>
	);
}

function TimelineEntry({ item, liveWorkers, liveBranches, channelId, agentId }: {
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
						item.role === "user" ? "bg-app-darkBox/30" : ""
					}`}
				>
					<span className="flex-shrink-0 pt-0.5 text-tiny text-ink-faint">
						{formatTimestamp(new Date(item.created_at).getTime())}
					</span>
					<div className="min-w-0 flex-1">
						<span className={`text-sm font-medium ${
							item.role === "user" ? "text-accent-faint" : "text-green-400"
						}`}>
							{item.role === "user" ? (item.sender_name ?? "user") : (item.sender_name ?? "bot")}
						</span>
						<div className="mt-0.5 text-sm text-ink-dull">
							<Markdown>{item.content}</Markdown>
						</div>
					</div>
				</div>
			);
		case "branch_run": {
			const live = liveBranches[item.id];
			if (live) return <LiveBranchRunItem item={item} live={live} channelId={channelId} />;
			return <BranchRunItem item={item} />;
		}
		case "worker_run": {
			const live = liveWorkers[item.id];
			if (live) return <LiveWorkerRunItem item={item} live={live} channelId={channelId} agentId={agentId} />;
			return <WorkerRunItem item={item} agentId={agentId} />;
		}
	}
}

export function ChannelDetail({ agentId, channelId, channel, liveState, onLoadMore }: ChannelDetailProps) {
	const timeline = liveState?.timeline ?? [];
	const hasMore = liveState?.hasMore ?? false;
	const loadingMore = liveState?.loadingMore ?? false;
	const isTyping = liveState?.isTyping ?? false;
	const workers = liveState?.workers ?? {};
	const branches = liveState?.branches ?? {};
	const activeWorkerCount = Object.keys(workers).length;
	const activeBranchCount = Object.keys(branches).length;
	const hasActivity = activeWorkerCount > 0 || activeBranchCount > 0;
	const [cortexOpen, setCortexOpen] = useState(true);

	const scrollRef = useRef<HTMLDivElement>(null);
	const sentinelRef = useRef<HTMLDivElement>(null);
	const lastLoadMoreAtRef = useRef(0);

	// Trigger load when the sentinel at the top of the timeline becomes visible
	const handleIntersection = useCallback((entries: IntersectionObserverEntry[]) => {
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
	}, [hasMore, loadingMore, onLoadMore]);

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
				<div className="flex h-12 items-center gap-3 border-b border-app-line/50 bg-app-darkBox/20 px-6">
					<Link
						to="/agents/$agentId/channels"
						params={{ agentId }}
						className="text-tiny text-ink-faint hover:text-ink-dull"
					>
						Channels
					</Link>
					<span className="text-ink-faint/50">/</span>
					<span className="text-sm font-medium text-ink">
						{channel?.display_name ?? channelId}
					</span>
					{channel && (
						<span className={`inline-flex items-center rounded-md px-1.5 py-0.5 text-tiny font-medium ${platformColor(channel.platform)}`}>
							{platformIcon(channel.platform)}
						</span>
					)}

					{/* Right side: activity indicators + typing + cortex toggle */}
					<div className="ml-auto flex items-center gap-3">
						{hasActivity && (
							<div className="flex items-center gap-2">
								{activeWorkerCount > 0 && (
									<div className="flex items-center gap-1.5">
										<div className="h-1.5 w-1.5 animate-pulse rounded-full bg-amber-400" />
										<span className="text-tiny text-amber-300">
											{activeWorkerCount} worker{activeWorkerCount !== 1 ? "s" : ""}
										</span>
									</div>
								)}
								{activeBranchCount > 0 && (
									<div className="flex items-center gap-1.5">
										<div className="h-1.5 w-1.5 animate-pulse rounded-full bg-violet-400" />
										<span className="text-tiny text-violet-300">
											{activeBranchCount} branch{activeBranchCount !== 1 ? "es" : ""}
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
						<div className="flex overflow-hidden rounded-md border border-app-line bg-app-darkBox">
							<Button
								onClick={() => setCortexOpen(!cortexOpen)}
								variant={cortexOpen ? "secondary" : "ghost"}
								size="icon"
								className={cortexOpen ? "bg-app-selected text-ink" : ""}
								title="Toggle cortex chat"
							>
								<HugeiconsIcon icon={IdeaIcon} className="h-4 w-4" />
							</Button>
						</div>
					</div>
				</div>

				{/* Timeline — flex-col-reverse keeps scroll pinned to bottom */}
				<div ref={scrollRef} className="flex flex-1 flex-col-reverse overflow-y-auto">
					<div className="flex flex-col gap-1 p-6">
						{/* Sentinel for infinite scroll — sits above the oldest item */}
						<div ref={sentinelRef} className="h-px" />
						{loadingMore && (
							<div className="flex justify-center py-3">
								<span className="text-tiny text-ink-faint">Loading older messages...</span>
							</div>
						)}
						{!hasMore && timeline.length > 0 && (
							<div className="flex justify-center py-3">
								<span className="text-tiny text-ink-faint/50">Beginning of conversation</span>
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
									<span className="text-sm font-medium text-green-400">bot</span>
									<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-ink-faint" />
									<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-ink-faint [animation-delay:0.2s]" />
									<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-ink-faint [animation-delay:0.4s]" />
								</div>
							</div>
						)}
					</div>
				</div>
			</div>

			{/* Cortex chat panel */}
			<AnimatePresence>
				{cortexOpen && (
					<motion.div
						initial={{ width: 0, opacity: 0 }}
						animate={{ width: 400, opacity: 1 }}
						exit={{ width: 0, opacity: 0 }}
						transition={{ type: "spring", stiffness: 400, damping: 30 }}
						className="flex-shrink-0 overflow-hidden border-l border-app-line/50"
					>
						<div className="h-full w-[400px]">
							<CortexChatPanel
								agentId={agentId}
								channelId={channelId}
								onClose={() => setCortexOpen(false)}
							/>
						</div>
					</motion.div>
				)}
			</AnimatePresence>
		</div>
	);
}
