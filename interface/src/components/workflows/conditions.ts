import type {
	GateDisposition,
	GateResult,
	StepGate,
	TaskGate,
	WorkflowEdge,
} from "@/api/client";
import {ancestorsOf} from "./graph";
import {readPredicate} from "./loops";

/**
 * Conditions, restated client-side so the editor can explain itself.
 *
 * A condition is a predicate plus a **disposition** saying what a false answer
 * means: `wait` polls again, `route` settles the step as skipped. The two ask
 * the same question with opposite failure modes, which is precisely why the
 * field exists — and why the editor must never leave an author guessing which
 * one they are getting.
 *
 * `disposition` is nullable and null means *derive*. The derivation is a fact,
 * not a heuristic: it asks whether the input can still change. A `task_output`
 * condition whose source has settled routes, because nothing can change the
 * answer; everything else waits. The server decides at poll time from the real
 * task status. What is knowable *here*, at authoring time, is the graph — and
 * whether the source runs before this step is exactly the thing that determines
 * whether it will have settled. So the editor can predict the derivation, and
 * showing that prediction is what makes the default trustworthy.
 *
 * The predicate language is deliberately the same one `loop_until` uses, and
 * `readPredicate` is imported rather than reimplemented. A second predicate
 * parser is a second set of shapes it silently eats.
 */

/** The minimum gap between polls. The server's floor, restated. */
export const MIN_POLL_INTERVAL_SECS = 15;

/** What the author picked, with `derive` as the un-set state rather than null. */
export type DispositionChoice = "derive" | GateDisposition;

export interface DerivedDisposition {
	/** What a false answer will actually mean. */
	disposition: GateDisposition;
	/** Why, in a clause that can follow the step's name. */
	because: string;
	/**
	 * True when the prediction rests on the graph rather than on the kind alone.
	 *
	 * An `http` condition always waits and that is certain. A `task_output` one
	 * depends on whether its source has settled, which the editor infers from
	 * the edges — right whenever the template is launched as drawn, which is the
	 * only way it can be launched.
	 */
	inferred: boolean;
}

/**
 * What omitting the disposition will resolve to, and why.
 *
 * The source running before this step is what makes it settled, and settled is
 * what makes the answer final. An unrelated step may still be running when this
 * one is considered, so its output cannot yet be a decision.
 */
export function deriveStepDisposition(
	kind: string,
	sourceStepKey: string | null | undefined,
	stepKey: string,
	edges: WorkflowEdge[],
): DerivedDisposition {
	if (kind !== "task_output") {
		return {
			disposition: "wait",
			because:
				"polls the outside world, which can always answer differently next time",
			inferred: false,
		};
	}
	const source = sourceStepKey?.trim();
	if (!source) {
		return {
			disposition: "wait",
			because: "no source step chosen yet",
			inferred: true,
		};
	}
	const ancestors = ancestorsOf(edges, stepKey);
	if (ancestors.has(source)) {
		return {
			disposition: "route",
			because: `reads \`${source}\`, which finishes before this runs`,
			inferred: true,
		};
	}
	return {
		disposition: "wait",
		because: `reads \`${source}\`, which is not upstream of this step, so it may still be running`,
		inferred: true,
	};
}

/** The same question for a compiled gate on a live task. */
export function effectiveDisposition(gate: TaskGate): GateDisposition {
	if (gate.disposition) return gate.disposition;
	// Without the source task's status the client cannot finish the server's
	// derivation, so it reports the half it knows. `http` is certain.
	return gate.kind === "http" ? "wait" : "route";
}

/** Whether a step declares any condition at all. */
export function gatesForStep(gates: StepGate[], stepKey: string): StepGate[] {
	return gates.filter((gate) => gate.step_key === stepKey);
}

/**
 * The condition as one readable line.
 *
 * The author's label wins whenever there is one: "waiting for CI on main" is
 * what a reader needs and a URL is what they have to decode. The generated form
 * is the fallback, not the norm.
 */
