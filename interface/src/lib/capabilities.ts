import type {AgentInfo, TaskItem} from "@/api/client";

/**
 * Capability matching, derived on the client.
 *
 * The server does this in `TaskStore::unclaimable_pool` and reports the answer
 * to the cortex log, which nobody reads while wondering why a task never ran.
 * There is no endpoint returning `ReadySweep`, and there does not need to be:
 * both inputs are already on the wire — `required_capabilities` on every task
 * in the list response, `capabilities` on every agent in `GET /agents` — and
 * `AgentInfo.capabilities` is published for exactly this reason. So the rules
 * live here once and every screen asks the same question of the same data.
 *
 * The predicates below are ports of named Rust constants. Where one is, it is
 * cited, because the failure mode of a drifted port is a screen that
 * confidently says a task is fine while the scheduler disagrees.
 */

/**
 * What `assigned_agent_id` holds while a pooled task waits to be claimed.
 *
 * Ports `UNASSIGNED_AGENT_ID` in `src/tasks/store.rs`. The column is `NOT NULL`,
 * so empty string is the sentinel rather than null.
 */
export const UNASSIGNED_AGENT_ID = "";

/**
 * Whether this task is addressed by capability rather than by name.
 *
 * Deliberately not the same question as {@link isUnclaimed}. A claim stamps an
 * agent on without emptying this, so between the claim and anything else the
 * task is both pooled and assigned — which is what lets the reaper return it
 * to the pool it came from rather than to an agent that may be gone.
 *
 * It is *not* permanent, despite the two facts being separate columns: a
 * `PATCH` that names an agent clears the requirement outright, because
 * `update_task` treats "assign this to designer" as ending pool membership for
 * good — otherwise the next reap would silently undo a person's assignment.
 * So a task observed as pooled may later report `null` here, and nothing
 * downstream should treat this as an immutable record of where it came from.
 */
export function isPooled(task: TaskCapabilityFields): boolean {
	return task.required_capabilities != null;
}

/** Whether nobody holds this task *right now*. Transient — a claim ends it. */
export function isUnclaimed(task: {assigned_agent_id: string}): boolean {
	return task.assigned_agent_id === UNASSIGNED_AGENT_ID;
}

/** A pooled task nobody has taken yet. Pushed tasks are never this. */
export function isAwaitingClaim(
	task: TaskCapabilityFields & {assigned_agent_id: string},
): boolean {
	return isPooled(task) && isUnclaimed(task);
}

/** The subset of `Task` this module reads, so callers can pass partials. */
export interface TaskCapabilityFields {
	required_capabilities?: string[] | null;
}

/** Every distinct label declared by anybody, sorted, for authoring suggestions. */
export function fleetCapabilities(agents: readonly AgentInfo[]): string[] {
	const seen = new Set<string>();
	for (const agent of agents) {
		// `?? []` although the schema types this as always present: an older
		// server omits the field entirely, and a crash on the agents list is a
		// worse answer than an empty suggestion list.
		for (const label of agent.capabilities ?? []) seen.add(label);
	}
	return [...seen].sort();
}

/**
 * Which agents could claim a task with this requirement.
 *
 * Ports the `NOT EXISTS (a requirement the agent does not hold)` half of
 * `CLAIMABLE_BY_AGENT`. Every-of, not any-of — an agent must hold the whole
 * set. An empty requirement is held by everybody, which matches the SQL: a
 * `json_each` over `[]` yields no rows, so the `NOT EXISTS` is trivially true.
 *
 * Case is not folded, exactly as `normalise_capabilities` does not fold it.
 * `rust` and `Rust` are two capabilities here too, because a picker that
 * quietly matched them would disagree with the scheduler that will not.
 */
export function agentsSatisfying(
	requires: readonly string[],
	agents: readonly AgentInfo[],
): AgentInfo[] {
	return agents.filter((agent) => {
		const held = new Set(agent.capabilities ?? []);
		return requires.every((label) => held.has(label));
	});
}

/** Why a pooled task is beyond the fleet. The two want different repairs. */
export type UnclaimableReason =
	/** At least one label no agent declares at all — a typo, or a missing specialist. */
	| "undeclared"
	/** Every label is declared by somebody; no single agent holds them all. */
	| "split";

/**
 * A pooled task no agent in the fleet can claim.
 *
 * Mirrors the Rust `UnclaimableTask`, which carries `undeclared` rather than a
 * boolean for the reason restated in {@link UnclaimableReason}: reporting only
 * "unclaimable" sends the reader hunting for a missing capability that is right
 * there in front of them.
 */
export interface UnclaimableTask {
	task: TaskItem;
	/** What the task asked for. */
	requires: string[];
	/** The subset of `requires` no agent declares at all. Empty means "split". */
	undeclared: string[];
	reason: UnclaimableReason;
}

/**
 * The pooled tasks that are ready, unclaimed, and beyond every agent.
 *
 * Ports `TaskStore::unclaimable_pool`, including its row filter: `status =
 * 'ready'`, unclaimed, pooled, and not a fan-out placeholder. A placeholder is
 * excluded because it is not real work yet — it expands into the tasks that
 * are — and reporting it would put a permanent phantom on the board.
 *
 * Returns `[]` when the fleet is empty rather than declaring everything
 * unclaimable: no agents usually means the agent list has not loaded, and a
 * board that screams during a refetch trains people to ignore it.
 */
export function unclaimablePool(
	tasks: readonly TaskItem[],
	agents: readonly AgentInfo[],
): UnclaimableTask[] {
	if (agents.length === 0) return [];

	const declaredAnywhere = new Set(fleetCapabilities(agents));
	const out: UnclaimableTask[] = [];

	for (const task of tasks) {
		if (task.status !== "ready") continue;
		if (!isUnclaimed(task)) continue;
		if (task.fan_out_placeholder) continue;
		const requires = task.required_capabilities;
		if (requires == null) continue;

		if (agentsSatisfying(requires, agents).length > 0) continue;

		const undeclared = requires.filter((label) => !declaredAnywhere.has(label));
		out.push({
			task,
			requires: [...requires],
			undeclared,
			reason: undeclared.length > 0 ? "undeclared" : "split",
		});
	}

	return out;
}

/**
 * One line naming the repair, phrased as the Rust `UnclaimableTask::explain`
 * phrases it — the same words whether it is read here or in the cortex log.
 */
export function explainUnclaimable(entry: UnclaimableTask): string {
	if (entry.reason === "split") {
		return `No single agent has all of [${entry.requires.join(
			", ",
		)}] — every one of them is declared by somebody, so give one agent the rest or split the task.`;
	}
	return `No agent declares [${entry.undeclared.join(
		", ",
	)}] — declare it on an agent, or correct the requirement.`;
}
