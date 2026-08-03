import {memo} from "react";
import {Handle, Position, type Node, type NodeProps} from "@xyflow/react";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {faCodeBranch, faLocationCrosshairs} from "@fortawesome/free-solid-svg-icons";
import type {TaskItem} from "@/api/client";
import {styleFor} from "@/components/tasks/boardColumns";
import {STATUS_LABEL} from "@/components/tasks/taskTransitions";
import {NODE_HEIGHT, NODE_WIDTH} from "@/components/workflows/layout";

/**
 * One task, as a box on the graph canvas.
 *
 * Sibling of `StepNode` rather than a reuse of it: that node is built around a
 * `WorkflowStep` — its key, its binding count, its output schema, whether it is
 * in a template cycle — and every one of those is absent here. What a task node
 * has to answer instead is "which one is this, how is it doing, and is it the
 * one I came from", so the number is prominent, the status pill is always drawn
 * (a task always has a status; a step does not), and the seed wears a ring
 * nothing else on the canvas wears.
 *
 * Status colour comes from `styleFor` and never from `@spacedrive/ai`'s
 * `TaskStatusIcon`, which throws on any status outside the five it knows —
 * `blocked` among them, which is exactly the status a stalled graph sits in and
 * the single most likely thing to be looking at here.
 */
export type TaskGraphNodeData = {
	task: TaskItem;
	/** The task the graph was asked about. Exactly one node carries this. */
	seed: boolean;
};

export type TaskGraphFlowNode = Node<TaskGraphNodeData, "task">;

function TaskGraphNodeImpl({data, selected}: NodeProps<TaskGraphFlowNode>) {
	const {task, seed} = data;
	const style = styleFor(task.status);
	const branch = task.fan_out_branch_key;

	// A blocked task's reason is the whole reason to be on this screen, so it
	// takes the node's last line ahead of anything else. `last_error` is the
	// fallback for a task that failed without being parked.
	const trouble = task.block_reason
		? `${task.block_kind ? `${task.block_kind}: ` : ""}${task.block_reason}`
		: (task.last_error ?? null);

	// Selection wins over seed: the panel on the right has to be attributable to
	// a box on the left, and the seed is still marked by its badge.
	const border = selected
		? "border-accent"
		: seed
			? "border-accent/70"
			: task.fan_out_placeholder
				? "border-dashed border-ink-faint/50"
				: task.status === "blocked"
					? "border-status-error/60"
					: task.status === "done"
						? "border-status-success/50"
						: task.status === "in_progress"
							? "border-accent/60"
							: "border-app-line";

	return (
		<div
			className={`flex flex-col justify-between rounded-lg border bg-app-dark-box px-2.5 py-2 text-left transition-colors ${border} ${
				task.fan_out_placeholder ? "opacity-80" : ""
			} ${
				seed
					? "ring-2 ring-accent/40"
					: selected
						? "shadow-lg shadow-accent/20"
						: "hover:border-ink-faint/60"
			}`}
			style={{width: NODE_WIDTH, height: NODE_HEIGHT}}
			title={task.title}
		>
			{/* Read-only on both sides: edges are authored in the drawer's
			    Dependencies panel, where the refusals are already explained. */}
			<Handle
				type="target"
				position={Position.Left}
				isConnectable={false}
				className="!size-2 !border !border-ink-faint !bg-app-box"
			/>

			<div className="min-w-0">
				<div className="flex items-baseline gap-1.5">
					<span className="shrink-0 font-mono text-[10px] text-ink-faint">
						#{task.task_number}
					</span>
					<span className="min-w-0 flex-1 truncate text-[13px] leading-tight text-ink">
						{withoutBranchSuffix(task)}
					</span>
				</div>
				<div className="mt-0.5 flex items-center gap-1.5">
					{seed && (
						<span
							className="inline-flex shrink-0 items-center gap-1 rounded-full border border-accent/50 bg-accent/15 px-1.5 text-[9px] uppercase tracking-wide text-accent"
							title="The task you came from. The rest of this graph is what it is connected to."
						>
							<FontAwesomeIcon
								icon={faLocationCrosshairs}
								className="shrink-0 text-[7px]"
							/>
							this task
						</span>
					)}
					{branch && (
						<span
							className="inline-flex min-w-0 shrink items-center gap-1 rounded-full border border-accent/40 bg-accent/10 px-1.5 text-[9px] text-accent"
							title={`Branch \`${branch}\` of a fan-out. A fan-in downstream collects the branches by this key.`}
						>
							<FontAwesomeIcon icon={faCodeBranch} className="shrink-0 text-[7px]" />
							<span className="truncate font-mono">{branch}</span>
						</span>
					)}
					{task.workflow_step_key && !branch && !seed && (
						<span
							className="min-w-0 shrink truncate font-mono text-[10px] text-ink-faint"
							title="The workflow step this task was compiled from"
						>
							{task.workflow_step_key}
						</span>
					)}
				</div>
			</div>

			<div className="flex min-w-0 items-center gap-1">
				<span
					className="inline-flex shrink-0 items-center gap-1.5 rounded-full border border-app-line bg-app-box/60 px-1.5 py-0.5 text-[10px] text-ink-dull"
					title={style.hint}
				>
					<span className={`size-1.5 shrink-0 rounded-full ${style.dot}`} />
					<span className="truncate">
						{STATUS_LABEL[task.status] ?? task.status}
					</span>
				</span>

				{trouble ? (
					<span
						className={`min-w-0 flex-1 truncate text-[10px] ${
							task.block_reason ? "text-status-error" : "text-status-warning"
						}`}
						title={trouble}
					>
						{trouble}
					</span>
				) : task.fan_out_placeholder ? (
					<span
						className="min-w-0 flex-1 truncate text-[10px] text-ink-faint"
						title="Not work. It holds the edges its branches will inherit until the fan-out expands."
					>
						waiting to fan out
					</span>
				) : null}
			</div>

			<Handle
				type="source"
				position={Position.Right}
				isConnectable={false}
				className="!size-2 !border !border-ink-faint !bg-app-box"
			/>
		</div>
	);
}

/**
 * The title without the branch suffix the compiler appended.
 *
 * Expansion names a branch's task `Audit the repository [grimoire]` so it can be
 * told apart on the board, where there is no column of siblings to compare it
 * against. Here there is, and the branch is already a badge, so repeating it
 * spends the node's one line of title on a word just read.
 */
function withoutBranchSuffix(task: TaskItem): string {
	const branch = task.fan_out_branch_key;
	if (!branch) return task.title;
	const suffix = ` [${branch}]`;
	return task.title.endsWith(suffix)
		? task.title.slice(0, -suffix.length)
		: task.title;
}

export const TaskGraphNode = memo(TaskGraphNodeImpl);
