import {useCallback, useMemo, useState} from "react";
import {
	DndContext,
	DragOverlay,
	KeyboardSensor,
	PointerSensor,
	closestCorners,
	useDroppable,
	useSensor,
	useSensors,
	type CollisionDetection,
	type DragEndEvent,
	type DragStartEvent,
} from "@dnd-kit/core";
import {
	SortableContext,
	sortableKeyboardCoordinates,
	useSortable,
	type SortingStrategy,
} from "@dnd-kit/sortable";
import {CSS} from "@dnd-kit/utilities";
import {Badge} from "@spacedrive/primitives";
import type {
	AgentInfo,
	TaskEdgeSummary,
	TaskItem,
	TaskPriority,
	TaskStatus,
} from "@/api/client";
import {agentsSatisfying, isPooled, isUnclaimed} from "@/lib/capabilities";
import {CapabilityChips} from "./CapabilityChips";
import {BlockKindChip} from "./BlockKindChip";
import {LoopChips} from "./LoopChips";
import {DependencyBadges} from "./DependencyBadges";
import {RepoChip, type BindingNames} from "./RepoChip";
import {columnsFor, type BoardColumn} from "./boardColumns";
import {dependencyRefusal} from "./dependencyGate";
import {
	STATUS_LABEL,
	planStatusChange,
	type TransitionTable,
} from "./taskTransitions";

/**
 * A kanban board over the task list, owned locally.
 *
 * `@spacedrive/ai` has no board component, and the two it does ship for tasks
 * have both broken on this app's sixth status in the same session — `TaskList`
 * by dropping `blocked` cards, `TaskStatusIcon` by throwing on them. So this
 * renders its own cards rather than adapting ours into shapes the design
 * system tolerates, and `blocked` gets a real column instead of the bolted-on
 * section above the list. Nothing here goes through `designSystemTask.ts`.
 *
 * Scope is rendering and drag. Editing dependency edges stays in the drawer,
 * where there is room to explain a rejection; a board has room for a status
 * and not much else.
 */
export interface TaskBoardProps {
	tasks: TaskItem[];
	/** Edge counts keyed by task number, from the list response. */
	edges: Map<number, TaskEdgeSummary>;
	bindingNames?: BindingNames;
	/** The server's legal-move table, shared with the list and the drawer. */
	transitions: TransitionTable;
	activeTaskId?: string | null;
	onTaskClick: (task: TaskItem) => void;
	/** Perform a move the board has already found acceptable. */
	onMove: (task: TaskItem, status: TaskStatus) => void;
	/**
	 * Explain a move the board turned down.
	 *
	 * The board decides, because it is the only view where a target is a place
	 * the user pointed at rather than a button they pressed — but the message
	 * belongs to the page, which already has one banner for refusals and no
	 * reason to grow a second.
	 */
	onRefuse: (task: TaskItem, status: TaskStatus, reason: string) => void;
	resolveAgentName?: (agentId: string) => string;
	/**
	 * The fleet, so a pooled card can say whether anything can actually take it.
	 *
	 * Optional because two callers render this board without an agents query at
	 * all; without it a pooled card still reads as pooled and lists what it
	 * wants, it just cannot mark which labels are the problem.
	 */
	agents?: readonly AgentInfo[];
}

/** Droppable ids are namespaced so a column is never mistaken for a card id. */
const COLUMN_PREFIX = "column:";

/**
 * Cards do not shift to preview an insertion point.
 *
 * A column's order is the store's — priority, then task number — not something
 * a drop can set. Opening a gap where the card would land would promise an
 * ordering that the next refetch silently undoes, which is worse than no
 * affordance at all. The highlighted column is the affordance, because the
 * only thing a drop decides here is *which column*.
 */
const NO_REORDER: SortingStrategy = () => null;

/**
 * Only columns are drop targets.
 *
 * `useSortable` registers every card as a droppable as well as a draggable, so
 * the default pass had cards competing with columns — and a card dropped on an
 * empty column resolved to whichever card in the *origin* column happened to
 * be nearest, which reads as "the drag did nothing". Cards mean nothing as
 * targets on this board, because a drop only ever decides a status; filtering
 * them out before the corner-distance pass is what makes an empty column
 * reachable at all.
 */
const columnCollision: CollisionDetection = (args) =>
	closestCorners({
		...args,
		droppableContainers: args.droppableContainers.filter((container) =>
			String(container.id).startsWith(COLUMN_PREFIX),
		),
	});

