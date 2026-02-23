import { useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { AnimatePresence, motion } from "framer-motion";
import {
	LineChart,
	Line,
	XAxis,
	YAxis,
	Tooltip,
	ResponsiveContainer,
} from "recharts";
import { formatTimeAgo } from "@/lib/format";

// ---------------------------------------------------------------------------
// Types — defined locally until the API client is extended
// ---------------------------------------------------------------------------

const BASE_PATH: string = (window as any).__SPACEBOT_BASE_PATH || "";
const LEARNING_BASE = BASE_PATH + "/api/agents/learning";

async function learningFetch<T>(path: string): Promise<T> {
	const response = await fetch(path);
	if (!response.ok) throw new Error(`API error: ${response.status}`);
	return response.json();
}

export type DistillationType =
	| "policy"
	| "playbook"
	| "sharp_edge"
	| "heuristic"
	| "anti_pattern";

export const DISTILLATION_TYPES: DistillationType[] = [
	"policy",
	"playbook",
	"sharp_edge",
	"heuristic",
	"anti_pattern",
];

export interface LearningMetricsPoint {
	date: string;
	compounding_rate: number;
	advisory_precision: number;
	ralph_pass_rate: number;
	emission_rate: number;
}

export interface LearningMetricsResponse {
	compounding_rate: number;
	advisory_precision: number;
	ralph_pass_rate: number;
	emission_rate: number;
	history: LearningMetricsPoint[];
	recent_activity: { label: string; value: string; at: string }[];
}

export interface LearningDistillation {
	id: string;
	distillation_type: DistillationType;
	statement: string;
	confidence: number;
	retrieved_count: number;
	used_count: number;
	helped_count: number;
	created_at: string;
}

export interface LearningDistillationsResponse {
	distillations: LearningDistillation[];
	total: number;
}

export interface EpisodeStep {
	step: number;
	description: string;
	outcome: string | null;
}

export interface LearningEpisode {
	id: string;
	task: string;
	predicted_outcome: string | null;
	actual_outcome: string | null;
	duration_ms: number | null;
	steps: EpisodeStep[];
	created_at: string;
}

export interface LearningEpisodesResponse {
	episodes: LearningEpisode[];
	total: number;
}

export interface LearningInsight {
	id: string;
	category: string;
	content: string;
	reliability: number;
	confidence: number;
	validation_count: number;
	created_at: string;
}

export interface LearningInsightsResponse {
	insights: LearningInsight[];
	total: number;
}

export type ChipStatus = "active" | "promoted" | "dismissed" | "quarantined";

export interface LearningChip {
	chip_id: string;
	observation_count: number;
	success_rate: number;
	status: ChipStatus;
	confidence: number;
	created_at: string;
}

export interface LearningChipsResponse {
	chips: LearningChip[];
	total: number;
}

export interface QuarantineEntry {
	id: string;
	source: string;
	stage: string;
	reason: string;
	created_at: string;
}

export interface LearningQuarantineResponse {
	entries: QuarantineEntry[];
	total: number;
}

export interface Tuneable {
	key: string;
	value: string | number | boolean;
	description: string | null;
}

export interface LearningTuneablesResponse {
	tuneables: Tuneable[];
}

// ---------------------------------------------------------------------------
// Local API helpers
// ---------------------------------------------------------------------------

const learningApi = {
	metrics: (agentId: string) =>
		learningFetch<LearningMetricsResponse>(
			`${LEARNING_BASE}/metrics?agent_id=${encodeURIComponent(agentId)}`,
		),
	distillations: (agentId: string, type?: DistillationType) => {
		const params = new URLSearchParams({ agent_id: agentId });
		if (type) params.set("type", type);
		return learningFetch<LearningDistillationsResponse>(
			`${LEARNING_BASE}/distillations?${params}`,
		);
	},
	dismissDistillation: (agentId: string, id: string) =>
		fetch(`${LEARNING_BASE}/distillations/${encodeURIComponent(id)}/dismiss`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ agent_id: agentId }),
		}).then((r) => {
			if (!r.ok) throw new Error(`API error: ${r.status}`);
			return r.json();
		}),
	episodes: (agentId: string) =>
		learningFetch<LearningEpisodesResponse>(
			`${LEARNING_BASE}/episodes?agent_id=${encodeURIComponent(agentId)}`,
		),
	insights: (agentId: string) =>
		learningFetch<LearningInsightsResponse>(
			`${LEARNING_BASE}/insights?agent_id=${encodeURIComponent(agentId)}`,
		),
	promoteInsight: (agentId: string, id: string) =>
		fetch(`${LEARNING_BASE}/insights/${encodeURIComponent(id)}/promote`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ agent_id: agentId }),
		}).then((r) => {
			if (!r.ok) throw new Error(`API error: ${r.status}`);
			return r.json();
		}),
	dismissInsight: (agentId: string, id: string) =>
		fetch(`${LEARNING_BASE}/insights/${encodeURIComponent(id)}/dismiss`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ agent_id: agentId }),
		}).then((r) => {
			if (!r.ok) throw new Error(`API error: ${r.status}`);
			return r.json();
		}),
	chips: (agentId: string) =>
		learningFetch<LearningChipsResponse>(
			`${LEARNING_BASE}/chips?agent_id=${encodeURIComponent(agentId)}`,
		),
	quarantine: (agentId: string) =>
		learningFetch<LearningQuarantineResponse>(
			`${LEARNING_BASE}/quarantine?agent_id=${encodeURIComponent(agentId)}`,
		),
	tuneables: (agentId: string) =>
		learningFetch<LearningTuneablesResponse>(
			`${LEARNING_BASE}/tuneables?agent_id=${encodeURIComponent(agentId)}`,
		),
	updateTuneable: (agentId: string, key: string, value: string | number | boolean) =>
		fetch(`${LEARNING_BASE}/tuneables`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ agent_id: agentId, key, value }),
		}).then((r) => {
			if (!r.ok) throw new Error(`API error: ${r.status}`);
			return r.json();
		}),
};

