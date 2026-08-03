import {useCallback, useState} from "react";
import {Link} from "@tanstack/react-router";
import {useMutation, useQuery, useQueryClient} from "@tanstack/react-query";
import {Button} from "@spacedrive/primitives";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {faTrash} from "@fortawesome/free-solid-svg-icons";
import {api, type SaveWorkflowRequest, type Workflow} from "@/api/client";
import {parseJson} from "@/components/workflows/schemaForm";

/**
 * The templates, and the two things you can do to the set of them.
 *
 * Everything about a *single* template — steps, edges, bindings, launching —
 * lives in the editor. This page stays a list on purpose: a template with no
 * steps is not worth previewing, and the list endpoint does not carry them.
 */
export function Workflows() {
	const queryClient = useQueryClient();
	const [createOpen, setCreateOpen] = useState(false);
	const [pendingDelete, setPendingDelete] = useState<string | null>(null);

	const {data, isLoading, error} = useQuery({
		queryKey: ["workflows"],
		queryFn: api.listWorkflows,
	});

	const invalidate = useCallback(
		() => queryClient.invalidateQueries({queryKey: ["workflows"]}),
		[queryClient],
	);

	const create = useMutation({
		mutationFn: (body: SaveWorkflowRequest) => api.createWorkflow(body),
		onSuccess: () => {
			setCreateOpen(false);
			void invalidate();
		},
	});

	const remove = useMutation({
		mutationFn: (id: string) => api.deleteWorkflow(id),
		onSuccess: () => {
			setPendingDelete(null);
			void invalidate();
		},
	});

	const workflows = data?.workflows ?? [];

	return (
		<div className="flex h-full min-h-0 w-full flex-col">
			<div className="flex items-center justify-between border-b border-app-line px-4 py-2">
				<div className="flex items-center gap-3">
					<h1 className="text-sm font-medium text-ink">Workflows</h1>
					<span className="text-sm text-ink-dull">
						{workflows.length} template{workflows.length !== 1 ? "s" : ""}
					</span>
				</div>
				<Button
					size="md"
					onClick={() => {
						create.reset();
						setCreateOpen((open) => !open);
					}}
				>
					{createOpen ? "Cancel" : "New workflow"}
				</Button>
			</div>

			{createOpen && (
				<CreateWorkflowForm
					busy={create.isPending}
					error={create.error instanceof Error ? create.error.message : null}
					onCancel={() => setCreateOpen(false)}
					onSubmit={(body) => {
						create.reset();
						create.mutate(body);
					}}
				/>
			)}

			{remove.error instanceof Error && (
				<p className="border-b border-status-error/30 bg-status-error/5 px-4 py-2 font-mono text-xs text-status-error">
					{remove.error.message}
				</p>
			)}

			<div className="min-h-0 flex-1 overflow-y-auto">
				{isLoading ? (
					<p className="py-8 text-center text-sm text-ink-faint">
						Loading workflows…
					</p>
				) : error ? (
					<div className="py-8 text-center text-sm text-status-error">
						Failed to load workflows.
						<div className="mt-1 font-mono text-[10px] text-ink-faint">
							{(error as Error).message}
						</div>
					</div>
				) : workflows.length === 0 ? (
					<div className="flex h-full flex-col items-center justify-center gap-1">
						<p className="text-sm text-ink-dull">No workflows yet.</p>
						<p className="text-xs text-ink-faint">
							A workflow is a pipeline you can launch again and again.
						</p>
					</div>
				) : (
					<ul>
						{workflows.map((workflow) => (
							<WorkflowRow
								key={workflow.id}
								workflow={workflow}
								confirming={pendingDelete === workflow.id}
								busy={remove.isPending && remove.variables === workflow.id}
								onAskDelete={() => {
									remove.reset();
									setPendingDelete(workflow.id);
								}}
								onCancelDelete={() => setPendingDelete(null)}
								onConfirmDelete={() => remove.mutate(workflow.id)}
							/>
						))}
					</ul>
				)}
			</div>
		</div>
	);
}

