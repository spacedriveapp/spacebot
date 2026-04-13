// Mermaid diagram view — renders the code graph as a flowchart using
// the mermaid library. Shows File nodes grouped by folder subgraphs
// with CALLS/IMPORTS/EXTENDS/IMPLEMENTS edges between them.

import { useEffect, useMemo, useRef, useState } from "react";
import mermaid from "mermaid";
import type { BulkNode } from "./types";
import type { CodeGraphBulkEdgeSummary } from "@/api/client";

interface Props {
	nodes: BulkNode[];
	edges: CodeGraphBulkEdgeSummary[];
}

// Max nodes to render — mermaid slows down on very large diagrams.
const MAX_NODES = 200;

// Edge types worth showing in the diagram. Structural edges (CONTAINS,
// DEFINES) are implied by subgraph nesting.
const VISIBLE_EDGE_TYPES = new Set(["CALLS", "IMPORTS", "EXTENDS", "IMPLEMENTS"]);

// Node labels to include. Skip metadata nodes.
const INCLUDE_LABELS = new Set([
	"File", "Function", "Method", "Class", "Interface", "Struct", "Trait",
	"Enum", "Impl", "Module", "Route",
]);

/** Sanitize a string for use as a mermaid node ID (alphanumeric + underscore). */
function mermaidId(s: string): string {
	return s.replace(/[^a-zA-Z0-9_]/g, "_").replace(/^_+|_+$/g, "") || "node";
}

/** Sanitize a string for use inside mermaid quotes. */
function mermaidLabel(s: string): string {
	return s.replace(/"/g, "'").replace(/[[\](){}]/g, "");
}

/** Generate mermaid flowchart syntax from bulk graph data. */
function generateMermaid(nodes: BulkNode[], edges: CodeGraphBulkEdgeSummary[]): { code: string; truncated: boolean } {
	// Filter to relevant nodes.
	let filtered = nodes.filter((n) => INCLUDE_LABELS.has(n.label));
	const truncated = filtered.length > MAX_NODES;
	if (truncated) {
		// Prioritize Files, then Classes, then Functions.
		const priority: Record<string, number> = { File: 0, Class: 1, Interface: 1, Struct: 1, Trait: 1, Enum: 1, Function: 2, Method: 2 };
		filtered.sort((a, b) => (priority[a.label] ?? 3) - (priority[b.label] ?? 3));
		filtered = filtered.slice(0, MAX_NODES);
	}

	const nodeSet = new Set(filtered.map((n) => n.qualified_name));

	// Group nodes by folder (first 2 path segments of source_file).
	const groups = new Map<string, BulkNode[]>();
	for (const node of filtered) {
		const parts = (node.source_file ?? "").split("/").filter(Boolean);
		const group = parts.length >= 2 ? `${parts[0]}/${parts[1]}` : parts[0] || "root";
		if (!groups.has(group)) groups.set(group, []);
		groups.get(group)!.push(node);
	}

	const lines: string[] = ["flowchart LR"];

	// Render subgraphs.
	for (const [group, members] of groups) {
		const groupId = mermaidId(group);
		lines.push(`  subgraph ${groupId}["${mermaidLabel(group)}"]`);
		for (const node of members) {
			const id = mermaidId(node.qualified_name);
			const shape = node.label === "File" ? `["${mermaidLabel(node.name)}"]`
				: node.label === "Class" || node.label === "Interface" || node.label === "Struct" || node.label === "Trait"
				? `[["${mermaidLabel(node.name)}"]]`
				: `("${mermaidLabel(node.name)}")`;
			lines.push(`    ${id}${shape}`);
		}
		lines.push("  end");
	}

	// Render edges.
	for (const edge of edges) {
		if (!VISIBLE_EDGE_TYPES.has(edge.edge_type)) continue;
		if (!nodeSet.has(edge.from_qname) || !nodeSet.has(edge.to_qname)) continue;
		const from = mermaidId(edge.from_qname);
		const to = mermaidId(edge.to_qname);
		if (from === to) continue;
		const label = edge.edge_type === "CALLS" ? "" : `|${edge.edge_type}|`;
		lines.push(`  ${from} -->${label} ${to}`);
	}

	return { code: lines.join("\n"), truncated };
}

// Initialize mermaid once with a dark theme.
let mermaidInitialized = false;
function ensureMermaidInit() {
	if (mermaidInitialized) return;
	mermaidInitialized = true;
	mermaid.initialize({
		startOnLoad: false,
		theme: "dark",
		themeVariables: {
			primaryColor: "#1e1e2e",
			primaryTextColor: "#cdd6f4",
			primaryBorderColor: "#6366f1",
			lineColor: "#585b70",
			secondaryColor: "#181825",
			tertiaryColor: "#11111b",
			fontFamily: "JetBrains Mono, ui-monospace, monospace",
			fontSize: "12px",
		},
		flowchart: {
			htmlLabels: true,
			curve: "basis",
		},
	});
}

export function MermaidView({ nodes, edges }: Props) {
	const containerRef = useRef<HTMLDivElement>(null);
	const [error, setError] = useState<string | null>(null);
	const [copied, setCopied] = useState(false);

	const { code, truncated } = useMemo(
		() => generateMermaid(nodes, edges),
		[nodes, edges],
	);

	useEffect(() => {
		if (!containerRef.current) return;
		ensureMermaidInit();

		let cancelled = false;
		const renderAsync = async () => {
			try {
				const id = `mermaid-${Date.now()}`;
				const { svg } = await mermaid.render(id, code);
				if (!cancelled && containerRef.current) {
					containerRef.current.innerHTML = svg;
					setError(null);
				}
			} catch (err) {
				if (!cancelled) {
					setError(err instanceof Error ? err.message : "Mermaid render failed");
				}
			}
		};

		renderAsync();
		return () => { cancelled = true; };
	}, [code]);

	const handleCopy = () => {
		navigator.clipboard.writeText(code).then(() => {
			setCopied(true);
			setTimeout(() => setCopied(false), 2000);
		});
	};

	return (
		<div className="flex h-full w-full flex-col bg-app">
			{/* Toolbar */}
			<div className="flex items-center gap-2 border-b border-app-line bg-app-darkBox px-4 py-1.5">
				{truncated && (
					<span className="text-[11px] text-amber-400">
						Diagram truncated to {MAX_NODES} nodes
					</span>
				)}
				<div className="ml-auto flex items-center gap-2">
					<button
						onClick={handleCopy}
						className="rounded px-2.5 py-1 text-[11px] text-ink-faint transition-colors hover:bg-app-hover hover:text-ink"
					>
						{copied ? "Copied!" : "Copy Mermaid"}
					</button>
				</div>
			</div>

			{/* Diagram */}
			{error ? (
				<div className="flex flex-1 items-center justify-center p-8">
					<div className="max-w-md text-center">
						<p className="text-sm font-medium text-red-400">Failed to render diagram</p>
						<p className="mt-2 text-xs text-ink-faint">{error}</p>
					</div>
				</div>
			) : (
				<div
					ref={containerRef}
					className="flex-1 overflow-auto p-6 [&_svg]:mx-auto [&_svg]:max-w-full"
				/>
			)}
		</div>
	);
}