// ---------------------------------------------------------------------------
// Tabs definition
// ---------------------------------------------------------------------------

const TABS = [
	{ id: "overview", label: "Overview" },
	{ id: "distillations", label: "Distillations" },
	{ id: "episodes", label: "Episodes" },
	{ id: "insights", label: "Insights" },
	{ id: "chips", label: "Chips" },
	{ id: "quarantine", label: "Quarantine" },
	{ id: "tuneables", label: "Tuneables" },
] as const;

type TabId = (typeof TABS)[number]["id"];

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatDurationMs(ms: number | null): string {
	if (ms === null) return "—";
	if (ms < 1000) return `${ms}ms`;
	if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
	return `${Math.floor(ms / 60_000)}m ${Math.floor((ms % 60_000) / 1000)}s`;
}

function ConfidenceBar({ value, color = "bg-accent" }: { value: number; color?: string }) {
	const pct = Math.round(Math.max(0, Math.min(1, value)) * 100);
	return (
		<div className="flex items-center gap-2">
			<div className="h-1.5 flex-1 overflow-hidden rounded-full bg-app-line">
				<div className={`h-full rounded-full ${color}`} style={{ width: `${pct}%` }} />
			</div>
			<span className="w-8 text-right text-tiny tabular-nums text-ink-faint">{pct}%</span>
		</div>
	);
}

function MetricCard({
	label,
	value,
	sub,
}: {
	label: string;
	value: string;
	sub?: string;
}) {
	return (
		<div className="rounded-lg border border-app-line/50 bg-app-darkBox/30 p-4">
			<p className="mb-1 text-tiny text-ink-faint">{label}</p>
			<p className="font-plex text-2xl font-semibold text-ink">{value}</p>
			{sub && <p className="mt-0.5 text-tiny text-ink-faint">{sub}</p>}
		</div>
	);
}

function LoadingSpinner() {
	return (
		<div className="flex flex-1 items-center justify-center py-12">
			<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
		</div>
	);
}

function ErrorState({ message }: { message: string }) {
	return (
		<div className="rounded-xl bg-red-500/10 px-4 py-3 text-sm text-red-400">{message}</div>
	);
}

function EmptyState({ message }: { message: string }) {
	return (
		<div className="flex items-start justify-center pt-[10vh]">
			<p className="text-sm text-ink-faint">{message}</p>
		</div>
	);
}

function FilterPill({
	label,
	active,
	onClick,
}: {
	label: string;
	active: boolean;
	onClick: () => void;
}) {
	return (
		<button
			onClick={onClick}
			className={`rounded-md px-2 py-1 text-tiny font-medium transition-colors ${
				active ? "bg-accent/15 text-accent" : "text-ink-faint hover:text-ink-dull"
			}`}
		>
			{label}
		</button>
	);
}

// ---------------------------------------------------------------------------
// Tab content: Overview
// ---------------------------------------------------------------------------

