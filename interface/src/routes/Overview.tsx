import { useMemo, useRef, useState, useEffect, useCallback, type ComponentType, type ForwardRefExoticComponent } from "react";
import { BotIcon } from "@/components/icons/bot";
import { BrainIcon } from "@/components/icons/brain";
import { CpuIcon } from "@/components/icons/cpu";
import { ZapIcon } from "@/components/icons/zap";
import { ActivityIcon } from "@/components/icons/activity";
import { AtomIcon } from "@/components/icons/atom";
import { SparklesIcon } from "@/components/icons/sparkles";
import { RocketIcon } from "@/components/icons/rocket";
import { FlameIcon } from "@/components/icons/flame";
import { Link } from "@tanstack/react-router";
import { useQuery } from "@tanstack/react-query";
import { motion, AnimatePresence } from "framer-motion";
import { api, type AgentSummary, type CortexEvent } from "@/api/client";
import { CreateAgentDialog } from "@/components/CreateAgentDialog";
import type { ChannelLiveState } from "@/hooks/useChannelLiveState";
import { formatTimeAgo, formatUptime } from "@/lib/format";
import { staggerContainer, cardEntrance } from "@/lib/motion";
import { SkeletonCard } from "@/ui/Skeleton";
import { FilterButton } from "@/ui/FilterButton";

interface OverviewProps {
	liveStates: Record<string, ChannelLiveState>;
}

