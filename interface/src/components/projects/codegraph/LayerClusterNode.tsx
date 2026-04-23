// Overview-level cluster card. One per folder-based layer. Big solid
// card with a colored stripe, uppercase LAYER tag, folder name, a brief
// description line, file count, and a subtle "Click to explore" hint.

import { memo } from "react";
import { Handle, Position, type NodeProps } from "@xyflow/react";
import { COMMUNITY_COLORS, getCommunityColor } from "./constants";

export interface LayerClusterData extends Record<string, unknown> {
	layerId: string;
	layerName: string;
	fileCount: number;
	description: string;
	colorIndex: number;
	onDrillIn: (layerId: string) => void;
}

export const LayerClusterNode = memo(({ data }: NodeProps) => {
	const { layerId, layerName, fileCount, description, colorIndex, onDrillIn } =
		data as LayerClusterData;
	const color = getCommunityColor(colorIndex % COMMUNITY_COLORS.length);

	return (
		<div
			className="group relative min-w-[260px] max-w-[320px] overflow-hidden rounded-xl border border-app-line bg-app-darkBox text-ink shadow-[0_4px_16px_rgba(0,0,0,0.4)] cursor-pointer transition-colors hover:border-accent/60"
			onClick={() => onDrillIn(layerId)}
		>
			<Handle type="target" position={Position.Top} isConnectable={false} className="!h-1.5 !w-1.5 !border-0 !bg-ink-faint opacity-40" />
			<span
				aria-hidden
				className="absolute inset-y-0 left-0 w-1.5"
				style={{ background: color }}
			/>
			<div className="pl-5 pr-4 py-4">
				<div className="flex items-center justify-between mb-1.5">
					<span
						className="text-[10px] font-semibold uppercase tracking-wider"
						style={{ color }}
					>
						Layer
					</span>
					<span className="font-mono text-[10px] text-ink-faint">
						{fileCount} {fileCount === 1 ? "file" : "files"}
					</span>
				</div>
				<div className="text-base font-medium text-ink truncate" title={layerName}>
					{layerName}
				</div>
				<p className="mt-1 text-[11px] text-ink-dull line-clamp-2 leading-tight">
					{description}
				</p>
				<div className="mt-3 flex justify-end text-[10px] text-ink-faint opacity-0 transition-opacity group-hover:opacity-100">
					Click to explore →
				</div>
			</div>
			<Handle type="source" position={Position.Bottom} isConnectable={false} className="!h-1.5 !w-1.5 !border-0 !bg-ink-faint opacity-40" />
		</div>
	);
});

LayerClusterNode.displayName = "LayerClusterNode";
