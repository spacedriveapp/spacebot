// Code Graph tab — the interactive graph view for a single indexed
// project. 4-zone layout:
//   1. Top: CodeGraphSearchBar
//   2. Left: CodeGraphSidebar (Explorer / Filters)
//   3. Center: GraphCanvas (Sigma + ForceAtlas2)
//   4. Right: CodeInspectorPanel (source viewer, open on selection)
//   5. Bottom: CodeGraphStatusBar
//
// All state is kept in local useState bags and prop-drilled down. No
// context is needed since this component is not reused elsewhere.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type CodeGraphProjectDetailResponse, type CodeGraphProjectListResponse } from "@/api/client";
import { CodeGraphSearchBar } from "./codegraph/CodeGraphSearchBar";
import { CodeGraphSidebar } from "./codegraph/CodeGraphSidebar";
import { GraphCanvas, type GraphCanvasHandle } from "./codegraph/GraphCanvas";
import { CodeInspectorPanel } from "./codegraph/CodeInspectorPanel";
import { CodeGraphStatusBar } from "./codegraph/CodeGraphStatusBar";
import { MermaidView } from "./codegraph/MermaidView";
import { MermaidRightPanel } from "./codegraph/MermaidRightPanel";
import { buildFileGraph } from "./codegraph/mermaidGraphBuilder";
import { buildGraph } from "./codegraph/graphAdapter";
import {
	DEFAULT_VISIBLE_LABELS,
	DEFAULT_VISIBLE_EDGES,
	FILTERABLE_LABELS,
	ALL_EDGE_TYPES,
	type NodeLabel,
	type EdgeType,
} from "./codegraph/constants";
import type { BulkNode } from "./codegraph/types";
import { nodeKey } from "./codegraph/graphAdapter";