export function Overview({ liveStates }: OverviewProps) {
	const [createOpen, setCreateOpen] = useState(false);
	const [hoveredAgent, setHoveredAgent] = useState<string | null>(null);
	const [activityFilter, setActivityFilter] = useState<string>("all");

	const { data: statusData } = useQuery({
		queryKey: ["status"],
		queryFn: api.status,
		refetchInterval: 5000,
	});

	const { data: overviewData, isLoading: overviewLoading } = useQuery({
		queryKey: ["overview"],
		queryFn: api.overview,
		refetchInterval: 10_000,
	});

	const { data: channelsData } = useQuery({
		queryKey: ["channels"],
		queryFn: api.channels,
		refetchInterval: 10000,
	});

	const channels = channelsData?.channels ?? [];
	const agents = overviewData?.agents ?? [];

	const activity = useMemo(() => {
		let workers = 0;
		let branches = 0;
		let typing = 0;
		for (const state of Object.values(liveStates)) {
			workers += Object.keys(state.workers).length;
			branches += Object.keys(state.branches).length;
			if (state.isTyping) typing++;
		}
		return { workers, branches, typing };
	}, [liveStates]);

	const getAgentActivity = (agentId: string) => {
		let workers = 0;
		let branches = 0;
		for (const channel of channels) {
			if (channel.agent_id !== agentId) continue;
			const live = liveStates[channel.id];
			if (!live) continue;
			workers += Object.keys(live.workers).length;
			branches += Object.keys(live.branches).length;
		}
		return { workers, branches, hasActivity: workers > 0 || branches > 0 };
	};

	const allEvents = useQuery({
		queryKey: ["global-cortex-events", agents.map((a) => a.id).join(",")],
		queryFn: async () => {
			if (agents.length === 0) return [];
			const results = await Promise.all(
				agents.map((a) =>
					api.cortexEvents(a.id, { limit: 8 }).then((r) =>
						r.events.map((e) => ({ ...e, agentId: a.id, agentName: a.profile?.display_name ?? a.id }))
					)
				)
			);
			return results
				.flat()
				.sort((a, b) => new Date(b.created_at).getTime() - new Date(a.created_at).getTime())
				.slice(0, 20);
		},
		enabled: agents.length > 0,
		refetchInterval: 15_000,
	});

	const totalMemories = agents.reduce((sum, a) => sum + a.memory_total, 0);
	const activeAgents = agents.filter((a) => {
		const act = getAgentActivity(a.id);
		return act.hasActivity || (a.last_activity_at && new Date(a.last_activity_at).getTime() > Date.now() - 5 * 60 * 1000);
	}).length;

	return (
		<div className="flex flex-col h-full">
			<HeroSection
				status={statusData}
				totalChannels={channels.length}
				totalAgents={agents.length}
				activity={activity}
			/>

			<main className="relative flex-1 min-h-0 overflow-hidden px-8 py-6">
				{overviewLoading ? (
					<div className="grid grid-cols-3 gap-5">
						{[1, 2, 3].map((i) => <SkeletonCard key={i} className="h-48" />)}
					</div>
				) : agents.length === 0 ? (
					<div className="flex h-full items-center justify-center">
						<div className="rounded-lg border border-dashed border-app-line p-8 text-center">
							<p className="text-[14px] text-ink-faint">No agents configured.</p>
							<button
								onClick={() => setCreateOpen(true)}
								className="mt-3 text-[14px] text-accent hover:text-accent/80 transition-colors"
							>
								Create your first agent
							</button>
							<CreateAgentDialog open={createOpen} onOpenChange={setCreateOpen} />
						</div>
					</div>
				) : (
					<div className="flex flex-col h-full gap-6">
						<section>
							<SectionHeader title="Agents" right={
								<button onClick={() => setCreateOpen(true)} className="text-[12px] text-ink-faint/60 hover:text-accent transition-colors">+ New</button>
							} />
							<motion.div
								className="grid grid-cols-1 gap-5 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4"
								variants={staggerContainer}
								initial="initial"
								animate="animate"
							>
								{agents.map((agent) => (
									<AgentCard
										key={agent.id}
										agent={agent}
										liveActivity={getAgentActivity(agent.id)}
										isHovered={hoveredAgent === agent.id}
										isDimmed={hoveredAgent !== null && hoveredAgent !== agent.id}
										onHover={setHoveredAgent}
									/>
								))}
							</motion.div>
							<CreateAgentDialog open={createOpen} onOpenChange={setCreateOpen} />
						</section>

					<StatsPills
						uptime={statusData?.uptime_seconds ?? 0}
						totalAgents={agents.length}
						activeAgents={activeAgents}
						totalChannels={channels.length}
						totalMemories={totalMemories}
						activity={activity}
					/>

					<section className="flex flex-col flex-1 min-h-0">
						<SectionHeader title="Activity" right={
							<span className="text-[12px] tabular-nums text-ink-faint/50">{allEvents.data?.length ?? 0} events</span>
						} />
						<div className="flex items-center gap-1.5 mb-2">
							<FilterButton active={activityFilter === "all"} onClick={() => setActivityFilter("all")}>All</FilterButton>
							<FilterButton active={activityFilter === "errors"} onClick={() => setActivityFilter("errors")}>Errors</FilterButton>
							{agents.map(a => (
								<FilterButton key={a.id} active={activityFilter === a.id} onClick={() => setActivityFilter(a.id)}>
									{a.profile?.display_name ?? a.id}
								</FilterButton>
							))}
						</div>
						<div className="relative flex-1 min-h-0">
							<div className="absolute inset-0 overflow-y-auto scrollbar-sleek">
								<ActivityFeed
									events={allEvents.data ?? []}
									agents={agents}
									hoveredAgent={hoveredAgent}
									activityFilter={activityFilter}
								/>
							</div>
							<div className="pointer-events-none absolute bottom-0 left-0 right-0 h-20 bg-gradient-to-t from-app-darkerBox via-app-darkerBox/60 to-transparent" />
						</div>
					</section>
					</div>
				)}
			</main>
		</div>
	);
}

function SectionHeader({ title, right }: { title: string; right?: React.ReactNode }) {
	return (
		<div className="mb-3.5 flex items-center justify-between">
			<h2 className="text-[12px] font-semibold uppercase tracking-[0.12em] text-ink-dull">{title}</h2>
			{right}
		</div>
	);
}

