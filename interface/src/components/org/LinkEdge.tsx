import {BaseEdge, getSmoothStepPath, type EdgeProps} from "@xyflow/react";
import {EDGE_COLOR, EDGE_COLOR_ACTIVE} from "./constants";

export function LinkEdge({
	id,
	sourceX,
	sourceY,
	targetX,
	targetY,
	sourcePosition,
	targetPosition,
	data,
	selected,
}: EdgeProps) {
	const [edgePath] = getSmoothStepPath({
		sourceX,
		sourceY,
		sourcePosition,
		targetX,
		targetY,
		targetPosition,
		borderRadius: 16,
	});

	const isActive = data?.active as boolean;
	const color = isActive ? EDGE_COLOR_ACTIVE : EDGE_COLOR;

	return (
		<>
			<BaseEdge
				id={id}
				path={edgePath}
				style={{
					stroke: selected ? "hsl(var(--color-accent))" : color,
					strokeWidth: selected ? 2.5 : isActive ? 2 : 1.5,
					opacity: selected ? 1 : isActive ? 0.9 : 0.4,
				}}
			/>
			{/* Animated dot traveling along the edge during active message traffic */}
			{isActive && (
				<circle r="3" fill={EDGE_COLOR_ACTIVE}>
					<animateMotion dur="2s" repeatCount="indefinite" path={edgePath} />
				</circle>
			)}
		</>
	);
}
