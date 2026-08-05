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
	type EdgeChange,
	type EdgeTypes,
	type NodeChange,
	type NodeTypes,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import type {
	StepBinding,
	StepGate,
	TaskItem,
	WorkflowEdge,
	WorkflowStep,
} from "@/api/client";
import {
	DISPOSITION_HINT,
	deriveStepDisposition,
	describeCondition,
} from "./conditions";
import {pathBetween, runNodes, wouldCycle} from "./graph";
import {layoutSteps, loopRegions, type NodePosition} from "./layout";
import {
	bodyByStepKey,
	describePredicate,
	finalResolution,
	loopBodies,
	sortPasses,
} from "./loops";
import {
	EXHAUSTED_HANDLE,
	StepNode,
	type NodeCondition,
	type StepFlowNode,
} from "./StepNode";
import {LoopGroupNode, type LoopGroupFlowNode} from "./LoopGroupNode";
import {DependencyEdge, type DependencyFlowEdge} from "./DependencyEdge";

const NODE_TYPES: NodeTypes = {step: StepNode, loopGroup: LoopGroupNode};
const EDGE_TYPES: EdgeTypes = {dependency: DependencyEdge};
const FIT_VIEW = {padding: 0.22, maxZoom: 1, minZoom: 0.2};

type CanvasNode = StepFlowNode | LoopGroupFlowNode;

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
	/**
	 * Omitted on a run, which is a record of what happened and not editable.
	 *
	 * `kind` is which arm out of a loop the author dragged from — see
	 * `onConnect`. Anything other than a loop exit only ever sends `normal`.
	 */
	onAddEdge?: (
		parentStepKey: string,
		childStepKey: string,
		kind: "normal" | "on_exhausted",
	) => void;
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
	/**
	 * The template's conditions. A step with one may never run, and the canvas
	 * has to say so — otherwise a branching template is drawn identically to a
	 * linear one and the whole shape of it is invisible until a run proves it.
	 */
	gates?: StepGate[];
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
	gates,
	emptyHint,
}: WorkflowCanvasProps) {
	const editable = onAddEdge != null;
	const [dragged, setDragged] = useState<Record<string, NodePosition>>({});
	const [selectedEdgeId, setSelectedEdgeId] = useState<string | null>(null);
	// A connection the client already knows will be refused. Kept separate from
	// `edgeError` so a local diagnosis is not overwritten by a stale server one.
	const [localError, setLocalError] = useState<string | null>(null);
	// Off by default: passes are sequential, and drawing them side by side is a
	// picture of a fan-out. On when someone wants each pass as its own box.
	const [expandLoops, setExpandLoops] = useState(false);

	const bodies = useMemo(() => loopBodies(steps, edges), [steps, edges]);
	const bodyOf = useMemo(() => bodyByStepKey(bodies), [bodies]);
	const hasLoops = bodies.size > 0;

	// One entry per box on screen. On the editor that is one per step; on a run
	// it is one per task, except a loop body step, whose passes collapse into one
	// — they happened one after another, not at once.
	const graphNodes = useMemo(
		() => runNodes(steps, tasksByStep, {expandLoops}),
		[steps, tasksByStep, expandLoops],
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

	/** Steps with an `on_exhausted` edge leaving them that is not a loop's exit. */
	const strayExhausted = useMemo(() => {
		const stray = new Set<string>();
		for (const edge of edges) {
			if (edge.kind !== "on_exhausted") continue;
			const body = bodyOf.get(edge.parent_step_key);
			if (body?.exit?.step_key !== edge.parent_step_key) {
				stray.add(edge.parent_step_key);
			}
		}
		return stray;
	}, [edges, bodyOf]);

	/** step key → which arms already leave it, so a wired handle stops shouting. */
	const armsWired = useMemo(() => {
		const map = new Map<string, {normal: boolean; exhausted: boolean}>();
		for (const edge of edges) {
			const entry = map.get(edge.parent_step_key) ?? {
				normal: false,
				exhausted: false,
			};
			if (edge.kind === "on_exhausted") entry.exhausted = true;
			else entry.normal = true;
			map.set(edge.parent_step_key, entry);
		}
		return map;
	}, [edges]);

	/** step key → how many passes it ran, however the canvas is drawing them. */
	const passTotals = useMemo(() => {
		const map = new Map<string, number>();
		if (!tasksByStep) return map;
		for (const [key, tasks] of tasksByStep) {
			const passes = tasks.filter((task) => task.loop_iteration != null).length;
			if (passes > 0) map.set(key, passes);
		}
		return map;
	}, [tasksByStep]);

	/**
	 * step key → the conditions on it, in the form the node draws.
	 *
	 * The disposition is resolved here rather than on the node: deriving it needs
	 * the edges, and a node is deliberately given plain values so React Flow can
	 * keep the node type referentially stable.
	 */
	const conditionsByStep = useMemo(() => {
		const map = new Map<string, NodeCondition[]>();
		for (const gate of gates ?? []) {
			const derived = deriveStepDisposition(
				gate.kind,
				gate.source_step_key,
				gate.step_key,
				edges,
			);
			const disposition = gate.disposition ?? derived.disposition;
			const list = map.get(gate.step_key) ?? [];
			list.push({
				text: describeCondition(gate),
				disposition,
				hint: `${describeCondition(gate)}\n\n${DISPOSITION_HINT[disposition]}${
					gate.disposition == null ? `\n\nDerived: ${derived.because}.` : ""
				}`,
			});
			map.set(gate.step_key, list);
		}
		return map;
	}, [gates, edges]);

	/** step key → how its loop came out on this run, once it has. */
	const resolutionByStep = useMemo(() => {
		const map = new Map<string, ReturnType<typeof finalResolution>>();
		if (!tasksByStep) return map;
		for (const [key, tasks] of tasksByStep) {
			// Sorted, because `finalResolution` reads the last pass and map order is
			// insertion order, not iteration order.
			const resolution = finalResolution(sortPasses(tasks));
			if (resolution) map.set(key, resolution);
		}
		return map;
	}, [tasksByStep]);

	const stepNodes = useMemo<StepFlowNode[]>(
		() =>
			graphNodes.flatMap(({id, stepKey, task, passes, collapsedLoop}) => {
				const step = stepsByKey.get(stepKey);
				if (!step) return [];
				const body = bodyOf.get(stepKey);
				const held =
					task?.awaiting_loop_group && task.awaiting_loop_arm
						? {group: task.awaiting_loop_group, arm: task.awaiting_loop_arm}
						: null;
				return [
					{
						id,
						type: "step" as const,
						position: dragged[id] ?? computed.get(id) ?? {x: 0, y: 0},
						selected: id === selectedKey,
						draggable: true,
						connectable: editable,
						zIndex: 1,
						data: {
							step,
							bindingCount: bindingCounts.get(stepKey) ?? 0,
							inCycle: cycleKeys.has(stepKey),
							status: task?.status,
							taskNumber: task?.task_number,
							// `skip_reason` is first because it is the only one of the three
							// that explains a *settled* task. A skipped task with a stale
							// `last_error` from an earlier attempt would otherwise be
							// labelled with the wrong story entirely.
							trouble: task
								? (task.skip_reason ??
									task.block_reason ??
									task.last_error ??
									null)
								: null,
							title: task ? withoutBranchSuffix(task) : undefined,
							branchKey: task?.fan_out_branch_key ?? null,
							placeholder: task?.fan_out_placeholder ?? false,
							loopGroup: step.loop_group ?? null,
							isLoopExit: body?.exit?.step_key === stepKey,
							editable,
							armWired: armsWired.get(stepKey),
							strayExhausted: strayExhausted.has(stepKey),
							// Only the collapsed box speaks for the whole history. An
							// expanded pass is one task and says which iteration it is
							// via its own loop_iteration.
							pass: collapsedLoop
								? {
										index: task?.loop_iteration ?? passes.length,
										total: passes.length,
									}
								: task?.loop_iteration != null
									? {
											index: task.loop_iteration,
											// The step's own pass count, not this box's — expanded,
											// each box is one pass, and "pass 2 of 2" on the middle
											// of three would be a lie the collapsed view does not
											// tell.
											total: passTotals.get(stepKey) ?? task.loop_iteration,
										}
									: null,
							resolution: collapsedLoop
								? finalResolution(passes)
								: (task?.loop_resolution ?? null),
							heldArm: held,
							conditions: conditionsByStep.get(stepKey),
						},
					},
				];
			}),
		[
			graphNodes,
			stepsByKey,
			bodyOf,
			dragged,
			computed,
			selectedKey,
			bindingCounts,
			cycleKeys,
			strayExhausted,
			armsWired,
			passTotals,
			conditionsByStep,
			editable,
		],
	);

	/**
	 * The box drawn behind each loop body.
	 *
	 * Derived from where the step nodes actually are, so a drag takes the region
	 * with it rather than leaving a rectangle asserting a grouping that is no
	 * longer on screen.
	 */
	const groupNodes = useMemo<LoopGroupFlowNode[]>(() => {
		if (bodies.size === 0) return [];
		const placed = new Map<string, NodePosition>();
		for (const node of stepNodes) placed.set(node.id, node.position);
		const idsByGroup = new Map<string, string[]>();
		for (const body of bodies.values()) {
			idsByGroup.set(
				body.group,
				body.members.flatMap(
					(member) => nodesByStep.get(member.step_key) ?? [member.step_key],
				),
			);
		}
		return loopRegions(placed, idsByGroup).flatMap((region) => {
			const body = bodies.get(region.group);
			if (!body) return [];
			const exitKey = body.exit?.step_key;
			const passes = exitKey ? (tasksByStep?.get(exitKey) ?? []) : [];
			const latest = passes.reduce(
				(best, task) => Math.max(best, task.loop_iteration ?? 0),
				0,
			);
			return [
				{
					id: `loop:${region.group}`,
					type: "loopGroup" as const,
					position: {x: region.x, y: region.y},
					draggable: false,
					selectable: false,
					connectable: false,
					focusable: false,
					// Behind the steps and behind the edges. A translucent panel over
					// the arrows would dim exactly the wiring it is meant to explain.
					zIndex: -1,
					style: {pointerEvents: "none" as const},
					data: {
						group: region.group,
						maxIterations: body.maxIterations,
						condition: describePredicate(body.until),
						problem:
							body.exitCandidates.length === 1
								? null
								: body.exitCandidates.length === 0
									? `no exit step — every step in this body waits on another one in it, so nothing decides whether to go round again`
									: `no single exit step — ${body.exitCandidates.join(", ")} all qualify, so nothing decides whether to go round again`,
						pass:
							latest > 0 ? {index: latest, total: body.maxIterations} : null,
						resolution: exitKey
							? (resolutionByStep.get(exitKey) ?? null)
							: null,
						width: region.width,
						height: region.height,
					},
				},
			];
		});
	}, [bodies, stepNodes, nodesByStep, tasksByStep, resolutionByStep]);

	// Regions first so React Flow mounts them under everything, `zIndex` doing
	// the rest.
	const nodes = useMemo<CanvasNode[]>(
		() => [...groupNodes, ...stepNodes],
		[groupNodes, stepNodes],
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
				const exhausted = edge.kind === "on_exhausted";
				const parentBody = bodyOf.get(edge.parent_step_key);
				const fromExit =
					parentBody?.exit?.step_key === edge.parent_step_key;
				const resolution = resolutionByStep.get(edge.parent_step_key) ?? null;
				// Which arm the run actually took. `iterated` is not a verdict on the
				// loop as a whole, so it leaves both arms alone.
				const notTaken =
					resolution === "converged"
						? exhausted
						: resolution === "exhausted_routed" ||
							  resolution === "exhausted_blocked"
							? !exhausted
							: false;
				return sources.flatMap((source) =>
					targets.flatMap((target) => {
						// Expanded, one pass of a body only ever feeds the same pass of
						// the next step in it. The cross product is right for a fan-out,
						// where every branch really does feed the fan-in, and wrong here:
						// it would draw pass 1's draft handing work to pass 3's review.
						const from = tasksByNode.get(source);
						const to = tasksByNode.get(target);
						if (
							from?.loop_iteration != null &&
							to?.loop_iteration != null &&
							from.loop_group === to.loop_group &&
							from.loop_iteration !== to.loop_iteration
						) {
							return [];
						}
						const id = `${source}→${target}`;
						// The arrow is drawn satisfied when *this* end of it is settled, so
						// a fan-out mid-flight shows finished branches feeding the fan-in
						// in solid and the unfinished ones still faint.
						//
						// Settled, not done: a skipped parent satisfies its edge exactly
						// as a finished one does — that is the whole of "settled instead of
						// done" the scheduler now runs on. Drawing it faint would show a
						// child that is already free to run as still waiting.
						const sourceStatus = tasksByNode.get(source)?.status;
						const done = sourceStatus === "done" || sourceStatus === "skipped";
						return [{
							id,
							type: "dependency" as const,
							source,
							target,
							// Every step node names its source handles, so an edge that
							// left this off would have nowhere to start from.
							sourceHandle: exhausted ? EXHAUSTED_HANDLE : "normal",
							selected: id === selectedEdgeId,
							zIndex: 0,
							markerEnd: {
								type: MarkerType.ArrowClosed,
								width: 13,
								height: 13,
								color:
									id === selectedEdgeId
										? "var(--color-accent)"
										: exhausted
											? "var(--color-status-warning)"
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
								exhausted,
								convergedArm: fromExit && !exhausted,
								notTaken,
							},
						}];
					}),
				);
			}),
		[
			edges,
			nodesByStep,
			tasksByNode,
			bodyOf,
			resolutionByStep,
			selectedEdgeId,
			onRemoveEdge,
			edgeBusy,
		],
	);

	// Only positions are absorbed into canvas state. Selection is owned by the
	// page — the step panel on the right has to agree with the highlighted box
	// — and deletion of a step is a confirmed action in that panel, not a
	// keystroke on the canvas. Selection changes still have to be *read* here:
	// Enter or Space on a focused node arrives as a select change, not an
	// onNodeClick, so without this the canvas is mouse-only.
	const onNodesChange = useCallback(
		(changes: NodeChange<CanvasNode>[]) => {
			setDragged((previous) => {
				let next = previous;
				for (const change of changes) {
					if (change.type !== "position" || !change.position) continue;
					if (next === previous) next = {...previous};
					next[change.id] = change.position;
				}
				return next;
			});
			for (const change of changes) {
				if (change.type !== "select" || !change.selected) continue;
				// A loop's group box is a bracket, not a step — the click handler
				// refuses it, and the keyboard path has to agree.
				const node = nodes.find((candidate) => candidate.id === change.id);
				if (!node || node.type === "loopGroup") continue;
				setSelectedEdgeId(null);
				onSelect(change.id);
				break;
			}
		},
		[nodes, onSelect],
	);

	// Same story for edges: a focused edge emits select changes on Enter, and
	// selectedEdgeId is what onEdgeClick sets. A click emits the change first
	// and then fires onEdgeClick, so both paths must be idempotent — a toggle
	// here would cancel the click's own toggle. Deselecting is Escape, or a
	// click on the pane.
	const onEdgesChange = useCallback(
		(changes: EdgeChange<DependencyFlowEdge>[]) => {
			for (const change of changes) {
				if (change.type !== "select") continue;
				setSelectedEdgeId((current) => {
					if (change.selected) return change.id;
					return current === change.id ? null : current;
				});
				break;
			}
		},
		[],
	);

	/**
	 * A dropped connection.
	 *
	 * The handle it came from decides the kind. A loop's exit step has two, one
	 * for converging and one for giving up, so the author has already said which
	 * path they mean by the time the line lands — there is no dropdown afterwards
	 * and no default to get wrong.
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
			const kind =
				connection.sourceHandle === EXHAUSTED_HANDLE
					? "on_exhausted"
					: "normal";

			if (parent === child) {
				setLocalError(`\`${parent}\` cannot wait for itself.`);
				return;
			}
			const existing = edges.find(
				(edge) =>
					edge.parent_step_key === parent && edge.child_step_key === child,
			);
			if (existing) {
				setLocalError(
					existing.kind === kind
						? `\`${child}\` already waits for \`${parent}\`${
								kind === "on_exhausted" ? " on the give-up arm" : ""
							}.`
						: `\`${child}\` already leaves \`${parent}\` on the ${
								existing.kind === "on_exhausted" ? "give-up" : "ordinary"
							} arm. Remove that edge before drawing the other one — one pair of steps is one edge, and the two arms have to lead somewhere different to mean anything.`,
				);
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
			// The server only refuses this at launch, by which point the template
			// has been saved and looks fine. Saying so at the moment the line lands
			// is the difference between a typo and a trap.
			if (kind === "on_exhausted") {
				const body = bodyOf.get(parent);
				if (body?.exit?.step_key !== parent) {
					setLocalError(
						`\`${parent}\` is not the exit step of a loop, so it can never run out of attempts — only a loop's exit step can have a give-up edge.`,
					);
					return;
				}
			}

			setLocalError(null);
			onAddEdge?.(parent, child, kind);
		},
		[edges, bodyOf, onAddEdge],
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
			<ReactFlow<CanvasNode, DependencyFlowEdge>
				nodes={nodes}
				edges={flowEdges}
				nodeTypes={NODE_TYPES}
				edgeTypes={EDGE_TYPES}
				onNodesChange={onNodesChange}
				onEdgesChange={onEdgesChange}
				onConnect={onConnect}
				onNodeClick={(_event, node) => {
					if (node.type === "loopGroup") return;
					setSelectedEdgeId(null);
					onSelect(node.id);
				}}
				onEdgeClick={(_event, edge) => setSelectedEdgeId(edge.id)}
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
					{!editable && hasLoops && (
						<button
							type="button"
							onClick={() => setExpandLoops((open) => !open)}
							title={
								expandLoops
									? "Collapse each loop back to one box per step. Passes ran one after another, so one box is the honest shape."
									: "Draw every pass as its own box. They ran in sequence, not at once — this is a history laid out sideways."
							}
							className="rounded border border-app-line bg-app-dark-box px-1.5 py-0.5 text-[10px] text-ink-faint hover:text-ink-dull"
						>
							{expandLoops ? "Collapse passes" : "Expand passes"}
						</button>
					)}
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
							? hasLoops
								? "Drag a handle onto another step to make it wait. A loop's exit step has two: converged, and gave up. Click a line to remove it."
								: "Drag a handle onto another step to make it wait. Click a line to remove it."
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
 *
 * A loop's later passes are titled `… (iteration 2)` for the same reason and
 * stripped here for the same one: the pass counter already says it.
 */
function withoutBranchSuffix(task: TaskItem): string {
	let title = task.title;
	const branch = task.fan_out_branch_key;
	if (branch) {
		const suffix = ` [${branch}]`;
		if (title.endsWith(suffix)) title = title.slice(0, -suffix.length);
	}
	if (task.loop_iteration != null) {
		title = title.replace(/\s*\(iteration \d+\)$/, "");
	}
	return title;
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
