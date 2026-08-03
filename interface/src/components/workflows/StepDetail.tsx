import {useEffect, useMemo, useState} from "react";
import {Button} from "@spacedrive/primitives";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {
	faArrowRightLong,
	faCodeBranch,
	faHourglassHalf,
	faPen,
	faQuoteLeft,
	faRotate,
	faTriangleExclamation,
	faXmark,
} from "@fortawesome/free-solid-svg-icons";
import type {
	GateDisposition,
	SaveBindingRequest,
	SaveStepGateRequest,
	SaveStepRequest,
	StepBinding,
	StepGate,
	TaskPriority,
	WorkflowEdge,
	WorkflowStep,
} from "@/api/client";
import {ancestorsOf, parentsByStep, wouldCycle} from "./graph";
import {
	DEFAULT_LOOP_MAX_ITERATIONS,
	MAX_LOOP_ITERATIONS,
	loopBodies,
	readPredicate,
	type LoopBody,
	type LoopPredicate,
} from "./loops";
import {
	DISPOSITION_HINT,
	DISPOSITION_LABEL,
	MIN_POLL_INTERVAL_SECS,
	deriveStepDisposition,
	describeCondition,
	gatesForStep,
	type DispositionChoice,
} from "./conditions";
import {parseJson} from "./schemaForm";

const PRIORITIES: TaskPriority[] = ["critical", "high", "medium", "low"];

export interface StepDetailProps {
	step: WorkflowStep;
	steps: WorkflowStep[];
	edges: WorkflowEdge[];
	bindings: StepBinding[];
	/** Every condition in the template. Filtered to this step inside. */
	gates: StepGate[];
	/** Whether the template declares a launch input at all. */
	hasRunInput: boolean;
	agents: {id: string; display_name?: string | null}[];
	onSave: (stepKey: string, body: SaveStepRequest) => void;
	onDelete: (stepKey: string) => void;
	onAddEdge: (
		parentStepKey: string,
		childStepKey: string,
		kind?: "normal" | "on_exhausted",
	) => void;
	onRemoveEdge: (parentStepKey: string, childStepKey: string) => void;
	onSetBinding: (
		stepKey: string,
		inputKey: string,
		body: SaveBindingRequest,
	) => void;
	onRemoveBinding: (stepKey: string, inputKey: string) => void;
	onSetGate: (
		stepKey: string,
		gateKey: string,
		body: SaveStepGateRequest,
	) => void;
	onRemoveGate: (stepKey: string, gateKey: string) => void;
	stepBusy?: boolean;
	stepError?: string | null;
	edgeBusy?: boolean;
	edgeError?: string | null;
	bindingBusy?: boolean;
	bindingError?: string | null;
	gateBusy?: boolean;
	gateError?: string | null;
}

/**
 * Everything about one step: what it does, what it waits for, what it is fed.
 *
 * Those three are one panel rather than three tabs because they are one
 * decision. An input bound to `draft` is meaningless unless this step also
 * waits for `draft` — the value would be read before it exists — and splitting
 * the two apart is precisely how that mistake gets made.
 */
export function StepDetail(props: StepDetailProps) {
	const {step, steps, edges, bindings} = props;
	const parents = useMemo(
		() => parentsByStep(edges).get(step.step_key) ?? [],
		[edges, step.step_key],
	);
	const stepBindings = useMemo(
		() => bindings.filter((b) => b.step_key === step.step_key),
		[bindings, step.step_key],
	);
	const stepGates = useMemo(
		() => gatesForStep(props.gates, step.step_key),
		[props.gates, step.step_key],
	);
	// Anything upstream at any depth, which is what "will have finished" means.
	const ancestors = useMemo(
		() => ancestorsOf(edges, step.step_key),
		[edges, step.step_key],
	);
	const bodies = useMemo(() => loopBodies(steps, edges), [steps, edges]);
	const ownBody = step.loop_group ? (bodies.get(step.loop_group) ?? null) : null;

	return (
		<div className="flex h-full min-h-0 flex-col overflow-y-auto">
			<StepFields
				key={step.step_key}
				step={step}
				steps={steps}
				edges={edges}
				agents={props.agents}
				busy={props.stepBusy}
				error={props.stepError ?? null}
				onSave={(body) => props.onSave(step.step_key, body)}
				onDelete={() => props.onDelete(step.step_key)}
			/>
			<Dependencies
				step={step}
				steps={steps}
				edges={edges}
				parents={parents}
				bodies={bodies}
				busy={props.edgeBusy}
				error={props.edgeError ?? null}
				onAdd={(parentKey, kind) =>
					props.onAddEdge(parentKey, step.step_key, kind)
				}
				onRemove={(parentKey) => props.onRemoveEdge(parentKey, step.step_key)}
			/>
			<Conditions
				step={step}
				steps={steps}
				edges={edges}
				gates={stepGates}
				busy={props.gateBusy}
				error={props.gateError ?? null}
				onSet={(gateKey, body) => props.onSetGate(step.step_key, gateKey, body)}
				onRemove={(gateKey) => props.onRemoveGate(step.step_key, gateKey)}
			/>
			<Bindings
				step={step}
				steps={steps}
				parents={parents}
				ancestors={ancestors}
				body={ownBody}
				bindings={stepBindings}
				hasRunInput={props.hasRunInput}
				busy={props.bindingBusy}
				error={props.bindingError ?? null}
				onSet={(inputKey, body) =>
					props.onSetBinding(step.step_key, inputKey, body)
				}
				onRemove={(inputKey) => props.onRemoveBinding(step.step_key, inputKey)}
			/>
		</div>
	);
}

function Section({
	title,
	hint,
	children,
}: {
	title: string;
	hint?: string;
	children: React.ReactNode;
}) {
	return (
		<div className="border-t border-app-line/40 px-4 py-3 first:border-t-0">
			<h3 className="text-xs font-medium uppercase tracking-wide text-ink-dull">
				{title}
			</h3>
			{hint && <p className="mb-2 mt-0.5 text-[10px] text-ink-faint">{hint}</p>}
			<div className={hint ? "" : "mt-2"}>{children}</div>
		</div>
	);
}

/**
 * The step's own fields.
 *
 * `PUT /steps/{key}` replaces the step wholesale, so every field is sent on
 * every save — posting only what changed would blank the rest. The key itself
 * is not editable here: it is what edges and bindings reference, and renaming
 * it in place would leave both pointing at a step that no longer exists.
 */
