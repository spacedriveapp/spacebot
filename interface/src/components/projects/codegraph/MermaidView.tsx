// Mermaid-mode graph view — two-level navigation modeled after
// Lum1104/Understand-Anything:
//   Overview: one LayerClusterNode per top-level folder, connected by
//             aggregated cross-layer bundle edges.
//   Detail:   the clicked layer's files as FileNodeCards, with
//             PortalNodes on the rim leading to connected layers.
// Selecting a file in detail highlights its neighbors, fades everything
// else to ~20 % opacity, and restyles the selected node's edges to
// dashed-and-animated. Lines sit behind cards via explicit zIndex.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
	Background,
	BackgroundVariant,
	Controls,
	MiniMap,
	ReactFlow,
	ReactFlowProvider,
	type Edge,
	type Node,
	type NodeMouseHandler,
	type NodeTypes,
	useReactFlow,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import dagre from "@dagrejs/dagre";
import { FileNodeCard } from "./FileNodeCard";
import { FileNodeCardCompact } from "./FileNodeCardCompact";
import { LayerClusterNode, type LayerClusterData } from "./LayerClusterNode";
import { PortalNode, type PortalNodeData } from "./PortalNode";
import { Breadcrumb } from "./Breadcrumb";
import { OrphanDrawer } from "./OrphanDrawer";
import { loadNodeColors, saveNodeColor, type NodeColorOverrides } from "./mermaidOverrides";
import {
	type BuildResult,
	type FileEdgeData,
	type FileNodeData,
} from "./mermaidGraphBuilder";
import { findCrossLayerNeighbors, firstSegment } from "./layerBuilder";
import { describeLayer } from "./nodeDescription";
import { COMMUNITY_COLORS, getCommunityColor } from "./constants";
import type { BulkNode } from "./types";

interface Props {
	projectId: string;
	graph: BuildResult;
	selectedNode: BulkNode | null;
	onSelectFile: (file: BulkNode | null) => void;
}

// Geometry.
const FILE_NODE_WIDTH = 240;
const FILE_NODE_HEIGHT = 150;
const FILE_NODE_COMPACT_HEIGHT = 68;
const LAYER_NODE_WIDTH = 300;
const LAYER_NODE_HEIGHT = 140;
const PORTAL_NODE_WIDTH = 180;
const PORTAL_NODE_HEIGHT = 90;
const DAGRE_NODE_SEP = 70;
const DAGRE_RANK_SEP = 120;

// Above this many on-canvas file nodes, swap the full card for a
// single-line compact variant. Full cards pull in a description, a
// symbol list, and a Radix Popover trigger per card — fine at low
// counts, but at the zoom-out level needed to see a layer with
// hundreds of files the text is unreadable anyway and the extra DOM
// is what's making pan/zoom feel sluggish.
const COMPACT_THRESHOLD = 150;

// Edge colors keyed by edge type.
const EDGE_COLORS: Record<string, string> = {
	IMPORTS: "rgba(147,197,253,0.85)",
	CALLS: "rgba(252,211,77,0.85)",
	EXTENDS: "rgba(251,146,60,0.85)",
	IMPLEMENTS: "rgba(196,181,253,0.85)",
};
const DEFAULT_EDGE_COLOR = "rgba(212,165,116,0.75)";
const FADED_EDGE_COLOR = "rgba(212,165,116,0.08)";

const HUGE_GRAPH_NODES = 500;

const NODE_TYPES: NodeTypes = {
	file: FileNodeCard,
	fileCompact: FileNodeCardCompact,
	layerCluster: LayerClusterNode,
	portal: PortalNode,
};

interface Dim { width: number; height: number }

