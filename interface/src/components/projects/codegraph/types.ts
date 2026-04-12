// Type aliases over the generated API types.
// Thin layer so component code doesn't have to reference schema.d.ts
// directly and so we can evolve the internal shapes without changing
// every call site.

import type { CodeGraphNodeSummary, CodeGraphBulkEdgeSummary } from "@/api/client";
import type { NodeLabel, EdgeType } from "./constants";

export type BulkNode = CodeGraphNodeSummary;
export type BulkEdge = CodeGraphBulkEdgeSummary;

/** Narrowed view where the label is a known `NodeLabel`. */
export interface BulkNodeTyped extends Omit<BulkNode, "label"> {
	label: NodeLabel;
}

/** Narrowed view where the edge type is a known `EdgeType`. */
export interface BulkEdgeTyped extends Omit<BulkEdge, "edge_type"> {
	edge_type: EdgeType;
}
