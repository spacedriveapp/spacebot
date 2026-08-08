import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Card, CardHeader, CardContent, Button } from "@spacedrive/primitives";
import { api, type ActivityDay } from "@/api/client";
import {
	ResponsiveContainer,
	AreaChart,
	Area,
	XAxis,
	YAxis,
	CartesianGrid,
	Tooltip,
} from "recharts";

const CHART_COLORS = {
	grid: "#2a2a3a",
	axis: "#4a4a5a",
	tick: "#8a8a9a",
	tooltip: {
		bg: "#1a1a2e",
		border: "#2a2a3a",
		text: "#e0e0e0",
	},
};

const SERIES = [
	{ key: "messages", label: "Messages", color: "#3b82f6" },
	{ key: "branches", label: "Branches", color: "#8b5cf6" },
	{ key: "workers", label: "Workers", color: "#f59e0b" },
	{ key: "cron", label: "Cron", color: "#10b981" },
] as const;

type Period = "7d" | "30d" | "90d";

const PERIOD_DAYS: Record<Period, number> = {
	"7d": 7,
	"30d": 30,
	"90d": 90,
};

function sinceDate(period: Period): string {
	const d = new Date();
	d.setDate(d.getDate() - PERIOD_DAYS[period]);
	return d.toISOString();
}

function formatCost(usd: number): string {
	if (usd >= 1) return `$${usd.toFixed(2)}`;
	if (usd >= 0.01) return `$${usd.toFixed(3)}`;
	return `$${usd.toFixed(4)}`;
}

function formatNumber(n: number): string {
	if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
	if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
	return String(n);
}

export function ActivityCard() {
	const [period, setPeriod] = useState<Period>("30d");

	const { data, isLoading } = useQuery({
		queryKey: ["activity", period],
		queryFn: () => api.activity({ since: sinceDate(period) }),
		staleTime: 60_000,
	});

	const chartData = (data?.daily ?? []).map((d: ActivityDay) => ({
		date: new Date(d.date).toLocaleDateString("en-US", {
			month: "short",
			day: "numeric",
		}),
		messages: d.messages,
		branches: d.branches,
		workers: d.workers,
		cron: d.cron,
	}));

	const totals = data?.totals;
	const hasData = totals && (totals.messages + totals.branches + totals.workers + totals.cron) > 0;

	return (
		<Card variant="dark" className="flex flex-col">
			<CardHeader className="flex-row items-center justify-between p-4 pb-3">
				<h2 className="font-plex text-sm font-medium text-ink-dull">
					Instance Activity
				</h2>
				<div className="flex items-center gap-0.5">
					{(["7d", "30d", "90d"] as Period[]).map((p) => (
						<Button
							key={p}
							size="xs"
							variant={period === p ? "gray" : "subtle"}
							onClick={() => setPeriod(p)}
						>
							{p}
						</Button>
					))}
				</div>
			</CardHeader>

			<CardContent className="flex flex-1 flex-col px-4 pb-4 pt-0">
				{isLoading || !totals ? (
					<div className="flex h-48 items-center justify-center">
						<span className="text-sm text-ink-faint">Loading...</span>
					</div>
				) : !hasData ? (
					<div className="flex h-48 items-center justify-center">
						<p className="text-sm text-ink-faint">
							No activity recorded in this period.
						</p>
					</div>
				) : (
					<>
						{/* Chart */}
						<div className="h-48 min-h-[192px]">
							<ResponsiveContainer
								width="100%"
								height="100%"
								minWidth={100}
								minHeight={100}
							>
								<AreaChart
									data={chartData}
									margin={{ top: 10, right: 10, left: 0, bottom: 0 }}
								>
									<defs>
										{SERIES.map((s) => (
											<linearGradient
												key={s.key}
												id={`activity-${s.key}`}
												x1="0"
												y1="0"
												x2="0"
												y2="1"
											>
												<stop
													offset="5%"
													stopColor={s.color}
													stopOpacity={0.3}
												/>
												<stop
													offset="95%"
													stopColor={s.color}
													stopOpacity={0.02}
												/>
											</linearGradient>
										))}
									</defs>
									<CartesianGrid
										stroke={CHART_COLORS.grid}
										strokeDasharray="3 3"
										vertical={false}
									/>
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
									{SERIES.map((s) => (
										<Area
											key={s.key}
											type="monotone"
											dataKey={s.key}
											name={s.label}
											stroke={s.color}
											strokeWidth={2}
											fill={`url(#activity-${s.key})`}
											fillOpacity={1}
											stackId="1"
										/>
									))}
								</AreaChart>
							</ResponsiveContainer>
						</div>

						{/* Summary row */}
						<div className="mt-3 flex flex-wrap items-center gap-x-4 gap-y-1.5 text-tiny">
							{SERIES.map((s) => {
								const val = totals[s.key as keyof typeof totals] as number;
								if (val === 0) return null;
								return (
									<div key={s.key} className="flex items-center gap-1.5">
										<span
											className="inline-block h-2 w-2 rounded-full"
											style={{ backgroundColor: s.color }}
										/>
										<span className="text-ink-faint">{s.label}</span>
										<span className="tabular-nums text-ink-dull">
											{formatNumber(val)}
										</span>
									</div>
								);
							})}
							{totals.active_channels > 0 && (
								<div className="flex items-center gap-1.5">
									<span className="text-ink-faint">Channels</span>
									<span className="tabular-nums text-ink-dull">
										{totals.active_channels}
									</span>
								</div>
							)}
							{totals.tokens.cost_usd > 0 && (
								<div className="ml-auto flex items-center gap-1.5">
									<span className="text-ink-faint">Cost</span>
									<span className="tabular-nums text-ink-dull">
										{formatCost(totals.tokens.cost_usd)}
									</span>
								</div>
							)}
						</div>
					</>
				)}
			</CardContent>
		</Card>
	);
}
