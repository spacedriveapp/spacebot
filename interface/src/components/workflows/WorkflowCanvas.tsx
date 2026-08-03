import {useCallback, useEffect, useMemo, useRef, useState} from "react";
import {
	Background,
	BackgroundVariant,
	Controls,
	MarkerType,
	Panel,
	ReactFlow,
	ReactFlowProvider,
	useReactFlow,
	type Connection,
	type EdgeTypes,
	type NodeChange,
	type NodeTypes,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import type {
	StepBinding,
	TaskItem,
	WorkflowEdge,
	WorkflowStep,
} from "@/api/client";
import {pathBetween, wouldCycle} from "./graph";
import {layoutSteps, type NodePosition} from "./layout";
import {StepNode, type StepFlowNode} from "./StepNode";
import {DependencyEdge, type DependencyFlowEdge} from "./DependencyEdge";

const NODE_TYPES: NodeTypes = {step: StepNode};
const EDGE_TYPES: EdgeTypes = {dependency: DependencyEdge};
const FIT_VIEW = {padding: 0.22, maxZoom: 1, minZoom: 0.2};

export interface WorkflowCanvasProps {
	steps: WorkflowStep[];
	edges: WorkflowEdge[];
	bindings: StepBinding[];
	/** Steps that could not be ordered, from `orderSteps`. */
	cycle: string[];
	selectedKey: string | null;
	onSelect: (stepKey: string) => void;
	/** Omitted on a run, which is a record of what happened and not editable. */
	onAddEdge?: (parentStepKey: string, childStepKey: string) => void;
	onRemoveEdge?: (parentStepKey: string, childStepKey: string) => void;
	edgeBusy?: boolean;
	/** The server's refusal, verbatim. */
	edgeError?: string | null;
	/** Live task per step key. Present only on a run. */
	tasksByStep?: Map<string, TaskItem>;
	emptyHint?: string;
}

/**
 * The template as a graph.
 *
 * The list view sorts steps topologically, which is correct and still cannot
 * show the one thing a pipeline author most needs to check: that two steps
 * which should run at the same time actually will. In a list, a fan-out reads
 * as "review then publish"; here it reads as two boxes side by side, level with
 * each other, both fed by one arrow. That is the entire argument for the
 * canvas, and it is why layout is computed by topological depth rather than by
 * `position`.
 *
 * Positions are session-local by design. The server has no x/y — `position` is
 * display order and nothing else — so the canvas derives geometry from the
 * edges every time. A drag is honoured until reload, and "Re-layout" puts it
 * back; persisting drags would mean a saved arrangement quietly disagreeing
 * with a template someone else has since rewired.
 */
export function WorkflowCanvas(props: WorkflowCanvasProps) {
	return (
		<ReactFlowProvider>
			<CanvasInner {...props} />
		</ReactFlowProvider>
	);
}

function CanvasInner({
	steps,
	edges,
	bindings,
	cycle,
	selectedKey,
	onSelect,
	onAddEdge,
	onRemoveEdge,
	edgeBusy,
	edgeError,
	tasksByStep,
	emptyHint,
}: WorkflowCanvasProps) {
	const editable = onAddEdge != null;
	const [dragged, setDragged] = useState<Record<string, NodePosition>>({});
	const [selectedEdgeId, setSelectedEdgeId] = useState<string | null>(null);
	// A connection the client already knows will be refused. Kept separate from
	// `edgeError` so a local diagnosis is not overwritten by a stale server one.
	const [localError, setLocalError] = useState<string | null>(null);

	const computed = useMemo(() => layoutSteps(steps, edges), [steps, edges]);
	const cycleKeys = useMemo(() => new Set(cycle), [cycle]);

	const bindingCounts = useMemo(() => {
		const counts = new Map<string, number>();
		for (const binding of bindings) {
			counts.set(binding.step_key, (counts.get(binding.step_key) ?? 0) + 1);
		}
		return counts;
	}, [bindings]);

	const nodes = useMemo<StepFlowNode[]>(
		() =>
			steps.map((step) => {
				const task = tasksByStep?.get(step.step_key);
				return {
					id: step.step_key,
					type: "step" as const,
					position: dragged[step.step_key] ??
						computed.get(step.step_key) ?? {x: 0, y: 0},
					selected: step.step_key === selectedKey,
					draggable: true,
					connectable: editable,
					data: {
						step,
						bindingCount: bindingCounts.get(step.step_key) ?? 0,
						inCycle: cycleKeys.has(step.step_key),
						status: task?.status,
						taskNumber: task?.task_number,
						trouble: task
							? (task.block_reason ?? task.last_error ?? null)
							: null,
					},
				};
			}),
		[
			steps,
			dragged,
			computed,
			selectedKey,
			bindingCounts,
			cycleKeys,
			tasksByStep,
			editable,
		],
	);

	const flowEdges = useMemo<DependencyFlowEdge[]>(
		() =>
			edges.map((edge) => {
				const id = `${edge.parent_step_key}→${edge.child_step_key}`;
				const upstream = tasksByStep?.get(edge.parent_step_key);
				return {
					id,
					type: "dependency" as const,
					source: edge.parent_step_key,
					target: edge.child_step_key,
					selected: id === selectedEdgeId,
					markerEnd: {
						type: MarkerType.ArrowClosed,
						width: 13,
						height: 13,
						color:
							id === selectedEdgeId
								? "var(--color-accent)"
								: upstream?.status === "done"
									? "var(--color-status-success)"
									: "var(--color-ink-faint)",
					},
					data: {
						onRemove: onRemoveEdge,
						busy: edgeBusy,
						satisfied: upstream?.status === "done",
					},
				};
			}),
		[edges, selectedEdgeId, onRemoveEdge, edgeBusy, tasksByStep],
	);

	// Only positions are absorbed. Selection is owned by the page — the step
	// panel on the right has to agree with the highlighted box — and deletion of
	// a step is a confirmed action in that panel, not a keystroke on the canvas.
	const onNodesChange = useCallback((changes: NodeChange<StepFlowNode>[]) => {
		setDragged((previous) => {
			let next = previous;
			for (const change of changes) {
				if (change.type !== "position" || !change.position) continue;
				if (next === previous) next = {...previous};
				next[change.id] = change.position;
			}
			return next;
		});
	}, []);

	/**
	 * A dropped connection.
	 *
	 * The client refuses what it can already prove wrong, and says why in the
	 * same words the server would — the path that would close the loop. That is
	 * not a substitute for the server's 409: a second author rewiring the same
	 * template can make a connection cyclic between this client's last fetch and
	 * its request, and that refusal is shown verbatim below. It is only that
	 * making the author wait for a round trip to learn something already on
	 * screen is a worse way to say the same thing.
	 */
	const onConnect = useCallback(
		(connection: Connection) => {
			const parent = connection.source;
			const child = connection.target;
			if (!parent || !child) return;

			if (parent === child) {
				setLocalError(`\`${parent}\` cannot wait for itself.`);
				return;
			}
			if (
				edges.some(
					(edge) =>
						edge.parent_step_key === parent && edge.child_step_key === child,
				)
			) {
				setLocalError(`\`${child}\` already waits for \`${parent}\`.`);
				return;
			}
			if (wouldCycle(edges, parent, child)) {
				const loop = pathBetween(edges, child, parent);
				setLocalError(
					loop
						? `That would form a cycle: ${loop.join(" → ")} → ${child}. ` +
							`\`${parent}\` already runs after \`${child}\`.`
						: `That would form a cycle — \`${parent}\` already runs after \`${child}\`.`,
				);
				return;
			}

			setLocalError(null);
			onAddEdge?.(parent, child);
		},
		[edges, onAddEdge],
	);

	// A refusal is about the connection just attempted, so it goes stale the
	// moment the graph changes underneath it.
	useEffect(() => setLocalError(null), [edges]);

	const message = localError ?? edgeError ?? null;

	if (steps.length === 0) {
		return (
			<div className="flex h-full flex-col items-center justify-center gap-1">
				<p className="text-sm text-ink-dull">No steps yet.</p>
				<p className="text-xs text-ink-faint">
					{emptyHint ?? "A step becomes one task per launch."}
				</p>
			</div>
		);
	}

	return (
		<div className="workflow-canvas relative h-full min-h-0 w-full">
			<ReactFlow<StepFlowNode, DependencyFlowEdge>
				nodes={nodes}
				edges={flowEdges}
				nodeTypes={NODE_TYPES}
				edgeTypes={EDGE_TYPES}
				onNodesChange={onNodesChange}
				onConnect={onConnect}
				onNodeClick={(_event, node) => {
					setSelectedEdgeId(null);
					onSelect(node.id);
				}}
				onEdgeClick={(_event, edge) =>
					setSelectedEdgeId((current) => (current === edge.id ? null : edge.id))
				}
				onPaneClick={() => setSelectedEdgeId(null)}
				nodesDraggable
				nodesConnectable={editable}
				nodesFocusable
				elementsSelectable
				// Steps are deleted from the panel, behind a confirmation, because
				// deleting one takes its edges and bindings with it.
				deleteKeyCode={null}
				connectionRadius={28}
				proOptions={{hideAttribution: true}}
				fitView
				fitViewOptions={FIT_VIEW}
				minZoom={0.2}
				maxZoom={1.6}
			>
				<Background
					variant={BackgroundVariant.Dots}
					gap={18}
					size={1}
					color="var(--color-app-line)"
				/>
				<Controls
					showInteractive={false}
					className="!bottom-2 !left-2 !shadow-none"
				/>
				<AutoFit signature={steps.map((step) => step.step_key).join(",")} />
				<Panel position="top-right" className="!m-2 flex items-center gap-2">
					{Object.keys(dragged).length > 0 && (
						<button
							type="button"
							onClick={() => setDragged({})}
							title="Put every step back where the graph says it goes"
							className="rounded border border-app-line bg-app-dark-box px-1.5 py-0.5 text-[10px] text-ink-faint hover:text-ink-dull"
						>
							Re-layout
						</button>
					)}
					<span className="rounded border border-app-line bg-app-dark-box px-1.5 py-0.5 text-[10px] text-ink-faint">
						{editable
							? "Drag a handle onto another step to make it wait. Click a line to remove it."
							: "Read-only — this run already happened."}
					</span>
				</Panel>
			</ReactFlow>

			{message && (
				<p className="pointer-events-none absolute inset-x-3 bottom-3 z-10 break-words rounded border border-status-error/40 bg-app-dark-box px-2 py-1.5 text-[11px] text-status-error shadow-lg">
					{message}
				</p>
			)}
		</div>
	);
}

/**
 * Keep a newly added step on screen.
 *
 * Adding one is the moment a canvas is most likely to place a box outside the
 * viewport — a new step has no prerequisites, so it lands in the first column,
 * which may be off to the left of wherever the author had panned to. Fitting on
 * a change to the *set* of steps, and not on every render, leaves dragging and
 * editing alone.
 */
function AutoFit({signature}: {signature: string}) {
	const {fitView} = useReactFlow();
	const previous = useRef(signature);
	useEffect(() => {
		if (previous.current === signature) return;
		previous.current = signature;
		void fitView({...FIT_VIEW, duration: 200});
	}, [signature, fitView]);
	return null;
}