export function CodeGraphTab({ projectId }: { projectId: string }) {
	const [selectedNode, setSelectedNode] = useState<BulkNode | null>(null);
	const [visibleLabels, setVisibleLabels] = useState<NodeLabel[]>(() => {
		try {
			const saved = localStorage.getItem("spacebot.codegraph.visibleLabels");
			return saved ? JSON.parse(saved) : DEFAULT_VISIBLE_LABELS;
		} catch { return DEFAULT_VISIBLE_LABELS; }
	});
	const [visibleEdgeTypes, setVisibleEdgeTypes] = useState<EdgeType[]>(() => {
		try {
			const saved = localStorage.getItem("spacebot.codegraph.visibleEdgeTypes");
			return saved ? JSON.parse(saved) : DEFAULT_VISIBLE_EDGES;
		} catch { return DEFAULT_VISIBLE_EDGES; }
	});
	const [depthFilter, setDepthFilter] = useState<number | null>(null);
	const [isLayoutRunning, setIsLayoutRunning] = useState(false);
	const [layoutMode, setLayoutMode] = useState<import("./codegraph/graphAdapter").LayoutMode>(() => {
		try {
			const saved = localStorage.getItem("spacebot.codegraph.layoutMode");
			return (saved as import("./codegraph/graphAdapter").LayoutMode) || "force";
		} catch { return "force"; }
	});
	const [colorOverrides, setColorOverrides] = useState<Record<string, string>>(() => {
		try {
			const saved = localStorage.getItem("spacebot.codegraph.colorOverrides");
			return saved ? JSON.parse(saved) : {};
		} catch {
			return {};
		}
	});
	const [edgeColorOverrides, setEdgeColorOverrides] = useState<Record<string, string>>(() => {
		try {
			const saved = localStorage.getItem("spacebot.codegraph.edgeColorOverrides");
			return saved ? JSON.parse(saved) : {};
		} catch {
			return {};
		}
	});

	const handleColorChange = useCallback((label: NodeLabel, color: string | null) => {
		setColorOverrides((prev) => {
			const next = { ...prev };
			if (color === null) {
				delete next[label];
			} else {
				next[label] = color;
			}
			try {
				localStorage.setItem("spacebot.codegraph.colorOverrides", JSON.stringify(next));
			} catch { /* ignore */ }
			return next;
		});
	}, []);

	const handleEdgeColorChange = useCallback((edge: EdgeType, color: string | null) => {
		setEdgeColorOverrides((prev) => {
			const next = { ...prev };
			if (color === null) {
				delete next[edge];
			} else {
				next[edge] = color;
			}
			try {
				localStorage.setItem("spacebot.codegraph.edgeColorOverrides", JSON.stringify(next));
			} catch { /* ignore */ }
			return next;
		});
	}, []);

	const canvasRef = useRef<GraphCanvasHandle>(null);
	const queryClient = useQueryClient();
	const [isUpdating, setIsUpdating] = useState(false);
	// Mermaid view routes the right rail through MermaidRightPanel by default.
	// When the user clicks "View source" on the NodeInfoCard, this flag opens
	// the existing CodeInspectorPanel as a third column on top of it.
	const [showInspector, setShowInspector] = useState(false);

	// Subscribe to SSE for real-time graph updates. When files change in
	// the project, the watcher fires GraphStale → incremental pipeline →
	// GraphChanged. We invalidate queries so the graph refreshes.
	useEffect(() => {
		const source = new EventSource(api.getEventsUrl());
		source.addEventListener("message", (e) => {
			try {
				const event = JSON.parse(e.data);
				if (event.type === "code_graph_stale" && event.project_id === projectId) {
					setIsUpdating(true);
				}
				if (event.type === "code_graph_changed" && event.project_id === projectId) {
					setIsUpdating(false);
					queryClient.invalidateQueries({ queryKey: ["codegraph-graph-stream", projectId] });
					queryClient.invalidateQueries({ queryKey: ["codegraph-project", projectId] });
				}
				if (event.type === "code_graph_indexed" && event.project_id === projectId) {
					setIsUpdating(false);
					queryClient.invalidateQueries({ queryKey: ["codegraph-graph-stream", projectId] });
					queryClient.invalidateQueries({ queryKey: ["codegraph-project", projectId] });
				}
			} catch { /* ignore parse errors */ }
		});
		return () => source.close();
	}, [projectId, queryClient]);

	// Piggyback on ProjectDetail's existing project query — it owns the
	// polling schedule during indexing. We just read the cached status to
	// detect the indexing → indexed transition and invalidate the bulk
	// queries so the graph reloads after a re-index.
	const projectQuery = useQuery({
		queryKey: ["codegraph-project", projectId],
		queryFn: () => api.codegraphProject(projectId),
	});

	const projectStatus = projectQuery.data?.project.status;
	const prevStatusRef = useRef<string | undefined>(undefined);
	useEffect(() => {
		if (prevStatusRef.current === "indexing" && projectStatus === "indexed") {
			queryClient.invalidateQueries({ queryKey: ["codegraph-graph-stream", projectId] });
		}
		prevStatusRef.current = projectStatus;
	}, [projectStatus, projectId, queryClient]);

	// Persist filter state to localStorage so it survives tab switches
	// and project changes.
	useEffect(() => {
		try { localStorage.setItem("spacebot.codegraph.visibleLabels", JSON.stringify(visibleLabels)); } catch { /* ignore */ }
	}, [visibleLabels]);
	useEffect(() => {
		try { localStorage.setItem("spacebot.codegraph.visibleEdgeTypes", JSON.stringify(visibleEdgeTypes)); } catch { /* ignore */ }
	}, [visibleEdgeTypes]);
	useEffect(() => {
		try { localStorage.setItem("spacebot.codegraph.layoutMode", layoutMode); } catch { /* ignore */ }
	}, [layoutMode]);

	// Totals for the loading-progress percentage. Fetched in parallel with
	// the stream so the loading UI can show a real % rather than an
	// indeterminate bar.
	const statsQuery = useQuery({
		queryKey: ["codegraph-stats-totals", projectId],
		queryFn: () => api.codegraphStats(projectId),
		enabled: projectStatus === "indexed" || projectStatus === "stale",
		staleTime: 60_000,
	});

	const [loadProgress, setLoadProgress] = useState<{
		phase: "nodes" | "edges";
		nodesLoaded: number;
		edgesLoaded: number;
	} | null>(null);

	// Load the full graph in a single NDJSON stream — nodes then edges are
	// delivered row-by-row from the Kuzu cursor, so huge graphs land
	// without crashing the backend. Aborts on unmount via the signal.
	// Reset progress to 0 at the start of every fetch so the bar doesn't
	// carry over from a previous load (navigation, reindex, refetch).
	const graphQuery = useQuery({
		queryKey: ["codegraph-graph-stream", projectId],
		queryFn: ({ signal }) => {
			setLoadProgress(null);
			return api.codegraphGraphStream(projectId, signal, (p) => {
				setLoadProgress(p);
			});
		},
		staleTime: 60_000,
		enabled: projectStatus === "indexed" || projectStatus === "stale",
		retry: 1,
	});

	const nodes = graphQuery.data?.nodes ?? [];
	const edges = graphQuery.data?.edges ?? [];

	// Indexed lookup for O(1) selection resolution. Keyed by the composite
	// graphology key (label:id) since LadybugDB IDs are per-table.
	const nodesByKey = useMemo(() => {
		const map = new Map<string, BulkNode>();
		for (const n of nodes) map.set(nodeKey(n), n);
		return map;
	}, [nodes]);

	// Build the graphology graph once the payload lands. Skipped in mermaid
	// mode because GraphCanvas isn't mounted — building it for large projects
	// (10k+ nodes) was a visible hit on the first mermaid render.
	const graph = useMemo(() => {
		if (!graphQuery.data) return null;
		if (layoutMode === "mermaid") return null;
		return buildGraph(nodes, edges, colorOverrides, edgeColorOverrides);
	}, [graphQuery.data, layoutMode, nodes, edges, colorOverrides, edgeColorOverrides]);

	// Parallel file-only graph powering the mermaid view and its right panel.
	// Cheap to compute (O(nodes+edges)) but memoized so the React Flow layout
	// stays stable across re-renders.
	const fileGraph = useMemo(() => {
		if (layoutMode !== "mermaid" || !graphQuery.data) return null;
		return buildFileGraph(nodes, edges);
	}, [layoutMode, graphQuery.data, nodes, edges]);

	const handleToggleLabel = (label: NodeLabel) => {
		setVisibleLabels((prev) =>
			prev.includes(label) ? prev.filter((l) => l !== label) : [...prev, label],
		);
	};

	const handleToggleEdge = (edge: EdgeType) => {
		setVisibleEdgeTypes((prev) =>
			prev.includes(edge) ? prev.filter((e) => e !== edge) : [...prev, edge],
		);
	};

	// Mermaid ignores the label/edge/depth filters — the view is all files
	// and folders regardless of the stored toggle state. Derive an effective
	// filter set that is fully permissive when layoutMode === "mermaid" so
	// anything downstream that reads these values (e.g. a future filtered
	// search) also sees "all on" without mutating the user's saved prefs.
	const isMermaid = layoutMode === "mermaid";
	const effectiveVisibleLabels = isMermaid ? FILTERABLE_LABELS : visibleLabels;
	const effectiveVisibleEdgeTypes = isMermaid ? ALL_EDGE_TYPES : visibleEdgeTypes;
	const effectiveDepthFilter = isMermaid ? null : depthFilter;

	const handleSelectAndFocus = (node: BulkNode) => {
		setSelectedNode(node);
		canvasRef.current?.focusNode(node);
	};

	const reindexMutation = useMutation({
		mutationFn: () => api.codegraphReindex(projectId),
		onMutate: async () => {
			const detailKey = ["codegraph-project", projectId];
			const listKey = ["codegraph-projects"];
			await Promise.all([
				queryClient.cancelQueries({ queryKey: detailKey }),
				queryClient.cancelQueries({ queryKey: listKey }),
			]);
			const prevDetail = queryClient.getQueryData<CodeGraphProjectDetailResponse>(detailKey);
			const prevList = queryClient.getQueryData<CodeGraphProjectListResponse>(listKey);
			if (prevDetail) {
				queryClient.setQueryData<CodeGraphProjectDetailResponse>(detailKey, {
					project: { ...prevDetail.project, status: "indexing" },
				});
			}
			if (prevList) {
				queryClient.setQueryData<CodeGraphProjectListResponse>(listKey, {
					projects: prevList.projects.map((p) =>
						p.project_id === projectId ? { ...p, status: "indexing" } : p,
					),
				});
			}
			return { prevDetail, prevList };
		},
		onError: (_err, _vars, ctx) => {
			if (ctx?.prevDetail) queryClient.setQueryData(["codegraph-project", projectId], ctx.prevDetail);
			if (ctx?.prevList) queryClient.setQueryData(["codegraph-projects"], ctx.prevList);
		},
		onSettled: () => {
			queryClient.invalidateQueries({ queryKey: ["codegraph-project", projectId] });
			queryClient.invalidateQueries({ queryKey: ["codegraph-projects"] });
		},
	});

	// Wait for the project to finish indexing before trying to load the graph.
	if (projectStatus === "indexing" || projectStatus === "pending") {
		const progress = projectQuery.data?.project.progress;
		const pct = progress
			? Math.round(((progress.stats?.files_parsed ?? 0) / Math.max(progress.stats?.files_found ?? 1, 1)) * 100)
			: 0;
		return (
			<div className="flex h-full w-full flex-1 flex-col items-center justify-center gap-4 bg-app">
				<div className="h-8 w-8 animate-spin rounded-full border-2 border-accent/30 border-t-accent" />
				<p className="text-lg font-semibold text-ink">Indexing Project</p>
				<p className="text-sm text-ink-dull">{progress?.message ?? "Preparing..."}</p>
				<div className="mt-2 h-1.5 w-64 overflow-hidden rounded-full bg-app-line">
					<div
						className="h-full rounded-full bg-accent transition-all duration-500"
						style={{ width: `${Math.max(2, pct)}%` }}
					/>
				</div>
				<p className="text-xs text-ink-faint">{pct}% complete</p>
			</div>
		);
	}

	if (projectStatus === "error") {
		return (
			<div className="flex h-full w-full flex-1 flex-col items-center justify-center gap-3 bg-app">
				<div className="h-3 w-3 rounded-full bg-red-500" />
				<p className="text-lg font-semibold text-red-400">Indexing Failed</p>
				<p className="text-sm text-ink-dull">Re-index the project from the Overview tab.</p>
			</div>
		);
	}

	if (graphQuery.isError) {
		return (
			<div className="flex h-full w-full flex-1 flex-col items-center justify-center gap-3 bg-app">
				<div className="h-3 w-3 rounded-full bg-red-500" />
				<p className="text-lg font-semibold text-red-400">Failed to Load Graph</p>
				<p className="text-sm text-ink-dull">Check the server logs for details.</p>
			</div>
		);
	}

	// Loading state — streaming nodes + edges in a single NDJSON request.
	// Progress bar is the combined (nodes + edges) ratio so it advances
	// continuously from 0→100% without resetting when the phase flips.
	const viewReady = layoutMode === "mermaid" ? !!fileGraph : !!graph;
	if (graphQuery.isLoading || !viewReady) {
		const totalNodes = statsQuery.data?.total_nodes;
		const totalEdges = statsQuery.data?.total_edges;
		const loadedNodes = loadProgress?.nodesLoaded ?? 0;
		const loadedEdges = loadProgress?.edgesLoaded ?? 0;
		const hasTotals = totalNodes !== undefined && totalEdges !== undefined;
		const loadedCombined = loadedNodes + loadedEdges;
		const totalCombined = hasTotals ? totalNodes! + totalEdges! : 0;
		const pct = hasTotals && totalCombined > 0
			? Math.min(100, Math.round((loadedCombined / totalCombined) * 100))
			: null;
		const phaseLabel = loadProgress?.phase === "edges" ? "Loading edges" : "Loading nodes";
		const phaseLoaded = loadProgress?.phase === "edges" ? loadedEdges : loadedNodes;
		const phaseTotal = loadProgress?.phase === "edges" ? totalEdges : totalNodes;
		const detail = phaseTotal !== undefined
			? `${phaseLabel}... ${phaseLoaded.toLocaleString()} / ${phaseTotal.toLocaleString()}${pct !== null ? ` (${pct}%)` : ""}`
			: `${phaseLabel}... ${phaseLoaded.toLocaleString()}`;
		return (
			<div className="flex h-full w-full flex-1 flex-col items-center justify-center gap-4 bg-app">
				<p className="text-lg font-semibold text-ink">Loading Code Graph</p>
				<p className="text-sm text-ink-dull">{detail}</p>
				<div className="mt-2 h-1.5 w-64 overflow-hidden rounded-full bg-app-line">
					{pct !== null && (
						<div
							className="h-full rounded-full bg-accent transition-all duration-200"
							style={{ width: `${pct}%` }}
						/>
					)}
				</div>
			</div>
		);
	}

	return (
		<div className="flex h-full min-h-0 w-full flex-1 flex-col bg-app">
			<CodeGraphSearchBar
				nodes={nodes}
				nodeCount={nodes.length}
				edgeCount={edges.length}
				onSelectNode={handleSelectAndFocus}
				onReindex={() => reindexMutation.mutate()}
				isReindexing={reindexMutation.isPending}
				colorOverrides={colorOverrides}
				layoutMode={layoutMode}
				onLayoutModeChange={setLayoutMode}
				visibleLabels={effectiveVisibleLabels}
			/>

			<div className="flex min-h-0 flex-1">
				<CodeGraphSidebar
					projectId={projectId}
					nodes={nodes}
					selectedNode={selectedNode}
					onSelectNode={setSelectedNode}
					onFocusNode={(node) => canvasRef.current?.focusNode(node)}
					visibleLabels={effectiveVisibleLabels}
					onToggleLabel={handleToggleLabel}
					visibleEdgeTypes={effectiveVisibleEdgeTypes}
					onToggleEdge={handleToggleEdge}
					depthFilter={effectiveDepthFilter}
					onChangeDepthFilter={setDepthFilter}
					colorOverrides={colorOverrides}
					onColorChange={handleColorChange}
					edgeColorOverrides={edgeColorOverrides}
					onEdgeColorChange={handleEdgeColorChange}
					showFilters={!isMermaid}
				/>

				<div className="relative min-w-0 flex-1">
					{layoutMode === "mermaid" && fileGraph ? (
						<MermaidView
							projectId={projectId}
							graph={fileGraph}
							selectedNode={selectedNode}
							onSelectFile={(file) => {
								setSelectedNode(file);
								if (!file) setShowInspector(false);
							}}
						/>
					) : (
						<GraphCanvas
							ref={canvasRef}
							graph={graph}
							nodesByKey={nodesByKey}
							selectedNode={selectedNode}
							onSelectNode={setSelectedNode}
							visibleLabels={effectiveVisibleLabels}
							visibleEdgeTypes={effectiveVisibleEdgeTypes}
							depthFilter={effectiveDepthFilter}
							colorOverrides={colorOverrides}
							layoutMode={layoutMode}
							onLayoutRunningChange={setIsLayoutRunning}
						/>
					)}
				</div>

				{layoutMode === "mermaid" && fileGraph && (
					<MermaidRightPanel
						projectName={projectQuery.data?.project.name ?? "Project"}
						graph={fileGraph}
						allNodes={nodes}
						allEdges={edges}
						selectedNode={selectedNode}
						onSelectFile={(file) => {
							setSelectedNode(file);
							if (!file) setShowInspector(false);
						}}
						onRequestSource={() => setShowInspector(true)}
					/>
				)}

				{selectedNode && layoutMode !== "mermaid" && (
					<CodeInspectorPanel
						projectId={projectId}
						selectedNode={selectedNode}
						onClose={() => setSelectedNode(null)}
						colorOverrides={colorOverrides}
					/>
				)}

				{layoutMode === "mermaid" && showInspector && selectedNode && (
					<CodeInspectorPanel
						projectId={projectId}
						selectedNode={selectedNode}
						onClose={() => setShowInspector(false)}
						colorOverrides={colorOverrides}
					/>
				)}
			</div>

			<CodeGraphStatusBar
				nodeCount={nodes.length}
				edgeCount={edges.length}
				isLayoutRunning={isLayoutRunning}
				isUpdating={isUpdating}
			/>
		</div>
	);
}
