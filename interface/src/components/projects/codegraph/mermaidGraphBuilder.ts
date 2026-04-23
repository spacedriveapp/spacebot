// Pure helpers that transform the bulk code-graph payload into React Flow
// data for the mermaid view. Files become card nodes; symbol-level edges
// are lifted to file-to-file edges and aggregated so edge thickness
// scales with the number of underlying relationships. Layer metadata
// (folder-based) is computed alongside so the overview can render one
// cluster card per folder and the detail view can draw portals to the
// other layers.

import type { Edge, Node } from "@xyflow/react";
import type { CodeGraphBulkEdgeSummary } from "@/api/client";
import type { BulkNode } from "./types";
import {
	buildLayers,
	type AggregatedEdge,
	type CrossLayerBundle,
	type Layer,
} from "./layerBuilder";

const FILE_EDGE_TYPES = new Set(["IMPORTS", "CALLS", "EXTENDS", "IMPLEMENTS"]);

// Labels that define a child symbol displayed inside a file card.
const SYMBOL_LABELS = new Set([
	"Class",
	"Interface",
	"Struct",
	"Trait",
	"Enum",
	"Type",
	"TypeAlias",
	"Function",
	"Method",
	"Route",
	"Tool",
	"Record",
	"Template",
]);

export interface FileNodeData extends Record<string, unknown> {
	file: BulkNode;
	symbols: BulkNode[];
	selected?: boolean;
}

export interface FileEdgeData extends Record<string, unknown> {
	edgeType: string;
	count: number;
}

export interface BuildResult {
	nodes: Node<FileNodeData>[];
	edges: Edge<FileEdgeData>[];
	totalFiles: number;
	fileNodes: BulkNode[];
	symbolsByFile: Map<string, BulkNode[]>;
	fileDegree: Map<string, number>;
	layers: Layer[];
	fileToLayer: Map<string, string>;
	crossLayerBundles: CrossLayerBundle[];
	aggregatedEdges: AggregatedEdge[];
}

function edgeKey(from: string, to: string, type: string): string {
	return `${from}\u0001${to}\u0001${type}`;
}

export function buildFileGraph(
	allNodes: BulkNode[],
	allEdges: CodeGraphBulkEdgeSummary[],
): BuildResult {
	const files = allNodes.filter((n) => n.label === "File");

	const fileByPath = new Map<string, BulkNode>();
	for (const f of files) {
		if (f.source_file) fileByPath.set(f.source_file, f);
	}

	// Collect symbols per file (up to 6) so the card can list them inline.
	const symbolsByFile = new Map<string, BulkNode[]>();
	for (const node of allNodes) {
		if (!node.source_file) continue;
		if (!SYMBOL_LABELS.has(node.label)) continue;
		const parent = fileByPath.get(node.source_file);
		if (!parent) continue;
		const list = symbolsByFile.get(parent.qualified_name) ?? [];
		if (list.length < 6) list.push(node);
		symbolsByFile.set(parent.qualified_name, list);
	}

	// Map every node qname → its parent file qname so symbol edges can be
	// promoted to file-to-file edges.
	const qnameToFileQname = new Map<string, string>();
	for (const node of allNodes) {
		if (!node.source_file) continue;
		const parent = fileByPath.get(node.source_file);
		if (parent) qnameToFileQname.set(node.qualified_name, parent.qualified_name);
	}
	for (const f of files) qnameToFileQname.set(f.qualified_name, f.qualified_name);

	// Aggregate file-to-file edges per (from, to, type) triple.
	const aggregated = new Map<string, AggregatedEdge>();
	const fileDegree = new Map<string, number>();
	for (const edge of allEdges) {
		if (!FILE_EDGE_TYPES.has(edge.edge_type)) continue;
		const from = qnameToFileQname.get(edge.from_qname);
		const to = qnameToFileQname.get(edge.to_qname);
		if (!from || !to || from === to) continue;
		const key = edgeKey(from, to, edge.edge_type);
		const existing = aggregated.get(key);
		if (existing) {
			existing.count += 1;
		} else {
			aggregated.set(key, { fromQname: from, toQname: to, type: edge.edge_type, count: 1 });
			fileDegree.set(from, (fileDegree.get(from) ?? 0) + 1);
			fileDegree.set(to, (fileDegree.get(to) ?? 0) + 1);
		}
	}

	const aggregatedEdges = Array.from(aggregated.values());
	const { layers, fileToLayer, crossLayerBundles } = buildLayers(files, aggregatedEdges);

	const rfNodes: Node<FileNodeData>[] = files.map((file) => ({
		id: file.qualified_name,
		type: "file",
		position: { x: 0, y: 0 },
		data: {
			file,
			symbols: symbolsByFile.get(file.qualified_name) ?? [],
		},
	}));

	const rfEdges: Edge<FileEdgeData>[] = aggregatedEdges.map((e) => ({
		id: `${e.fromQname}__${e.type}__${e.toQname}`,
		source: e.fromQname,
		target: e.toQname,
		data: { edgeType: e.type, count: e.count },
	}));

	return {
		nodes: rfNodes,
		edges: rfEdges,
		totalFiles: files.length,
		fileNodes: files,
		symbolsByFile,
		fileDegree,
		layers,
		fileToLayer,
		crossLayerBundles,
		aggregatedEdges,
	};
}
