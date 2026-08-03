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
};

/** Every styled status, in lifecycle order. */
export const COLUMN_ORDER = Object.keys(COLUMN_STYLE) as TaskStatus[];

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

	return [...buckets.entries()].map(([status, columnTasks]) => ({
		status,
		label: STATUS_LABEL[status] ?? status,
		style: COLUMN_STYLE[status] ?? UNKNOWN_STYLE,
		tasks: columnTasks,
		unrecognised: COLUMN_STYLE[status] == null,
	}));
}
