import type {WorkflowEdge, WorkflowStep} from "@/api/client";

/**
 * Put the steps in the order they will actually run.
 *
 * `position` is display order only — the server says so in the schema, and it
 * is not maintained when an edge is added. So a template whose steps were
 * created in one order and wired in another reads as nonsense if you trust
 * `position` alone: "publish" can sit above the step it waits for. A
 * topological sort is what makes the list match execution.
 *
 * Kahn's algorithm, with `position` (then `step_key`) breaking ties so two
 * independent steps keep a stable, author-chosen order rather than whichever
 * one the map happened to yield first.
 *
 * A cycle cannot be ordered at all. The server refuses to create one — both
 * `POST /edges` and `POST /run` answer 409 — but a template can still be read
 * while the refusal is on screen, and returning nothing would blank the editor
 * at the moment it is most needed. So the steps that *can* be ordered come
 * back first, the rest are appended in position order, and `cycle` names the
 * ones that could not be placed.
 */
export function orderSteps(
	steps: WorkflowStep[],
	edges: WorkflowEdge[],
): {ordered: WorkflowStep[]; cycle: string[]} {
	const byKey = new Map(steps.map((step) => [step.step_key, step]));
	const indegree = new Map<string, number>(
		steps.map((step) => [step.step_key, 0]),
	);
	const children = new Map<string, string[]>();

	for (const edge of edges) {
		// An edge can name a step that no longer exists only if the server let it
		// linger; ignore it rather than counting a dependency nothing satisfies.
		if (!byKey.has(edge.parent_step_key) || !byKey.has(edge.child_step_key)) {
			continue;
		}
		indegree.set(edge.child_step_key, (indegree.get(edge.child_step_key) ?? 0) + 1);
		const list = children.get(edge.parent_step_key);
		if (list) list.push(edge.child_step_key);
		else children.set(edge.parent_step_key, [edge.child_step_key]);
	}

	const rank = (key: string) => {
		const step = byKey.get(key);
		return step ? step.position : 0;
	};
	const compare = (a: string, b: string) =>
		rank(a) - rank(b) || a.localeCompare(b);

	const ready = [...indegree.entries()]
		.filter(([, degree]) => degree === 0)
		.map(([key]) => key)
		.sort(compare);

	const ordered: WorkflowStep[] = [];
	while (ready.length > 0) {
		const key = ready.shift() as string;
		const step = byKey.get(key);
		if (step) ordered.push(step);
		for (const child of children.get(key) ?? []) {
			const next = (indegree.get(child) ?? 0) - 1;
			indegree.set(child, next);
			if (next === 0) {
				ready.push(child);
				ready.sort(compare);
			}
		}
	}

	const placed = new Set(ordered.map((step) => step.step_key));
	const cycle = steps
		.filter((step) => !placed.has(step.step_key))
		.sort((a, b) => a.position - b.position || a.step_key.localeCompare(b.step_key));

	return {ordered: [...ordered, ...cycle], cycle: cycle.map((s) => s.step_key)};
}

/** parent keys for each step, so a card can say what it waits for. */
export function parentsByStep(edges: WorkflowEdge[]): Map<string, string[]> {
	const map = new Map<string, string[]>();
	for (const edge of edges) {
		const list = map.get(edge.child_step_key);
		if (list) list.push(edge.parent_step_key);
		else map.set(edge.child_step_key, [edge.parent_step_key]);
	}
	for (const list of map.values()) list.sort();
	return map;
}

/** child keys for each step — the other half of "what does removing this break". */
export function childrenByStep(edges: WorkflowEdge[]): Map<string, string[]> {
	const map = new Map<string, string[]>();
	for (const edge of edges) {
		const list = map.get(edge.parent_step_key);
		if (list) list.push(edge.child_step_key);
		else map.set(edge.parent_step_key, [edge.child_step_key]);
	}
	for (const list of map.values()) list.sort();
	return map;
}

/**
 * Every step that is guaranteed to have finished before this one starts.
 *
 * Not the same as its direct prerequisites, and the difference matters: in
 * `draft → review → publish`, `publish` waits only for `review`, but binding
 * `publish` to `draft`'s output is perfectly safe because `draft` is finished
 * by then. Checking direct parents alone flags that as a mistake, and a warning
 * that fires on correct pipelines is one people learn to ignore.
 */
export function ancestorsOf(edges: WorkflowEdge[], stepKey: string): Set<string> {
	const parents = parentsByStep(edges);
	const seen = new Set<string>();
	const stack = [stepKey];
	while (stack.length > 0) {
		const key = stack.pop() as string;
		for (const parent of parents.get(key) ?? []) {
			if (seen.has(parent)) continue;
			seen.add(parent);
			stack.push(parent);
		}
	}
	return seen;
}

/**
 * Would adding parent → child close a loop?
 *
 * The server refuses cycles with a 409 that names the path, and that refusal is
 * the authority. This only exists so the picker can grey out the choices that
 * are certain to be refused: offering a step its own descendant as a
 * prerequisite is offering a mistake.
 */
export function wouldCycle(
	edges: WorkflowEdge[],
	parentKey: string,
	childKey: string,
): boolean {
	if (parentKey === childKey) return true;
	// Walk down from the prospective child. Reaching the prospective parent means
	// the parent already depends on it, so the new edge would close the loop.
	const children = childrenByStep(edges);
	const seen = new Set<string>([childKey]);
	const stack = [childKey];
	while (stack.length > 0) {
		const key = stack.pop() as string;
		for (const next of children.get(key) ?? []) {
			if (next === parentKey) return true;
			if (seen.has(next)) continue;
			seen.add(next);
			stack.push(next);
		}
	}
	return false;
}

/**
 * A step key the server will accept, derived from a title.
 *
 * Keys are what edges and bindings reference, so they are typed into pointers
 * and read in refusals — `draft-the-release-headline` is worth more there than
 * a uuid, and far more than making the author invent one before they have
 * written the title.
 */
export function suggestStepKey(title: string, taken: string[]): string {
	const base =
		title
			.toLowerCase()
			.replace(/[^a-z0-9]+/g, "_")
			.replace(/^_+|_+$/g, "")
			.slice(0, 40) || "step";
	if (!taken.includes(base)) return base;
	for (let n = 2; n < 100; n++) {
		const candidate = `${base}_${n}`;
		if (!taken.includes(candidate)) return candidate;
	}
	return `${base}_${Date.now()}`;
}
