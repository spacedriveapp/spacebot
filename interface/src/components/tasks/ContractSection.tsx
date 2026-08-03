import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button } from "@spacedrive/primitives";
import { FontAwesomeIcon } from "@fortawesome/react-fontawesome";
import {
	faCircleExclamation,
	faQuoteLeft,
	faRightLong,
} from "@fortawesome/free-solid-svg-icons";
import {
	api,
	type ContractProblem,
	type TaskContractResponse,
	type TaskInputBinding,
} from "@/api/client";

export interface ContractSectionProps {
	taskNumber: number;
	onSelectTask?: (taskNumber: number) => void;
}

export function ContractSection({ taskNumber, onSelectTask }: ContractSectionProps) {
	const queryClient = useQueryClient();
	const { data } = useQuery({
		queryKey: ["task-contract", taskNumber],
		queryFn: () => api.getTaskContract(taskNumber),
	});

	const save = useMutation({
		mutationFn: (body: {input_schema?: unknown; output_schema?: unknown}) =>
			api.setTaskContract(taskNumber, body),
		onSuccess: () =>
			void queryClient.invalidateQueries({queryKey: ["task-contract", taskNumber]}),
	});

	if (!data) return null;
	return (
		<ContractSectionView
			data={data}
			onSelectTask={onSelectTask}
			onSaveSchemas={(body) => save.mutate(body)}
			saving={save.isPending}
			saveError={save.error instanceof Error ? save.error.message : null}
		/>
	);
}

/** Split from the fetching wrapper so it renders against fixtures. */
export function ContractSectionView({
	data,
	onSelectTask,
	onSaveSchemas,
	saving,
	saveError,
}: {
	data: TaskContractResponse;
	onSelectTask?: (taskNumber: number) => void;
	/** Omitted in read-only contexts such as the fixture harness. */
	onSaveSchemas?: (body: {input_schema?: unknown; output_schema?: unknown}) => void;
	saving?: boolean;
	saveError?: string | null;
}) {
	const hasContract =
		data.input_schema != null ||
		data.output_schema != null ||
		data.bindings.length > 0 ||
		data.outputs != null;

	// Most tasks declare nothing, and an empty "Contract" heading on every one
	// of them would be noise. But a task with no contract is exactly the one
	// somebody needs to give a contract to, so the editor still gets a way in.
	if (!hasContract && !onSaveSchemas) return null;

	// Which keys the graph currently cannot supply, so each row can say so
	// rather than making the reader match a list of problems to a list of rows.
	const failedKeys = new Set(
		data.problems
			.map((problem) => ("input_key" in problem ? problem.input_key : null))
			.filter((key): key is string => key !== null),
	);

	const resolved = (data.resolved_inputs ?? data.inputs ?? {}) as Record<
		string,
		unknown
	>;

	return (
		<div className="border-t border-app-line/40 px-4 py-3">
			<h3 className="mb-2 text-xs font-medium uppercase tracking-wide text-ink-dull">
				Contract
			</h3>

			{/* Problems first and unmissable: a graph that cannot supply a task's
			    inputs is the single most common way a hand-built pipeline is
			    wrong, and it is silent everywhere else. */}
			{data.problems.length > 0 && (
				<ul className="mb-3 space-y-1 rounded border border-status-error/30 bg-status-error/5 px-2 py-1.5">
					{data.problems.map((problem) => (
						<li
							key={problemKey(problem)}
							className="flex gap-1.5 text-xs text-status-error"
						>
							<FontAwesomeIcon
								icon={faCircleExclamation}
								className="mt-0.5 shrink-0 text-[10px]"
							/>
							<span className="break-words">{describe(problem)}</span>
						</li>
					))}
				</ul>
			)}

			{data.bindings.length > 0 && (
				<div className="mb-3">
					<h4 className="mb-1 text-[11px] font-medium text-ink-faint">Inputs</h4>
					<div className="space-y-1">
						{data.bindings.map((binding) => (
							<BindingRow
								key={binding.input_key}
								binding={binding}
								value={resolved[binding.input_key]}
								failed={failedKeys.has(binding.input_key)}
								onSelectTask={onSelectTask}
							/>
						))}
					</div>
				</div>
			)}

			{data.outputs != null ? (
				<JsonBlock label="Outputs" value={data.outputs} />
			) : (
				data.output_schema != null && (
					<div>
						<h4 className="mb-1 text-[11px] font-medium text-ink-faint">
							Outputs
						</h4>
						<p className="text-xs text-ink-faint">
							Not produced yet. Must match the declared schema.
						</p>
					</div>
				)
			)}

			{data.output_schema != null && (
				<JsonBlock label="Required output shape" value={data.output_schema} muted />
			)}

			{onSaveSchemas && (
				<SchemaEditor
					inputSchema={data.input_schema}
					outputSchema={data.output_schema}
					onSave={onSaveSchemas}
					saving={saving}
					saveError={saveError}
				/>
			)}
		</div>
	);
}