function OverviewTab({ agentId }: { agentId: string }) {
	const { data, isLoading, isError } = useQuery({
		queryKey: ["learning-metrics", agentId],
		queryFn: () => learningApi.metrics(agentId),
		refetchInterval: 30_000,
	});

	if (isLoading) return <LoadingSpinner />;
	if (isError) return <ErrorState message="Failed to load learning metrics" />;
	if (!data) return <EmptyState message="No metrics available yet" />;

	const fmt = (n: number) => `${Math.round(n * 100)}%`;

	return (
		<div className="flex flex-col gap-6">
			{/* Metric cards */}
			<div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
				<MetricCard label="Compounding Rate" value={fmt(data.compounding_rate)} sub="last 30 days" />
				<MetricCard label="Advisory Precision" value={fmt(data.advisory_precision)} />
				<MetricCard label="Ralph Pass Rate" value={fmt(data.ralph_pass_rate)} />
				<MetricCard label="Emission Rate" value={fmt(data.emission_rate)} />
			</div>

			{/* History chart */}
			{data.history.length > 0 && (
				<div className="rounded-lg border border-app-line/50 bg-app-darkBox/30 p-4">
					<p className="mb-4 text-tiny font-medium text-ink-dull">Metrics — last 30 days</p>
					<ResponsiveContainer width="100%" height={180}>
						<LineChart data={data.history} margin={{ top: 4, right: 8, bottom: 0, left: -24 }}>
							<XAxis
								dataKey="date"
								tick={{ fontSize: 10, fill: "var(--color-ink-faint, #666)" }}
								tickLine={false}
								axisLine={false}
							/>
							<YAxis
								domain={[0, 1]}
								tickFormatter={(v) => `${Math.round(v * 100)}%`}
								tick={{ fontSize: 10, fill: "var(--color-ink-faint, #666)" }}
								tickLine={false}
								axisLine={false}
							/>
							<Tooltip
								contentStyle={{
									background: "var(--color-app-darkBox, #1a1a1a)",
									border: "1px solid var(--color-app-line, #333)",
									borderRadius: 6,
									fontSize: 11,
								}}
								// eslint-disable-next-line @typescript-eslint/no-explicit-any
							formatter={((value: number) => `${Math.round(value * 100)}%`) as any}
							/>
							<Line
								type="monotone"
								dataKey="compounding_rate"
								name="Compounding"
								stroke="#6366f1"
								strokeWidth={1.5}
								dot={false}
							/>
							<Line
								type="monotone"
								dataKey="advisory_precision"
								name="Advisory"
								stroke="#22d3ee"
								strokeWidth={1.5}
								dot={false}
							/>
							<Line
								type="monotone"
								dataKey="ralph_pass_rate"
								name="Ralph Pass"
								stroke="#4ade80"
								strokeWidth={1.5}
								dot={false}
							/>
							<Line
								type="monotone"
								dataKey="emission_rate"
								name="Emission"
								stroke="#f59e0b"
								strokeWidth={1.5}
								dot={false}
							/>
						</LineChart>
					</ResponsiveContainer>
					<div className="mt-2 flex flex-wrap gap-4">
						{[
							{ label: "Compounding", color: "#6366f1" },
							{ label: "Advisory", color: "#22d3ee" },
							{ label: "Ralph Pass", color: "#4ade80" },
							{ label: "Emission", color: "#f59e0b" },
						].map((l) => (
							<div key={l.label} className="flex items-center gap-1.5">
								<span
									className="inline-block h-2 w-2 rounded-full"
									style={{ background: l.color }}
								/>
								<span className="text-tiny text-ink-faint">{l.label}</span>
							</div>
						))}
					</div>
				</div>
			)}

			{/* Recent activity */}
			{data.recent_activity.length > 0 && (
				<div className="rounded-lg border border-app-line/50 bg-app-darkBox/30 p-4">
					<p className="mb-3 text-tiny font-medium text-ink-dull">Recent Activity</p>
					<div className="flex flex-col gap-1.5">
						{data.recent_activity.map((item, i) => (
							<div key={i} className="flex items-center justify-between gap-4 text-sm">
								<span className="text-ink-dull">{item.label}</span>
								<span className="font-medium text-ink">{item.value}</span>
								<span className="ml-auto text-tiny text-ink-faint">{formatTimeAgo(item.at)}</span>
							</div>
						))}
					</div>
				</div>
			)}
		</div>
	);
}

// ---------------------------------------------------------------------------
// Tab content: Distillations
// ---------------------------------------------------------------------------