function HeroSection({
	status,
	totalChannels,
	totalAgents,
	activity,
}: {
	status: { status: string; pid: number; uptime_seconds: number } | undefined;
	totalChannels: number;
	totalAgents: number;
	activity: { workers: number; branches: number; typing: number };
}) {
	const uptime = status?.uptime_seconds ?? 0;

	return (
		<div className="border-b border-app-line/60 bg-app-darkBox/80 px-4 py-2.5">
			<div className="mx-auto flex items-center justify-between">
				<div className="flex items-center gap-3">
					<h1 className="font-plex text-[17px] font-semibold tracking-tight text-ink">OpenOz</h1>
					{status ? (
						<div className="flex items-center gap-1.5 text-[12px] text-ink-faint">
							<div className="h-1.5 w-1.5 rounded-full bg-success" />
							<span>{formatUptime(uptime)}</span>
						</div>
					) : (
						<div className="flex items-center gap-1.5 text-[12px]">
							<div className="h-1.5 w-1.5 rounded-full bg-error" />
							<span className="text-error">Offline</span>
						</div>
					)}
					<span className="text-[12px] text-ink-faint/60">·</span>
					<span className="text-[12px] tabular-nums text-ink-faint">{totalAgents} agents</span>
					<span className="text-[12px] text-ink-faint/60">·</span>
					<span className="text-[12px] tabular-nums text-ink-faint">{totalChannels} channels</span>
				</div>

				{(activity.workers > 0 || activity.branches > 0) && (
					<div className="flex items-center gap-2">
						{activity.workers > 0 && (
							<span className="flex items-center gap-1.5 rounded-full bg-warning/10 px-2 py-0.5 text-[11px] text-warning">
								<span className="h-1.5 w-1.5 animate-pulse rounded-full bg-warning" />
								{activity.workers}w
							</span>
						)}
						{activity.branches > 0 && (
							<span className="flex items-center gap-1.5 rounded-full bg-accent/10 px-2 py-0.5 text-[11px] text-accent">
								<span className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
								{activity.branches}b
							</span>
						)}
					</div>
				)}
			</div>
		</div>
	);
}

function hashCode(s: string): number {
	let h = 0;
	for (let i = 0; i < s.length; i++) { h = s.charCodeAt(i) + ((h << 5) - h); h |= 0; }
	return h;
}

function seedGradient(seed: string): [string, string] {
	let hash = 0;
	for (let i = 0; i < seed.length; i++) {
		hash = seed.charCodeAt(i) + ((hash << 5) - hash);
		hash |= 0;
	}
	const hue1 = ((hash >>> 0) % 360);
	const hue2 = (hue1 + 40 + ((hash >>> 8) % 60)) % 360;
	return [
		`hsl(${hue1}, 70%, 55%)`,
		`hsl(${hue2}, 60%, 45%)`,
	];
}

function seedColorAlpha(color: string, alpha: number): string {
	return color.replace('hsl(', 'hsla(').replace(')', `, ${alpha})`);
}

const AGENT_ICONS = [BotIcon, BrainIcon, CpuIcon, ZapIcon, ActivityIcon, AtomIcon, SparklesIcon, RocketIcon, FlameIcon];

function AgentAvatar({ seed, size = 64 }: { seed: string; size?: number }) {
	const [c1, c2] = seedGradient(seed);
	const h = hashCode(seed);
	const IconComponent = AGENT_ICONS[Math.abs(h) % AGENT_ICONS.length];
	const iconSize = Math.round(size * 0.55);

	return (
		<div
			className="flex-shrink-0 flex items-center justify-center rounded-full"
			style={{
				width: size,
				height: size,
				background: `linear-gradient(135deg, ${c1}, ${c2})`,
			}}
		>
			<IconComponent size={iconSize} className="text-white/90" />
		</div>
	);
}

