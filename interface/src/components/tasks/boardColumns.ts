import type {TaskItem, TaskStatus} from "@/api/client";
import {STATUS_LABEL} from "./taskTransitions";

/**
 * The board's columns — one per status the API can actually return.
 *
 * The set is keyed by the generated `TaskStatus` union rather than written out
 * as an array, because a hand-written array is the exact bug this board exists
 * to replace. `@spacedrive/ai`'s `TaskList` holds five statuses in a const,
 * builds a bucket per entry, and drops any task whose status has no bucket —
 * which is why `blocked` vanished from the board and why
 * `BlockedTasksSection` had to be written. Declaring this as
 * `Record<TaskStatus, ColumnStyle>` turns the same omission into a compile
 * error: regenerate `schema.d.ts` with a seventh status and `tsc` fails here
 * until a column exists for it.
 *
 * Type checking only binds at build time, though, and the server is deployed
 * separately from the bundle. So `columnsFor` *also* appends a column for any
 * status this build has never heard of. A newer server can make the board look
 * unstyled; it can never make a card disappear.
 *
 * Labels come from `STATUS_LABEL` rather than being repeated here — two label
 * tables for six statuses is how the drawer and the board end up disagreeing
 * about what `pending_approval` is called.
 */
export interface ColumnStyle {
	/** Tint for the column header rule and the card's left edge. */
	accent: string;
	/** Dot beside the header, so the column reads at a glance while dragging. */
	dot: string;
	/** What being in this queue means. Columns are otherwise just nouns. */
	hint: string;
}

/**
 * Insertion order is the column order: JS preserves it for non-numeric string
 * keys, and `COLUMN_ORDER` below relies on that. The sequence is the lifecycle
 * a card walks, with `blocked` sitting after `in_progress` because that is
 * where cards fall out of the flow — not at the end next to `done`, which
 * would read as an outcome rather than an interruption.
 */
const COLUMN_STYLE: Record<TaskStatus, ColumnStyle> = {
	pending_approval: {
		accent: "border-t-status-warning/60",
		dot: "bg-status-warning",
		hint: "Filed, awaiting a human's yes",
	},
	backlog: {
		accent: "border-t-app-line",
		dot: "bg-ink-faint",
		hint: "Accepted, waiting its turn",
	},
	ready: {
		accent: "border-t-status-info/60",
		dot: "bg-status-info",
		hint: "Claimable by an agent now",
	},
	in_progress: {
		accent: "border-t-accent/60",
		dot: "bg-accent",
		hint: "A worker holds this",
	},
	blocked: {
		accent: "border-t-status-error/60",
		dot: "bg-status-error",
		hint: "Stuck — not picked up automatically",
	},
	done: {
		accent: "border-t-status-success/60",
		dot: "bg-status-success",
		hint: "Terminal",
	},
	// Terminal like `done`, and deliberately not styled like it. A skipped task
	// is settled — nothing downstream is waiting on it — but it never ran, and a
	// board that congratulates you for work that did not happen is lying.
	//
	// The dot is hollow rather than filled, which is the one thing no other
	// column's is. `backlog` is also a grey dot and means very nearly the
	// opposite — waiting its turn, as against never getting one — and two
	// identical grey dots is the same one-visual-two-meanings bug conditions
	// exist to remove, reintroduced in the legend.
	skipped: {
		accent: "border-t-app-line",
		dot: "border border-ink-dull bg-transparent",
		hint: "A condition ruled this out — it will never run",
	},
};

/** Every styled status, in lifecycle order. */
export const COLUMN_ORDER = Object.keys(COLUMN_STYLE) as TaskStatus[];

/**
 * Columns that earn their width only when they have something in them.
 *
 * Every other column is a place a card can be *put*: an empty `Ready` is a drop
 * target and a standing invitation, so it is worth 280px of a board that has to
 * scroll at seven columns anyway. `skipped` is neither. There is no transition
 * into it — the poller settles a task when a routing condition answers no, and
 * there is deliberately no un-skip — so an empty Skipped column is not an
 * invitation to anything. It is a permanent 280px of chrome on the six boards
 * here that have no branching at all, pushing `done` off the edge of the
 * viewport to advertise a state that will never occur.
 *
 * Folding it in beside `done` was the alternative and is worse: "we did it" and
 * "we ruled it out" are exactly the two meanings that must not share a label,
 * and the design this implements exists because that conflation has already
 * caused three incidents in this codebase. So it keeps its own column and its
 * own name, and simply is not drawn until there is something to draw.
 */
const HIDDEN_WHEN_EMPTY: ReadonlySet<TaskStatus> = new Set<TaskStatus>([
	"skipped",
]);

/** Fallback for a status a newer server knows and this build does not. */
const UNKNOWN_STYLE: ColumnStyle = {
	accent: "border-t-app-line",
	dot: "bg-ink-faint",
	hint: "Unrecognised status — this page is older than the server",
};

/**
 * The style for one status, wherever a status needs rendering.
 *
 * Shared rather than re-tabulated because `@spacedrive/ai`'s `TaskStatusIcon`
 * *throws* on a status it does not know — it indexes a five-entry map and calls
 * a method on the result — so every surface outside the board that shows a
 * status has to bring its own table or crash on `blocked`. Bringing its own is
 * how the board and the drawer end up disagreeing; this keeps one table and one
 * fallback, and neither can throw.
 */
export function styleFor(status: TaskStatus): ColumnStyle {
	return COLUMN_STYLE[status] ?? UNKNOWN_STYLE;
}

export interface BoardColumn {
	status: TaskStatus;
	label: string;
	style: ColumnStyle;
	tasks: TaskItem[];
	/** True when the status arrived from the server with no column defined. */
	unrecognised: boolean;
}

/**
 * Bucket tasks into columns, inventing a column for anything unexpected.
 *
 * Cards keep the order the list endpoint sent them in, which is the store's
 * own ordering. The board deliberately does not re-sort: a second sort here
 * would mean the board and the list disagree about what is "next".
 */
export function columnsFor(tasks: TaskItem[]): BoardColumn[] {
	const buckets = new Map<TaskStatus, TaskItem[]>(
		COLUMN_ORDER.map((status) => [status, []]),
	);

	for (const task of tasks) {
		const bucket = buckets.get(task.status);
		// The one line that separates this from the component it replaces: an
		// unknown status creates its bucket instead of falling on the floor.
		if (bucket) bucket.push(task);
		else buckets.set(task.status, [task]);
	}

	return [...buckets.entries()]
		.filter(
			([status, columnTasks]) =>
				columnTasks.length > 0 || !HIDDEN_WHEN_EMPTY.has(status),
		)
		.map(([status, columnTasks]) => ({
			status,
			label: STATUS_LABEL[status] ?? status,
			style: COLUMN_STYLE[status] ?? UNKNOWN_STYLE,
			tasks: columnTasks,
			unrecognised: COLUMN_STYLE[status] == null,
		}));
}
