import type {
	TaskDependenciesResponse,
	TaskEdgeSummary,
	TaskItem,
	TaskStatus,
} from "@/api/client";
import {STATUS_LABEL} from "./taskTransitions";

/**
 * The second half of a drop check: the dependency graph.
 *
 * `planStatusChange` answers "is this move in the store's transition table",
 * and that is the only question anything in this app asked until now. It is
 * not the only one that matters. A task with an unfinished parent may legally
 * move `backlog → ready` — the table permits it and the API accepts it — and
 * then nothing happens, forever, because `claim_next_ready` re-checks the
 * dependency invariant in its own WHERE clause and skips the card. The move is
 * safe, but the board has told the user it started something when it did not.
 *
 * So the board refuses the drop instead, and says which parent is outstanding.
 * A refusal that names #3 is actionable; "blocked" is not.
 *
 * This lives beside `taskTransitions.ts` rather than inside it on purpose:
 * that file's whole argument is that the transition rules belong to the server
 * and are fetched, never re-derived. The dependency invariant is a different
 * rule with a different source (`edges` on the list response), and folding it
 * in would blur what is authoritative.
 */

/**
 * Targets that put a card in front of a worker.
 *
 * `ready` is the obvious one. `in_progress` is here because dragging a
 * `backlog` card onto it routes through `POST /tasks/{n}/execute`, and that
 * endpoint sets the status to `ready` — so the two drops have the same
 * destination and must have the same guard. Guarding only `ready` would leave
 * the identical stall reachable one column further right.
 */
const GATED_TARGETS: ReadonlySet<TaskStatus> = new Set(["ready", "in_progress"]);

/**
 * Why this move cannot happen yet, or `null` if the graph permits it.
 *
 * Uses only `edges` from the list response, so the check is synchronous and
 * every card on the board already has what it needs — the drop is refused on
 * the same frame it was attempted. `blocked_by` is a count, so the message
 * this returns can only say *how many*; `namedDependencyRefusal` upgrades it
 * once the parent numbers have been fetched.
 */
export function dependencyRefusal(
	task: TaskItem,
	target: TaskStatus,
	summary: TaskEdgeSummary | undefined,
): string | null {
	if (!GATED_TARGETS.has(target)) return null;
	if (!summary || summary.blocked_by === 0) return null;

	const plural = summary.blocked_by === 1 ? "task" : "tasks";
	return `#${task.task_number} is waiting on ${summary.blocked_by} unfinished upstream ${plural}. Moving it to ${STATUS_LABEL[target]} would not start it — a worker only claims a task whose parents are all done.`;
}

/**
 * The same refusal, with the outstanding parents named.
 *
 * `GET /tasks/{n}/dependencies` returns `blocked_by` as task *numbers*, which
 * is the difference between "waiting on 2 tasks" and "go look at #3 and #5".
 * It costs a request, so it is only made after a drop has already been
 * refused, and the refusal is shown immediately from the count rather than
 * waiting for it — a rejection that appears half a second late reads as a bug.
 */
export function namedDependencyRefusal(
	task: TaskItem,
	target: TaskStatus,
	dependencies: TaskDependenciesResponse,
	titleOf: (taskNumber: number) => string | undefined,
): string | null {
	const outstanding = dependencies.blocked_by;
	if (outstanding.length === 0) return null;

	const named = outstanding
		.map((number) => {
			const title = titleOf(number);
			return title ? `#${number} ${title}` : `#${number}`;
		})
		.join(", ");

	const plural = outstanding.length === 1 ? "" : "s";
	return `#${task.task_number} cannot go to ${STATUS_LABEL[target]} yet — upstream task${plural} still unfinished: ${named}.`;
}
