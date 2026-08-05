import type {TaskGraphEdge, TaskItem} from "@/api/client";
import {
	COLUMN_GAP,
	NODE_HEIGHT,
	NODE_WIDTH,
	ROW_GAP,
	type NodePosition,
} from "@/components/workflows/layout";

/**
 * Where each task sits on the graph canvas.
 *
 * Same idea as `layoutSteps`, and deliberately the same constants — layer by
 * longest path from a root so two tasks that both wait on the same parent are
 * drawn level with each other, then order each column by the average row of its
 * prerequisites so the edges stop crossing for no visible reason.
 *
 * It is a separate function rather than a call into `layoutSteps` because the
 * two are laying out different things. There a node is a *step*, a fan-out is
 * one step drawn several times, and the column is computed on the template
 * graph and then expanded. Here a node is a *task* — the fan-out has already
 * happened, three audits are three first-class nodes with three real edges, and
 * there is no template to compute anything on. Feeding tasks through the step
 * layout would mean inventing a fake `WorkflowStep` per task, which is a worse
 * lie than sixty lines of arithmetic.
 *
 * Two things this handles that the step layout never has to:
 *
 * - **Cycles.** Task edges are acyclic by the store's own rule, but a truncated
 *   walk can return an edge whose other end was never included, and a graph
 *   assembled from a partial edge set should not be trusted to be a DAG. Any
 *   task that cannot be layered is parked in a column past everything that
 *   could, so it is visible rather than stacked on the origin.
 * - **Several roots that are not connected to each other.** The component is
 *   connected by construction, but truncation can cut the path between two
 *   halves of it. They still lay out; they just read as two clumps, which is
 *   honest — the banner says the walk was cut short.
 */
export function layoutTaskGraph(
	tasks: TaskItem[],
	edges: TaskGraphEdge[],
): Map<number, NodePosition> {
	const present = new Set(tasks.map((task) => task.task_number));
	const parents = new Map<number, number[]>();
	const children = new Map<number, number[]>();
	const indegree = new Map<number, number>(
		tasks.map((task) => [task.task_number, 0]),
	);

	for (const edge of edges) {
		// An edge to a task the walk did not return is not a dependency anything
		// on screen can satisfy, so counting it would park the child forever.
		if (
			!present.has(edge.parent_task_number) ||
			!present.has(edge.child_task_number)
		) {
			continue;
		}
		push(parents, edge.child_task_number, edge.parent_task_number);
		push(children, edge.parent_task_number, edge.child_task_number);
		indegree.set(
			edge.child_task_number,
			(indegree.get(edge.child_task_number) ?? 0) + 1,
		);
	}

	// Kahn's algorithm, task number breaking ties so the result is stable across
	// reloads and identical for two people looking at the same graph.
	const ready = [...indegree.entries()]
		.filter(([, degree]) => degree === 0)
		.map(([number]) => number)
		.sort((a, b) => a - b);
	const remaining = new Map(indegree);
	const ordered: number[] = [];
	while (ready.length > 0) {
		const number = ready.shift() as number;
		ordered.push(number);
		for (const child of children.get(number) ?? []) {
			const left = (remaining.get(child) ?? 0) - 1;
			remaining.set(child, left);
			if (left === 0) {
				ready.push(child);
				ready.sort((a, b) => a - b);
			}
		}
	}

	const placeable = new Set(ordered);
	const stranded = tasks
		.map((task) => task.task_number)
		.filter((number) => !placeable.has(number))
		.sort((a, b) => a - b);

	// `ordered` is topological, so one pass suffices: every parent already has a
	// depth by the time its child is read.
	const depth = new Map<number, number>();
	let maxDepth = -1;
	for (const number of ordered) {
		let own = 0;
		for (const parent of parents.get(number) ?? []) {
			const parentDepth = depth.get(parent);
			if (parentDepth != null) own = Math.max(own, parentDepth + 1);
		}
		depth.set(number, own);
		maxDepth = Math.max(maxDepth, own);
	}
	for (const number of stranded) depth.set(number, maxDepth + 1);

	const columns = new Map<number, number[]>();
	const orderIndex = new Map(
		[...ordered, ...stranded].map((number, index) => [number, index]),
	);
	for (const number of [...ordered, ...stranded]) {
		push(columns, depth.get(number) ?? 0, number);
	}

	const row = new Map<number, number>();
	const positions = new Map<number, NodePosition>();
	for (const column of [...columns.keys()].sort((a, b) => a - b)) {
		const members = columns.get(column) ?? [];
		const barycentre = (number: number) => {
			const upstream = (parents.get(number) ?? []).filter((parent) =>
				row.has(parent),
			);
			if (upstream.length === 0) return null;
			return (
				upstream.reduce((total, parent) => total + (row.get(parent) ?? 0), 0) /
				upstream.length
			);
		};
		const sorted = [...members].sort((a, b) => {
			const left = barycentre(a);
			const right = barycentre(b);
			if (left != null && right != null && left !== right) return left - right;
			if (left != null && right == null) return -1;
			if (left == null && right != null) return 1;
			return (orderIndex.get(a) ?? 0) - (orderIndex.get(b) ?? 0);
		});

		const span = Math.max(
			0,
			sorted.length * NODE_HEIGHT + (sorted.length - 1) * ROW_GAP,
		);
		sorted.forEach((number, index) => {
			row.set(number, index);
			positions.set(number, {
				x: column * (NODE_WIDTH + COLUMN_GAP),
				y: index * (NODE_HEIGHT + ROW_GAP) - span / 2,
			});
		});
	}

	return positions;
}

function push<K, V>(map: Map<K, V[]>, key: K, value: V) {
	const list = map.get(key);
	if (list) list.push(value);
	else map.set(key, [value]);
}