function applyDagre(
	nodes: Node[],
	edges: Array<{ source: string; target: string }>,
	dims: Map<string, Dim>,
): Node[] {
	const g = new dagre.graphlib.Graph({ compound: false });
	g.setGraph({
		rankdir: "TB",
		nodesep: DAGRE_NODE_SEP,
		ranksep: DAGRE_RANK_SEP,
		marginx: 40,
		marginy: 40,
		ranker: "tight-tree",
	});
	g.setDefaultEdgeLabel(() => ({}));

	for (const n of nodes) {
		const d = dims.get(n.id) ?? { width: FILE_NODE_WIDTH, height: FILE_NODE_HEIGHT };
		g.setNode(n.id, { width: d.width, height: d.height });
	}
	const seen = new Set<string>();
	for (const e of edges) {
		const key = `${e.source}\u0001${e.target}`;
		if (seen.has(key)) continue;
		seen.add(key);
		if (!dims.has(e.source) || !dims.has(e.target)) continue;
		g.setEdge(e.source, e.target);
	}
	dagre.layout(g);

	return nodes.map((n) => {
		const laid = g.node(n.id);
		const d = dims.get(n.id) ?? { width: FILE_NODE_WIDTH, height: FILE_NODE_HEIGHT };
		if (!laid) return n;
		return {
			...n,
			position: { x: laid.x - d.width / 2, y: laid.y - d.height / 2 },
			width: d.width,
			height: d.height,
		};
	});
}

// --- Overview level: layer cluster nodes + bundle edges ---------------

function useOverviewGraph(graph: BuildResult, onDrillIn: (layerId: string) => void) {
	return useMemo(() => {
		const { layers, crossLayerBundles, fileNodes } = graph;
		if (layers.length === 0) return { nodes: [] as Node[], edges: [] as Edge[] };

		const clusterNodes: Node<LayerClusterData>[] = layers.map((layer) => ({
			id: layer.id,
			type: "layerCluster",
			position: { x: 0, y: 0 },
			zIndex: 1,
			data: {
				layerId: layer.id,
				layerName: layer.name,
				fileCount: layer.fileCount,
				description: describeLayer(layer, fileNodes),
				colorIndex: layer.colorIndex,
				onDrillIn,
			},
		}));

		const bundleEdges: Edge<FileEdgeData>[] = crossLayerBundles.map((b) => {
			let dominant: string | null = null;
			let max = 0;
			for (const [type, n] of Object.entries(b.types)) {
				if (n > max) { max = n; dominant = type; }
			}
			const color = (dominant && EDGE_COLORS[dominant]) ?? DEFAULT_EDGE_COLOR;
			const strokeWidth = Math.min(2 + Math.log2(b.count + 1), 6);
			return {
				id: b.id,
				source: b.sourceLayerId,
				target: b.targetLayerId,
				type: "default",
				animated: false,
				focusable: false,
				selectable: false,
				zIndex: 0,
				data: { edgeType: dominant ?? "bundle", count: b.count },
				label: b.count > 1 ? String(b.count) : undefined,
				labelStyle: { fill: "rgba(220,220,235,0.95)", fontSize: 11, fontFamily: "ui-monospace, monospace" },
				labelBgStyle: { fill: "rgba(20,20,30,0.9)" },
				labelBgPadding: [4, 2] as [number, number],
				labelBgBorderRadius: 4,
				style: {
					stroke: color,
					strokeWidth,
					strokeLinecap: "round",
					opacity: 0.7,
				},
				markerEnd: undefined,
			};
		});

		const dims = new Map<string, Dim>();
		for (const n of clusterNodes) {
			dims.set(n.id, { width: LAYER_NODE_WIDTH, height: LAYER_NODE_HEIGHT });
		}
		const laid = applyDagre(clusterNodes as Node[], bundleEdges.map((e) => ({ source: e.source, target: e.target })), dims);
		return { nodes: laid, edges: bundleEdges as Edge[] };
	}, [graph, onDrillIn]);
}

// --- Detail level: topology (positions) separated from overlay (styling) -

interface DetailTopology {
	nodes: Node[];
	fileEdges: Edge<FileEdgeData>[];
	portalEdges: Edge<FileEdgeData>[];
	portalNodeIds: Set<string>;
	isolatedFiles: BulkNode[];
}

