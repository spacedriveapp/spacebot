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
import { api } from "@/api/client";
import { CodeGraphSearchBar } from "./codegraph/CodeGraphSearchBar";
import { CodeGraphSidebar } from "./codegraph/CodeGraphSidebar";
import { GraphCanvas, type GraphCanvasHandle } from "./codegraph/GraphCanvas";
import { CodeInspectorPanel } from "./codegraph/CodeInspectorPanel";
import { CodeGraphStatusBar } from "./codegraph/CodeGraphStatusBar";
import { MermaidView } from "./codegraph/MermaidView";
import { buildGraph } from "./codegraph/graphAdapter";
import {
	DEFAULT_VISIBLE_LABELS,
	DEFAULT_VISIBLE_EDGES,
	type NodeLabel,
	type EdgeType,
} from "./codegraph/constants";
import type { BulkNode } from "./codegraph/types";
import { nodeKey } from "./codegraph/graphAdapter";

export function CodeGraphTab({ projectId }: { projectId: string }) {
	const [selectedNode, setSelectedNode] = useState<BulkNode | null>(null);
	const [visibleLabels, setVisibleLabels] = useState<NodeLabel[]>(DEFAULT_VISIBLE_LABELS);
	const [visibleEdgeTypes, setVisibleEdgeTypes] = useState<EdgeType[]>(DEFAULT_VISIBLE_EDGES);
	const [depthFilter, setDepthFilter] = useState<number | null>(null);
	const [isLayoutRunning, setIsLayoutRunning] = useState(false);
	const [layoutMode, setLayoutMode] = useState<import("./codegraph/graphAdapter").LayoutMode>("force");
	const [colorOverrides, setColorOverrides] = useState<Record<string, string>>(() => {
		try {
			const saved = localStorage.getItem("spacebot.codegraph.colorOverrides");
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

	const canvasRef = useRef<GraphCanvasHandle>(null);
	const queryClient = useQueryClient();
	const [isUpdating, setIsUpdating] = useState(false);

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
					queryClient.invalidateQueries({ queryKey: ["codegraph-bulk-nodes", projectId] });
					queryClient.invalidateQueries({ queryKey: ["codegraph-bulk-edges", projectId] });
					queryClient.invalidateQueries({ queryKey: ["codegraph-project", projectId] });
				}
				if (event.type === "code_graph_indexed" && event.project_id === projectId) {
					setIsUpdating(false);
					queryClient.invalidateQueries({ queryKey: ["codegraph-bulk-nodes", projectId] });
					queryClient.invalidateQueries({ queryKey: ["codegraph-bulk-edges", projectId] });
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
			queryClient.invalidateQueries({ queryKey: ["codegraph-bulk-nodes", projectId] });
			queryClient.invalidateQueries({ queryKey: ["codegraph-bulk-edges", projectId] });
		}
		prevStatusRef.current = projectStatus;
	}, [projectStatus, projectId, queryClient]);

	// Load the full graph in two parallel requests. Edges wait for nodes
	// to resolve first so the server-side node-set matches.
	const nodesQuery = useQuery({
		queryKey: ["codegraph-bulk-nodes", projectId],
		queryFn: () => api.codegraphBulkNodes(projectId),
		staleTime: 60_000,
		enabled: projectStatus === "indexed" || projectStatus === "stale",
	});

	const edgesQuery = useQuery({
		queryKey: ["codegraph-bulk-edges", projectId],
		queryFn: () => api.codegraphBulkEdges(projectId),
		enabled: !!nodesQuery.data,
		staleTime: 60_000,
	});

	const nodes = nodesQuery.data?.nodes ?? [];
	const edges = edgesQuery.data?.edges ?? [];
	const truncated = nodesQuery.data?.truncated ?? false;
	const totalAvailable = nodesQuery.data?.total_available ?? 0;

	// Indexed lookup for O(1) selection resolution. Keyed by the composite
	// graphology key (label:id) since LadybugDB IDs are per-table.
	const nodesByKey = useMemo(() => {
		const map = new Map<string, BulkNode>();
		for (const n of nodes) map.set(nodeKey(n), n);
		return map;
	}, [nodes]);

	// Build the graphology graph once both payloads land.
	const graph = useMemo(() => {
		if (!nodesQuery.data || !edgesQuery.data) return null;
		return buildGraph(nodes, edges, colorOverrides);
	}, [nodesQuery.data, edgesQuery.data, nodes, edges]);

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

	const handleSelectAndFocus = (node: BulkNode) => {
		setSelectedNode(node);
		canvasRef.current?.focusNode(node);
	};

	const reindexMutation = useMutation({
		mutationFn: () => api.codegraphReindex(projectId),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["codegraph-project", projectId] });
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

	if (nodesQuery.isError || edgesQuery.isError) {
		return (
			<div className="flex h-full w-full flex-1 flex-col items-center justify-center gap-3 bg-app">
				<div className="h-3 w-3 rounded-full bg-red-500" />
				<p className="text-lg font-semibold text-red-400">Failed to Load Graph</p>
				<p className="text-sm text-ink-dull">Check the server logs for details.</p>
			</div>
		);
	}

	// Loading state — fetching nodes and edges.
	if (nodesQuery.isLoading || edgesQuery.isLoading || !graph) {
		const nodesPct = nodesQuery.data ? 50 : 0;
		const edgesPct = edgesQuery.data ? 100 : nodesPct;
		return (
			<div className="flex h-full w-full flex-1 flex-col items-center justify-center gap-4 bg-app">
				<div className="h-8 w-8 animate-spin rounded-full border-2 border-accent/30 border-t-accent" />
				<p className="text-lg font-semibold text-ink">Loading Code Graph</p>
				<p className="text-sm text-ink-dull">
					{!nodesQuery.data ? "Fetching nodes..." : !edgesQuery.data ? "Fetching edges..." : "Building graph..."}
				</p>
				<div className="mt-2 h-1.5 w-64 overflow-hidden rounded-full bg-app-line">
					<div
						className="h-full rounded-full bg-accent transition-all duration-500"
						style={{ width: `${Math.max(2, edgesPct)}%` }}
					/>
				</div>
				<p className="text-xs text-ink-faint">{edgesPct}%</p>
			</div>
		);
	}

	return (
		<div className="flex h-full min-h-0 w-full flex-1 flex-col bg-app">
			<CodeGraphSearchBar
				nodes={nodes}
				nodeCount={nodes.length}
				edgeCount={edges.length}
				truncated={truncated}
				totalAvailable={totalAvailable}
				onSelectNode={handleSelectAndFocus}
				onReindex={() => reindexMutation.mutate()}
				isReindexing={reindexMutation.isPending}
				colorOverrides={colorOverrides}
				layoutMode={layoutMode}
				onLayoutModeChange={setLayoutMode}
			/>

			<div className="flex min-h-0 flex-1">
				<CodeGraphSidebar
					projectId={projectId}
					nodes={nodes}
					selectedNode={selectedNode}
					onSelectNode={setSelectedNode}
					onFocusNode={(node) => canvasRef.current?.focusNode(node)}
					visibleLabels={visibleLabels}
					onToggleLabel={handleToggleLabel}
					visibleEdgeTypes={visibleEdgeTypes}
					onToggleEdge={handleToggleEdge}
					depthFilter={depthFilter}
					onChangeDepthFilter={setDepthFilter}
					colorOverrides={colorOverrides}
					onColorChange={handleColorChange}
				/>

				<div className="relative min-w-0 flex-1">
					{layoutMode === "mermaid" ? (
						<MermaidView nodes={nodes} edges={edges} />
					) : (
						<GraphCanvas
							ref={canvasRef}
							graph={graph}
							nodesByKey={nodesByKey}
							selectedNode={selectedNode}
							onSelectNode={setSelectedNode}
							visibleLabels={visibleLabels}
							visibleEdgeTypes={visibleEdgeTypes}
							depthFilter={depthFilter}
							colorOverrides={colorOverrides}
							layoutMode={layoutMode}
							onLayoutRunningChange={setIsLayoutRunning}
						/>
					)}
				</div>

				{selectedNode && (
					<CodeInspectorPanel
						projectId={projectId}
						selectedNode={selectedNode}
						onClose={() => setSelectedNode(null)}
						colorOverrides={colorOverrides}
					/>
				)}
			</div>

			<CodeGraphStatusBar
				nodeCount={nodes.length}
				edgeCount={edges.length}
				isLayoutRunning={isLayoutRunning}
				isUpdating={isUpdating}
				truncated={truncated}
			/>
		</div>
	);
}
