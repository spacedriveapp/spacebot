import {useMemo, useState} from "react";
import {useQuery} from "@tanstack/react-query";
import {api} from "@/api/client";
import {CreateAgentDialog} from "@/components/CreateAgentDialog";
import {TopologyGraph} from "@/components/TopologyGraph";
import type {ChannelLiveState} from "@/hooks/useChannelLiveState";
import {formatUptime} from "@/lib/format";

interface OverviewProps {
	liveStates: Record<string, ChannelLiveState>;
	activeLinks?: Set<string>;
}

export function Overview({liveStates, activeLinks}: OverviewProps) {
	const [createOpen, setCreateOpen] = useState(false);

	const {data: statusData} = useQuery({
		queryKey: ["status"],
		queryFn: api.status,
		refetchInterval: 5000,
	});

	const {data: overviewData, isLoading: overviewLoading} = useQuery({
		queryKey: ["overview"],
		queryFn: api.overview,
		refetchInterval: 10_000,
	});

	const {data: channelsData} = useQuery({
		queryKey: ["channels"],
		queryFn: api.channels,
		refetchInterval: 10000,
	});

	const channels = channelsData?.channels ?? [];
	const agents = overviewData?.agents ?? [];

	// Aggregate live activity across all agents
	const activity = useMemo(() => {
		let workers = 0;
		let branches = 0;
		for (const state of Object.values(liveStates)) {
			workers += Object.keys(state.workers).length;
			branches += Object.keys(state.branches).length;
		}
		return {workers, branches};
	}, [liveStates]);

	const uptime = statusData?.uptime_seconds ?? 0;

	return (
		<div className="flex flex-col h-full">
			{/* Compact status bar */}
			<div className="flex items-center justify-between border-b border-app-line bg-app-darkBox/50 px-6 py-3.5">
				<div className="flex items-center gap-4">
					<div className="flex items-center gap-2">
						<h1 className="font-plex text-sm font-medium text-ink">Spacebot</h1>
						{statusData ? (
							<div className="h-2 w-2 rounded-full bg-green-500" />
						) : (
							<div className="h-2 w-2 rounded-full bg-red-500" />
						)}
					</div>

					<div className="flex items-center gap-4 text-tiny text-ink-faint">
						<span>
							{agents.length} agent{agents.length !== 1 ? "s" : ""}
						</span>
						<span>
							{channels.length} channel{channels.length !== 1 ? "s" : ""}
						</span>
						<span>{formatUptime(uptime)}</span>
					</div>

					{(activity.workers > 0 || activity.branches > 0) && (
						<div className="flex items-center gap-2">
							{activity.workers > 0 && (
								<span className="flex items-center gap-1.5 rounded-full bg-amber-500/10 px-2.5 py-1 text-tiny">
									<span className="h-1.5 w-1.5 animate-pulse rounded-full bg-amber-400" />
									<span className="font-medium text-amber-400">
										{activity.workers}w
									</span>
								</span>
							)}
							{activity.branches > 0 && (
								<span className="flex items-center gap-1.5 rounded-full bg-violet-500/10 px-2.5 py-1 text-tiny">
									<span className="h-1.5 w-1.5 animate-pulse rounded-full bg-violet-400" />
									<span className="font-medium text-violet-400">
										{activity.branches}b
									</span>
								</span>
							)}
						</div>
					)}
				</div>

				<button
					onClick={() => setCreateOpen(true)}
					className="text-tiny text-ink-faint hover:text-ink transition-colors"
				>
					+ New Agent
				</button>
			</div>

			{/* Full-screen topology */}
			<div className="flex-1 overflow-hidden">
				{overviewLoading ? (
					<div className="flex h-full items-center justify-center">
						<div className="flex items-center gap-2 text-ink-dull">
							<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
							Loading...
						</div>
					</div>
				) : agents.length === 0 ? (
					<div className="flex h-full items-center justify-center">
						<div className="text-center">
							<p className="text-sm text-ink-faint">No agents configured</p>
							<button
								onClick={() => setCreateOpen(true)}
								className="mt-3 text-sm text-accent hover:text-accent/80 transition-colors"
							>
								Create your first agent
							</button>
						</div>
					</div>
				) : (
					<TopologyGraph activeEdges={activeLinks} agents={agents} />
				)}
			</div>

			<CreateAgentDialog open={createOpen} onOpenChange={setCreateOpen} />
		</div>
	);
}