function useDetailTopology(
	graph: BuildResult,
	activeLayerId: string | null,
	onNavigate: (layerId: string) => void,
	onRecolorNode: (fileQname: string, color: string | null) => void,
	nodeColors: NodeColorOverrides,
): DetailTopology {
	return useMemo(() => {
		const empty: DetailTopology = {
			nodes: [],
			fileEdges: [],
			portalEdges: [],
			portalNodeIds: new Set(),
			isolatedFiles: [],
		};
		if (!activeLayerId) return empty;
		const { layers, fileToLayer, aggregatedEdges, nodes: allFileNodes } = graph;
		const activeLayer = layers.find((l) => l.id === activeLayerId);
		if (!activeLayer) return empty;

		const fileNodesInLayer = allFileNodes.filter((n) =>
			activeLayer.fileQnames.has(n.id),
		);

		const layerFileQnames = activeLayer.fileQnames;

		// Intra-layer edges.
		const fileEdges: Edge<FileEdgeData>[] = aggregatedEdges
			.filter((e) => layerFileQnames.has(e.fromQname) && layerFileQnames.has(e.toQname))
			.map((e) => ({
				id: `fe:${e.fromQname}__${e.type}__${e.toQname}`,
				source: e.fromQname,
				target: e.toQname,
				type: "default",
				animated: false,
				focusable: false,
				zIndex: 0,
				data: { edgeType: e.type, count: e.count },
				style: {
					stroke: EDGE_COLORS[e.type] ?? DEFAULT_EDGE_COLOR,
					strokeWidth: Math.min(1.5 + Math.log2(e.count + 1), 3.5),
					strokeLinecap: "round",
					opacity: 0.85,
				},
				markerEnd: undefined,
			}));

		// Portals and their lead-out stubs.
		const { portals, portalEdges: portalStubs } = findCrossLayerNeighbors(
			aggregatedEdges,
			fileToLayer,
			layers,
			activeLayerId,
		);

		const portalFlowNodes: Node<PortalNodeData>[] = portals.map((portal) => ({
			id: `portal:${portal.layerId}`,
			type: "portal",
			position: { x: 0, y: 0 },
			zIndex: 1,
			data: {
				targetLayerId: portal.layerId,
				targetLayerName: portal.layerName,
				connectionCount: portal.connectionCount,
				colorIndex: portal.colorIndex,
				onNavigate,
			},
		}));
		const portalNodeIds = new Set(portalFlowNodes.map((n) => n.id));

		// Deduplicate portal edge stubs per (file, portal).
		const seenPortalStub = new Set<string>();
		const portalEdges: Edge<FileEdgeData>[] = [];
		for (const stub of portalStubs) {
			const portalId = `portal:${stub.targetLayerId}`;
			if (!portalNodeIds.has(portalId)) continue;
			if (!layerFileQnames.has(stub.fileQname)) continue;
			const key = `${stub.fileQname}__${portalId}`;
			if (seenPortalStub.has(key)) continue;
			seenPortalStub.add(key);
			const color = getCommunityColor(
				(layers.find((l) => l.id === stub.targetLayerId)?.colorIndex ?? 0) %
					COMMUNITY_COLORS.length,
			);
			portalEdges.push({
				id: `pe:${key}`,
				source: stub.fileQname,
				target: portalId,
				type: "default",
				animated: false,
				focusable: false,
				selectable: false,
				zIndex: 0,
				data: { edgeType: "PORTAL", count: 1 },
				style: {
					stroke: color,
					strokeWidth: 1,
					strokeDasharray: "4 4",
					opacity: 0.3,
				},
				markerEnd: undefined,
			});
		}

		// Above COMPACT_THRESHOLD on-canvas files, switch to the minimal
		// card variant so pan/zoom stays smooth when the whole layer is
		// in view. Height shrinks to match so dagre doesn't over-space.
		const compact = fileNodesInLayer.length > COMPACT_THRESHOLD;
		const fileNodeType = compact ? "fileCompact" : "file";
		const fileNodeHeight = compact ? FILE_NODE_COMPACT_HEIGHT : FILE_NODE_HEIGHT;

		// Keep the data object stable across selection changes — the
		// overlay (fade/ring) is driven by outer node.style in
		// useDetailGraph, which lets memo(FileNodeCard) skip re-renders
		// even when the selection churns.
		const fileFlowNodes: Node<FileNodeData>[] = fileNodesInLayer.map((n) => ({
			...n,
			type: fileNodeType,
			zIndex: 1,
			data: {
				...(n.data as FileNodeData),
				colorOverride: nodeColors[n.id],
				onRecolorNode,
			} as FileNodeData,
		}));

		// Files with no intra-layer or portal edge carry no relationships
		// worth rendering on the canvas. Hand them to the drawer instead of
		// crowding the graph (and collapsing into dagre's rank 0 as a line).
		const connectedIds = new Set<string>();
		for (const e of fileEdges) {
			connectedIds.add(e.source);
			connectedIds.add(e.target);
		}
		for (const e of portalEdges) {
			connectedIds.add(e.source);
		}
		const connectedFileNodes = fileFlowNodes.filter((n) => connectedIds.has(n.id));
		const isolatedFileNodes = fileFlowNodes.filter((n) => !connectedIds.has(n.id));

		const dagreNodes: Node[] = [...connectedFileNodes, ...portalFlowNodes] as Node[];
		const dims = new Map<string, Dim>();
		for (const n of connectedFileNodes) dims.set(n.id, { width: FILE_NODE_WIDTH, height: fileNodeHeight });
		for (const n of portalFlowNodes) dims.set(n.id, { width: PORTAL_NODE_WIDTH, height: PORTAL_NODE_HEIGHT });

		const edgesForLayout = [
			...fileEdges.map((e) => ({ source: e.source, target: e.target })),
			...portalEdges.map((e) => ({ source: e.source, target: e.target })),
		];
		const laid = applyDagre(dagreNodes, edgesForLayout, dims);

		// If the canvas has no relational content, fall back to tiling the
		// isolated files on it as a grid — otherwise the user sees an empty
		// canvas even though the layer has files. Only when the canvas
		// already has connected files / portals do orphans go to the drawer.
		const canvasEmpty = connectedFileNodes.length === 0 && portalFlowNodes.length === 0;
		const gridNodes: Node[] = [];
		let isolatedFiles: BulkNode[] = [];
		if (canvasEmpty) {
			const cols = 5;
			const colStride = FILE_NODE_WIDTH + 40;
			const rowStride = fileNodeHeight + 40;
			isolatedFileNodes.forEach((n, i) => {
				gridNodes.push({
					...n,
					position: { x: (i % cols) * colStride, y: Math.floor(i / cols) * rowStride },
					width: FILE_NODE_WIDTH,
					height: fileNodeHeight,
				});
			});
		} else {
			isolatedFiles = isolatedFileNodes.map((n) => (n.data as FileNodeData).file);
		}

		return { nodes: [...laid, ...gridNodes], fileEdges, portalEdges, portalNodeIds, isolatedFiles };
	}, [graph, activeLayerId, onNavigate, onRecolorNode, nodeColors]);
}

