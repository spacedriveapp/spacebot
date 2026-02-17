import { useMemo, useState } from "react";
import { Link } from "@tanstack/react-router";
import { AnimatePresence, motion } from "framer-motion";
import { api, type ChannelInfo, type TimelineWorkerRun } from "@/api/client";
import { LiveDuration } from "@/components/LiveDuration";
import type { ActiveWorker, ChannelLiveState } from "@/hooks/useChannelLiveState";
import { formatTimeAgo, formatTimestamp } from "@/lib/format";
import { Badge, Button } from "@/ui";

interface AgentWorkersProps {
	agentId: string;
	channels: ChannelInfo[];
	liveStates: Record<string, ChannelLiveState>;
}

interface ActiveWorkerView extends ActiveWorker {
	channel: ChannelInfo;
}

interface CompletedWorkerView extends TimelineWorkerRun {
	channel: ChannelInfo;
}

function statusVariant(status: string): "default" | "outline" | "amber" | "red" | "green" {
	const normalized = status.toLowerCase();
	if (normalized.includes("fail") || normalized.includes("error") || normalized.includes("killed")) {
		return "red";
	}
	if (normalized.includes("done") || normalized.includes("complete") || normalized.includes("success")) {
		return "green";
	}
	if (normalized.includes("run") || normalized.includes("start") || normalized.includes("tool")) {
		return "amber";
	}
	if (normalized.includes("wait") || normalized.includes("queue")) {
		return "outline";
	}
	return "default";
}

function summarizeResult(result: string | null): string {
	if (!result) return "No result text captured.";
	const compact = result.replace(/\s+/g, " ").trim();
	if (compact.length <= 220) return compact;
	return `${compact.slice(0, 220)}...`;
}

