import type {
	LoopResolution,
	TaskItem,
	WorkflowEdge,
	WorkflowStep,
} from "@/api/client";

/**
 * What the editor knows about a loop before anything has run.
 *
 * A loop is not a field on a step — it is a shape the steps and edges make
 * together, and every screen that draws one has to agree on where it starts and
 * where it ends. The rules here are the server's, restated: `loop_group` names
 * the body, the exit step is the one body step nothing else in the body waits
 * on, and `loop_until` / `loop_max_iterations` are read from that step alone.
 *
 * Restating them client-side is not a second source of truth. Launch is still
 * the authority and its refusals are shown verbatim. But an author drawing a
 * give-up edge needs to know *now* which step can have one, and a canvas that
 * waited for a 422 to find out would be offering the mistake and then reporting
 * it.
 */

/** How many passes a body runs when the exit step does not say. Server default. */
export const DEFAULT_LOOP_MAX_ITERATIONS = 3;
/** The server's ceiling. Every pass is a live model call, hence a hard cap. */
export const MAX_LOOP_ITERATIONS = 25;

export interface LoopBody {
	group: string;
	/** Body members, in template order. */
	members: WorkflowStep[];
	/**
	 * The one step whose output decides whether to go round again.
	 *
	 * `null` when the body has no single exit — zero, because the members form a
	 * ring, or several, because two of them are leaves. The server refuses both
	 * at launch; here it means "there is no step that may carry a give-up edge",
	 * which is exactly the right answer to give the canvas.
	 */
	exit: WorkflowStep | null;
	/** Every member with nothing after it inside the body. `exit` when it is one. */
	exitCandidates: string[];
	/** What the exit step will actually use, default included. */
	maxIterations: number;
	/** The predicate as stored. Unparsed, because the server takes it as written. */
	until: unknown;
	/** Steps outside the body that feed into it. One is the loop's entry. */
	entries: string[];
}

/** Every loop body in a template, keyed by group name. */
export function loopBodies(
	steps: WorkflowStep[],
	edges: WorkflowEdge[],
): Map<string, LoopBody> {
	const bodies = new Map<string, LoopBody>();
	const groups = new Map<string, WorkflowStep[]>();
	for (const step of steps) {
		const group = step.loop_group;
		if (!group) continue;
		const list = groups.get(group);
		if (list) list.push(step);
		else groups.set(group, [step]);
	}

	for (const [group, members] of groups) {
		const keys = new Set(members.map((step) => step.step_key));
		// The exit is the member nothing *inside the body* waits on. An edge
		// leaving the body does not disqualify it — that is the arm the loop
		// takes on its way out, not a step still to come in this pass.
		const exitCandidates = members
			.filter(
				(step) =>
					!edges.some(
						(edge) =>
							edge.parent_step_key === step.step_key &&
							keys.has(edge.child_step_key),
					),
			)
			.map((step) => step.step_key);
		const exit =
			exitCandidates.length === 1
				? (members.find((step) => step.step_key === exitCandidates[0]) ?? null)
				: null;
		const entries = [
			...new Set(
				edges
					.filter(
						(edge) =>
							keys.has(edge.child_step_key) && !keys.has(edge.parent_step_key),
					)
					.map((edge) => edge.parent_step_key),
			),
		].sort();

		bodies.set(group, {
			group,
			members,
			exit,
			exitCandidates,
			maxIterations: exit?.loop_max_iterations ?? DEFAULT_LOOP_MAX_ITERATIONS,
			until: exit?.loop_until ?? null,
			entries,
		});
	}
	return bodies;
}

/** Step key → the body it belongs to, for the screens that ask per step. */
export function bodyByStepKey(
	bodies: Map<string, LoopBody>,
): Map<string, LoopBody> {
	const map = new Map<string, LoopBody>();
	for (const body of bodies.values()) {
		for (const member of body.members) map.set(member.step_key, body);
	}
	return map;
}