function AgentCard({
	agent,
	liveActivity,
	isHovered,
	isDimmed,
	onHover,
}: {
	agent: AgentSummary;
	liveActivity: { workers: number; branches: number; hasActivity: boolean };
	isHovered: boolean;
	isDimmed: boolean;
	onHover: (id: string | null) => void;
}) {
	const isActive = liveActivity.hasActivity || (agent.last_activity_at && new Date(agent.last_activity_at).getTime() > Date.now() - 5 * 60 * 1000);
	const profile = agent.profile;
	const displayName = profile?.display_name ?? agent.id;
	const avatarSeed = profile?.avatar_seed ?? agent.id;
	const [c1, c2] = seedGradient(avatarSeed);

	return (
		<motion.div variants={cardEntrance} className="h-full">
			<Link
				to="/agents/$agentId"
				params={{ agentId: agent.id }}
				className="group relative flex flex-col h-full rounded-xl overflow-hidden transition-all duration-300"
				style={{
					border: `1px solid color-mix(in srgb, ${c1} ${isHovered ? "18%" : "10%"}, rgba(255,255,255,${isHovered ? "0.1" : "0.06"}))`,
					boxShadow: isHovered ? `0 4px 16px rgba(0,0,0,0.3), 0 1px 3px rgba(0,0,0,0.2), 0 0 0 1px rgba(255,255,255,0.06), 0 2px 12px ${seedColorAlpha(c1, 0.14)}, inset 0 1px 0 rgba(255,255,255,0.05)` : `0 1px 2px rgba(0,0,0,0.25), 0 0 0 1px rgba(255,255,255,0.03), 0 1px 4px rgba(0,0,0,0.2)`,
					background: `linear-gradient(170deg, ${seedColorAlpha(c1, 0.06)} 0%, ${seedColorAlpha(c2, 0.02)} 40%, hsla(245,12%,12%,1) 100%)`,
					opacity: isDimmed ? 0.4 : 1,
					filter: isDimmed ? "brightness(0.7)" : "none",
				}}
				onMouseEnter={() => onHover(agent.id)}
				onMouseLeave={() => onHover(null)}
			>
				<div
					className="pointer-events-none absolute inset-0 z-10 animate-card-shine"
					style={{
						"--shine-duration": `${30 + Math.abs(hashCode(avatarSeed) % 20)}s`,
						"--shine-delay": `${Math.abs(hashCode(avatarSeed + "d") % 20)}s`,
						backgroundImage: `linear-gradient(105deg, transparent 40%, rgba(255,255,255,0.01) 45%, rgba(255,255,255,0.01) 55%, transparent 60%)`,
						backgroundSize: "250% 100%",
					} as React.CSSProperties}
				/>
				<div className="relative flex-1 p-5">
					<div className="flex items-start gap-3">
						<div className="relative shrink-0 mt-0.5">
							<AgentAvatar seed={avatarSeed} size={36} />
							<div
								className={`absolute -bottom-0.5 -right-0.5 h-2.5 w-2.5 rounded-full border-[1.5px] ${
									isActive ? "bg-success shadow-[0_0_6px_rgba(74,222,128,0.4)]" : "bg-ink-faint/30"
								}`}
								style={{ borderColor: `hsla(245,12%,12%,1)` }}
							/>
						</div>
						<div className="min-w-0 flex-1">
							<div className="flex items-center justify-between">
								<h3 className="truncate flex-1 min-w-0 text-[15px] font-semibold tracking-[-0.01em] text-ink group-hover:text-white transition-colors">{displayName}</h3>
								{(liveActivity.workers > 0 || liveActivity.branches > 0) && (
									<div className="flex items-center gap-1 ml-2 shrink-0">
										{liveActivity.workers > 0 && (
											<span className="flex items-center gap-1 rounded-full bg-warning/10 px-1.5 py-0.5 text-[10px] font-medium text-warning">
												<span className="h-1 w-1 animate-pulse rounded-full bg-warning" />
												{liveActivity.workers}
											</span>
										)}
										{liveActivity.branches > 0 && (
											<span className="flex items-center gap-1 rounded-full bg-accent/10 px-1.5 py-0.5 text-[10px] font-medium text-accent">
												<span className="h-1 w-1 animate-pulse rounded-full bg-accent" />
												{liveActivity.branches}
											</span>
										)}
									</div>
								)}
							</div>
							{profile?.bio && (
								<ScrollOnHover text={profile.bio} className="mt-1 text-[13px] leading-[1.5] text-ink-dull/70" lines={1} />
							)}
						</div>
					</div>

					<div className="flex items-center gap-4 mt-auto pt-3 border-t border-white/[0.05] text-[12px]">
						<StatusRow label="Status" value={isActive ? "Active" : "Idle"} active={!!isActive} />
						<StatusRow label="Last" value={agent.last_activity_at ? formatTimeAgo(agent.last_activity_at) : "—"} />
						<StatusRow label="Ch" value={String(agent.channel_count)} />
						<StatusRow label="Mem" value={agent.memory_total >= 1000 ? `${(agent.memory_total / 1000).toFixed(1)}k` : String(agent.memory_total)} />
					</div>
				</div>
			</Link>
		</motion.div>
	);
}