function WorkflowRow({
	workflow,
	confirming,
	busy,
	onAskDelete,
	onCancelDelete,
	onConfirmDelete,
}: {
	workflow: Workflow;
	confirming: boolean;
	busy: boolean;
	onAskDelete: () => void;
	onCancelDelete: () => void;
	onConfirmDelete: () => void;
}) {
	return (
		<li className="group border-b border-app-line/40">
			<div className="flex items-center gap-3 px-4 py-2.5 hover:bg-app-box/40">
				<Link
					to="/workflows/$workflowId"
					params={{workflowId: workflow.id}}
					className="min-w-0 flex-1"
				>
					<div className="flex items-baseline gap-2">
						<span className="truncate font-mono text-sm text-ink">
							{workflow.name}
						</span>
						{workflow.input_schema != null && (
							<span
								className="shrink-0 rounded border border-app-line px-1 text-[9px] uppercase tracking-wide text-ink-faint"
								title="Declares the input a run is launched with"
							>
								typed input
							</span>
						)}
					</div>
					{workflow.description && (
						<p className="mt-0.5 truncate text-xs text-ink-dull">
							{workflow.description}
						</p>
					)}
				</Link>

				{confirming ? (
					// Deleting a template is not undoable, and the runs it already
					// produced survive it — worth one sentence rather than a bare
					// confirm dialog that says neither.
					<div className="flex shrink-0 items-center gap-2">
						<span className="text-[11px] text-ink-dull">
							Delete the template? Runs it already launched are kept.
						</span>
						<Button size="sm" variant="colored" className="border-status-error bg-status-error" disabled={busy} onClick={onConfirmDelete}>
							{busy ? "Deleting…" : "Delete"}
						</Button>
						<Button size="sm" variant="gray" onClick={onCancelDelete}>
							Cancel
						</Button>
					</div>
				) : (
					<button
						type="button"
						onClick={onAskDelete}
						title={`Delete \`${workflow.name}\``}
						className="shrink-0 px-1 text-ink-faint opacity-0 transition-opacity hover:text-status-error focus:opacity-100 group-hover:opacity-100"
					>
						<FontAwesomeIcon icon={faTrash} className="text-[11px]" />
					</button>
				)}
			</div>
		</li>
	);
}

/**
 * Name, description, and the shape of a run's launch input.
 *
 * The schema is optional and stays a JSON box here: it is the one field whose
 * author is necessarily comfortable with JSON Schema, and guessing at a builder
 * for it would only get in the way. What it buys is the launcher — a declared
 * schema turns that screen from a JSON textarea into a labelled form.
 */
function CreateWorkflowForm({
	busy,
	error,
	onSubmit,
	onCancel,
}: {
	busy: boolean;
	error: string | null;
	onSubmit: (body: SaveWorkflowRequest) => void;
	onCancel: () => void;
}) {
	const [name, setName] = useState("");
	const [description, setDescription] = useState("");
	const [schema, setSchema] = useState("");
	const [localError, setLocalError] = useState<string | null>(null);

	const submit = () => {
		const trimmed = name.trim();
		if (trimmed === "") {
			setLocalError("A workflow needs a name — it is how runs are identified.");
			return;
		}
		const parsed = parseJson(schema);
		if ("error" in parsed) {
			setLocalError(`Input schema: ${parsed.error}`);
			return;
		}
		setLocalError(null);
		onSubmit({
			name: trimmed,
			description: description.trim() || null,
			input_schema: parsed.value,
		});
	};

	return (
		<div className="border-b border-app-line bg-app-box/30 px-4 py-3">
			<div className="mb-2 flex gap-2">
				<input
					value={name}
					onChange={(event) => setName(event.target.value)}
					spellCheck={false}
					autoFocus
					placeholder="release-notes-chain"
					className="w-64 rounded border border-app-line bg-app px-2 py-1 font-mono text-xs text-ink outline-none focus:border-accent"
				/>
				<input
					value={description}
					onChange={(event) => setDescription(event.target.value)}
					placeholder="What this pipeline does"
					className="min-w-0 flex-1 rounded border border-app-line bg-app px-2 py-1 text-xs text-ink outline-none focus:border-accent"
				/>
			</div>

			<label className="mb-0.5 block text-[11px] font-medium text-ink-dull">
				Launch input schema
			</label>
			<p className="mb-1 text-[10px] text-ink-faint">
				Optional JSON Schema for the payload a run is started with. Declaring it
				turns the launcher into a form instead of a JSON box.
			</p>
			<textarea
				value={schema}
				onChange={(event) => setSchema(event.target.value)}
				spellCheck={false}
				rows={5}
				placeholder={
					'{\n  "type": "object",\n  "properties": {"version": {"type": "string"}},\n  "required": ["version"]\n}'
				}
				className="mb-2 w-full rounded border border-app-line bg-app px-2 py-1.5 font-mono text-[11px] text-ink outline-none focus:border-accent"
			/>

			{(localError || error) && (
				<p className="mb-2 break-words rounded border border-status-error/30 bg-status-error/5 px-2 py-1 font-mono text-[11px] text-status-error">
					{localError ?? error}
				</p>
			)}

			<div className="flex gap-2">
				<Button size="sm" variant="accent" disabled={busy} onClick={submit}>
					{busy ? "Creating…" : "Create workflow"}
				</Button>
				<Button size="sm" variant="gray" onClick={onCancel}>
					Cancel
				</Button>
			</div>
		</div>
	);
}
