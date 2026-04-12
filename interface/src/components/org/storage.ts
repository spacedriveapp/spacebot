import type {Node} from "@xyflow/react";

export const POSITIONS_KEY = "spacebot:topology:positions";
export const HANDLES_KEY = "spacebot:topology:handles";

export type SavedPositions = Record<string, {x: number; y: number}>;

export type SavedHandles = Record<
	string,
	{sourceHandle: string; targetHandle: string}
>;

export function loadPositions(): SavedPositions {
	try {
		const raw = localStorage.getItem(POSITIONS_KEY);
		if (raw) return JSON.parse(raw);
	} catch {
		// ignore
	}
	return {};
}

export function savePositions(nodes: Node[]) {
	const positions: SavedPositions = {};
	for (const node of nodes) {
		positions[node.id] = node.position;
	}
	try {
		localStorage.setItem(POSITIONS_KEY, JSON.stringify(positions));
	} catch {
		// ignore
	}
}

export function loadHandles(): SavedHandles {
	try {
		const raw = localStorage.getItem(HANDLES_KEY);
		if (raw) return JSON.parse(raw);
	} catch {
		// ignore
	}
	return {};
}