function ScrollOnHover({ text, className, lines = 2 }: { text: string; className?: string; lines?: number }) {
	const innerRef = useRef<HTMLParagraphElement>(null);
	const [expanded, setExpanded] = useState(false);
	const [animating, setAnimating] = useState(false);
	const [hasOverflow, setHasOverflow] = useState(false);
	const [clampedH, setClampedH] = useState(0);
	const [naturalH, setNaturalH] = useState(0);

	useEffect(() => {
		const el = innerRef.current;
		if (!el) return;
		const clamped = el.clientHeight;
		setClampedH(clamped);
		el.style.webkitLineClamp = "unset";
		const natural = el.scrollHeight;
		el.style.webkitLineClamp = "";
		setNaturalH(natural);
		setHasOverflow(natural > clamped + 2);
	}, [text, lines]);

	const useClamp = !expanded && !animating;

	return (
		<div
			className="overflow-hidden transition-[max-height] ease-[cubic-bezier(0.25,0.1,0.25,1)]"
			style={{
				maxHeight: expanded ? `${naturalH}px` : clampedH ? `${clampedH}px` : `${lines * 1.5}em`,
				transitionDuration: expanded ? "250ms" : "350ms",
			}}
			onMouseEnter={() => {
				if (!hasOverflow) return;
				setAnimating(true);
				setExpanded(true);
			}}
			onMouseLeave={() => setExpanded(false)}
			onTransitionEnd={(e) => {
				if (e.propertyName === "max-height" && !expanded) setAnimating(false);
			}}
		>
			<p
				ref={innerRef}
				className={`${useClamp ? `line-clamp-${lines}` : ""} ${className ?? ""}`}
			>
				{text}
			</p>
		</div>
	);
}

function StatusRow({ label, value, active }: { label: string; value: string; active?: boolean }) {
	return (
		<div className="flex items-center gap-1.5">
			{active !== undefined && (
				<div className={`h-1.5 w-1.5 rounded-full ${active ? "bg-success" : "bg-ink-faint/30"}`} />
			)}
			<span className="text-ink-faint/50">{label}</span>
			<span className="tabular-nums text-ink-dull/80">{value}</span>
		</div>
	);
}

type ActivityEvent = CortexEvent & { agentId: string; agentName: string };

const EVENT_ICONS: Record<string, string> = {
	inbound_message: "💬",
	outbound_message: "→",
	worker_started: "⚡",
	worker_completed: "✓",
	worker_failed: "✗",
	branch_started: "⎇",
	branch_completed: "✓",
	bulletin_generated: "📋",
	memory_merged: "🧠",
	association_created: "🔗",
	observation_created: "👁",
	cron_triggered: "⏰",
};

function getEventIndicator(eventType: string): { border: string; bg: string } | null {
	if (eventType.includes("failed") || eventType.includes("error")) {
		return { border: "border-l-2 border-red-500/60", bg: "bg-red-500/[0.03]" };
	}
	if (eventType.includes("completed") || eventType.includes("success")) {
		return { border: "border-l-2 border-emerald-500/40", bg: "" };
	}
	return null;
}

