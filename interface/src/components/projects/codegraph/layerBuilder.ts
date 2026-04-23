// Folder-based layer taxonomy for the mermaid view. Each file is bucketed
// by the first segment of its source_file path (e.g. interface/, src/,
// tests/). Cross-layer file-to-file edges are aggregated into a single
// bundle per (sourceLayer, targetLayer) pair so the overview renders tens
// of bundles instead of thousands of edges.

import type { BulkNode } from "./types";

export interface Layer {
	id: string;
	name: string;
	fileCount: number;
	fileQnames: Set<string>;
	colorIndex: number;
}

export interface CrossLayerBundle {
	id: string;
	sourceLayerId: string;
	targetLayerId: string;
	count: number;
	types: Record<string, number>;
}

export interface AggregatedEdge {
	fromQname: string;
	toQname: string;
	type: string;
	count: number;
}

export interface BuildLayersResult {
	layers: Layer[];
	fileToLayer: Map<string, string>;
	crossLayerBundles: CrossLayerBundle[];
}

export interface PortalInfo {
	layerId: string;
	layerName: string;
	connectionCount: number;
	colorIndex: number;
}

export interface PortalEdgeStub {
	fileQname: string;
	targetLayerId: string;
}

// Files with no directory (e.g. README.md at root) land in "root" so the
// layer set is total: every file qualifies for exactly one layer.
function firstSegment(path: string | undefined): string {
	if (!path) return "root";
	const clean = path.replace(/\\/g, "/").replace(/^\/+/, "");
	const idx = clean.indexOf("/");
	if (idx === -1) return "root";
	const seg = clean.slice(0, idx);
	return seg || "root";
}

export function buildLayers(
	files: BulkNode[],
	aggregated: AggregatedEdge[],
): BuildLayersResult {
	const fileToLayer = new Map<string, string>();
	const byLayer = new Map<string, { name: string; files: Set<string> }>();

	for (const f of files) {
		const name = firstSegment(f.source_file);
		const id = name;
		fileToLayer.set(f.qualified_name, id);
		const entry = byLayer.get(id) ?? { name, files: new Set<string>() };
		entry.files.add(f.qualified_name);
		byLayer.set(id, entry);
	}

	// Stable color index by sorted name so reloads don't reshuffle colors.
	const sortedNames = Array.from(byLayer.keys()).sort((a, b) => a.localeCompare(b));
	const colorIndexByLayer = new Map<string, number>();
	sortedNames.forEach((name, i) => colorIndexByLayer.set(name, i));

	const layers: Layer[] = Array.from(byLayer.entries()).map(([id, { name, files }]) => ({
		id,
		name,
		fileCount: files.size,
		fileQnames: files,
		colorIndex: colorIndexByLayer.get(id) ?? 0,
	}));
	layers.sort((a, b) => b.fileCount - a.fileCount || a.name.localeCompare(b.name));

	const bundleMap = new Map<string, CrossLayerBundle>();
	for (const edge of aggregated) {
		const sl = fileToLayer.get(edge.fromQname);
		const tl = fileToLayer.get(edge.toQname);
		if (!sl || !tl || sl === tl) continue;
		const key = `${sl}\u0001${tl}`;
		const existing = bundleMap.get(key);
		if (existing) {
			existing.count += edge.count;
			existing.types[edge.type] = (existing.types[edge.type] ?? 0) + edge.count;
		} else {
			bundleMap.set(key, {
				id: `lbundle:${sl}__${tl}`,
				sourceLayerId: sl,
				targetLayerId: tl,
				count: edge.count,
				types: { [edge.type]: edge.count },
			});
		}
	}

	return {
		layers,
		fileToLayer,
		crossLayerBundles: Array.from(bundleMap.values()),
	};
}

// For detail view: every other layer reachable from activeLayerId via at
// least one cross-layer edge becomes a "portal". Each in-layer file that
// participates in a cross-layer edge gets a portal stub so the detail
// canvas can draw a short dashed lead-out toward the target layer.
export function findCrossLayerNeighbors(
	aggregated: AggregatedEdge[],
	fileToLayer: Map<string, string>,
	layers: Layer[],
	activeLayerId: string,
): { portals: PortalInfo[]; portalEdges: PortalEdgeStub[] } {
	const layerById = new Map(layers.map((l) => [l.id, l]));
	const portalCounts = new Map<string, number>();
	const portalEdges: PortalEdgeStub[] = [];

	for (const edge of aggregated) {
		const src = fileToLayer.get(edge.fromQname);
		const tgt = fileToLayer.get(edge.toQname);
		if (!src || !tgt) continue;
		if (src === activeLayerId && tgt !== activeLayerId) {
			portalCounts.set(tgt, (portalCounts.get(tgt) ?? 0) + edge.count);
			portalEdges.push({ fileQname: edge.fromQname, targetLayerId: tgt });
		} else if (tgt === activeLayerId && src !== activeLayerId) {
			portalCounts.set(src, (portalCounts.get(src) ?? 0) + edge.count);
			portalEdges.push({ fileQname: edge.toQname, targetLayerId: src });
		}
	}

	const portals: PortalInfo[] = [];
	for (const [layerId, count] of portalCounts) {
		const layer = layerById.get(layerId);
		if (!layer) continue;
		portals.push({
			layerId,
			layerName: layer.name,
			connectionCount: count,
			colorIndex: layer.colorIndex,
		});
	}
	portals.sort((a, b) => b.connectionCount - a.connectionCount);
	return { portals, portalEdges };
}
