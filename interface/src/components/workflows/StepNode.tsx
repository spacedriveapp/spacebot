import {memo} from "react";
import {Handle, Position, type NodeProps, type Node} from "@xyflow/react";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {
	faCircleNodes,
	faCodeBranch,
	faQuoteLeft,
	faRightToBracket,
} from "@fortawesome/free-solid-svg-icons";
import type {TaskStatus, WorkflowStep} from "@/api/client";
import {styleFor} from "@/components/tasks/boardColumns";
import {STATUS_LABEL} from "@/components/tasks/taskTransitions";
import {NODE_HEIGHT, NODE_WIDTH} from "./layout";

/**
 * One step, as a box on the canvas.
 *
 * A node has to answer "what is this and is it wired up" from across the
 * viewport, because that is the only question a canvas is better at than a
 * list. So: the title big enough to read at a glance, the key underneath
 * because that is what edges and bindings are written in terms of, and badges
 * for the three things that decide whether the step will actually do anything —
 * whether it declares an output for anyone downstream to bind to, whether it
 * carries a system prompt, and how many of its inputs are fed. A step with no
 * output badge and a child bound to it is a run that will fail, and it is
 * visible here before it is launched.
 *
 * Data is plain values rather than callbacks: node types have to be referentially
 * stable across renders or React Flow remounts every node, and selection is
 * handled by the canvas's `onNodeClick` instead.
 */
export type StepNodeData = {
	step: WorkflowStep;
	bindingCount: number;
	inCycle: boolean;
	/** Set on the run canvas only; the editor leaves it undefined. */
	status?: TaskStatus;
	taskNumber?: number;
	/** A blocked or errored task says so on the node — that is the branch to go look at. */
	trouble?: string | null;
	/** The task's own title once there is one, so a renamed task is not hidden. */
	title?: string;
	/**
	 * Which branch of a fan-out this node is.
	 *
	 * Three boxes reading `Audit the repository` are three boxes nobody can tell
	 * apart, and the branch key is the only thing that does — it is what the
	 * fan-in downstream collects by, so it is also the name the reader will meet
	 * again in the report's inputs.
	 */
	branchKey?: string | null;
	/**
	 * A fan-out that has not expanded yet.
	 *
	 * Drawn provisionally on purpose. Its width is genuinely unknown until the
	 * upstream step finishes, so a node that looked like every other one would be
	 * claiming a certainty the run does not have.
	 */
	placeholder?: boolean;
};

export type StepFlowNode = Node<StepNodeData, "step">;

