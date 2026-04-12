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

import { useEffect, useMemo, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "@/api/client";
import { CodeGraphSearchBar } from "./codegraph/CodeGraphSearchBar";
import { CodeGraphSidebar } from "./codegraph/CodeGraphSidebar";
import { GraphCanvas, type GraphCanvasHandle } from "./codegraph/GraphCanvas";
import { CodeInspectorPanel } from "./codegraph/CodeInspectorPanel";
import { CodeGraphStatusBar } from "./codegraph/CodeGraphStatusBar";
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

	const canvasRef = useRef<GraphCanvasHandle>(null);
	const queryClient = useQueryClient();

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
		return buildGraph(nodes, edges);
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
		return (
			<div className="flex h-full items-center justify-center">
				<p className="text-sm text-ink-faint">Project is still indexing — the graph will load when the index completes.</p>
			</div>
		);
	}

	if (projectStatus === "error") {
		return (
			<div className="flex h-full items-center justify-center">
				<p className="text-sm text-red-400">Indexing failed. Re-index the project from the Overview tab.</p>
			</div>
		);
	}

	if (nodesQuery.isError || edgesQuery.isError) {
		return (
			<div className="flex h-full items-center justify-center">
				<p className="text-sm text-red-400">Failed to load code graph.</p>
			</div>
		);
	}

	// Initial loading state — graph hasn't been built yet.
	if (nodesQuery.isLoading || edgesQuery.isLoading || !graph) {
		return (
			<div className="flex h-full items-center justify-center">
				<p className="text-sm text-ink-faint">Loading code graph...</p>
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
				/>

				<div className="relative min-w-0 flex-1">
					<GraphCanvas
						ref={canvasRef}
						graph={graph}
						nodesByKey={nodesByKey}
						selectedNode={selectedNode}
						onSelectNode={setSelectedNode}
						visibleLabels={visibleLabels}
						visibleEdgeTypes={visibleEdgeTypes}
						depthFilter={depthFilter}
						onLayoutRunningChange={setIsLayoutRunning}
					/>
				</div>

				{selectedNode && (
					<CodeInspectorPanel
						projectId={projectId}
						selectedNode={selectedNode}
						onClose={() => setSelectedNode(null)}
					/>
				)}
			</div>

			<CodeGraphStatusBar
				nodeCount={nodes.length}
				edgeCount={edges.length}
				isLayoutRunning={isLayoutRunning}
				truncated={truncated}
			/>
		</div>
	);
}