// Past this many edges on the canvas we drop animated="true" on selection
// — the marching-ants effect applies per-edge SVG updates at 60Hz, which
// is the dominant cost for wide neighborhoods.
const EDGE_ANIMATION_LIMIT = 60;

function useDetailGraph(
	topo: DetailTopology,
	selectedId: string | null,
): { nodes: Node[]; edges: Edge[] } {
	return useMemo(() => {
		const allEdges: Edge<FileEdgeData>[] = [...topo.fileEdges, ...topo.portalEdges];

		// When no selection (or the selected file is an orphan in the
		// drawer) hand React Flow the topology arrays unchanged. Reusing
		// references is what lets memo(FileNodeCard) skip rendering 100+
		// cards on every click.
		const selectionOnCanvas =
			selectedId != null && topo.nodes.some((n) => n.id === selectedId);
		if (!selectionOnCanvas) {
			return { nodes: topo.nodes, edges: allEdges };
		}
		const activeSelection = selectedId as string;

		const neighborIds = new Set<string>([activeSelection]);
		for (const e of allEdges) {
			if (e.source === activeSelection) neighborIds.add(e.target);
			if (e.target === activeSelection) neighborIds.add(e.source);
		}

		// Selection overlay rides on the wrapper's className — CSS (below,
		// in InnerFlow) targets the inner card via `> *` so the outline
		// hugs the card's actual bounds instead of the dagre-sized
		// wrapper box. Data reference stays identical, so memo() on the
		// card component still skips re-render.
		const styledNodes: Node[] = topo.nodes.map((node) => {
			const isSelected = node.id === activeSelection;
			const isNeighbor = !isSelected && neighborIds.has(node.id);
			const faded = !isSelected && !isNeighbor;
			const cn = isSelected ? "rf-sel" : isNeighbor ? "rf-nbr" : undefined;
			return {
				...node,
				className: cn,
				zIndex: isSelected ? 2 : 1,
				style: {
					...(node.style ?? {}),
					opacity: faded ? 0.2 : 1,
					transition: "opacity 200ms",
				},
			};
		});

		const animate = allEdges.length <= EDGE_ANIMATION_LIMIT;

		const styledEdges: Edge[] = allEdges.map((edge) => {
			const touches = edge.source === activeSelection || edge.target === activeSelection;
			if (touches) {
				const isPortalStub = edge.data?.edgeType === "PORTAL";
				const baseColor = isPortalStub
					? getPortalColor(edge, topo)
					: EDGE_COLORS[edge.data?.edgeType ?? ""] ?? DEFAULT_EDGE_COLOR;
				return {
					...edge,
					animated: animate,
					zIndex: 0,
					style: {
						...(edge.style ?? {}),
						stroke: baseColor,
						strokeWidth: 2.5,
						strokeDasharray: animate ? "6 4" : undefined,
						opacity: 1,
					},
				};
			}
			return {
				...edge,
				animated: false,
				zIndex: 0,
				style: {
					...(edge.style ?? {}),
					stroke: FADED_EDGE_COLOR,
					strokeWidth: 1,
					strokeDasharray: undefined,
					opacity: 0.08,
				},
			};
		});

		return { nodes: styledNodes, edges: styledEdges };
	}, [topo, selectedId]);
}

