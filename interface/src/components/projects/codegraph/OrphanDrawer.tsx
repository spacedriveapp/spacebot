// Right-side drawer for files that have no intra-layer or portal edges.
// Kept off the graph canvas because they carry no relationships to draw —
// showing them here as compact, searchable chips lets users scan large
// layers (hundreds of files) without an unbounded grid crowding the view.

import { useMemo, useState } from "react";
import { NODE_COLORS, type NodeLabel } from "./constants";
import type { NodeColorOverrides } from "./mermaidOverrides";
import type { BulkNode } from "./types";

interface Props {
	files: BulkNode[];
	selectedFileId: string | null;
	nodeColors: NodeColorOverrides;
	onSelect: (file: BulkNode) => void;
	onClose: () => void;
}

function basename(path: string | undefined, fallback: string): string {
	if (!path) return fallback;
	const clean = path.replace(/\\/g, "/");
	const idx = clean.lastIndexOf("/");
	return idx === -1 ? clean : clean.slice(idx + 1);
}

export function OrphanDrawer({ files, selectedFileId, nodeColors, onSelect, onClose }: Props) {
	const [query, setQuery] = useState("");

	const filtered = useMemo(() => {
		const q = query.trim().toLowerCase();
		if (!q) return files;
		return files.filter((f) => {
			const bn = basename(f.source_file, f.name).toLowerCase();
			if (bn.includes(q)) return true;
			return f.qualified_name.toLowerCase().includes(q);
		});
	}, [files, query]);

	return (
		<aside className="flex h-full w-[300px] shrink-0 flex-col border-l border-app-line bg-app-darkBox">
			<div className="flex items-start justify-between gap-2 border-b border-app-line px-3 py-2">
				<div className="min-w-0">
					<div className="text-[11px] font-semibold uppercase tracking-wider text-ink-dull">
						Other files
					</div>
					<div className="mt-0.5 text-[10px] text-ink-faint">
						{files.length} {files.length === 1 ? "file" : "files"} with no relationships
					</div>
				</div>
				<button
					type="button"
					onClick={onClose}
					title="Hide drawer"
					className="shrink-0 rounded p-1 text-ink-faint transition-colors hover:bg-app-hover hover:text-ink"
				>
					<svg viewBox="0 0 16 16" fill="currentColor" className="h-3 w-3">
						<path d="M4.3 3.3a1 1 0 0 1 1.4 0L8 5.6l2.3-2.3a1 1 0 1 1 1.4 1.4L9.4 7l2.3 2.3a1 1 0 1 1-1.4 1.4L8 8.4l-2.3 2.3a1 1 0 1 1-1.4-1.4L6.6 7 4.3 4.7a1 1 0 0 1 0-1.4z" />
					</svg>
				</button>
			</div>
			<div className="border-b border-app-line p-2">
				<input
					type="text"
					value={query}
					onChange={(e) => setQuery(e.target.value)}
					placeholder="Filter files…"
					className="w-full rounded border border-app-line bg-app px-2 py-1 text-[11px] text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
				/>
			</div>
			<div className="min-h-0 flex-1 overflow-y-auto px-1 py-1">
				{filtered.length === 0 ? (
					<div className="px-3 py-6 text-center text-[10px] text-ink-faint">
						No files match "{query}"
					</div>
				) : (
					<ul className="space-y-0.5">
						{filtered.map((f) => {
							const isSelected = f.qualified_name === selectedFileId;
							const labelColor = NODE_COLORS[f.label as NodeLabel] ?? "#3b82f6";
							const color = nodeColors[f.qualified_name] ?? labelColor;
							const name = basename(f.source_file, f.name);
							return (
								<li key={f.qualified_name}>
									<button
										type="button"
										onClick={() => onSelect(f)}
										title={f.source_file ?? f.qualified_name}
										className={
											"flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-[11px] transition-colors " +
											(isSelected
												? "bg-accent/15 text-ink ring-1 ring-accent/40"
												: "text-ink-dull hover:bg-app-hover hover:text-ink")
										}
									>
										<span
											aria-hidden
											className="h-2 w-2 shrink-0 rounded-full"
											style={{ background: color }}
										/>
										<span className="min-w-0 flex-1 truncate">{name}</span>
										<span
											className="shrink-0 font-semibold uppercase tracking-wider text-[9px]"
											style={{ color: labelColor, opacity: 0.75 }}
										>
											{f.label}
										</span>
									</button>
								</li>
							);
						})}
					</ul>
				)}
			</div>
		</aside>
	);
}
