import {useMemo} from "react";
import {useQuery} from "@tanstack/react-query";
import {api, type TaskItem, type TaskStatus} from "@/api/client";

/**
 * The status moves the task store actually permits.
 *
 * The store rejects anything outside its table, but the board's status
 * controls come from `@spacedrive/ai`, which offers every status it knows
 * regardless of the one the card is in — so the row menu happily proposes
 * `ready → done`, which the API refuses. Re-deriving the rules here in
 * TypeScript is how two copies drift apart; fetching the server's own table is
 * how they cannot.
 */

/** Human wording for a status, including the one the design system lacks. */
export const STATUS_LABEL: Record<TaskStatus, string> = {
	pending_approval: "Pending approval",
	backlog: "Backlog",
	ready: "Ready",
	in_progress: "In progress",
	blocked: "Blocked",
	done: "Done",
	skipped: "Skipped",
};

export interface TransitionTable {
	/** False until the table has arrived. */
	loaded: boolean;
	/** Every status this one may legally move to. */
	targetsOf: (from: TaskStatus) => TaskStatus[];
	allows: (from: TaskStatus, to: TaskStatus) => boolean;
}

/**
 * Fetch the legal-transitions table once per session.
 *
 * It is compiled into the server binary, so it cannot change while the page is
 * open — caching it forever costs one request and keeps every status control
 * on the board reading from the same source.
 */
export function useTaskTransitions(): TransitionTable {
	const {data} = useQuery({
		queryKey: ["task-transitions"],
		queryFn: api.listTaskTransitions,
		staleTime: Infinity,
		gcTime: Infinity,
	});

	return useMemo(() => {
		const byFrom = new Map<TaskStatus, TaskStatus[]>();
		for (const {from, to} of data?.transitions ?? []) {
			const targets = byFrom.get(from) ?? [];
			targets.push(to);
			byFrom.set(from, targets);
		}
		return {
			loaded: data != null,
			targetsOf: (from) => byFrom.get(from) ?? [],
			allows: (from, to) => byFrom.get(from)?.includes(to) ?? false,
		};
	}, [data]);
}

/**
 * What a requested status change should actually do, or why it cannot.
 *
 * Three moves are not plain status writes — they have endpoints that do extra
 * work (record an approver, clear a block reason and re-check dependencies) —
 * so the decision of which call to make belongs next to the decision of whether
 * the move is legal at all.
 */
export type StatusMove =
	| {action: "unblock"}
	| {action: "approve"}
	| {action: "execute"}
	| {action: "update"; status: TaskStatus}
	| {action: "refuse"; reason: string};

/**
 * Resolve a requested move against the store's table.
 *
 * `task` must be the real task, not the copy handed to `@spacedrive/ai` — a
 * blocked card reaches those components reading `pending_approval`, and
 * branching on that would approve a task nobody was asked to approve.
 */
export function planStatusChange(
	task: TaskItem,
	target: TaskStatus,
	table: TransitionTable,
): StatusMove {
	if (task.status === target) return {action: "refuse", reason: "Already there."};

	// Leaving a blocked state is unblock's job rather than a status write: it
	// has to clear the block reason and re-check dependencies, which a PUT of
	// `status` does not do.
	if (task.status === "blocked") return {action: "unblock"};

	// Approve and execute exist because they do more than move the status —
	// approve records who approved, execute refuses a card that never was. Both
	// land on `ready`, which the table permits from where they start.
	if (task.status === "pending_approval" && target === "ready") {
		return {action: "approve"};
	}
	if (task.status === "backlog" && target === "in_progress") {
		return {action: "execute"};
	}

	// Fail open while the table is in flight: the server is still the enforcer,
	// and freezing every control on first paint would be a worse bug than the
	// one this prevents.
	if (!table.loaded || table.allows(task.status, target)) {
		return {action: "update", status: target};
	}

	return {action: "refuse", reason: refusal(task.status, target, table)};
}

/** Say what was refused and what is possible instead, not just "no". */
function refusal(
	from: TaskStatus,
	to: TaskStatus,
	table: TransitionTable,
): string {
	const targets = table.targetsOf(from);
	const legal = targets.length
		? targets.map((status) => STATUS_LABEL[status]).join(", ")
		: "nothing — it is a terminal state";
	return `${STATUS_LABEL[from]} → ${STATUS_LABEL[to]} is not a legal move. From ${STATUS_LABEL[from]} this task can go to: ${legal}.`;
}
