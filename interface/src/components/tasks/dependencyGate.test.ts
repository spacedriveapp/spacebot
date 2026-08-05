import {describe, expect, test} from "bun:test";
import type {
	TaskDependenciesResponse,
	TaskEdgeSummary,
	TaskItem,
	TaskStatus,
} from "@/api/client";
import {
	dependencyRefusal,
	namedDependencyRefusal,
} from "@/components/tasks/dependencyGate";

function taskWith(status: TaskStatus): TaskItem {
	// Only `status` and `task_number` are read; the rest of the row is
	// irrelevant to a dependency check.
	return {task_number: 5, status} as TaskItem;
}

function summary(blockedBy: number): TaskEdgeSummary {
	return {task_number: 5, blocked_by: blockedBy, parents: blockedBy, children: 0};
}

function dependencies(blockedBy: number[]): TaskDependenciesResponse {
	return {blocked_by: blockedBy, parents: blockedBy, children: []};
}

describe("dependencyRefusal", () => {
	test("permits the drop when nothing is outstanding", () => {
		expect(dependencyRefusal(taskWith("backlog"), "ready", summary(0))).toBeNull();
	});

	test("permits the drop when the edge summary has not loaded", () => {
		expect(dependencyRefusal(taskWith("backlog"), "ready", undefined)).toBeNull();
	});

	test("refuses a drop onto ready while parents are unfinished", () => {
		const refusal = dependencyRefusal(taskWith("backlog"), "ready", summary(2));
		expect(refusal).toContain("#5");
		expect(refusal).toContain("2 unfinished upstream tasks");
	});

	test("singular wording for a single outstanding parent", () => {
		const refusal = dependencyRefusal(taskWith("backlog"), "ready", summary(1));
		expect(refusal).toContain("1 unfinished upstream task.");
	});

	test("in_progress is gated too — execute lands on the same ready state", () => {
		expect(
			dependencyRefusal(taskWith("backlog"), "in_progress", summary(1)),
		).not.toBeNull();
	});

	test("targets that do not put the card in front of a worker are not gated", () => {
		for (const target of ["backlog", "blocked", "done", "skipped"] as TaskStatus[]) {
			expect(dependencyRefusal(taskWith("ready"), target, summary(3))).toBeNull();
		}
	});
});

describe("namedDependencyRefusal", () => {
	test("permits when the dependency response shows nothing outstanding", () => {
		expect(
			namedDependencyRefusal(taskWith("backlog"), "ready", dependencies([]), () => undefined),
		).toBeNull();
	});

	test("names the outstanding parents with their titles", () => {
		const titles = new Map<number, string>([
			[3, "Write the migration"],
			[4, "Backfill the rows"],
		]);
		const refusal = namedDependencyRefusal(
			taskWith("backlog"),
			"ready",
			dependencies([3, 4]),
			(number) => titles.get(number),
		);
		expect(refusal).toContain("#3 Write the migration");
		expect(refusal).toContain("#4 Backfill the rows");
		expect(refusal).toContain("tasks still unfinished");
	});

	test("falls back to the bare number when a title is unknown", () => {
		const refusal = namedDependencyRefusal(
			taskWith("backlog"),
			"ready",
			dependencies([9]),
			() => undefined,
		);
		expect(refusal).toContain("#9");
		expect(refusal).toContain("task still unfinished");
	});
});
