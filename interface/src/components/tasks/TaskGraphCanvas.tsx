import {useEffect, useMemo, useRef, useState} from "react";
import {
	Background,
	BackgroundVariant,
	Controls,
	MarkerType,
	Panel,
	ReactFlow,
	ReactFlowProvider,
	useReactFlow,
	type NodeChange,
	type NodeTypes,
	type EdgeTypes,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import type {TaskGraphEdge, TaskItem} from "@/api/client";
import {
	DependencyEdge,
	type DependencyFlowEdge,
} from "@/components/workflows/DependencyEdge";
import type {NodePosition} from "@/components/workflows/layout";
import {layoutTaskGraph} from "./taskGraphLayout";
import {TaskGraphNode, type TaskGraphFlowNode} from "./TaskGraphNode";

const NODE_TYPES: NodeTypes = {task: TaskGraphNode};
const EDGE_TYPES: EdgeTypes = {dependency: DependencyEdge};
const FIT_VIEW = {padding: 0.22, maxZoom: 1, minZoom: 0.1};

export interface TaskGraphCanvasProps {
	tasks: TaskItem[];
	edges: TaskGraphEdge[];
	/** The task the graph was asked about; drawn with a ring and a badge. */
	seed: number;
	selected: number | null;
	onSelect: (taskNumber: number) => void;
}

/**
 * A task's dependency graph, drawn from the task edges themselves.
 *
 * Deliberately *not* `WorkflowCanvas` with extra props. That canvas takes
 * `WorkflowStep[]`, `WorkflowEdge[]`, bindings and a cycle list, computes its
 * layers on the template graph and expands a step into one node per task; every
 * one of those inputs is a template, and the whole point of this screen is that
 * it works when there is no template — deleted, or never there. Making steps,
 * bindings and edge editing all optional to squeeze tasks through it would have
 * meant touching the code path both existing canvases run on, to make it worse.
 * So this is a parallel component that reuses what genuinely is shared: the
 * edge renderer, the grid constants, and the `.workflow-canvas` class — which
 * is load-bearing, because React Flow stamps `light`/`dark` on its own root and
 * `@spacedrive/tokens` defines colours on `.light`. Without that class the
 * canvas renders as a white rectangle inside a dark window.
 */
export function TaskGraphCanvas(props: TaskGraphCanvasProps) {
	return (
		<ReactFlowProvider>
			<CanvasInner {...props} />
		</ReactFlowProvider>
	);
}

function CanvasInner({
	tasks,
	edges,
	seed,
	selected,
	onSelect,
}: TaskGraphCanvasProps) {
	const [dragged, setDragged] = useState<Record<string, NodePosition>>({});

	const computed = useMemo(() => layoutTaskGraph(tasks, edges), [tasks, edges]);

	const nodes = useMemo<TaskGraphFlowNode[]>(
		() =>
			tasks.map((task) => {
				const id = String(task.task_number);
				return {
					id,
					type: "task" as const,
					position: dragged[id] ??
						computed.get(task.task_number) ?? {x: 0, y: 0},
					selected: task.task_number === selected,
					draggable: true,
					connectable: false,
					data: {task, seed: task.task_number === seed},
				};
			}),
		[tasks, computed, dragged, selected, seed],
	);

	const statusByNumber = useMemo(
		() => new Map(tasks.map((task) => [task.task_number, task.status])),
		[tasks],
	);

	/**
	 * One line per edge, and only for edges with both ends on screen.
	 *
	 * A truncated walk can return an edge pointing at a task it never included;
	 * React Flow drops such an edge silently, which would be fine, except that
	 * dropping it silently is exactly what the `truncated` banner exists to stop
	 * happening. Filtering here keeps the count honest for anything that asks.
	 */
	const flowEdges = useMemo<DependencyFlowEdge[]>(() => {
		const present = new Set(tasks.map((task) => task.task_number));
		return edges
			.filter(
				(edge) =>
					present.has(edge.parent_task_number) &&
					present.has(edge.child_task_number),
			)
			.map((edge) => {
				const source = String(edge.parent_task_number);
				const target = String(edge.child_task_number);
				// An arrow is drawn satisfied once its *parent* is done, so a fan-in
				// mid-flight shows the finished branches solid and the rest faint.
				const done = statusByNumber.get(edge.parent_task_number) === "done";
				return {
					id: `${source}→${target}`,
					type: "dependency" as const,
					source,
					target,
					markerEnd: {
						type: MarkerType.ArrowClosed,
						width: 13,
						height: 13,
						color: done
							? "var(--color-status-success)"
							: "var(--color-ink-faint)",
					},
					// No `onRemove`: this canvas is a view of the graph, and an edge is
					// removed from the drawer's Dependencies panel, which can explain a
					// refusal properly.
					data: {satisfied: done},
				};
			});
	}, [edges, tasks, statusByNumber]);

	// Only positions are absorbed. Selection belongs to the page, because the
	// panel beside the canvas has to agree with the highlighted box.
	const onNodesChange = (changes: NodeChange<TaskGraphFlowNode>[]) => {
		setDragged((previous) => {
			let next = previous;
			for (const change of changes) {
				if (change.type !== "position" || !change.position) continue;
				if (next === previous) next = {...previous};
				next[change.id] = change.position;
			}
			return next;
		});
	};

	return (
		<div className="workflow-canvas relative h-full min-h-0 w-full">
			<ReactFlow<TaskGraphFlowNode, DependencyFlowEdge>
				nodes={nodes}
				edges={flowEdges}
				nodeTypes={NODE_TYPES}
				edgeTypes={EDGE_TYPES}
				onNodesChange={onNodesChange}
				onNodeClick={(_event, node) => onSelect(Number(node.id))}
				nodesDraggable
				nodesConnectable={false}
				nodesFocusable
				elementsSelectable
				deleteKeyCode={null}
				proOptions={{hideAttribution: true}}
				fitView
				fitViewOptions={FIT_VIEW}
				minZoom={0.1}
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
				{/* Re-fit when the *set* of tasks changes — walking to a neighbour's
				    graph can bring in nodes well outside the current viewport — and
				    never on an ordinary re-render, which would fight a drag. */}
				<AutoFit
					signature={tasks.map((task) => task.task_number).join(",")}
				/>
				{Object.keys(dragged).length > 0 && (
					<Panel position="top-right" className="!m-2">
						<button
							type="button"
							onClick={() => setDragged({})}
							title="Put every task back where the graph says it goes"
							className="rounded border border-app-line bg-app-dark-box px-1.5 py-0.5 text-[10px] text-ink-faint hover:text-ink-dull"
						>
							Re-layout
						</button>
					</Panel>
				)}
			</ReactFlow>
		</div>
	);
}

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
