// Brief one-line descriptions for layer clusters and file cards. Used as
// the secondary text line on every card so the graph reads at a glance.
// Deterministic heuristics only — no AI, no network.

import type { BulkNode } from "./types";
import type { Layer } from "./layerBuilder";

function formatBytes(bytes: number | null | undefined): string | null {
	if (bytes == null) return null;
	if (bytes < 1024) return `${bytes} B`;
	if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
	return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function fileExtension(path: string | undefined): string {
	if (!path) return "";
	const base = path.split(/[\\/]/).pop() ?? "";
	const dot = base.lastIndexOf(".");
	if (dot <= 0) return "";
	return base.slice(dot + 1).toLowerCase();
}

function pluralize(label: string, n: number): string {
	const lc = label.toLowerCase();
	if (n === 1) return lc;
	if (lc.endsWith("s")) return lc;
	if (lc.endsWith("y")) return `${lc.slice(0, -1)}ies`;
	return `${lc}s`;
}

export function describeFile(file: BulkNode, symbols: BulkNode[]): string {
	const bits: string[] = [];
	const size = formatBytes(file.file_size);
	if (size) bits.push(size);

	if (symbols.length > 0) {
		const counts = new Map<string, number>();
		for (const s of symbols) counts.set(s.label, (counts.get(s.label) ?? 0) + 1);
		const breakdown = Array.from(counts.entries())
			.sort((a, b) => b[1] - a[1])
			.slice(0, 3)
			.map(([label, n]) => `${n} ${pluralize(label, n)}`)
			.join(", ");
		if (breakdown) bits.push(breakdown);
	} else {
		const ext = fileExtension(file.source_file);
		if (ext) bits.push(`.${ext}`);
	}

	if (bits.length === 0) {
		const path = file.source_file ?? "";
		const folder = path.replace(/\\/g, "/").split("/").slice(0, -1).join("/");
		return folder || "No description";
	}

	return bits.join(" · ");
}

export function describeLayer(layer: Layer, files: BulkNode[]): string {
	const extCounts = new Map<string, number>();
	for (const f of files) {
		if (!layer.fileQnames.has(f.qualified_name)) continue;
		const ext = fileExtension(f.source_file);
		if (!ext) continue;
		extCounts.set(ext, (extCounts.get(ext) ?? 0) + 1);
	}
	const breakdown = Array.from(extCounts.entries())
		.sort((a, b) => b[1] - a[1])
		.slice(0, 3)
		.map(([ext, n]) => `${ext} ${n}`)
		.join(", ");

	const base = `${layer.fileCount} ${layer.fileCount === 1 ? "file" : "files"}`;
	return breakdown ? `${base} · ${breakdown}` : base;
}