/**
 * Declare the shape a task must produce.
 *
 * Humans define the contract; only a worker writes values into it. Setting an
 * output schema here is what turns `task_complete` from "record whatever the
 * model said" into a checked submission that is rejected when it does not fit.
 *
 * The JSON is validated locally before being sent, because the server stores a
 * schema it cannot compile and only surfaces the problem later, at the moment
 * a task tries to run.
 */
function SchemaEditor({
	inputSchema,
	outputSchema,
	onSave,
	saving,
	saveError,
}: {
	inputSchema: unknown;
	outputSchema: unknown;
	onSave: (body: {input_schema?: unknown; output_schema?: unknown}) => void;
	saving?: boolean;
	saveError?: string | null;
}) {
	const [open, setOpen] = useState(false);
	const [inputText, setInputText] = useState(() => format(inputSchema));
	const [outputText, setOutputText] = useState(() => format(outputSchema));
	const [localError, setLocalError] = useState<string | null>(null);

	if (!open) {
		return (
			<button
				type="button"
				onClick={() => {
					setInputText(format(inputSchema));
					setOutputText(format(outputSchema));
					setLocalError(null);
					setOpen(true);
				}}
				className="mt-2 text-[11px] text-ink-faint hover:text-ink-dull hover:underline"
			>
				{outputSchema == null && inputSchema == null
					? "Define a contract…"
					: "Edit contract…"}
			</button>
		);
	}

	const submit = () => {
		const input = parse(inputText);
		const output = parse(outputText);
		if (input.error || output.error) {
			setLocalError(
				input.error
					? `Input schema: ${input.error}`
					: `Output schema: ${output.error}`,
			);
			return;
		}
		setLocalError(null);
		onSave({input_schema: input.value, output_schema: output.value});
		setOpen(false);
	};

	return (
		<div className="mt-3 rounded border border-app-line bg-app-box/30 p-2">
			<SchemaField
				label="Input schema"
				hint="What this task needs before it can run. Leave blank for none."
				value={inputText}
				onChange={setInputText}
			/>
			<SchemaField
				label="Output schema"
				hint="What it must produce. Enforced when the worker calls task_complete."
				value={outputText}
				onChange={setOutputText}
			/>

			{(localError || saveError) && (
				<p className="mb-2 text-[11px] text-status-error">
					{localError ?? saveError}
				</p>
			)}

			<div className="flex gap-2">
				<Button size="sm" variant="accent" disabled={saving} onClick={submit}>
					{saving ? "Saving…" : "Save contract"}
				</Button>
				<Button size="sm" variant="gray" onClick={() => setOpen(false)}>
					Cancel
				</Button>
			</div>
		</div>
	);
}

function SchemaField({
	label,
	hint,
	value,
	onChange,
}: {
	label: string;
	hint: string;
	value: string;
	onChange: (next: string) => void;
}) {
	return (
		<div className="mb-2">
			<label className="mb-0.5 block text-[11px] font-medium text-ink-dull">
				{label}
			</label>
			<p className="mb-1 text-[10px] text-ink-faint">{hint}</p>
			<textarea
				value={value}
				onChange={(event) => onChange(event.target.value)}
				spellCheck={false}
				rows={6}
				className="w-full rounded border border-app-line bg-app px-2 py-1.5 font-mono text-[11px] text-ink outline-none focus:border-accent"
				placeholder={'{\n  "type": "object",\n  "required": ["result"]\n}'}
			/>
		</div>
	);
}

function format(schema: unknown): string {
	return schema == null ? "" : JSON.stringify(schema, null, 2);
}

