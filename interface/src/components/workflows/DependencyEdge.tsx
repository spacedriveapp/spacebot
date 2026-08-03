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
	/**
	 * The template edge this line came from.
	 *
	 * Not the same as `source`/`target` once a run has expanded a fan-out: three
	 * branches mean three lines whose node ids are `audit#39` and friends, while
	 * the edge that produced them is still `scan → audit`. Removal is an editor
	 * action, where the two coincide, but naming the step keys explicitly is what
	 * keeps that a fact rather than a coincidence.
	 */
	parentStepKey?: string;
	childStepKey?: string;
	busy?: boolean;
	/** A run draws finished dependencies solid and pending ones faint. */
	satisfied?: boolean;
	/**
	 * `on_exhausted` — the path taken only when the loop above runs out.
	 *
	 * Drawn differently in three ways at once, and that redundancy is the point:
	 * colour alone is unreadable for a large minority of people, so the give-up
	 * path is also dashed and also captioned. Two edges leaving one step and
	 * meaning opposite things is the single most expensive thing on this canvas
	 * to get wrong.
	 */
	exhausted?: boolean;
	/**
	 * The ordinary arm out of a loop's exit, which is conditional too.
	 *
	 * Easy to miss and worth captioning: the body finishes whether the loop
	 * converged or gave up, so a plain-looking line out of an exit step is not
	 * the plain "runs next" it looks like.
	 */
	convergedArm?: boolean;
	/** Run only: this arm was not the one taken. */
	notTaken?: boolean;
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
	const parentKey = data?.parentStepKey ?? source;
	const childKey = data?.childStepKey ?? target;
	const exhausted = data?.exhausted ?? false;
	const arm = exhausted || (data?.convergedArm ?? false);

	const stroke = selected
		? "var(--color-accent)"
		: exhausted
			? "var(--color-status-warning)"
			: data?.satisfied
				? "var(--color-status-success)"
				: "var(--color-ink-faint)";

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
					stroke,
					strokeWidth: selected ? 2.5 : exhausted ? 2 : 1.5,
					strokeDasharray: exhausted ? "7 4" : undefined,
					opacity: data?.notTaken
						? 0.35
						: selected
							? 1
							: data?.satisfied
								? 0.85
								: 0.65,
				}}
			/>
			{/* Both arms out of a loop are captioned, always — not on hover and not
			    only when selected. An arm you have to interrogate to identify is one
			    that gets read as the other. */}
			{arm && (
				<EdgeLabelRenderer>
					<span
						className={`pointer-events-none absolute whitespace-nowrap rounded-full border bg-app-dark-box px-1.5 py-px text-[9px] leading-4 ${
							exhausted
								? "border-status-warning/60 text-status-warning"
								: "border-status-success/50 text-status-success"
						} ${data?.notTaken ? "opacity-50" : ""}`}
						style={{
							transform: `translate(-50%, -50%) translate(${labelX}px, ${labelY - 11}px)`,
						}}
					>
						{exhausted ? "gave up" : "converged"}
						{data?.notTaken ? " · not taken" : ""}
					</span>
				</EdgeLabelRenderer>
			)}
			{selected && onRemove && (
				<EdgeLabelRenderer>
					<button
						type="button"
						disabled={data?.busy}
						onClick={(event) => {
							event.stopPropagation();
							onRemove(parentKey, childKey);
						}}
						title={
							exhausted
								? `Remove the give-up edge from \`${parentKey}\` to \`${childKey}\``
								: `Stop \`${childKey}\` waiting for \`${parentKey}\``
						}
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