function StepFields({
	step,
	steps,
	edges,
	agents,
	busy,
	error,
	onSave,
	onDelete,
}: {
	step: WorkflowStep;
	steps: WorkflowStep[];
	edges: WorkflowEdge[];
	agents: {id: string; display_name?: string | null}[];
	busy?: boolean;
	error: string | null;
	onSave: (body: SaveStepRequest) => void;
	onDelete: () => void;
}) {
	const [title, setTitle] = useState(step.title);
	const [description, setDescription] = useState(step.description ?? "");
	const [priority, setPriority] = useState<string>(step.priority);
	const [systemPrompt, setSystemPrompt] = useState(step.system_prompt ?? "");
	const [agentId, setAgentId] = useState(step.assigned_agent_id ?? "");
	const [inputSchema, setInputSchema] = useState(() => format(step.input_schema));
	const [outputSchema, setOutputSchema] = useState(() =>
		format(step.output_schema),
	);
	const [localError, setLocalError] = useState<string | null>(null);
	const [confirmDelete, setConfirmDelete] = useState(false);

	const [loopGroup, setLoopGroup] = useState(step.loop_group ?? "");
	const [maxIterations, setMaxIterations] = useState(() =>
		step.loop_max_iterations == null ? "" : String(step.loop_max_iterations),
	);
	const initialPredicate = readPredicate(step.loop_until);
	const [pointer, setPointer] = useState(initialPredicate?.pointer ?? "");
	const [mode, setMode] = useState<"equals" | "any_of" | "present">(
		initialPredicate?.mode ?? "equals",
	);
	const [expected, setExpected] = useState(() =>
		initialPredicate?.mode === "equals"
			? JSON.stringify(initialPredicate.value)
			: initialPredicate?.mode === "any_of"
				? JSON.stringify(initialPredicate.values)
				: "true",
	);

	// A step saved elsewhere (or reordered by a sibling's save) must not leave
	// stale text in these boxes. Keyed remount handles switching steps; this
	// handles the same step coming back changed.
	useEffect(() => {
		setTitle(step.title);
		setDescription(step.description ?? "");
		setPriority(step.priority);
		setSystemPrompt(step.system_prompt ?? "");
		setAgentId(step.assigned_agent_id ?? "");
		setInputSchema(format(step.input_schema));
		setOutputSchema(format(step.output_schema));
		setLoopGroup(step.loop_group ?? "");
		setMaxIterations(
			step.loop_max_iterations == null ? "" : String(step.loop_max_iterations),
		);
		const predicate = readPredicate(step.loop_until);
		setPointer(predicate?.pointer ?? "");
		setMode(predicate?.mode ?? "equals");
		setExpected(
			predicate?.mode === "equals"
				? JSON.stringify(predicate.value)
				: predicate?.mode === "any_of"
					? JSON.stringify(predicate.values)
					: "true",
		);
	}, [step]);

	/**
	 * The body this step would be in if the typed group were saved.
	 *
	 * Computed against the group as *typed*, not as stored, so the panel can say
	 * "this is the exit step" while the name is still being entered rather than
	 * after a save-and-look. The exit is whichever body step nothing else in the
	 * body waits on, which is a fact about the edges and therefore knowable here.
	 */
	const prospective = useMemo(() => {
		const group = loopGroup.trim();
		if (group === "") return null;
		const patched = steps.map((candidate) =>
			candidate.step_key === step.step_key
				? {...candidate, loop_group: group}
				: candidate,
		);
		return loopBodies(patched, edges).get(group) ?? null;
	}, [loopGroup, steps, edges, step.step_key]);
	const isExit = prospective?.exit?.step_key === step.step_key;
	const existingGroups = useMemo(
		() =>
			[
				...new Set(
					steps
						.map((candidate) => candidate.loop_group)
						.filter((group): group is string => !!group),
				),
			].sort(),
		[steps],
	);

	const submit = () => {
		const trimmed = title.trim();
		if (trimmed === "") {
			setLocalError("A step needs a title — it becomes the task's title.");
			return;
		}
		const input = parseJson(inputSchema);
		if ("error" in input) {
			setLocalError(`Input schema: ${input.error}`);
			return;
		}
		const output = parseJson(outputSchema);
		if ("error" in output) {
			setLocalError(`Output schema: ${output.error}`);
			return;
		}

		// Loop settings are read from the exit step and nowhere else — the server
		// refuses a launch that finds them elsewhere rather than leaving a number
		// that does nothing. So they are sent only from the step that owns them.
		const group = loopGroup.trim() || null;
		let until: unknown = null;
		let iterations: number | null = null;
		if (group && isExit) {
			const trimmedPointer = pointer.trim();
			if (trimmedPointer === "") {
				setLocalError(
					"A loop needs a pointer saying what to read in this step's output — without one it always runs its whole budget, which is a retry, not a loop.",
				);
				return;
			}
			if (mode === "equals") {
				const parsed = parseJson(expected);
				if ("error" in parsed) {
					setLocalError(`Exit condition: ${parsed.error}`);
					return;
				}
				if (parsed.value === undefined || parsed.value === null) {
					setLocalError(
						"The exit condition needs a value to compare against. A bare string still needs its quotes.",
					);
					return;
				}
				until = {pointer: trimmedPointer, equals: parsed.value};
			} else if (mode === "any_of") {
				const parsed = parseJson(expected);
				if ("error" in parsed) {
					setLocalError(`Exit condition: ${parsed.error}`);
					return;
				}
				if (!Array.isArray(parsed.value) || parsed.value.length === 0) {
					setLocalError(
						'"One of" takes a JSON array of the values that count as done, e.g. ["green", "clean"].',
					);
					return;
				}
				until = {pointer: trimmedPointer, any_of: parsed.value};
			} else {
				until = {pointer: trimmedPointer};
			}

			if (maxIterations.trim() !== "") {
				const parsed = Number(maxIterations.trim());
				if (!Number.isInteger(parsed)) {
					setLocalError("Passes must be a whole number.");
					return;
				}
				if (parsed < 1 || parsed > MAX_LOOP_ITERATIONS) {
					setLocalError(
						`Passes must be between 1 and ${MAX_LOOP_ITERATIONS} — every pass is a live model call, so the ceiling is enforced rather than trusted.`,
					);
					return;
				}
				iterations = parsed;
			}
		}

		setLocalError(null);
		onSave({
			title: trimmed,
			description: description.trim() || null,
			priority,
			system_prompt: systemPrompt.trim() || null,
			assigned_agent_id: agentId || null,
			input_schema: input.value,
			output_schema: output.value,
			position: step.position,
			// Sent on every save because `PUT /steps/{key}` replaces the step
			// wholesale: omitting these would quietly un-loop a body the moment
			// somebody fixed a typo in its title.
			loop_group: group,
			loop_until: until,
			loop_max_iterations: iterations,
			// Same hazard, and nothing here edits them. There is no fan-out
			// control in this panel yet, so these are carried through untouched
			// rather than dropped — otherwise renaming a step would silently
			// un-fan-out it, and the run would go from N branches to one with
			// nothing on screen having said so.
			for_each_step_key: step.for_each_step_key,
			for_each_pointer: step.for_each_pointer,
			for_each_key: step.for_each_key,
		});
	};

	return (
		<Section title="Step">
			<div className="mb-2 flex items-center gap-2">
				<span
					className="shrink-0 rounded border border-app-line bg-app-box/50 px-1.5 py-0.5 font-mono text-[11px] text-ink-dull"
					title="The key edges and bindings reference. Delete and re-add to rename."
				>
					{step.step_key}
				</span>
				<select
					value={priority}
					onChange={(event) => setPriority(event.target.value)}
					className="rounded border border-app-line bg-app px-1.5 py-1 text-[11px] text-ink outline-none focus:border-accent"
				>
					{PRIORITIES.map((value) => (
						<option key={value} value={value}>
							{value}
						</option>
					))}
					{/* A priority the server knows and this build does not must still
					    round-trip rather than silently becoming `critical`. */}
					{!PRIORITIES.includes(priority as TaskPriority) && (
						<option value={priority}>{priority}</option>
					)}
				</select>
			</div>

			<Field label="Title">
				<input
					value={title}
					onChange={(event) => setTitle(event.target.value)}
					className="w-full rounded border border-app-line bg-app px-2 py-1 text-xs text-ink outline-none focus:border-accent"
				/>
			</Field>

			<Field
				label="Description"
				hint="The brief the worker is given. This is the task's body."
			>
				<textarea
					value={description}
					onChange={(event) => setDescription(event.target.value)}
					rows={4}
					className="w-full rounded border border-app-line bg-app px-2 py-1.5 text-[11px] text-ink outline-none focus:border-accent"
				/>
			</Field>

			<Field
				label="System prompt"
				hint="Appended to the worker prompt when this step runs. Standing instructions, not the task itself."
			>
				<textarea
					value={systemPrompt}
					onChange={(event) => setSystemPrompt(event.target.value)}
					rows={3}
					spellCheck={false}
					placeholder="Always answer in British English."
					className="w-full rounded border border-app-line bg-app px-2 py-1.5 text-[11px] text-ink outline-none focus:border-accent"
				/>
			</Field>

			<Field
				label="Assigned agent"
				hint="Blank runs the step as whoever launched the run."
			>
				<select
					value={agentId}
					onChange={(event) => setAgentId(event.target.value)}
					className="w-full rounded border border-app-line bg-app px-2 py-1 text-[11px] text-ink outline-none focus:border-accent"
				>
					<option value="">Whoever launches the run</option>
					{agents.map((agent) => (
						<option key={agent.id} value={agent.id}>
							{agent.display_name ?? agent.id}
						</option>
					))}
					{agentId && !agents.some((a) => a.id === agentId) && (
						<option value={agentId}>{agentId} (unknown agent)</option>
					)}
				</select>
			</Field>

			<Field
				label="Input schema"
				hint="What this step needs before it runs. Each key here wants a binding below."
			>
				<textarea
					value={inputSchema}
					onChange={(event) => setInputSchema(event.target.value)}
					rows={5}
					spellCheck={false}
					placeholder={'{\n  "type": "object",\n  "required": ["headline"]\n}'}
					className="w-full rounded border border-app-line bg-app px-2 py-1.5 font-mono text-[11px] text-ink outline-none focus:border-accent"
				/>
			</Field>

			<Field
				label="Output schema"
				hint="What it must produce. Enforced when the worker calls task_complete."
			>
				<textarea
					value={outputSchema}
					onChange={(event) => setOutputSchema(event.target.value)}
					rows={5}
					spellCheck={false}
					placeholder={
						'{\n  "type": "object",\n  "properties": {"headline": {"type": "string"}}\n}'
					}
					className="w-full rounded border border-app-line bg-app px-2 py-1.5 font-mono text-[11px] text-ink outline-none focus:border-accent"
				/>
			</Field>

			<div className="mb-2 mt-3 border-t border-app-line/40 pt-2">
				<div className="mb-1 flex items-center gap-1.5">
					<FontAwesomeIcon icon={faRotate} className="text-[9px] text-accent" />
					<span className="text-[11px] font-medium text-ink-dull">Loop</span>
				</div>

				<Field
					label="Loop body"
					hint="Every step sharing this name is one body, and the whole body runs again until it converges or runs out. Blank for an ordinary step."
				>
					<input
						value={loopGroup}
						onChange={(event) => setLoopGroup(event.target.value)}
						spellCheck={false}
						list={`loop-groups-${step.step_key}`}
						placeholder="revise"
						className="w-full rounded border border-app-line bg-app px-2 py-1 font-mono text-[11px] text-ink outline-none focus:border-accent"
					/>
					<datalist id={`loop-groups-${step.step_key}`}>
						{existingGroups.map((group) => (
							<option key={group} value={group} />
						))}
					</datalist>
				</Field>

				{loopGroup.trim() !== "" &&
					(isExit ? (
						<>
							<p className="mb-2 rounded border border-accent/30 bg-accent/5 px-2 py-1 text-[10px] text-ink-dull">
								Nothing else in{" "}
								<span className="font-mono text-accent">
									{loopGroup.trim()}
								</span>{" "}
								waits for this step, so this is the body's{" "}
								<strong className="font-medium text-ink">exit step</strong>: its
								output decides whether the body goes round again, and both ways
								out of the loop leave from here.
							</p>

							<Field
								label="Stop when"
								hint="Read from this step's output after each pass. The same shape a task_output gate takes."
							>
								<PredicateFields
									value={{pointer, mode, expected}}
									pointerPlaceholder="/converged"
									pointerTitle="An RFC 6901 pointer into this step's outputs."
									onChange={(next) => {
										setPointer(next.pointer);
										setMode(next.mode);
										setExpected(next.expected);
									}}
								/>
							</Field>

							<Field
								label="Passes"
								hint={`How many times the body may run before the loop gives up. Blank means ${DEFAULT_LOOP_MAX_ITERATIONS}. Ceiling is ${MAX_LOOP_ITERATIONS} — every pass is a live model call.`}
							>
								<div className="flex items-center gap-2">
									<input
										value={maxIterations}
										onChange={(event) => setMaxIterations(event.target.value)}
										inputMode="numeric"
										spellCheck={false}
										placeholder={String(DEFAULT_LOOP_MAX_ITERATIONS)}
										className="w-20 rounded border border-app-line bg-app px-2 py-1 font-mono text-[11px] text-ink outline-none focus:border-accent"
									/>
									<span className="text-[10px] text-ink-faint">
										{maxIterations.trim() === ""
											? `defaults to ${DEFAULT_LOOP_MAX_ITERATIONS} passes`
											: `at most ${maxIterations.trim()} pass${
													maxIterations.trim() === "1" ? "" : "es"
												}`}
									</span>
								</div>
							</Field>

							<p className="mb-2 text-[10px] text-ink-faint">
								Converging and giving up leave by different edges. Draw them from
								this step's two handles on the canvas — the upper one for
								converged, the lower for gave up.
							</p>
						</>
					) : prospective && prospective.exit ? (
						<p className="mb-2 rounded border border-app-line bg-app-box/40 px-2 py-1 text-[10px] text-ink-faint">
							Something else in this body waits for this step, so the exit
							condition lives on{" "}
							<span className="font-mono text-ink-dull">
								{prospective.exit.step_key}
							</span>{" "}
							— the one step in{" "}
							<span className="font-mono">{loopGroup.trim()}</span> with nothing
							after it inside the body. Set anywhere else it would be a number
							nothing reads, so launch refuses that.
						</p>
					) : (
						<p className="mb-2 flex items-start gap-1.5 rounded border border-status-warning/30 bg-status-warning/5 px-2 py-1 text-[10px] text-status-warning">
							<FontAwesomeIcon
								icon={faTriangleExclamation}
								className="mt-0.5 shrink-0 text-[9px]"
							/>
							<span>
								{prospective && prospective.exitCandidates.length === 0
									? `Every step in \`${loopGroup.trim()}\` waits for another one in it, so nothing decides whether to go round again. Launch will refuse this body.`
									: `\`${loopGroup.trim()}\` has more than one step with nothing after it (${prospective?.exitCandidates.join(", ")}), so it has no single exit. Wire the body into a line, or launch will refuse it.`}
							</span>
						</p>
					))}
			</div>

			{(localError || error) && (
				<p className="mb-2 break-words rounded border border-status-error/30 bg-status-error/5 px-2 py-1 font-mono text-[11px] text-status-error">
					{localError ?? error}
				</p>
			)}

			<div className="flex flex-wrap items-center gap-2">
				<Button
					size="sm"
					variant="accent"
					className="whitespace-nowrap"
					disabled={busy}
					onClick={submit}
				>
					{busy ? "Saving…" : "Save step"}
				</Button>
				{confirmDelete ? (
					<>
						<span className="basis-full text-[11px] text-ink-dull">
							Removes its edges and bindings too.
						</span>
						<Button
							size="sm"
							variant="colored"
							className="border-status-error bg-status-error"
							disabled={busy}
							onClick={onDelete}
						>
							Delete
						</Button>
						<Button
							size="sm"
							variant="gray"
							onClick={() => setConfirmDelete(false)}
						>
							Cancel
						</Button>
					</>
				) : (
					<button
						type="button"
						onClick={() => setConfirmDelete(true)}
						className="text-[11px] text-ink-faint hover:text-status-error hover:underline"
					>
						Delete step…
					</button>
				)}
			</div>
		</Section>
	);
}

