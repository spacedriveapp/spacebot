import {useEffect, useMemo, useRef} from "react";
import {Link} from "@tanstack/react-router";
import {useQuery, useQueryClient} from "@tanstack/react-query";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {faArrowLeft} from "@fortawesome/free-solid-svg-icons";
import {api, type TaskItem} from "@/api/client";
import {useLiveContext} from "@/hooks/useLiveContext";
import {TaskStatusPill} from "@/components/workflows/TaskStatusPill";
import {orderSteps} from "@/components/workflows/graph";

/**
 * One launch, and the tasks it became.
 *
 * A run is not a separate execution engine — launching compiles the template
 * into ordinary tasks with ordinary dependency edges, and the same scheduler
 * that drives the board drives these. So this screen's job is to show which
 * step became which task, what each one produced, and to hand off to the task
 * drawer for anything deeper. It deliberately does not re-implement the drawer.
 */
export function WorkflowRunView({runId}: {runId: string}) {
	const queryClient = useQueryClient();
	const {taskEventVersion} = useLiveContext();

	const runKey = useMemo(() => ["workflow-run", runId], [runId]);

	const {data, isLoading, error} = useQuery({
		queryKey: runKey,
		queryFn: () => api.getWorkflowRun(runId),
		// A run in flight changes without anyone clicking anything.
		refetchInterval: 10_000,
	});

	// Same SSE-driven invalidation the board uses, so a task finishing shows up
	// here in the same moment rather than up to ten seconds later.
	const previousVersion = useRef(taskEventVersion);
	useEffect(() => {
		if (taskEventVersion !== previousVersion.current) {
			previousVersion.current = taskEventVersion;
			void queryClient.invalidateQueries({queryKey: runKey});
		}
	}, [taskEventVersion, queryClient, runKey]);

	const workflowId = data?.run.workflow_id;
	// A run outlives the template it came from, so this is allowed to fail. The
	// run still renders; it just loses the template's name and step order.
	const {data: workflow} = useQuery({
		queryKey: ["workflow", workflowId],
		queryFn: () => api.getWorkflow(workflowId as string),
		enabled: workflowId != null,
		retry: false,
	});

	const tasks = useMemo(() => data?.tasks ?? [], [data]);

	/**
	 * Tasks in the order their steps run, not the order they were created.
	 *
	 * Compilation happens to emit them in topological order today, but that is
	 * an implementation detail of the compiler; sorting by the template's own
	 * graph is what keeps "review" under "draft" if it ever stops being one.
	 * Without the template — deleted, or still loading — task number is the
	 * honest fallback.
	 */
	const orderedTasks = useMemo(() => {
		if (!workflow) {
			return [...tasks].sort((a, b) => a.task_number - b.task_number);
		}
		const rank = new Map(
			orderSteps(workflow.steps, workflow.edges).ordered.map((step, index) => [
				step.step_key,
				index,
			]),
		);
		const fallback = Number.MAX_SAFE_INTEGER;
		return [...tasks].sort(
			(a, b) =>
				(rank.get(a.workflow_step_key ?? "") ?? fallback) -
					(rank.get(b.workflow_step_key ?? "") ?? fallback) ||
				a.task_number - b.task_number,
		);
	}, [tasks, workflow]);

	if (isLoading) {
		return <p className="py-8 text-center text-sm text-ink-faint">Loading run…</p>;
	}
	if (error || !data) {
		return (
			<div className="py-8 text-center text-sm text-status-error">
				Failed to load this run.
				<div className="mt-1 font-mono text-[10px] text-ink-faint">
					{error instanceof Error ? error.message : "unknown error"}
				</div>
				<div className="mt-3">
					<Link to="/workflows" className="text-xs text-accent hover:underline">
						Back to workflows
					</Link>
				</div>
			</div>
		);
	}

	const run = data.run;
	const done = orderedTasks.filter((task) => task.status === "done").length;

	return (
		<div className="flex h-full min-h-0 w-full flex-col">
			<div className="flex items-center gap-3 border-b border-app-line px-4 py-2">
				<Link
					to="/workflows/$workflowId"
					params={{workflowId: run.workflow_id}}
					className="shrink-0 text-ink-faint hover:text-ink-dull"
					title="Back to the template"
				>
					<FontAwesomeIcon icon={faArrowLeft} className="text-xs" />
				</Link>
				<div className="min-w-0 flex-1">
					<div className="flex items-baseline gap-2">
						<h1 className="truncate font-mono text-sm text-ink">
							{workflow?.workflow.name ?? "Run"}
						</h1>
						<span className="shrink-0 text-xs text-ink-dull">
							{done}/{orderedTasks.length} done
						</span>
					</div>
					<p className="truncate text-[11px] text-ink-faint">
						Launched by {run.launched_by} ·{" "}
						{new Date(run.created_at).toLocaleString()}
					</p>
				</div>
			</div>

			<div className="min-h-0 flex-1 overflow-y-auto">
				<section className="border-b border-app-line/40 px-4 py-3">
					<h3 className="mb-1 text-xs font-medium uppercase tracking-wide text-ink-dull">
						Launch input
					</h3>
					<pre className="overflow-x-auto rounded border border-app-line bg-app-box/40 px-2 py-1.5 font-mono text-[11px] leading-relaxed text-ink-dull">
						{JSON.stringify(run.inputs, null, 2)}
					</pre>
				</section>

				{orderedTasks.length === 0 ? (
					<p className="px-4 py-6 text-center text-xs text-ink-faint">
						This run produced no tasks.
					</p>
				) : (
					<ol>
						{orderedTasks.map((task, index) => (
							<RunTaskRow key={task.id} task={task} index={index} />
						))}
					</ol>
				)}
			</div>
		</div>
	);
}

