import type {LinkKind} from "@/api/client";

/** Pick source/target handle IDs based on link kind and node positions. */
export function getHandlesForKind(
	kind: string,
	sourcePos?: {x: number; y: number},
	targetPos?: {x: number; y: number},
	savedHandles?: {sourceHandle: string; targetHandle: string},
): {
	sourceHandle: string;
	targetHandle: string;
} {
	// Use saved handles if available (user's explicit choice)
	if (savedHandles) {
		return savedHandles;
	}

	if (kind === "hierarchical") {
		// from is above to: connect bottom of superior to top of subordinate
		return {sourceHandle: "bottom", targetHandle: "top"};
	}
	// Peer: always use left/right handles (horizontal connections)
	if (sourcePos && targetPos) {
		const dx = targetPos.x - sourcePos.x;
		return dx > 0
			? {sourceHandle: "right", targetHandle: "left"}
			: {sourceHandle: "left", targetHandle: "right"};
	}
	return {sourceHandle: "right", targetHandle: "left"};
}

/** Infer link kind from the handle the user dragged from. */
export function inferKindFromHandle(sourceHandle: string | null): LinkKind {
	switch (sourceHandle) {
		case "top":
		case "bottom":
			return "hierarchical";
		default:
			return "peer";
	}
}