/**
 * Priority, keyed by the generated union for the same reason the columns are:
 * a missing case should stop the build, not paint a card with no marker.
 */
const PRIORITY_STYLE: Record<TaskPriority, {bar: string; label: string}> = {
	critical: {bar: "bg-status-error", label: "Critical"},
	high: {bar: "bg-status-warning", label: "High"},
	medium: {bar: "bg-status-info/60", label: "Medium"},
	low: {bar: "bg-app-line", label: "Low"},
};

export function TaskBoard({
	tasks,
	edges,
	bindingNames,
	transitions,
	activeTaskId,
	onTaskClick,
	onMove,
	onRefuse,
	resolveAgentName,
	agents,
}: TaskBoardProps) {
	const columns = useMemo(() => columnsFor(tasks), [tasks]);
	const [draggingId, setDraggingId] = useState<string | null>(null);

	const draggingTask = useMemo(
		() => tasks.find((task) => task.id === draggingId) ?? null,
		[tasks, draggingId],
	);

	// A drag must not fire on a click: the card is both the drag handle and the
	// button that opens the drawer, and 5px is the usual threshold between the
	// two intents.
	const sensors = useSensors(
		useSensor(PointerSensor, {activationConstraint: {distance: 5}}),
		useSensor(KeyboardSensor, {coordinateGetter: sortableKeyboardCoordinates}),
	);

	const handleDragEnd = useCallback(
		(event: DragEndEvent) => {
			setDraggingId(null);
			const {active, over} = event;
			if (!over) return;

			const task = tasks.find((candidate) => candidate.id === active.id);
			if (!task) return;

			// `columnCollision` guarantees the id is a column, so the status is
			// read straight off it rather than looked up.
			const overId = String(over.id);
			if (!overId.startsWith(COLUMN_PREFIX)) return;
			const target = overId.slice(COLUMN_PREFIX.length) as TaskStatus;

			// Dropping a card back where it started is not a mistake worth a
			// message, so it is silently nothing rather than "already there".
			if (target === task.status) return;

			// The same function that dimmed the column decides the drop, so a
			// column that looked closed cannot quietly accept a card anyway.
			const reason = refusalFor(task, target, transitions, edges);
			if (reason) {
				onRefuse(task, target, reason);
				return;
			}
			onMove(task, target);
		},
		[tasks, transitions, edges, onMove, onRefuse],
	);

	return (
		<DndContext
			sensors={sensors}
			collisionDetection={columnCollision}
			onDragStart={(event: DragStartEvent) =>
				setDraggingId(String(event.active.id))
			}
			onDragCancel={() => setDraggingId(null)}
			onDragEnd={handleDragEnd}
		>
			<div className="flex h-full gap-3 overflow-x-auto p-3">
				{columns.map((column) => (
					<Column
						key={column.status}
						column={column}
						edges={edges}
						bindingNames={bindingNames}
						activeTaskId={activeTaskId}
						onTaskClick={onTaskClick}
						resolveAgentName={resolveAgentName}
						agents={agents}
						// Only meaningful mid-drag: says whether this column would
						// accept the card currently in the air.
						rejects={
							draggingTask
								? refusalFor(draggingTask, column.status, transitions, edges)
								: null
						}
						dragging={draggingTask != null}
					/>
				))}
			</div>

			{/* Without an overlay the card stays inside its column's scroll box and
			    is clipped the moment it leaves — the drag looks broken rather than
			    blocked. */}
			<DragOverlay dropAnimation={null}>
				{draggingTask && (
					<CardBody
						task={draggingTask}
						edge={edges.get(draggingTask.task_number)}
						bindingNames={bindingNames}
						resolveAgentName={resolveAgentName}
						agents={agents}
						className="rotate-1 shadow-lg"
					/>
				)}
			</DragOverlay>
		</DndContext>
	);
}

/**
 * Why this column would refuse the dragged card, or `null` if it would take it.
 *
 * Both halves of the check, in the order a person would ask them: is the move
 * legal at all, and if it is, does the graph allow it yet. `planStatusChange`
 * is the server's own table, fetched once; `dependencyRefusal` reads the edge
 * counts that arrived with the list. Neither is re-derived here.
 */