function RunTaskRow({task, index}: {task: TaskItem; index: number}) {
	return (
		<li className="border-b border-app-line/40 px-4 py-3">
			<div className="flex items-start gap-3">
				<span className="mt-0.5 w-5 shrink-0 text-right font-mono text-[11px] text-ink-faint">
					{index + 1}
				</span>
				<div className="min-w-0 flex-1">
					<div className="flex flex-wrap items-baseline gap-2">
						{/* The drawer on /tasks is the task UI; this only opens it. */}
						<Link
							to="/tasks"
							search={{task: task.task_number}}
							className="truncate text-sm text-ink hover:underline"
						>
							{task.title}
						</Link>
						<span className="shrink-0 font-mono text-[10px] text-ink-faint">
							#{task.task_number}
						</span>
						{task.workflow_step_key && (
							<span
								className="shrink-0 rounded border border-app-line px-1 font-mono text-[10px] text-ink-faint"
								title="The step this task was compiled from"
							>
								{task.workflow_step_key}
							</span>
						)}
						{/* Never @spacedrive/ai's TaskStatusIcon: it throws on `blocked`,
						    which is exactly the status a stalled pipeline sits in. */}
						<TaskStatusPill status={task.status} />
					</div>

					{task.block_reason && (
						<p className="mt-1 rounded border border-status-error/30 bg-status-error/5 px-2 py-1 text-[11px] text-status-error">
							{task.block_kind ? `${task.block_kind}: ` : ""}
							{task.block_reason}
						</p>
					)}
					{!task.block_reason && task.last_error && (
						<p className="mt-1 break-words font-mono text-[10px] text-status-warning">
							{task.last_error}
						</p>
					)}

					{task.inputs != null && (
						<JsonBlock label="Inputs" value={task.inputs} muted />
					)}
					{task.outputs != null ? (
						<JsonBlock label="Outputs" value={task.outputs} />
					) : (
						<p className="mt-1 text-[11px] text-ink-faint">
							No output yet.
							{task.output_schema != null &&
								" It must match the step's declared output schema."}
						</p>
					)}
				</div>
			</div>
		</li>
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
		<div className="mt-1.5">
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
