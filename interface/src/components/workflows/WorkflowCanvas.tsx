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
import {pathBetween, runNodes, wouldCycle} from "./graph";
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
	/**
	 * The selected node's id.
	 *
	 * A step key on the editor, where one step is one box. On a run it is a
	 * node id from `runNodes` — a fan-out's three branches are three boxes and
	 * selecting one has to mean *that* branch, not the step all three came from.
	 */
	selectedKey: string | null;
	onSelect: (nodeId: string) => void;
	/** Omitted on a run, which is a record of what happened and not editable. */
	onAddEdge?: (parentStepKey: string, childStepKey: string) => void;
	onRemoveEdge?: (parentStepKey: string, childStepKey: string) => void;
	edgeBusy?: boolean;
	/** The server's refusal, verbatim. */
	edgeError?: string | null;
	/**
	 * Live tasks per step key. Present only on a run.
	 *
	 * A list rather than a task, because a step that declares `for_each_step_key`
	 * compiles into one task *per item* an upstream step produced. Keying by step
	 * and keeping the last one drew a three-way fan-out as a single node.
	 */
	tasksByStep?: Map<string, TaskItem[]>;
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

	// One entry per box on screen. On the editor that is one per step; on a run
	// it is one per task, so an expanded fan-out is as wide as it really was.
	const graphNodes = useMemo(
		() => runNodes(steps, tasksByStep),
		[steps, tasksByStep],
	);

	/** step key → the node ids it expanded into, in the order they stack. */
	const nodesByStep = useMemo(() => {
		const map = new Map<string, string[]>();
		for (const node of graphNodes) {
			const list = map.get(node.stepKey);
			if (list) list.push(node.id);
			else map.set(node.stepKey, [node.id]);
		}
		return map;
	}, [graphNodes]);

	/** node id → its task, for edges that need to know if their source finished. */
	const tasksByNode = useMemo(() => {
		const map = new Map<string, TaskItem>();
		for (const node of graphNodes) {
			if (node.task) map.set(node.id, node.task);
		}
		return map;
	}, [graphNodes]);

	const computed = useMemo(
		() => layoutSteps(steps, edges, nodesByStep),
		[steps, edges, nodesByStep],
	);
	const cycleKeys = useMemo(() => new Set(cycle), [cycle]);

	const bindingCounts = useMemo(() => {
		const counts = new Map<string, number>();
		for (const binding of bindings) {
			counts.set(binding.step_key, (counts.get(binding.step_key) ?? 0) + 1);
		}
		return counts;
	}, [bindings]);

	const stepsByKey = useMemo(
		() => new Map(steps.map((step) => [step.step_key, step])),
		[steps],
	);

	const nodes = useMemo<StepFlowNode[]>(
		() =>
			graphNodes.flatMap(({id, stepKey, task}) => {
				const step = stepsByKey.get(stepKey);
				if (!step) return [];
				return [
					{
						id,
						type: "step" as const,
						position: dragged[id] ?? computed.get(id) ?? {x: 0, y: 0},
						selected: id === selectedKey,
						draggable: true,
						connectable: editable,
						data: {
							step,
							bindingCount: bindingCounts.get(stepKey) ?? 0,
							inCycle: cycleKeys.has(stepKey),
							status: task?.status,
							taskNumber: task?.task_number,
							trouble: task
								? (task.block_reason ?? task.last_error ?? null)
								: null,
							title: task ? withoutBranchSuffix(task) : undefined,
							branchKey: task?.fan_out_branch_key ?? null,
							placeholder: task?.fan_out_placeholder ?? false,
						},
					},
				];
			}),
		[
			graphNodes,
			stepsByKey,
			dragged,
			computed,
			selectedKey,
			bindingCounts,
			cycleKeys,
			editable,
		],
	);

	/**
	 * Template edges, fanned across the tasks at each end.
	 *
	 * The run response carries no task-level edge list, and it does not need to:
	 * expansion gave every branch the edges its step declared, so the cross
	 * product of one template edge over both endpoints' tasks *is* the graph in
	 * the database. `scan → audit` becomes one arrow into each branch, and
	 * `audit → report` one out of each, which is the shape of a fan-out drawn
	 * honestly. Off a run every step has exactly one node and this collapses back
	 * to one edge each, unchanged.
	 */
	const flowEdges = useMemo<DependencyFlowEdge[]>(
		() =>
			edges.flatMap((edge) => {
				const sources = nodesByStep.get(edge.parent_step_key) ?? [];
				const targets = nodesByStep.get(edge.child_step_key) ?? [];
				return sources.flatMap((source) =>
					targets.map((target) => {
						const id = `${source}→${target}`;
						// The arrow is drawn satisfied when *this* end of it is done, so a
						// fan-out mid-flight shows finished branches feeding the fan-in in
						// solid and the unfinished ones still faint.
						const done = tasksByNode.get(source)?.status === "done";
						return {
							id,
							type: "dependency" as const,
							source,
							target,
							selected: id === selectedEdgeId,
							markerEnd: {
								type: MarkerType.ArrowClosed,
								width: 13,
								height: 13,
								color:
									id === selectedEdgeId
										? "var(--color-accent)"
										: done
											? "var(--color-status-success)"
											: "var(--color-ink-faint)",
							},
							data: {
								onRemove: onRemoveEdge,
								parentStepKey: edge.parent_step_key,
								childStepKey: edge.child_step_key,
								busy: edgeBusy,
								satisfied: done,
							},
						};
					}),
				);
			}),
		[edges, nodesByStep, tasksByNode, selectedEdgeId, onRemoveEdge, edgeBusy],
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
				{/* Node ids, not step keys: a fan-out expanding adds boxes without
				    touching the template, and those are exactly the boxes most likely
				    to land outside the viewport. */}
				<AutoFit signature={graphNodes.map((node) => node.id).join(",")} />
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
 * The task's title without the branch suffix the compiler appended.
 *
 * Expansion names a branch's task `Audit the repository [grimoire]` so it is
 * identifiable on the board, where there is no column of siblings to compare it
 * against. On the canvas there is, and the branch key is already a badge, so
 * repeating it in the title spends the one line of headroom a node has on a
 * word the reader has just read.
 */
function withoutBranchSuffix(task: TaskItem): string {
	const branch = task.fan_out_branch_key;
	if (!branch) return task.title;
	const suffix = ` [${branch}]`;
	return task.title.endsWith(suffix)
		? task.title.slice(0, -suffix.length)
		: task.title;
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
