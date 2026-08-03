import {useEffect, useMemo, useRef, useState} from "react";
import {Link} from "@tanstack/react-router";
import {useQuery, useQueryClient} from "@tanstack/react-query";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {
	faArrowLeft,
	faArrowUpRightFromSquare,
	faCodeBranch,
	faLocationCrosshairs,
	faTriangleExclamation,
} from "@fortawesome/free-solid-svg-icons";
import {api, type TaskItem} from "@/api/client";
import {useLiveContext} from "@/hooks/useLiveContext";
import {TaskStatusPill} from "@/components/workflows/TaskStatusPill";
import {TaskGraphCanvas} from "@/components/tasks/TaskGraphCanvas";

/**
 * One task, and everything it is connected to.
 *
 * The run view answers "how did this launch go" and draws its picture from the
 * workflow template — which means it draws nothing once the template is deleted,
 * and nothing at all for a graph that never came from a workflow. This screen
 * answers the question a person actually arrives with, "what is this task part
 * of", and it answers it from the task edges themselves. Deleting the template,
 * or never having had one, changes nothing here.
 *
 * The server returns the *undirected* connected component, which is what makes
 * it useful from a leaf: opening the graph from one branch of a three-way
 * fan-out shows the other two, because they are reachable only back through the
 * parent they share.
 */
export function TaskGraphView({taskNumber}: {taskNumber: number}) {
	const queryClient = useQueryClient();
	const {taskEventVersion} = useLiveContext();

	const queryKey = useMemo(() => ["task-graph", taskNumber], [taskNumber]);

	const {data, isLoading, error} = useQuery({
		queryKey,
		queryFn: () => api.getTaskGraph(taskNumber),
		retry: false,
	});

	// The same SSE-driven invalidation the board uses, so a task finishing
	// recolours its node in the same moment rather than on the next reload.
	const previousVersion = useRef(taskEventVersion);
	useEffect(() => {
		if (taskEventVersion !== previousVersion.current) {
			previousVersion.current = taskEventVersion;
			void queryClient.invalidateQueries({queryKey});
		}
	}, [taskEventVersion, queryClient, queryKey]);

	// Selection follows the seed, so arriving from a task's drawer opens with
	// that task already inspected rather than an empty panel.
	const [selected, setSelected] = useState<number | null>(taskNumber);
	useEffect(() => setSelected(taskNumber), [taskNumber]);

	const tasks = useMemo(() => data?.tasks ?? [], [data]);
	const selectedTask = useMemo(
		() => tasks.find((task) => task.task_number === selected) ?? null,
		[tasks, selected],
	);
	const seedTask = useMemo(
		() => tasks.find((task) => task.task_number === (data?.seed ?? taskNumber)),
		[tasks, data, taskNumber],
	);

	/**
	 * Edges with both ends on screen — the ones actually drawn.
	 *
	 * A truncated walk can return an edge whose other end it never got to, and
	 * counting those would put a number in the header that nobody can find on the
	 * canvas. The banner is where the missing ones are accounted for.
	 */
	const drawnEdges = useMemo(() => {
		const present = new Set(tasks.map((task) => task.task_number));
		return (data?.edges ?? []).filter(
			(edge) =>
				present.has(edge.parent_task_number) &&
				present.has(edge.child_task_number),
		).length;
	}, [tasks, data]);

	if (isLoading) {
		return (
			<p className="py-8 text-center text-sm text-ink-faint">Loading graph…</p>
		);
	}
	if (error || !data) {
		return (
			<div className="py-8 text-center text-sm text-status-error">
				Failed to load the graph for #{taskNumber}.
				<div className="mt-1 font-mono text-[10px] text-ink-faint">
					{error instanceof Error ? error.message : "unknown error"}
				</div>
				<div className="mt-3">
					<Link
						to="/tasks"
						search={{task: taskNumber}}
						className="text-xs text-accent hover:underline"
					>
						Back to the task
					</Link>
				</div>
			</div>
		);
	}

	const done = tasks.filter((task) => task.status === "done").length;

	return (
		<div className="flex h-full min-h-0 w-full flex-col">
			<div className="flex items-center gap-3 border-b border-app-line px-4 py-2">
				<Link
					to="/tasks"
					search={{task: data.seed}}
					className="shrink-0 text-ink-faint hover:text-ink-dull"
					title="Back to this task on the board"
				>
					<FontAwesomeIcon icon={faArrowLeft} className="text-xs" />
				</Link>
				<div className="min-w-0 flex-1">
					<div className="flex items-baseline gap-2">
						<span className="shrink-0 font-mono text-sm text-ink-faint">
							#{data.seed}
						</span>
						<h1 className="truncate text-sm text-ink">
							{seedTask?.title ?? "Task graph"}
						</h1>
					</div>
					<p className="truncate text-[11px] text-ink-faint">
						{tasks.length === 1
							? "Not connected to anything — this task stands alone."
							: `${tasks.length} connected tasks · ${done} done · ${drawnEdges} dependencies`}
					</p>
				</div>
			</div>

			{/* Never quietly. A truncated walk is a partial graph, and a partial
			    graph presented as a whole one is the failure this banner exists to
			    prevent — the tasks that are missing are exactly the ones nobody
			    would think to look for. */}
			{data.truncated && (
				<div className="flex items-start gap-2 border-b border-status-warning/30 bg-status-warning/5 px-4 py-2 text-[11px] text-status-warning">
					<FontAwesomeIcon
						icon={faTriangleExclamation}
						className="mt-0.5 shrink-0 text-[10px]"
					/>
					<span>
						Showing {tasks.length} tasks — the walk hit its cap and stopped.
						There are more connected to this one than are drawn here, and some
						of the boxes below may look disconnected because the tasks that
						joined them were not returned.
					</span>
				</div>
			)}

			<div className="flex min-h-0 flex-1">
				<div className="min-w-0 flex-1">
					<TaskGraphCanvas
						tasks={tasks}
						edges={data.edges}
						seed={data.seed}
						selected={selected}
						onSelect={setSelected}
					/>
				</div>
				<div className="flex w-[400px] shrink-0 flex-col overflow-y-auto border-l border-app-line">
					{selectedTask ? (
						<GraphTaskPanel task={selectedTask} seed={data.seed} />
					) : (
						<p className="px-4 py-6 text-center text-xs text-ink-faint">
							Select a task to see what it is and what it produced.
						</p>
					)}
				</div>
			</div>
		</div>
	);
}