function Field({
	label,
	hint,
	children,
}: {
	label: string;
	hint?: string;
	children: React.ReactNode;
}) {
	return (
		<div className="mb-2">
			<label className="mb-0.5 block text-[11px] font-medium text-ink-dull">
				{label}
			</label>
			{hint && <p className="mb-1 text-[10px] text-ink-faint">{hint}</p>}
			{children}
		</div>
	);
}

/**
 * The predicate, as three controls: where to read, how to compare, and what to.
 *
 * One control rather than two because a loop's exit condition and a step's
 * condition are the *same* predicate language — `loop_until` and a
 * `task_output` gate are read by the same evaluator on the server. Two editors
 * for one grammar is two places for "is one of" to start meaning something
 * slightly different.
 */
export interface PredicateDraft {
	pointer: string;
	mode: "equals" | "any_of" | "present";
	/** The comparison value as typed JSON. Unused when `mode` is `present`. */
	expected: string;
}

/** A stored predicate as editable text. The inverse of `buildPredicate`. */
function draftFromPredicate(predicate: LoopPredicate | null): PredicateDraft {
	return {
		pointer: predicate?.pointer ?? "",
		mode: predicate?.mode ?? "equals",
		expected:
			predicate?.mode === "equals"
				? JSON.stringify(predicate.value)
				: predicate?.mode === "any_of"
					? JSON.stringify(predicate.values)
					: "true",
	};
}

/**
 * The draft as the object the server stores, or the reason it cannot be.
 *
 * Returns the message rather than throwing so each caller can prefix it with
 * what the reader was editing — "Exit condition:" or "Condition:" — since the
 * same malformed JSON means different things in the two places.
 */
function buildPredicate(
	draft: PredicateDraft,
): {value: Record<string, unknown>} | {error: string} {
	const pointer = draft.pointer.trim();
	if (pointer === "") {
		return {error: "needs a pointer saying what to read."};
	}
	if (draft.mode === "present") return {value: {pointer}};
	const parsed = parseJson(draft.expected);
	if ("error" in parsed) return {error: parsed.error};
	if (draft.mode === "equals") {
		if (parsed.value === undefined || parsed.value === null) {
			return {
				error:
					"needs a value to compare against. A bare string still needs its quotes.",
			};
		}
		return {value: {pointer, equals: parsed.value}};
	}
	if (!Array.isArray(parsed.value) || parsed.value.length === 0) {
		return {
			error:
				'"is one of" takes a JSON array of the values that count, e.g. ["green", "clean"].',
		};
	}
	return {value: {pointer, any_of: parsed.value}};
}

