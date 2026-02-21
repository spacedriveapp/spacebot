import { useMemo, useState } from "react";
import { Link } from "@tanstack/react-router";
import { useQuery } from "@tanstack/react-query";
import { motion } from "framer-motion";
import { api, type CortexEvent, type CronJobInfo, MEMORY_TYPES } from "@/api/client";
import type { ChannelLiveState } from "@/hooks/useChannelLiveState";
import { formatTimeAgo, formatDuration } from "@/lib/format";
import { DeleteAgentDialog } from "@/components/DeleteAgentDialog";
import { staggerContainer, slideUp, spring } from "@/lib/motion";
import {
	ResponsiveContainer,
	AreaChart,
	Area,
	XAxis,
	YAxis,
	CartesianGrid,
	Tooltip,
	PieChart,
	Pie,
	Cell,
} from "recharts";

interface AgentDetailProps {
	agentId: string;
	liveStates: Record<string, ChannelLiveState>;
}

export function AgentDetail({ agentId, liveStates }: AgentDetailProps) {
	const { data: agentsData } = useQuery({
		queryKey: ["agents"],
		queryFn: api.agents,
		refetchInterval: 30_000,
	});

	const { data: overviewData } = useQuery({
		queryKey: ["agent-overview", agentId],
		queryFn: () => api.agentOverview(agentId),
		refetchInterval: 15_000,
	});

	const { data: configData } = useQuery({
		queryKey: ["agent-config", agentId],
		queryFn: () => api.agentConfig(agentId),
		refetchInterval: 60_000,
	});

	const { data: identityData } = useQuery({
		queryKey: ["agent-identity", agentId],
		queryFn: () => api.agentIdentity(agentId),
		refetchInterval: 60_000,
	});

	const { data: channelsData } = useQuery({
		queryKey: ["channels"],
		queryFn: api.channels,
		refetchInterval: 10_000,
	});

	const agent = agentsData?.agents.find((a) => a.id === agentId);
	const agentChannels = useMemo(
		() => (channelsData?.channels ?? []).filter((c) => c.agent_id === agentId),
		[channelsData, agentId],
	);

	// Aggregate live activity for this agent
	const activity = useMemo(() => {
		let workers = 0;
		let branches = 0;
		let typing = 0;
		for (const channel of agentChannels) {
			const live = liveStates[channel.id];
			if (!live) continue;
			workers += Object.keys(live.workers).length;
			branches += Object.keys(live.branches).length;
			if (live.isTyping) typing++;
		}
		return { workers, branches, typing };
	}, [agentChannels, liveStates]);

	const [deleteOpen, setDeleteOpen] = useState(false);

	if (!agent) {
		return (
			<div className="flex h-full items-center justify-center">
				<p className="text-sm text-ink-faint">Agent not found: {agentId}</p>
			</div>
		);
	}

	const hasLiveActivity = activity.workers > 0 || activity.branches > 0 || activity.typing > 0;

	return (
		<div className="h-full overflow-y-auto">
			<div className="mx-auto max-w-6xl p-4 pb-6">
				{/* Hero Section */}
				<HeroSection
					agentId={agentId}
					channelCount={agentChannels.length}
					workers={activity.workers}
					branches={activity.branches}
					hasLiveActivity={hasLiveActivity}
					onDelete={() => setDeleteOpen(true)}
				/>
				<DeleteAgentDialog open={deleteOpen} onOpenChange={setDeleteOpen} agentId={agentId} />

				{/* Bulletin - the most important text */}
				{overviewData?.latest_bulletin && (
					<BulletinSection bulletin={overviewData.latest_bulletin} />
				)}

				{/* Charts Grid */}
				{overviewData && (
					<motion.div
						className="mt-8 grid grid-cols-1 gap-6 lg:grid-cols-2"
						variants={staggerContainer}
						initial="initial"
						animate="animate"
					>
						{/* Memory Growth Chart */}
						<div className="col-span-1 lg:col-span-2 rounded-xl border border-app-line bg-app-darkBox p-3.5 transition-colors duration-150 hover:border-app-hover">
							<div className="mb-4 flex items-center justify-between">
								<h3 className="font-plex text-sm font-medium text-ink-dull">Memory Growth</h3>
								<span className="text-tiny text-ink-faint">Last 30 days</span>
							</div>
							<MemoryGrowthChart data={overviewData.memory_daily} />
						</div>

						{/* Activity Heatmap */}
						<div className="rounded-xl border border-app-line bg-app-darkBox p-3.5 transition-colors duration-150 hover:border-app-hover">
							<div className="mb-4 flex items-center justify-between">
								<h3 className="font-plex text-sm font-medium text-ink-dull">Activity Heatmap</h3>
								<span className="text-tiny text-ink-faint">Messages by day/hour</span>
							</div>
							<ActivityHeatmap data={overviewData.activity_heatmap} />
						</div>

						{/* Process Activity Chart */}
						<div className="rounded-xl border border-app-line bg-app-darkBox p-3.5 transition-colors duration-150 hover:border-app-hover">
							<div className="mb-4 flex items-center justify-between">
								<h3 className="font-plex text-sm font-medium text-ink-dull">Process Activity</h3>
								<span className="text-tiny text-ink-faint">Branches + Workers</span>
							</div>
							<ProcessActivityChart data={overviewData.activity_daily} />
						</div>
					</motion.div>
				)}

				{/* Secondary Grid */}
				{overviewData && (
					<div className="mt-6 grid grid-cols-1 gap-6 lg:grid-cols-3">
						{/* Memory Donut */}
						<div className="rounded-xl border border-app-line bg-app-darkBox p-3.5 transition-colors duration-150 hover:border-app-hover">
							<div className="mb-4 flex items-center justify-between">
								<h3 className="font-plex text-sm font-medium text-ink-dull">Memory Types</h3>
								<span className="text-sm font-semibold tabular-nums text-ink">{overviewData.memory_total}</span>
							</div>
							<MemoryDonut counts={overviewData.memory_counts} />
						</div>

						{/* Model Routing */}
						{configData && (
							<div className="rounded-xl border border-app-line bg-app-darkBox p-3.5 transition-colors duration-150 hover:border-app-hover">
								<div className="mb-4 flex items-center justify-between">
									<h3 className="font-plex text-sm font-medium text-ink-dull">Model Routing</h3>
									<Link
										to="/agents/$agentId/config"
										params={{ agentId }}
										className="text-tiny text-accent hover:text-accent/80"
									>
										Edit
									</Link>
								</div>
								<ModelRoutingList config={configData} />
							</div>
						)}

						{/* Quick Stats */}
						<div className="rounded-xl border border-app-line bg-app-darkBox p-3.5 transition-colors duration-150 hover:border-app-hover">
							<div className="mb-4">
								<h3 className="font-plex text-sm font-medium text-ink-dull">Configuration</h3>
							</div>
							<div className="flex flex-col gap-3">
								<StatRow label="Context Window" value={agent.context_window.toLocaleString()} />
								<StatRow label="Max Turns" value={String(agent.max_turns)} />
								<StatRow label="Max Branches" value={String(agent.max_concurrent_branches)} />
								<StatRow label="Max Workers" value={String(agent.max_concurrent_workers)} />
								<StatRow label="Workspace" value={agent.workspace} truncate />
							</div>
						</div>
					</div>
				)}

				{/* Identity Preview */}
				{identityData && <IdentitySection agentId={agentId} identity={identityData} />}

				{/* Cron Jobs */}
				{overviewData && overviewData.cron_jobs.length > 0 && (
					<CronSection agentId={agentId} jobs={overviewData.cron_jobs} />
				)}

				{/* Recent Cortex Events */}
				{overviewData && overviewData.recent_cortex_events.length > 0 && (
					<CortexEventsSection
						agentId={agentId}
						events={overviewData.recent_cortex_events}
						lastBulletinAt={overviewData.last_bulletin_at}
					/>
				)}
			</div>
		</div>
	);
}

