// Graph canvas host component. Owns the Sigma container, floating
// controls (zoom/fit/focus/reset/layout), hover pill, and the
// "selected node" top-center badge.
//
// Ported from reference/GitNexus/gitnexus-web/src/components/GraphCanvas.tsx
// with all AI features (query highlights, blast radius, chat FAB) removed.

import { forwardRef, useCallback, useEffect, useImperativeHandle, useMemo, useRef, useState } from "react";
import { HugeiconsIcon } from "@hugeicons/react";
import {
	PlusSignIcon,
	MinusSignIcon,
	Target02Icon,
	RefreshIcon,
	PlayIcon,
	PauseIcon,
	Cancel01Icon,
} from "@hugeicons/core-free-icons";
import { useSigma } from "./useSigma";
import {
	filterGraphByDepth, getNodeColor,
	applySolarLayout, applyRadialLayout, applyHierarchyLayout,
	type SigmaNodeAttributes, type SigmaEdgeAttributes, type LayoutMode,
} from "./graphAdapter";
import type { BulkNode } from "./types";
import type { EdgeType, NodeLabel } from "./constants";
import { nodeKey } from "./graphAdapter";
import type Graph from "graphology";

/** Apply a non-force layout to the graph by repositioning all nodes. */
function applyLayout(
	mode: LayoutMode,
	graph: Graph<SigmaNodeAttributes, SigmaEdgeAttributes>,
) {
	switch (mode) {
		case "solar": applySolarLayout(graph); break;
		case "radial": applyRadialLayout(graph); break;
		case "hierarchy": applyHierarchyLayout(graph); break;
	}
}

export interface GraphCanvasHandle {
	focusNode: (node: BulkNode) => void;
}

interface Props {
	graph: Graph<SigmaNodeAttributes, SigmaEdgeAttributes> | null;
	nodesByKey: Map<string, BulkNode>;
	selectedNode: BulkNode | null;
	onSelectNode: (node: BulkNode | null) => void;
	visibleLabels: NodeLabel[];
	visibleEdgeTypes: EdgeType[];
	depthFilter: number | null;
	colorOverrides?: Record<string, string>;
	layoutMode: LayoutMode;
	/** Notified whenever the FA2 worker starts or stops running. */
	onLayoutRunningChange?: (running: boolean) => void;
}