function PredicateFields({
	value,
	onChange,
	pointerPlaceholder,
	pointerTitle,
}: {
	value: PredicateDraft;
	onChange: (next: PredicateDraft) => void;
	pointerPlaceholder: string;
	pointerTitle: string;
}) {
	return (
		<div className="flex items-center gap-1.5">
			<input
				value={value.pointer}
				onChange={(event) => onChange({...value, pointer: event.target.value})}
				spellCheck={false}
				placeholder={pointerPlaceholder}
				title={pointerTitle}
				className="min-w-0 flex-1 rounded border border-app-line bg-app px-2 py-1 font-mono text-[11px] text-ink outline-none focus:border-accent"
			/>
			<select
				value={value.mode}
				onChange={(event) =>
					onChange({
						...value,
						mode: event.target.value as PredicateDraft["mode"],
					})
				}
				className="shrink-0 rounded border border-app-line bg-app px-1.5 py-1 text-[11px] text-ink outline-none focus:border-accent"
			>
				<option value="equals">is</option>
				<option value="any_of">is one of</option>
				<option value="present">is present</option>
			</select>
			{value.mode !== "present" && (
				<input
					value={value.expected}
					onChange={(event) =>
						onChange({...value, expected: event.target.value})
					}
					spellCheck={false}
					placeholder={value.mode === "equals" ? "true" : '["green"]'}
					title={
						value.mode === "equals"
							? "JSON. A bare string still needs its quotes."
							: "A JSON array of the values that count."
					}
					className="w-24 shrink-0 rounded border border-app-line bg-app px-2 py-1 font-mono text-[11px] text-ink outline-none focus:border-accent"
				/>
			)}
		</div>
	);
}

/**
 * Whether this step runs at all.
 *
 * Its own section rather than a field on the step, because a condition is not a
 * property of the work — it is the question asked before the work is considered,
 * and there can be more than one. Kept next to Dependencies since the two are
 * read together: an edge says *when* this step is considered, a condition says
 * *whether* it then runs.
 */
function Conditions({
	step,
	steps,
	edges,
	gates,
	busy,
	error,
	onSet,
	onRemove,
}: {
	step: WorkflowStep;
	steps: WorkflowStep[];
	edges: WorkflowEdge[];
	gates: StepGate[];
	busy?: boolean;
	error: string | null;
	onSet: (gateKey: string, body: SaveStepGateRequest) => void;
	onRemove: (gateKey: string) => void;
}) {
	// `null` is the closed state; `""` means the blank form for a new condition.
	// A gate key means editing that one, and it is the form's `key` so switching
	// between two conditions remounts rather than leaving the first one's URL in
	// the second one's box.
	const [editing, setEditing] = useState<string | null>(null);
	const [confirmRemove, setConfirmRemove] = useState<string | null>(null);

	return (
		<Section
			title="Conditions"
			hint="When this step should run at all. No condition means it always runs once its prerequisites are done."
		>
			{gates.length > 0 && (
				<ul className="mb-2 flex flex-col gap-1.5">
					{gates.map((gate) => {
						const derived = deriveStepDisposition(
							gate.kind,
							gate.source_step_key,
							step.step_key,
							edges,
						);
						const effective = gate.disposition ?? derived.disposition;
						const routes = effective === "route";
						return (
							<li
								key={gate.gate_key}
								className="rounded border border-app-line bg-app-box/40 px-2 py-1.5"
							>
								<div className="flex items-start gap-1.5">
									<FontAwesomeIcon
										icon={routes ? faCodeBranch : faHourglassHalf}
										className={`mt-[3px] shrink-0 text-[9px] ${
											routes ? "text-status-warning" : "text-status-info"
										}`}
										title={DISPOSITION_LABEL[effective]}
									/>
									<div className="min-w-0 flex-1">
										<p className="break-words text-[11px] text-ink">
											{describeCondition(gate)}
										</p>
										<p className="mt-0.5 text-[10px] text-ink-faint">
											<span
												className={
													routes ? "text-status-warning" : "text-status-info"
												}
											>
												{routes
													? "skips this step if false"
													: "holds this step until true"}
											</span>
											{gate.disposition == null && (
												<span title={derived.because}> · derived</span>
											)}
											<span className="font-mono"> · {gate.gate_key}</span>
										</p>
									</div>
									<button
										type="button"
										onClick={() =>
											setEditing(editing === gate.gate_key ? null : gate.gate_key)
										}
										title="Edit this condition"
										className="shrink-0 text-ink-faint hover:text-ink"
									>
										<FontAwesomeIcon icon={faPen} className="text-[9px]" />
									</button>
									<button
										type="button"
										onClick={() =>
											setConfirmRemove(
												confirmRemove === gate.gate_key ? null : gate.gate_key,
											)
										}
										title="Remove this condition"
										className="shrink-0 text-ink-faint hover:text-status-error"
									>
										<FontAwesomeIcon icon={faXmark} className="text-[10px]" />
									</button>
								</div>

								{confirmRemove === gate.gate_key && (
									<div className="mt-1.5 flex items-center gap-2 border-t border-app-line/40 pt-1.5">
										<span className="text-[10px] text-ink-dull">
											{routes
												? "The step will then always run."
												: "The step will no longer wait for this."}
										</span>
										<Button
											size="sm"
											variant="colored"
											className="border-status-error bg-status-error"
											disabled={busy}
											onClick={() => {
												onRemove(gate.gate_key);
												setConfirmRemove(null);
											}}
										>
											Remove
										</Button>
									</div>
								)}

								{editing === gate.gate_key && (
									<div className="mt-2 border-t border-app-line/40 pt-2">
										<ConditionForm
											key={gate.gate_key}
											gate={gate}
											step={step}
											steps={steps}
											edges={edges}
											busy={busy}
											onSubmit={(body) => {
												onSet(gate.gate_key, body);
												setEditing(null);
											}}
											onCancel={() => setEditing(null)}
										/>
									</div>
								)}
							</li>
						);
					})}
				</ul>
			)}

			{editing === "" ? (
				<div className="rounded border border-accent/30 bg-accent/5 px-2 py-2">
					<ConditionForm
						key="__new__"
						gate={null}
						step={step}
						steps={steps}
						edges={edges}
						busy={busy}
						onSubmit={(body, gateKey) => {
							onSet(gateKey, body);
							setEditing(null);
						}}
						onCancel={() => setEditing(null)}
					/>
				</div>
			) : (
				<button
					type="button"
					onClick={() => setEditing("")}
					className="text-[11px] text-accent hover:underline"
				>
					Add a condition…
				</button>
			)}

			{error && (
				<p className="mt-2 break-words rounded border border-status-error/30 bg-status-error/5 px-2 py-1 font-mono text-[11px] text-status-error">
					{error}
				</p>
			)}
		</Section>
	);
}

/**
 * One condition's fields.
 *
 * The disposition control shows what *Derive* currently resolves to and why. A
 * silent default here is a branch that surprises someone: the same predicate
 * with the other disposition either holds the pipeline forever or skips past a
 * step that should have run, and neither failure announces itself. Making the
 * derivation legible is what earns it the right to be the default.
 */
