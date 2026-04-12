// Sigma.js wrapper hook. Owns a single Sigma instance, runs ForceAtlas2
// in a web worker for layout, and wires up selection / hover / filter
// reducers so the canvas updates without re-instantiating Sigma.
//
// Ported from reference/GitNexus/gitnexus-web/src/hooks/useSigma.ts with
// AI-specific features (blast radius, query animations, node pulse) and
// the @sigma/edge-curve dependency removed. Uses the built-in
// EdgeArrowProgram that's already a transitive dep via sigma itself.

import { useRef, useEffect, useCallback, useState } from "react";
import Sigma from "sigma";
import { EdgeArrowProgram } from "sigma/rendering";
import Graph from "graphology";
import FA2Layout from "graphology-layout-forceatlas2/worker";
import forceAtlas2 from "graphology-layout-forceatlas2";
import type { SigmaNodeAttributes, SigmaEdgeAttributes } from "./graphAdapter";
import type { EdgeType } from "./constants";

// ---------------------------------------------------------------------------
// Color helpers — mix with the dark background to dim non-selected nodes.
// ---------------------------------------------------------------------------

const hexToRgb = (hex: string): { r: number; g: number; b: number } => {
	const result = /^#?([a-f\d]{2})([a-f\d]{2})([a-f\d]{2})$/i.exec(hex);
	return result
		? {
				r: parseInt(result[1], 16),
				g: parseInt(result[2], 16),
				b: parseInt(result[3], 16),
		  }
		: { r: 100, g: 100, b: 100 };
};

const rgbToHex = (r: number, g: number, b: number): string =>
	"#" +
	[r, g, b]
		.map((x) => {
			const hex = Math.max(0, Math.min(255, Math.round(x))).toString(16);
			return hex.length === 1 ? "0" + hex : hex;
		})
		.join("");

/** Fade `hex` toward the dark background by `amount` (0..1). */
const dimColor = (hex: string, amount: number): string => {
	const rgb = hexToRgb(hex);
	const bg = { r: 12, g: 12, b: 18 };
	return rgbToHex(
		bg.r + (rgb.r - bg.r) * amount,
		bg.g + (rgb.g - bg.g) * amount,
		bg.b + (rgb.b - bg.b) * amount,
	);
};

const brightenColor = (hex: string, factor: number): string => {
	const rgb = hexToRgb(hex);
	return rgbToHex(
		rgb.r + ((255 - rgb.r) * (factor - 1)) / factor,
		rgb.g + ((255 - rgb.g) * (factor - 1)) / factor,
		rgb.b + ((255 - rgb.b) * (factor - 1)) / factor,
	);
};

// ---------------------------------------------------------------------------
// Layout tuning — scaled to graph size so small graphs don't explode and
// large graphs still converge in reasonable time.
// ---------------------------------------------------------------------------

const getFA2Settings = (nodeCount: number) => {
	const isSmall = nodeCount < 500;
	const isMedium = nodeCount >= 500 && nodeCount < 2000;
	const isLarge = nodeCount >= 2000 && nodeCount < 10000;
	return {
		gravity: isSmall ? 0.8 : isMedium ? 0.5 : isLarge ? 0.3 : 0.15,
		scalingRatio: isSmall ? 15 : isMedium ? 30 : isLarge ? 60 : 100,
		slowDown: isSmall ? 1 : isMedium ? 2 : isLarge ? 3 : 5,
		barnesHutOptimize: nodeCount > 200,
		barnesHutTheta: isLarge ? 0.8 : 0.6,
		strongGravityMode: false,
		outboundAttractionDistribution: true,
		linLogMode: false,
		adjustSizes: true,
		edgeWeightInfluence: 1,
	};
};

/** Capped at 30s — the GitNexus original used 45s for huge graphs but
 *  spacebot's bulk endpoint is hard-capped at 15k nodes, so we don't
 *  need that long. */
const getLayoutDuration = (nodeCount: number): number => {
	if (nodeCount > 5000) return 30000;
	if (nodeCount > 2000) return 25000;
	if (nodeCount > 1000) return 20000;
	if (nodeCount > 500) return 15000;
	return 10000;
};