// -- Section Components --

function HeroSection({
	agentId,
	channelCount,
	workers,
	branches,
	hasLiveActivity,
	onDelete,
}: {
	agentId: string;
	channelCount: number;
	workers: number;
	branches: number;
	hasLiveActivity: boolean;
	onDelete: () => void;
}) {
	return (
		<div className="flex flex-col gap-3 border-b border-app-line pb-4">
			<div className="flex items-start justify-between">
				<div>
					<h1 className="font-plex text-lg font-semibold tracking-tight text-ink">{agentId}</h1>
					<div className="mt-1.5 flex items-center gap-3 text-[13px]">
						<div className="flex items-center gap-2">
							<div className={`h-2 w-2 rounded-full ${hasLiveActivity ? "animate-pulse bg-warning" : "bg-success"}`} />
							<span className="text-ink-dull">{hasLiveActivity ? "Active" : "Idle"}</span>
						</div>
						<Link
							to="/agents/$agentId/channels"
							params={{ agentId }}
							className="text-accent hover:text-accent/80"
						>
							{channelCount} channel{channelCount !== 1 ? "s" : ""}
						</Link>
					</div>
				</div>
				<button
					onClick={onDelete}
					className="rounded-md px-3 py-1.5 text-sm text-ink-faint transition-colors hover:bg-red-500/10 hover:text-red-400"
				>
					Delete
				</button>
			</div>

			{(workers > 0 || branches > 0) && (
				<div className="flex flex-wrap gap-2">
					{workers > 0 && (
						<div className="flex items-center gap-2 rounded-full bg-warning/10 px-3 py-1.5 text-sm">
							<div className="h-2 w-2 animate-pulse rounded-full bg-warning" />
							<span className="font-medium text-warning">{workers} worker{workers !== 1 ? "s" : ""}</span>
						</div>
					)}
					{branches > 0 && (
						<div className="flex items-center gap-2 rounded-full bg-accent/10 px-3 py-1.5 text-sm">
							<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
							<span className="font-medium text-accent">{branches} branch{branches !== 1 ? "es" : ""}</span>
						</div>
					)}
				</div>
			)}
		</div>
	);
}

