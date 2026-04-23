// Top search bar for the Code Graph tab. Instant client-side filter over
// the loaded node list with a dropdown of matches. Cmd+K focuses the
// input. Selecting a result focuses the graph and opens the inspector.

import { useDeferredValue, useEffect, useMemo, useRef, useState } from "react";
import { clsx } from "clsx";
import { HugeiconsIcon } from "@hugeicons/react";
import { Search01Icon, RefreshIcon } from "@hugeicons/core-free-icons";
import { getNodeColor, type LayoutMode } from "./graphAdapter";
import type { NodeLabel } from "./constants";
import type { BulkNode } from "./types";

interface Props {
	nodes: BulkNode[];
	nodeCount: number;
	edgeCount: number;
	onSelectNode: (node: BulkNode) => void;
	onReindex?: () => void;
	isReindexing?: boolean;
	colorOverrides?: Record<string, string>;
	layoutMode: LayoutMode;
	onLayoutModeChange: (mode: LayoutMode) => void;
	/** Effective label visibility. On mermaid the caller passes the full
	 *  label set; on other layouts it mirrors the Filters-tab toggles, so
	 *  hiding "File" in the sidebar also hides File results here. */
	visibleLabels: NodeLabel[];
}

const LAYOUT_MODES: { key: LayoutMode; label: string }[] = [
	{ key: "force", label: "Force" },
	{ key: "solar", label: "Solar" },
	{ key: "radial", label: "Radial" },
	{ key: "mermaid", label: "Mermaid" },
];

const MAX_RESULTS = 30;

// Boost applied to Folder and File matches so they always rank above
// symbol matches at the same match tier. Without this, typing a query
// that matches many symbols exactly (e.g. "playlist" in a project where
// a variable and a method share that name) floods the top of the list
// with score-3 exacts and pushes the score-2 File prefix match
// (playlist.tsx) out of the MAX_RESULTS window.
const CONTAINER_BOOST = 10;

// Character-subsequence fuzzy match. Returns 0 if any query character
// is missing from `target`; otherwise a small positive score that
// rewards earlier matches and consecutive runs. Deliberately weaker than
// a substring hit — fuzzy is a typo-safety net ("plyist" still finds
// playlist), not the primary ranking signal.
function fuzzySubsequenceScore(target: string, query: string): number {
	let qi = 0;
	let consecutive = 0;
	let score = 0;
	for (let ti = 0; ti < target.length && qi < query.length; ti++) {
		if (target.charCodeAt(ti) === query.charCodeAt(qi)) {
			consecutive += 1;
			// Earlier match position + run length = stronger signal.
			score += consecutive + (ti === 0 ? 2 : 0);
			qi += 1;
		} else {
			consecutive = 0;
		}
	}
	return qi === query.length ? score : 0;
}

// Sort tiebreaker so when many nodes tie on relevance, the more
// "container-shaped" types (folders, files, classes) surface above deep
// symbols. Without this, a query like "mcp" can fill the dropdown with
// twenty `Method`s before any Folder or File match is visible.
const LABEL_PRIORITY: Record<string, number> = {
	Folder: 0,
	File: 1,
	Class: 2,
	Interface: 3,
	Enum: 4,
	Type: 5,
	Function: 6,
	Method: 7,
	Variable: 8,
	Decorator: 9,
	Import: 10,
};