function refusalFor(
	task: TaskItem,
	target: TaskStatus,
	transitions: TransitionTable,
	edges: Map<number, TaskEdgeSummary>,
): string | null {
	if (target === task.status) return null;
	const move = planStatusChange(task, target, transitions);
	if (move.action === "refuse") return move.reason;

	// `planStatusChange` sends every move out of `blocked` to unblock before it
	// looks at the target at all, which is right in the drawer — unblock is one
	// button there and has no destination to disagree with. A board has
	// destinations, and unblock has exactly one outcome: requeue. Without this,
	// all six columns light up for a blocked card and four of them are lies —
	// dropping one on Done would claim the work finished when it is about to be
	// handed back to a worker.
	if (move.action === "unblock" && target !== "ready") {
		return `Leaving ${STATUS_LABEL.blocked} always goes through unblock, which requeues the task — drop it on ${STATUS_LABEL.ready}. Whether it lands there or back in ${STATUS_LABEL.backlog} is decided by whether its upstream tasks are done.`;
	}

	return dependencyRefusal(task, target, edges.get(task.task_number));
}

function Column({
	column,
	edges,
	bindingNames,
	activeTaskId,
	onTaskClick,
	resolveAgentName,
	agents,
	rejects,
	dragging,
}: {
	column: BoardColumn;
	edges: Map<number, TaskEdgeSummary>;
	bindingNames?: BindingNames;
	activeTaskId?: string | null;
	onTaskClick: (task: TaskItem) => void;
	resolveAgentName?: (agentId: string) => string;
	agents?: readonly AgentInfo[];
	rejects: string | null;
	dragging: boolean;
}) {
	const {setNodeRef, isOver} = useDroppable({
		id: `${COLUMN_PREFIX}${column.status}`,
	});

	// Dimmed, but still droppable. Disabling the column would make the drop a
	// no-op and teach nothing; the refusal is the only place the user finds out
	// *which* parent is outstanding, so it has to remain reachable.
	const dimmed = dragging && rejects != null;

	return (
		<section
			ref={setNodeRef}
			title={rejects ?? undefined}
			className={`flex w-[280px] shrink-0 flex-col rounded-md border border-t-2 border-app-line bg-app-box/30 transition-opacity ${
				column.style.accent
			} ${dimmed ? "opacity-40" : ""} ${
				isOver && !dimmed ? "ring-1 ring-accent/60" : ""
			}`}
		>
			<header className="flex flex-col gap-0.5 border-b border-app-line/60 px-3 py-2">
				<div className="flex items-center gap-2">
					<span className={`h-2 w-2 shrink-0 rounded-full ${column.style.dot}`} />
					<span className="text-xs font-semibold uppercase tracking-wide text-ink">
						{column.label}
					</span>
					<span className="text-xs text-ink-faint">{column.tasks.length}</span>
				</div>
				<span className="text-[10px] leading-tight text-ink-faint">
					{column.style.hint}
				</span>
			</header>

			<SortableContext
				items={column.tasks.map((task) => task.id)}
				strategy={NO_REORDER}
			>
				<div className="flex min-h-[80px] flex-1 flex-col gap-2 overflow-y-auto p-2">
					{column.tasks.map((task) => (
						<Card
							key={task.id}
							task={task}
							edge={edges.get(task.task_number)}
							bindingNames={bindingNames}
							active={activeTaskId === task.id}
							onClick={() => onTaskClick(task)}
							resolveAgentName={resolveAgentName}
							agents={agents}
						/>
					))}
					{column.tasks.length === 0 && (
						<p className="px-1 py-2 text-[11px] text-ink-faint">Nothing here.</p>
					)}
				</div>
			</SortableContext>
		</section>
	);
}

function Card({
	task,
	edge,
	bindingNames,
	active,
	onClick,
	resolveAgentName,
	agents,
}: {
	task: TaskItem;
	edge?: TaskEdgeSummary;
	bindingNames?: BindingNames;
	active: boolean;
	onClick: () => void;
	resolveAgentName?: (agentId: string) => string;
	agents?: readonly AgentInfo[];
}) {
	const {attributes, listeners, setNodeRef, transform, transition, isDragging} =
		useSortable({id: task.id});

	return (
		<CardBody
			ref={setNodeRef}
			task={task}
			edge={edge}
			bindingNames={bindingNames}
			resolveAgentName={resolveAgentName}
			agents={agents}
			onClick={onClick}
			style={{transform: CSS.Transform.toString(transform), transition}}
			// The original stays in place as a ghost while the overlay copy moves,
			// so the column keeps its height and nothing below it jumps.
			className={`${isDragging ? "opacity-30" : ""} ${
				active ? "border-accent/60 bg-app-box" : ""
			}`}
			{...attributes}
			{...listeners}
		/>
	);
}