// ---------------------------------------------------------------------------
// Hook options + return types
// ---------------------------------------------------------------------------

export interface UseSigmaOptions {
	onNodeClick?: (nodeId: string) => void;
	onNodeHover?: (nodeId: string | null) => void;
	onStageClick?: () => void;
	visibleEdgeTypes?: EdgeType[];
}

export interface UseSigmaReturn {
	containerRef: React.RefObject<HTMLDivElement | null>;
	sigmaRef: React.RefObject<Sigma | null>;
	setGraph: (graph: Graph<SigmaNodeAttributes, SigmaEdgeAttributes>) => void;
	zoomIn: () => void;
	zoomOut: () => void;
	resetZoom: () => void;
	focusNode: (nodeId: string) => void;
	isLayoutRunning: boolean;
	startLayout: () => void;
	stopLayout: () => void;
	selectedNode: string | null;
	setSelectedNode: (nodeId: string | null) => void;
	refresh: () => void;
}

export const useSigma = (options: UseSigmaOptions = {}): UseSigmaReturn => {
	const containerRef = useRef<HTMLDivElement>(null);
	const sigmaRef = useRef<Sigma | null>(null);
	const graphRef = useRef<Graph<SigmaNodeAttributes, SigmaEdgeAttributes> | null>(null);
	const layoutRef = useRef<FA2Layout | null>(null);
	const selectedNodeRef = useRef<string | null>(null);
	const visibleEdgeTypesRef = useRef<EdgeType[] | null>(null);
	const layoutTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
	const [isLayoutRunning, setIsLayoutRunning] = useState(false);
	const [selectedNode, setSelectedNodeState] = useState<string | null>(null);

	useEffect(() => {
		visibleEdgeTypesRef.current = options.visibleEdgeTypes ?? null;
		sigmaRef.current?.refresh();
	}, [options.visibleEdgeTypes]);

	const setSelectedNode = useCallback((nodeId: string | null) => {
		selectedNodeRef.current = nodeId;
		setSelectedNodeState(nodeId);
		const sigma = sigmaRef.current;
		if (!sigma) return;
		// Tiny camera nudge to force edge refresh — Sigma caches edges
		// aggressively and the reducer won't re-run without a scene change.
		const camera = sigma.getCamera();
		const ratio = camera.ratio;
		camera.animate({ ratio: ratio * 1.0001 }, { duration: 50 });
		sigma.refresh();
	}, []);

	// Initialize Sigma once on mount.
	useEffect(() => {
		if (!containerRef.current) return;

		const graph = new Graph<SigmaNodeAttributes, SigmaEdgeAttributes>();
		graphRef.current = graph;

		const sigma = new Sigma(graph, containerRef.current, {
			allowInvalidContainer: true,
			renderLabels: true,
			labelFont: "JetBrains Mono, ui-monospace, monospace",
			labelSize: 11,
			labelWeight: "500",
			labelColor: { color: "#e4e4ed" },
			labelRenderedSizeThreshold: 8,
			labelDensity: 0.1,
			labelGridCellSize: 70,
			defaultNodeColor: "#6b7280",
			defaultEdgeColor: "#2a2a3a",
			defaultEdgeType: "arrow",
			edgeProgramClasses: { arrow: EdgeArrowProgram },
			minCameraRatio: 0.002,
			maxCameraRatio: 50,
			hideEdgesOnMove: true,
			zIndex: true,

			// Custom hover label — pill background tinted with the node's color.
			defaultDrawNodeHover: (context, data, settings) => {
				const label = data.label;
				if (!label) return;
				const size = settings.labelSize || 11;
				const font = settings.labelFont || "JetBrains Mono, monospace";
				const weight = settings.labelWeight || "500";
				const nodeColor = data.color || "#6366f1";
				context.font = `${weight} ${size}px ${font}`;
				const textWidth = context.measureText(label).width;
				const nodeSize = data.size || 8;
				const x = data.x;
				const y = data.y - nodeSize - 12;
				const paddingX = 10;
				const paddingY = 6;
				const height = size + paddingY * 2;
				const width = textWidth + paddingX * 2;
				const radius = 6;
				// Tinted background — node color at 35% over dark base
				const rgb = hexToRgb(nodeColor);
				const bg = { r: 12, g: 12, b: 18 };
				const mix = 0.35;
				context.fillStyle = rgbToHex(
					bg.r + (rgb.r - bg.r) * mix,
					bg.g + (rgb.g - bg.g) * mix,
					bg.b + (rgb.b - bg.b) * mix,
				);
				context.beginPath();
				context.roundRect(x - width / 2, y - height / 2, width, height, radius);
				context.fill();
				// Colored border
				context.strokeStyle = nodeColor;
				context.lineWidth = 1.5;
				context.stroke();
				// Label text
				context.fillStyle = "#f5f5f7";
				context.textAlign = "center";
				context.textBaseline = "middle";
				context.fillText(label, x, y);
				// Glow ring around the node
				context.beginPath();
				context.arc(data.x, data.y, nodeSize + 4, 0, Math.PI * 2);
				context.strokeStyle = nodeColor;
				context.lineWidth = 2;
				context.globalAlpha = 0.5;
				context.stroke();
				context.globalAlpha = 1;
			},

			// Node reducer — dims unselected nodes when a node is selected.
			nodeReducer: (node, data) => {
				const res = { ...data };
				if (data.hidden) {
					res.hidden = true;
					return res;
				}
				const current = selectedNodeRef.current;
				if (current) {
					const g = graphRef.current;
					if (g) {
						const isSelected = node === current;
						const isNeighbor = g.hasEdge(node, current) || g.hasEdge(current, node);
						if (isSelected) {
							res.size = (data.size || 8) * 1.8;
							res.zIndex = 2;
							res.highlighted = true;
						} else if (isNeighbor) {
							res.size = (data.size || 8) * 1.3;
							res.zIndex = 1;
						} else {
							res.color = dimColor(data.color, 0.25);
							res.size = (data.size || 8) * 0.6;
							res.zIndex = 0;
						}
					}
				}
				return res;
			},

			// Edge reducer — hides edges by type, brightens edges touching
			// the selected node.
			edgeReducer: (edge, data) => {
				const res = { ...data };
				const allowedTypes = visibleEdgeTypesRef.current;
				if (allowedTypes && data.relationType) {
					if (!allowedTypes.includes(data.relationType as EdgeType)) {
						res.hidden = true;
						return res;
					}
				}
				const current = selectedNodeRef.current;
				if (current) {
					const g = graphRef.current;
					if (g) {
						const [source, target] = g.extremities(edge);
						const isConnected = source === current || target === current;
						if (isConnected) {
							res.color = brightenColor(data.color, 1.5);
							res.size = Math.max(3, (data.size || 1) * 4);
							res.zIndex = 2;
						} else {
							res.color = dimColor(data.color, 0.1);
							res.size = 0.3;
							res.zIndex = 0;
						}
					}
				}
				return res;
			},
		});

		sigmaRef.current = sigma;

		sigma.on("clickNode", ({ node }) => {
			setSelectedNode(node);
			options.onNodeClick?.(node);
		});

		sigma.on("clickStage", () => {
			setSelectedNode(null);
			options.onStageClick?.();
		});

		sigma.on("enterNode", ({ node }) => {
			options.onNodeHover?.(node);
			if (containerRef.current) {
				containerRef.current.style.cursor = "pointer";
			}
		});

		sigma.on("leaveNode", () => {
			options.onNodeHover?.(null);
			if (containerRef.current) {
				containerRef.current.style.cursor = "grab";
			}
		});

		return () => {
			if (layoutTimeoutRef.current) {
				clearTimeout(layoutTimeoutRef.current);
			}
			layoutRef.current?.kill();
			sigma.kill();
			sigmaRef.current = null;
			graphRef.current = null;
		};
		// eslint-disable-next-line react-hooks/exhaustive-deps
	}, []);

	const runLayout = useCallback((graph: Graph<SigmaNodeAttributes, SigmaEdgeAttributes>) => {
		const nodeCount = graph.order;
		if (nodeCount === 0) return;
		if (layoutRef.current) {
			layoutRef.current.kill();
			layoutRef.current = null;
		}
		if (layoutTimeoutRef.current) {
			clearTimeout(layoutTimeoutRef.current);
			layoutTimeoutRef.current = null;
		}
		const inferred = forceAtlas2.inferSettings(graph);
		const tuned = getFA2Settings(nodeCount);
		const settings = { ...inferred, ...tuned };
		const layout = new FA2Layout(graph, { settings });
		layoutRef.current = layout;
		layout.start();
		setIsLayoutRunning(true);
		const duration = getLayoutDuration(nodeCount);
		layoutTimeoutRef.current = setTimeout(() => {
			if (layoutRef.current) {
				layoutRef.current.stop();
				layoutRef.current = null;
				sigmaRef.current?.refresh();
				// Zoom in past the default bounding box so the graph fills
				// the canvas edge-to-edge like GitNexus. ratio < 1 = zoomed
				// in. 0.6 shows ~60% of the normalized coordinate space,
				// which makes the node cluster appear ~1.7× larger.
				sigmaRef.current?.getCamera().animate(
					{ x: 0.5, y: 0.5, ratio: 0.6, angle: 0 },
					{ duration: 400 },
				);
				setIsLayoutRunning(false);
			}
		}, duration);
	}, []);

	const setGraph = useCallback(
		(newGraph: Graph<SigmaNodeAttributes, SigmaEdgeAttributes>) => {
			const sigma = sigmaRef.current;
			if (!sigma) return;
			if (layoutRef.current) {
				layoutRef.current.kill();
				layoutRef.current = null;
			}
			if (layoutTimeoutRef.current) {
				clearTimeout(layoutTimeoutRef.current);
				layoutTimeoutRef.current = null;
			}
			graphRef.current = newGraph;
			sigma.setGraph(newGraph);
			setSelectedNode(null);
			runLayout(newGraph);
			// Don't reset camera here — the layout-stop callback will center
			// the camera after FA2 converges (otherwise we center on the
			// pre-layout positions and the graph drifts off-screen).
		},
		[runLayout, setSelectedNode],
	);

	const focusNode = useCallback((nodeId: string) => {
		const sigma = sigmaRef.current;
		const graph = graphRef.current;
		if (!sigma || !graph || !graph.hasNode(nodeId)) return;
		const already = selectedNodeRef.current === nodeId;
		selectedNodeRef.current = nodeId;
		setSelectedNodeState(nodeId);
		if (!already) {
			const attrs = graph.getNodeAttributes(nodeId);
			sigma.getCamera().animate({ x: attrs.x, y: attrs.y, ratio: 0.15 }, { duration: 400 });
		}
		sigma.refresh();
	}, []);

	const zoomIn = useCallback(() => {
		sigmaRef.current?.getCamera().animatedZoom({ duration: 200 });
	}, []);

	const zoomOut = useCallback(() => {
		sigmaRef.current?.getCamera().animatedUnzoom({ duration: 200 });
	}, []);

	const resetZoom = useCallback(() => {
		sigmaRef.current?.getCamera().animate(
			{ x: 0.5, y: 0.5, ratio: 0.6, angle: 0 },
			{ duration: 300 },
		);
		setSelectedNode(null);
	}, [setSelectedNode]);

	const startLayout = useCallback(() => {
		const graph = graphRef.current;
		if (!graph || graph.order === 0) return;
		runLayout(graph);
	}, [runLayout]);

	const stopLayout = useCallback(() => {
		if (layoutTimeoutRef.current) {
			clearTimeout(layoutTimeoutRef.current);
			layoutTimeoutRef.current = null;
		}
		if (layoutRef.current) {
			layoutRef.current.stop();
			layoutRef.current = null;
			sigmaRef.current?.refresh();
			setIsLayoutRunning(false);
		}
	}, []);

	const refresh = useCallback(() => {
		sigmaRef.current?.refresh();
	}, []);

	return {
		containerRef,
		sigmaRef,
		setGraph,
		zoomIn,
		zoomOut,
		resetZoom,
		focusNode,
		isLayoutRunning,
		startLayout,
		stopLayout,
		selectedNode,
		setSelectedNode,
		refresh,
	};
};
