import { useMemo } from "react";
import { Link, useMatchRoute } from "@tanstack/react-router";
import { useQuery } from "@tanstack/react-query";
import { motion } from "framer-motion";
import { api, BASE_PATH, type ChannelInfo } from "@/api/client";
import type { ChannelLiveState } from "@/hooks/useChannelLiveState";
import { Button } from "@/ui";
import { ArrowLeft01Icon, DashboardSquare01Icon, LeftToRightListBulletIcon, Settings01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";

interface SidebarProps {
	liveStates: Record<string, ChannelLiveState>;
	collapsed: boolean;
	onToggle: () => void;
}

export function Sidebar({ liveStates, collapsed, onToggle }: SidebarProps) {
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
			<div className="flex h-12 items-center border-b border-sidebar-line px-3">
			{collapsed ? (
				<Button onClick={onToggle} variant="ghost" size="icon" className="h-full w-full">
					<img src={`${BASE_PATH}/ball.png`} alt="" className="h-6 w-6" draggable={false} />
				</Button>
			) : (
				<div className="flex flex-1 items-center justify-between">
					<Link to="/" className="flex items-center gap-2">
						<img src={`${BASE_PATH}/ball.png`} alt="" className="h-6 w-6 flex-shrink-0" draggable={false} />
						<span className="whitespace-nowrap font-plex text-sm font-semibold text-sidebar-ink">
							Spacebot
						</span>
					</Link>
					<Button
						onClick={onToggle}
						variant="ghost"
						size="icon"
						className="h-6 w-6 text-sidebar-inkFaint hover:bg-sidebar-selected/50 hover:text-sidebar-inkDull"
					>
						<HugeiconsIcon icon={ArrowLeft01Icon} className="h-4 w-4" />
					</Button>
				</div>
			)}
			</div>

			{/* Collapsed: icon-only nav */}
			{collapsed ? (
				<div className="flex flex-col items-center gap-1 pt-2">
				<Link
					to="/"
					className={`flex h-8 w-8 items-center justify-center rounded-md ${
						isOverview ? "bg-sidebar-selected text-sidebar-ink" : "text-sidebar-inkDull hover:bg-sidebar-selected/50"
					}`}
					title="Dashboard"
				>
					<HugeiconsIcon icon={DashboardSquare01Icon} className="h-4 w-4" />
				</Link>
				<Link
					to="/settings"
					className={`flex h-8 w-8 items-center justify-center rounded-md ${
						isSettings ? "bg-sidebar-selected text-sidebar-ink" : "text-sidebar-inkDull hover:bg-sidebar-selected/50"
					}`}
					title="Settings"
				>
					<HugeiconsIcon icon={Settings01Icon} className="h-4 w-4" />
				</Link>
				<div className="my-1 h-px w-5 bg-sidebar-line" />
					{agents.map((agent) => {
						const isActive = matchRoute({ to: "/agents/$agentId", params: { agentId: agent.id }, fuzzy: true });
						return (
							<Link
								key={agent.id}
								to="/agents/$agentId"
								params={{ agentId: agent.id }}
								className={`flex h-8 w-8 items-center justify-center rounded-md text-xs font-medium ${
									isActive ? "bg-sidebar-selected text-sidebar-ink" : "text-sidebar-inkDull hover:bg-sidebar-selected/50"
								}`}
								title={agent.id}
							>
								{agent.id.charAt(0).toUpperCase()}
							</Link>
						);
					})}
				</div>
			) : (
				<>
					{/* Top-level nav */}
					<div className="flex flex-col gap-0.5 pt-2">
						<Link
							to="/"
							className={`mx-2 flex items-center gap-2 rounded-md px-2 py-1.5 text-sm ${
								isOverview
									? "bg-sidebar-selected text-sidebar-ink"
									: "text-sidebar-inkDull hover:bg-sidebar-selected/50"
							}`}
						>
							Dashboard
						</Link>
					<Link
						to="/settings"
						className={`mx-2 flex items-center gap-2 rounded-md px-2 py-1.5 text-sm ${
							isSettings
								? "bg-sidebar-selected text-sidebar-ink"
								: "text-sidebar-inkDull hover:bg-sidebar-selected/50"
						}`}
					>
						Settings
					</Link>
				</div>

					{/* Agents */}
					<div className="flex flex-1 flex-col overflow-y-auto pt-3">
						<span className="px-3 pb-1 text-tiny font-medium uppercase tracking-wider text-sidebar-inkFaint">
							Agents
						</span>
						{agents.length === 0 ? (
							<span className="px-3 py-2 text-tiny text-sidebar-inkFaint">
								No agents configured
							</span>
						) : (
							<div className="flex flex-col gap-0.5">
								{agents.map((agent) => {
									const activity = agentActivity[agent.id];
									const isActive = matchRoute({ to: "/agents/$agentId", params: { agentId: agent.id }, fuzzy: true });

									return (
										<Link
											key={agent.id}
											to="/agents/$agentId"
											params={{ agentId: agent.id }}
											className={`mx-2 flex items-center gap-2 rounded-md px-2 py-1.5 text-sm ${
												isActive
													? "bg-sidebar-selected text-sidebar-ink"
													: "text-sidebar-inkDull hover:bg-sidebar-selected/50"
											}`}
										>
											<span className="flex-1 truncate">{agent.id}</span>
											{activity && (activity.workers > 0 || activity.branches > 0) && (
												<div className="flex items-center gap-1">
													{activity.workers > 0 && (
														<span className="rounded bg-amber-500/15 px-1 py-0.5 text-tiny text-amber-400">
															{activity.workers}w
														</span>
													)}
													{activity.branches > 0 && (
														<span className="rounded bg-violet-500/15 px-1 py-0.5 text-tiny text-violet-400">
															{activity.branches}b
														</span>
													)}
												</div>
											)}
										</Link>
									);
								})}
							</div>
						)}
						<Button
							variant="outline"
							size="sm"
							className="mx-2 mt-1 w-auto justify-center border-dashed border-sidebar-line text-sidebar-inkFaint hover:border-sidebar-inkFaint hover:text-sidebar-inkDull"
						>
							+ New Agent
						</Button>
					</div>
				</>
			)}
		</motion.nav>
	);
}
