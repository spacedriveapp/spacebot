// Right-side source inspector. Fetches the file behind the selected graph
// node via api.fsReadFile and renders it in the CodeViewer with the
// symbol's line range highlighted. Drag-resizable width persisted to
// localStorage.
//
// Pared-down port of
// reference/GitNexus/gitnexus-web/src/components/CodeReferencesPanel.tsx —
// only the "Selected file viewer" half; no AI citation list.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { HugeiconsIcon } from "@hugeicons/react";
import { CodeIcon, Cancel01Icon } from "@hugeicons/core-free-icons";
import { api } from "@/api/client";
import { CodeViewer } from "./CodeViewer";
import { NODE_COLORS, type NodeLabel } from "./constants";
import type { BulkNode } from "./types";

// Labels that should NOT trigger the code viewer — they have no source
// file or line range.
const NON_INSPECTABLE_LABELS = new Set<NodeLabel>([
	"Folder",
	"Community",
	"Process",
	"Project",
	"Package",
]);

interface Props {
	projectId: string;
	selectedNode: BulkNode;
	onClose: () => void;
}

const MIN_WIDTH = 420;
const MAX_WIDTH = 900;
const DEFAULT_WIDTH = 560;
const STORAGE_KEY = "spacebot.codegraph.inspectorWidth";

