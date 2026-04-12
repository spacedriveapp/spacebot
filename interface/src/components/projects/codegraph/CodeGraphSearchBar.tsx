// Top search bar for the Code Graph tab. Instant client-side filter over
// the loaded node list with a dropdown of matches. Cmd+K focuses the
// input. Selecting a result focuses the graph and opens the inspector.

import { useEffect, useMemo, useRef, useState } from "react";
import { clsx } from "clsx";
import { HugeiconsIcon } from "@hugeicons/react";
import { Search01Icon, RefreshIcon } from "@hugeicons/core-free-icons";
import { NODE_COLORS, type NodeLabel } from "./constants";
import type { BulkNode } from "./types";

interface Props {
	nodes: BulkNode[];
	nodeCount: number;
	edgeCount: number;
	truncated: boolean;
	totalAvailable: number;
	onSelectNode: (node: BulkNode) => void;
	onReindex?: () => void;
	isReindexing?: boolean;
}

const MAX_RESULTS = 12;

export function CodeGraphSearchBar({
	nodes,
	nodeCount,
	edgeCount,
	truncated,
	totalAvailable,
	onSelectNode,
	onReindex,
	isReindexing,
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

	// Client-side filter. The bulk endpoint is small enough that scanning
	// every node per keystroke is fine.
	const results = useMemo(() => {
		if (!query.trim()) return [];
		const q = query.toLowerCase();
		const matches: BulkNode[] = [];
		for (const n of nodes) {
			if (matches.length >= MAX_RESULTS) break;
			if (n.name?.toLowerCase().includes(q) || n.qualified_name?.toLowerCase().includes(q)) {
				matches.push(n);
			}
		}
		return matches;
	}, [query, nodes]);

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
					<kbd className="rounded border border-app-line bg-app-darkBox px-1.5 py-0.5 font-mono text-[10px] text-ink-faint">
						⌘K
					</kbd>
				</div>

				{/* Dropdown */}
				{open && query.trim() && (
					<div className="absolute left-0 right-0 top-full z-50 mt-1 overflow-hidden rounded-xl border border-app-line bg-app-darkBox shadow-xl">
						{results.length === 0 ? (
							<div className="px-4 py-3 text-sm text-ink-faint">
								No nodes found for &ldquo;{query}&rdquo;
							</div>
						) : (
							<div className="max-h-80 overflow-y-auto">
								{results.map((node, i) => {
									const color = NODE_COLORS[node.label as NodeLabel] ?? "#6b7280";
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
											<span className="flex-1 truncate text-sm font-medium">{node.name}</span>
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

			{/* Stats + Re-index — pushed to far right */}
			<div className="ml-auto flex shrink-0 items-center gap-4 text-xs text-ink-faint">
				<span>{nodeCount.toLocaleString()} nodes</span>
				<span>{edgeCount.toLocaleString()} edges</span>
				{truncated && (
					<span
						className="rounded bg-amber-500/10 px-2 py-0.5 text-[10px] text-amber-400"
						title={`Showing ${nodeCount} of ${totalAvailable}. Toggle 'Include noise' or lower the cap to see more.`}
					>
						Truncated ({totalAvailable.toLocaleString()} total)
					</span>
				)}
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
