import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import Graph from "graphology";
import Sigma from "sigma";
import { EdgeArrowProgram } from "sigma/rendering";
import FA2Layout from "graphology-layout-forceatlas2/worker";
import { AnimatePresence, motion } from "framer-motion";
import {
	api,
	type AssociationItem,
	type MemoryItem,
	type MemorySort,
	type MemoryType,
	type RelationType,
} from "@/api/client";
import { formatTimeAgo } from "@/lib/format";
import { Button } from "@/ui";
import { Target02Icon, PlusSignIcon, MinusSignIcon, RefreshIcon, Cancel01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";

// -- Constants --

const NODE_COLORS: Record<MemoryType, string> = {
	fact: "#3b82f6",
	preference: "#ec4899",
	decision: "#f59e0b",
	identity: "#a855f7",
	event: "#22c55e",
	observation: "#06b6d4",
	goal: "#f97316",
	todo: "#ef4444",
};

const EDGE_COLORS: Record<RelationType, string> = {
	related_to: "#555555",
	updates: "#4ade80",
	contradicts: "#f87171",
	caused_by: "#fb923c",
	result_of: "#fb923c",
	part_of: "#60a5fa",
};

const FADED_NODE_COLOR = "#333333";

interface MemoryGraphProps {
	agentId: string;
	sort: MemorySort;
	typeFilter: MemoryType | null;
	refreshToken?: number;
	onEditMemory?: (memory: MemoryItem) => void;
	onDeleteMemory?: (memory: MemoryItem) => void;
}

interface NodeDetail {
	memory: MemoryItem;
	x: number;
	y: number;
}

export function MemoryGraph({
	agentId,
	sort,
	typeFilter,
	refreshToken,
	onEditMemory,
	onDeleteMemory,
}: MemoryGraphProps) {
	const containerRef = useRef<HTMLDivElement>(null);
	const sigmaRef = useRef<Sigma | null>(null);
	const graphRef = useRef<Graph | null>(null);
	const layoutRef = useRef<FA2Layout | null>(null);
	const [isLoading, setIsLoading] = useState(true);
	const [error, setError] = useState<string | null>(null);
	const [nodeCount, setNodeCount] = useState(0);
	const [edgeCount, setEdgeCount] = useState(0);
	const [selectedNode, setSelectedNode] = useState<NodeDetail | null>(null);
	const [hoveredNode, setHoveredNode] = useState<string | null>(null);
	const [expandingNode, setExpandingNode] = useState<string | null>(null);
	// Track loaded node IDs so we can exclude them when fetching neighbors
	const loadedNodeIds = useRef<Set<string>>(new Set());

	// Memoize the graph data key to refetch when filters change
	const queryKey = useMemo(
		() => [agentId, sort, typeFilter, refreshToken],
		[agentId, sort, typeFilter, refreshToken],
	);

	const cleanup = useCallback(() => {
		if (layoutRef.current) {
			layoutRef.current.kill();
			layoutRef.current = null;
		}
		if (sigmaRef.current) {
			sigmaRef.current.kill();
			sigmaRef.current = null;
		}
		graphRef.current = null;
		loadedNodeIds.current.clear();
	}, []);

	// Load graph data and initialize Sigma
	useEffect(() => {
		if (!containerRef.current) return;

		let cancelled = false;

		async function loadGraph() {
			setIsLoading(true);
			setError(null);
			setSelectedNode(null);
			cleanup();

			try {
				const data = await api.memoryGraph(agentId, {
					limit: 300,
					sort,
					memory_type: typeFilter ?? undefined,
				});

				if (cancelled) return;

				if (data.nodes.length === 0) {
					setIsLoading(false);
					setNodeCount(0);
					setEdgeCount(0);
					return;
				}

				const graph = new Graph({ multi: false, type: "directed" });
				graphRef.current = graph;

				// Add nodes
				for (const node of data.nodes) {
					const size = 3 + node.importance * 8;
					graph.addNode(node.id, {
						label: truncateLabel(node.content),
						size,
						color: NODE_COLORS[node.memory_type] ?? "#666666",
						x: Math.random() * 100,
						y: Math.random() * 100,
						// Store full memory data on the node for later retrieval
						memoryData: node,
					});
					loadedNodeIds.current.add(node.id);
				}

				// Add edges
				addEdgesToGraph(graph, data.edges);

				setNodeCount(graph.order);
				setEdgeCount(graph.size);

				// Initialize Sigma
				if (!containerRef.current || cancelled) return;

				const sigma = new Sigma(graph, containerRef.current, {
					allowInvalidContainer: true,
					renderLabels: true,
					labelRenderedSizeThreshold: 12,
					labelSize: 10,
					labelColor: { color: "#999999" },
					defaultEdgeType: "arrow",
					defaultEdgeColor: "#444444",
					edgeLabelSize: 10,
					edgeProgramClasses: {
						arrow: EdgeArrowProgram,
					},
					nodeReducer: (node, data) => {
						const res = { ...data };
						if (hoveredNode && hoveredNode !== node) {
							const graph = graphRef.current;
							if (graph && !graph.hasEdge(hoveredNode, node) && !graph.hasEdge(node, hoveredNode)) {
								res.color = FADED_NODE_COLOR;
								res.label = "";
							}
						}
						return res;
					},
					edgeReducer: (_edge, data) => {
						return { ...data };
					},
				});

				sigmaRef.current = sigma;

				// Start ForceAtlas2 layout in a web worker
				const layout = new FA2Layout(graph, {
					settings: {
						gravity: 1,
						scalingRatio: 10,
						barnesHutOptimize: true,
						barnesHutTheta: 0.5,
						strongGravityMode: false,
						slowDown: 5,
					},
				});
				layoutRef.current = layout;
				layout.start();

				// Stop layout after settling
				setTimeout(() => {
					if (layout.isRunning()) {
						layout.stop();
					}
				}, 3000);

				setIsLoading(false);
			} catch (err) {
				if (!cancelled) {
					setError(err instanceof Error ? err.message : "Failed to load graph");
					setIsLoading(false);
				}
			}
		}

		loadGraph();

		return () => {
			cancelled = true;
			cleanup();
		};
	}, [queryKey, cleanup, agentId, sort, typeFilter]);

	// Register Sigma event handlers (separate from graph loading to avoid recreating)
	useEffect(() => {
		const sigma = sigmaRef.current;
		if (!sigma) return;

		function handleClickNode({ node }: { node: string }) {
			const graph = graphRef.current;
			const s = sigmaRef.current;
			if (!graph || !s) return;

			const attrs = graph.getNodeAttributes(node);
			const memory = attrs.memoryData as MemoryItem | undefined;
			if (!memory) return;

			// Get screen coordinates for the detail panel
			const position = s.graphToViewport({ x: attrs.x, y: attrs.y });
			setSelectedNode({
				memory,
				x: position.x,
				y: position.y,
			});
		}

		function handleDoubleClickNode({ node, event }: { node: string; event: { preventSigmaDefault: () => void } }) {
			event.preventSigmaDefault();
			expandNeighbors(node);
		}

		function handleEnterNode({ node }: { node: string }) {
			setHoveredNode(node);
			if (sigmaRef.current) {
				sigmaRef.current.getContainer().style.cursor = "pointer";
			}
		}

		function handleLeaveNode() {
			setHoveredNode(null);
			if (sigmaRef.current) {
				sigmaRef.current.getContainer().style.cursor = "default";
			}
		}

		function handleClickStage() {
			setSelectedNode(null);
		}

		sigma.on("clickNode", handleClickNode);
		sigma.on("doubleClickNode", handleDoubleClickNode);
		sigma.on("enterNode", handleEnterNode);
		sigma.on("leaveNode", handleLeaveNode);
		sigma.on("clickStage", handleClickStage);

		return () => {
			sigma.off("clickNode", handleClickNode);
			sigma.off("doubleClickNode", handleDoubleClickNode);
			sigma.off("enterNode", handleEnterNode);
			sigma.off("leaveNode", handleLeaveNode);
			sigma.off("clickStage", handleClickStage);
		};
	}, [isLoading]);

	// Re-render sigma when hoveredNode changes (for fade effect)
	useEffect(() => {
		sigmaRef.current?.refresh();
	}, [hoveredNode]);

	async function expandNeighbors(nodeId: string) {
		if (expandingNode) return;
		setExpandingNode(nodeId);

		try {
			const exclude = Array.from(loadedNodeIds.current);
			const data = await api.memoryGraphNeighbors(agentId, nodeId, {
				depth: 1,
				exclude,
			});

			const graph = graphRef.current;
			if (!graph) return;

			// Add new nodes
			for (const node of data.nodes) {
				if (!graph.hasNode(node.id)) {
					const parentAttrs = graph.getNodeAttributes(nodeId);
					const size = 3 + node.importance * 8;
					graph.addNode(node.id, {
						label: truncateLabel(node.content),
						size,
						color: NODE_COLORS[node.memory_type] ?? "#666666",
						x: (parentAttrs.x as number) + (Math.random() - 0.5) * 20,
						y: (parentAttrs.y as number) + (Math.random() - 0.5) * 20,
						memoryData: node,
					});
					loadedNodeIds.current.add(node.id);
				}
			}

			// Add new edges
			addEdgesToGraph(graph, data.edges);

			setNodeCount(graph.order);
			setEdgeCount(graph.size);

			// Restart layout briefly to settle new nodes
			const layout = layoutRef.current;
			if (layout && !layout.isRunning()) {
				layout.start();
				setTimeout(() => {
					if (layout.isRunning()) layout.stop();
				}, 2000);
			}

			sigmaRef.current?.refresh();
		} catch (err) {
			console.error("Failed to expand neighbors:", err);
		} finally {
			setExpandingNode(null);
		}
	}

	return (
		<div className="relative h-full w-full">
			{/* Stats bar */}
			<div className="absolute left-4 top-4 z-10 flex items-center gap-3 rounded-md bg-app-darkBox/80 px-3 py-1.5 text-tiny text-ink-faint backdrop-blur-sm">
				<span>{nodeCount} nodes</span>
				<span className="text-app-line">|</span>
				<span>{edgeCount} edges</span>
				{expandingNode && (
					<>
						<span className="text-app-line">|</span>
						<span className="flex items-center gap-1.5">
							<span className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
							expanding...
						</span>
					</>
				)}
			</div>

			{/* Legend */}
			<div className="absolute bottom-4 left-4 z-10 rounded-md bg-app-darkBox/80 p-3 backdrop-blur-sm">
				<div className="mb-2 text-tiny font-medium text-ink-faint">Node Types</div>
				<div className="grid grid-cols-2 gap-x-4 gap-y-1">
					{(Object.entries(NODE_COLORS) as [MemoryType, string][]).map(([type, color]) => (
						<div key={type} className="flex items-center gap-1.5">
							<span
								className="inline-block h-2 w-2 rounded-full"
								style={{ backgroundColor: color }}
							/>
							<span className="text-tiny text-ink-faint">{type}</span>
						</div>
					))}
				</div>
				<div className="mt-2 border-t border-app-line/30 pt-2">
					<div className="mb-1 text-tiny font-medium text-ink-faint">Edge Types</div>
					<div className="grid grid-cols-2 gap-x-4 gap-y-1">
						{(Object.entries(EDGE_COLORS) as [RelationType, string][]).map(([type, color]) => (
							<div key={type} className="flex items-center gap-1.5">
								<span
									className="inline-block h-0.5 w-3"
									style={{
										backgroundColor: color,
										borderStyle: type === "contradicts" ? "dashed" : "solid",
									}}
								/>
								<span className="text-tiny text-ink-faint">{type.replace("_", " ")}</span>
							</div>
						))}
					</div>
				</div>
				<div className="mt-2 border-t border-app-line/30 pt-2 text-tiny text-ink-faint">
					Double-click a node to expand neighbors
				</div>
			</div>

			{/* Controls */}
			<div className="absolute right-4 top-4 z-10 flex flex-col gap-1.5">
			<Button
				onClick={() => sigmaRef.current?.getCamera().animatedReset()}
				variant="ghost"
				size="icon"
				className="h-8 w-8 bg-app-darkBox/80 backdrop-blur-sm"
				title="Reset zoom"
			>
				<HugeiconsIcon icon={Target02Icon} className="h-4 w-4" />
			</Button>
			<Button
				onClick={() => {
					const camera = sigmaRef.current?.getCamera();
					if (camera) camera.animatedZoom({ duration: 200 });
				}}
				variant="ghost"
				size="icon"
				className="h-8 w-8 bg-app-darkBox/80 backdrop-blur-sm"
				title="Zoom in"
			>
				<HugeiconsIcon icon={PlusSignIcon} className="h-4 w-4" />
			</Button>
			<Button
				onClick={() => {
					const camera = sigmaRef.current?.getCamera();
					if (camera) camera.animatedUnzoom({ duration: 200 });
				}}
				variant="ghost"
				size="icon"
				className="h-8 w-8 bg-app-darkBox/80 backdrop-blur-sm"
				title="Zoom out"
			>
				<HugeiconsIcon icon={MinusSignIcon} className="h-4 w-4" />
			</Button>
			<Button
				onClick={() => {
					const layout = layoutRef.current;
					if (layout) {
						if (layout.isRunning()) {
							layout.stop();
						} else {
							layout.start();
							setTimeout(() => { if (layout.isRunning()) layout.stop(); }, 3000);
						}
					}
				}}
				variant="ghost"
				size="icon"
				className="h-8 w-8 bg-app-darkBox/80 backdrop-blur-sm"
				title="Re-run layout"
			>
				<HugeiconsIcon icon={RefreshIcon} className="h-4 w-4" />
			</Button>
			</div>

			{/* Node detail panel */}
			<AnimatePresence>
				{selectedNode && (
					<motion.div
						initial={{ opacity: 0, y: 8 }}
						animate={{ opacity: 1, y: 0 }}
						exit={{ opacity: 0, y: 8 }}
						transition={{ duration: 0.15 }}
						className="absolute right-4 bottom-4 z-20 w-80 rounded-lg border border-app-line/50 bg-app-darkBox/95 p-4 shadow-xl backdrop-blur-sm"
					>
						<div className="mb-2 flex items-center justify-between">
							<span
								className="rounded px-1.5 py-0.5 text-tiny font-medium"
								style={{
									backgroundColor: `${NODE_COLORS[selectedNode.memory.memory_type]}22`,
									color: NODE_COLORS[selectedNode.memory.memory_type],
								}}
							>
								{selectedNode.memory.memory_type}
							</span>
						<Button
							onClick={() => setSelectedNode(null)}
							variant="ghost"
							size="icon"
							className="h-7 w-7"
						>
							<HugeiconsIcon icon={Cancel01Icon} className="h-3.5 w-3.5" />
						</Button>
						</div>
						<p className="mb-3 max-h-32 overflow-y-auto whitespace-pre-wrap text-sm leading-relaxed text-ink-dull">
							{selectedNode.memory.content}
						</p>
						<div className="flex flex-wrap gap-x-4 gap-y-1 text-tiny text-ink-faint">
							<span>Importance: {selectedNode.memory.importance.toFixed(2)}</span>
							<span>Accessed: {selectedNode.memory.access_count}x</span>
							<span>Created: {formatTimeAgo(selectedNode.memory.created_at)}</span>
							{selectedNode.memory.source && (
								<span>Source: {selectedNode.memory.source}</span>
							)}
						</div>
						<div className="mt-3 flex gap-2">
							<Button
								size="sm"
								variant="secondary"
								className="flex-1"
								onClick={() => {
									onEditMemory?.(selectedNode.memory);
									setSelectedNode(null);
								}}
							>
								Edit
							</Button>
							<Button
								size="sm"
								variant="destructive"
								className="flex-1"
								onClick={() => {
									onDeleteMemory?.(selectedNode.memory);
									setSelectedNode(null);
								}}
							>
								Remove
							</Button>
						</div>
						<Button
							onClick={() => expandNeighbors(selectedNode.memory.id)}
							size="sm"
							loading={expandingNode === selectedNode.memory.id}
							className="mt-3 w-full bg-accent/15 text-tiny text-accent hover:bg-accent/25"
						>
							Expand Neighbors
						</Button>
					</motion.div>
				)}
			</AnimatePresence>

			{/* Sigma container */}
			<div
				ref={containerRef}
				className="h-full w-full"
				style={{ background: "transparent" }}
			/>

			{/* Loading / error / empty states */}
			{isLoading && (
				<div className="absolute inset-0 flex items-center justify-center bg-app/80">
					<div className="flex items-center gap-2 text-ink-dull">
						<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
						Loading graph...
					</div>
				</div>
			)}
			{error && (
				<div className="absolute inset-0 flex items-center justify-center">
					<p className="text-sm text-red-400">{error}</p>
				</div>
			)}
			{!isLoading && !error && nodeCount === 0 && (
				<div className="absolute inset-0 flex items-center justify-center">
					<p className="text-sm text-ink-faint">No memories to display</p>
				</div>
			)}
		</div>
	);
}

function truncateLabel(content: string): string {
	// Take the first line, then cap at 24 chars
	const firstLine = content.split("\n")[0].trim();
	if (firstLine.length <= 24) return firstLine;
	return firstLine.slice(0, 22) + "...";
}

function addEdgesToGraph(graph: Graph, edges: AssociationItem[]) {
	for (const edge of edges) {
		if (
			graph.hasNode(edge.source_id) &&
			graph.hasNode(edge.target_id) &&
			!graph.hasEdge(edge.id) &&
			!graph.hasDirectedEdge(edge.source_id, edge.target_id)
		) {
			graph.addEdgeWithKey(edge.id, edge.source_id, edge.target_id, {
				color: EDGE_COLORS[edge.relation_type] ?? "#444444",
				size: 1 + edge.weight * 2,
				type: "arrow",
				relationType: edge.relation_type,
			});
		}
	}
}