function DistillationsTab({ agentId }: { agentId: string }) {
	const queryClient = useQueryClient();
	const [typeFilter, setTypeFilter] = useState<DistillationType | null>(null);

	const { data, isLoading, isError } = useQuery({
		queryKey: ["learning-distillations", agentId, typeFilter],
		queryFn: () => learningApi.distillations(agentId, typeFilter ?? undefined),
		refetchInterval: 30_000,
	});

	const dismissMutation = useMutation({
		mutationFn: (id: string) => learningApi.dismissDistillation(agentId, id),
		onSuccess: () =>
			queryClient.invalidateQueries({ queryKey: ["learning-distillations", agentId] }),
	});

	const distillations = data?.distillations ?? [];

	return (
		<div className="flex flex-col gap-4">
			{/* Type filter pills */}
			<div className="flex flex-wrap items-center gap-1.5">
				<FilterPill label="All" active={typeFilter === null} onClick={() => setTypeFilter(null)} />
				{DISTILLATION_TYPES.map((t) => (
					<FilterPill
						key={t}
						label={t.replace(/_/g, " ")}
						active={typeFilter === t}
						onClick={() => setTypeFilter(typeFilter === t ? null : t)}
					/>
				))}
			</div>

			{isLoading && <LoadingSpinner />}
			{isError && <ErrorState message="Failed to load distillations" />}

			{!isLoading && !isError && distillations.length === 0 && (
				<EmptyState message="No distillations yet" />
			)}

			{distillations.length > 0 && (
				<div className="overflow-hidden rounded-lg border border-app-line/50">
					<table className="w-full text-sm">
						<thead>
							<tr className="border-b border-app-line/50 bg-app-darkBox/50">
								<th className="px-4 py-2.5 text-left text-tiny font-medium text-ink-faint">Type</th>
								<th className="px-4 py-2.5 text-left text-tiny font-medium text-ink-faint">Statement</th>
								<th className="w-28 px-4 py-2.5 text-left text-tiny font-medium text-ink-faint">
									Confidence
								</th>
								<th className="w-16 px-4 py-2.5 text-center text-tiny font-medium text-ink-faint">
									Retrieved
								</th>
								<th className="w-16 px-4 py-2.5 text-center text-tiny font-medium text-ink-faint">
									Used
								</th>
								<th className="w-16 px-4 py-2.5 text-center text-tiny font-medium text-ink-faint">
									Helped
								</th>
								<th className="w-16 px-4 py-2.5" />
							</tr>
						</thead>
						<tbody>
							{distillations.map((d, i) => (
								<tr
									key={d.id}
									className={`border-b border-app-line/30 ${i % 2 === 0 ? "" : "bg-app-darkBox/20"}`}
								>
									<td className="px-4 py-2.5">
										<span className="inline-flex rounded bg-app-darkBox px-1.5 py-0.5 text-tiny font-medium text-ink-dull">
											{d.distillation_type.replace(/_/g, " ")}
										</span>
									</td>
									<td className="max-w-xs px-4 py-2.5 text-ink-dull" title={d.statement}>
										{d.statement.length > 120
											? d.statement.slice(0, 120) + "…"
											: d.statement}
									</td>
									<td className="px-4 py-2.5">
										<ConfidenceBar value={d.confidence} />
									</td>
									<td className="px-4 py-2.5 text-center tabular-nums text-ink-faint">
										{d.retrieved_count}
									</td>
									<td className="px-4 py-2.5 text-center tabular-nums text-ink-faint">
										{d.used_count}
									</td>
									<td className="px-4 py-2.5 text-center tabular-nums text-ink-faint">
										{d.helped_count}
									</td>
									<td className="px-4 py-2.5 text-right">
										<button
											onClick={() => dismissMutation.mutate(d.id)}
											disabled={dismissMutation.isPending}
											className="rounded px-2 py-0.5 text-tiny text-ink-faint transition-colors hover:text-red-400 disabled:opacity-40"
										>
											Dismiss
										</button>
									</td>
								</tr>
							))}
						</tbody>
					</table>
				</div>
			)}
		</div>
	);
}

// ---------------------------------------------------------------------------
// Tab content: Episodes
// ---------------------------------------------------------------------------

