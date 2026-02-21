import { useMemo, useState } from "react";
import { Link, useMatchRoute } from "@tanstack/react-router";
import { useQuery } from "@tanstack/react-query";
import { motion, AnimatePresence } from "framer-motion";
import { api, BASE_PATH, type ChannelInfo } from "@/api/client";
import type { ChannelLiveState } from "@/hooks/useChannelLiveState";
import { Button, Tooltip, TooltipTrigger, TooltipContent, TooltipProvider } from "@/ui";
import { ArrowLeft01Icon, DashboardSquare01Icon, LeftToRightListBulletIcon, Settings01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { CreateAgentDialog } from "@/components/CreateAgentDialog";

function StatusDot({ activity, size = "sm" }: { activity?: { workers: number; branches: number }; size?: "sm" | "md" }) {
	const s = size === "sm" ? "h-1.5 w-1.5" : "h-2 w-2";
	if (activity && activity.workers > 0) return <span className={`${s} shrink-0 rounded-full bg-warning animate-pulse`} />;
	if (activity && activity.branches > 0) return <span className={`${s} shrink-0 rounded-full bg-accent animate-pulse`} />;
	return <span className={`${s} shrink-0 rounded-full bg-success/60`} />;
}

interface SidebarProps {
	liveStates: Record<string, ChannelLiveState>;
	collapsed: boolean;
	onToggle: () => void;
}

export function Sidebar({ liveStates, collapsed, onToggle }: SidebarProps) {
	const [createOpen, setCreateOpen] = useState(false);

	const { data: agentsData } = useQuery({
		queryKey: ["agents"],
		queryFn: api.agents,
		refetchInterval: 30_000,
	});

	const { data: channelsData } = useQuery({
		queryKey: ["channels"],
		queryFn: api.channels,
		refetchInterval: 10_000,
	});

	const agents = agentsData?.agents ?? [];
	const channels = channelsData?.channels ?? [];

	const matchRoute = useMatchRoute();
	const isOverview = matchRoute({ to: "/" });
	const isSettings = matchRoute({ to: "/settings" });

	const agentActivity = useMemo(() => {
		const byAgent: Record<string, { workers: number; branches: number }> = {};
		for (const channel of channels) {
			const live = liveStates[channel.id];
			if (!live) continue;
			if (!byAgent[channel.agent_id]) byAgent[channel.agent_id] = { workers: 0, branches: 0 };
			byAgent[channel.agent_id].workers += Object.keys(live.workers).length;
			byAgent[channel.agent_id].branches += Object.keys(live.branches).length;
		}
		return byAgent;
	}, [channels, liveStates]);

	return (
		<motion.nav
			className="flex h-full flex-col overflow-hidden border-r border-sidebar-line bg-sidebar"
			animate={{ width: collapsed ? 56 : 224 }}
			transition={{ type: "spring", stiffness: 500, damping: 35 }}
		>
			{/* Logo + collapse toggle */}
			<div className="flex h-10 items-center border-b border-sidebar-line px-3">
			{collapsed ? (
				<button onClick={onToggle} className="flex h-full w-full items-center justify-center">
					<img src={`${BASE_PATH}/ball.png`} alt="" className="h-6 w-6 transition-transform duration-150 ease-out hover:scale-110 active:scale-95" draggable={false} />
				</button>
			) : (
				<div className="flex flex-1 items-center justify-between">
					<Link to="/" className="flex items-center gap-2">
						<img src={`${BASE_PATH}/ball.png`} alt="" className="h-6 w-6 flex-shrink-0 transition-transform duration-150 ease-out hover:scale-110 active:scale-95" draggable={false} />
						<span className="whitespace-nowrap font-plex text-sm font-semibold text-sidebar-ink">
							OpenOz
						</span>
					</Link>
					<Button
						onClick={onToggle}
						variant="ghost"
						size="icon"
						className="h-6 w-6 text-sidebar-inkFaint transition-transform duration-150 hover:scale-105 hover:bg-sidebar-selected/50 hover:text-sidebar-inkDull active:scale-95"
					>
						<HugeiconsIcon icon={ArrowLeft01Icon} className="h-4 w-4" />
					</Button>
				</div>
			)}
			</div>

		{collapsed ? (
			<TooltipProvider delayDuration={200}>
				<div className="flex flex-1 flex-col items-center gap-1 pt-2">
					<Tooltip>
						<TooltipTrigger asChild>
							<Link
								to="/"
								className={`flex h-8 w-8 items-center justify-center rounded-md transition-colors duration-150 ${
									isOverview ? "bg-sidebar-selected text-sidebar-ink" : "text-sidebar-inkDull hover:bg-sidebar-selected/50"
								}`}
							>
								<HugeiconsIcon icon={DashboardSquare01Icon} className="h-4 w-4" />
							</Link>
						</TooltipTrigger>
						<TooltipContent side="right">Dashboard</TooltipContent>
					</Tooltip>
					<div className="my-1 h-px w-5 bg-sidebar-line" />
					{agents.map((agent) => {
						const isActive = matchRoute({ to: "/agents/$agentId", params: { agentId: agent.id }, fuzzy: true });
						const activity = agentActivity[agent.id];
						return (
							<Tooltip key={agent.id}>
								<TooltipTrigger asChild>
									<Link
										to="/agents/$agentId"
										params={{ agentId: agent.id }}
										className={`relative flex h-8 w-8 items-center justify-center rounded-md text-xs font-medium transition-colors duration-150 ${
											isActive ? "bg-sidebar-selected text-sidebar-ink" : "text-sidebar-inkDull hover:bg-sidebar-selected/50"
										}`}
									>
										{agent.id.charAt(0).toUpperCase()}
										<span className="absolute -right-0.5 -top-0.5">
											<StatusDot activity={activity} size="sm" />
										</span>
									</Link>
								</TooltipTrigger>
								<TooltipContent side="right">{agent.id}</TooltipContent>
							</Tooltip>
						);
					})}
					<button
						onClick={() => setCreateOpen(true)}
						className="flex h-8 w-8 items-center justify-center rounded-md text-lg text-sidebar-inkFaint transition-colors duration-150 hover:bg-accent/10 hover:text-accent"
					>
						+
					</button>
				</div>
				<div className="border-t border-sidebar-line pb-2 pt-2">
					<Tooltip>
						<TooltipTrigger asChild>
							<Link
								to="/settings"
								className={`mx-auto flex h-8 w-8 items-center justify-center rounded-md transition-colors duration-150 ${
									isSettings ? "bg-sidebar-selected text-sidebar-ink" : "text-sidebar-inkDull hover:bg-sidebar-selected/50"
								}`}
							>
								<HugeiconsIcon icon={Settings01Icon} className="h-4 w-4" />
							</Link>
						</TooltipTrigger>
						<TooltipContent side="right">Settings</TooltipContent>
					</Tooltip>
				</div>
			</TooltipProvider>
		) : (
			<>
				<div className="flex flex-col gap-0.5 pt-2">
					<Link
						to="/"
						className={`mx-2 flex items-center gap-2 rounded-md px-2 py-1.5 text-sm transition-colors duration-150 ${
							isOverview
								? "bg-sidebar-selected text-sidebar-ink"
								: "text-sidebar-inkDull hover:bg-sidebar-selected/50"
						}`}
					>
						Dashboard
					</Link>
				</div>

				<div className="flex flex-1 flex-col overflow-y-auto pt-3">
					<span className="px-3 pb-1 text-[11px] font-semibold uppercase tracking-widest text-sidebar-inkFaint">
						Agents
					</span>
					{agents.length === 0 ? (
						<span className="px-3 py-2 text-tiny text-sidebar-inkFaint">
							No agents configured
						</span>
					) : (
						<div className="flex flex-col gap-0.5">
							<AnimatePresence>
								{agents.map((agent) => {
									const activity = agentActivity[agent.id];
									const isActive = matchRoute({ to: "/agents/$agentId", params: { agentId: agent.id }, fuzzy: true });

									return (
										<motion.div
											key={agent.id}
											initial={{ opacity: 0, x: -8 }}
											animate={{ opacity: 1, x: 0 }}
											exit={{ opacity: 0, x: -8 }}
										>
											<Link
												to="/agents/$agentId"
												params={{ agentId: agent.id }}
												className={`relative mx-2 flex items-center gap-2 rounded-md px-2 py-1.5 text-sm transition-colors duration-150 ${
													isActive
														? "bg-sidebar-selected text-sidebar-ink"
														: "text-sidebar-inkDull hover:bg-sidebar-selected/50"
												}`}
											>
												{isActive && (
													<span className="absolute left-0 top-1/2 h-4 w-0.5 -translate-y-1/2 rounded-full bg-accent" />
												)}
												<StatusDot activity={activity} />
												<span className="flex-1 truncate">{agent.id}</span>
												{activity && (activity.workers > 0 || activity.branches > 0) && (
													<div className="flex items-center gap-1">
														{activity.workers > 0 && (
															<span className="rounded bg-warning/15 px-1 py-0.5 text-tiny text-warning">
																{activity.workers}w
															</span>
														)}
														{activity.branches > 0 && (
															<span className="rounded bg-accent/15 px-1 py-0.5 text-tiny text-accent">
																{activity.branches}b
															</span>
														)}
													</div>
												)}
											</Link>
										</motion.div>
									);
								})}
							</AnimatePresence>
						</div>
					)}
					<Button
						variant="outline"
						size="sm"
						onClick={() => setCreateOpen(true)}
						className="mx-2 mt-1 w-auto justify-center bg-accent/10 border border-accent/30 text-accent hover:bg-accent/20 transition-colors duration-150"
					>
						+ New Agent
					</Button>
				</div>

				<div className="mt-auto border-t border-sidebar-line pb-2 pt-2">
					<Link
						to="/settings"
						className={`mx-2 flex items-center gap-2 rounded-md px-2 py-1.5 text-sm transition-colors duration-150 ${
							isSettings
								? "bg-sidebar-selected text-sidebar-ink"
								: "text-sidebar-inkDull hover:bg-sidebar-selected/50"
						}`}
					>
						<HugeiconsIcon icon={Settings01Icon} className="h-4 w-4" />
						Settings
					</Link>
				</div>
			</>
		)}
			<CreateAgentDialog open={createOpen} onOpenChange={setCreateOpen} />
		</motion.nav>
	);
}
