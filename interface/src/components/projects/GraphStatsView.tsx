import { useQuery } from "@tanstack/react-query";
import { api } from "@/api/client";
import {
	ResponsiveContainer,
	BarChart,
	Bar,
	XAxis,
	YAxis,
	Tooltip,
	PieChart,
	Pie,
	Cell,
} from "recharts";

const LABEL_COLORS: Record<string, string> = {
	Function: "#3b82f6",
	Method: "#06b6d4",
	Class: "#ec4899",
	Interface: "#a855f7",
	Struct: "#f59e0b",
	File: "#6366f1",
	Module: "#22c55e",
	Variable: "#78716c",
	Import: "#94a3b8",
	Enum: "#14b8a6",
	Trait: "#f97316",
	Route: "#ef4444",
	Test: "#84cc16",
	Community: "#8b5cf6",
	Process: "#0ea5e9",
};

const EDGE_COLORS: Record<string, string> = {
	CALLS: "#3b82f6",
	IMPORTS: "#22c55e",
	CONTAINS: "#6b7280",
	DEFINES: "#6b7280",
	EXTENDS: "#f59e0b",
	IMPLEMENTS: "#a855f7",
	HAS_METHOD: "#06b6d4",
	MEMBER_OF: "#8b5cf6",
	STEP_IN_PROCESS: "#0ea5e9",
};

function getColor(name: string, palette: Record<string, string>): string {
	return palette[name] ?? "#6b7280";
}

