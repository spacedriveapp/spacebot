// Node and edge visual constants for the code graph canvas.
// Ported from reference/GitNexus/gitnexus-web/src/lib/constants.ts and
// extended to cover spacebot's 29-label schema.

export type NodeLabel =
	| "Project"
	| "Package"
	| "Module"
	| "Namespace"
	| "Folder"
	| "File"
	| "Class"
	| "Interface"
	| "Struct"
	| "Trait"
	| "Impl"
	| "Enum"
	| "Type"
	| "TypeAlias"
	| "Function"
	| "Method"
	| "Const"
	| "MacroDef"
	| "Record"
	| "Template"
	| "Test"
	| "Route"
	| "Section"
	| "Community"
	| "Process";

export type EdgeType =
	| "CONTAINS"
	| "DEFINES"
	| "CALLS"
	| "IMPORTS"
	| "EXTENDS"
	| "IMPLEMENTS"
	| "OVERRIDES"
	| "HAS_METHOD"
	| "MEMBER_OF"
	| "STEP_IN_PROCESS"
	| "TESTED_BY"
	| "HANDLES_TOOL"
	| "QUERIES";

// ---------------------------------------------------------------------------
// Colors — similar tokens reused across related labels.
// ---------------------------------------------------------------------------

export const NODE_COLORS: Record<NodeLabel, string> = {
	Project: "#a855f7",
	Package: "#8b5cf6",
	Module: "#7c3aed",
	Namespace: "#7c3aed",
	Folder: "#6366f1",
	File: "#3b82f6",
	Class: "#f59e0b",
	Interface: "#ec4899",
	Struct: "#f59e0b",
	Trait: "#ec4899",
	Impl: "#14b8a6",
	Enum: "#f97316",
	Type: "#a78bfa",
	TypeAlias: "#a78bfa",
	Function: "#10b981",
	Method: "#14b8a6",
	Const: "#64748b",
	MacroDef: "#eab308",
	Record: "#f59e0b",
	Template: "#a78bfa",
	Test: "#84cc16",
	Route: "#f43f5e",
	Section: "#60a5fa",
	Community: "#818cf8",
	Process: "#f43f5e",
};

// ---------------------------------------------------------------------------
// Sizes — larger = more visual weight in the force-directed layout.
// Community and Process are set to 0 because they're metadata, not visible
// graph nodes. The adapter hides them from the canvas entirely.
// ---------------------------------------------------------------------------

export const NODE_SIZES: Record<NodeLabel, number> = {
	Project: 20,
	Package: 16,
	Module: 13,
	Namespace: 13,
	Folder: 10,
	File: 6,
	Class: 8,
	Interface: 7,
	Struct: 8,
	Trait: 7,
	Impl: 3,
	Enum: 5,
	Type: 3,
	TypeAlias: 3,
	Function: 4,
	Method: 3,
	Const: 2,
	MacroDef: 2,
	Record: 8,
	Template: 3,
	Test: 4,
	Route: 5,
	Section: 6,
	Community: 0,
	Process: 0,
};

// Community color palette for cluster-based coloring of symbol nodes.
export const COMMUNITY_COLORS = [
	"#ef4444",
	"#f97316",
	"#eab308",
	"#22c55e",
	"#06b6d4",
	"#3b82f6",
	"#8b5cf6",
	"#d946ef",
	"#ec4899",
	"#f43f5e",
	"#14b8a6",
	"#84cc16",
];

export const getCommunityColor = (communityIndex: number): string => {
	return COMMUNITY_COLORS[communityIndex % COMMUNITY_COLORS.length];
};

// ---------------------------------------------------------------------------
// Default visibility — matches GitNexus's "structural + top-level symbols"
// default. Pipeline-only labels (Variable, Import, Parameter, Decorator)
// are deleted before finalization and don't appear in the final graph.
// ---------------------------------------------------------------------------

export const DEFAULT_VISIBLE_LABELS: NodeLabel[] = [
	"Project",
	"Package",
	"Module",
	"Namespace",
	"Folder",
	"File",
	"Class",
	"Interface",
	"Struct",
	"Trait",
	"Enum",
	"Type",
	"TypeAlias",
	"Function",
	"Method",
	"Impl",
	"Record",
	"Template",
	"Test",
	"Route",
	"Section",
	"Const",
	"MacroDef",
];

export const FILTERABLE_LABELS: NodeLabel[] = [
	"Folder",
	"File",
	"Class",
	"Interface",
	"Struct",
	"Trait",
	"Enum",
	"Type",
	"TypeAlias",
	"Function",
	"Method",
	"Impl",
	"Const",
	"MacroDef",
	"Record",
	"Template",
	"Test",
	"Route",
	"Section",
];

// ---------------------------------------------------------------------------
// Edge types and their display colors.
// ---------------------------------------------------------------------------

export const ALL_EDGE_TYPES: EdgeType[] = [
	"CONTAINS",
	"DEFINES",
	"IMPORTS",
	"CALLS",
	"EXTENDS",
	"IMPLEMENTS",
	"OVERRIDES",
	"HAS_METHOD",
	"MEMBER_OF",
	"STEP_IN_PROCESS",
	"TESTED_BY",
	"HANDLES_TOOL",
	"QUERIES",
];

// By default, hide edge types that tend to dominate (CALLS, ACCESSES,
// HAS_PARAMETER) so the initial view stays readable.
export const DEFAULT_VISIBLE_EDGES: EdgeType[] = [
	"CONTAINS",
	"DEFINES",
	"IMPORTS",
	"EXTENDS",
	"IMPLEMENTS",
	"OVERRIDES",
	"HAS_METHOD",
	"MEMBER_OF",
	"STEP_IN_PROCESS",
	"HANDLES_TOOL",
	"QUERIES",
];

export const EDGE_INFO: Record<EdgeType, { color: string; label: string }> = {
	CONTAINS: { color: "#2d5a3d", label: "Contains" },
	DEFINES: { color: "#0e7490", label: "Defines" },
	IMPORTS: { color: "#1d4ed8", label: "Imports" },
	CALLS: { color: "#7c3aed", label: "Calls" },
	EXTENDS: { color: "#c2410c", label: "Extends" },
	IMPLEMENTS: { color: "#be185d", label: "Implements" },
	OVERRIDES: { color: "#b45309", label: "Overrides" },
	HAS_METHOD: { color: "#0891b2", label: "Has Method" },
	MEMBER_OF: { color: "#6366f1", label: "Member Of" },
	STEP_IN_PROCESS: { color: "#e11d48", label: "Step In Process" },
	TESTED_BY: { color: "#65a30d", label: "Tested By" },
	HANDLES_TOOL: { color: "#9333ea", label: "Handles Tool" },
	QUERIES: { color: "#2563eb", label: "Queries" },
};