function BulletinSection({ bulletin }: { bulletin: string }) {
	const [expanded, setExpanded] = useState(false);
	const lines = bulletin.split("\n");
	const shouldTruncate = lines.length > 6;
	const displayText = expanded || !shouldTruncate ? bulletin : lines.slice(0, 6).join("\n") + "\n...";

	return (
		<div className="mt-4 rounded-xl border border-accent/20 bg-accent/5 p-3.5">
			<div className="mb-3 flex items-center gap-2">
				<div className="h-2 w-2 rounded-full bg-accent" />
				<h3 className="font-plex text-sm font-medium text-accent">Latest Memory Bulletin</h3>
			</div>
			<div className="whitespace-pre-wrap text-sm leading-relaxed text-ink-dull">
				{displayText}
			</div>
			{shouldTruncate && (
				<button
					onClick={() => setExpanded(!expanded)}
					className="mt-3 text-tiny text-accent hover:text-accent/80"
				>
					{expanded ? "Show less" : "Show more"}
				</button>
			)}
		</div>
	);
}

// -- Charts --

const CHART_COLORS = {
	grid: "hsla(225, 12%, 16%, 1)",
	axis: "hsla(225, 10%, 22%, 1)",
	tick: "hsla(225, 8%, 45%, 1)",
	accent: "hsla(250, 80%, 65%, 1)",
	amber: "hsla(38, 90%, 55%, 1)",
	violet: "hsla(250, 80%, 65%, 1)",
	green: "hsla(142, 70%, 50%, 1)",
	blue: "#3b82f6",
	tooltip: {
		bg: "hsla(225, 18%, 5%, 0.95)",
		border: "hsla(225, 14%, 16%, 1)",
		text: "hsla(225, 20%, 95%, 1)",
	},
};

const MEMORY_TYPE_COLORS = [
	"#3b82f6", // fact - blue
	"#ec4899", // preference - pink
	"#f59e0b", // decision - amber
	"#10b981", // identity - green
	"#06b6d4", // event - cyan
	"#8b5cf6", // observation - purple
	"#f97316", // goal - orange
	"#ef4444", // todo - red
];

