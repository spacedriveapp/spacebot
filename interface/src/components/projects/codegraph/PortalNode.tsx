// Detail-view portal pill. One per neighbor layer reachable from the
// active layer. Clicking navigates the canvas to that layer's detail.
// Opaque background so edges don't visually leak through.

import { memo } from "react";
import { Handle, Position, type NodeProps } from "@xyflow/react";
import { COMMUNITY_COLORS, getCommunityColor } from "./constants";

export interface PortalNodeData extends Record<string, unknown> {
	targetLayerId: string;
	targetLayerName: string;
	connectionCount: number;
	colorIndex: number;
	onNavigate: (layerId: string) => void;
}

export const PortalNode = memo(({ data }: NodeProps) => {
	const { targetLayerId, targetLayerName, connectionCount, colorIndex, onNavigate } =
		data as PortalNodeData;
	const color = getCommunityColor(colorIndex % COMMUNITY_COLORS.length);

	return (
		<div
			className="group relative w-[180px] overflow-hidden rounded-lg border border-app-line bg-app-darkBox text-ink shadow-[0_2px_8px_rgba(0,0,0,0.4)] cursor-pointer transition-colors hover:border-accent/60"
			onClick={() => onNavigate(targetLayerId)}
		>
			<Handle type="target" position={Position.Top} className="!h-1.5 !w-1.5 !border-0 !bg-ink-faint opacity-40" />
			<span
				aria-hidden
				className="absolute inset-y-0 left-0 w-1"
				style={{ background: color }}
			/>
			<div className="pl-3 pr-2 py-2">
				<div className="flex items-center justify-between">
					<span
						className="text-[10px] font-semibold uppercase tracking-wider"
						style={{ color }}
					>
						Portal
					</span>
					<span className="font-mono text-[10px] text-ink-faint">
						{connectionCount}
					</span>
				</div>
				<div className="mt-0.5 text-sm font-medium text-ink truncate" title={targetLayerName}>
					→ {targetLayerName}
				</div>
				<p className="mt-0.5 text-[10px] text-ink-dull">
					{connectionCount} {connectionCount === 1 ? "connection" : "connections"}
				</p>
			</div>
			<Handle type="source" position={Position.Bottom} className="!h-1.5 !w-1.5 !border-0 !bg-ink-faint opacity-40" />
		</div>
	);
});

PortalNode.displayName = "PortalNode";