export function describeCondition(
	gate: Pick<StepGate, "kind" | "label" | "config" | "source_step_key">,
): string {
	if (gate.label?.trim()) return gate.label.trim();
	const predicate = readPredicate(gate.config);
	const config = (gate.config ?? {}) as Record<string, unknown>;
	if (gate.kind === "http") {
		const url = typeof config.url === "string" ? config.url : "a URL";
		if (predicate) return `${url} — ${describePredicateClause(predicate)}`;
		if (typeof config.expect_status === "number") {
			return `${url} returns ${config.expect_status}`;
		}
		return url;
	}
	const source = gate.source_step_key ?? "another step";
	if (!predicate) return `reads \`${source}\``;
	return `\`${source}\` ${describePredicateClause(predicate)}`;
}

function describePredicateClause(
	predicate: NonNullable<ReturnType<typeof readPredicate>>,
): string {
	const at = predicate.pointer || "/";
	switch (predicate.mode) {
		case "equals":
			return `${at} is ${JSON.stringify(predicate.value)}`;
		case "any_of":
			return `${at} is one of ${JSON.stringify(predicate.values)}`;
		case "present":
			return `${at} is present`;
	}
}

/**
 * How a condition reads on a node, where there is room for a few words.
 *
 * `route` and `wait` are drawn apart everywhere because they are different
 * facts about the step: one says it might never run, the other says it is held
 * until the world catches up. A canvas that showed them the same way would make
 * a branching template look linear, which is the thing this exists to fix.
 */
export const DISPOSITION_SHORT: Record<GateDisposition, string> = {
	wait: "waits",
	route: "may skip",
};

export const DISPOSITION_LABEL: Record<GateDisposition, string> = {
	wait: "Wait — hold this step until it becomes true",
	route: "Route — a false answer means this step does not apply",
};

export const DISPOSITION_HINT: Record<GateDisposition, string> = {
	wait: "The step is held and the condition polled again. It never gives up on its own.",
	route:
		"A false answer settles this step as skipped. It will never run, and anything that needs its output skips too.",
};

/** Whether a gate's verdict means a person has to do something. */
export function needsAPerson(gate: TaskGate): boolean {
	if (gate.last_result === "failed") return true;
	// `erroring` is our problem rather than the graph's — being unable to reach
	// CI is not CI saying no, and it must never route a branch. But one that has
	// been erroring long enough to have stopped backing off is not going to fix
	// itself either, and saying so is the whole point of keeping the state
	// separate from `failed`.
	return gate.last_result === "erroring" && gate.consecutive_errors >= 3;
}

export const RESULT_LABEL: Record<GateResult, string> = {
	pending: "Not yet",
	satisfied: "Open",
	failed: "Answered no",
	erroring: "Cannot tell",
	routed: "Ruled it out",
};

export const RESULT_HINT: Record<GateResult, string> = {
	pending: "Not true yet. It may become true on its own, so it keeps polling.",
	satisfied: "True. Latched — a condition that has opened stays open.",
	failed: "Definitively false. Polling will not change it.",
	erroring:
		"We could not evaluate it. That is our problem, not an answer — it never rules a step out on its own.",
	routed:
		"It did not hold, and this condition routes rather than waits — so the step it guarded was ruled out and this condition is finished.",
};

/**
 * Colour for a verdict.
 *
 * `erroring` is deliberately warning rather than error: it says we could not
 * tell, and painting it the same red as a decided no is how someone reads a DNS
 * failure as CI going red.
 */
export const RESULT_TONE: Record<GateResult, string> = {
	pending: "text-ink-dull",
	satisfied: "text-status-success",
	failed: "text-status-error",
	erroring: "text-status-warning",
	// Neutral, not red. Routing is an ordinary outcome — the branch did not
	// apply — where `failed` is trouble that needs a person.
	routed: "text-ink-dull",
};

export const RESULT_DOT: Record<GateResult, string> = {
	pending: "bg-ink-faint",
	satisfied: "bg-status-success",
	failed: "bg-status-error",
	erroring: "bg-status-warning",
	// Hollow, matching the `skipped` column dot: settled, and settled by not
	// applying rather than by finishing.
	routed: "border border-ink-faint",
};
