import {useEffect, useMemo, useRef, useState} from "react";
import {Link} from "@tanstack/react-router";
import {useQuery, useQueryClient} from "@tanstack/react-query";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {
	faArrowLeft,
	faHandPaper,
	faRotate,
} from "@fortawesome/free-solid-svg-icons";
import {api, type TaskItem} from "@/api/client";
import {useLiveContext} from "@/hooks/useLiveContext";
import {TaskStatusPill} from "@/components/workflows/TaskStatusPill";
import {WorkflowCanvas} from "@/components/workflows/WorkflowCanvas";
import {useWorkflowView, ViewToggle} from "@/components/workflows/ViewToggle";
import {orderSteps, runNodes} from "@/components/workflows/graph";
import {
	RESOLUTION_HINT,
	RESOLUTION_LABEL,
	finalResolution,
	sortPasses,
} from "@/components/workflows/loops";

/**
 * One launch, and the tasks it became.
 *
 * A run is not a separate execution engine — launching compiles the template
 * into ordinary tasks with ordinary dependency edges, and the same scheduler
 * that drives the board drives these. So this screen's job is to show which
 * step became which task, what each one produced, and to hand off to the task
 * drawer for anything deeper. It deliberately does not re-implement the drawer.
 *
 * This is where the graph earns its keep. A run of a branching template is a
 * question about *where* it stopped, and a list answers that badly: eight rows
 * of "done" and one "blocked" tells you which task is stuck but not what is now
 * waiting behind it. On the canvas the stalled node is the one with live
 * branches dead-ending into it, and the branch still running beside it is
 * visibly still running.
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

	const [view, setView] = useWorkflowView();
	const [selectedKey, setSelectedKey] = useState<string | null>(null);

	/**
	 * step key → every task it compiled into.
	 *
	 * A list, not a task. This used to keep the last task it saw for each step,
	 * which was correct for exactly as long as one step meant one task. A step
	 * declaring `for_each_step_key` fans out into one task per item an upstream
	 * step produced, so three audits sharing the key `audit` collapsed into one
	 * node and two thirds of the run went missing from the canvas.
	 */
	const tasksByStep = useMemo(() => {
		const map = new Map<string, TaskItem[]>();
		for (const task of tasks) {
			if (!task.workflow_step_key) continue;
			const list = map.get(task.workflow_step_key);
			if (list) list.push(task);
			else map.set(task.workflow_step_key, [task]);
		}
		return map;
	}, [tasks]);

	// A run outlives its template, and a deleted one leaves no edges to draw. The
	// list is the honest fallback rather than an empty canvas.
	const canDrawGraph = workflow != null && workflow.steps.length > 0;
	const showCanvas = view === "canvas" && canDrawGraph;

	/**
	 * The canvas selects a node, and on a run a node is a task.
	 *
	 * So the panel is keyed by task too: clicking the `sigil` branch has to show
	 * sigil's inputs and sigil's finding, not whichever audit happened to be
	 * indexed last. The node ids come from the same helper the canvas uses, which
	 * is what keeps the two in agreement.
	 */
	/**
	 * Both shapes, because the canvas can be in either.
	 *
	 * A loop draws collapsed by default and can be expanded into one box per
	 * pass, and the two use different node ids. The panel does not need to know
	 * which mode the canvas is in — it needs to resolve whichever id it was
	 * handed — so it looks the selection up in both rather than tracking a
	 * setting it has no other use for.
	 */
	const nodes = useMemo(() => {
		const steps = workflow?.steps ?? [];
		return [
			...runNodes(steps, tasksByStep),
			...runNodes(steps, tasksByStep, {expandLoops: true}),
		];
	}, [workflow, tasksByStep]);
	const selectedNode = selectedKey
		? (nodes.find((node) => node.id === selectedKey) ?? null)
		: null;
	const selectedTask = selectedNode?.task ?? null;

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
				{canDrawGraph && <ViewToggle value={view} onChange={setView} />}
			</div>

			{showCanvas ? (
				<div className="flex min-h-0 flex-1">
					<div className="min-w-0 flex-1">
						<WorkflowCanvas
							steps={workflow.steps}
							edges={workflow.edges}
							bindings={workflow.bindings}
							cycle={orderSteps(workflow.steps, workflow.edges).cycle}
							selectedKey={selectedKey}
							onSelect={setSelectedKey}
							tasksByStep={tasksByStep}
							emptyHint="This run produced no tasks."
						/>
					</div>
					<div className="flex w-[420px] shrink-0 flex-col overflow-y-auto border-l border-app-line">
						<LaunchInput inputs={run.inputs} />
						{selectedTask ? (
							<div className="px-4 py-3">
								{selectedNode && selectedNode.passes.length > 1 ? (
									<LoopPasses
										stepKey={selectedNode.stepKey}
										passes={selectedNode.passes}
									/>
								) : (
									<RunTaskBody task={selectedTask} />
								)}
							</div>
						) : (
							<p className="px-4 py-6 text-center text-xs text-ink-faint">
								{selectedNode
									? `\`${selectedNode.stepKey}\` produced no task in this run.`
									: "Select a step to see what its task did."}
							</p>
						)}
					</div>
				</div>
			) : (
				<div className="min-h-0 flex-1 overflow-y-auto">
					<LaunchInput inputs={run.inputs} />

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
			)}
		</div>
	);
}

