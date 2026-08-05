import {useMemo, useState} from "react";
import {Button} from "@spacedrive/primitives";
import type {LaunchRequest, Workflow} from "@/api/client";
import {
	buildInputs,
	fieldsFor,
	initialValues,
	parseJson,
	type FieldValue,
	type SchemaField,
} from "./schemaForm";

/**
 * Start a run.
 *
 * The launch payload is the one value the whole pipeline is driven from — every
 * `run_input` binding reads a pointer into it — so getting it wrong does not
 * fail here, it fails three steps later with a task that resolved to nothing.
 * When the template declares an `input_schema`, that is used to build labelled
 * controls; only an undeclared or unrepresentable schema falls back to raw JSON.
 */
export function LaunchPanel({
	workflow,
	agents,
	busy,
	error,
	stepCount,
	onLaunch,
	onCancel,
}: {
	workflow: Workflow;
	agents: {id: string; display_name?: string | null}[];
	busy?: boolean;
	error: string | null;
	stepCount: number;
	onLaunch: (body: LaunchRequest) => void;
	onCancel: () => void;
}) {
	const fields = useMemo(
		() => fieldsFor(workflow.input_schema),
		[workflow.input_schema],
	);
	const [values, setValues] = useState<Record<string, FieldValue>>(() =>
		fields ? initialValues(fields) : {},
	);
	const [raw, setRaw] = useState(() =>
		workflow.input_schema == null ? "" : "{}",
	);
	const [launchedBy, setLaunchedBy] = useState(agents[0]?.id ?? "");
	const [localError, setLocalError] = useState<string | null>(null);

	const submit = () => {
		if (launchedBy.trim() === "") {
			setLocalError("Name the agent launching this run.");
			return;
		}

		let inputs: unknown;
		if (fields) {
			const built = buildInputs(fields, values);
			if ("error" in built) {
				setLocalError(built.error);
				return;
			}
			inputs = built.inputs;
		} else {
			const parsed = parseJson(raw);
			if ("error" in parsed) {
				setLocalError(`Inputs: ${parsed.error}`);
				return;
			}
			inputs = parsed.value ?? {};
		}

		setLocalError(null);
		onLaunch({inputs, launched_by: launchedBy.trim()});
	};

	return (
		<div className="border-b border-app-line bg-app-box/30 px-4 py-3">
			<div className="mb-2 flex items-baseline justify-between gap-2">
				<h3 className="text-xs font-medium uppercase tracking-wide text-ink-dull">
					Launch a run
				</h3>
				<span className="text-[10px] text-ink-faint">
					Compiles {stepCount} step{stepCount === 1 ? "" : "s"} into tasks and
					hands them to the scheduler.
				</span>
			</div>

			{fields ? (
				fields.length === 0 ? (
					<p className="mb-2 text-[11px] text-ink-faint">
						This workflow takes no launch input.
					</p>
				) : (
					<div className="mb-2 space-y-2">
						{fields.map((field) => (
							<SchemaControl
								key={field.key}
								field={field}
								value={values[field.key] ?? ""}
								onChange={(next) =>
									setValues((prev) => ({...prev, [field.key]: next}))
								}
							/>
						))}
					</div>
				)
			) : (
				<div className="mb-2">
					<label
						htmlFor="launch-inputs-json"
						className="mb-0.5 block text-[11px] font-medium text-ink-dull"
					>
						Inputs
					</label>
					<p className="mb-1 text-[10px] text-ink-faint">
						{workflow.input_schema == null
							? "This workflow declares no input schema, so the payload is free-form JSON."
							: "The declared schema is richer than this form can render, so the payload is edited as JSON."}
					</p>
					<textarea
						id="launch-inputs-json"
						value={raw}
						onChange={(event) => setRaw(event.target.value)}
						spellCheck={false}
						rows={5}
						placeholder={'{\n  "version": "v1.4.2"\n}'}
						className="w-full rounded border border-app-line bg-app px-2 py-1.5 font-mono text-[11px] text-ink outline-none focus:border-accent"
					/>
				</div>
			)}

			<div className="mb-2">
				<label
					htmlFor="launch-launched-by"
					className="mb-0.5 block text-[11px] font-medium text-ink-dull"
				>
					Launched by
				</label>
				<p className="mb-1 text-[10px] text-ink-faint">
					Credited with the run, and the default assignee for any step that does
					not name one.
				</p>
				<select
					id="launch-launched-by"
					value={launchedBy}
					onChange={(event) => setLaunchedBy(event.target.value)}
					className="w-full rounded border border-app-line bg-app px-2 py-1 text-[11px] text-ink outline-none focus:border-accent"
				>
					{agents.length === 0 && <option value="">No agents available</option>}
					{agents.map((agent) => (
						<option key={agent.id} value={agent.id}>
							{agent.display_name ?? agent.id}
						</option>
					))}
				</select>
			</div>

			{(localError || error) && (
				<p className="mb-2 break-words rounded border border-status-error/30 bg-status-error/5 px-2 py-1 font-mono text-[11px] text-status-error">
					{localError ?? error}
				</p>
			)}

			<div className="flex gap-2">
				<Button
					size="sm"
					variant="accent"
					disabled={busy || stepCount === 0}
					title={
						stepCount === 0 ? "Add a step before launching a run." : undefined
					}
					onClick={submit}
				>
					{busy ? "Launching…" : "Launch"}
				</Button>
				<Button size="sm" variant="gray" onClick={onCancel}>
					Cancel
				</Button>
			</div>
		</div>
	);
}