function getPortalColor(edge: Edge<FileEdgeData>, _topo: DetailTopology): string {
	// Portal edges already carry their target-layer color on their style.
	const style = edge.style as React.CSSProperties | undefined;
	return (style?.stroke as string) ?? DEFAULT_EDGE_COLOR;
}

// --- fitView helpers --------------------------------------------------

function CanvasFitView({ signal, isHuge }: { signal: string; isHuge: boolean }) {
	const { fitView } = useReactFlow();
	const prev = useRef<string>("");
	useEffect(() => {
		if (prev.current === signal) return;
		prev.current = signal;
		const id = requestAnimationFrame(() => {
			fitView({ padding: 0.2, duration: isHuge ? 0 : 300 });
		});
		return () => cancelAnimationFrame(id);
	}, [signal, isHuge, fitView]);
	return null;
}

function SelectionFitView({ selectedId, onCanvas }: { selectedId: string | null; onCanvas: boolean }) {
	const { fitView } = useReactFlow();
	const prev = useRef<string | null>(null);
	useEffect(() => {
		if (!selectedId || !onCanvas || selectedId === prev.current) {
			prev.current = onCanvas ? selectedId : prev.current;
			return;
		}
		prev.current = selectedId;
		const t = setTimeout(() => {
			fitView({ nodes: [{ id: selectedId }], duration: 400, padding: 0.4, maxZoom: 1.2, minZoom: 0.2 });
		}, 80);
		return () => clearTimeout(t);
	}, [selectedId, onCanvas, fitView]);
	return null;
}

// --- Inner (inside ReactFlowProvider) ---------------------------------