export function CodeInspectorPanel({ projectId, selectedNode, onClose }: Props) {
	const label = selectedNode.label as NodeLabel;
	const filePath = selectedNode.source_file ?? null;

	const [width, setWidth] = useState<number>(() => {
		try {
			const saved = window.localStorage.getItem(STORAGE_KEY);
			const parsed = saved ? parseInt(saved, 10) : NaN;
			if (!Number.isFinite(parsed)) return DEFAULT_WIDTH;
			return Math.max(MIN_WIDTH, Math.min(parsed, MAX_WIDTH));
		} catch {
			return DEFAULT_WIDTH;
		}
	});

	useEffect(() => {
		try {
			window.localStorage.setItem(STORAGE_KEY, String(width));
		} catch {
			// localStorage can throw in privacy modes — ignore.
		}
	}, [width]);

	// Drag-resize handle on the right edge of the panel.
	const resizeRef = useRef<{ startX: number; startWidth: number } | null>(null);
	const startResize = useCallback(
		(e: React.MouseEvent) => {
			e.preventDefault();
			e.stopPropagation();
			resizeRef.current = { startX: e.clientX, startWidth: width };
			document.body.style.cursor = "col-resize";
			document.body.style.userSelect = "none";
			const onMove = (ev: MouseEvent) => {
				const state = resizeRef.current;
				if (!state) return;
				// The panel sits on the RIGHT of the canvas, so dragging
				// the LEFT edge left should widen it.
				const delta = state.startX - ev.clientX;
				const next = Math.max(MIN_WIDTH, Math.min(state.startWidth + delta, MAX_WIDTH));
				setWidth(next);
			};
			const onUp = () => {
				resizeRef.current = null;
				document.body.style.cursor = "";
				document.body.style.userSelect = "";
				window.removeEventListener("mousemove", onMove);
				window.removeEventListener("mouseup", onUp);
			};
			window.addEventListener("mousemove", onMove);
			window.addEventListener("mouseup", onUp);
		},
		[width],
	);

	// Figure out the read range. For File nodes we fetch the whole file;
	// for symbols we fetch a buffer of ±50 lines around the symbol so the
	// user has context.
	const CONTEXT_LINES = 50;
	const readRange = useMemo(() => {
		if (!filePath) return null;
		if (label === "File") return { startLine: undefined, endLine: undefined };
		const ls = selectedNode.line_start ?? undefined;
		const le = selectedNode.line_end ?? ls;
		if (ls === undefined) return { startLine: undefined, endLine: undefined };
		return {
			startLine: Math.max(1, ls - CONTEXT_LINES),
			endLine: (le ?? ls) + CONTEXT_LINES,
		};
	}, [filePath, label, selectedNode.line_start, selectedNode.line_end]);

	const inspectable = !!filePath && !NON_INSPECTABLE_LABELS.has(label);

	const fileQuery = useQuery({
		queryKey: ["fs-read-file", projectId, filePath, readRange?.startLine, readRange?.endLine],
		queryFn: () =>
			api.fsReadFile({
				projectId,
				path: filePath!,
				startLine: readRange?.startLine,
				endLine: readRange?.endLine,
			}),
		enabled: inspectable,
		staleTime: 60_000,
	});

	if (!inspectable) {
		return (
			<aside
				className="relative flex h-full flex-shrink-0 flex-col border-l border-app-line bg-app-darkBox/95 shadow-2xl backdrop-blur-md"
				style={{ width }}
			>
				<ResizeHandle onMouseDown={startResize} />
				<Header node={selectedNode} onClose={onClose} />
				<div className="flex flex-1 items-center justify-center px-4 text-center text-sm text-ink-faint">
					{label === "Folder"
						? "Folder nodes don't have source code. Expand it in the Explorer tab to see its files."
						: label === "Community" || label === "Process"
						? `${label} is a metadata node — it has no file.`
						: "This node doesn't have a source file to display."}
				</div>
			</aside>
		);
	}

	return (
		<aside
			className="relative flex h-full flex-shrink-0 flex-col border-l border-app-line bg-app-darkBox/95 shadow-2xl backdrop-blur-md"
			style={{ width }}
		>
			<ResizeHandle onMouseDown={startResize} />

			<Header node={selectedNode} onClose={onClose} />

			<div className="flex min-h-0 flex-1 flex-col">
				{fileQuery.isLoading && (
					<div className="flex flex-1 items-center justify-center gap-2 text-sm text-ink-faint">
						<span>Loading source...</span>
					</div>
				)}
				{fileQuery.isError && (
					<div className="flex flex-1 items-center justify-center px-4 text-center text-sm text-red-400">
						Failed to read file.
					</div>
				)}
				{fileQuery.data && (
					<CodeViewer
						content={fileQuery.data.content}
						language={fileQuery.data.language}
						startLine={fileQuery.data.start_line}
						highlightStart={selectedNode.line_start ?? undefined}
						highlightEnd={selectedNode.line_end ?? selectedNode.line_start ?? undefined}
					/>
				)}
			</div>
		</aside>
	);
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

function Header({ node, onClose }: { node: BulkNode; onClose: () => void }) {
	const label = node.label as NodeLabel;
	const color = NODE_COLORS[label] ?? "#6b7280";
	const fileName = node.source_file?.split("/").pop() ?? node.name;
	return (
		<div className="flex items-center gap-2 border-b border-app-line bg-app/50 px-3 py-2.5">
			<HugeiconsIcon icon={CodeIcon} className="h-4 w-4 text-cyan-400" />
			<span
				className="shrink-0 rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide"
				style={{ backgroundColor: color, color: "#06060a" }}
			>
				{label}
			</span>
			<span className="min-w-0 flex-1 truncate font-mono text-xs text-ink">{fileName}</span>
			<button
				onClick={onClose}
				className="rounded p-1 text-ink-faint transition-colors hover:bg-app-hover hover:text-ink"
				title="Clear selection"
			>
				<HugeiconsIcon icon={Cancel01Icon} className="h-4 w-4" />
			</button>
		</div>
	);
}

function ResizeHandle({ onMouseDown }: { onMouseDown: (e: React.MouseEvent) => void }) {
	return (
		<div
			onMouseDown={onMouseDown}
			className="absolute left-0 top-0 z-20 h-full w-2 cursor-col-resize bg-transparent transition-colors hover:bg-accent/25"
			title="Drag to resize"
		/>
	);
}