function ConditionForm({
	gate,
	step,
	steps,
	edges,
	busy,
	onSubmit,
	onCancel,
}: {
	gate: StepGate | null;
	step: WorkflowStep;
	steps: WorkflowStep[];
	edges: WorkflowEdge[];
	busy?: boolean;
	onSubmit: (body: SaveStepGateRequest, gateKey: string) => void;
	onCancel: () => void;
}) {
	const config = useMemo(
		() => ((gate?.config ?? {}) as Record<string, unknown>) ?? {},
		[gate],
	);
	const [gateKey, setGateKey] = useState(gate?.gate_key ?? "");
	const [kind, setKind] = useState<string>(gate?.kind ?? "task_output");
	const [label, setLabel] = useState(gate?.label ?? "");
	const [sourceStepKey, setSourceStepKey] = useState(gate?.source_step_key ?? "");
	const [predicate, setPredicate] = useState<PredicateDraft>(() =>
		draftFromPredicate(readPredicate(gate?.config)),
	);
	const [url, setUrl] = useState(
		typeof config.url === "string" ? config.url : "",
	);
	const [expectStatus, setExpectStatus] = useState(
		typeof config.expect_status === "number" ? String(config.expect_status) : "",
	);
	// An http gate needs at least one of `expect_status` or a pointer, so the
	// body check is opt-in rather than always-on: a gate with an empty pointer
	// silently sent would be refused by the server for a reason the form could
	// have explained.
	const [checkBody, setCheckBody] = useState(
		gate?.kind === "http" && typeof config.pointer === "string",
	);
	const [headers, setHeaders] = useState(() =>
		config.headers ? JSON.stringify(config.headers, null, 2) : "",
	);
	const [pollSecs, setPollSecs] = useState(
		gate?.poll_interval_secs ? String(gate.poll_interval_secs) : "",
	);
	const [disposition, setDisposition] = useState<DispositionChoice>(
		gate?.disposition ?? "derive",
	);
	const [localError, setLocalError] = useState<string | null>(null);

	// Anything that is not this step. A condition reading its own step's output
	// can never be true — the output does not exist until the step runs, and the
	// step will not run until the condition is true — so the server refuses it
	// and the picker does not offer it.
	const candidates = useMemo(
		() => steps.filter((candidate) => candidate.step_key !== step.step_key),
		[steps, step.step_key],
	);
	const ancestors = useMemo(
		() => ancestorsOf(edges, step.step_key),
		[edges, step.step_key],
	);

	const derived = useMemo(
		() => deriveStepDisposition(kind, sourceStepKey, step.step_key, edges),
		[kind, sourceStepKey, step.step_key, edges],
	);
	const effective = disposition === "derive" ? derived.disposition : disposition;

	const submit = () => {
		const key = gateKey.trim();
		if (key === "") {
			setLocalError(
				"A condition needs a name. It is what makes saving it twice an edit rather than a second condition.",
			);
			return;
		}

		let body: SaveStepGateRequest;
		if (kind === "task_output") {
			if (sourceStepKey.trim() === "") {
				setLocalError("Pick the step whose output this reads.");
				return;
			}
			const built = buildPredicate(predicate);
			if ("error" in built) {
				setLocalError(`Condition: ${built.error}`);
				return;
			}
			body = {
				kind,
				source_step_key: sourceStepKey.trim(),
				config: built.value,
				label: label.trim() || null,
				disposition: disposition === "derive" ? null : disposition,
			};
		} else {
			const trimmedUrl = url.trim();
			if (!/^https?:\/\//.test(trimmedUrl)) {
				setLocalError(
					"An http condition needs an http:// or https:// URL. It is fetched by the server, unattended, on a timer.",
				);
				return;
			}
			const httpConfig: Record<string, unknown> = {url: trimmedUrl};
			if (expectStatus.trim() !== "") {
				const parsed = Number(expectStatus.trim());
				if (!Number.isInteger(parsed) || parsed < 100 || parsed > 599) {
					setLocalError("Expected status must be a whole HTTP status code.");
					return;
				}
				httpConfig.expect_status = parsed;
			}
			if (checkBody) {
				const built = buildPredicate(predicate);
				if ("error" in built) {
					setLocalError(`Response body: ${built.error}`);
					return;
				}
				Object.assign(httpConfig, built.value);
			}
			if (httpConfig.expect_status == null && !checkBody) {
				setLocalError(
					"A condition with nothing to assert is satisfied by any response, which makes it not a condition. Expect a status, check the body, or both.",
				);
				return;
			}
			if (headers.trim() !== "") {
				const parsed = parseJson(headers);
				if ("error" in parsed) {
					setLocalError(`Headers: ${parsed.error}`);
					return;
				}
				if (
					typeof parsed.value !== "object" ||
					parsed.value === null ||
					Array.isArray(parsed.value)
				) {
					setLocalError('Headers must be a JSON object, e.g. {"Accept": "…"}.');
					return;
				}
				httpConfig.headers = parsed.value;
			}
			let poll: number | null = null;
			if (pollSecs.trim() !== "") {
				const parsed = Number(pollSecs.trim());
				if (!Number.isInteger(parsed) || parsed < MIN_POLL_INTERVAL_SECS) {
					setLocalError(
						`Poll interval must be a whole number of seconds, at least ${MIN_POLL_INTERVAL_SECS}. A condition is polled by the server, unattended and repeatedly, so the floor is enforced rather than trusted.`,
					);
					return;
				}
				poll = parsed;
			}
			body = {
				kind,
				config: httpConfig,
				label: label.trim() || null,
				poll_interval_secs: poll,
				disposition: disposition === "derive" ? null : disposition,
			};
		}

		setLocalError(null);
		onSubmit(body, key);
	};

	return (
		<div>
			<Field
				label="Name"
				hint="What this condition is called in the template. Saving the same name twice edits it."
			>
				<input
					value={gateKey}
					onChange={(event) => setGateKey(event.target.value)}
					disabled={gate != null}
					spellCheck={false}
					placeholder="deploy-was-green"
					className="w-full rounded border border-app-line bg-app px-2 py-1 font-mono text-[11px] text-ink outline-none focus:border-accent disabled:text-ink-faint"
				/>
			</Field>

			<Field
				label="Reads"
				hint="Another step's output, or a URL polled until it answers."
			>
				<select
					value={kind}
					onChange={(event) => setKind(event.target.value)}
					className="w-full rounded border border-app-line bg-app px-2 py-1 text-[11px] text-ink outline-none focus:border-accent"
				>
					<option value="task_output">Another step's output</option>
					<option value="http">An HTTP endpoint</option>
				</select>
			</Field>

			{kind === "task_output" ? (
				<>
					<Field label="Whose output">
						<select
							value={sourceStepKey}
							onChange={(event) => setSourceStepKey(event.target.value)}
							className="w-full rounded border border-app-line bg-app px-2 py-1 text-[11px] text-ink outline-none focus:border-accent"
						>
							<option value="">Pick a step…</option>
							{candidates.map((candidate) => (
								<option key={candidate.step_key} value={candidate.step_key}>
									{candidate.step_key}
									{ancestors.has(candidate.step_key) ? "" : " (not upstream)"}
								</option>
							))}
						</select>
					</Field>
					<Field
						label="Runs when"
						hint="Read from that step's output. The same shape a loop's exit condition takes."
					>
						<PredicateFields
							value={predicate}
							onChange={setPredicate}
							pointerPlaceholder="/status"
							pointerTitle="An RFC 6901 pointer into that step's outputs."
						/>
					</Field>
				</>
			) : (
				<>
					<Field label="URL">
						<input
							value={url}
							onChange={(event) => setUrl(event.target.value)}
							spellCheck={false}
							placeholder="https://ci.example.com/status"
							className="w-full rounded border border-app-line bg-app px-2 py-1 font-mono text-[11px] text-ink outline-none focus:border-accent"
						/>
					</Field>
					<Field label="Expected status" hint="Blank to accept any status.">
						<input
							value={expectStatus}
							onChange={(event) => setExpectStatus(event.target.value)}
							inputMode="numeric"
							spellCheck={false}
							placeholder="200"
							className="w-20 rounded border border-app-line bg-app px-2 py-1 font-mono text-[11px] text-ink outline-none focus:border-accent"
						/>
					</Field>
					<div className="mb-2">
						<label className="flex items-center gap-1.5 text-[11px] font-medium text-ink-dull">
							<input
								type="checkbox"
								checked={checkBody}
								onChange={(event) => setCheckBody(event.target.checked)}
							/>
							Also check the response body
						</label>
						{checkBody && (
							<div className="mt-1">
								<PredicateFields
									value={predicate}
									onChange={setPredicate}
									pointerPlaceholder="/state"
									pointerTitle="An RFC 6901 pointer into the JSON response."
								/>
							</div>
						)}
					</div>
					<Field
						label="Headers"
						hint="JSON object, sent with every poll. For an auth token, blank is safer than a secret in a template."
					>
						<textarea
							value={headers}
							onChange={(event) => setHeaders(event.target.value)}
							rows={2}
							spellCheck={false}
							placeholder={'{"Accept": "application/json"}'}
							className="w-full rounded border border-app-line bg-app px-2 py-1.5 font-mono text-[11px] text-ink outline-none focus:border-accent"
						/>
					</Field>
					<Field
						label="Poll every"
						hint={`Seconds between polls. Minimum ${MIN_POLL_INTERVAL_SECS} — this is polled server-side with nobody watching, so a short interval is a way to be mistaken for an attack.`}
					>
						<input
							value={pollSecs}
							onChange={(event) => setPollSecs(event.target.value)}
							inputMode="numeric"
							spellCheck={false}
							placeholder="60"
							className="w-20 rounded border border-app-line bg-app px-2 py-1 font-mono text-[11px] text-ink outline-none focus:border-accent"
						/>
					</Field>
				</>
			)}

			<Field
				label="Label"
				hint='What the board shows. "waiting for CI on main" beats a URL.'
			>
				<input
					value={label}
					onChange={(event) => setLabel(event.target.value)}
					placeholder="the deploy went green"
					className="w-full rounded border border-app-line bg-app px-2 py-1 text-[11px] text-ink outline-none focus:border-accent"
				/>
			</Field>

			<Field label="If it is false" hint="The whole of branching is this field.">
				<select
					value={disposition}
					onChange={(event) =>
						setDisposition(event.target.value as DispositionChoice)
					}
					className="w-full rounded border border-app-line bg-app px-2 py-1 text-[11px] text-ink outline-none focus:border-accent"
				>
					<option value="derive">Derive — work it out from the graph</option>
					<option value="wait">{DISPOSITION_LABEL.wait}</option>
					<option value="route">{DISPOSITION_LABEL.route}</option>
				</select>
			</Field>

			<p
				className={`mb-2 rounded border px-2 py-1 text-[10px] ${
					effective === "route"
						? "border-status-warning/30 bg-status-warning/5 text-ink-dull"
						: "border-status-info/30 bg-status-info/5 text-ink-dull"
				}`}
			>
				{disposition === "derive" ? (
					<>
						{derived.because} →{" "}
						<strong className="font-medium text-ink">
							{effective === "route"
								? "will skip this step if false"
								: "will hold this step until true"}
						</strong>
						.{" "}
						{effective === "route" &&
							"Anything binding this step's output skips too, unless that input is optional."}
					</>
				) : (
					DISPOSITION_HINT[effective as GateDisposition]
				)}
			</p>

			{localError && (
				<p className="mb-2 break-words rounded border border-status-error/30 bg-status-error/5 px-2 py-1 font-mono text-[11px] text-status-error">
					{localError}
				</p>
			)}

			<div className="flex items-center gap-2">
				<Button size="sm" variant="accent" disabled={busy} onClick={submit}>
					{busy ? "Saving…" : gate ? "Save condition" : "Add condition"}
				</Button>
				<Button size="sm" variant="gray" onClick={onCancel}>
					Cancel
				</Button>
			</div>
		</div>
	);
}