/** Blank clears the schema; anything else must parse. */
function parse(text: string): {value: unknown; error?: string} {
	if (text.trim() === "") return {value: null};
	try {
		return {value: JSON.parse(text)};
	} catch (error) {
		return {
			value: null,
			error: error instanceof Error ? error.message : "invalid JSON",
		};
	}
}

/**
 * One input, and where its value comes from.
 *
 * Showing the source next to the value is what makes a pipeline debuggable —
 * `#42 → /image/tag` says which upstream task to go look at when the value is
 * wrong, which no amount of staring at the value itself will tell you.
 */
function BindingRow({
	binding,
	value,
	failed,
	onSelectTask,
}: {
	binding: TaskInputBinding;
	value: unknown;
	failed: boolean;
	onSelectTask?: (taskNumber: number) => void;
}) {
	const isLiteral = binding.source_task_number == null;

	return (
		<div className="flex items-baseline gap-2 text-xs">
			<span
				className={`w-28 shrink-0 truncate font-mono ${
					failed ? "text-status-error" : "text-ink-dull"
				}`}
				title={binding.input_key}
			>
				{binding.input_key}
			</span>

			<span className="flex shrink-0 items-center gap-1 text-[10px] text-ink-faint">
				{isLiteral ? (
					<>
						<FontAwesomeIcon icon={faQuoteLeft} className="text-[8px]" />
						literal
					</>
				) : (
					<>
						{onSelectTask ? (
							<button
								type="button"
								onClick={() => onSelectTask(binding.source_task_number!)}
								className="font-mono hover:underline"
							>
								#{binding.source_task_number}
							</button>
						) : (
							<span className="font-mono">#{binding.source_task_number}</span>
						)}
						<FontAwesomeIcon icon={faRightLong} className="text-[8px]" />
						<span className="font-mono">{binding.source_pointer || "/"}</span>
					</>
				)}
			</span>

			<span
				className={`min-w-0 flex-1 truncate font-mono ${
					failed ? "text-status-error" : "text-ink"
				}`}
				title={failed ? "unresolved" : render(value)}
			>
				{failed ? "unresolved" : render(value)}
			</span>
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
			<h4 className="mb-1 text-[11px] font-medium text-ink-faint">{label}</h4>
			<pre
				className={`overflow-x-auto rounded border border-app-line bg-app-box/40 px-2 py-1.5 font-mono text-[11px] leading-relaxed ${
					muted ? "text-ink-faint" : "text-ink-dull"
				}`}
			>
				{JSON.stringify(value, null, 2)}
			</pre>
		</div>
	);
}

function render(value: unknown): string {
	if (value === undefined) return "—";
	if (typeof value === "string") return value;
	return JSON.stringify(value);
}

/** Stable list key. Problems have no id, but key+kind is unique per resolution. */
function problemKey(problem: ContractProblem): string {
	return "input_key" in problem
		? `${problem.kind}:${problem.input_key}`
		: `${problem.kind}:${JSON.stringify(problem)}`;
}

/**
 * Prose for a problem.
 *
 * The server's `Display` text is already good, but it is not sent — only the
 * structured variant is — so the wording lives here. Each one names the key and
 * the upstream task, because "validation failed" sends someone reading prompts
 * to guess.
 */
function describe(problem: ContractProblem): string {
	switch (problem.kind) {
		case "task_missing":
			return `Task #${problem.task_number} no longer exists.`;
		case "source_missing":
			return `\`${problem.input_key}\` reads from #${problem.source_task_number}, which no longer exists.`;
		case "source_has_no_outputs":
			return `\`${problem.input_key}\` is waiting on #${problem.source_task_number}, which has not produced output yet.`;
		case "pointer_missed":
			return `\`${problem.input_key}\`: #${problem.source_task_number} produced nothing at \`${problem.pointer}\`.`;
		case "empty_literal":
			return `\`${problem.input_key}\` is declared a literal but carries no value.`;
		case "schema_violation":
			return `${problem.side === "input" ? "Input" : "Output"} at \`${
				problem.path || "/"
			}\` does not match the schema: ${problem.message}`;
		case "invalid_schema":
			return `The declared ${problem.side} schema is not valid JSON Schema: ${problem.message}`;
		case "storage":
			return `\`${problem.input_key}\` could not be read: ${problem.message}`;
	}
}