export function GraphStatsView({ projectId, compact }: { projectId: string; compact?: boolean }) {
	const { data, isLoading } = useQuery({
		queryKey: ["codegraph-stats", projectId],
		queryFn: () => api.codegraphStats(projectId),
	});

	if (isLoading) return <p className="text-sm text-ink-faint">Loading stats...</p>;
	if (!data) return <p className="text-sm text-ink-faint">No stats available</p>;

	const { total_nodes, total_edges, nodes_by_label, edges_by_type } = data;

	const fileCount = nodes_by_label.find((n) => n.label === "File")?.count ?? 0;
	const avgEdges = total_nodes > 0 ? (total_edges / total_nodes).toFixed(1) : "0";

	const barData = nodes_by_label.slice(0, 15).map((n) => ({
		name: n.label,
		count: n.count,
		fill: getColor(n.label, LABEL_COLORS),
	}));

	const pieData = edges_by_type.slice(0, 10).map((e) => ({
		name: e.edge_type,
		value: e.count,
	}));

	if (compact) {
		return (
			<div className="flex flex-col gap-3">
				{/* Summary row */}
				<div className="grid grid-cols-2 gap-2">
					<StatCard label="Nodes" value={total_nodes.toLocaleString()} />
					<StatCard label="Edges" value={total_edges.toLocaleString()} />
					<StatCard label="Files" value={fileCount.toLocaleString()} />
					<StatCard label="Avg E/N" value={avgEdges} />
				</div>

				{/* Nodes by label — horizontal bar list */}
				<div>
					<h4 className="mb-2 text-[10px] font-medium uppercase tracking-wide text-ink-faint">
						Nodes by Label
					</h4>
					<div className="flex flex-col gap-1">
						{barData.map((entry) => {
							const pct = total_nodes > 0 ? (entry.count / total_nodes) * 100 : 0;
							return (
								<div key={entry.name} className="flex items-center gap-2 text-[11px]">
									<span className="w-16 shrink-0 truncate text-ink-dull">{entry.name}</span>
									<div className="h-1.5 min-w-0 flex-1 overflow-hidden rounded-full bg-app-line">
										<div
											className="h-full rounded-full"
											style={{ width: `${Math.max(2, pct)}%`, backgroundColor: entry.fill }}
										/>
									</div>
									<span className="w-10 shrink-0 text-right tabular-nums text-ink-faint">
										{entry.count.toLocaleString()}
									</span>
								</div>
							);
						})}
					</div>
				</div>

				{/* Edges by type — same compact list */}
				<div>
					<h4 className="mb-2 text-[10px] font-medium uppercase tracking-wide text-ink-faint">
						Edges by Type
					</h4>
					<div className="flex flex-col gap-1">
						{pieData.map((entry) => {
							const pct = total_edges > 0 ? (entry.value / total_edges) * 100 : 0;
							return (
								<div key={entry.name} className="flex items-center gap-2 text-[11px]">
									<span className="w-16 shrink-0 truncate text-ink-dull">{entry.name}</span>
									<div className="h-1.5 min-w-0 flex-1 overflow-hidden rounded-full bg-app-line">
										<div
											className="h-full rounded-full"
											style={{
												width: `${Math.max(2, pct)}%`,
												backgroundColor: getColor(entry.name, EDGE_COLORS),
											}}
										/>
									</div>
									<span className="w-10 shrink-0 text-right tabular-nums text-ink-faint">
										{entry.value.toLocaleString()}
									</span>
								</div>
							);
						})}
					</div>
				</div>
			</div>
		);
	}

	return (
		<div className="flex flex-col gap-6">
			{/* Summary cards */}
			<div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
				<StatCard label="Total Nodes" value={total_nodes.toLocaleString()} />
				<StatCard label="Total Edges" value={total_edges.toLocaleString()} />
				<StatCard label="Files" value={fileCount.toLocaleString()} />
				<StatCard label="Avg Edges/Node" value={avgEdges} />
			</div>

			{/* Charts */}
			<div className="grid gap-4 lg:grid-cols-2">
				{/* Nodes by label */}
				<div className="rounded-xl border border-app-line bg-app-darkBox p-4">
					<h4 className="mb-3 text-xs font-medium text-ink-faint">Nodes by Label</h4>
					<div className="h-64">
						<ResponsiveContainer width="100%" height="100%">
							<BarChart data={barData} layout="vertical" margin={{ left: 80 }}>
								<XAxis type="number" tick={{ fill: "#9ca3af", fontSize: 11 }} />
								<YAxis
									type="category"
									dataKey="name"
									tick={{ fill: "#d1d5db", fontSize: 11 }}
									width={75}
								/>
								<Tooltip
									contentStyle={{
										backgroundColor: "#1f2937",
										border: "1px solid #374151",
										borderRadius: "6px",
										color: "#f3f4f6",
									}}
								/>
								<Bar dataKey="count" radius={[0, 4, 4, 0]}>
									{barData.map((entry, index) => (
										<Cell key={index} fill={entry.fill} />
									))}
								</Bar>
							</BarChart>
						</ResponsiveContainer>
					</div>
				</div>

				{/* Edges by type */}
				<div className="rounded-xl border border-app-line bg-app-darkBox p-4">
					<h4 className="mb-3 text-xs font-medium text-ink-faint">Edges by Type</h4>
					<div className="h-64">
						<ResponsiveContainer width="100%" height="100%">
							<PieChart>
								<Pie
									data={pieData}
									dataKey="value"
									nameKey="name"
									cx="50%"
									cy="50%"
									innerRadius={50}
									outerRadius={90}
									paddingAngle={2}
								>
									{pieData.map((entry, index) => (
										<Cell key={index} fill={getColor(entry.name, EDGE_COLORS)} />
									))}
								</Pie>
								<Tooltip
									contentStyle={{
										backgroundColor: "#1f2937",
										border: "1px solid #374151",
										borderRadius: "6px",
										color: "#f3f4f6",
									}}
								/>
							</PieChart>
						</ResponsiveContainer>
					</div>
					{/* Legend */}
					<div className="mt-2 flex flex-wrap gap-2">
						{pieData.map((entry) => (
							<div key={entry.name} className="flex items-center gap-1 text-xs text-ink-dull">
								<span
									className="inline-block h-2 w-2 rounded-full"
									style={{ backgroundColor: getColor(entry.name, EDGE_COLORS) }}
								/>
								{entry.name}
							</div>
						))}
					</div>
				</div>
			</div>
		</div>
	);
}

function StatCard({ label, value }: { label: string; value: string }) {
	return (
		<div className="rounded-lg border border-app-line bg-app-darkBox px-4 py-3">
			<p className="text-xs text-ink-faint">{label}</p>
			<p className="mt-1 text-lg font-semibold text-ink">{value}</p>
		</div>
	);
}