/**
 * What this step waits for.
 *
 * Phrased as prerequisites of the selected step rather than as a list of edges,
 * because "publish waits for review" is a fact about publish, and an edge list
 * makes the reader work out which end they are looking at.
 *
 * The picker hides the choices the server is certain to refuse — itself, and
 * anything downstream of it — so the 409 that names the cycle stays a
 * diagnosis of something surprising rather than the routine cost of clicking.
 */
function Dependencies({
	step,
	steps,
	edges,
	parents,
	bodies,
	busy,
	error,
	onAdd,
	onRemove,
}: {
	step: WorkflowStep;
	steps: WorkflowStep[];
	edges: WorkflowEdge[];
	parents: string[];
	bodies: Map<string, LoopBody>;
	busy?: boolean;
	error: string | null;
	onAdd: (parentStepKey: string, kind: "normal" | "on_exhausted") => void;
	onRemove: (parentStepKey: string) => void;
}) {
	const [choice, setChoice] = useState("");
	const [arm, setArm] = useState<"normal" | "on_exhausted">("normal");

	/** Exit step key → the loop it ends, so a prerequisite can be armed. */
	const exits = useMemo(() => {
		const map = new Map<string, LoopBody>();
		for (const body of bodies.values()) {
			if (body.exit) map.set(body.exit.step_key, body);
		}
		return map;
	}, [bodies]);

	const candidates = useMemo(
		() =>
			steps
				.filter(
					(candidate) =>
						candidate.step_key !== step.step_key &&
						!parents.includes(candidate.step_key) &&
						!wouldCycle(edges, candidate.step_key, step.step_key),
				)
				.map((candidate) => candidate.step_key),
		[steps, step.step_key, parents, edges],
	);

	/** parent key → the kind of the edge that already exists. */
	const kindOf = useMemo(() => {
		const map = new Map<string, string>();
		for (const edge of edges) {
			if (edge.child_step_key === step.step_key) {
				map.set(edge.parent_step_key, edge.kind);
			}
		}
		return map;
	}, [edges, step.step_key]);

	const chosenLoop = choice === "" ? null : (exits.get(choice) ?? null);

	return (
		<Section
			title="Waits for"
			hint="This step will not start until every step listed here is done."
		>
			{parents.length === 0 ? (
				<p className="mb-2 text-xs text-ink-faint">
					Nothing — it starts as soon as the run does.
				</p>
			) : (
				<div className="mb-2 flex flex-wrap gap-1.5">
					{parents.map((parent) => {
						const exhausted = kindOf.get(parent) === "on_exhausted";
						const loop = exits.get(parent);
						return (
							<span
								key={parent}
								className={`group inline-flex items-center gap-1.5 rounded border px-1.5 py-0.5 font-mono text-[11px] ${
									exhausted
										? "border-dashed border-status-warning/60 bg-status-warning/10 text-status-warning"
										: loop
											? "border-status-success/50 bg-status-success/10 text-status-success"
											: "border-app-line bg-app-box/50 text-ink-dull"
								}`}
								title={
									exhausted
										? `Runs only if loop \`${loop?.group ?? "?"}\` runs out of passes. If it converges, this step never runs.`
										: loop
											? `Runs only if loop \`${loop.group}\` converges. If it runs out of passes, this step never runs.`
											: undefined
								}
							>
								{parent}
								{(exhausted || loop) && (
									<span className="not-italic">
										· {exhausted ? "gave up" : "converged"}
									</span>
								)}
								<button
									type="button"
									onClick={() => onRemove(parent)}
									disabled={busy}
									title={`Stop waiting for \`${parent}\``}
									className="opacity-70 hover:text-status-error hover:opacity-100 disabled:opacity-50"
								>
									<FontAwesomeIcon icon={faXmark} className="text-[9px]" />
								</button>
							</span>
						);
					})}
				</div>
			)}

			{candidates.length > 0 && (
				<>
					<div className="flex items-center gap-2">
						<select
							value={choice}
							onChange={(event) => {
								setChoice(event.target.value);
								setArm("normal");
							}}
							className="min-w-0 flex-1 rounded border border-app-line bg-app px-2 py-1 font-mono text-[11px] text-ink outline-none focus:border-accent"
						>
							<option value="">Add a prerequisite…</option>
							{candidates.map((key) => (
								<option key={key} value={key}>
									{key}
									{exits.has(key) ? " (loop exit)" : ""}
								</option>
							))}
						</select>
						<Button
							size="sm"
							variant="gray"
							title="Add prerequisite"
							disabled={busy || choice === ""}
							onClick={() => {
								if (choice === "") return;
								onAdd(choice, arm);
								setChoice("");
								setArm("normal");
							}}
						>
							{busy ? "Saving…" : "Add"}
						</Button>
					</div>

					{/* Two ways out of a loop, asked for as two, because a step wired
					    downstream of a loop the ordinary way runs after three failed
					    passes exactly as it does after three successful ones. */}
					{chosenLoop && (
						<div className="mt-1.5 rounded border border-app-line bg-app-box/30 p-2">
							<p className="mb-1.5 text-[10px] text-ink-faint">
								<span className="font-mono text-ink-dull">{choice}</span> ends
								loop <span className="font-mono">{chosenLoop.group}</span>, so
								this edge has to say which way out it is on.
							</p>
							<div className="flex overflow-hidden rounded border border-app-line text-[10px]">
								<button
									type="button"
									onClick={() => setArm("normal")}
									className={`flex-1 px-2 py-1 ${
										arm === "normal"
											? "bg-status-success/20 text-status-success"
											: "text-ink-faint hover:text-ink-dull"
									}`}
								>
									When it converges
								</button>
								<button
									type="button"
									onClick={() => setArm("on_exhausted")}
									className={`flex-1 px-2 py-1 ${
										arm === "on_exhausted"
											? "bg-status-warning/20 text-status-warning"
											: "text-ink-faint hover:text-ink-dull"
									}`}
								>
									When it gives up
								</button>
							</div>
						</div>
					)}
				</>
			)}

			{error && (
				<p className="mt-1.5 break-words rounded border border-status-error/30 bg-status-error/5 px-2 py-1 font-mono text-[11px] text-status-error">
					{error}
				</p>
			)}
		</Section>
	);
}

/**
 * Where each of this step's inputs comes from.
 *
 * Three sources, and the difference between them matters: another step's output
 * at a JSON Pointer, a pointer into the payload the whole run was launched
 * with, or a constant. The row shows the source next to the key for the same
 * reason the task drawer does — `draft → /headline` tells you which step to go
 * look at when the value is wrong.
 */
