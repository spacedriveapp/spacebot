import type {WorkflowEdge, WorkflowStep} from "@/api/client";
import {orderSteps, parentsByStep} from "./graph";

/**
 * Where each step sits on the canvas.
 *
 * The server stores no coordinates. `position` is display order and nothing
 * else — the schema says so — so a canvas has to derive its own geometry from
 * the only thing that is actually true about a template: the edges.
 *
 * Layering by longest path from a root is what makes the shape readable. Two
 * steps that both wait on `draft` and on nothing else land in the same column,
 * side by side, which is the whole reason to draw a graph instead of a list:
 * parallel branches only *look* parallel if they are drawn level with each
 * other. Layering by shortest path would break that the moment one branch grew
 * an extra step — the fan-in would sit level with its own prerequisite.
 *
 * Nothing here is persisted. A drag moves a node for the rest of the session
 * and is deliberately forgotten on reload, because the alternative is a saved
 * layout that silently stops matching a template someone else edited.
 */

/** Node box, in flow units. Shared with the node component's CSS. */
export const NODE_WIDTH = 232;
export const NODE_HEIGHT = 92;

const COLUMN_GAP = 104;
const ROW_GAP = 30;

export interface NodePosition {
	x: number;
	y: number;
}

/**
 * Positions for every step, keyed by step key.
 *
 * Steps caught in a cycle cannot be layered at all — there is no "longest path
 * from a root" when the path re-enters itself — so they are parked in a column
 * of their own past everything that could be placed. The editor draws the
 * cycle banner over the top; the point here is only that they still appear
 * somewhere rather than stacking on the origin.
 */
export function layoutSteps(
	steps: WorkflowStep[],
	edges: WorkflowEdge[],
): Map<string, NodePosition> {
	const {ordered, cycle} = orderSteps(steps, edges);
	const cycleKeys = new Set(cycle);
	const parents = parentsByStep(edges);
	const present = new Set(steps.map((step) => step.step_key));

	// `ordered` is topological for everything outside a cycle, so one pass is
	// enough: a step's parents already have their depth by the time it is read.
	const depth = new Map<string, number>();
	let maxDepth = -1;
	for (const step of ordered) {
		if (cycleKeys.has(step.step_key)) continue;
		let own = 0;
		for (const parent of parents.get(step.step_key) ?? []) {
			if (!present.has(parent)) continue;
			const parentDepth = depth.get(parent);
			if (parentDepth != null) own = Math.max(own, parentDepth + 1);
		}
		depth.set(step.step_key, own);
		maxDepth = Math.max(maxDepth, own);
	}
	for (const key of cycleKeys) depth.set(key, maxDepth + 1);

	const columns = new Map<number, string[]>();
	const orderIndex = new Map(ordered.map((step, index) => [step.step_key, index]));
	for (const step of ordered) {
		const column = depth.get(step.step_key) ?? 0;
		const list = columns.get(column);
		if (list) list.push(step.step_key);
		else columns.set(column, [step.step_key]);
	}

	// Sort each column by the average row of its prerequisites in the columns
	// already placed. Without it, a fan-in's two feeders can be drawn in the
	// opposite order to the way their own feeders were, and the edges cross for
	// no reason the reader can see.
	const row = new Map<string, number>();
	const positions = new Map<string, NodePosition>();
	for (const column of [...columns.keys()].sort((a, b) => a - b)) {
		const keys = columns.get(column) ?? [];
		const barycentre = (key: string) => {
			const upstream = (parents.get(key) ?? []).filter((parent) =>
				row.has(parent),
			);
			if (upstream.length === 0) return null;
			return (
				upstream.reduce((total, parent) => total + (row.get(parent) ?? 0), 0) /
				upstream.length
			);
		};
		const sorted = [...keys].sort((a, b) => {
			const left = barycentre(a);
			const right = barycentre(b);
			if (left != null && right != null && left !== right) return left - right;
			if (left != null && right == null) return -1;
			if (left == null && right != null) return 1;
			return (orderIndex.get(a) ?? 0) - (orderIndex.get(b) ?? 0);
		});

		const span = (NODE_HEIGHT + ROW_GAP) * (sorted.length - 1);
		sorted.forEach((key, index) => {
			row.set(key, index);
			positions.set(key, {
				x: column * (NODE_WIDTH + COLUMN_GAP),
				y: index * (NODE_HEIGHT + ROW_GAP) - span / 2,
			});
		});
	}

	return positions;
}
