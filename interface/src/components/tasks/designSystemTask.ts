import type { TaskItem, TaskStatus } from "@/api/client";

/**
 * Adapter for handing our tasks to `@spacedrive/ai` components.
 *
 * The design system knows five statuses — `pending_approval`, `backlog`,
 * `ready`, `in_progress`, `done` — and handles a sixth badly in two different
 * ways. `TaskList` builds a bucket per known status and drops any task whose
 * status has no bucket, so a `blocked` task silently vanishes from the board.
 * `TaskStatusIcon` is worse: it destructures `config[status]` with no fallback,
 * so a `blocked` task throws and takes the drawer down with it.
 *
 * Until the design system learns the status, the boundary is adapted here
 * rather than in each call site, so there is one place to delete when it does.
 */

/** Statuses `@spacedrive/ai` can actually render. */
export type DesignSystemStatus =
	| "pending_approval"
	| "backlog"
	| "ready"
	| "in_progress"
	| "done";

const RENDERABLE: ReadonlySet<string> = new Set([
	"pending_approval",
	"backlog",
	"ready",
	"in_progress",
	"done",
]);

/**
 * The closest status the design system can draw.
 *
 * `blocked` maps to `pending_approval`: both mean "parked, waiting on a
 * person", and it renders as an amber clock, which reads correctly. The real
 * status and its block kind are shown alongside by our own components, so the
 * substitution is never the only thing the reader sees.
 *
 * `skipped` maps to `done` instead, because the property that misleads if lost
 * is *terminality*, not success. Drawn as an amber clock it would read as
 * still-waiting, and someone would wait for a task that a condition ruled out
 * and which will never run. `done` overstates the outcome, which our own
 * components correct alongside; "pending" would misstate whether it is over,
 * which nothing downstream would.
 */
export function toDesignSystemStatus(status: TaskStatus): DesignSystemStatus {
	if (RENDERABLE.has(status)) return status as DesignSystemStatus;
	return status === "skipped" ? "done" : "pending_approval";
}

/** Whether this status is one the design system would mishandle. */
export function needsStatusAdapter(status: TaskStatus): boolean {
	return !RENDERABLE.has(status);
}

/**
 * A copy of the task safe to hand to a design-system component.
 *
 * Only the status is rewritten. Anything reading the result back — a status
 * change handler, a delete — must resolve the real task by id rather than
 * trusting the status on this copy.
 */
export function toDesignSystemTask<T extends TaskItem>(task: T): T {
	if (!needsStatusAdapter(task.status)) return task;
	return {...task, status: toDesignSystemStatus(task.status)};
}