export const GraphCanvas = forwardRef<GraphCanvasHandle, Props>(function GraphCanvas(
	{
		graph,
		nodesByKey,
		selectedNode,
		onSelectNode,
		visibleLabels,
		visibleEdgeTypes,
		depthFilter,
		colorOverrides,
		layoutMode,
		onLayoutRunningChange,
	},
	ref,
) {
	const [hoveredNode, setHoveredNode] = useState<{ name: string; color: string } | null>(null);

	const handleNodeClick = useCallback(
		(gKey: string) => {
			const n = nodesByKey.get(gKey);
			if (n) onSelectNode(n);
		},
		[nodesByKey, onSelectNode],
	);

	const handleNodeHover = useCallback(
		(gKey: string | null) => {
			if (!gKey || !graph) {
				setHoveredNode(null);
				return;
			}
			const attrs = graph.getNodeAttributes(gKey);
			setHoveredNode(attrs.label ? { name: attrs.label, color: attrs.color } : null);
		},
		[graph],
	);

	const handleStageClick = useCallback(() => {
		onSelectNode(null);
	}, [onSelectNode]);

	const {
		containerRef,
		sigmaRef,
		setGraph: pushGraph,
		zoomIn,
		zoomOut,
		resetZoom,
		focusNode: sigmaFocusNode,
		setSelectedNode: setSigmaSelected,
		isLayoutRunning,
		startLayout,
		stopLayout,
	} = useSigma({
		onNodeClick: handleNodeClick,
		onNodeHover: handleNodeHover,
		onStageClick: handleStageClick,
		visibleEdgeTypes,
	});

	// Push the incoming graph into sigma whenever it changes. This also
	// kicks off a fresh ForceAtlas2 layout run (only in force mode).
	useEffect(() => {
		if (!graph) return;
		pushGraph(graph);
		// If initial mode isn't force, apply the correct layout immediately.
		if (layoutMode !== "force") {
			stopLayout();
			const sigmaGraph = sigmaRef.current?.getGraph() as Graph<SigmaNodeAttributes, SigmaEdgeAttributes> | undefined;
			if (sigmaGraph && sigmaGraph.order > 0) {
				applyLayout(layoutMode, sigmaGraph);
				sigmaRef.current?.refresh();
				sigmaRef.current?.getCamera().animate({ x: 0.5, y: 0.5, ratio: 0.6, angle: 0 }, { duration: 300 });
			}
		}
		// eslint-disable-next-line react-hooks/exhaustive-deps
	}, [graph, pushGraph]);

	// Apply layout when mode changes.
	const prevLayoutModeRef = useRef(layoutMode);
	useEffect(() => {
		if (prevLayoutModeRef.current === layoutMode) return;
		prevLayoutModeRef.current = layoutMode;
		const sigma = sigmaRef.current;
		if (!sigma || !graph) return;
		const g = sigma.getGraph() as Graph<SigmaNodeAttributes, SigmaEdgeAttributes>;
		if (g.order === 0) return;

		if (layoutMode === "force") {
			startLayout();
		} else {
			stopLayout();
			applyLayout(layoutMode, g);
			sigma.refresh();
			sigma.getCamera().animate({ x: 0.5, y: 0.5, ratio: 0.6, angle: 0 }, { duration: 400 });
		}
	}, [layoutMode, graph, startLayout, stopLayout, sigmaRef]);

	// Relay the layout-running flag to the parent so the status bar can
	// show it. Done via an effect so we don't spam during renders.
	useEffect(() => {
		onLayoutRunningChange?.(isLayoutRunning);
	}, [isLayoutRunning, onLayoutRunningChange]);

	// Keep the sigma selection in sync with the React state.
	useEffect(() => {
		setSigmaSelected(selectedNode ? nodeKey(selectedNode) : null);
	}, [selectedNode, setSigmaSelected]);

	// Update node colors in-place when user changes colors via the picker.
	useEffect(() => {
		const sigma = sigmaRef.current;
		if (!sigma || !graph || !colorOverrides) return;
		const g = sigma.getGraph() as Graph<SigmaNodeAttributes, SigmaEdgeAttributes>;
		if (g.order === 0) return;
		g.forEachNode((id, attrs) => {
			const newColor = getNodeColor(attrs.nodeType, colorOverrides);
			if (attrs.color !== newColor) {
				g.setNodeAttribute(id, "color", newColor);
			}
		});
		sigma.refresh();
		// eslint-disable-next-line react-hooks/exhaustive-deps
	}, [colorOverrides]);

	// Re-filter when labels/edges/depth change. We call directly into the
	// sigma graph instead of relying on the reducer because label
	// visibility gets stored as a node attribute.
	useEffect(() => {
		const sigma = sigmaRef.current;
		if (!sigma || !graph) return;
		const g = sigma.getGraph() as Graph<SigmaNodeAttributes, SigmaEdgeAttributes>;
		if (g.order === 0) return;
		filterGraphByDepth(
			g,
			selectedNode ? nodeKey(selectedNode) : null,
			depthFilter,
			visibleLabels,
		);
		sigma.refresh();
		// eslint-disable-next-line react-hooks/exhaustive-deps
	}, [visibleLabels, depthFilter, selectedNode, graph]);

	// Expose a focusNode handle for the search bar + sidebar.
	useImperativeHandle(
		ref,
		() => ({
			focusNode: (node: BulkNode) => {
				const key = nodeKey(node);
				if (!graph || !graph.hasNode(key)) return;
				onSelectNode(node);
				sigmaFocusNode(key);
			},
		}),
		[graph, onSelectNode, sigmaFocusNode],
	);

	const handleClearSelection = useCallback(() => {
		onSelectNode(null);
		resetZoom();
	}, [onSelectNode, resetZoom]);

	const handleFocusSelected = useCallback(() => {
		if (selectedNode) sigmaFocusNode(nodeKey(selectedNode));
	}, [selectedNode, sigmaFocusNode]);

	const selectionLabel = useMemo(() => selectedNode?.label ?? "", [selectedNode]);

	return (
		<div className="relative h-full w-full overflow-hidden bg-app">
			{/* Background gradient — subtle, matches GitNexus */}
			<div
				className="pointer-events-none absolute inset-0"
				style={{
					background:
						"radial-gradient(circle at 50% 50%, rgba(124, 58, 237, 0.03) 0%, transparent 70%), linear-gradient(to bottom, #06060a, #0a0a10)",
				}}
			/>

			{/* Sigma container */}
			<div
				ref={containerRef}
				className="h-full w-full cursor-grab active:cursor-grabbing"
			/>

			{/* Hover tooltip — only when nothing is selected */}
			{hoveredNode && !selectedNode && (
				<div
					className="pointer-events-none absolute left-1/2 top-4 z-20 -translate-x-1/2 rounded-lg border px-3 py-1.5 backdrop-blur-sm"
					style={{
						backgroundColor: `${hoveredNode.color}30`,
						borderColor: `${hoveredNode.color}60`,
					}}
				>
					<span className="font-mono text-sm text-ink">{hoveredNode.name}</span>
				</div>
			)}

			{/* Selection pill */}
			{selectedNode && (
				<div className="absolute left-1/2 top-4 z-20 flex -translate-x-1/2 items-center gap-2 rounded-xl border border-accent/30 bg-accent/20 px-4 py-2 backdrop-blur-sm">
					<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
					<span className="font-mono text-sm text-ink">{selectedNode.name}</span>
					<span className="text-xs text-ink-faint">({selectionLabel})</span>
					<button
						onClick={handleClearSelection}
						className="ml-2 rounded p-0.5 text-ink-dull transition-colors hover:bg-app-hover hover:text-ink"
						title="Clear selection"
					>
						<HugeiconsIcon icon={Cancel01Icon} className="h-3.5 w-3.5" />
					</button>
				</div>
			)}

			{/* Bottom-right floating controls */}
			<div className="absolute bottom-4 right-4 z-10 flex flex-col gap-1">
				<IconBtn onClick={zoomIn} title="Zoom In">
					<HugeiconsIcon icon={PlusSignIcon} className="h-4 w-4" />
				</IconBtn>
				<IconBtn onClick={zoomOut} title="Zoom Out">
					<HugeiconsIcon icon={MinusSignIcon} className="h-4 w-4" />
				</IconBtn>
				<IconBtn onClick={resetZoom} title="Fit to Screen">
					{/* Inline SVG for fit-to-screen — no matching hugeicon */}
					<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" className="h-4 w-4">
						<path d="M3 8V5a2 2 0 0 1 2-2h3M3 16v3a2 2 0 0 0 2 2h3M21 8V5a2 2 0 0 0-2-2h-3M21 16v3a2 2 0 0 1-2 2h-3" strokeLinecap="round" strokeLinejoin="round" />
					</svg>
				</IconBtn>

				<div className="my-1 h-px bg-app-line" />

				{selectedNode && (
					<IconBtn
						onClick={handleFocusSelected}
						title="Focus on Selected"
						variant="accent"
					>
						<HugeiconsIcon icon={Target02Icon} className="h-4 w-4" />
					</IconBtn>
				)}
				{selectedNode && (
					<IconBtn onClick={handleClearSelection} title="Clear Selection">
						<HugeiconsIcon icon={RefreshIcon} className="h-4 w-4" />
					</IconBtn>
				)}

				<div className="my-1 h-px bg-app-line" />

				<IconBtn
					onClick={isLayoutRunning ? stopLayout : startLayout}
					title={isLayoutRunning ? "Stop Layout" : "Run Layout"}
					variant={isLayoutRunning ? "running" : "default"}
				>
					{isLayoutRunning ? (
						<HugeiconsIcon icon={PauseIcon} className="h-4 w-4" />
					) : (
						<HugeiconsIcon icon={PlayIcon} className="h-4 w-4" />
					)}
				</IconBtn>
			</div>

			{/* Layout-running badge */}
			{isLayoutRunning && (
				<div className="absolute bottom-4 left-1/2 z-10 flex -translate-x-1/2 items-center gap-2 rounded-full border border-emerald-500/30 bg-emerald-500/20 px-3 py-1.5 backdrop-blur-sm">
					<div className="h-2 w-2 animate-ping rounded-full bg-emerald-400" />
					<span className="text-xs font-medium text-emerald-400">Layout optimizing...</span>
				</div>
			)}

			{/* Empty state */}
			{!graph && (
				<div className="absolute inset-0 flex items-center justify-center">
					<p className="text-sm text-ink-faint">Loading graph...</p>
				</div>
			)}
		</div>
	);
});

// ---------------------------------------------------------------------------
// Tiny shared button styling used only by this component. Inlined to avoid
// a one-off UI primitive file.
// ---------------------------------------------------------------------------

function IconBtn({
	children,
	onClick,
	title,
	variant = "default",
}: {
	children: React.ReactNode;
	onClick: () => void;
	title: string;
	variant?: "default" | "accent" | "running";
}) {
	const base =
		"flex h-9 w-9 items-center justify-center rounded-md border transition-colors";
	const variants = {
		default:
			"border-app-line bg-app-darkBox text-ink-dull hover:bg-app-hover hover:text-ink",
		accent: "border-accent/30 bg-accent/20 text-accent hover:bg-accent/30",
		running:
			"animate-pulse border-accent bg-accent text-white shadow-[0_0_20px_rgba(124,58,237,0.4)]",
	};
	return (
		<button onClick={onClick} title={title} className={`${base} ${variants[variant]}`}>
			{children}
		</button>
	);
}
