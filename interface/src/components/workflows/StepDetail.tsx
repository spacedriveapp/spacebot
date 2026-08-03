import {useEffect, useMemo, useState} from "react";
import {Button} from "@spacedrive/primitives";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {
	faArrowRightLong,
	faPen,
	faQuoteLeft,
	faXmark,
} from "@fortawesome/free-solid-svg-icons";
import type {
	SaveBindingRequest,
	SaveStepRequest,
	StepBinding,
	TaskPriority,
	WorkflowEdge,
	WorkflowStep,
} from "@/api/client";
import {ancestorsOf, parentsByStep, wouldCycle} from "./graph";
import {parseJson} from "./schemaForm";

const PRIORITIES: TaskPriority[] = ["critical", "high", "medium", "low"];

export interface StepDetailProps {
	step: WorkflowStep;
	steps: WorkflowStep[];
	edges: WorkflowEdge[];
	bindings: StepBinding[];
	/** Whether the template declares a launch input at all. */
	hasRunInput: boolean;
	agents: {id: string; display_name?: string | null}[];
	onSave: (stepKey: string, body: SaveStepRequest) => void;
	onDelete: (stepKey: string) => void;
	onAddEdge: (parentStepKey: string, childStepKey: string) => void;
	onRemoveEdge: (parentStepKey: string, childStepKey: string) => void;
	onSetBinding: (
		stepKey: string,
		inputKey: string,
		body: SaveBindingRequest,
	) => void;
	onRemoveBinding: (stepKey: string, inputKey: string) => void;
	stepBusy?: boolean;
	stepError?: string | null;
	edgeBusy?: boolean;
	edgeError?: string | null;
	bindingBusy?: boolean;
	bindingError?: string | null;
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
	// Anything upstream at any depth, which is what "will have finished" means.
	const ancestors = useMemo(
		() => ancestorsOf(edges, step.step_key),
		[edges, step.step_key],
	);

	return (
		<div className="flex h-full min-h-0 flex-col overflow-y-auto">
			<StepFields
				key={step.step_key}
				step={step}
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
				busy={props.edgeBusy}
				error={props.edgeError ?? null}
				onAdd={(parentKey) => props.onAddEdge(parentKey, step.step_key)}
				onRemove={(parentKey) => props.onRemoveEdge(parentKey, step.step_key)}
			/>
			<Bindings
				step={step}
				steps={steps}
				parents={parents}
				ancestors={ancestors}
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
	agents,
	busy,
	error,
	onSave,
	onDelete,
}: {
	step: WorkflowStep;
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
	}, [step]);

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
	busy,
	error,
	onAdd,
	onRemove,
}: {
	step: WorkflowStep;
	steps: WorkflowStep[];
	edges: WorkflowEdge[];
	parents: string[];
	busy?: boolean;
	error: string | null;
	onAdd: (parentStepKey: string) => void;
	onRemove: (parentStepKey: string) => void;
}) {
	const [choice, setChoice] = useState("");

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
					{parents.map((parent) => (
						<span
							key={parent}
							className="group inline-flex items-center gap-1.5 rounded border border-app-line bg-app-box/50 px-1.5 py-0.5 font-mono text-[11px] text-ink-dull"
						>
							{parent}
							<button
								type="button"
								onClick={() => onRemove(parent)}
								disabled={busy}
								title={`Stop waiting for \`${parent}\``}
								className="text-ink-faint hover:text-status-error disabled:opacity-50"
							>
								<FontAwesomeIcon icon={faXmark} className="text-[9px]" />
							</button>
						</span>
					))}
				</div>
			)}

			{candidates.length > 0 && (
				<div className="flex items-center gap-2">
					<select
						value={choice}
						onChange={(event) => setChoice(event.target.value)}
						className="min-w-0 flex-1 rounded border border-app-line bg-app px-2 py-1 font-mono text-[11px] text-ink outline-none focus:border-accent"
					>
						<option value="">Add a prerequisite…</option>
						{candidates.map((key) => (
							<option key={key} value={key}>
								{key}
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
							onAdd(choice);
							setChoice("");
						}}
					>
						{busy ? "Saving…" : "Add"}
					</Button>
				</div>
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
						<span
							className={`shrink-0 font-mono ${
								warnUnordered ? "text-status-warning" : "text-ink-dull"
							}`}
							title={
								warnUnordered
									? "This step does not wait for that one, so the value may not exist yet."
									: undefined
							}
						>
							{binding.source === "run_input"
								? "run input"
								: (binding.source_step_key ?? "?")}
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
}: {
	value: StepBinding["source"];
	onChange: (next: StepBinding["source"]) => void;
	stepDisabled: boolean;
}) {
	const options: {label: string; value: StepBinding["source"]; off?: boolean}[] =
		[
			{label: "From a step", value: "step", off: stepDisabled},
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
						option.off ? "There is no other step to read from yet." : undefined
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