function StepNodeImpl({data, selected}: NodeProps<StepFlowNode>) {
	const {
		step,
		bindingCount,
		inCycle,
		status,
		taskNumber,
		trouble,
		title,
		branchKey,
		placeholder,
	} = data;
	const style = status && !placeholder ? styleFor(status) : null;

	// The border carries the one fact that matters most on that canvas: on the
	// editor, whether this is the step being edited; on a run, how its task is
	// doing. Selection still wins, because the panel on the right has to be
	// attributable to a box on the left.
	const border = selected
		? "border-accent"
		: inCycle
			? "border-status-error"
			: placeholder
				? "border-dashed border-ink-faint/50"
				: status === "blocked"
					? "border-status-error/60"
					: status === "done"
						? "border-status-success/50"
						: status === "in_progress"
							? "border-accent/60"
							: "border-app-line";

	return (
		<div
			className={`flex flex-col justify-between rounded-lg border bg-app-dark-box px-2.5 py-2 text-left transition-colors ${border} ${
				placeholder ? "border-dashed opacity-80" : ""
			} ${selected ? "shadow-lg shadow-accent/20" : "hover:border-ink-faint/60"}`}
			style={{width: NODE_WIDTH, height: NODE_HEIGHT}}
		>
			<Handle
				type="target"
				position={Position.Left}
				className="!size-2.5 !border !border-ink-faint !bg-app-box"
				title="Drop a connection here to make this step wait for another"
			/>

			<div className="min-w-0">
				<div className="flex items-baseline gap-1.5">
					<span className="min-w-0 flex-1 truncate text-[13px] leading-tight text-ink">
						{title ?? step.title}
					</span>
					{taskNumber != null && (
						<span className="shrink-0 font-mono text-[10px] text-ink-faint">
							#{taskNumber}
						</span>
					)}
				</div>
				<div className="mt-0.5 flex items-center gap-1.5">
					<span className="shrink-0 truncate font-mono text-[10px] text-ink-faint">
						{step.step_key}
					</span>
					{branchKey && (
						<span
							className="inline-flex min-w-0 shrink items-center gap-1 rounded-full border border-accent/40 bg-accent/10 px-1.5 text-[9px] text-accent"
							title={`Branch \`${branchKey}\` of this step's fan-out`}
						>
							<FontAwesomeIcon icon={faCodeBranch} className="shrink-0 text-[7px]" />
							<span className="truncate font-mono">{branchKey}</span>
						</span>
					)}
					{inCycle && (
						<span className="shrink-0 rounded border border-status-error/50 px-1 text-[9px] uppercase text-status-error">
							cycle
						</span>
					)}
				</div>
			</div>

			<div className="flex min-w-0 items-center gap-1">
				{placeholder ? (
					<span
						className="inline-flex min-w-0 items-center gap-1.5 rounded-full border border-dashed border-ink-faint/60 bg-app-box/40 px-1.5 py-0.5 text-[10px] text-ink-faint"
						title={
							step.for_each_step_key
								? `Waiting for \`${step.for_each_step_key}\` to finish. It decides how many of these there are.`
								: "Waiting to fan out. How many branches this becomes is not known yet."
						}
					>
						<FontAwesomeIcon icon={faCodeBranch} className="shrink-0 text-[7px]" />
						<span className="truncate">waiting to fan out</span>
					</span>
				) : style ? (
					<span
						className="inline-flex min-w-0 shrink items-center gap-1.5 rounded-full border border-app-line bg-app-box/60 px-1.5 py-0.5 text-[10px] text-ink-dull"
						title={style.hint}
					>
						<span className={`size-1.5 shrink-0 rounded-full ${style.dot}`} />
						<span className="truncate">
							{status ? (STATUS_LABEL[status] ?? status) : ""}
						</span>
					</span>
				) : (
					<span className="shrink-0 rounded border border-app-line bg-app-box/50 px-1 py-px text-[9px] text-ink-faint">
						{step.priority}
					</span>
				)}

				{trouble ? (
					<span
						className="min-w-0 flex-1 truncate text-[10px] text-status-error"
						title={trouble}
					>
						{trouble}
					</span>
				) : (
					<>
						{step.output_schema != null && (
							<Badge icon={faCircleNodes} label="output" hint="Declares an output schema, so a later step can bind to it." />
						)}
						{step.system_prompt && (
							<Badge icon={faQuoteLeft} label="prompt" hint={step.system_prompt} />
						)}
						{bindingCount > 0 && (
							<Badge
								icon={faRightToBracket}
								label={String(bindingCount)}
								hint={`${bindingCount} input${bindingCount === 1 ? "" : "s"} bound`}
							/>
						)}
					</>
				)}
			</div>

			<Handle
				type="source"
				position={Position.Right}
				className="!size-2.5 !border !border-ink-faint !bg-app-box"
				title="Drag from here onto another step to make it wait for this one"
			/>
		</div>
	);
}

function Badge({
	icon,
	label,
	hint,
}: {
	icon: Parameters<typeof FontAwesomeIcon>[0]["icon"];
	label: string;
	hint: string;
}) {
	return (
		<span
			className="inline-flex shrink-0 items-center gap-1 rounded border border-app-line bg-app-box/50 px-1 py-px text-[9px] text-ink-faint"
			title={hint}
		>
			<FontAwesomeIcon icon={icon} className="text-[7px]" />
			{label}
		</span>
	);
}

export const StepNode = memo(StepNodeImpl);
