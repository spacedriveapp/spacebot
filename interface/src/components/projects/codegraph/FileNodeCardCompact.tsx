// Minimal single-line variant of FileNodeCard. Swapped in above
// COMPACT_THRESHOLD nodes so layers with hundreds of files don't drag
// pan/zoom under the weight of per-card descriptions, symbol lists, and
// Radix Popover listeners.

import { memo } from "react";
import { Handle, Position, type NodeProps } from "@xyflow/react";
import { NODE_COLORS, type NodeLabel } from "./constants";
import type { FileNodeData } from "./mermaidGraphBuilder";
import { describeFile } from "./nodeDescription";

const DEFAULT_STRIPE = "#3b82f6";

interface CompactData extends FileNodeData {
	colorOverride?: string;
}

export const FileNodeCardCompact = memo(({ data }: NodeProps) => {
	const { file, symbols, colorOverride } = data as CompactData;
	const labelColor = NODE_COLORS[file.label as NodeLabel] ?? DEFAULT_STRIPE;
	const stripeColor = colorOverride ?? labelColor;
	const displayName = file.name.length > 30 ? `${file.name.slice(0, 29)}…` : file.name;
	const description = describeFile(file, symbols);

	return (
		<div className="relative min-w-[220px] max-w-[260px] cursor-pointer overflow-hidden rounded-md border border-app-line bg-app-darkBox text-ink shadow-[0_1px_4px_rgba(0,0,0,0.3)] hover:border-app-line/80">
			<Handle type="target" position={Position.Top} isConnectable={false} className="!h-1.5 !w-1.5 !border-0 !bg-ink-faint opacity-40" />
			<span
				aria-hidden
				className="absolute inset-y-0 left-0 w-1"
				style={{ background: stripeColor }}
			/>
			<div className="flex flex-col gap-0.5 pl-3 pr-2.5 py-1.5">
				<div className="flex items-center gap-2">
					<span
						className="shrink-0 text-[9px] font-semibold uppercase tracking-wider"
						style={{ color: labelColor }}
					>
						{file.label}
					</span>
					<span
						className="min-w-0 flex-1 truncate text-[12px] font-medium text-ink"
						title={file.source_file ?? file.name}
					>
						{displayName}
					</span>
				</div>
				<p className="truncate text-[10px] text-ink-dull leading-tight">{description}</p>
			</div>
			<Handle type="source" position={Position.Bottom} isConnectable={false} className="!h-1.5 !w-1.5 !border-0 !bg-ink-faint opacity-40" />
		</div>
	);
});

FileNodeCardCompact.displayName = "FileNodeCardCompact";
