import {useMemo, useState} from "react";
import {useQuery, useMutation, useQueryClient} from "@tanstack/react-query";
import {Link} from "@tanstack/react-router";
import {Sparkle, Lightbulb, FolderSimplePlus} from "@phosphor-icons/react";
import {api} from "@/api/client";
import {CreateAgentDialog} from "@/components/CreateAgentDialog";
import {OrgGraph} from "@/components/org";
import {Button, CircleButton} from "@spacedrive/primitives";
import {UpdatePill} from "@/components/UpdatePill";
import type {ChannelLiveState} from "@/hooks/useChannelLiveState";
import {formatUptime} from "@/lib/format";

interface OverviewProps {
	liveStates: Record<string, ChannelLiveState>;
	activeLinks?: Set<string>;
}

export function Overview({liveStates, activeLinks}: OverviewProps) {
	const [createOpen, setCreateOpen] = useState(false);
	const queryClient = useQueryClient();

	const createHuman = useMutation({
		mutationFn: (id: string) => api.createHuman({id}),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["topology"]});
		},
	});

	const {data: topologyData} = useQuery({
		queryKey: ["topology"],
		queryFn: api.topology,
		staleTime: 10_000,
	});

	const createGroup = useMutation({
		mutationFn: (name: string) => api.createGroup({name, agent_ids: []}),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["topology"]});
		},
	});

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
			<div className="flex h-12 shrink-0 items-center justify-between border-b border-app-line pl-6 pr-3">
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

				<div className="flex items-center gap-3">
					<CircleButton
						icon={FolderSimplePlus}
						title="New Group"
						onClick={() =>
							createGroup.mutate(
								`Group ${(topologyData?.groups?.length ?? 0) + 1}`,
							)
						}
					/>
					<UpdatePill />
					{providersData?.has_any && (
						<Button
							variant="gray"
							size="md"
							onClick={() => setCreateOpen(true)}
						>
							New Agent
						</Button>
					)}
					<Button
						variant="gray"
						size="md"
						onClick={() => createHuman.mutate(`human-${Date.now()}`)}
					>
						New Human
					</Button>
				</div>
			</div>
			{providersData && !providersData.has_any && agents.length > 0 && (
				<div className="mx-6 mt-4 flex items-center justify-between gap-3 rounded-lg border border-amber-500/25 bg-amber-500/10 px-4 py-3">
					<p className="text-sm text-amber-200">
						Agents are configured, but no provider credentials are available.
						Add or unlock secrets to bring agents online.
					</p>
					<Link
						to="/settings"
						search={{tab: "secrets"}}
						className="shrink-0 text-sm font-medium text-amber-100 underline-offset-4 hover:underline"
					>
						Open Secrets Settings
					</Link>
				</div>
			)}

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
						{providersData && !providersData.has_any ? (
							<div className="flex flex-col items-center gap-6 max-w-md text-center">
								<div className="flex flex-col items-center gap-4">
									<h2 className="text-lg font-medium text-ink">
										Welcome to Spacebot
									</h2>
									<p className="text-sm text-ink-faint leading-relaxed">
										An agentic AI system where every process has a dedicated
										role. To get started, connect an LLM provider.
									</p>
									<div className="mt-1 flex items-center gap-3">
										<Link
											to="/settings"
											search={{tab: "providers"}}
											className="inline-flex items-center gap-2 rounded-lg bg-accent px-4 py-2 text-sm font-medium text-white shadow hover:bg-accent/90 transition-colors"
										>
											<Sparkle className="h-4 w-4" />
											Add LLM Provider
										</Link>
										<a
											href="https://docs.spacebot.sh"
											target="_blank"
											rel="noopener noreferrer"
											className="inline-flex items-center rounded-lg border border-app-line px-4 py-2 text-sm font-medium text-ink-dull hover:bg-app-hover/40 hover:text-ink transition-colors"
										>
											Read the Docs
										</a>
									</div>
								</div>
								<div className="flex items-start gap-3 rounded-lg border border-app-line bg-app-dark-box/50 px-4 py-3 text-left">
									<Lightbulb className="mt-0.5 h-4 w-4 shrink-0 text-accent" />
									<p className="text-tiny text-ink-faint leading-relaxed">
										<span className="font-medium text-ink-dull">Pro tip:</span>{" "}
										Once set up, you can talk to the Cortex from any agent page
										to get help with configuration, debugging, or learning how
										Spacebot works.
									</p>
								</div>
							</div>
						) : (
							<div className="text-center">
								<p className="text-sm text-ink-faint">No agents configured</p>
								<button
									onClick={() => setCreateOpen(true)}
									className="mt-3 text-sm text-accent hover:text-accent/80 transition-colors"
								>
									Create your first agent
								</button>
							</div>
						)}
					</div>
				) : (
					<OrgGraph activeEdges={activeLinks} agents={agents} />
				)}
			</div>

			{agents[0] && (
				<CreateAgentDialog
					open={createOpen}
					onOpenChange={setCreateOpen}
					agentId={agents[0].id}
				/>
			)}
		</div>
	);
}