function MemoryGrowthChart({ data }: { data: { date: string; count: number }[] }) {
	if (data.length === 0) {
		return <div className="h-48 flex items-center justify-center text-ink-faint text-sm">No data</div>;
	}

	// Calculate cumulative for area effect
	let cumulative = 0;
	const chartData = data.map((d) => {
		cumulative += d.count;
		return {
			date: new Date(d.date).toLocaleDateString("en-US", { month: "short", day: "numeric" }),
			daily: d.count,
			total: cumulative,
		};
	});

	return (
		<div className="h-48">
			<ResponsiveContainer width="100%" height="100%">
				<AreaChart data={chartData} margin={{ top: 10, right: 10, left: 0, bottom: 0 }}>
					<defs>
						<linearGradient id="memoryGradient" x1="0" y1="0" x2="0" y2="1">
							<stop offset="5%" stopColor={CHART_COLORS.accent} stopOpacity={0.4} />
							<stop offset="95%" stopColor={CHART_COLORS.accent} stopOpacity={0.05} />
						</linearGradient>
					</defs>
					<CartesianGrid stroke={CHART_COLORS.grid} strokeDasharray="3 3" vertical={false} />
					<XAxis
						dataKey="date"
						stroke={CHART_COLORS.axis}
						tick={{ fill: CHART_COLORS.tick, fontSize: 11 }}
						tickLine={false}
						axisLine={{ stroke: CHART_COLORS.grid }}
						minTickGap={30}
					/>
					<YAxis
						stroke={CHART_COLORS.axis}
						tick={{ fill: CHART_COLORS.tick, fontSize: 11 }}
						tickLine={false}
						axisLine={false}
					/>
					<Tooltip
						contentStyle={{
							backgroundColor: CHART_COLORS.tooltip.bg,
							border: `1px solid ${CHART_COLORS.tooltip.border}`,
							borderRadius: "6px",
							fontSize: "12px",
						}}
						labelStyle={{ color: CHART_COLORS.tick }}
						itemStyle={{ color: CHART_COLORS.tooltip.text }}
						cursor={{ stroke: CHART_COLORS.axis }}
					/>
					<Area
						type="monotone"
						dataKey="total"
						stroke={CHART_COLORS.accent}
						strokeWidth={2}
						fill="url(#memoryGradient)"
						fillOpacity={1}
					/>
				</AreaChart>
			</ResponsiveContainer>
		</div>
	);
}

function ProcessActivityChart({ data }: { data: { date: string; branches: number; workers: number }[] }) {
	if (data.length === 0) {
		return <div className="h-48 flex items-center justify-center text-ink-faint text-sm">No activity</div>;
	}

	const chartData = data.map((d) => ({
		date: new Date(d.date).toLocaleDateString("en-US", { month: "short", day: "numeric" }),
		branches: d.branches,
		workers: d.workers,
	}));

	return (
		<div className="h-48">
			<ResponsiveContainer width="100%" height="100%">
				<AreaChart data={chartData} margin={{ top: 10, right: 10, left: 0, bottom: 0 }}>
					<defs>
						<linearGradient id="branchGradient" x1="0" y1="0" x2="0" y2="1">
							<stop offset="5%" stopColor={CHART_COLORS.violet} stopOpacity={0.4} />
							<stop offset="95%" stopColor={CHART_COLORS.violet} stopOpacity={0.05} />
						</linearGradient>
						<linearGradient id="workerGradient" x1="0" y1="0" x2="0" y2="1">
							<stop offset="5%" stopColor={CHART_COLORS.amber} stopOpacity={0.4} />
							<stop offset="95%" stopColor={CHART_COLORS.amber} stopOpacity={0.05} />
						</linearGradient>
					</defs>
					<CartesianGrid stroke={CHART_COLORS.grid} strokeDasharray="3 3" vertical={false} />
					<XAxis
						dataKey="date"
						stroke={CHART_COLORS.axis}
						tick={{ fill: CHART_COLORS.tick, fontSize: 11 }}
						tickLine={false}
						axisLine={{ stroke: CHART_COLORS.grid }}
						minTickGap={30}
					/>
					<YAxis
						stroke={CHART_COLORS.axis}
						tick={{ fill: CHART_COLORS.tick, fontSize: 11 }}
						tickLine={false}
						axisLine={false}
					/>
					<Tooltip
						contentStyle={{
							backgroundColor: CHART_COLORS.tooltip.bg,
							border: `1px solid ${CHART_COLORS.tooltip.border}`,
							borderRadius: "6px",
							fontSize: "12px",
						}}
						labelStyle={{ color: CHART_COLORS.tick }}
						itemStyle={{ color: CHART_COLORS.tooltip.text }}
						cursor={{ stroke: CHART_COLORS.axis }}
					/>
					<Area
						type="monotone"
						dataKey="branches"
						stroke={CHART_COLORS.violet}
						strokeWidth={2}
						fill="url(#branchGradient)"
						fillOpacity={1}
					/>
					<Area
						type="monotone"
						dataKey="workers"
						stroke={CHART_COLORS.amber}
						strokeWidth={2}
						fill="url(#workerGradient)"
						fillOpacity={1}
					/>
				</AreaChart>
			</ResponsiveContainer>
		</div>
	);
}