function Bindings({
	step,
	steps,
	parents,
	ancestors,
	body,
	bindings,
	hasRunInput,
	busy,
	error,
	onSet,
	onRemove,
}: {
	step: WorkflowStep;
	steps: WorkflowStep[];
	parents: string[];
	/** Every step upstream at any depth — see `ancestorsOf`. */
	ancestors: Set<string>;
	/** The loop body this step is in, if any. `previous_iteration` needs it. */
	body: LoopBody | null;
	bindings: StepBinding[];
	hasRunInput: boolean;
	busy?: boolean;
	error: string | null;
	onSet: (inputKey: string, body: SaveBindingRequest) => void;
	onRemove: (inputKey: string) => void;
}) {
	const [editing, setEditing] = useState<string | null>(null);

	// Keys the step's own schema says it needs but nothing supplies. The server
	// only complains at launch; naming them here is the difference between
	// finding out now and finding out from a 422 three clicks later.
	const unbound = useMemo(() => {
		const declared = declaredInputKeys(step.input_schema);
		const bound = new Set(bindings.map((b) => b.input_key));
		return declared.filter((key) => !bound.has(key));
	}, [step.input_schema, bindings]);

	return (
		<Section
			title="Inputs"
			hint="Each key the step reads, and where its value comes from."
		>
			{bindings.length === 0 && editing === null && (
				<p className="mb-2 text-xs text-ink-faint">Nothing bound yet.</p>
			)}

			<div className="space-y-1">
				{bindings.map((binding) =>
					editing === binding.input_key ? (
						<BindingForm
							key={binding.input_key}
							binding={binding}
							stepKey={step.step_key}
							steps={steps}
							parents={parents}
							ancestors={ancestors}
							body={body}
							hasRunInput={hasRunInput}
							takenKeys={bindings.map((b) => b.input_key)}
							busy={busy}
							onCancel={() => setEditing(null)}
							onSave={(inputKey, body) => {
								onSet(inputKey, body);
								setEditing(null);
							}}
						/>
					) : (
						<BindingRow
							key={binding.input_key}
							binding={binding}
							// A binding whose source step is not upstream at all reads a
							// value that may not exist yet. The server compiles it happily;
							// the run is what fails. Transitively upstream counts — see
							// `ancestorsOf`.
							warnUnordered={
								binding.source === "step" &&
								binding.source_step_key != null &&
								!ancestors.has(binding.source_step_key)
							}
							busy={busy}
							onEdit={() => setEditing(binding.input_key)}
							onRemove={() => onRemove(binding.input_key)}
						/>
					),
				)}

				{editing === "" && (
					<BindingForm
						stepKey={step.step_key}
						steps={steps}
						parents={parents}
						ancestors={ancestors}
						body={body}
						hasRunInput={hasRunInput}
						takenKeys={bindings.map((b) => b.input_key)}
						suggestions={unbound}
						busy={busy}
						onCancel={() => setEditing(null)}
						onSave={(inputKey, body) => {
							onSet(inputKey, body);
							setEditing(null);
						}}
					/>
				)}
			</div>

			{unbound.length > 0 && editing !== "" && (
				<p className="mt-2 rounded border border-status-warning/30 bg-status-warning/5 px-2 py-1 text-[11px] text-status-warning">
					Declared by this step's input schema but not bound:{" "}
					<span className="font-mono">{unbound.join(", ")}</span>
				</p>
			)}

			{error && (
				<p className="mt-1.5 break-words rounded border border-status-error/30 bg-status-error/5 px-2 py-1 font-mono text-[11px] text-status-error">
					{error}
				</p>
			)}

			{editing !== "" && (
				<button
					type="button"
					onClick={() => setEditing("")}
					className="mt-1.5 text-[11px] text-ink-faint hover:text-ink-dull hover:underline"
				>
					Add an input…
				</button>
			)}
		</Section>
	);
}

function BindingRow({
	binding,
	warnUnordered,
	busy,
	onEdit,
	onRemove,
}: {
	binding: StepBinding;
	warnUnordered: boolean;
	busy?: boolean;
	onEdit: () => void;
	onRemove: () => void;
}) {
	return (
		<div className="group flex items-baseline gap-2 text-xs">
			<span
				className="w-24 shrink-0 truncate font-mono text-ink-dull"
				title={binding.input_key}
			>
				{binding.input_key}
			</span>

			<span className="flex min-w-0 flex-1 items-center gap-1 text-[10px] text-ink-faint">
				{binding.source === "literal" ? (
					<>
						<FontAwesomeIcon icon={faQuoteLeft} className="text-[8px]" />
						<span className="min-w-0 truncate font-mono text-ink">
							{renderLiteral(binding.literal_value)}
						</span>
					</>
				) : (
					<>
						{binding.source === "previous_iteration" && (
							<FontAwesomeIcon
								icon={faRotate}
								className="shrink-0 text-[8px] text-accent"
								title="Reads the previous pass of this loop body, not this one."
							/>
						)}
						<span
							className={`shrink-0 font-mono ${
								warnUnordered
									? "text-status-warning"
									: binding.source === "previous_iteration"
										? "text-accent"
										: "text-ink-dull"
							}`}
							title={
								warnUnordered
									? "This step does not wait for that one, so the value may not exist yet."
									: binding.source === "previous_iteration"
										? `The previous pass's \`${binding.source_step_key ?? "?"}\`. On pass 1 it falls back to the step the loop was entered from.`
										: undefined
							}
						>
							{binding.source === "run_input"
								? "run input"
								: (binding.source_step_key ?? "?")}
							{binding.source === "previous_iteration" ? " (last pass)" : ""}
						</span>
						<FontAwesomeIcon
							icon={faArrowRightLong}
							className="shrink-0 text-[8px]"
						/>
						<span className="min-w-0 truncate font-mono text-ink">
							{binding.source_pointer || "/"}
						</span>
					</>
				)}
			</span>

			<span className="flex shrink-0 items-center gap-1 opacity-0 transition-opacity focus-within:opacity-100 group-hover:opacity-100">
				<button
					type="button"
					onClick={onEdit}
					disabled={busy}
					title={`Rewire \`${binding.input_key}\``}
					className="text-ink-faint hover:text-ink-dull disabled:opacity-50"
				>
					<FontAwesomeIcon icon={faPen} className="text-[9px]" />
				</button>
				<button
					type="button"
					onClick={onRemove}
					disabled={busy}
					title={`Unbind \`${binding.input_key}\``}
					className="text-ink-faint hover:text-status-error disabled:opacity-50"
				>
					<FontAwesomeIcon icon={faXmark} className="text-[9px]" />
				</button>
			</span>
		</div>
	);
}

/**
 * Wire one input.
 *
 * The key is the binding's identity — `PUT .../bindings/{key}` replaces
 * whatever was on that key — so editing fixes the key and only rewires the
 * source. Renaming in place would leave the old binding sitting beside the new
 * one, both feeding the same step.
 */