function EpisodesTab({ agentId }: { agentId: string }) {
	const [expandedId, setExpandedId] = useState<string | null>(null);

	const { data, isLoading, isError } = useQuery({
		queryKey: ["learning-episodes", agentId],
		queryFn: () => learningApi.episodes(agentId),
		refetchInterval: 30_000,
	});

	const episodes = data?.episodes ?? [];

	return (
		<div className="flex flex-col gap-2">
			{isLoading && <LoadingSpinner />}
			{isError && <ErrorState message="Failed to load episodes" />}
			{!isLoading && !isError && episodes.length === 0 && (
				<EmptyState message="No episodes recorded yet" />
			)}

			{episodes.length > 0 && (
				<div className="overflow-hidden rounded-lg border border-app-line/50">
					<table className="w-full text-sm">
						<thead>
							<tr className="border-b border-app-line/50 bg-app-darkBox/50">
								<th className="px-4 py-2.5 text-left text-tiny font-medium text-ink-faint">Task</th>
								<th className="w-40 px-4 py-2.5 text-left text-tiny font-medium text-ink-faint">
									Predicted
								</th>
								<th className="w-40 px-4 py-2.5 text-left text-tiny font-medium text-ink-faint">
									Actual
								</th>
								<th className="w-24 px-4 py-2.5 text-left text-tiny font-medium text-ink-faint">
									Duration
								</th>
								<th className="w-28 px-4 py-2.5 text-left text-tiny font-medium text-ink-faint">
									Date
								</th>
							</tr>
						</thead>
						<tbody>
							{episodes.map((ep, i) => (
								<>
									<tr
										key={ep.id}
										onClick={() => setExpandedId(expandedId === ep.id ? null : ep.id)}
										className={`cursor-pointer border-b border-app-line/30 transition-colors hover:bg-app-darkBox/30 ${
											i % 2 === 0 ? "" : "bg-app-darkBox/20"
										}`}
									>
										<td className="max-w-xs px-4 py-2.5 text-ink-dull" title={ep.task}>
											{ep.task.length > 80 ? ep.task.slice(0, 80) + "…" : ep.task}
										</td>
										<td
											className="max-w-[160px] px-4 py-2.5 text-ink-faint"
											title={ep.predicted_outcome ?? undefined}
										>
											{ep.predicted_outcome
												? ep.predicted_outcome.length > 40
													? ep.predicted_outcome.slice(0, 40) + "…"
													: ep.predicted_outcome
												: "—"}
										</td>
										<td
											className="max-w-[160px] px-4 py-2.5 text-ink-faint"
											title={ep.actual_outcome ?? undefined}
										>
											{ep.actual_outcome
												? ep.actual_outcome.length > 40
													? ep.actual_outcome.slice(0, 40) + "…"
													: ep.actual_outcome
												: "—"}
										</td>
										<td className="px-4 py-2.5 tabular-nums text-ink-faint">
											{formatDurationMs(ep.duration_ms)}
										</td>
										<td className="px-4 py-2.5 text-tiny text-ink-faint">
											{formatTimeAgo(ep.created_at)}
										</td>
									</tr>

									<AnimatePresence>
										{expandedId === ep.id && ep.steps.length > 0 && (
											<tr key={`${ep.id}-steps`} className="border-b border-app-line/20">
												<td colSpan={5} className="p-0">
													<motion.div
														initial={{ height: 0, opacity: 0 }}
														animate={{ height: "auto", opacity: 1 }}
														exit={{ height: 0, opacity: 0 }}
														transition={{ type: "spring", stiffness: 500, damping: 35 }}
														className="overflow-hidden"
													>
														<div className="bg-app-darkBox/20 px-6 py-3">
															<p className="mb-2 text-tiny font-medium text-ink-faint">Steps</p>
															<div className="flex flex-col gap-1.5">
																{ep.steps.map((step) => (
																	<div key={step.step} className="flex items-start gap-3 text-sm">
																		<span className="mt-0.5 flex h-4 w-4 flex-shrink-0 items-center justify-center rounded-full bg-app-line text-tiny text-ink-faint">
																			{step.step}
																		</span>
																		<span className="text-ink-dull">{step.description}</span>
																		{step.outcome && (
																			<span className="ml-auto text-tiny text-ink-faint">
																				{step.outcome}
																			</span>
																		)}
																	</div>
																))}
															</div>
														</div>
													</motion.div>
												</td>
											</tr>
										)}
									</AnimatePresence>
								</>
							))}
						</tbody>
					</table>
				</div>
			)}
		</div>
	);
}

// ---------------------------------------------------------------------------
// Tab content: Insights
// ---------------------------------------------------------------------------

