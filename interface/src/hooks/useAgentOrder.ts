import { useState, useEffect } from "react";

const STORAGE_KEY = "spacebot:agent-order";

/**
 * Hook to manage persistent agent ordering via localStorage.
 * Preserves user's custom sort order across sessions.
 */
export function useAgentOrder(agentIds: string[]) {
	const [order, setOrder] = useState<string[]>([]);

	// Load order from localStorage on mount and when agentIds change
	useEffect(() => {
		const stored = localStorage.getItem(STORAGE_KEY);
		let storedOrder: string[] = [];
		
		if (stored) {
			try {
				storedOrder = JSON.parse(stored);
			} catch {
				storedOrder = [];
			}
		}

		// Merge: keep stored order for existing agents, append new agents.
		// The "main" agent is always pinned first in the sidebar.
		const storedSet = new Set(storedOrder);
		const newAgents = agentIds.filter((id) => !storedSet.has(id));
		const validStoredOrder = storedOrder.filter((id) => agentIds.includes(id));
		const merged = [...validStoredOrder, ...newAgents];

		const mainId = agentIds.includes("main") ? "main" : agentIds[0];
		if (mainId && merged[0] !== mainId) {
			const idx = merged.indexOf(mainId);
			if (idx > 0) {
				merged.splice(idx, 1);
				merged.unshift(mainId);
			}
		}

		setOrder(merged);
	}, [agentIds]);

	// Persist order to localStorage, keeping the main agent pinned first
	const updateOrder = (newOrder: string[]) => {
		const mainId = agentIds.includes("main") ? "main" : agentIds[0];
		if (mainId && newOrder[0] !== mainId) {
			const idx = newOrder.indexOf(mainId);
			if (idx > 0) {
				newOrder = [...newOrder];
				newOrder.splice(idx, 1);
				newOrder.unshift(mainId);
			}
		}
		setOrder(newOrder);
		localStorage.setItem(STORAGE_KEY, JSON.stringify(newOrder));
	};

	return [order, updateOrder] as const;
}
