import {
	BaseEdge,
	EdgeLabelRenderer,
	getSmoothStepPath,
	type Edge,
	type EdgeProps,
} from "@xyflow/react";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {faXmark} from "@fortawesome/free-solid-svg-icons";

/**
 * "This step waits for that one", drawn.
 *
 * Orthogonal rather than curved: a template is read as columns, and a smooth
 * step path leaves the source and enters the target horizontally, so a fan-in
 * arrives as two lines converging on one handle instead of two arcs crossing
 * near it. Removal lives on the edge itself and appears once the edge is
 * clicked — an always-visible button per edge turns a twelve-step template into
 * a field of crosses, and hover alone is not reachable from a keyboard.
 */
/** Half the handle's width plus the arrowhead's, in flow units. */
const HANDLE_CLEARANCE = 11;

export type DependencyEdgeData = {
	onRemove?: (parentStepKey: string, childStepKey: string) => void;
	busy?: boolean;
	/** A run draws finished dependencies solid and pending ones faint. */
	satisfied?: boolean;
};

export type DependencyFlowEdge = Edge<DependencyEdgeData, "dependency">;

export function DependencyEdge({
	id,
	source,
	target,
	sourceX,
	sourceY,
	targetX,
	targetY,
	sourcePosition,
	targetPosition,
	markerEnd,
	data,
	selected,
}: EdgeProps<DependencyFlowEdge>) {
	// React Flow draws edges *under* nodes, so an arrowhead landing on the
	// target's own handle is drawn and then covered by it — the direction of
	// every connection disappears. Stopping the path short of the handle puts
	// the arrow in clear space just outside it. Target handles on this canvas
	// are always on the left, so a fixed horizontal inset is enough.
	const [path, labelX, labelY] = getSmoothStepPath({
		sourceX,
		sourceY,
		sourcePosition,
		targetX: targetX - HANDLE_CLEARANCE,
		targetY,
		targetPosition,
		borderRadius: 10,
	});

	const onRemove = data?.onRemove;

	return (
		<>
			<BaseEdge
				id={id}
				path={path}
				// A custom edge has to forward this itself; React Flow resolves the
				// marker definition but does not attach it for you, and an edge with
				// no arrowhead is an edge with no direction.
				markerEnd={markerEnd}
				interactionWidth={18}
				style={{
					// `app-line` is the border colour and is deliberately almost
					// invisible against `app`; a connection has to be followable across
					// the viewport, so it is drawn in the faint ink instead.
					stroke: selected
						? "var(--color-accent)"
						: data?.satisfied
							? "var(--color-status-success)"
							: "var(--color-ink-faint)",
					strokeWidth: selected ? 2 : 1.5,
					opacity: selected ? 1 : data?.satisfied ? 0.8 : 0.65,
				}}
			/>
			{selected && onRemove && (
				<EdgeLabelRenderer>
					<button
						type="button"
						disabled={data?.busy}
						onClick={(event) => {
							event.stopPropagation();
							onRemove(source, target);
						}}
						title={`Stop \`${target}\` waiting for \`${source}\``}
						className="pointer-events-auto absolute flex size-4 items-center justify-center rounded-full border border-app-line bg-app-dark-box text-ink-faint hover:border-status-error hover:text-status-error disabled:opacity-50"
						style={{
							transform: `translate(-50%, -50%) translate(${labelX}px, ${labelY}px)`,
						}}
					>
						<FontAwesomeIcon icon={faXmark} className="text-[8px]" />
					</button>
				</EdgeLabelRenderer>
			)}
		</>
	);
}