function InsightsTab({ agentId }: { agentId: string }) {
	const queryClient = useQueryClient();

	const { data, isLoading, isError } = useQuery({
		queryKey: ["learning-insights", agentId],
		queryFn: () => learningApi.insights(agentId),
		refetchInterval: 30_000,
	});

	const promoteMutation = useMutation({
		mutationFn: (id: string) => learningApi.promoteInsight(agentId, id),
		onSuccess: () =>
			queryClient.invalidateQueries({ queryKey: ["learning-insights", agentId] }),
	});

	const dismissMutation = useMutation({
		mutationFn: (id: string) => learningApi.dismissInsight(agentId, id),
		onSuccess: () =>
			queryClient.invalidateQueries({ queryKey: ["learning-insights", agentId] }),
	});

	if (isLoading) return <LoadingSpinner />;
	if (isError) return <ErrorState message="Failed to load insights" />;

	const insights = data?.insights ?? [];
	if (insights.length === 0) return <EmptyState message="No insights generated yet" />;

	// Group by category
	const grouped = insights.reduce<Record<string, LearningInsight[]>>((acc, insight) => {
		const cat = insight.category || "Uncategorised";
		if (!acc[cat]) acc[cat] = [];
		acc[cat].push(insight);
		return acc;
	}, {});

	return (
		<div className="flex flex-col gap-6">
			{Object.entries(grouped).map(([category, items]) => (
				<div key={category}>
					<h3 className="mb-3 text-tiny font-semibold uppercase tracking-wider text-ink-faint">
						{category}
					</h3>
					<div className="grid gap-3 sm:grid-cols-2">
						{items.map((insight) => (
							<div
								key={insight.id}
								className="rounded-lg border border-app-line/50 bg-app-darkBox/30 p-4"
							>
								<p className="mb-3 text-sm text-ink-dull">{insight.content}</p>

								<div className="mb-2 flex flex-col gap-1.5">
									<div className="flex items-center justify-between text-tiny text-ink-faint">
										<span>Reliability</span>
										<span>{Math.round(insight.reliability * 100)}%</span>
									</div>
									<ConfidenceBar
										value={insight.reliability}
										color={
											insight.reliability >= 0.75
												? "bg-green-500"
												: insight.reliability >= 0.5
												? "bg-yellow-500"
												: "bg-red-500"
										}
									/>
								</div>

								<div className="mb-3 flex items-center gap-4 text-tiny text-ink-faint">
									<span>
										Confidence{" "}
										<span className="text-ink-dull">
											{Math.round(insight.confidence * 100)}%
										</span>
									</span>
									<span>
										Validations{" "}
										<span className="text-ink-dull">{insight.validation_count}</span>
									</span>
								</div>

								<div className="flex items-center gap-2">
									<button
										onClick={() => promoteMutation.mutate(insight.id)}
										disabled={promoteMutation.isPending}
										className="rounded-md bg-accent/10 px-2.5 py-1 text-tiny font-medium text-accent transition-colors hover:bg-accent/20 disabled:opacity-40"
									>
										Promote
									</button>
									<button
										onClick={() => dismissMutation.mutate(insight.id)}
										disabled={dismissMutation.isPending}
										className="rounded-md px-2.5 py-1 text-tiny font-medium text-ink-faint transition-colors hover:text-red-400 disabled:opacity-40"
									>
										Dismiss
									</button>
									<span className="ml-auto text-tiny text-ink-faint">
										{formatTimeAgo(insight.created_at)}
									</span>
								</div>
							</div>
						))}
					</div>
				</div>
			))}
		</div>
	);
}

// ---------------------------------------------------------------------------
// Tab content: Chips
// ---------------------------------------------------------------------------

const CHIP_STATUS_COLORS: Record<ChipStatus, string> = {
	active: "bg-green-500/15 text-green-400",
	promoted: "bg-blue-500/15 text-blue-400",
	dismissed: "bg-gray-500/15 text-gray-400",
	quarantined: "bg-red-500/15 text-red-400",
};

function ChipsTab({ agentId }: { agentId: string }) {
	const { data, isLoading, isError } = useQuery({
		queryKey: ["learning-chips", agentId],
		queryFn: () => learningApi.chips(agentId),
		refetchInterval: 30_000,
	});

	if (isLoading) return <LoadingSpinner />;
	if (isError) return <ErrorState message="Failed to load chips" />;

	const chips = data?.chips ?? [];
	if (chips.length === 0) return <EmptyState message="No chips recorded yet" />;

	return (
		<div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
			{chips.map((chip) => (
				<div
					key={chip.chip_id}
					className="rounded-lg border border-app-line/50 bg-app-darkBox/30 p-4"
				>
					<div className="mb-3 flex items-start justify-between gap-2">
						<code className="break-all text-xs font-medium text-ink">{chip.chip_id}</code>
						<span
							className={`flex-shrink-0 rounded px-1.5 py-0.5 text-tiny font-medium ${CHIP_STATUS_COLORS[chip.status]}`}
						>
							{chip.status}
						</span>
					</div>

					<div className="mb-3 grid grid-cols-2 gap-2 text-sm">
						<div>
							<p className="text-tiny text-ink-faint">Observations</p>
							<p className="font-medium text-ink">{chip.observation_count}</p>
						</div>
						<div>
							<p className="text-tiny text-ink-faint">Success Rate</p>
							<p className="font-medium text-ink">
								{Math.round(chip.success_rate * 100)}%
							</p>
						</div>
					</div>

					<div>
						<div className="mb-1 flex items-center justify-between text-tiny text-ink-faint">
							<span>Confidence</span>
							<span>{Math.round(chip.confidence * 100)}%</span>
						</div>
						<ConfidenceBar
							value={chip.confidence}
							color={
								chip.confidence >= 0.75
									? "bg-blue-500"
									: chip.confidence >= 0.5
									? "bg-yellow-500"
									: "bg-red-500"
							}
						/>
					</div>

					<p className="mt-3 text-tiny text-ink-faint">{formatTimeAgo(chip.created_at)}</p>
				</div>
			))}
		</div>
	);
}

