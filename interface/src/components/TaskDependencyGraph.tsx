import { useEffect, useRef, useCallback, useState } from "react";
import Graph from "graphology";
import Sigma from "sigma";
import { EdgeArrowProgram } from "sigma/rendering";
import type { TaskItem } from "@/api/client";

const STATUS_COLORS: Record<string, string> = {
	pending_approval: "#f59e0b",
	backlog: "#6b7280",
	ready: "#3b82f6",
	in_progress: "#a855f7",
	done: "#22c55e",
};

const PRIORITY_SIZES: Record<string, number> = {
	critical: 14,
	high: 11,
	medium: 8,
	low: 6,
};

interface TaskDependencyGraphProps {
	tasks: TaskItem[];
	onSelectTask?: (taskNumber: number) => void;
}

export function TaskDependencyGraph({
	tasks,
	onSelectTask,
}: TaskDependencyGraphProps) {
	const containerRef = useRef<HTMLDivElement>(null);
	const sigmaRef = useRef<Sigma | null>(null);
	const graphRef = useRef<Graph | null>(null);
	const [hoveredNode, setHoveredNode] = useState<string | null>(null);

	const buildGraph = useCallback(() => {
		const graph = new Graph({ type: "directed" });

		// Add nodes
		for (const task of tasks) {
			const key = String(task.task_number);
			const deps = getDeps(task);
			const isBlocked =
				deps.length > 0 &&
				deps.some((d) => {
					const dt = tasks.find((t) => t.task_number === d);
					return dt && dt.status !== "done";
				});

			graph.addNode(key, {
				label: `#${task.task_number} ${task.title.slice(0, 25)}${task.title.length > 25 ? "…" : ""}`,
				size: PRIORITY_SIZES[task.priority] ?? 8,
				color: isBlocked ? "#ef4444" : (STATUS_COLORS[task.status] ?? "#6b7280"),
				x: Math.random() * 10,
				y: Math.random() * 10,
			});
		}

		// Add edges (dependency → task)
		for (const task of tasks) {
			const deps = getDeps(task);
			for (const depNum of deps) {
				const depKey = String(depNum);
				const taskKey = String(task.task_number);
				if (graph.hasNode(depKey) && graph.hasNode(taskKey)) {
					const edgeKey = `${depKey}->${taskKey}`;
					if (!graph.hasEdge(edgeKey)) {
						graph.addEdgeWithKey(edgeKey, depKey, taskKey, {
							color: "#555555",
							size: 2,
						});
					}
				}
			}
		}

		return graph;
	}, [tasks]);

	// Simple force-directed layout (no worker needed for small graphs)
	const applyLayout = useCallback((graph: Graph) => {
		const nodes = graph.nodes();
		const positions: Record<string, { x: number; y: number }> = {};

		// Initialize positions
		for (const node of nodes) {
			positions[node] = {
				x: graph.getNodeAttribute(node, "x"),
				y: graph.getNodeAttribute(node, "y"),
			};
		}

		// Simple force iterations
		for (let iter = 0; iter < 100; iter++) {
			// Repulsion between all nodes
			for (let i = 0; i < nodes.length; i++) {
				for (let j = i + 1; j < nodes.length; j++) {
					const a = positions[nodes[i]];
					const b = positions[nodes[j]];
					const dx = b.x - a.x;
					const dy = b.y - a.y;
					const dist = Math.max(Math.sqrt(dx * dx + dy * dy), 0.1);
					const force = 2 / (dist * dist);
					const fx = (dx / dist) * force;
					const fy = (dy / dist) * force;
					a.x -= fx;
					a.y -= fy;
					b.x += fx;
					b.y += fy;
				}
			}

			// Attraction along edges
			graph.forEachEdge((_edge, _attrs, source, target) => {
				const a = positions[source];
				const b = positions[target];
				const dx = b.x - a.x;
				const dy = b.y - a.y;
				const dist = Math.sqrt(dx * dx + dy * dy);
				const force = dist * 0.05;
				const fx = (dx / Math.max(dist, 0.1)) * force;
				const fy = (dy / Math.max(dist, 0.1)) * force;
				a.x += fx;
				a.y += fy;
				b.x -= fx;
				b.y -= fy;
			});
		}

		// Apply positions
		for (const node of nodes) {
			graph.setNodeAttribute(node, "x", positions[node].x);
			graph.setNodeAttribute(node, "y", positions[node].y);
		}
	}, []);

	useEffect(() => {
		if (!containerRef.current) return;

		// Only show graph if there are dependencies
		const hasDeps = tasks.some((t) => getDeps(t).length > 0);
		if (!hasDeps) return;

		const graph = buildGraph();
		applyLayout(graph);
		graphRef.current = graph;

		const sigma = new Sigma(graph, containerRef.current, {
			defaultEdgeType: "arrow",
			edgeProgramClasses: { arrow: EdgeArrowProgram },
			labelColor: { color: "#e0e0e0" },
			labelFont: "JetBrains Mono, monospace",
			labelSize: 11,
			labelRenderedSizeThreshold: 4,
			defaultNodeColor: "#6b7280",
			defaultEdgeColor: "#555555",
			stagePadding: 40,
		});

		sigma.on("clickNode", ({ node }) => {
			onSelectTask?.(parseInt(node, 10));
		});

		sigma.on("enterNode", ({ node }) => setHoveredNode(node));
		sigma.on("leaveNode", () => setHoveredNode(null));

		sigmaRef.current = sigma;

		return () => {
			sigma.kill();
			sigmaRef.current = null;
			graphRef.current = null;
		};
	}, [tasks, buildGraph, applyLayout, onSelectTask]);

	const hasDeps = tasks.some((t) => getDeps(t).length > 0);

	if (!hasDeps) {
		return (
			<div className="flex h-full items-center justify-center text-sm text-ink-faint">
				No task dependencies to visualize
			</div>
		);
	}

	return (
		<div className="relative h-full w-full">
			<div ref={containerRef} className="h-full w-full" />
			{/* Legend */}
			<div className="absolute bottom-3 left-3 flex flex-wrap gap-2 rounded-md bg-app/80 px-3 py-2 backdrop-blur-sm">
				{Object.entries(STATUS_COLORS).map(([status, color]) => (
					<div key={status} className="flex items-center gap-1">
						<span
							className="inline-block h-2.5 w-2.5 rounded-full"
							style={{ backgroundColor: color }}
						/>
						<span className="text-tiny text-ink-faint">
							{status.replace("_", " ")}
						</span>
					</div>
				))}
				<div className="flex items-center gap-1">
					<span className="inline-block h-2.5 w-2.5 rounded-full bg-red-500" />
					<span className="text-tiny text-ink-faint">blocked</span>
				</div>
			</div>
			{/* Hover tooltip */}
			{hoveredNode && (
				<div className="absolute right-3 top-3 rounded-md bg-app-box/90 px-3 py-2 text-xs text-ink backdrop-blur-sm">
					Task #{hoveredNode}
				</div>
			)}
		</div>
	);
}

function getDeps(task: TaskItem): number[] {
	const deps = task.metadata?.depends_on;
	if (Array.isArray(deps))
		return deps.filter((n): n is number => typeof n === "number");
	return [];
}