function ActivityHeatmap({ data }: { data: { day: number; hour: number; count: number }[] }) {
	if (data.length === 0) {
		return <div className="h-48 flex items-center justify-center text-ink-faint text-sm">No activity data</div>;
	}

	const maxCount = Math.max(...data.map((d) => d.count), 1);
	const days = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

	// Create a 7x24 grid
	const getCell = (day: number, hour: number) => {
		const cell = data.find((d) => d.day === day && d.hour === hour);
		return cell?.count ?? 0;
	};

	const getOpacity = (count: number) => {
		if (count === 0) return 0.05;
		return 0.2 + (count / maxCount) * 0.8;
	};

	return (
		<div className="h-48 overflow-auto">
			<div className="flex flex-col gap-1">
				{/* Hour labels */}
				<div className="flex gap-1">
					<div className="w-8 flex-shrink-0" /> {/* Day label spacer */}
					{Array.from({ length: 24 }, (_, h) => (
						<div key={h} className="w-4 flex-shrink-0 text-center text-[10px] text-ink-faint">
							{h % 6 === 0 ? h : ""}
						</div>
					))}
				</div>
				{/* Heatmap grid */}
				{days.map((dayLabel, day) => (
					<div key={day} className="flex items-center gap-1">
						<div className="w-8 flex-shrink-0 text-[10px] text-ink-faint">{dayLabel}</div>
						<div className="flex gap-1">
							{Array.from({ length: 24 }, (_, hour) => {
								const count = getCell(day, hour);
								return (
									<div
										key={hour}
										title={`${dayLabel} ${hour}:00 - ${count} messages`}
										className="h-4 w-4 flex-shrink-0 rounded-sm bg-accent transition-opacity hover:ring-1 hover:ring-accent"
										style={{ opacity: getOpacity(count) }}
									/>
								);
							})}
						</div>
					</div>
				))}
			</div>
		</div>
	);
}