// ---------------------------------------------------------------------------
// Tab content: Quarantine
// ---------------------------------------------------------------------------

function QuarantineTab({ agentId }: { agentId: string }) {
	const { data, isLoading, isError } = useQuery({
		queryKey: ["learning-quarantine", agentId],
		queryFn: () => learningApi.quarantine(agentId),
		refetchInterval: 30_000,
	});

	if (isLoading) return <LoadingSpinner />;
	if (isError) return <ErrorState message="Failed to load quarantine entries" />;

	const entries = data?.entries ?? [];
	if (entries.length === 0) return <EmptyState message="Quarantine is empty" />;

	return (
		<div className="overflow-hidden rounded-lg border border-app-line/50">
			<table className="w-full text-sm">
				<thead>
					<tr className="border-b border-app-line/50 bg-app-darkBox/50">
						<th className="px-4 py-2.5 text-left text-tiny font-medium text-ink-faint">Source</th>
						<th className="w-32 px-4 py-2.5 text-left text-tiny font-medium text-ink-faint">Stage</th>
						<th className="px-4 py-2.5 text-left text-tiny font-medium text-ink-faint">Reason</th>
						<th className="w-28 px-4 py-2.5 text-left text-tiny font-medium text-ink-faint">Date</th>
					</tr>
				</thead>
				<tbody>
					{entries.map((entry, i) => (
						<tr
							key={entry.id}
							className={`border-b border-app-line/30 ${i % 2 === 0 ? "" : "bg-app-darkBox/20"}`}
						>
							<td className="max-w-[200px] px-4 py-2.5 font-mono text-xs text-ink-dull">
								{entry.source}
							</td>
							<td className="px-4 py-2.5">
								<span className="rounded bg-red-500/10 px-1.5 py-0.5 text-tiny text-red-400">
									{entry.stage}
								</span>
							</td>
							<td className="px-4 py-2.5 text-ink-faint">{entry.reason}</td>
							<td className="px-4 py-2.5 text-tiny text-ink-faint">
								{formatTimeAgo(entry.created_at)}
							</td>
						</tr>
					))}
				</tbody>
			</table>
		</div>
	);
}

// ---------------------------------------------------------------------------
// Tab content: Tuneables
// ---------------------------------------------------------------------------