export function CodeGraphSearchBar({
	nodes,
	nodeCount,
	edgeCount,
	onSelectNode,
	onReindex,
	isReindexing,
	colorOverrides,
	layoutMode,
	onLayoutModeChange,
	visibleLabels,
}: Props) {
	const [query, setQuery] = useState("");
	const [open, setOpen] = useState(false);
	const [selectedIndex, setSelectedIndex] = useState(0);
	const inputRef = useRef<HTMLInputElement>(null);
	const wrapperRef = useRef<HTMLDivElement>(null);

	// Cmd/Ctrl+K focuses the input; Escape closes the dropdown.
	useEffect(() => {
		const onKey = (e: KeyboardEvent) => {
			if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
				e.preventDefault();
				inputRef.current?.focus();
				setOpen(true);
			} else if (e.key === "Escape") {
				setOpen(false);
				inputRef.current?.blur();
			}
		};
		document.addEventListener("keydown", onKey);
		return () => document.removeEventListener("keydown", onKey);
	}, []);

	// Close on outside click.
	useEffect(() => {
		const onClick = (e: MouseEvent) => {
			if (wrapperRef.current && !wrapperRef.current.contains(e.target as Node)) {
				setOpen(false);
			}
		};
		document.addEventListener("mousedown", onClick);
		return () => document.removeEventListener("mousedown", onClick);
	}, []);

	// Prebuilt lowercase index of the node list. Rebuilt only when `nodes`
	// changes — not per keystroke — so a 15k-node graph doesn't pay the
	// toLowerCase cost on every character typed. Includes the label-tie
	// value for sort so the hot loop is purely numeric comparisons.
	const nodeIndex = useMemo(
		() =>
			nodes.map((n) => ({
				node: n,
				name: n.name?.toLowerCase() ?? "",
				qname: n.qualified_name?.toLowerCase() ?? "",
				tie: LABEL_PRIORITY[n.label] ?? 99,
			})),
		[nodes],
	);

	// Defer the query so keystrokes stay responsive — React displays the
	// last results while the new scan runs, then swaps. On large graphs
	// this is the difference between typing feeling laggy and instant.
	const deferredQuery = useDeferredValue(query);

	// Client-side search. Explicitly scans every node regardless of the
	// user's Filters-tab toggles — the search bar is the canonical way to
	// jump to any file / folder / function in the graph, so visibility
	// filters should never hide results. Matches are scored by how the
	// query lines up (exact > prefix > substring) and tiebroken by label
	// priority so Folder/File results aren't drowned out by deeper
	// symbols when many names collide. In mermaid mode, only Folder and
	// File nodes are navigable (symbols have no card on that canvas), so
	// the dropdown is restricted to those two labels.
	const isMermaid = layoutMode === "mermaid";
	const visibleLabelSet = useMemo(() => new Set<string>(visibleLabels), [visibleLabels]);
	const results = useMemo(() => {
		const raw = deferredQuery.trim();
		if (!raw) return [];
		const q = raw.toLowerCase();
		const scored: { node: BulkNode; score: number; tie: number }[] = [];
		for (const entry of nodeIndex) {
			if (isMermaid) {
				// Mermaid can only navigate to Folder/File cards.
				if (entry.node.label !== "Folder" && entry.node.label !== "File") continue;
			} else if (!visibleLabelSet.has(entry.node.label)) {
				// Honour the Filters-tab toggles on non-mermaid layouts.
				continue;
			}
			let score = 0;
			if (entry.name === q || entry.qname === q) score = 3;
			else if (entry.name.startsWith(q) || entry.qname.startsWith(q)) score = 2;
			else if (entry.name.includes(q) || entry.qname.includes(q)) score = 1;
			else {
				// No substring hit — try fuzzy subsequence as a typo fallback.
				// Normalized into (0, 1) so it always ranks below every real
				// tier above but still competes against other fuzzy matches.
				const fz = Math.max(
					fuzzySubsequenceScore(entry.name, q),
					fuzzySubsequenceScore(entry.qname, q),
				);
				if (fz > 0) score = fz / (fz + 10);
			}
			if (score === 0) continue;
			if (entry.node.label === "Folder" || entry.node.label === "File") {
				score += CONTAINER_BOOST;
			}
			scored.push({ node: entry.node, score, tie: entry.tie });
		}
		scored.sort((a, b) => {
			if (b.score !== a.score) return b.score - a.score;
			if (a.tie !== b.tie) return a.tie - b.tie;
			return (a.node.name ?? "").localeCompare(b.node.name ?? "");
		});
		return scored.slice(0, MAX_RESULTS).map((s) => s.node);
	}, [deferredQuery, nodeIndex, isMermaid, visibleLabelSet]);

	const handleKeyDown = (e: React.KeyboardEvent) => {
		if (!open || results.length === 0) return;
		if (e.key === "ArrowDown") {
			e.preventDefault();
			setSelectedIndex((i) => Math.min(i + 1, results.length - 1));
		} else if (e.key === "ArrowUp") {
			e.preventDefault();
			setSelectedIndex((i) => Math.max(i - 1, 0));
		} else if (e.key === "Enter") {
			e.preventDefault();
			const picked = results[selectedIndex];
			if (picked) {
				onSelectNode(picked);
				setQuery("");
				setOpen(false);
				setSelectedIndex(0);
			}
		}
	};

	return (
		<div className="flex items-center gap-4 border-b border-app-line bg-app-darkBox px-4 py-2">
			{/* Search — fills all available space between sidebar and stats */}
			<div ref={wrapperRef} className="relative min-w-0 flex-1">
				<div className="flex items-center gap-2.5 rounded-lg border border-app-line bg-app px-3.5 py-2 transition-all focus-within:border-accent focus-within:ring-2 focus-within:ring-accent/20">
					<HugeiconsIcon icon={Search01Icon} className="h-4 w-4 flex-shrink-0 text-ink-faint" />
					<input
						ref={inputRef}
						type="text"
						placeholder="Search nodes..."
						value={query}
						onChange={(e) => {
							setQuery(e.target.value);
							setOpen(true);
							setSelectedIndex(0);
						}}
						onFocus={() => setOpen(true)}
						onKeyDown={handleKeyDown}
						className="flex-1 border-none bg-transparent text-sm text-ink outline-none placeholder:text-ink-faint"
					/>
				</div>

				{/* Dropdown */}
				{open && query.trim() && (
					<div className="absolute left-0 right-0 top-full z-50 mt-1 overflow-hidden rounded-xl border border-app-line bg-app-darkBox shadow-xl">
						{results.length === 0 ? (
							<div className="px-4 py-3 text-sm text-ink-faint">
								No nodes found for &ldquo;{query}&rdquo;
							</div>
						) : (
							<div className="max-h-80 overflow-y-auto scrollbar-app">
								{results.map((node, i) => {
									const color = getNodeColor(node.label, colorOverrides);
									return (
										<button
											key={node.id}
											onClick={() => {
												onSelectNode(node);
												setQuery("");
												setOpen(false);
												setSelectedIndex(0);
											}}
											className={clsx(
												"flex w-full items-center gap-3 px-4 py-2 text-left transition-colors",
												i === selectedIndex
													? "bg-accent/20 text-ink"
													: "text-ink-dull hover:bg-app-hover",
											)}
										>
											<span
												className="h-2.5 w-2.5 flex-shrink-0 rounded-full"
												style={{ backgroundColor: color }}
											/>
											<span className="flex-1 truncate text-sm font-medium">
												{node.name}
												{node.file_size != null && (
													<span className="ml-1.5 text-xs font-normal text-ink-faint">
														({node.file_size < 1024
															? `${node.file_size} B`
															: node.file_size < 1024 * 1024
															? `${(node.file_size / 1024).toFixed(1)} KB`
															: `${(node.file_size / (1024 * 1024)).toFixed(1)} MB`})
													</span>
												)}
											</span>
											<span className="rounded bg-app px-1.5 py-0.5 text-[10px] text-ink-faint">
												{node.label}
											</span>
										</button>
									);
								})}
							</div>
						)}
					</div>
				)}
			</div>

			{/* Layout mode tabs */}
			<div className="flex shrink-0 items-center gap-1 rounded-lg border border-app-line bg-app p-0.5">
				{LAYOUT_MODES.map((mode) => (
					<button
						key={mode.key}
						onClick={() => onLayoutModeChange(mode.key)}
						className={clsx(
							"rounded-md px-2.5 py-1 text-[11px] font-medium transition-colors",
							layoutMode === mode.key
								? "bg-accent/20 text-accent"
								: "text-ink-faint hover:text-ink-dull",
						)}
					>
						{mode.label}
					</button>
				))}
			</div>

			{/* Stats + Re-index — pushed to far right */}
			<div className="ml-auto flex shrink-0 items-center gap-4 text-xs text-ink-faint">
				<span>{nodeCount.toLocaleString()} nodes</span>
				<span>{edgeCount.toLocaleString()} edges</span>
				{onReindex && (
					<button
						onClick={onReindex}
						disabled={isReindexing}
						className={clsx(
							"flex items-center gap-1.5 rounded-md border px-2.5 py-1 text-xs font-medium transition-colors",
							isReindexing
								? "border-blue-500/30 bg-blue-500/10 text-blue-400"
								: "border-app-line bg-app text-ink-dull hover:bg-app-hover hover:text-ink",
						)}
						title="Re-index this project and refresh the graph"
					>
						<HugeiconsIcon
							icon={RefreshIcon}
							className={clsx("h-3.5 w-3.5", isReindexing && "animate-spin")}
						/>
						{isReindexing ? "Indexing..." : "Re-index"}
					</button>
				)}
			</div>
		</div>
	);
}
