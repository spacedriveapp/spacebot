import {useCallback, useEffect, useMemo, useState} from "react";
import {Link, useNavigate} from "@tanstack/react-router";
import {useMutation, useQuery, useQueryClient} from "@tanstack/react-query";
import {Button} from "@spacedrive/primitives";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {faArrowLeft, faArrowRightLong} from "@fortawesome/free-solid-svg-icons";
import {
	api,
	type LaunchRequest,
	type SaveBindingRequest,
	type SaveStepRequest,
	type SaveWorkflowRequest,
	type StepEdgeRequest,
	type WorkflowDetailResponse,
	type WorkflowStep,
} from "@/api/client";
import {StepDetail} from "@/components/workflows/StepDetail";
import {LaunchPanel} from "@/components/workflows/LaunchPanel";
import {WorkflowCanvas} from "@/components/workflows/WorkflowCanvas";
import {
	useWorkflowView,
	ViewToggle,
} from "@/components/workflows/ViewToggle";
import {orderSteps, parentsByStep, suggestStepKey} from "@/components/workflows/graph";
import {parseJson} from "@/components/workflows/schemaForm";

/**
 * The template editor: steps, the order they run in, and what feeds them.
 *
 * One query backs the whole screen. `GET /workflows/{id}` returns the workflow,
 * its steps, its edges and its bindings together, and every mutation returns
 * the same shape — so each save is written straight into the cache rather than
 * triggering a refetch. That matters more here than it looks: adding an edge
 * changes both the canvas layout and the order the step list is drawn in, and a
 * refetch would leave the screen one round trip behind the drag that rewired it.
 *
 * The graph is the default face of that data and the list is the other one.
 * They are two renderings of the same query and the same mutations — the panel
 * on the right is literally the same component either way — so switching views
 * cannot change what a template means, only how much of it fits on screen.
 */
