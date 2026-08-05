import {describe, expect, test} from "bun:test";
import type {
	LoopResolution,
	TaskItem,
	WorkflowEdge,
	WorkflowStep,
} from "@/api/client";
import {
	DEFAULT_LOOP_MAX_ITERATIONS,
	MAX_LOOP_ITERATIONS,
	bodyByStepKey,
	describePredicate,
	finalResolution,
	isLoopExit,
	loopBodies,
	readPredicate,
	sortPasses,
} from "@/components/workflows/loops";

/**
 * These two constants are a client-side mirror of `MAX_LOOP_ITERATIONS` and
 * `DEFAULT_LOOP_MAX_ITERATIONS` in `src/tasks/store.rs`. The server enforces
 * the real values at launch; these tests exist so the mirror cannot drift —
 * if the Rust side changes, change these expectations in the same commit.
 */
describe("iteration limits mirror the server", () => {
	test("ceiling matches MAX_LOOP_ITERATIONS in src/tasks/store.rs", () => {
		expect(MAX_LOOP_ITERATIONS).toBe(25);
	});

	test("default matches DEFAULT_LOOP_MAX_ITERATIONS in src/tasks/store.rs", () => {
		expect(DEFAULT_LOOP_MAX_ITERATIONS).toBe(3);
	});
});

function step(
	stepKey: string,
	loop: {group?: string; maxIterations?: number; until?: unknown} = {},
): WorkflowStep {
	// The loop logic reads only these fields; a full template step is noise.
	return {
		step_key: stepKey,
		loop_group: loop.group ?? null,
		loop_max_iterations: loop.maxIterations ?? null,
		loop_until: loop.until ?? null,
	} as WorkflowStep;
}

function edge(parent: string, child: string): WorkflowEdge {
	return {parent_step_key: parent, child_step_key: child, kind: "normal"};
}

describe("loopBodies", () => {
	test("the exit is the member nothing inside the body waits on", () => {
		const bodies = loopBodies(
			[step("a", {group: "g"}), step("b", {group: "g"})],
			[edge("a", "b")],
		);
		const body = bodies.get("g");
		expect(body?.exit?.step_key).toBe("b");
		expect(body?.exitCandidates).toEqual(["b"]);
	});

	test("an edge leaving the body does not disqualify the exit", () => {
		// The exit step's give-up edge points outside the body; that is the arm
		// the loop takes on its way out, not a step still to come in this pass.
		const bodies = loopBodies(
			[step("a", {group: "g"}), step("b", {group: "g"}), step("after")],
			[edge("a", "b"), edge("b", "after")],
		);
		expect(bodies.get("g")?.exit?.step_key).toBe("b");
	});

	test("a ring has no exit — the server refuses it, and the canvas offers nothing", () => {
		const bodies = loopBodies(
			[step("a", {group: "g"}), step("b", {group: "g"})],
			[edge("a", "b"), edge("b", "a")],
		);
		const body = bodies.get("g");
		expect(body?.exit).toBeNull();
		expect(body?.exitCandidates).toEqual([]);
	});

	test("two leaves means no single exit", () => {
		const bodies = loopBodies(
			[step("a", {group: "g"}), step("b", {group: "g"})],
			[],
		);
		const body = bodies.get("g");
		expect(body?.exit).toBeNull();
		expect(body?.exitCandidates).toEqual(["a", "b"]);
	});

	test("the exit step's iteration limit wins; an unset one gets the server default", () => {
		const withLimit = loopBodies(
			[step("a", {group: "g"}), step("b", {group: "g", maxIterations: 10})],
			[edge("a", "b")],
		);
		expect(withLimit.get("g")?.maxIterations).toBe(10);

		const withoutLimit = loopBodies(
			[step("a", {group: "g"}), step("b", {group: "g"})],
			[edge("a", "b")],
		);
		expect(withoutLimit.get("g")?.maxIterations).toBe(DEFAULT_LOOP_MAX_ITERATIONS);
	});

	test("entries are the outside steps feeding into the body, sorted", () => {
		const bodies = loopBodies(
			[step("z-entry"), step("a-entry"), step("a", {group: "g"})],
			[edge("z-entry", "a"), edge("a-entry", "a")],
		);
		expect(bodies.get("g")?.entries).toEqual(["a-entry", "z-entry"]);
	});

	test("steps without a loop group form no body", () => {
		expect(loopBodies([step("a"), step("b")], [edge("a", "b")]).size).toBe(0);
	});
});