function InnerFlow({ projectId, graph, selectedNode, onSelectFile }: Props) {
	const { totalFiles, fileToLayer } = graph;

	// Resolve an incoming selection (from the search bar, the sidebar, or
	// a click) into the two actionable views of it:
	//
	//   • `fileId` — the file qname whose card should be highlighted. Only
	//     set for direct File selections or symbol selections where we can
	//     find the parent file by matching `source_file`.
	//   • `layerId` — the layer to drill into. Set for Folder selections
	//     and derived from `fileId` when that's what came in.
	//
	// Folders have no card of their own in this view; selecting one just
	// drills into the matching layer. Symbols have no card either but their
	// containing file does — we redirect to that card so the search-bar UX
	// always feels like a navigation.
	const selectionTarget = useMemo(() => {
		if (!selectedNode) return null;
		if (selectedNode.label === "Folder") {
			const layerId = firstSegment(selectedNode.source_file);
			return { fileId: null as string | null, layerId };
		}
		if (selectedNode.label === "File") {
			const fileId = selectedNode.qualified_name;
			return { fileId, layerId: fileToLayer.get(fileId) ?? null };
		}
		const path = selectedNode.source_file;
		if (!path) return null;
		const parent = graph.fileNodes.find((f) => f.source_file === path);
		if (!parent) return null;
		const fileId = parent.qualified_name;
		return { fileId, layerId: fileToLayer.get(fileId) ?? null };
	}, [selectedNode, graph.fileNodes, fileToLayer]);

	const selectedFileId = selectionTarget?.fileId ?? null;

	const [navigationLevel, setNavigationLevel] = useState<"overview" | "detail">("overview");
	const [activeLayerId, setActiveLayerId] = useState<string | null>(null);
	const [nodeColors, setNodeColors] = useState<NodeColorOverrides>(() => loadNodeColors(projectId));
	const [drawerOpen, setDrawerOpen] = useState(false);

	useEffect(() => {
		setNodeColors(loadNodeColors(projectId));
		setNavigationLevel("overview");
		setActiveLayerId(null);
		setDrawerOpen(false);
	}, [projectId]);

	// Auto drill-in when something is selected from elsewhere (search bar,
	// right panel, file tree). Covers Folder selections (layer only, no
	// file card), File selections (drill + highlight), and symbol
	// selections (redirected to parent file by selectionTarget).
	useEffect(() => {
		const layerId = selectionTarget?.layerId;
		if (!layerId) return;
		if (navigationLevel !== "detail" || activeLayerId !== layerId) {
			setNavigationLevel("detail");
			setActiveLayerId(layerId);
		}
	}, [selectionTarget, navigationLevel, activeLayerId]);

	const drillIntoLayer = useCallback((layerId: string) => {
		setNavigationLevel("detail");
		setActiveLayerId(layerId);
	}, []);

	const backToOverview = useCallback(() => {
		setNavigationLevel("overview");
		setActiveLayerId(null);
		setDrawerOpen(false);
		onSelectFile(null);
	}, [onSelectFile]);

	const handleRecolorNode = useCallback((fileQname: string, color: string | null) => {
		setNodeColors((prev) => {
			const next = { ...prev };
			if (color === null) delete next[fileQname];
			else next[fileQname] = color;
			saveNodeColor(projectId, fileQname, color);
			return next;
		});
	}, [projectId]);

	const overviewGraph = useOverviewGraph(graph, drillIntoLayer);
	const detailTopology = useDetailTopology(graph, activeLayerId, drillIntoLayer, handleRecolorNode, nodeColors);
	const detailGraph = useDetailGraph(detailTopology, selectedFileId);

	// If the selected file has no intra-layer relationships it lives in the
	// Other-files drawer, not the canvas. Open the drawer automatically so
	// a search-bar selection is visible without a manual toggle.
	useEffect(() => {
		if (!selectedFileId) return;
		const isOrphan = detailTopology.isolatedFiles.some(
			(f) => f.qualified_name === selectedFileId,
		);
		if (isOrphan) setDrawerOpen(true);
	}, [selectedFileId, detailTopology.isolatedFiles]);

	const { nodes: displayNodes, edges: displayEdges } =
		navigationLevel === "overview" ? overviewGraph : detailGraph;

	const isHuge = displayNodes.length > HUGE_GRAPH_NODES;

	const activeLayerName = useMemo(() => {
		if (navigationLevel !== "detail" || !activeLayerId) return null;
		return graph.layers.find((l) => l.id === activeLayerId)?.name ?? activeLayerId;
	}, [navigationLevel, activeLayerId, graph.layers]);

	const onNodeClick: NodeMouseHandler = useCallback((_event, node) => {
		if (node.type === "layerCluster") {
			const d = node.data as LayerClusterData;
			drillIntoLayer(d.layerId);
			return;
		}
		if (node.type === "portal") {
			const d = node.data as PortalNodeData;
			drillIntoLayer(d.targetLayerId);
			return;
		}
		if (node.type === "file" || node.type === "fileCompact") {
			const d = node.data as FileNodeData | undefined;
			if (d?.file) onSelectFile(d.file);
			return;
		}
	}, [drillIntoLayer, onSelectFile]);

	const onPaneClick = useCallback(() => {
		if (selectedNode) onSelectFile(null);
	}, [selectedNode, onSelectFile]);

	const orphanCount = detailTopology.isolatedFiles.length;
	const showDrawer = navigationLevel === "detail" && drawerOpen && orphanCount > 0;
	const selectionOnCanvas = useMemo(
		() => selectedFileId != null && displayNodes.some((n) => n.id === selectedFileId),
		[selectedFileId, displayNodes],
	);

	const layoutSignal = useMemo(
		() => `${navigationLevel}:${activeLayerId ?? ""}:${displayNodes.length}:${showDrawer}`,
		[navigationLevel, activeLayerId, displayNodes.length, showDrawer],
	);

	return (
		<div className="relative flex h-full w-full flex-col bg-app">
			<div className="flex items-center gap-3 border-b border-app-line bg-app-darkBox px-4 py-1.5 text-[10px] text-ink-faint">
				<Breadcrumb activeLayerName={activeLayerName} onBackToOverview={backToOverview} />
				<span className="ml-auto">
					{totalFiles.toLocaleString()} {totalFiles === 1 ? "file" : "files"} ·{" "}
					{graph.layers.length.toLocaleString()} {graph.layers.length === 1 ? "layer" : "layers"}
				</span>
				{navigationLevel === "detail" && orphanCount > 0 && (
					<button
						type="button"
						onClick={() => setDrawerOpen((v) => !v)}
						title={drawerOpen ? "Hide other files" : "Show files without relationships"}
						className={
							"flex items-center gap-1.5 rounded border px-2 py-0.5 font-medium uppercase tracking-wider transition-colors " +
							(drawerOpen
								? "border-accent/60 bg-accent/15 text-ink"
								: "border-app-line text-ink-dull hover:border-app-line/80 hover:text-ink")
						}
					>
						<svg viewBox="0 0 16 16" fill="currentColor" className="h-2.5 w-2.5">
							<path d="M2 3h12v2H2zm0 4h12v2H2zm0 4h12v2H2z" />
						</svg>
						Other files ({orphanCount})
					</button>
				)}
			</div>

			<div className="relative flex min-h-0 flex-1">
				<div className="relative min-w-0 flex-1">
				<style>{`
					.react-flow__node.rf-sel > * {
						outline: 2px solid rgba(251,191,36,0.95);
						outline-offset: 2px;
						box-shadow: 0 0 0 6px rgba(251,191,36,0.15);
					}
					.react-flow__node.rf-nbr > * {
						outline: 1px solid rgba(251,191,36,0.45);
						outline-offset: 2px;
					}
				`}</style>
				<ReactFlow
					nodes={displayNodes}
					edges={displayEdges}
					onNodeClick={onNodeClick}
					onPaneClick={onPaneClick}
					nodeTypes={NODE_TYPES}
					nodesDraggable={false}
					nodesConnectable={false}
					elementsSelectable={false}
					edgesFocusable={false}
					onlyRenderVisibleElements
					proOptions={{ hideAttribution: true }}
					colorMode="dark"
					minZoom={0.05}
					maxZoom={2}
					fitView
					fitViewOptions={{ padding: 0.15 }}
				>
					<Background variant={BackgroundVariant.Dots} gap={20} size={1} color="rgba(180,180,200,0.12)" />
					<Controls showInteractive={false} />
					{!isHuge && (
						<MiniMap
							pannable
							zoomable
							nodeStrokeWidth={2}
							nodeColor={(n) => {
								if (n.type === "layerCluster") {
									const d = n.data as LayerClusterData;
									return getCommunityColor(d.colorIndex % COMMUNITY_COLORS.length);
								}
								if (n.type === "portal") {
									const d = n.data as PortalNodeData;
									return getCommunityColor(d.colorIndex % COMMUNITY_COLORS.length);
								}
								const d = n.data as FileNodeData & { colorOverride?: string };
								return d?.colorOverride ?? "#3b82f6";
							}}
							maskColor="rgba(10,10,15,0.7)"
							style={{ background: "rgba(20,20,30,0.9)" }}
						/>
					)}
					<CanvasFitView signal={layoutSignal} isHuge={isHuge} />
					<SelectionFitView selectedId={selectedFileId} onCanvas={selectionOnCanvas} />
				</ReactFlow>
				</div>
				{showDrawer && (
					<OrphanDrawer
						files={detailTopology.isolatedFiles}
						selectedFileId={selectedFileId}
						nodeColors={nodeColors}
						onSelect={onSelectFile}
						onClose={() => setDrawerOpen(false)}
					/>
				)}
			</div>
		</div>
	);
}

export function MermaidView(props: Props) {
	return (
		<ReactFlowProvider>
			<InnerFlow {...props} />
		</ReactFlowProvider>
	);
}