export function WorkflowDetail({workflowId}: {workflowId: string}) {
	const queryClient = useQueryClient();
	const navigate = useNavigate();
	const detailKey = useMemo(() => ["workflow", workflowId], [workflowId]);

	const {data, isLoading, error} = useQuery({
		queryKey: detailKey,
		queryFn: () => api.getWorkflow(workflowId),
	});

	const {data: agentsData} = useQuery({
		queryKey: ["agents"],
		queryFn: api.agents,
		staleTime: 10_000,
	});
	const agents = agentsData?.agents ?? [];

	const {data: runsData} = useQuery({
		queryKey: ["workflow-runs", workflowId],
		queryFn: () => api.listWorkflowRuns(workflowId),
	});

	const [view, setView] = useWorkflowView();
	const [selectedKey, setSelectedKey] = useState<string | null>(null);
	const [addOpen, setAddOpen] = useState(false);
	const [launchOpen, setLaunchOpen] = useState(false);
	const [templateOpen, setTemplateOpen] = useState(false);

	// Writing the response into the cache keeps the list, the picker and the
	// panel showing the same graph in the same frame.
	const absorb = useCallback(
		(next: WorkflowDetailResponse) => queryClient.setQueryData(detailKey, next),
		[queryClient, detailKey],
	);

	const saveStep = useMutation({
		mutationFn: ({stepKey, body}: {stepKey: string; body: SaveStepRequest}) =>
			api.saveWorkflowStep(workflowId, stepKey, body),
		onSuccess: absorb,
	});
	const deleteStep = useMutation({
		mutationFn: (stepKey: string) => api.deleteWorkflowStep(workflowId, stepKey),
		onSuccess: (next) => {
			absorb(next);
			setSelectedKey(null);
		},
	});
	const addEdge = useMutation({
		mutationFn: (body: StepEdgeRequest) => api.addWorkflowEdge(workflowId, body),
		onSuccess: absorb,
	});
	const removeEdge = useMutation({
		mutationFn: (body: StepEdgeRequest) =>
			api.removeWorkflowEdge(workflowId, body),
		onSuccess: absorb,
	});
	const setBinding = useMutation({
		mutationFn: ({
			stepKey,
			inputKey,
			body,
		}: {
			stepKey: string;
			inputKey: string;
			body: SaveBindingRequest;
		}) => api.setWorkflowBinding(workflowId, stepKey, inputKey, body),
		onSuccess: absorb,
	});
	const removeBinding = useMutation({
		mutationFn: ({stepKey, inputKey}: {stepKey: string; inputKey: string}) =>
			api.removeWorkflowBinding(workflowId, stepKey, inputKey),
		onSuccess: absorb,
	});
	const updateWorkflow = useMutation({
		mutationFn: (body: SaveWorkflowRequest) =>
			api.updateWorkflow(workflowId, body),
		onSuccess: (next) => {
			// This one returns only the workflow, so the rest of the detail is
			// carried over rather than dropped.
			const current = queryClient.getQueryData<WorkflowDetailResponse>(detailKey);
			if (current) absorb({...current, workflow: next.workflow});
			void queryClient.invalidateQueries({queryKey: ["workflows"]});
			setTemplateOpen(false);
		},
	});
	const launch = useMutation({
		mutationFn: (body: LaunchRequest) => api.launchWorkflow(workflowId, body),
		onSuccess: (result) => {
			setLaunchOpen(false);
			void queryClient.invalidateQueries({queryKey: ["workflow-runs", workflowId]});
			void queryClient.invalidateQueries({queryKey: ["tasks"]});
			void navigate({
				to: "/workflow-runs/$runId",
				params: {runId: result.run.id},
			});
		},
	});

	const steps = useMemo(() => data?.steps ?? [], [data]);
	const edges = useMemo(() => data?.edges ?? [], [data]);
	const bindings = useMemo(() => data?.bindings ?? [], [data]);

	// Wiring is reachable two ways — dragged on the canvas, or picked in the
	// panel — and both are the same call. Hoisting them here is what keeps a
	// refusal readable in whichever half of the screen the author was using.
	// `kind` is which way out of a loop the edge is: `normal` for converging,
	// `on_exhausted` for giving up. Defaulted rather than optional at the call
	// sites, because an edge sent with no kind is a `normal` one — and a give-up
	// edge that silently arrived as normal would be the exact merge the two arms
	// exist to prevent.
	const onAddEdge = useCallback(
		(
			parentKey: string,
			childKey: string,
			kind: "normal" | "on_exhausted" = "normal",
		) => {
			addEdge.reset();
			removeEdge.reset();
			addEdge.mutate({
				parent_step_key: parentKey,
				child_step_key: childKey,
				kind,
			});
		},
		[addEdge, removeEdge],
	);
	const onRemoveEdge = useCallback(
		(parentKey: string, childKey: string) => {
			addEdge.reset();
			removeEdge.reset();
			removeEdge.mutate({parent_step_key: parentKey, child_step_key: childKey});
		},
		[addEdge, removeEdge],
	);
	const edgeBusy = addEdge.isPending || removeEdge.isPending;
	const edgeError =
		(addEdge.error ?? removeEdge.error) instanceof Error
			? ((addEdge.error ?? removeEdge.error) as Error).message
			: null;

	const {ordered, cycle} = useMemo(
		() => orderSteps(steps, edges),
		[steps, edges],
	);
	const parents = useMemo(() => parentsByStep(edges), [edges]);
	const bindingCounts = useMemo(() => {
		const counts = new Map<string, number>();
		for (const binding of bindings) {
			counts.set(binding.step_key, (counts.get(binding.step_key) ?? 0) + 1);
		}
		return counts;
	}, [bindings]);

	// Keep a selection pointing at a step that still exists, and land on the
	// first one so the panel is never empty for a template that has steps.
	useEffect(() => {
		if (ordered.length === 0) {
			if (selectedKey !== null) setSelectedKey(null);
			return;
		}
		if (!selectedKey || !ordered.some((s) => s.step_key === selectedKey)) {
			setSelectedKey(ordered[0].step_key);
		}
	}, [ordered, selectedKey]);

	const selected = ordered.find((step) => step.step_key === selectedKey) ?? null;

	if (isLoading) {
		return (
			<p className="py-8 text-center text-sm text-ink-faint">Loading workflow…</p>
		);
	}
	if (error || !data) {
		return (
			<div className="py-8 text-center text-sm text-status-error">
				Failed to load this workflow.
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

	const workflow = data.workflow;

	return (
		<div className="flex h-full min-h-0 w-full flex-col">
			{/* Header */}
			<div className="flex items-center gap-3 border-b border-app-line px-4 py-2">
				<Link
					to="/workflows"
					className="shrink-0 text-ink-faint hover:text-ink-dull"
					title="All workflows"
				>
					<FontAwesomeIcon icon={faArrowLeft} className="text-xs" />
				</Link>
				<div className="min-w-0 flex-1">
					<div className="flex items-baseline gap-2">
						<h1 className="truncate font-mono text-sm text-ink">
							{workflow.name}
						</h1>
						<button
							type="button"
							onClick={() => {
								updateWorkflow.reset();
								setTemplateOpen((open) => !open);
							}}
							className="shrink-0 text-[11px] text-ink-faint hover:text-ink-dull hover:underline"
						>
							{templateOpen ? "Close" : "Edit template…"}
						</button>
					</div>
					{workflow.description && (
						<p className="truncate text-xs text-ink-dull">
							{workflow.description}
						</p>
					)}
				</div>
				<Button
					size="md"
					onClick={() => {
						launch.reset();
						setLaunchOpen((open) => !open);
					}}
				>
					{launchOpen ? "Cancel" : "Launch"}
				</Button>
			</div>

			{templateOpen && (
				<TemplateForm
					name={workflow.name}
					description={workflow.description ?? ""}
					inputSchema={workflow.input_schema}
					busy={updateWorkflow.isPending}
					error={
						updateWorkflow.error instanceof Error
							? updateWorkflow.error.message
							: null
					}
					onSubmit={(body) => {
						updateWorkflow.reset();
						updateWorkflow.mutate(body);
					}}
					onCancel={() => setTemplateOpen(false)}
				/>
			)}

			{launchOpen && (
				<LaunchPanel
					workflow={workflow}
					agents={agents}
					stepCount={steps.length}
					busy={launch.isPending}
					error={launch.error instanceof Error ? launch.error.message : null}
					onLaunch={(body) => {
						launch.reset();
						launch.mutate(body);
					}}
					onCancel={() => setLaunchOpen(false)}
				/>
			)}

			{/* A cycle cannot be ordered or launched. The server refuses to create
			    one, so this only fires for a template that already contains one —
			    but leaving it unexplained would make the step list look randomly
			    sorted for no visible reason. */}
			{cycle.length > 0 && (
				<p className="border-b border-status-error/30 bg-status-error/5 px-4 py-2 text-xs text-status-error">
					These steps form a cycle and cannot be ordered:{" "}
					<span className="font-mono">{cycle.join(" → ")}</span>. Launching will
					be refused until one of those prerequisites is removed.
				</p>
			)}

			<div className="flex min-h-0 flex-1">
				{/* Steps */}
				<div className="flex min-w-0 flex-1 flex-col">
					<div className="flex items-center justify-between gap-3 border-b border-app-line/40 px-4 py-1.5">
						<span className="min-w-0 truncate text-xs text-ink-dull">
							{steps.length} step{steps.length === 1 ? "" : "s"}
							{view === "canvas"
								? ", laid out by what waits for what"
								: ", in the order they run"}
						</span>
						<div className="flex shrink-0 items-center gap-3">
							<button
								type="button"
								onClick={() => {
									saveStep.reset();
									setAddOpen((open) => !open);
								}}
								className="text-[11px] text-ink-faint hover:text-ink-dull hover:underline"
							>
								{addOpen ? "Cancel" : "Add a step…"}
							</button>
							<ViewToggle value={view} onChange={setView} />
						</div>
					</div>

					{addOpen && (
						<AddStepForm
							takenKeys={steps.map((step) => step.step_key)}
							nextPosition={steps.length}
							busy={saveStep.isPending}
							error={
								saveStep.error instanceof Error ? saveStep.error.message : null
							}
							onCancel={() => setAddOpen(false)}
							onSubmit={(stepKey, body) => {
								saveStep.reset();
								saveStep.mutate(
									{stepKey, body},
									{
										onSuccess: () => {
											setAddOpen(false);
											setSelectedKey(stepKey);
										},
									},
								);
							}}
						/>
					)}

					<div className="min-h-0 flex-1 overflow-hidden">
						{view === "canvas" ? (
							<WorkflowCanvas
								steps={steps}
								edges={edges}
								bindings={bindings}
								cycle={cycle}
								selectedKey={selectedKey}
								onSelect={setSelectedKey}
								edgeBusy={edgeBusy}
								edgeError={edgeError}
								onAddEdge={onAddEdge}
								onRemoveEdge={onRemoveEdge}
							/>
						) : ordered.length === 0 ? (
							<div className="flex h-full flex-col items-center justify-center gap-1">
								<p className="text-sm text-ink-dull">No steps yet.</p>
								<p className="text-xs text-ink-faint">
									A step becomes one task per launch.
								</p>
							</div>
						) : (
							<div className="h-full overflow-y-auto">
								<ol>
									{ordered.map((step, index) => (
										<StepRow
											key={step.step_key}
											step={step}
											index={index}
											parents={parents.get(step.step_key) ?? []}
											bindingCount={bindingCounts.get(step.step_key) ?? 0}
											selected={step.step_key === selectedKey}
											inCycle={cycle.includes(step.step_key)}
											onSelect={() => setSelectedKey(step.step_key)}
										/>
									))}
								</ol>
							</div>
						)}
					</div>

					<RunsSection runs={runsData?.runs ?? []} />
				</div>

				{/* Selected step */}
				<div className="flex w-[420px] shrink-0 flex-col border-l border-app-line">
					{selected ? (
						<StepDetail
							step={selected}
							steps={steps}
							edges={edges}
							bindings={bindings}
							hasRunInput={workflow.input_schema != null}
							agents={agents}
							stepBusy={saveStep.isPending || deleteStep.isPending}
							stepError={
								(saveStep.error ?? deleteStep.error) instanceof Error
									? ((saveStep.error ?? deleteStep.error) as Error).message
									: null
							}
							edgeBusy={edgeBusy}
							edgeError={edgeError}
							bindingBusy={setBinding.isPending || removeBinding.isPending}
							bindingError={
								(setBinding.error ?? removeBinding.error) instanceof Error
									? ((setBinding.error ?? removeBinding.error) as Error).message
									: null
							}
							onSave={(stepKey, body) => {
								saveStep.reset();
								saveStep.mutate({stepKey, body});
							}}
							onDelete={(stepKey) => {
								deleteStep.reset();
								deleteStep.mutate(stepKey);
							}}
							onAddEdge={onAddEdge}
							onRemoveEdge={onRemoveEdge}
							onSetBinding={(stepKey, inputKey, body) => {
								setBinding.reset();
								removeBinding.reset();
								setBinding.mutate({stepKey, inputKey, body});
							}}
							onRemoveBinding={(stepKey, inputKey) => {
								setBinding.reset();
								removeBinding.reset();
								removeBinding.mutate({stepKey, inputKey});
							}}
						/>
					) : (
						<p className="px-4 py-6 text-center text-xs text-ink-faint">
							Select a step to edit it.
						</p>
					)}
				</div>
			</div>
		</div>
	);
}

function StepRow({
	step,
	index,
	parents,
	bindingCount,
	selected,
	inCycle,
	onSelect,
}: {
	step: WorkflowStep;
	index: number;
	parents: string[];
	bindingCount: number;
	selected: boolean;
	inCycle: boolean;
	onSelect: () => void;
}) {
	return (
		<li>
			<button
				type="button"
				onClick={onSelect}
				className={`flex w-full items-start gap-3 border-b border-app-line/40 px-4 py-2.5 text-left ${
					selected ? "bg-app-box/60" : "hover:bg-app-box/40"
				}`}
			>
				<span className="mt-0.5 w-5 shrink-0 text-right font-mono text-[11px] text-ink-faint">
					{index + 1}
				</span>
				<span className="min-w-0 flex-1">
					<span className="flex items-baseline gap-2">
						<span className="truncate text-sm text-ink">{step.title}</span>
						<span className="shrink-0 font-mono text-[10px] text-ink-faint">
							{step.step_key}
						</span>
						{inCycle && (
							<span className="shrink-0 rounded border border-status-error/40 px-1 text-[9px] uppercase text-status-error">
								cycle
							</span>
						)}
					</span>
					<span className="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[10px] text-ink-faint">
						<span>{step.priority}</span>
						{parents.length > 0 && (
							<span className="inline-flex items-center gap-1">
								<FontAwesomeIcon
									icon={faArrowRightLong}
									className="text-[8px]"
								/>
								waits for{" "}
								<span className="font-mono text-ink-dull">
									{parents.join(", ")}
								</span>
							</span>
						)}
						{bindingCount > 0 && (
							<span>
								{bindingCount} input{bindingCount === 1 ? "" : "s"}
							</span>
						)}
						{step.system_prompt && <span title={step.system_prompt}>prompt</span>}
					</span>
				</span>
			</button>
		</li>
	);
}

/** Every launch of this template, newest first. */
function RunsSection({
	runs,
}: {
	runs: {id: string; created_at: string; launched_by: string; inputs: unknown}[];
}) {
	if (runs.length === 0) return null;
	return (
		<div className="max-h-48 shrink-0 overflow-y-auto border-t border-app-line">
			<h3 className="sticky top-0 bg-app px-4 py-1.5 text-xs font-medium uppercase tracking-wide text-ink-dull">
				Runs
			</h3>
			<ul>
				{runs.map((run) => (
					<li key={run.id}>
						<Link
							to="/workflow-runs/$runId"
							params={{runId: run.id}}
							className="flex items-baseline gap-2 border-t border-app-line/40 px-4 py-1.5 text-xs hover:bg-app-box/40"
						>
							<span className="shrink-0 text-ink-dull">
								{new Date(run.created_at).toLocaleString()}
							</span>
							<span className="shrink-0 text-[10px] text-ink-faint">
								by {run.launched_by}
							</span>
							<span className="min-w-0 flex-1 truncate font-mono text-[10px] text-ink-faint">
								{JSON.stringify(run.inputs)}
							</span>
						</Link>
					</li>
				))}
			</ul>
		</div>
	);
}

/** Rename, re-describe, or change the shape of the launch input. */
function TemplateForm({
	name,
	description,
	inputSchema,
	busy,
	error,
	onSubmit,
	onCancel,
}: {
	name: string;
	description: string;
	inputSchema: unknown;
	busy: boolean;
	error: string | null;
	onSubmit: (body: SaveWorkflowRequest) => void;
	onCancel: () => void;
}) {
	const [nameText, setNameText] = useState(name);
	const [descriptionText, setDescriptionText] = useState(description);
	const [schemaText, setSchemaText] = useState(() =>
		inputSchema == null ? "" : JSON.stringify(inputSchema, null, 2),
	);
	const [localError, setLocalError] = useState<string | null>(null);

	const submit = () => {
		const trimmed = nameText.trim();
		if (trimmed === "") {
			setLocalError("A workflow needs a name.");
			return;
		}
		const parsed = parseJson(schemaText);
		if ("error" in parsed) {
			setLocalError(`Input schema: ${parsed.error}`);
			return;
		}
		setLocalError(null);
		onSubmit({
			name: trimmed,
			description: descriptionText.trim() || null,
			input_schema: parsed.value,
		});
	};

	return (
		<div className="border-b border-app-line bg-app-box/30 px-4 py-3">
			<div className="mb-2 flex gap-2">
				<input
					value={nameText}
					onChange={(event) => setNameText(event.target.value)}
					spellCheck={false}
					className="w-64 rounded border border-app-line bg-app px-2 py-1 font-mono text-xs text-ink outline-none focus:border-accent"
				/>
				<input
					value={descriptionText}
					onChange={(event) => setDescriptionText(event.target.value)}
					placeholder="What this pipeline does"
					className="min-w-0 flex-1 rounded border border-app-line bg-app px-2 py-1 text-xs text-ink outline-none focus:border-accent"
				/>
			</div>
			<label className="mb-0.5 block text-[11px] font-medium text-ink-dull">
				Launch input schema
			</label>
			<p className="mb-1 text-[10px] text-ink-faint">
				What a run is started with. `run input` bindings read pointers into this.
			</p>
			<textarea
				value={schemaText}
				onChange={(event) => setSchemaText(event.target.value)}
				spellCheck={false}
				rows={5}
				className="mb-2 w-full rounded border border-app-line bg-app px-2 py-1.5 font-mono text-[11px] text-ink outline-none focus:border-accent"
			/>
			{(localError || error) && (
				<p className="mb-2 break-words rounded border border-status-error/30 bg-status-error/5 px-2 py-1 font-mono text-[11px] text-status-error">
					{localError ?? error}
				</p>
			)}
			<div className="flex gap-2">
				<Button size="sm" variant="accent" disabled={busy} onClick={submit}>
					{busy ? "Saving…" : "Save template"}
				</Button>
				<Button size="sm" variant="gray" onClick={onCancel}>
					Cancel
				</Button>
			</div>
		</div>
	);
}

/**
 * Add a step.
 *
 * Only a title and a key, because everything else is easier to fill in against
 * the step once it exists — and the key is the one field that cannot be changed
 * afterwards, so it is worth showing while there is still a choice to make.
 */
function AddStepForm({
	takenKeys,
	nextPosition,
	busy,
	error,
	onSubmit,
	onCancel,
}: {
	takenKeys: string[];
	nextPosition: number;
	busy: boolean;
	error: string | null;
	onSubmit: (stepKey: string, body: SaveStepRequest) => void;
	onCancel: () => void;
}) {
	const [title, setTitle] = useState("");
	const [key, setKey] = useState("");
	const [keyTouched, setKeyTouched] = useState(false);
	const [localError, setLocalError] = useState<string | null>(null);

	const effectiveKey = keyTouched ? key : suggestStepKey(title, takenKeys);

	const submit = () => {
		const trimmedTitle = title.trim();
		if (trimmedTitle === "") {
			setLocalError("A step needs a title — it becomes the task's title.");
			return;
		}
		const trimmedKey = effectiveKey.trim();
		if (trimmedKey === "") {
			setLocalError("A step needs a key. Edges and bindings reference it.");
			return;
		}
		if (takenKeys.includes(trimmedKey)) {
			setLocalError(`\`${trimmedKey}\` is already a step. Pick another key.`);
			return;
		}
		setLocalError(null);
		onSubmit(trimmedKey, {
			title: trimmedTitle,
			priority: "medium",
			position: nextPosition,
		});
	};

	return (
		<div className="border-b border-app-line bg-app-box/30 px-4 py-3">
			<div className="mb-2 flex gap-2">
				<input
					value={title}
					onChange={(event) => setTitle(event.target.value)}
					autoFocus
					placeholder="Draft the release headline"
					className="min-w-0 flex-1 rounded border border-app-line bg-app px-2 py-1 text-xs text-ink outline-none focus:border-accent"
				/>
				<input
					value={effectiveKey}
					onChange={(event) => {
						setKeyTouched(true);
						setKey(event.target.value);
					}}
					spellCheck={false}
					placeholder="draft"
					title="The stable name edges and bindings reference. It cannot be changed later."
					className="w-44 rounded border border-app-line bg-app px-2 py-1 font-mono text-xs text-ink outline-none focus:border-accent"
				/>
			</div>
			{(localError || error) && (
				<p className="mb-2 break-words rounded border border-status-error/30 bg-status-error/5 px-2 py-1 font-mono text-[11px] text-status-error">
					{localError ?? error}
				</p>
			)}
			<div className="flex gap-2">
				<Button size="sm" variant="accent" disabled={busy} onClick={submit}>
					{busy ? "Adding…" : "Add step"}
				</Button>
				<Button size="sm" variant="gray" onClick={onCancel}>
					Cancel
				</Button>
			</div>
		</div>
	);
}