function LaunchInput({inputs}: {inputs: unknown}) {
	return (
		<section className="border-b border-app-line/40 px-4 py-3">
			<h3 className="mb-1 text-xs font-medium uppercase tracking-wide text-ink-dull">
				Launch input
			</h3>
			<pre className="overflow-x-auto rounded border border-app-line bg-app-box/40 px-2 py-1.5 font-mono text-[11px] leading-relaxed text-ink-dull">
				{JSON.stringify(inputs, null, 2)}
			</pre>
		</section>
	);
}

/**
 * Every pass a loop body step took, oldest first.
 *
 * The canvas collapses them into one box on purpose — three passes are one step
 * run three times, not three steps — but "which pass failed and what did it
 * say" is then a question the canvas has stopped answering, and it is the main
 * question anybody has about a loop. So the whole history lands here, in order,
 * with each pass's inputs and outputs: pass 2's input is visibly pass 1's
 * output, which is the entire mechanism of a loop shown rather than described.
 */
function LoopPasses({stepKey, passes}: {stepKey: string; passes: TaskItem[]}) {
	const ordered = sortPasses(passes);
	const resolution = finalResolution(ordered);
	const group = ordered[0]?.loop_group;

	return (
		<>
			<div className="mb-2 flex flex-wrap items-center gap-1.5">
				<FontAwesomeIcon icon={faRotate} className="text-[10px] text-accent" />
				<span className="font-mono text-xs text-ink">{stepKey}</span>
				{group && (
					<span
						className="rounded-full border border-accent/40 bg-accent/10 px-1.5 font-mono text-[10px] text-accent"
						title="The loop body this step belongs to."
					>
						loop {group}
					</span>
				)}
				<span className="text-[11px] text-ink-dull">
					{ordered.length} pass{ordered.length === 1 ? "" : "es"}, one after
					another
				</span>
				{resolution && (
					<span
						className={`rounded-full border px-1.5 text-[10px] ${
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
			<p className="mb-3 text-[10px] text-ink-faint">
				Pass {ordered.length} exists because the passes before it did not meet
				the loop's exit condition. Only the last one feeds anything downstream.
			</p>

			<ol className="space-y-3">
				{ordered.map((task, index) => (
					<li
						key={task.id}
						className="border-t border-app-line/40 pt-2 first:border-t-0 first:pt-0"
					>
						<div className="mb-1 flex items-center gap-1.5">
							<span
								className={`rounded-full border px-1.5 font-mono text-[10px] ${
									index === ordered.length - 1
										? "border-accent/50 bg-accent/10 text-accent"
										: "border-app-line bg-app-box/50 text-ink-faint"
								}`}
							>
								pass {task.loop_iteration ?? index + 1}
							</span>
							{task.loop_resolution && (
								<span
									className="text-[10px] text-ink-faint"
									title={RESOLUTION_HINT[task.loop_resolution]}
								>
									{RESOLUTION_LABEL[task.loop_resolution]}
								</span>
							)}
						</div>
						<RunTaskBody task={task} hideStepKey />
					</li>
				))}
			</ol>
		</>
	);
}

/**
 * A task held on one arm of a loop.
 *
 * Not backlog, and it must not read as backlog: it may never run at all. Which
 * arm it is on is the whole story — the loop converging and the loop giving up
 * release different tasks, and this one is waiting to find out which happened.
 */
function LoopHoldNotice({task}: {task: TaskItem}) {
	const group = task.awaiting_loop_group;
	const arm = task.awaiting_loop_arm;
	if (!group || !arm) return null;
	const gaveUp = arm === "on_exhausted";
	// Once the verdict is in, the reason on the task says which way it went, and
	// that is a decision rather than a wait.
	const decided = task.block_reason?.includes("will not run") ?? false;

	return (
		<div
			className={`mt-1 rounded border px-2 py-1.5 text-[11px] ${
				decided
					? "border-status-warning/40 bg-status-warning/5 text-status-warning"
					: "border-app-line bg-app-box/40 text-ink-dull"
			}`}
		>
			<div className="mb-0.5 flex items-center gap-1.5">
				<FontAwesomeIcon icon={faHandPaper} className="text-[9px]" />
				<span className="font-medium">
					{decided ? "Never ran" : "Held"} — {gaveUp ? "give-up" : "ordinary"}{" "}
					arm of loop <span className="font-mono">{group}</span>
				</span>
			</div>
			<p className="text-[10px] opacity-90">
				{gaveUp
					? `This step runs only if \`${group}\` runs out of passes. If the loop converges, it never runs.`
					: `This step runs only if \`${group}\` converges. If the loop runs out of passes, it never runs.`}
			</p>
			{task.block_reason && (
				<p className="mt-1 break-words font-mono text-[10px] opacity-90">
					{task.block_reason}
				</p>
			)}
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
					<RunTaskBody task={task} />
				</div>
			</div>
		</li>
	);
}

/** One task's title, status and payloads — the same either side of the toggle. */
function RunTaskBody({
	task,
	hideStepKey,
}: {
	task: TaskItem;
	/** The pass list already names the step once, at the top. */
	hideStepKey?: boolean;
}) {
	// The compiler appends the branch to the title so a task is identifiable on
	// the board. Here the branch is a badge of its own, so showing both would
	// print `[sigil]` twice on one line. A later pass is suffixed `(iteration 2)`
	// for the same reason and stripped for the same one.
	const branch = task.fan_out_branch_key;
	const suffix = branch ? ` [${branch}]` : "";
	let title =
		suffix && task.title.endsWith(suffix)
			? task.title.slice(0, -suffix.length)
			: task.title;
	if (hideStepKey && task.loop_iteration != null) {
		title = title.replace(/\s*\(iteration \d+\)$/, "");
	}
	const held = task.awaiting_loop_group != null && task.awaiting_loop_arm != null;

	return (
		<>
			<div className="flex flex-wrap items-baseline gap-2">
				{/* The drawer on /tasks is the task UI; this only opens it. */}
				<Link
					to="/tasks"
					search={{task: task.task_number}}
					className="truncate text-sm text-ink hover:underline"
				>
					{title}
				</Link>
				<span className="shrink-0 font-mono text-[10px] text-ink-faint">
					#{task.task_number}
				</span>
				{task.workflow_step_key && !hideStepKey && (
					<span
						className="shrink-0 rounded border border-app-line px-1 font-mono text-[10px] text-ink-faint"
						title="The step this task was compiled from"
					>
						{task.workflow_step_key}
					</span>
				)}
				{branch && (
					<span
						className="shrink-0 rounded border border-accent/40 bg-accent/10 px-1 font-mono text-[10px] text-accent"
						title="Which branch of the step's fan-out this task is. A fan-in downstream collects the branches by this key."
					>
						{branch}
					</span>
				)}
				{task.loop_group && !hideStepKey && (
					<span
						className="inline-flex shrink-0 items-center gap-1 rounded border border-accent/40 bg-accent/10 px-1 font-mono text-[10px] text-accent"
						title={`Pass ${task.loop_iteration ?? "?"} of loop body \`${task.loop_group}\`. Passes are sequential — this one exists because the one before it did not converge.`}
					>
						<FontAwesomeIcon icon={faRotate} className="text-[7px]" />
						{task.loop_group} · pass {task.loop_iteration ?? "?"}
					</span>
				)}
				{/* Never @spacedrive/ai's TaskStatusIcon: it throws on `blocked`,
				    which is exactly the status a stalled pipeline sits in. */}
				<TaskStatusPill status={task.status} />
			</div>

			{task.fan_out_placeholder && (
				<p className="mt-1 rounded border border-dashed border-app-line bg-app-box/30 px-2 py-1 text-[11px] text-ink-faint">
					Waiting to fan out. This is not work — it holds the edges its
					branches will inherit, so the steps after it have something to wait
					on. It is replaced by one task per item once the step it iterates
					finishes.
				</p>
			)}

			{/* A held arm explains itself; the generic block box would report it as
			    an ordinary upstream wait, which is the one thing it is not. */}
			{held ? (
				<LoopHoldNotice task={task} />
			) : (
				task.block_reason && (
					<p className="mt-1 rounded border border-status-error/30 bg-status-error/5 px-2 py-1 text-[11px] text-status-error">
						{task.block_kind ? `${task.block_kind}: ` : ""}
						{task.block_reason}
					</p>
				)
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
		</>
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