function MemoryDonut({ counts }: { counts: Record<string, number> }) {
	const data = MEMORY_TYPES.map((type, idx) => ({
		name: type,
		value: counts[type] ?? 0,
		color: MEMORY_TYPE_COLORS[idx % MEMORY_TYPE_COLORS.length],
	})).filter((d) => d.value > 0);

	if (data.length === 0) {
		return <div className="h-48 flex items-center justify-center text-ink-faint text-sm">No memories</div>;
	}

	return (
		<div>
			<div className="h-40">
				<ResponsiveContainer width="100%" height="100%">
					<PieChart>
						<Pie
							data={data}
							dataKey="value"
							nameKey="name"
							cx="50%"
							cy="50%"
							innerRadius={50}
							outerRadius={70}
							paddingAngle={2}
							stroke="none"
						>
							{data.map((entry, index) => (
								<Cell key={`cell-${index}`} fill={entry.color} />
							))}
						</Pie>
						<Tooltip
							contentStyle={{
								backgroundColor: CHART_COLORS.tooltip.bg,
								border: `1px solid ${CHART_COLORS.tooltip.border}`,
								borderRadius: "6px",
								fontSize: "12px",
							}}
							itemStyle={{ color: CHART_COLORS.tooltip.text }}
						/>
					</PieChart>
				</ResponsiveContainer>
			</div>
			<div className="mt-2 flex flex-wrap justify-center gap-2">
				{data.map((item) => (
					<div key={item.name} className="flex items-center gap-1.5 text-tiny">
						<div className="h-2 w-2 rounded-full" style={{ backgroundColor: item.color }} />
						<span className="text-ink-faint">{item.name}</span>
						<span className="tabular-nums text-ink-dull">{item.value}</span>
					</div>
				))}
			</div>
		</div>
	);
}

// -- List Components --

function ModelRoutingList({ config }: { config: { routing: { channel: string; branch: string; worker: string; compactor: string; cortex: string } } }) {
	const models = [
		{ label: "Channel", model: config.routing.channel, color: "text-success" },
		{ label: "Branch", model: config.routing.branch, color: "text-accent" },
		{ label: "Worker", model: config.routing.worker, color: "text-warning" },
		{ label: "Compactor", model: config.routing.compactor, color: "text-blue-400" },
		{ label: "Cortex", model: config.routing.cortex, color: "text-pink-400" },
	];

	return (
		<div className="flex flex-col gap-2">
			{models.map(({ label, model, color }) => (
				<div key={label} className="flex items-center justify-between">
					<span className="text-tiny text-ink-faint">{label}</span>
					<span className={`text-sm ${color} truncate max-w-[180px]`} title={model}>
						{formatModelName(model)}
					</span>
				</div>
			))}
		</div>
	);
}

function StatRow({ label, value, truncate }: { label: string; value: string; truncate?: boolean }) {
	return (
		<div className="flex items-center justify-between">
			<span className="text-tiny text-ink-faint">{label}</span>
			<span className={`text-sm text-ink-dull ${truncate ? "truncate max-w-[200px]" : ""}`} title={value}>
				{value}
			</span>
		</div>
	);
}

function formatModelName(model: string): string {
	const name = model.includes("/") ? model.split("/").pop()! : model;
	return name.replace(/-\d{8}$/, "").replace(/claude-/, "claude-").replace(/-202[0-9]+/, "");
}

// -- Other Sections --

function IdentitySection({
	agentId,
	identity,
}: {
	agentId: string;
	identity: { soul: string | null; identity: string | null; user: string | null };
}) {
	const hasContent = identity.soul || identity.identity || identity.user;
	if (!hasContent) return null;

	const files = [
		{ label: "SOUL.md", content: identity.soul },
		{ label: "IDENTITY.md", content: identity.identity },
		{ label: "USER.md", content: identity.user },
	].filter((f) => f.content && f.content.trim().length > 0 && !f.content.startsWith("<!--"));

	if (files.length === 0) return null;

	return (
		<section className="mt-4">
			<div className="mb-3 flex items-center justify-between">
				<h3 className="font-plex text-sm font-medium text-ink-dull">Identity</h3>
				<Link
					to="/agents/$agentId/config"
					params={{ agentId }}
					className="text-tiny text-accent hover:text-accent/80"
				>
					Edit
				</Link>
			</div>
			<div className="grid grid-cols-1 gap-4 lg:grid-cols-3">
				{files.map(({ label, content }) => (
					<div key={label} className="rounded-xl bg-app-darkBox p-4">
						<span className="text-tiny font-medium text-ink-faint">{label}</span>
						<p className="mt-2 line-clamp-4 whitespace-pre-wrap text-sm leading-relaxed text-ink-dull">
							{content!.trim()}
						</p>
					</div>
				))}
			</div>
		</section>
	);
}