function TuneablesTab({ agentId }: { agentId: string }) {
	const queryClient = useQueryClient();
	const [editingKey, setEditingKey] = useState<string | null>(null);
	const [editValue, setEditValue] = useState<string>("");

	const { data, isLoading, isError } = useQuery({
		queryKey: ["learning-tuneables", agentId],
		queryFn: () => learningApi.tuneables(agentId),
	});

	const updateMutation = useMutation({
		mutationFn: ({ key, value }: { key: string; value: string | number | boolean }) =>
			learningApi.updateTuneable(agentId, key, value),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["learning-tuneables", agentId] });
			setEditingKey(null);
		},
	});

	if (isLoading) return <LoadingSpinner />;
	if (isError) return <ErrorState message="Failed to load tuneables" />;

	const tuneables = data?.tuneables ?? [];
	if (tuneables.length === 0) return <EmptyState message="No tuneables available" />;

	const startEdit = (tuneable: Tuneable) => {
		setEditingKey(tuneable.key);
		setEditValue(String(tuneable.value));
	};

	const saveEdit = (key: string, originalType: typeof tuneables[number]["value"]) => {
		let parsed: string | number | boolean = editValue;
		if (typeof originalType === "boolean") {
			parsed = editValue === "true";
		} else if (typeof originalType === "number") {
			const n = Number(editValue);
			if (!isNaN(n)) parsed = n;
		}
		updateMutation.mutate({ key, value: parsed });
	};

	return (
		<div className="overflow-hidden rounded-lg border border-app-line/50">
			<table className="w-full text-sm">
				<thead>
					<tr className="border-b border-app-line/50 bg-app-darkBox/50">
						<th className="px-4 py-2.5 text-left text-tiny font-medium text-ink-faint">Key</th>
						<th className="w-56 px-4 py-2.5 text-left text-tiny font-medium text-ink-faint">
							Value
						</th>
						<th className="px-4 py-2.5 text-left text-tiny font-medium text-ink-faint">
							Description
						</th>
						<th className="w-20 px-4 py-2.5" />
					</tr>
				</thead>
				<tbody>
					{tuneables.map((tuneable, i) => (
						<tr
							key={tuneable.key}
							className={`border-b border-app-line/30 ${i % 2 === 0 ? "" : "bg-app-darkBox/20"}`}
						>
							<td className="px-4 py-2.5 font-mono text-xs text-ink">{tuneable.key}</td>
							<td className="px-4 py-2.5">
								{editingKey === tuneable.key ? (
									<div className="flex items-center gap-2">
										<input
											autoFocus
											value={editValue}
											onChange={(e) => setEditValue(e.target.value)}
											onKeyDown={(e) => {
												if (e.key === "Enter") saveEdit(tuneable.key, tuneable.value);
												if (e.key === "Escape") setEditingKey(null);
											}}
											className="w-full rounded border border-app-line bg-app px-2 py-1 font-mono text-xs text-ink focus:border-accent focus:outline-none"
										/>
									</div>
								) : (
									<span
										className={`font-mono text-xs ${
											typeof tuneable.value === "boolean"
												? tuneable.value
													? "text-green-400"
													: "text-red-400"
												: "text-ink-dull"
										}`}
									>
										{String(tuneable.value)}
									</span>
								)}
							</td>
							<td className="px-4 py-2.5 text-ink-faint">
								{tuneable.description ?? "—"}
							</td>
							<td className="px-4 py-2.5 text-right">
								{editingKey === tuneable.key ? (
									<div className="flex items-center justify-end gap-1">
										<button
											onClick={() => saveEdit(tuneable.key, tuneable.value)}
											disabled={updateMutation.isPending}
											className="rounded px-2 py-0.5 text-tiny text-accent transition-colors hover:text-accent/80 disabled:opacity-40"
										>
											Save
										</button>
										<button
											onClick={() => setEditingKey(null)}
											className="rounded px-2 py-0.5 text-tiny text-ink-faint transition-colors hover:text-ink-dull"
										>
											Cancel
										</button>
									</div>
								) : (
									<button
										onClick={() => startEdit(tuneable)}
										className="rounded px-2 py-0.5 text-tiny text-ink-faint transition-colors hover:text-ink-dull"
									>
										Edit
									</button>
								)}
							</td>
						</tr>
					))}
				</tbody>
			</table>
		</div>
	);
}

// ---------------------------------------------------------------------------
// Main component
// ---------------------------------------------------------------------------

export function AgentLearning({ agentId }: { agentId: string }) {
	const [activeTab, setActiveTab] = useState<TabId>("overview");

	return (
		<div className="flex h-full flex-col overflow-hidden">
			{/* Tab bar */}
			<div className="flex h-10 flex-shrink-0 items-stretch border-b border-app-line/50 px-4">
				{TABS.map((tab) => (
					<button
						key={tab.id}
						onClick={() => setActiveTab(tab.id)}
						className={`relative flex items-center px-3 text-sm transition-colors ${
							activeTab === tab.id
								? "text-ink"
								: "text-ink-faint hover:text-ink-dull"
						}`}
					>
						{tab.label}
						{activeTab === tab.id && (
							<motion.span
								layoutId="learning-tab-indicator"
								className="absolute bottom-0 left-0 right-0 h-0.5 rounded-full bg-accent"
								transition={{ type: "spring", stiffness: 500, damping: 35 }}
							/>
						)}
					</button>
				))}
			</div>

			{/* Content */}
			<div className="flex-1 overflow-auto p-6">
				<AnimatePresence mode="wait">
					<motion.div
						key={activeTab}
						initial={{ opacity: 0, y: 4 }}
						animate={{ opacity: 1, y: 0 }}
						exit={{ opacity: 0, y: -4 }}
						transition={{ duration: 0.12 }}
					>
						{activeTab === "overview" && <OverviewTab agentId={agentId} />}
						{activeTab === "distillations" && <DistillationsTab agentId={agentId} />}
						{activeTab === "episodes" && <EpisodesTab agentId={agentId} />}
						{activeTab === "insights" && <InsightsTab agentId={agentId} />}
						{activeTab === "chips" && <ChipsTab agentId={agentId} />}
						{activeTab === "quarantine" && <QuarantineTab agentId={agentId} />}
						{activeTab === "tuneables" && <TuneablesTab agentId={agentId} />}
					</motion.div>
				</AnimatePresence>
			</div>
		</div>
	);
}