describe("bodyByStepKey / isLoopExit", () => {
	test("every member maps to its body, and only the exit is the exit", () => {
		const bodies = loopBodies(
			[step("a", {group: "g"}), step("b", {group: "g"})],
			[edge("a", "b")],
		);
		const byKey = bodyByStepKey(bodies);
		expect(byKey.get("a")?.group).toBe("g");
		expect(byKey.get("b")?.group).toBe("g");
		expect(byKey.has("outside")).toBe(false);

		expect(isLoopExit("b", bodies)).toBe(true);
		expect(isLoopExit("a", bodies)).toBe(false);
	});
});

describe("readPredicate", () => {
	test("reads the three shapes the server's evaluator understands", () => {
		expect(readPredicate({pointer: "/verdict", equals: "ship"})).toEqual({
			mode: "equals",
			pointer: "/verdict",
			value: "ship",
		});
		expect(readPredicate({pointer: "/verdict", any_of: ["ship", "ok"]})).toEqual({
			mode: "any_of",
			pointer: "/verdict",
			values: ["ship", "ok"],
		});
		expect(readPredicate({pointer: "/verdict"})).toEqual({
			mode: "present",
			pointer: "/verdict",
		});
	});

	test("a shape the server would not understand is not one the editor invents", () => {
		expect(readPredicate(null)).toBeNull();
		expect(readPredicate("ship")).toBeNull();
		expect(readPredicate({equals: "ship"})).toBeNull();
	});
});

describe("describePredicate", () => {
	test("one readable line per shape", () => {
		expect(describePredicate({pointer: "/verdict", equals: "ship"})).toBe(
			'until /verdict is "ship"',
		);
		expect(describePredicate({pointer: "/verdict"})).toBe("until /verdict is present");
		expect(describePredicate(undefined)).toBe("no exit condition");
	});
});

function pass(taskNumber: number, iteration: number, resolution: LoopResolution | null): TaskItem {
	// Pass history reads only these three fields.
	return {
		task_number: taskNumber,
		loop_iteration: iteration,
		loop_resolution: resolution,
	} as TaskItem;
}

describe("sortPasses", () => {
	test("orders by iteration, oldest first, even when tasks arrive out of order", () => {
		const sorted = sortPasses([
			pass(12, 3, null),
			pass(10, 1, null),
			pass(11, 2, null),
		]);
		expect(sorted.map((task) => task.task_number)).toEqual([10, 11, 12]);
	});

	test("task number breaks an iteration tie, and a missing iteration sorts as zero", () => {
		const sorted = sortPasses([
			pass(21, 1, null),
			pass(20, 1, null),
			pass(9, 0, null),
		]);
		expect(sorted.map((task) => task.task_number)).toEqual([9, 20, 21]);
	});
});

describe("finalResolution", () => {
	test("the newest pass with a resolution is the loop's answer", () => {
		expect(
			finalResolution([
				pass(10, 1, "iterated"),
				pass(11, 2, "converged"),
			]),
		).toBe("converged");
	});

	test("an unresolved tail does not hide an older resolution", () => {
		expect(
			finalResolution([
				pass(10, 1, "exhausted_routed"),
				pass(11, 2, null),
			]),
		).toBe("exhausted_routed");
	});

	test("no resolutions yet means the loop has not resolved", () => {
		expect(finalResolution([pass(10, 1, null)])).toBeNull();
	});
});
