import {useMemo, useState} from "react";
import {useQuery} from "@tanstack/react-query";
import {Link} from "@tanstack/react-router";
import {SparklesIcon, IdeaIcon} from "@hugeicons/core-free-icons";
import {HugeiconsIcon} from "@hugeicons/react";
import {api} from "@/api/client";
import {CreateAgentDialog} from "@/components/CreateAgentDialog";
import {TopologyGraph} from "@/components/TopologyGraph";
import {UpdatePill} from "@/components/UpdatePill";
import {useSetTopBar} from "@/components/TopBar";
import type {ChannelLiveState} from "@/hooks/useChannelLiveState";
import {useIsMobile} from "@/hooks/useViewport";
import {formatUptime} from "@/lib/format";

interface OverviewProps {
	liveStates: Record<string, ChannelLiveState>;
	activeLinks?: Set<string>;
}

export function Overview({liveStates, activeLinks}: OverviewProps) {
	const [createOpen, setCreateOpen] = useState(false);
	const isMobile = useIsMobile();

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

	const {data: providersData} = useQuery({
		queryKey: ["providers"],
		queryFn: api.providers,
		staleTime: 10_000,
	});

	const channels = channelsData?.channels ?? [];
	const agents = overviewData?.agents ?? [];

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

	useSetTopBar(
		<div className="flex h-full min-w-0 flex-1 items-center justify-between gap-2 px-3 sm:px-6">
			<div className="flex min-w-0 items-center gap-3 sm:gap-4">
				<div className="flex items-center gap-2">
					<h1 className="font-plex text-sm font-medium text-ink">Spacebot</h1>
					{statusData ? (
						<div className="h-2 w-2 rounded-full bg-green-500" />
					) : (
						<div className="h-2 w-2 rounded-full bg-red-500" />
					)}
				</div>

				<div className="hidden items-center gap-4 text-tiny text-ink-faint sm:flex">
					<span>{agents.length} agent{agents.length !== 1 ? "s" : ""}</span>
					<span>{channels.length} channel{channels.length !== 1 ? "s" : ""}</span>
					<span>{formatUptime(uptime)}</span>
				</div>

				{(activity.workers > 0 || activity.branches > 0) && (
					<div className="hidden items-center gap-2 sm:flex">
						{activity.workers > 0 && (
							<span className="flex items-center gap-1.5 rounded-full bg-amber-500/10 px-2.5 py-1 text-tiny">
								<span className="h-1.5 w-1.5 animate-pulse rounded-full bg-amber-400" />
								<span className="font-medium text-amber-400">{activity.workers}w</span>
							</span>
						)}
						{activity.branches > 0 && (
							<span className="flex items-center gap-1.5 rounded-full bg-violet-500/10 px-2.5 py-1 text-tiny">
								<span className="h-1.5 w-1.5 animate-pulse rounded-full bg-violet-400" />
								<span className="font-medium text-violet-400">{activity.branches}b</span>
							</span>
						)}
					</div>
				)}
			</div>

			<div className="flex shrink-0 items-center gap-3">
				<UpdatePill />
				{providersData?.has_any && (
					<button
						onClick={() => setCreateOpen(true)}
						className="hidden text-tiny text-ink-faint transition-colors hover:text-ink sm:block"
					>
						+ New Agent
					</button>
				)}
			</div>
		</div>,
	);

	return (
		<div className="flex h-full flex-col">
			<div className="flex-1 overflow-hidden">
				{overviewLoading ? (
					<div className="flex h-full items-center justify-center">
						<div className="flex items-center gap-2 text-ink-dull">
							<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
							Loading...
						</div>
					</div>
				) : agents.length === 0 ? (
					<div className="flex h-full items-center justify-center p-4">
						{providersData && !providersData.has_any ? (
							<div className="flex max-w-md flex-col items-center gap-6 text-center">
								<div className="flex flex-col items-center gap-4">
									<h2 className="text-lg font-medium text-ink">Welcome to Spacebot</h2>
									<p className="text-sm leading-relaxed text-ink-faint">
										An agentic AI system where every process has a dedicated role. To get started, connect an LLM provider.
									</p>
									<div className="mt-1 flex w-full flex-col items-center gap-3 sm:flex-row">
										<Link
											to="/settings"
											search={{tab: "providers"}}
											className="inline-flex items-center gap-2 rounded-lg bg-accent px-4 py-2 text-sm font-medium text-white shadow transition-colors hover:bg-accent/90"
										>
											<HugeiconsIcon icon={SparklesIcon} className="h-4 w-4" />
											Add LLM Provider
										</Link>
										<a
											href="https://docs.spacebot.sh"
											target="_blank"
											rel="noopener noreferrer"
											className="inline-flex items-center rounded-lg border border-app-line px-4 py-2 text-sm font-medium text-ink-dull transition-colors hover:bg-app-hover/40 hover:text-ink"
										>
											Read the Docs
										</a>
									</div>
								</div>
								<div className="flex items-start gap-3 rounded-lg border border-app-line bg-app-darkBox/50 px-4 py-3 text-left">
									<HugeiconsIcon icon={IdeaIcon} className="mt-0.5 h-4 w-4 shrink-0 text-accent" />
									<p className="text-tiny leading-relaxed text-ink-faint">
										<span className="font-medium text-ink-dull">Pro tip:</span>{" "}
										Once set up, you can talk to the Cortex from any agent page to get help with configuration, debugging, or learning how Spacebot works.
									</p>
								</div>
							</div>
						) : (
							<div className="text-center">
								<p className="text-sm text-ink-faint">No agents configured</p>
								<button
									onClick={() => setCreateOpen(true)}
									className="mt-3 text-sm text-accent transition-colors hover:text-accent/80"
								>
									Create your first agent
								</button>
							</div>
						)}
					</div>
				) : isMobile ? (
					<div className="h-full overflow-y-auto p-4">
						<div className="mb-3 rounded-lg border border-app-line bg-app-darkBox/20 px-3 py-2 text-xs text-ink-faint">
							Topology view is optimized for larger screens. Use this quick list on mobile.
						</div>
						<div className="grid gap-2">
							{agents.map((agent) => (
								<Link
									key={agent.id}
									to="/agents/$agentId"
									params={{agentId: agent.id}}
									className="rounded-lg border border-app-line bg-app-darkBox/20 px-3 py-2 text-sm text-ink-dull transition-colors hover:bg-app-hover"
								>
									<div className="truncate font-medium text-ink">{agent.display_name || agent.id}</div>
									<div className="truncate text-tiny text-ink-faint">{agent.id}</div>
								</Link>
							))}
						</div>
					</div>
				) : (
					<TopologyGraph activeEdges={activeLinks} agents={agents} />
				)}
			</div>

			{agents[0] && (
				<CreateAgentDialog open={createOpen} onOpenChange={setCreateOpen} agentId={agents[0].id} />
			)}
		</div>
	);
}