function BindingForm({
	binding,
	stepKey,
	steps,
	parents,
	ancestors,
	body,
	hasRunInput,
	takenKeys,
	suggestions = [],
	busy,
	onSave,
	onCancel,
}: {
	binding?: StepBinding;
	stepKey: string;
	steps: WorkflowStep[];
	parents: string[];
	ancestors: Set<string>;
	body: LoopBody | null;
	hasRunInput: boolean;
	takenKeys: string[];
	suggestions?: string[];
	busy?: boolean;
	onSave: (inputKey: string, body: SaveBindingRequest) => void;
	onCancel: () => void;
}) {
	const [key, setKey] = useState(binding?.input_key ?? suggestions[0] ?? "");
	// A step with prerequisites is almost always being fed by one of them —
	// that is what waiting for it was for. Only a step with nothing upstream
	// defaults to the run's launch payload.
	const [source, setSource] = useState<StepBinding["source"]>(
		binding?.source ??
			(parents.length > 0 ? "step" : hasRunInput ? "run_input" : "step"),
	);
	const [sourceStep, setSourceStep] = useState(
		binding?.source_step_key ?? parents[0] ?? "",
	);
	const [pointer, setPointer] = useState(binding?.source_pointer ?? "");
	const [literal, setLiteral] = useState(() =>
		binding?.literal_value == null
			? ""
			: JSON.stringify(binding.literal_value, null, 2),
	);
	const [error, setError] = useState<string | null>(null);

	const otherSteps = steps.filter((s) => s.step_key !== stepKey);
	// A previous-iteration binding names a step in the *same body*: it reads what
	// that step produced last time round. Naming one outside the body would be
	// reading a pass that step never had, and launch refuses it.
	const bodySteps = body?.members ?? [];
	const [previousStep, setPreviousStep] = useState(
		binding?.source === "previous_iteration"
			? (binding.source_step_key ?? "")
			: (body?.exit?.step_key ?? ""),
	);

	const submit = () => {
		const inputKey = key.trim();
		if (inputKey === "") {
			setError("An input needs a key — the name the step reads it by.");
			return;
		}
		if (!binding && takenKeys.includes(inputKey)) {
			setError(`\`${inputKey}\` is already bound. Edit that row instead.`);
			return;
		}

		if (source === "step") {
			if (sourceStep === "") {
				setError("Name the step this value comes from.");
				return;
			}
			onSave(inputKey, {
				source: "step",
				source_step_key: sourceStep,
				source_pointer: pointer.trim() || null,
			});
			return;
		}

		if (source === "previous_iteration") {
			if (previousStep === "") {
				setError("Name the step in this body whose last pass this reads.");
				return;
			}
			onSave(inputKey, {
				source: "previous_iteration",
				source_step_key: previousStep,
				source_pointer: pointer.trim() || null,
			});
			return;
		}

		if (source === "run_input") {
			onSave(inputKey, {
				source: "run_input",
				source_pointer: pointer.trim() || null,
			});
			return;
		}

		const parsed = parseJson(literal);
		if ("error" in parsed) {
			setError(`Literal: ${parsed.error}`);
			return;
		}
		if (parsed.value == null) {
			setError("A literal needs a value. A bare string still needs its quotes.");
			return;
		}
		onSave(inputKey, {source: "literal", literal_value: parsed.value});
	};

	return (
		<div className="rounded border border-app-line bg-app-box/30 p-2">
			<div className="mb-2 flex flex-wrap items-center gap-2">
				<input
					value={key}
					onChange={(event) => setKey(event.target.value)}
					readOnly={binding != null}
					spellCheck={false}
					placeholder="input key"
					list={suggestions.length > 0 ? `keys-${stepKey}` : undefined}
					title={
						binding
							? "The key identifies the binding. Remove and re-add to rename."
							: undefined
					}
					className={`w-28 rounded border border-app-line bg-app px-2 py-1 font-mono text-[11px] text-ink outline-none focus:border-accent ${
						binding ? "cursor-not-allowed text-ink-dull" : ""
					}`}
				/>
				{suggestions.length > 0 && (
					<datalist id={`keys-${stepKey}`}>
						{suggestions.map((suggestion) => (
							<option key={suggestion} value={suggestion} />
						))}
					</datalist>
				)}
				<SourceToggle
					value={source}
					onChange={setSource}
					stepDisabled={otherSteps.length === 0}
					previousDisabled={bodySteps.length === 0}
				/>
			</div>

			{source === "step" && (
				<div className="mb-2 flex items-center gap-1.5">
					<select
						value={sourceStep}
						onChange={(event) => setSourceStep(event.target.value)}
						className="w-32 shrink-0 rounded border border-app-line bg-app px-1.5 py-1 font-mono text-[11px] text-ink outline-none focus:border-accent"
					>
						<option value="">step…</option>
						{otherSteps.map((candidate) => (
							<option key={candidate.step_key} value={candidate.step_key}>
								{candidate.step_key}
								{ancestors.has(candidate.step_key) ? "" : " (not upstream)"}
							</option>
						))}
					</select>
					<FontAwesomeIcon
						icon={faArrowRightLong}
						className="shrink-0 text-[8px] text-ink-faint"
					/>
					<input
						value={pointer}
						onChange={(event) => setPointer(event.target.value)}
						spellCheck={false}
						placeholder="/headline"
						className="min-w-0 flex-1 rounded border border-app-line bg-app px-2 py-1 font-mono text-[11px] text-ink outline-none focus:border-accent"
					/>
				</div>
			)}

			{source === "previous_iteration" && (
				<div className="mb-2 flex items-center gap-1.5">
					<select
						value={previousStep}
						onChange={(event) => setPreviousStep(event.target.value)}
						className="w-32 shrink-0 rounded border border-app-line bg-app px-1.5 py-1 font-mono text-[11px] text-ink outline-none focus:border-accent"
					>
						<option value="">step…</option>
						{bodySteps.map((candidate) => (
							<option key={candidate.step_key} value={candidate.step_key}>
								{candidate.step_key}
							</option>
						))}
					</select>
					<FontAwesomeIcon
						icon={faArrowRightLong}
						className="shrink-0 text-[8px] text-ink-faint"
					/>
					<input
						value={pointer}
						onChange={(event) => setPointer(event.target.value)}
						spellCheck={false}
						placeholder="/attempt"
						className="min-w-0 flex-1 rounded border border-app-line bg-app px-2 py-1 font-mono text-[11px] text-ink outline-none focus:border-accent"
					/>
				</div>
			)}

			{source === "run_input" && (
				<input
					value={pointer}
					onChange={(event) => setPointer(event.target.value)}
					spellCheck={false}
					placeholder="/version"
					className="mb-2 w-full rounded border border-app-line bg-app px-2 py-1 font-mono text-[11px] text-ink outline-none focus:border-accent"
				/>
			)}

			{source === "literal" && (
				<textarea
					value={literal}
					onChange={(event) => setLiteral(event.target.value)}
					spellCheck={false}
					rows={3}
					placeholder={'"v1.4.2"'}
					className="mb-2 w-full rounded border border-app-line bg-app px-2 py-1.5 font-mono text-[11px] text-ink outline-none focus:border-accent"
				/>
			)}

			<p className="mb-2 text-[10px] text-ink-faint">
				{source === "step"
					? "An RFC 6901 pointer into that step's outputs. Blank reads the whole object."
					: source === "previous_iteration"
						? `What that step produced on the previous pass. On pass 1 there is no previous pass, so it reads ${
								body?.entries.length === 1
									? `\`${body.entries[0]}\` — the step this loop is entered from`
									: "the step the loop is entered from"
							} instead, automatically.`
						: source === "run_input"
							? "An RFC 6901 pointer into the payload the run was launched with."
							: "JSON. A bare string still needs its quotes."}
			</p>

			{error && <p className="mb-2 text-[11px] text-status-error">{error}</p>}

			<div className="flex gap-2">
				<Button size="sm" variant="accent" disabled={busy} onClick={submit}>
					{busy ? "Saving…" : binding ? "Rewire" : "Add input"}
				</Button>
				<Button size="sm" variant="gray" onClick={onCancel}>
					Cancel
				</Button>
			</div>
		</div>
	);
}

function SourceToggle({
	value,
	onChange,
	stepDisabled,
	previousDisabled,
}: {
	value: StepBinding["source"];
	onChange: (next: StepBinding["source"]) => void;
	stepDisabled: boolean;
	previousDisabled: boolean;
}) {
	const options: {
		label: string;
		value: StepBinding["source"];
		off?: boolean;
		offHint?: string;
		hint?: string;
	}[] = [
		{label: "From a step", value: "step", off: stepDisabled},
		{
			label: "Last pass",
			value: "previous_iteration",
			off: previousDisabled,
			offHint:
				"Only a step inside a loop body has a previous pass to read. Give this step a loop body first.",
			hint: "What a step in this loop body produced the last time round. This is how a body improves on itself instead of starting over.",
		},
		{label: "Run input", value: "run_input"},
		{label: "Literal", value: "literal"},
	];

	return (
		<div className="flex overflow-hidden rounded border border-app-line text-[10px]">
			{options.map((option) => (
				<button
					key={option.value}
					type="button"
					disabled={option.off}
					title={
						option.off
							? (option.offHint ??
								"There is no other step to read from yet.")
							: option.hint
					}
					onClick={() => onChange(option.value)}
					className={`px-2 py-1 disabled:opacity-40 ${
						value === option.value
							? "bg-accent text-white"
							: "text-ink-faint hover:text-ink-dull"
					}`}
				>
					{option.label}
				</button>
			))}
		</div>
	);
}

function format(schema: unknown): string {
	return schema == null ? "" : JSON.stringify(schema, null, 2);
}

function renderLiteral(value: unknown): string {
	if (value === undefined || value === null) return "—";
	return typeof value === "string" ? value : JSON.stringify(value);
}

/** Property names from a step's input schema, if it is one we can read. */
function declaredInputKeys(schema: unknown): string[] {
	if (typeof schema !== "object" || schema === null) return [];
	const properties = (schema as {properties?: unknown}).properties;
	if (typeof properties !== "object" || properties === null) return [];
	return Object.keys(properties as Record<string, unknown>);
}
