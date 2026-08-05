import {useCallback, useMemo, useState} from "react";
import {Link} from "@tanstack/react-router";
import {
	useMutation,
	useQueries,
	useQuery,
	useQueryClient,
} from "@tanstack/react-query";
import {Button} from "@spacedrive/primitives";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {faTrash, faTriangleExclamation} from "@fortawesome/free-solid-svg-icons";
import {
	api,
	type SaveWorkflowRequest,
	type Workflow,
	type WorkflowRun,
} from "@/api/client";
import {parseJson} from "@/components/workflows/schemaForm";
import {RunStatusPill, runStatusOf} from "@/components/workflows/runStatus";

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

	const workflows = useMemo(() => data?.workflows ?? [], [data]);

	/**
	 * Every template's runs, so a stuck one is findable from here.
	 *
	 * This is the screen a person is on when they have not already decided which
	 * pipeline to worry about, and a stuck run announces itself nowhere else that
	 * is reachable without opening each template in turn — which is the silence
	 * run state was added to end. So the list asks.
	 *
	 * One request per template rather than one for the instance, because there is
	 * no endpoint for the latter: runs are listed under their workflow. The keys
	 * are exactly the ones the template editor uses, so opening a template reuses
	 * what this already fetched rather than refetching it.
	 */
	const runQueries = useQueries({
		queries: workflows.map((workflow) => ({
			queryKey: ["workflow-runs", workflow.id],
			queryFn: () => api.listWorkflowRuns(workflow.id),
			staleTime: 15_000,
			refetchInterval: 30_000,
		})),
	});

	/** template id → its runs, for the row badges. */
	const runsByWorkflow = useMemo(() => {
		const map = new Map<string, WorkflowRun[]>();
		workflows.forEach((workflow, index) => {
			map.set(workflow.id, runQueries[index]?.data?.runs ?? []);
		});
		return map;
	}, [workflows, runQueries]);

	// Stuck only, and deliberately not "everything that went wrong". A failed run
	// is a report — the pipeline ran and said no — and it is already visible on
	// the template it belongs to. A stuck run is the one nobody is going to find
	// on their own, because it produced no report at all; it simply stopped. A
	// banner that also carried failures would be permanently non-empty on an
	// instance with one flaky template, and a banner nobody can clear is a banner
	// nobody reads.
	const stuck = useMemo(
		() =>
			workflows.flatMap((workflow) =>
				(runsByWorkflow.get(workflow.id) ?? [])
					.filter((run) => runStatusOf(run) === "stuck")
					.map((run) => ({workflow, run})),
			),
		[workflows, runsByWorkflow],
	);

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

			<StuckRunsBanner stuck={stuck} />

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
								runs={runsByWorkflow.get(workflow.id) ?? []}
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

/**
 * The wedged runs on this instance, at the top of the page that lists pipelines.
 *
 * A stuck run has, by definition, nothing left that will announce it: no task
 * is failing, no worker is dying, nothing is being retried. It stops, quietly,
 * and stays stopped. The inbox catches the transition once, and an inbox entry
 * is read once and then dismissed — so a run that is *still* stuck a day later
 * needs somewhere standing to be seen, and this is the only screen a person
 * reaches without having already guessed which template to suspect.
 *
 * Each entry links straight to the run rather than to its template, because the
 * reason is on the run and the two actions it wants — cancel, delete — are
 * there too.
 */
function StuckRunsBanner({
	stuck,
}: {
	stuck: {workflow: Workflow; run: WorkflowRun}[];
}) {
	if (stuck.length === 0) return null;
	return (
		<div className="border-b border-status-warning/60 bg-status-warning/10 px-4 py-2">
			<div className="flex items-center gap-2 text-xs font-medium text-status-warning">
				<FontAwesomeIcon icon={faTriangleExclamation} className="text-[11px]" />
				{stuck.length} run{stuck.length === 1 ? " is" : "s are"} stuck
				<span className="font-normal opacity-80">
					— nothing in {stuck.length === 1 ? "it" : "them"} can advance, and
					nothing will change that on its own.
				</span>
			</div>
			<ul className="mt-1.5 space-y-1">
				{stuck.map(({workflow, run}) => (
					<li key={run.id}>
						<Link
							to="/workflow-runs/$runId"
							params={{runId: run.id}}
							className="block rounded border border-status-warning/30 bg-app/40 px-2 py-1 hover:border-status-warning/60"
						>
							<div className="flex items-baseline gap-2">
								<span className="truncate font-mono text-[11px] text-ink">
									{workflow.name}
								</span>
								<span className="shrink-0 text-[10px] text-ink-faint">
									{new Date(run.created_at).toLocaleString()} · by{" "}
									{run.launched_by}
								</span>
							</div>
							{run.status_reason && (
								<p
									className="truncate text-[10px] text-status-warning"
									title={run.status_reason}
								>
									{run.status_reason}
								</p>
							)}
						</Link>
					</li>
				))}
			</ul>
		</div>
	);
}

function WorkflowRow({
	workflow,
	runs,
	confirming,
	busy,
	onAskDelete,
	onCancelDelete,
	onConfirmDelete,
}: {
	workflow: Workflow;
	runs: WorkflowRun[];
	confirming: boolean;
	busy: boolean;
	onAskDelete: () => void;
	onCancelDelete: () => void;
	onConfirmDelete: () => void;
}) {
	// The worst thing this template's runs are currently doing, and nothing else.
	// A row is a template; enumerating its five run statuses here would make the
	// list a dashboard, and the one status worth interrupting a scan for is the
	// one that will not resolve itself.
	const stuck = runs.filter((run) => runStatusOf(run) === "stuck").length;
	const running = runs.filter((run) => runStatusOf(run) === "running").length;

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
						{stuck > 0 ? (
							<span
								className="inline-flex shrink-0 items-center gap-1 rounded-full border border-status-warning bg-status-warning/15 px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wide text-status-warning"
								title={`${stuck} run${stuck === 1 ? "" : "s"} of this template cannot advance. Nothing is in flight and nothing at the frontier can move.`}
							>
								<FontAwesomeIcon
									icon={faTriangleExclamation}
									className="text-[9px]"
								/>
								{stuck} stuck
							</span>
						) : (
							running > 0 && (
								<RunStatusPill
									status="running"
									className="self-center"
								/>
							)
						)}
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
					aria-label="Workflow key"
					placeholder="release-notes-chain"
					className="w-64 rounded border border-app-line bg-app px-2 py-1 font-mono text-xs text-ink outline-none focus:border-accent"
				/>
				<input
					value={description}
					onChange={(event) => setDescription(event.target.value)}
					aria-label="Workflow description"
					placeholder="What this pipeline does"
					className="min-w-0 flex-1 rounded border border-app-line bg-app px-2 py-1 text-xs text-ink outline-none focus:border-accent"
				/>
			</div>

			<label
				htmlFor="workflow-input-schema"
				className="mb-0.5 block text-[11px] font-medium text-ink-dull"
			>
				Launch input schema
			</label>
			<p className="mb-1 text-[10px] text-ink-faint">
				Optional JSON Schema for the payload a run is started with. Declaring it
				turns the launcher into a form instead of a JSON box.
			</p>
			<textarea
				id="workflow-input-schema"
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
