import {useState} from "react";
import {useQuery} from "@tanstack/react-query";
import {Card, CardHeader, CardContent, Button} from "@spacedrive/primitives";
import {api, type UsageByModel} from "@/api/client";

function formatTokens(n: number): string {
	if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
	if (n >= 1_000) return `${(n / 1_000).toFixed(0)}K`;
	return String(n);
}

function formatCost(usd: number): string {
	if (usd >= 1) return `$${usd.toFixed(2)}`;
	if (usd >= 0.01) return `$${usd.toFixed(3)}`;
	return `$${usd.toFixed(4)}`;
}

const MODEL_COLORS: Record<string, string> = {
	"claude-opus": "#e11d48",
	"claude-sonnet": "#f59e0b",
	"claude-haiku": "#8b5cf6",
	"gpt-4o": "#3b82f6",
	"gpt-4o-mini": "#06b6d4",
	o3: "#10b981",
	gemini: "#f97316",
	deepseek: "#64748b",
};

function colorForModel(model: string): string {
	for (const [key, color] of Object.entries(MODEL_COLORS)) {
		if (model.includes(key)) return color;
	}
	return "#94a3b8";
}

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

export function TokenUsageCard() {
	const [period, setPeriod] = useState<Period>("7d");

	const {data, isLoading} = useQuery({
		queryKey: ["usage", period],
		queryFn: () => api.usage({since: sinceDate(period), group_by: "model"}),
		staleTime: 60_000,
	});

	const total = data?.total;
	const models = (data?.by_model ?? []).slice(0, 3);
	const totalTokens = total
		? total.input_tokens +
			total.output_tokens +
			total.cache_read_tokens +
			total.cache_write_tokens
		: 0;

	return (
		<Card variant="dark" className="flex h-full flex-col">
			<CardHeader className="flex-row items-center justify-between p-4 pb-3">
				<h2 className="font-plex text-sm font-medium text-ink-dull">
					Token Usage
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

			<CardContent className="flex flex-1 flex-col px-6 pb-4 pt-0">
				{isLoading || !total ? (
					<div className="flex flex-1 items-center justify-center">
						<span className="text-sm text-ink-faint">Loading...</span>
					</div>
				) : total.request_count === 0 ? (
					<div className="flex flex-1 items-center justify-center">
						<p className="text-sm text-ink-faint">
							No usage recorded in this period.
						</p>
					</div>
				) : (
					<>
						{/* Cost + total tokens */}
						<div className="mb-3 flex items-baseline gap-3">
							{total.estimated_cost_usd != null && (
								<div>
									<span className="font-plex text-2xl font-semibold tabular-nums text-ink">
										{formatCost(total.estimated_cost_usd)}
									</span>
									<span
										className="ml-1 cursor-help text-tiny text-ink-faint"
										title="Estimated from published model pricing. May differ from your actual bill."
									>
										est.
									</span>
								</div>
							)}
							<div>
								<span className="font-plex text-lg font-medium tabular-nums text-ink-dull">
									{formatTokens(totalTokens)}
								</span>
								<span className="ml-1 text-tiny text-ink-faint">tokens</span>
							</div>
						</div>

						{/* Token breakdown */}
						<div className="mb-3 grid grid-cols-2 gap-x-4 gap-y-1 text-tiny">
							<TokenRow label="Input" value={total.input_tokens} />
							<TokenRow label="Output" value={total.output_tokens} />
							<TokenRow label="Cache read" value={total.cache_read_tokens} />
							<TokenRow label="Cache write" value={total.cache_write_tokens} />
							{total.reasoning_tokens > 0 && (
								<TokenRow label="Reasoning" value={total.reasoning_tokens} />
							)}
						</div>

						{/* Model breakdown — always shown, top 3 */}
						{models.length > 0 && (
							<div className="mt-auto flex flex-col gap-2.5">
								{models.map((m) => (
									<ModelBar key={m.model} model={m} totalTokens={totalTokens} />
								))}
							</div>
						)}

						{/* Unknown pricing note */}
						{total.cost_status === "unknown" && (
							<p className="mt-2 text-tiny text-ink-faint">
								Some usage has unknown pricing and is excluded from the cost
								total.
							</p>
						)}
					</>
				)}
			</CardContent>
		</Card>
	);
}

function TokenRow({label, value}: {label: string; value: number}) {
	if (value === 0) return null;
	return (
		<>
			<span className="text-ink-faint">{label}</span>
			<span className="tabular-nums text-ink-dull text-right">
				{formatTokens(value)}
			</span>
		</>
	);
}

function ModelBar({
	model,
	totalTokens,
}: {
	model: UsageByModel;
	totalTokens: number;
}) {
	const modelTokens =
		model.input_tokens +
		model.output_tokens +
		model.cache_read_tokens +
		model.cache_write_tokens;
	const pct = totalTokens > 0 ? (modelTokens / totalTokens) * 100 : 0;
	const color = colorForModel(model.model);

	const displayName = model.model.includes("/")
		? model.model.split("/").pop()!
		: model.model;

	return (
		<div>
			<div className="mb-1 flex items-center justify-between">
				<span className="text-tiny text-ink-dull">{displayName}</span>
				<span className="tabular-nums text-tiny text-ink-faint">
					{formatTokens(modelTokens)}
					{model.estimated_cost_usd != null && (
						<span className="ml-1.5">
							{formatCost(model.estimated_cost_usd)}
						</span>
					)}
				</span>
			</div>
			<div className="h-1.5 w-full overflow-hidden rounded-full bg-app-selected/40">
				<div
					className="h-full rounded-full transition-all duration-500"
					style={{width: `${pct}%`, backgroundColor: color}}
				/>
			</div>
		</div>
	);
}