export function AgentWorkers({ agentId, channels, liveStates }: AgentWorkersProps) {
	const [cancellingIds, setCancellingIds] = useState<Record<string, boolean>>({});

	const agentChannels = useMemo(
		() => channels.filter((channel) => channel.agent_id === agentId),
		[agentId, channels],
	);

	const activeWorkers = useMemo<ActiveWorkerView[]>(() => {
		return agentChannels
			.flatMap((channel) =>
				Object.values(liveStates[channel.id]?.workers ?? {}).map((worker) => ({
					...worker,
					channel,
				})),
			)
			.sort((a, b) => a.startedAt - b.startedAt);
	}, [agentChannels, liveStates]);

	const completedWorkers = useMemo<CompletedWorkerView[]>(() => {
		return agentChannels
			.flatMap((channel) => {
				const timeline = liveStates[channel.id]?.timeline ?? [];
				return timeline
					.filter((item): item is TimelineWorkerRun => item.type === "worker_run")
					.filter((item) => Boolean(item.completed_at || item.status === "done" || item.result))
					.map((item) => ({ ...item, channel }));
			})
			.sort((a, b) => {
				const aTime = new Date(a.completed_at ?? a.started_at).getTime();
				const bTime = new Date(b.completed_at ?? b.started_at).getTime();
				return bTime - aTime;
			})
			.slice(0, 24);
	}, [agentChannels, liveStates]);

	const activeChannelCount = useMemo(
		() => new Set(activeWorkers.map((worker) => worker.channel.id)).size,
		[activeWorkers],
	);
	const workersUsingTools = useMemo(
		() => activeWorkers.filter((worker) => worker.currentTool !== null).length,
		[activeWorkers],
	);
	const totalToolCalls = useMemo(
		() => activeWorkers.reduce((sum, worker) => sum + worker.toolCalls, 0),
		[activeWorkers],
	);

	const handleCancel = (worker: ActiveWorkerView) => {
		setCancellingIds((previous) => ({ ...previous, [worker.id]: true }));
		api.cancelProcess(worker.channel.id, "worker", worker.id)
			.catch((error) => {
				console.warn("Failed to cancel worker:", error);
			})
			.finally(() => {
				setCancellingIds((previous) => {
					const { [worker.id]: _, ...remaining } = previous;
					return remaining;
				});
			});
	};

	return (
		<div className="flex h-full flex-col overflow-hidden">
			<div className="border-b border-app-line/50 bg-app-darkBox/30 px-6 py-4">
				<div className="flex flex-wrap items-center justify-between gap-3">
					<div>
						<h2 className="font-plex text-base font-semibold tracking-tight text-ink">Workers Live Panel</h2>
						<p className="mt-1 text-sm text-ink-faint">
							Monitor every active worker, current tool usage, and recent outputs across this agent.
						</p>
					</div>
					<div className="grid min-w-[290px] flex-1 grid-cols-3 gap-2 sm:max-w-[420px]">
						<div className="rounded-lg border border-app-line/60 bg-app-darkBox/80 px-3 py-2 text-center">
							<div className="text-xs text-ink-faint">Active</div>
							<div className="mt-0.5 font-mono text-sm font-semibold text-amber-300">{activeWorkers.length}</div>
						</div>
						<div className="rounded-lg border border-app-line/60 bg-app-darkBox/80 px-3 py-2 text-center">
							<div className="text-xs text-ink-faint">Channels</div>
							<div className="mt-0.5 font-mono text-sm font-semibold text-violet-300">{activeChannelCount}</div>
						</div>
						<div className="rounded-lg border border-app-line/60 bg-app-darkBox/80 px-3 py-2 text-center">
							<div className="text-xs text-ink-faint">Tool Calls</div>
							<div className="mt-0.5 font-mono text-sm font-semibold text-cyan-300">{totalToolCalls}</div>
						</div>
					</div>
				</div>
			</div>

			<div className="flex-1 overflow-y-auto p-6">
				<div className="grid gap-4 xl:grid-cols-[minmax(0,1.6fr)_minmax(320px,1fr)]">
					<section className="space-y-3">
						<div className="flex items-center justify-between">
							<h3 className="font-plex text-sm font-medium text-ink">Active Workers</h3>
							<Badge variant={workersUsingTools > 0 ? "amber" : "outline"} size="sm">
								{workersUsingTools} using tools
							</Badge>
						</div>

						{activeWorkers.length === 0 ? (
							<div className="rounded-xl border border-dashed border-app-line/70 bg-app-darkBox/25 px-5 py-8">
								<p className="text-sm text-ink-faint">No workers are active right now.</p>
								<p className="mt-1 text-tiny text-ink-faint/80">
									Spawn a worker from a channel and it will appear here with live status updates.
								</p>
							</div>
						) : (
							<div className="grid grid-cols-1 gap-3 lg:grid-cols-2">
								<AnimatePresence initial={false}>
									{activeWorkers.map((worker) => (
										<motion.div
											key={worker.id}
											layout
											initial={{ opacity: 0, y: 8 }}
											animate={{ opacity: 1, y: 0 }}
											exit={{ opacity: 0, y: -8 }}
											transition={{ type: "spring", stiffness: 420, damping: 35 }}
											className="relative overflow-hidden rounded-xl border border-amber-500/25 bg-amber-500/5 p-4"
										>
											<div className="absolute right-0 top-0 h-20 w-20 rounded-full bg-amber-400/15 blur-2xl" />
											<div className="relative">
												<div className="mb-3 flex items-start gap-3">
													<div className="mt-1 h-2.5 w-2.5 flex-shrink-0 animate-pulse rounded-full bg-amber-400" />
													<div className="min-w-0 flex-1">
														<div className="text-sm font-medium text-ink">{worker.task}</div>
														<div className="mt-1 font-mono text-tiny text-ink-faint">{worker.id}</div>
													</div>
													<Button
														type="button"
														size="sm"
														variant="ghost"
														disabled={Boolean(cancellingIds[worker.id])}
														onClick={() => handleCancel(worker)}
														className="text-red-300 hover:bg-red-500/15 hover:text-red-200"
													>
														{cancellingIds[worker.id] ? "Cancelling..." : "Cancel"}
													</Button>
												</div>

												<div className="flex flex-wrap items-center gap-2">
													<Badge variant={statusVariant(worker.status)} size="sm">{worker.status}</Badge>
													<Badge variant="outline" size="sm">
														<LiveDuration startMs={worker.startedAt} />
													</Badge>
													<Badge variant="outline" size="sm">{worker.toolCalls} tool calls</Badge>
												</div>

												<div className="mt-3 rounded-md border border-app-line/70 bg-app-darkBox/60 px-3 py-2">
													<div className="text-tiny uppercase tracking-wide text-ink-faint/80">Current Tool</div>
													<div className="mt-1 flex items-center gap-2 text-sm text-ink-dull">
														<span className={`h-1.5 w-1.5 rounded-full ${worker.currentTool ? "animate-pulse bg-amber-400" : "bg-ink-faint/50"}`} />
														<span className="truncate">
															{worker.currentTool ?? "Waiting for next tool call"}
														</span>
													</div>
												</div>

												<div className="mt-3 flex items-center justify-between text-tiny text-ink-faint">
													<Link
														to="/agents/$agentId/channels/$channelId"
														params={{ agentId, channelId: worker.channel.id }}
														className="truncate rounded px-1 py-0.5 hover:bg-app-hover/50 hover:text-ink"
													>
														{worker.channel.display_name ?? worker.channel.id}
													</Link>
													<span>{formatTimestamp(worker.startedAt)}</span>
												</div>
											</div>
										</motion.div>
									))}
								</AnimatePresence>
							</div>
						)}
					</section>

					<section className="space-y-3">
						<div className="rounded-xl border border-app-line/70 bg-app-darkBox/40 p-4">
							<h3 className="font-plex text-sm font-medium text-ink">Live Telemetry</h3>
							<p className="mt-1 text-tiny text-ink-faint">
								Active workers currently inside a tool call.
							</p>
							<div className="mt-3 space-y-2">
								{activeWorkers.filter((worker) => worker.currentTool).length === 0 ? (
									<div className="rounded-md border border-dashed border-app-line/70 bg-app-darkBox/20 px-3 py-2 text-tiny text-ink-faint">
										No tool calls in-flight.
									</div>
								) : (
									activeWorkers
										.filter((worker) => worker.currentTool)
										.map((worker) => (
											<div
												key={`${worker.id}-${worker.currentTool}`}
												className="rounded-md border border-amber-500/20 bg-amber-500/10 px-3 py-2"
											>
												<div className="flex items-center justify-between gap-2">
													<div className="truncate text-sm text-amber-200">{worker.currentTool}</div>
													<div className="text-tiny text-amber-300/80">
														<LiveDuration startMs={worker.startedAt} />
													</div>
												</div>
												<div className="mt-1 truncate text-tiny text-ink-faint">
													{worker.task}
												</div>
											</div>
										))
								)}
							</div>
						</div>

						<div className="rounded-xl border border-app-line/70 bg-app-darkBox/40 p-4">
							<h3 className="font-plex text-sm font-medium text-ink">Recent Worker Results</h3>
							<p className="mt-1 text-tiny text-ink-faint">
								Most recent worker completions from loaded channel history.
							</p>
							<div className="mt-3 space-y-2">
								{completedWorkers.length === 0 ? (
									<div className="rounded-md border border-dashed border-app-line/70 bg-app-darkBox/20 px-3 py-2 text-tiny text-ink-faint">
										No recent worker completions yet.
									</div>
								) : (
									completedWorkers.map((workerRun) => (
										<div key={`${workerRun.channel.id}-${workerRun.id}`} className="rounded-md border border-app-line/70 bg-app-darkBox/60 px-3 py-2">
											<div className="flex items-center gap-2">
												<Badge variant={statusVariant(workerRun.status)} size="sm">{workerRun.status}</Badge>
												<span className="truncate text-tiny text-ink-faint">
													{workerRun.channel.display_name ?? workerRun.channel.id}
												</span>
												<span className="ml-auto text-tiny text-ink-faint/80">
													{formatTimeAgo(workerRun.completed_at ?? workerRun.started_at)}
												</span>
											</div>
											<div className="mt-1 text-sm text-ink">{workerRun.task}</div>
											<div className="mt-1 text-tiny text-ink-dull [display:-webkit-box] [-webkit-box-orient:vertical] [-webkit-line-clamp:3] overflow-hidden">
												{summarizeResult(workerRun.result)}
											</div>
										</div>
									))
								)}
							</div>
						</div>
					</section>
				</div>
			</div>
		</div>
	);
}
