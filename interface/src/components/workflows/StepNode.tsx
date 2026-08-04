import {memo} from "react";
import {Handle, Position, type NodeProps, type Node} from "@xyflow/react";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {
	faCheck,
	faCircleNodes,
	faCodeBranch,
	faFolderTree,
	faHandPaper,
	faHourglassHalf,
	faQuoteLeft,
	faRightToBracket,
	faRotate,
	faTerminal,
	faTriangleExclamation,
} from "@fortawesome/free-solid-svg-icons";
import type {
	GateDisposition,
	LoopArm,
	LoopResolution,
	TaskStatus,
	WorkflowStep,
} from "@/api/client";
import {styleFor} from "@/components/tasks/boardColumns";
import {STATUS_LABEL} from "@/components/tasks/taskTransitions";
import {NODE_HEIGHT, NODE_WIDTH} from "./layout";
import {RESOLUTION_HINT, RESOLUTION_SHORT} from "./loops";
import {
	EXPECT_EXIT_UNSET,
	WORKTREE_MODE_HINT,
	WORKTREE_MODE_LABEL,
	expectExitCodeMeaning,
	formatTimeout,
	isCommandStep,
	provisionsWorktree,
	worktreeModeOf,
} from "./commands";

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
/** The handle ids the two arms out of a loop are dragged from. */
export const NORMAL_HANDLE = "normal";
export const EXHAUSTED_HANDLE = "on_exhausted";

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
	/**
	 * Which loop body this step is in, if any, and whether it is the body's exit.
	 *
	 * Only the exit step can carry a give-up edge, so only the exit step gets a
	 * second source handle. Offering one everywhere would make it as easy to
	 * author an edge launch refuses as one it accepts.
	 */
	loopGroup?: string | null;
	isLoopExit?: boolean;
	/**
	 * Whether the canvas takes edits.
	 *
	 * The handle captions are an affordance — "drag from here for the give-up
	 * path" — so on a run, where nothing can be dragged, they are two words of
	 * clutter sitting on top of the edge labels that say the same thing.
	 */
	editable?: boolean;
	/** Which arms already leave this step, so a wired handle stops advertising itself. */
	armWired?: {normal: boolean; exhausted: boolean};
	/**
	 * An `on_exhausted` edge already leaves this step although it is not a loop
	 * exit.
	 *
	 * The edge endpoint is still drawn — hiding it would leave a template with an
	 * invisible reason for refusing to launch — and the handle is drawn in error
	 * so the wrong one is the one that looks wrong.
	 */
	strayExhausted?: boolean;
	/** Run only: which pass of its body this box is showing, and of how many. */
	pass?: {index: number; total: number} | null;
	/** Run only: how the loop came out, once it has. */
	resolution?: LoopResolution | null;
	/**
	 * Run only: this task is downstream of a loop and held pending its verdict.
	 *
	 * Not backlog. It may never run at all, and which arm it sits on is the whole
	 * story — see `LoopHoldNotice`.
	 */
	heldArm?: {group: string; arm: LoopArm} | null;
	/**
	 * The conditions under which this step runs at all.
	 *
	 * Drawn because a step with a condition may never run, and one without
	 * always will — a difference the box otherwise does not show, which makes a
	 * branching template look exactly like a linear one. `route` and `wait` are
	 * drawn apart because they are different facts: "this might be skipped"
	 * versus "this waits for the world".
	 */
	conditions?: NodeCondition[];
};