/** Whether this step is the one that may carry a give-up edge. */
export function isLoopExit(
	stepKey: string,
	bodies: Map<string, LoopBody>,
): boolean {
	for (const body of bodies.values()) {
		if (body.exit?.step_key === stepKey) return true;
	}
	return false;
}

/**
 * The predicate, in the two shapes the server's evaluator understands.
 *
 * `equals` is an exact match, `any_of` a set, and a bare pointer means "there is
 * something there". Kept as a discriminated read rather than a parse into a
 * class, because an author may have typed a fourth shape into a template
 * elsewhere and the editor must still be able to show it rather than eat it.
 */
export type LoopPredicate =
	| {mode: "equals"; pointer: string; value: unknown}
	| {mode: "any_of"; pointer: string; values: unknown[]}
	| {mode: "present"; pointer: string};

export function readPredicate(until: unknown): LoopPredicate | null {
	if (typeof until !== "object" || until === null) return null;
	const record = until as Record<string, unknown>;
	const pointer = typeof record.pointer === "string" ? record.pointer : null;
	if (pointer === null) return null;
	if ("equals" in record) {
		return {mode: "equals", pointer, value: record.equals};
	}
	if (Array.isArray(record.any_of)) {
		return {mode: "any_of", pointer, values: record.any_of};
	}
	return {mode: "present", pointer};
}

/** The predicate as one readable line — the canvas has room for a line. */
export function describePredicate(until: unknown): string {
	const predicate = readPredicate(until);
	if (!predicate) return "no exit condition";
	switch (predicate.mode) {
		case "equals":
			return `until ${predicate.pointer || "/"} is ${JSON.stringify(predicate.value)}`;
		case "any_of":
			return `until ${predicate.pointer || "/"} is one of ${JSON.stringify(predicate.values)}`;
		case "present":
			return `until ${predicate.pointer || "/"} is present`;
	}
}

/**
 * Wording for a resolution where there is room for a clause, and where there is
 * not.
 *
 * A node has about eighty pixels; the region caption and the panel have a line.
 * "gave up — took the give-up edge" truncated to "gave up — took the give-…" on
 * a node says less than "gave up" does, so the short form is a real form and
 * not an abbreviation of the long one.
 */
export const RESOLUTION_SHORT: Record<LoopResolution, string> = {
	converged: "converged",
	iterated: "another pass",
	exhausted_routed: "gave up",
	exhausted_blocked: "gave up",
};

export const RESOLUTION_LABEL: Record<LoopResolution, string> = {
	converged: "converged",
	iterated: "went round again",
	exhausted_routed: "gave up — took the give-up edge",
	exhausted_blocked: "gave up — nothing to take",
};

export const RESOLUTION_HINT: Record<LoopResolution, string> = {
	converged: "The exit condition was met on this pass, so the loop stopped here.",
	iterated: "The exit condition was not met, so the body ran again.",
	exhausted_routed:
		"The loop ran out of passes. Its give-up edge was followed and the ordinary one was not.",
	exhausted_blocked:
		"The loop ran out of passes and has no give-up edge, so nothing downstream runs.",
};

/**
 * A step's passes on one run, oldest first.
 *
 * Passes are sequential — pass 2 exists only because pass 1 did not converge —
 * so this is a history, not a set of siblings. Sorting by iteration rather than
 * task number keeps that true even if the compiler ever emits them out of order.
 */
export function sortPasses(tasks: TaskItem[]): TaskItem[] {
	return [...tasks].sort(
		(a, b) =>
			(a.loop_iteration ?? 0) - (b.loop_iteration ?? 0) ||
			a.task_number - b.task_number,
	);
}

/** The resolution the loop as a whole reached, if it has reached one. */
export function finalResolution(passes: TaskItem[]): LoopResolution | null {
	for (let index = passes.length - 1; index >= 0; index--) {
		const resolution = passes[index].loop_resolution;
		if (resolution) return resolution;
	}
	return null;
}
