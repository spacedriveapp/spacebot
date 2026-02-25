import { useMemo, useState } from "react";
import { Link } from "@tanstack/react-router";
import { useQuery } from "@tanstack/react-query";
import { api, type CortexEvent, type CronJobInfo, MEMORY_TYPES } from "@/api/client";
import type { ChannelLiveState } from "@/hooks/useChannelLiveState";
import { formatTimeAgo, formatDuration } from "@/lib/format";
import { DeleteAgentDialog } from "@/components/DeleteAgentDialog";
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
			<div className="mx-auto max-w-6xl p-6 pb-24">
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
					<div className="mt-8 grid grid-cols-1 gap-6 lg:grid-cols-2">
						{/* Memory Growth Chart */}
						<div className="col-span-1 lg:col-span-2 rounded-xl bg-app-darkBox p-5">
							<div className="mb-4 flex items-center justify-between">
								<h3 className="font-plex text-sm font-medium text-ink-dull">Memory Growth</h3>
								<span className="text-tiny text-ink-faint">Last 30 days</span>
							</div>
							<MemoryGrowthChart data={overviewData.memory_daily} />
						</div>

						{/* Activity Heatmap */}
						<div className="rounded-xl bg-app-darkBox p-5">
							<div className="mb-4 flex items-center justify-between">
								<h3 className="font-plex text-sm font-medium text-ink-dull">Activity Heatmap</h3>
								<span className="text-tiny text-ink-faint">Messages by day/hour</span>
							</div>
							<ActivityHeatmap data={overviewData.activity_heatmap} />
						</div>

						{/* Process Activity Chart */}
						<div className="rounded-xl bg-app-darkBox p-5">
							<div className="mb-4 flex items-center justify-between">
								<h3 className="font-plex text-sm font-medium text-ink-dull">Process Activity</h3>
								<span className="text-tiny text-ink-faint">Branches + Workers</span>
							</div>
							<ProcessActivityChart data={overviewData.activity_daily} />
						</div>
					</div>
				)}

				{/* Secondary Grid */}
				{overviewData && (
					<div className="mt-6 grid grid-cols-1 gap-6 lg:grid-cols-3">
						{/* Memory Donut */}
						<div className="rounded-xl bg-app-darkBox p-5">
							<div className="mb-4 flex items-center justify-between">
								<h3 className="font-plex text-sm font-medium text-ink-dull">Memory</h3>
								<Link
									to="/agents/$agentId/memories"
									params={{ agentId }}
									className="text-tiny text-accent hover:text-accent/80"
								>
									Edit
								</Link>
							</div>
							<MemoryDonut counts={overviewData.memory_counts} />
						</div>

						{/* Model Routing */}
						{configData && (
							<div className="flex h-full flex-col rounded-xl bg-app-darkBox p-5">
								<div className="mb-4 flex items-center justify-between">
									<h3 className="font-plex text-sm font-medium text-ink-dull">Model Routing</h3>
									<Link
										to="/agents/$agentId/config"
										params={{ agentId }}
										search={{ tab: "routing" }}
										className="text-tiny text-accent hover:text-accent/80"
									>
										Edit
									</Link>
								</div>
								<div className="flex-1">
									<ModelRoutingList config={configData} />
								</div>
							</div>
						)}

						{/* Quick Stats */}
						<div className="flex h-full flex-col rounded-xl bg-app-darkBox p-5">
							<div className="mb-4 flex items-center justify-between">
								<h3 className="font-plex text-sm font-medium text-ink-dull">Configuration</h3>
								<Link
									to="/agents/$agentId/config"
									params={{ agentId }}
									className="text-tiny text-accent hover:text-accent/80"
								>
									Edit
								</Link>
							</div>
							<div className="flex flex-1 flex-col justify-evenly gap-2">
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
		<div className="flex flex-col gap-4 border-b border-app-line pb-6">
			<div className="flex items-start justify-between">
				<div>
					<h1 className="font-plex text-3xl font-semibold text-ink">{agentId}</h1>
					<div className="mt-2 flex items-center gap-4 text-sm">
						<div className="flex items-center gap-2">
							<div className={`h-2 w-2 rounded-full ${hasLiveActivity ? "animate-pulse bg-amber-400" : "bg-green-500"}`} />
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
						<div className="flex items-center gap-2 rounded-full bg-amber-500/10 px-3 py-1.5 text-sm">
							<div className="h-2 w-2 animate-pulse rounded-full bg-amber-400" />
							<span className="font-medium text-amber-400">{workers} worker{workers !== 1 ? "s" : ""}</span>
						</div>
					)}
					{branches > 0 && (
						<div className="flex items-center gap-2 rounded-full bg-violet-500/10 px-3 py-1.5 text-sm">
							<div className="h-2 w-2 animate-pulse rounded-full bg-violet-400" />
							<span className="font-medium text-violet-400">{branches} branch{branches !== 1 ? "es" : ""}</span>
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
		<div className="mt-6 rounded-xl border border-accent/20 bg-accent/5 p-5">
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
	grid: "#2a2a3a",
	axis: "#4a4a5a",
	tick: "#8a8a9a",
	accent: "#6366f1", // indigo
	amber: "#f59e0b",
	violet: "#8b5cf6",
	green: "#10b981",
	blue: "#3b82f6",
	tooltip: {
		bg: "#1a1a2e",
		border: "#2a2a3a",
		text: "#e0e0e0",
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
	const hours = Array.from({ length: 24 }, (_value, index) => index);

	const getCell = (day: number, hour: number) => {
		const cell = data.find((d) => d.day === day && d.hour === hour);
		return cell?.count ?? 0;
	};

	const getOpacity = (count: number) => {
		if (count === 0) return 0.05;
		return 0.2 + (count / maxCount) * 0.8;
	};

	return (
		<div className="h-48">
			<div className="grid h-full grid-rows-[auto_repeat(7,minmax(0,1fr))] gap-y-1">
				<div className="grid grid-cols-[2rem_repeat(24,minmax(0,1fr))] gap-1">
					<div />
					{hours.map((hour) => (
						<div key={hour} className="text-center text-[10px] text-ink-faint">
							{hour % 6 === 0 ? hour : ""}
						</div>
					))}
				</div>
				{days.map((dayLabel, day) => (
					<div key={day} className="grid grid-cols-[2rem_repeat(24,minmax(0,1fr))] items-center gap-1">
						<div className="text-[10px] text-ink-faint">{dayLabel}</div>
						{hours.map((hour) => {
							const count = getCell(day, hour);
							return (
								<div
									key={hour}
									title={`${dayLabel} ${hour}:00 - ${count} messages`}
									className="h-3.5 w-full rounded-sm bg-accent transition-opacity hover:ring-1 hover:ring-accent"
									style={{ opacity: getOpacity(count) }}
								/>
							);
						})}
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
	const total = data.reduce((sum, item) => sum + item.value, 0);

	if (data.length === 0) {
		return <div className="h-48 flex items-center justify-center text-ink-faint text-sm">No memories</div>;
	}

	return (
			<div>
			<div className="relative h-40">
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
								padding: "4px 6px",
							}}
							wrapperStyle={{ zIndex: 20 }}
							labelStyle={{ display: "none" }}
							itemStyle={{ color: CHART_COLORS.tooltip.text, margin: 0, lineHeight: 1.2 }}
						/>
					</PieChart>
				</ResponsiveContainer>
				<div className="pointer-events-none absolute inset-0 z-0 flex flex-col items-center justify-center">
					<span className="text-2xl font-medium tabular-nums text-ink">{total}</span>
					<span className="text-tiny text-ink-faint">total</span>
				</div>
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
		{ label: "Channel", model: config.routing.channel, color: "text-green-400" },
		{ label: "Branch", model: config.routing.branch, color: "text-violet-400" },
		{ label: "Worker", model: config.routing.worker, color: "text-amber-400" },
		{ label: "Compactor", model: config.routing.compactor, color: "text-blue-400" },
		{ label: "Cortex", model: config.routing.cortex, color: "text-pink-400" },
	];

	return (
		<div className="flex h-full flex-col justify-evenly gap-2">
			{models.map(({ label, model, color }) => (
				<div key={label} className="flex items-center justify-between">
					<span className="text-tiny text-ink-faint">{label}</span>
					<span className={`text-sm ${color} truncate max-w-[140px]`} title={model}>
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
		{ label: "SOUL.md", tab: "soul", content: identity.soul },
		{ label: "IDENTITY.md", tab: "identity", content: identity.identity },
		{ label: "USER.md", tab: "user", content: identity.user },
	].filter((f) => f.content && f.content.trim().length > 0 && !f.content.startsWith("<!--"));

	if (files.length === 0) return null;

	return (
		<section className="mt-6">
			<div className="mb-3">
				<h3 className="font-plex text-sm font-medium text-ink-dull">Identity</h3>
			</div>
			<div className="grid grid-cols-1 gap-4 lg:grid-cols-3">
				{files.map(({ label, tab, content }) => (
					<div key={label} className="rounded-xl bg-app-darkBox p-4">
						<div className="flex items-center justify-between gap-3">
							<span className="text-tiny font-medium text-ink-faint">{label}</span>
							<Link
								to="/agents/$agentId/config"
								params={{ agentId }}
								search={{ tab }}
								className="text-tiny text-accent hover:text-accent/80"
							>
								Edit
							</Link>
						</div>
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
		<section className="mt-6">
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
							className={`h-2 w-2 rounded-full ${job.enabled ? "bg-green-500" : "bg-gray-500"}`}
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
	bulletin_generated: "bg-green-500/20 text-green-400",
	bulletin_failed: "bg-red-500/20 text-red-400",
	maintenance_run: "bg-blue-500/20 text-blue-400",
	memory_merged: "bg-cyan-500/20 text-cyan-400",
	memory_decayed: "bg-yellow-500/20 text-yellow-400",
	memory_pruned: "bg-orange-500/20 text-orange-400",
	association_created: "bg-purple-500/20 text-purple-400",
	contradiction_flagged: "bg-red-500/20 text-red-400",
	worker_killed: "bg-red-500/20 text-red-400",
	branch_killed: "bg-red-500/20 text-red-400",
	circuit_breaker_tripped: "bg-amber-500/20 text-amber-400",
	observation_created: "bg-indigo-500/20 text-indigo-400",
	health_check: "bg-gray-500/20 text-gray-400",
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
		<section className="mt-6">
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
	const color = CORTEX_EVENT_COLORS[type] ?? "bg-gray-500/20 text-gray-400";
	const label = type.replace(/_/g, " ");
	return (
		<span className={`flex-shrink-0 rounded px-1.5 py-0.5 text-tiny ${color}`}>
			{label}
		</span>
	);
}