function ActivityFeed({ events, agents, hoveredAgent, activityFilter }: { events: ActivityEvent[]; agents: AgentSummary[]; hoveredAgent: string | null; activityFilter: string }) {
	const filteredEvents = useMemo(() => {
		if (activityFilter === "all") return events;
		if (activityFilter === "errors") return events.filter(e => e.event_type.includes("failed") || e.event_type.includes("error"));
		return events.filter(e => e.agentId === activityFilter);
	}, [events, activityFilter]);

	if (filteredEvents.length === 0) {
		return (
			<div className="rounded-lg border border-dashed border-white/[0.06] p-6 text-center">
				<p className="text-[12px] text-ink-faint/50">No recent activity</p>
			</div>
		);
	}

	return (
		<div className="flex flex-col pb-12">
			<AnimatePresence mode="popLayout" initial={false}>
				{filteredEvents.map((event, i) => {
					const agent = agents.find((a) => a.id === event.agentId);
					const avatarSeed = agent?.profile?.avatar_seed ?? event.agentId;
					const icon = EVENT_ICONS[event.event_type] ?? "·";
					const isHighlighted = hoveredAgent === null || hoveredAgent === event.agentId;
					const indicator = getEventIndicator(event.event_type);
					const isError = event.event_type.includes("failed") || event.event_type.includes("error");

					return (
						<motion.div
							key={event.id}
							initial={{ opacity: 0, y: 6 }}
							animate={{ opacity: isHighlighted ? 1 : 0.25, y: 0, filter: isHighlighted ? "none" : "brightness(0.6)" }}
							exit={{ opacity: 0, y: -4 }}
							transition={{ type: "spring", stiffness: 400, damping: 30 }}
							className={`flex items-start gap-3 py-1.5 px-2.5 rounded-lg ${i > 0 ? "border-t border-white/[0.04]" : ""} ${indicator?.border ?? ""} ${indicator?.bg ?? ""}`}
						>
							<AgentAvatar seed={avatarSeed} size={28} />
							<div className="min-w-0 flex-1">
								<div className="flex items-center gap-2">
									<span className="text-[13px] font-medium text-ink">{event.agentName}</span>
									<span className="text-[11px] text-ink-faint">{formatTimeAgo(event.created_at)}</span>
								</div>
								<p className={`mt-0.5 text-[13px] leading-[1.5] line-clamp-1 ${isError ? "font-mono text-red-400/80" : "text-ink-dull"}`}>
									<span className="mr-1.5">{icon}</span>
									{event.summary}
								</p>
							</div>
							<EventTypeBadge type={event.event_type} />
						</motion.div>
					);
				})}
			</AnimatePresence>
		</div>
	);
}

const BADGE_COLORS: Record<string, string> = {
	inbound_message: "bg-blue-500/10 text-blue-400",
	outbound_message: "bg-violet-500/10 text-violet-400",
	worker_started: "bg-amber-500/10 text-amber-400",
	worker_completed: "bg-emerald-500/10 text-emerald-400",
	worker_failed: "bg-red-500/10 text-red-400",
	branch_started: "bg-cyan-500/10 text-cyan-400",
	bulletin_generated: "bg-indigo-500/10 text-indigo-400",
	memory_merged: "bg-purple-500/10 text-purple-400",
	cron_triggered: "bg-orange-500/10 text-orange-400",
};

function EventTypeBadge({ type }: { type: string }) {
	const shortName = type.replace(/_/g, " ").replace(/^(\w)/, (c) => c.toUpperCase());
	const colorClass = BADGE_COLORS[type] ?? "bg-white/[0.04] text-ink-faint/60";
	return (
		<span className={`shrink-0 rounded-full px-2.5 py-0.5 text-[11px] font-medium ${colorClass}`}>
			{shortName.length > 14 ? shortName.slice(0, 12) + "…" : shortName}
		</span>
	);
}

function StatsPills({
	uptime,
	totalAgents,
	activeAgents,
	totalChannels,
	totalMemories,
	activity,
}: {
	uptime: number;
	totalAgents: number;
	activeAgents: number;
	totalChannels: number;
	totalMemories: number;
	activity: { workers: number; branches: number; typing: number };
}) {
	const stats = [
		{ label: "Uptime", value: formatUptime(uptime), accent: true },
		{ label: "Active", value: `${activeAgents}/${totalAgents}`, accent: activeAgents > 0 },
		{ label: "Channels", value: String(totalChannels), accent: false },
		{ label: "Memories", value: totalMemories >= 1000 ? `${(totalMemories / 1000).toFixed(1)}k` : String(totalMemories), accent: false },
		...(activity.workers > 0 ? [{ label: "Workers", value: String(activity.workers), accent: true }] : []),
		...(activity.branches > 0 ? [{ label: "Branches", value: String(activity.branches), accent: true }] : []),
	];

	return (
		<div className="flex flex-wrap gap-2 mb-4">
			{stats.map(stat => (
				<span key={stat.label} className="flex items-center gap-1.5 rounded-md bg-app-darkBox/80 border border-app-line/40 px-2.5 py-1 text-xs">
					<span className="text-ink-faint">{stat.label}</span>
					<span className={`font-medium ${stat.accent ? "text-accent" : "text-ink"}`}>{stat.value}</span>
				</span>
			))}
		</div>
	);
}