function CronSection({ agentId, jobs }: { agentId: string; jobs: CronJobInfo[] }) {
	return (
		<section className="mt-4">
			<div className="mb-3 flex items-center justify-between">
				<h3 className="font-plex text-sm font-medium text-ink-dull">Cron Jobs</h3>
				<Link
					to="/agents/$agentId/cron"
					params={{ agentId }}
					className="text-tiny text-accent hover:text-accent/80"
				>
					Manage
				</Link>
			</div>
			<div className="flex flex-col gap-2">
				{jobs.map((job) => (
					<div
						key={job.id}
						className="flex items-center gap-3 rounded-xl bg-app-darkBox px-4 py-3"
					>
						<div
							className={`h-2 w-2 rounded-full ${job.enabled ? "bg-success" : "bg-ink-faint/50"}`}
							title={job.enabled ? "Enabled" : "Disabled"}
						/>
						<span className="min-w-0 flex-1 truncate text-sm text-ink-dull" title={job.prompt}>
							{job.prompt}
						</span>
						<span className="text-tiny tabular-nums text-ink-faint">
							every {formatDuration(job.interval_secs)}
						</span>
						{job.active_hours && (
							<span className="text-tiny text-ink-faint">
								{job.active_hours[0]}:00–{job.active_hours[1]}:00
							</span>
						)}
						<span className="text-tiny text-ink-faint">{job.delivery_target}</span>
					</div>
				))}
			</div>
		</section>
	);
}

const CORTEX_EVENT_COLORS: Record<string, string> = {
	bulletin_generated: "bg-success/20 text-success",
	bulletin_failed: "bg-error/20 text-error",
	maintenance_run: "bg-blue-500/20 text-blue-400",
	memory_merged: "bg-accent/20 text-accent",
	memory_decayed: "bg-warning/20 text-warning",
	memory_pruned: "bg-orange-500/20 text-orange-400",
	association_created: "bg-accent/20 text-accent",
	contradiction_flagged: "bg-error/20 text-error",
	worker_killed: "bg-error/20 text-error",
	branch_killed: "bg-error/20 text-error",
	circuit_breaker_tripped: "bg-warning/20 text-warning",
	observation_created: "bg-accent/20 text-accent",
	health_check: "bg-ink-faint/10 text-ink-faint",
};

function CortexEventsSection({
	agentId,
	events,
	lastBulletinAt,
}: {
	agentId: string;
	events: CortexEvent[];
	lastBulletinAt: string | null;
}) {
	return (
		<section className="mt-4">
			<div className="mb-3 flex items-center justify-between">
				<h3 className="font-plex text-sm font-medium text-ink-dull">Recent Cortex Events</h3>
				<div className="flex items-center gap-4">
					{lastBulletinAt && (
						<span className="text-tiny text-ink-faint">
							Bulletin {formatTimeAgo(lastBulletinAt)}
						</span>
					)}
					<Link
						to="/agents/$agentId/cortex"
						params={{ agentId }}
						className="text-tiny text-accent hover:text-accent/80"
					>
						View all
					</Link>
				</div>
			</div>
			<div className="rounded-xl bg-app-darkBox p-4">
				<div className="flex flex-col gap-2">
					{events.map((event) => (
						<div key={event.id} className="flex items-center gap-3 text-sm">
							<CortexEventBadge type={event.event_type} />
							<span className="min-w-0 flex-1 truncate text-ink-dull">{event.summary}</span>
							<span className="flex-shrink-0 text-tiny tabular-nums text-ink-faint">
								{formatTimeAgo(event.created_at)}
							</span>
						</div>
					))}
				</div>
			</div>
		</section>
	);
}

function CortexEventBadge({ type }: { type: string }) {
	const color = CORTEX_EVENT_COLORS[type] ?? "bg-ink-faint/50/20 text-ink-faint";
	const label = type.replace(/_/g, " ");
	return (
		<span className={`flex-shrink-0 rounded px-1.5 py-0.5 text-tiny ${color}`}>
			{label}
		</span>
	);
}