/**
 * The card itself, with no drag wiring — rendered both in place and in the
 * overlay, which is why it takes a ref and arbitrary props.
 */
function CardBody({
	ref,
	task,
	edge,
	bindingNames,
	resolveAgentName,
	agents,
	className,
	...rest
}: {
	ref?: React.Ref<HTMLDivElement>;
	task: TaskItem;
	edge?: TaskEdgeSummary;
	bindingNames?: BindingNames;
	resolveAgentName?: (agentId: string) => string;
	agents?: readonly AgentInfo[];
	className?: string;
} & React.HTMLAttributes<HTMLDivElement>) {
	const priority = PRIORITY_STYLE[task.priority];

	// Pooled and unclaimed is the only state with no assignee to show. Pooled
	// and *claimed* is ordinary work belonging to whoever took it — the pool is
	// how it arrived, not what it is now — so it renders exactly like a pushed
	// task and the provenance is left to the drawer.
	const awaitingClaim = isPooled(task) && isUnclaimed(task);
	const requires = task.required_capabilities ?? [];
	// Marked only when the fleet is actually known. With no agents prop, or
	// none loaded, every label would look undeclared and every pooled card
	// would turn red during a refetch.
	const undeclared =
		agents && agents.length > 0 && agentsSatisfying(requires, agents).length === 0
			? requires.filter(
					(label) =>
						!agents.some((agent) => (agent.capabilities ?? []).includes(label)),
				)
			: undefined;

	return (
		<div
			ref={ref}
			role="button"
			tabIndex={0}
			className={`cursor-grab select-none rounded border border-app-line bg-app-box/80 p-2 text-left transition-colors hover:border-app-active hover:bg-app-box active:cursor-grabbing ${
				className ?? ""
			}`}
			{...rest}
		>
			<div className="flex items-start gap-2">
				{/* Priority as a rule down the edge rather than another chip: the
				    chip row is already the busiest part of the card. */}
				<span
					title={`${priority?.label ?? task.priority} priority`}
					className={`mt-0.5 h-8 w-0.5 shrink-0 rounded-full ${
						priority?.bar ?? "bg-app-line"
					}`}
				/>
				<div className="min-w-0 flex-1">
					<div className="flex items-baseline gap-1.5">
						<span className="shrink-0 font-mono text-[10px] text-ink-faint">
							#{task.task_number}
						</span>
						<span className="line-clamp-2 text-xs font-medium leading-snug text-ink">
							{task.title}
						</span>
					</div>

					{/* Everything the list row has no width for. */}
					<div className="mt-1.5 flex flex-wrap items-center gap-1">
						<RepoChip task={task} names={bindingNames} />
						<BlockKindChip kind={task.block_kind} reason={task.block_reason} />
						<LoopChips task={task} />
						<DependencyBadges summary={edge} />
						{task.consecutive_failures > 0 && (
							<Badge variant="error" size="sm" className="shrink-0">
								{task.consecutive_failures}
								{task.max_retries ? `/${task.max_retries}` : ""} failed
							</Badge>
						)}
					</div>

					{/* Why a card is parked matters more than anything else on it, but
					    must not outshout the title — muted, clamped, full on hover.
					    `skip_reason` leads: it is the only one that explains a settled
					    card, and a skipped task captioned with a stale `last_error`
					    from an earlier attempt would name the wrong cause entirely. */}
					{(task.skip_reason ?? task.block_reason ?? task.last_error) && (
						<p
							className="mt-1 line-clamp-2 break-all font-mono text-[10px] leading-relaxed text-ink-dull"
							title={
								task.skip_reason ?? task.block_reason ?? task.last_error ?? ""
							}
						>
							{task.skip_reason ?? task.block_reason ?? task.last_error}
						</p>
					)}

					{/* Who this belongs to — or, for a pooled task nobody has taken,
					    what it asked for instead. Until capabilities existed the line
					    below simply rendered an empty string for an unassigned task,
					    so a card addressed to a capability and a card whose agent had
					    been deleted looked identical: a blank line. Both now say which
					    they are. */}
					{awaitingClaim ? (
						<CapabilityChips
							requires={requires}
							unsatisfied={undeclared}
							className="mt-1"
						/>
					) : (
						resolveAgentName && (
							<p className="mt-1 truncate text-[10px] text-ink-faint">
								{task.assigned_agent_id === "" ? (
									<span className="italic">Unassigned</span>
								) : (
									resolveAgentName(task.assigned_agent_id)
								)}
							</p>
						)
					)}
				</div>
			</div>
		</div>
	);
}
