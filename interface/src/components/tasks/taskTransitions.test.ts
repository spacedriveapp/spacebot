import {describe, expect, test} from "bun:test";
import type {TaskItem, TaskStatus} from "@/api/client";
import {
	planStatusChange,
	type TransitionTable,
} from "@/components/tasks/taskTransitions";

/**
 * The table the way `useTaskTransitions` reduces the server's response — a
 * map from a status to the statuses it may move to. Rebuilt here rather than
 * imported so the tests pin `planStatusChange` against any table shape, not
 * just today's server table.
 */
function tableOf(edges: [TaskStatus, TaskStatus][]): TransitionTable {
	const byFrom = new Map<TaskStatus, TaskStatus[]>();
	for (const [from, to] of edges) {
		const targets = byFrom.get(from) ?? [];
		targets.push(to);
		byFrom.set(from, targets);
	}
	return {
		loaded: true,
		targetsOf: (from) => byFrom.get(from) ?? [],
		allows: (from, to) => byFrom.get(from)?.includes(to) ?? false,
	};
}

const UNLOADED: TransitionTable = {
	loaded: false,
	targetsOf: () => [],
	allows: () => false,
};

function taskWith(status: TaskStatus): TaskItem {
	// Only `status` and `task_number` are read; the rest of the row is
	// irrelevant to a transition decision.
	return {task_number: 7, status} as TaskItem;
}

describe("planStatusChange", () => {
	test("moving to the current status is refused as a no-op", () => {
		expect(planStatusChange(taskWith("ready"), "ready", UNLOADED)).toEqual({
			action: "refuse",
			reason: "Already there.",
		});
	});

	test("leaving blocked always routes to unblock, whatever the target", () => {
		// Unblock clears the block reason and re-checks dependencies; a plain
		// status write would leave the reason behind.
		for (const target of ["backlog", "ready", "done"] as TaskStatus[]) {
			expect(planStatusChange(taskWith("blocked"), target, UNLOADED)).toEqual({
				action: "unblock",
			});
		}
	});

	test("pending_approval → ready routes to approve, not a status write", () => {
		expect(planStatusChange(taskWith("pending_approval"), "ready", UNLOADED)).toEqual({
			action: "approve",
		});
	});

	test("backlog → in_progress routes to execute, not a status write", () => {
		expect(planStatusChange(taskWith("backlog"), "in_progress", UNLOADED)).toEqual({
			action: "execute",
		});
	});

	test("an unloaded table fails open to a plain update", () => {
		// The server still enforces; freezing every control until the table
		// arrives would be worse than one refused write.
		expect(planStatusChange(taskWith("ready"), "done", UNLOADED)).toEqual({
			action: "update",
			status: "done",
		});
	});

	test("a move the loaded table permits is a plain update", () => {
		const table = tableOf([
			["backlog", "ready"],
			["ready", "in_progress"],
			["in_progress", "done"],
		]);
		expect(planStatusChange(taskWith("backlog"), "ready", table)).toEqual({
			action: "update",
			status: "ready",
		});
	});

	test("a move the loaded table forbids is refused, naming the legal targets", () => {
		const table = tableOf([
			["backlog", "ready"],
			["ready", "in_progress"],
		]);
		const move = planStatusChange(taskWith("ready"), "done", table);
		expect(move.action).toBe("refuse");
		if (move.action !== "refuse") return;
		expect(move.reason).toContain("Ready → Done is not a legal move");
		expect(move.reason).toContain("In progress");
	});

	test("a terminal state refuses everything and says so", () => {
		// `done` has no outgoing edges in this table, which is what terminal
		// means: the refusal must say "nothing" rather than list targets.
		const table = tableOf([["backlog", "ready"]]);
		const move = planStatusChange(taskWith("done"), "backlog", table);
		expect(move.action).toBe("refuse");
		if (move.action !== "refuse") return;
		expect(move.reason).toContain("terminal state");
	});
});
