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

/**
 * Exported so a second canvas laying out a different kind of node still lands
 * on the same grid. Two graph screens in one app that disagree about column
 * width read as two apps.
 */
export const COLUMN_GAP = 104;
export const ROW_GAP = 30;
/**
 * Gap between two branches of the same fan-out.
 *
 * Deliberately tighter than the gap between two different steps. A fan-out's
 * branches are one step seen three times, not three steps, and the only thing
 * on the canvas that can say so is the spacing — they share a column with
 * everything else at that depth, so proximity is what groups them.
 */
const BRANCH_GAP = 14;

export interface NodePosition {
	x: number;
	y: number;
}

/**
 * Positions for every node, keyed by node id.
 *
 * On the editor a node *is* a step and the ids are step keys. On a run a step
 * that fanned out became several tasks, and each one is its own node, so the
 * caller passes `nodesByStep` — the node ids that step expanded into, in the
 * order they should stack. Positions come back keyed by those ids.
 *
 * Layering is still computed on the template graph, because that is where the
 * shape lives: the branches of one step are all at the same depth by
 * definition, so laying out the expanded graph from scratch could only ever
 * arrive back at the same columns, and would lose the guarantee that they do.
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
	nodesByStep?: Map<string, string[]>,
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

		// Each step occupies a block as tall as the number of nodes it expanded
		// into, so a three-branch fan-out pushes the step under it down instead of
		// being drawn on top of it. Blocks are stacked first and centred after,
		// because a column's height is not known until every block is measured.
		let cursor = 0;
		const blocks = sorted.map((key) => {
			const ids = nodesByStep?.get(key) ?? [key];
			const top = cursor;
			cursor +=
				ids.length * NODE_HEIGHT + (ids.length - 1) * BRANCH_GAP + ROW_GAP;
			return {key, ids, top};
		});
		const span = Math.max(0, cursor - ROW_GAP);

		blocks.forEach(({key, ids, top}, index) => {
			row.set(key, index);
			ids.forEach((id, branch) => {
				positions.set(id, {
					x: column * (NODE_WIDTH + COLUMN_GAP),
					y: top + branch * (NODE_HEIGHT + BRANCH_GAP) - span / 2,
				});
			});
		});
	}

	return positions;
}