function SchemaControl({
	field,
	value,
	onChange,
}: {
	field: SchemaField;
	value: FieldValue;
	onChange: (next: FieldValue) => void;
}) {
	const label = field.title ?? field.key;
	// One control per field, so the id can come straight from the field key.
	const controlId = `launch-field-${field.key}`;

	if (field.kind === "boolean") {
		return (
			<label className="flex items-center gap-2 text-[11px] text-ink-dull">
				<input
					type="checkbox"
					checked={value === true}
					onChange={(event) => onChange(event.target.checked)}
					className="accent-accent"
				/>
				<span>{label}</span>
				{field.description && (
					<span className="text-[10px] text-ink-faint">{field.description}</span>
				)}
			</label>
		);
	}

	return (
		<div>
			<label
				htmlFor={controlId}
				className="mb-0.5 block text-[11px] font-medium text-ink-dull"
			>
				{label}
				{field.required && (
					<span className="ml-1 text-status-error" title="Required">
						*
					</span>
				)}
				{label !== field.key && (
					<span className="ml-1.5 font-mono text-[10px] text-ink-faint">
						{field.key}
					</span>
				)}
			</label>
			{field.description && (
				<p className="mb-1 text-[10px] text-ink-faint">{field.description}</p>
			)}
			{field.kind === "enum" ? (
				<select
					id={controlId}
					value={typeof value === "string" ? value : ""}
					onChange={(event) => onChange(event.target.value)}
					className="w-full rounded border border-app-line bg-app px-2 py-1 text-[11px] text-ink outline-none focus:border-accent"
				>
					<option value="">Choose…</option>
					{(field.options ?? []).map((option) => (
						<option key={option} value={option}>
							{option}
						</option>
					))}
				</select>
			) : (
				<input
					id={controlId}
					value={typeof value === "string" ? value : ""}
					onChange={(event) => onChange(event.target.value)}
					inputMode={
						field.kind === "number" || field.kind === "integer"
							? "decimal"
							: undefined
					}
					spellCheck={false}
					placeholder={field.kind === "string" ? "" : "0"}
					className="w-full rounded border border-app-line bg-app px-2 py-1 text-[11px] text-ink outline-none focus:border-accent"
				/>
			)}
		</div>
	);
}
