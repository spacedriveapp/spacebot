import { useQuery } from "@tanstack/react-query";
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
	const { data } = useQuery({
		queryKey: ["task-contract", taskNumber],
		queryFn: () => api.getTaskContract(taskNumber),
	});

	if (!data) return null;
	return <ContractSectionView data={data} onSelectTask={onSelectTask} />;
}

/** Split from the fetching wrapper so it renders against fixtures. */
export function ContractSectionView({
	data,
	onSelectTask,
}: {
	data: TaskContractResponse;
	onSelectTask?: (taskNumber: number) => void;
}) {
	const hasContract =
		data.input_schema != null ||
		data.output_schema != null ||
		data.bindings.length > 0 ||
		data.outputs != null;

	// Most tasks declare nothing. An empty "Contract" heading on every one of
	// them would be noise that teaches people to skip the section.
	if (!hasContract) return null;

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
		</div>
	);
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