export interface NodeCondition {
	/** The author's label if there is one, else the predicate in words. */
	text: string;
	disposition: GateDisposition;
	/** Why the disposition is what it is. Reads as the pill's tooltip. */
	hint: string;
}

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
		loopGroup,
		isLoopExit,
		editable,
		armWired,
		strayExhausted,
		pass,
		resolution,
		heldArm,
		conditions,
	} = data;
	// One pill has room for its label; three have room for nothing. The first is
	// shown in full and the rest counted, because a reader who needs all three
	// is reading the panel anyway.
	const shownCondition = conditions?.[0];
	const extraConditions = (conditions?.length ?? 0) - 1;
	const maySkip = conditions?.some((c) => c.disposition === "route") ?? false;
	// Captioned while it is still an invitation, and not once it has been taken
	// up: the edge that leaves a wired handle carries its own label, and two
	// copies of the word "converged" a few pixels apart read as one thing said
	// twice rather than two things.
	const showNormalCaption = editable && !armWired?.normal;
	const showExhaustedCaption = editable && !armWired?.exhausted;
	// A give-up handle is offered while it can be dragged from, and afterwards
	// only if something actually leaves by it. On a finished run an empty second
	// handle is a dot advertising a path this loop never had.
	const twoHandles = strayExhausted
		? true
		: (isLoopExit ?? false) && (editable === true || armWired?.exhausted === true);
	const style = status && !placeholder ? styleFor(status) : null;
	const gaveUp =
		resolution === "exhausted_routed" || resolution === "exhausted_blocked";

	/**
	 * A shell command is not a model step and must not be drawn as one.
	 *
	 * Same argument that got loops and conditions their own treatment: a graph
	 * that renders `rm -rf build && bun run lint` identically to "summarise the
	 * findings" is lying about what it does, and the thing a reader most needs
	 * from a canvas is to know which boxes are deterministic. So the command
	 * line itself is on the box — it is the step's whole behaviour, in a way a
	 * title never is.
	 */
	const command = isCommandStep(step);
	const worktreeMode = worktreeModeOf(step);
	const provisions = provisionsWorktree(worktreeMode);

	// The border carries the one fact that matters most on that canvas: on the
	// editor, whether this is the step being edited; on a run, how its task is
	// doing. Selection still wins, because the panel on the right has to be
	// attributable to a box on the left.
	const border = selected
		? "border-accent"
		: inCycle || strayExhausted
			? "border-status-error"
			: placeholder
				? "border-dashed border-ink-faint/50"
				: heldArm
					? "border-status-warning/50"
					: status === "blocked"
						? "border-status-error/60"
						: // Ruled out: settled, but neither an achievement nor a failure.
							// Dashed says "this box did not happen" — the same grammar the
							// fan-out placeholder uses for a box that has not happened yet.
							status === "skipped"
							? "border-dashed border-ink-faint/40"
							: status === "done"
								? "border-status-success/50"
								: status === "in_progress"
									? "border-accent/60"
									: // No task yet and a condition that can rule this step out:
										// the box is provisional, and drawn as such before a run
										// exists to prove it either way.
										maySkip && !status
										? "border-dashed border-status-warning/40"
										: "border-app-line";

	return (
		<div
			className={`flex flex-col justify-between rounded-lg border bg-app-dark-box px-2.5 py-2 text-left transition-colors ${border} ${
				placeholder ? "border-dashed opacity-80" : ""
			} ${status === "skipped" ? "opacity-60" : ""} ${
				selected ? "shadow-lg shadow-accent/20" : "hover:border-ink-faint/60"
			}`}
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

			{/* The command line, on the box. A shell command is what the step *is*,
			    the way a brief is what an agent step is — and unlike a brief it is
			    short enough to read at a glance. Drawn `$`-prefixed and monospaced
			    so a command node is identifiable across the viewport without
			    reading a word of it. */}
			{command && (
				<div className="flex min-w-0 items-center gap-1">
					<FontAwesomeIcon
						icon={faTerminal}
						className="shrink-0 text-[8px] text-status-info"
					/>
					<span
						className="min-w-0 flex-1 truncate rounded border border-status-info/25 bg-status-info/5 px-1 py-px font-mono text-[9px] text-status-info"
						title={
							step.command
								? `${step.command}\n\n${
										step.command_timeout_secs != null
											? `Timeout ${formatTimeout(step.command_timeout_secs)}. `
											: ""
									}${
										step.expect_exit_code != null
											? expectExitCodeMeaning(step.expect_exit_code)
											: EXPECT_EXIT_UNSET
									}`
								: "A command step with no command line. Launch refuses this."
						}
					>
						{step.command ?? "no command line"}
					</span>
				</div>
			)}

			{/* The loop line. Present only for a body step, because for anything else
			    it would be a blank row of reserved space. */}
			{(loopGroup || heldArm || provisions) && (
				<div className="flex min-w-0 items-center gap-1">
					{/* Which checkout this runs in. A step that provisions one is doing
					    something the graph otherwise cannot show: two boxes over the
					    same repo that do not share a working tree. */}
					{provisions && (
						<span
							className="inline-flex min-w-0 shrink items-center gap-1 rounded-full border border-app-line bg-app-box/60 px-1.5 py-px text-[9px] text-ink-dull"
							title={`${WORKTREE_MODE_LABEL[worktreeMode]} — ${WORKTREE_MODE_HINT[worktreeMode]}`}
						>
							<FontAwesomeIcon
								icon={faFolderTree}
								className="shrink-0 text-[7px]"
							/>
							<span className="truncate">
								{worktreeMode === "per_branch" ? "worktree/branch" : "own worktree"}
							</span>
						</span>
					)}
					{/* The region drawn behind the body already names the loop, so the
					    node says only what the region cannot: which of its steps is
					    the exit, and therefore where both ways out leave from. */}
					{loopGroup && isLoopExit && (
						<span
							className="inline-flex shrink-0 items-center gap-1 rounded-full border border-accent/35 bg-accent/10 px-1.5 py-px text-[9px] text-accent"
							title={`Loop \`${loopGroup}\` ends here — this step's output decides whether the body goes round again.`}
						>
							<FontAwesomeIcon icon={faRotate} className="shrink-0 text-[7px]" />
							loop exit
						</span>
					)}
					{pass && (
						<span
							className="shrink-0 rounded-full border border-app-line bg-app-box/60 px-1.5 py-px text-[9px] text-ink-dull"
							title={`${pass.total} pass${pass.total === 1 ? "" : "es"} ran, one after another. Pass ${pass.index} is the last. Select this step to read every pass.`}
						>
							pass {pass.index} of {pass.total}
						</span>
					)}
					{resolution && (
						<span
							className={`inline-flex min-w-0 shrink items-center gap-1 rounded-full border px-1.5 py-px text-[9px] ${
								resolution === "converged"
									? "border-status-success/50 bg-status-success/10 text-status-success"
									: gaveUp
										? "border-status-warning/50 bg-status-warning/10 text-status-warning"
										: "border-app-line bg-app-box/60 text-ink-dull"
							}`}
							title={RESOLUTION_HINT[resolution]}
						>
							<FontAwesomeIcon
								icon={resolution === "converged" ? faCheck : faTriangleExclamation}
								className="shrink-0 text-[7px]"
							/>
							<span className="truncate">{RESOLUTION_SHORT[resolution]}</span>
						</span>
					)}
					{heldArm && (
						<span
							className="inline-flex min-w-0 shrink items-center gap-1 rounded-full border border-status-warning/50 bg-status-warning/10 px-1.5 py-px text-[9px] text-status-warning"
							title={
								heldArm.arm === "on_exhausted"
									? `Held. This is the give-up arm of loop \`${heldArm.group}\` — it runs only if that loop runs out of passes.`
									: `Held. This is the ordinary arm of loop \`${heldArm.group}\` — it runs only if that loop converges.`
							}
						>
							<FontAwesomeIcon icon={faHandPaper} className="shrink-0 text-[7px]" />
							<span className="truncate">
								held ·{" "}
								{heldArm.arm === "on_exhausted" ? "gave-up arm" : "converged arm"}
							</span>
						</span>
					)}
				</div>
			)}

			{/* The condition line. A step that may never run says so here, and the
			    label is the point — "waiting for CI on main" is what a reader can
			    act on and a pointer is what they have to decode. */}
			{shownCondition && (
				<div className="flex min-w-0 items-center gap-1">
					<span
						className={`inline-flex min-w-0 shrink items-center gap-1 rounded-full border px-1.5 py-px text-[9px] ${
							shownCondition.disposition === "route"
								? "border-status-warning/50 bg-status-warning/10 text-status-warning"
								: "border-status-info/50 bg-status-info/10 text-status-info"
						}`}
						title={shownCondition.hint}
					>
						<FontAwesomeIcon
							icon={
								shownCondition.disposition === "route"
									? faCodeBranch
									: faHourglassHalf
							}
							className="shrink-0 text-[7px]"
						/>
						<span className="truncate">
							{shownCondition.disposition === "route" ? "may skip" : "waits"} ·{" "}
							{shownCondition.text}
						</span>
					</span>
					{extraConditions > 0 && (
						<span
							className="shrink-0 rounded-full border border-app-line bg-app-box/60 px-1 py-px text-[9px] text-ink-dull"
							title={conditions
								?.slice(1)
								.map((c) => c.text)
								.join("\n")}
						>
							+{extraConditions}
						</span>
					)}
				</div>
			)}

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
				) : command ? (
					// A command step has no priority worth reading at this size — what
					// decides whether it does the right thing is the timeout and the
					// exit-code rule, and only one of those is ever non-default.
					<span
						className="inline-flex shrink-0 items-center gap-1 rounded-full border border-status-info/40 bg-status-info/10 px-1.5 py-0.5 text-[10px] text-status-info"
						title="Runs a process instead of a model. Its exit code, stdout and stderr are the step's outputs."
					>
						<FontAwesomeIcon icon={faTerminal} className="shrink-0 text-[7px]" />
						command
					</span>
				) : (
					<span className="shrink-0 rounded border border-app-line bg-app-box/50 px-1 py-px text-[9px] text-ink-faint">
						{step.priority}
					</span>
				)}

				{trouble ? (
					<span
						className={`min-w-0 flex-1 truncate text-[10px] ${
							// A skip reason is not trouble. It is the answer to "why is
							// this box here and empty", and painting it error-red would
							// report a decision the graph made correctly as a failure.
							status === "skipped"
								? "text-ink-faint"
								: heldArm
									? "text-status-warning"
									: "text-status-error"
						}`}
						title={trouble}
					>
						{trouble}
					</span>
				) : (
					<>
						{command && step.command_timeout_secs != null && (
							<Badge
								icon={faHourglassHalf}
								label={formatTimeout(step.command_timeout_secs)}
								hint={`Killed after ${step.command_timeout_secs}s, and a command that never reported is a task failure.`}
							/>
						)}
						{command && step.expect_exit_code != null && (
							<Badge
								icon={faCheck}
								label={`exit ${step.expect_exit_code}`}
								hint={expectExitCodeMeaning(step.expect_exit_code)}
							/>
						)}
						{!command && step.output_schema != null && (
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

			{/*
			 * Leaving a loop.
			 *
			 * Converging and running out are opposite results and they leave by
			 * different edges, so the exit step gets one handle for each and the
			 * choice is made by which one you drag from — before the edge exists,
			 * rather than in a dropdown after it. A single handle plus a kind
			 * picker would make the two paths identical to author and identical to
			 * mis-author; two labelled handles make them impossible to confuse.
			 */}
			{twoHandles ? (
				<>
					<Handle
						id={NORMAL_HANDLE}
						type="source"
						position={Position.Right}
						style={{top: "34%"}}
						className="!size-3 !border-2 !border-status-success !bg-app-box"
						title="Drag from here for the path taken when the loop converges."
					/>
					{showNormalCaption && (
						<span
							className="pointer-events-none absolute left-full top-[34%] ml-1.5 -translate-y-[135%] whitespace-nowrap rounded-full border border-status-success/50 bg-app-dark-box px-1.5 text-[9px] text-status-success"
							aria-hidden
						>
							converged
						</span>
					)}
					<Handle
						id={EXHAUSTED_HANDLE}
						type="source"
						position={Position.Right}
						style={{top: "74%"}}
						className={`!size-3 !border-2 !bg-app-box ${
							strayExhausted
								? "!border-status-error"
								: "!border-status-warning"
						}`}
						title={
							strayExhausted
								? "This step is not a loop exit, so a give-up edge from it cannot be honoured. Launch will refuse it."
								: "Drag from here for the path taken when the loop runs out of passes."
						}
					/>
					{(showExhaustedCaption || strayExhausted) && (
						<span
							className={`pointer-events-none absolute left-full top-[74%] ml-1.5 -translate-y-[135%] whitespace-nowrap rounded-full border bg-app-dark-box px-1.5 text-[9px] ${
								strayExhausted
									? "border-status-error/60 text-status-error"
									: "border-status-warning/60 text-status-warning"
							}`}
							aria-hidden
						>
							{strayExhausted ? "not a loop exit" : "gave up"}
						</span>
					)}
				</>
			) : (
				<Handle
					id={NORMAL_HANDLE}
					type="source"
					position={Position.Right}
					className="!size-2.5 !border !border-ink-faint !bg-app-box"
					title="Drag from here onto another step to make it wait for this one"
				/>
			)}
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