/**
 * The selected node, in enough detail to decide whether to go there.
 *
 * Deliberately not the task drawer. The drawer is the task UI — it edits
 * status, dependencies, contracts and the failure budget — and reproducing it
 * here would be a second place for all of that to drift. This shows what a node
 * cannot fit and then hands off, exactly the way the run view does.
 */
function GraphTaskPanel({task, seed}: {task: TaskItem; seed: number}) {
	const branch = task.fan_out_branch_key;
	const suffix = branch ? ` [${branch}]` : "";
	const title =
		suffix && task.title.endsWith(suffix)
			? task.title.slice(0, -suffix.length)
			: task.title;

	return (
		<div className="px-4 py-3">
			<div className="flex flex-wrap items-baseline gap-2">
				<span className="shrink-0 font-mono text-[11px] text-ink-faint">
					#{task.task_number}
				</span>
				<span className="min-w-0 flex-1 text-sm text-ink">{title}</span>
				<TaskStatusPill status={task.status} />
			</div>

			<div className="mt-1.5 flex flex-wrap items-center gap-1.5">
				{task.task_number === seed && (
					<span className="inline-flex items-center gap-1 rounded-full border border-accent/50 bg-accent/15 px-1.5 py-px text-[9px] uppercase tracking-wide text-accent">
						<FontAwesomeIcon icon={faLocationCrosshairs} className="text-[7px]" />
						this task
					</span>
				)}
				{branch && (
					<span
						className="inline-flex items-center gap-1 rounded-full border border-accent/40 bg-accent/10 px-1.5 py-px font-mono text-[9px] text-accent"
						title="Which branch of a fan-out this task is. A fan-in downstream collects the branches by this key."
					>
						<FontAwesomeIcon icon={faCodeBranch} className="text-[7px]" />
						{branch}
					</span>
				)}
				{task.workflow_step_key && (
					<span
						className="rounded border border-app-line px-1 font-mono text-[10px] text-ink-faint"
						title="The workflow step this task was compiled from"
					>
						{task.workflow_step_key}
					</span>
				)}
			</div>

			<div className="mt-2 flex flex-wrap items-center gap-3">
				{/* The drawer on /tasks is the task UI; this only opens it. */}
				<Link
					to="/tasks"
					search={{task: task.task_number}}
					className="inline-flex items-center gap-1.5 text-xs text-accent hover:underline"
				>
					<FontAwesomeIcon
						icon={faArrowUpRightFromSquare}
						className="text-[9px]"
					/>
					Open this task
				</Link>
				{task.task_number !== seed && (
					<Link
						to="/tasks/$taskNumber/graph"
						params={{taskNumber: String(task.task_number)}}
						className="inline-flex items-center gap-1.5 text-xs text-ink-dull hover:text-ink hover:underline"
						title="Redraw the graph asking from this task instead. Useful after a truncated walk, which is cut relative to wherever it started."
					>
						<FontAwesomeIcon
							icon={faLocationCrosshairs}
							className="text-[9px]"
						/>
						Centre here
					</Link>
				)}
			</div>

			{task.fan_out_placeholder && (
				<p className="mt-2 rounded border border-dashed border-app-line bg-app-box/30 px-2 py-1 text-[11px] text-ink-faint">
					Waiting to fan out. This is not work — it holds the edges its branches
					will inherit, so the tasks after it have something to wait on. It is
					replaced by one task per item once the step it iterates finishes.
				</p>
			)}

			{task.description && (
				<p className="mt-2 whitespace-pre-wrap break-words text-[11px] leading-relaxed text-ink-dull">
					{task.description}
				</p>
			)}

			{task.block_reason && (
				<p className="mt-2 rounded border border-status-error/30 bg-status-error/5 px-2 py-1 text-[11px] text-status-error">
					{task.block_kind ? `${task.block_kind}: ` : ""}
					{task.block_reason}
				</p>
			)}
			{!task.block_reason && task.last_error && (
				<p className="mt-2 break-words font-mono text-[10px] text-status-warning">
					{task.last_error}
				</p>
			)}

			{task.inputs != null && <JsonBlock label="Inputs" value={task.inputs} muted />}
			{task.outputs != null && <JsonBlock label="Outputs" value={task.outputs} />}
		</div>
	);
}

function JsonBlock({
	label,
	value,
	muted,
}: {
	label: string;
	value: unknown;
	muted?: boolean;
}) {
	return (
		<div className="mt-2">
			<h4 className="mb-0.5 text-[10px] font-medium uppercase tracking-wide text-ink-faint">
				{label}
			</h4>
			<pre
				className={`max-h-56 overflow-auto whitespace-pre-wrap break-words rounded border border-app-line bg-app-box/40 px-2 py-1.5 font-mono text-[11px] leading-relaxed ${
					muted ? "text-ink-faint" : "text-ink-dull"
				}`}
			>
				{JSON.stringify(value, null, 2)}
			</pre>
		</div>
	);
}
