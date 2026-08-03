import {memo} from "react";
import type {Node, NodeProps} from "@xyflow/react";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {
	faRotate,
	faTriangleExclamation,
} from "@fortawesome/free-solid-svg-icons";
import type {LoopResolution} from "@/api/client";
import {LOOP_HEADER} from "./layout";
import {RESOLUTION_HINT, RESOLUTION_LABEL} from "./loops";

/**
 * The region behind one loop body.
 *
 * Purely a caption and an outline — it takes no clicks, so the nodes inside it
 * behave exactly as they did before it existed. The caption carries the three
 * facts that decide what the body will actually do: which body it is, how many
 * passes it may take, and what has to be true for it to stop. A body drawn
 * without them is a rectangle asserting that something repeats and declining to
 * say for how long.
 */
export type LoopGroupNodeData = {
	group: string;
	maxIterations: number;
	/** Rendered from `describePredicate` by the canvas, so this stays presentational. */
	condition: string;
	/** Named when the body has no single exit step — the server refuses to launch it. */
	problem?: string | null;
	/** Run only: which pass the body is on, and how it came out. */
	pass?: {index: number; total: number} | null;
	resolution?: LoopResolution | null;
	width: number;
	height: number;
};

export type LoopGroupFlowNode = Node<LoopGroupNodeData, "loopGroup">;

function LoopGroupNodeImpl({data}: NodeProps<LoopGroupFlowNode>) {
	const {group, maxIterations, condition, problem, pass, resolution} = data;
	const failed =
		resolution === "exhausted_routed" || resolution === "exhausted_blocked";
	const tone = problem
		? "border-status-error/70"
		: failed
			? "border-status-warning/60"
			: resolution === "converged"
				? "border-status-success/50"
				: "border-accent/45";

	return (
		<div
			className={`pointer-events-none rounded-xl border-2 border-dashed ${tone} bg-accent/[0.04]`}
			style={{width: data.width, height: data.height}}
		>
			<div
				className="flex items-center gap-1.5 overflow-hidden px-3"
				style={{height: LOOP_HEADER}}
			>
				<FontAwesomeIcon
					icon={problem ? faTriangleExclamation : faRotate}
					className={`shrink-0 text-[9px] ${
						problem ? "text-status-error" : "text-accent"
					}`}
				/>
				<span
					className={`shrink-0 font-mono text-[11px] ${
						problem ? "text-status-error" : "text-accent"
					}`}
				>
					loop {group}
				</span>
				<span className="shrink-0 text-[10px] text-ink-faint">
					·{" "}
					{pass
						? `pass ${pass.index} of ${pass.total}`
						: `up to ${maxIterations} pass${maxIterations === 1 ? "" : "es"}`}
				</span>
				<span
					className="pointer-events-auto min-w-0 truncate font-mono text-[10px] text-ink-faint"
					title={problem ?? condition}
				>
					· {problem ?? condition}
				</span>
				{resolution && (
					<span
						className={`pointer-events-auto ml-auto shrink-0 rounded-full border px-1.5 text-[9px] ${
							resolution === "converged"
								? "border-status-success/50 bg-status-success/10 text-status-success"
								: resolution === "iterated"
									? "border-app-line bg-app-box/60 text-ink-dull"
									: "border-status-warning/50 bg-status-warning/10 text-status-warning"
						}`}
						title={RESOLUTION_HINT[resolution]}
					>
						{RESOLUTION_LABEL[resolution]}
					</span>
				)}
			</div>
		</div>
	);
}

export const LoopGroupNode = memo(LoopGroupNodeImpl);
