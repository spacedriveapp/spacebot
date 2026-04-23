// Default right-panel content for the mermaid view — shown when no node
// is selected. Stats grid + relationship-group breakdown + node-type
// distribution bars + top connected files.

import { useMemo } from "react";
import { NODE_COLORS, type NodeLabel } from "./constants";
import type { BuildResult } from "./mermaidGraphBuilder";
import type { BulkNode } from "./types";

interface Props {
	projectName: string;
	graph: BuildResult;
	allNodes: BulkNode[];
	edgeCount: number;
	onSelectFile?: (file: BulkNode) => void;
}

interface Stat {
	label: string;
	value: string | number;
}

function formatCount(n: number): string {
	return n.toLocaleString();
}

export function ProjectOverviewCard({ projectName, graph, allNodes, edgeCount, onSelectFile }: Props) {
	const { fileNodes, fileDegree, layers } = graph;

	const stats: Stat[] = useMemo(() => [
		{ label: "Files", value: formatCount(fileNodes.length) },
		{ label: "Edges", value: formatCount(edgeCount) },
		{ label: "Layers", value: formatCount(layers.length) },
		{ label: "Nodes", value: formatCount(allNodes.length) },
	], [fileNodes.length, edgeCount, layers.length, allNodes.length]);

	const typeDistribution = useMemo(() => {
		const counts = new Map<NodeLabel, number>();
		for (const node of allNodes) {
			const label = node.label as NodeLabel;
			counts.set(label, (counts.get(label) ?? 0) + 1);
		}
		const total = allNodes.length || 1;
		return Array.from(counts.entries())
			.sort((a, b) => b[1] - a[1])
			.slice(0, 8)
			.map(([label, count]) => ({
				label,
				count,
				pct: Math.round((count / total) * 100),
			}));
	}, [allNodes]);

	const topConnected = useMemo(() => {
		return fileNodes
			.map((file) => ({ file, degree: fileDegree.get(file.qualified_name) ?? 0 }))
			.sort((a, b) => b.degree - a.degree)
			.slice(0, 5)
			.filter((entry) => entry.degree > 0);
	}, [fileNodes, fileDegree]);

	return (
		<div className="flex h-full flex-col overflow-hidden bg-app-darkBox text-ink">
			<div className="border-b border-app-line px-5 py-4">
				<div className="text-[10px] font-semibold uppercase tracking-wider text-ink-faint">
					Project Overview
				</div>
				<h2 className="mt-0.5 text-lg font-semibold text-ink" title={projectName}>
					{projectName}
				</h2>
				<p className="mt-1 text-xs leading-relaxed text-ink-dull">
					Files are organized into layers by top-level folder. Click a
					layer to drill into its files, click a file to focus its
					connections, and recolor any card from its palette swatch.
				</p>
			</div>

			<div className="flex-1 overflow-y-auto px-5 py-4">
				<div className="grid grid-cols-2 gap-2">
					{stats.map((stat) => (
						<div key={stat.label} className="rounded-lg border border-app-line bg-app p-3">
							<div className="font-mono text-xl font-semibold text-accent">{stat.value}</div>
							<div className="mt-0.5 text-[10px] uppercase tracking-wider text-ink-faint">{stat.label}</div>
						</div>
					))}
				</div>

				{typeDistribution.length > 0 && (
					<section className="mt-6">
						<h3 className="mb-2 text-[10px] font-semibold uppercase tracking-wider text-ink-faint">
							Node Types
						</h3>
						<ul className="space-y-1.5">
							{typeDistribution.map(({ label, count, pct }) => (
								<li key={label}>
									<div className="flex items-center justify-between text-[11px]">
										<span className="text-ink">{label}</span>
										<span className="font-mono text-ink-faint">{count.toLocaleString()} ({pct}%)</span>
									</div>
									<div className="mt-0.5 h-1 overflow-hidden rounded-full bg-app">
										<div
											className="h-full rounded-full"
											style={{
												width: `${Math.max(pct, 2)}%`,
												background: NODE_COLORS[label] ?? "#64748b",
											}}
										/>
									</div>
								</li>
							))}
						</ul>
					</section>
				)}

				{topConnected.length > 0 && (
					<section className="mt-6">
						<h3 className="mb-2 text-[10px] font-semibold uppercase tracking-wider text-ink-faint">
							Most Connected Files
						</h3>
						<ol className="space-y-1">
							{topConnected.map(({ file, degree }, idx) => (
								<li key={file.qualified_name}>
									<button
										type="button"
										onClick={() => onSelectFile?.(file)}
										className="flex w-full items-center gap-2 rounded-md border border-transparent px-2 py-1.5 text-left text-[11px] text-ink-dull transition-colors hover:border-app-line hover:bg-app hover:text-ink"
									>
										<span className="flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-accent/20 font-mono text-[10px] text-accent">
											{idx + 1}
										</span>
										<span className="flex-1 truncate" title={file.source_file ?? file.name}>
											{file.name}
										</span>
										<span className="font-mono text-ink-faint">{degree}</span>
									</button>
								</li>
							))}
						</ol>
					</section>
				)}
			</div>
		</div>
	);
}
