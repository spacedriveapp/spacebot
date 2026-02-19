import { useCallback, useEffect, useRef, useState } from "react";
import {
	api,
	type BranchCompletedEvent,
	type BranchStartedEvent,
	type InboundMessageEvent,
	type OutboundMessageEvent,
	type TimelineItem,
	type ToolCompletedEvent,
	type ToolStartedEvent,
	type TypingStateEvent,
	type WorkerCompletedEvent,
	type WorkerStartedEvent,
	type WorkerStatusEvent,
	type ChannelInfo,
} from "../api/client";

export interface ActiveWorker {
	id: string;
	task: string;
	status: string;
	startedAt: number;
	toolCalls: number;
	currentTool: string | null;
}

export interface ActiveBranch {
	id: string;
	description: string;
	startedAt: number;
	currentTool: string | null;
	lastTool: string | null;
	toolCalls: number;
}

export interface ChannelLiveState {
	isTyping: boolean;
	timeline: TimelineItem[];
	workers: Record<string, ActiveWorker>;
	branches: Record<string, ActiveBranch>;
	historyLoaded: boolean;
	hasMore: boolean;
	loadingMore: boolean;
}

const PAGE_SIZE = 50;

function emptyLiveState(): ChannelLiveState {
	return { isTyping: false, timeline: [], workers: {}, branches: {}, historyLoaded: false, hasMore: true, loadingMore: false };
}

/** Get a sortable timestamp from any timeline item. */
function itemTimestamp(item: TimelineItem): string {
	switch (item.type) {
		case "message": return item.created_at;
		case "branch_run": return item.started_at;
		case "worker_run": return item.started_at;
	}
}

/**
 * Manages all live channel state from SSE events, message history loading,
 * and status snapshot fetching. Returns the state map and SSE event handlers.
 */
export function useChannelLiveState(channels: ChannelInfo[]) {
	const [liveStates, setLiveStates] = useState<Record<string, ChannelLiveState>>({});

	// Load conversation history for each channel on first appearance
	useEffect(() => {
		for (const channel of channels) {
			setLiveStates((prev) => {
				if (prev[channel.id]?.historyLoaded) return prev;

				const updated = {
					...prev,
					[channel.id]: { ...(prev[channel.id] ?? emptyLiveState()), historyLoaded: true },
				};

				api.channelMessages(channel.id, PAGE_SIZE).then((data) => {
					const history: TimelineItem[] = data.items;

					setLiveStates((current) => {
						const existing = current[channel.id];
						if (!existing) return current;
						const sseItems = existing.timeline;
						const lastHistoryTs = history.length > 0
							? itemTimestamp(history[history.length - 1])
							: "";
						const newSseItems = sseItems.filter((item) => itemTimestamp(item) > lastHistoryTs);
						return {
							...current,
							[channel.id]: {
								...existing,
								timeline: [...history, ...newSseItems],
								hasMore: data.has_more,
							},
						};
					});
				}).catch((error) => {
					console.warn(`Failed to load history for ${channel.id}:`, error);
				});

				return updated;
			});
		}
	}, [channels]);

	// Fetch channel status snapshot and merge into live state.
	// Called on mount and on SSE reconnect/lag recovery.
	const syncStatusSnapshot = useCallback(() => {
		api.channelStatus().then((statusMap) => {
			setLiveStates((prev) => {
				const next = { ...prev };
				for (const [channelId, snapshot] of Object.entries(statusMap)) {
					const existing = next[channelId] ?? emptyLiveState();
					const workers: Record<string, ActiveWorker> = {};
					for (const w of snapshot.active_workers) {
						// Preserve SSE-derived tool state if we already have this worker
						const existingWorker = existing.workers[w.id];
						workers[w.id] = {
							id: w.id,
							task: w.task,
							status: w.status,
							startedAt: new Date(w.started_at).getTime(),
							toolCalls: w.tool_calls,
							currentTool: existingWorker?.currentTool ?? null,
						};
					}
					const branches: Record<string, ActiveBranch> = {};
					for (const b of snapshot.active_branches) {
						const existingBranch = existing.branches[b.id];
						branches[b.id] = {
							id: b.id,
							description: b.description,
							startedAt: new Date(b.started_at).getTime(),
							currentTool: existingBranch?.currentTool ?? null,
							lastTool: existingBranch?.lastTool ?? null,
							toolCalls: existingBranch?.toolCalls ?? 0,
						};
					}
					next[channelId] = { ...existing, workers, branches };
				}
				return next;
			});
		}).catch((error) => {
			console.warn("Failed to fetch channel status:", error);
		});
	}, []);

	// Initial status snapshot load
	const initialSyncDone = useRef(false);
	useEffect(() => {
		if (!initialSyncDone.current) {
			initialSyncDone.current = true;
			syncStatusSnapshot();
		}
	}, [syncStatusSnapshot]);

	// Helper: get or create channel state
	const getOrCreate = (prev: Record<string, ChannelLiveState>, channelId: string) =>
		prev[channelId] ?? emptyLiveState();

	// Helper: push a timeline item into a channel's state
	const pushItem = useCallback((channelId: string, item: TimelineItem) => {
		setLiveStates((prev) => {
			const existing = getOrCreate(prev, channelId);
			const timeline = [...existing.timeline, item];
			return { ...prev, [channelId]: { ...existing, timeline } };
		});
	}, []);

	// Helper: update an existing timeline item by id, or ignore if not found
	const updateItem = useCallback((channelId: string, itemId: string, updater: (item: TimelineItem) => TimelineItem) => {
		setLiveStates((prev) => {
			const state = prev[channelId];
			if (!state) return prev;
			const timeline = state.timeline.map((item) =>
				item.id === itemId ? updater(item) : item
			);
			return { ...prev, [channelId]: { ...state, timeline } };
		});
	}, []);

	// -- SSE event handlers --

	const handleInboundMessage = useCallback((data: unknown) => {
		const event = data as InboundMessageEvent;
		pushItem(event.channel_id, {
			type: "message",
			id: `in-${Date.now()}-${crypto.randomUUID()}`,
			role: "user",
			sender_name: event.sender_name ?? event.sender_id,
			sender_id: event.sender_id,
			content: event.text,
			created_at: new Date().toISOString(),
		});
	}, [pushItem]);

	const handleOutboundMessage = useCallback((data: unknown) => {
		const event = data as OutboundMessageEvent;
		pushItem(event.channel_id, {
			type: "message",
			id: `out-${Date.now()}-${crypto.randomUUID()}`,
			role: "assistant",
			sender_name: null,
			sender_id: null,
			content: event.text,
			created_at: new Date().toISOString(),
		});
		setLiveStates((prev) => {
			const existing = getOrCreate(prev, event.channel_id);
			return { ...prev, [event.channel_id]: { ...existing, isTyping: false } };
		});
	}, [pushItem]);

	const handleTypingState = useCallback((data: unknown) => {
		const event = data as TypingStateEvent;
		setLiveStates((prev) => {
			const existing = getOrCreate(prev, event.channel_id);
			return { ...prev, [event.channel_id]: { ...existing, isTyping: event.is_typing } };
		});
	}, []);

	const handleWorkerStarted = useCallback((data: unknown) => {
		const event = data as WorkerStartedEvent;
		if (!event.channel_id) return;
		const channelId = event.channel_id;

		// Add to active workers (for activity bar)
		setLiveStates((prev) => {
			const existing = getOrCreate(prev, channelId);
			return {
				...prev,
				[channelId]: {
					...existing,
					workers: {
						...existing.workers,
						[event.worker_id]: {
							id: event.worker_id,
							task: event.task,
							status: "starting",
							startedAt: Date.now(),
							toolCalls: 0,
							currentTool: null,
						},
					},
				},
			};
		});

		// Insert timeline item
		pushItem(channelId, {
			type: "worker_run",
			id: event.worker_id,
			task: event.task,
			result: null,
			status: "running",
			started_at: new Date().toISOString(),
			completed_at: null,
		});
	}, [pushItem]);

	const handleWorkerStatus = useCallback((data: unknown) => {
		const event = data as WorkerStatusEvent;
		if (event.channel_id) {
			// Direct lookup via channel_id
			setLiveStates((prev) => {
				const state = prev[event.channel_id!];
				const worker = state?.workers[event.worker_id];
				if (!worker) return prev;
				return {
					...prev,
					[event.channel_id!]: {
						...state,
						workers: {
							...state.workers,
							[event.worker_id]: { ...worker, status: event.status },
						},
					},
				};
			});
			// Update timeline item status
			updateItem(event.channel_id, event.worker_id, (item) => {
				if (item.type !== "worker_run") return item;
				return { ...item, status: event.status };
			});
		} else {
			// Fallback scan for workers without a channel
			setLiveStates((prev) => {
				for (const [channelId, state] of Object.entries(prev)) {
					const worker = state.workers[event.worker_id];
					if (worker) {
						return {
							...prev,
							[channelId]: {
								...state,
								workers: {
									...state.workers,
									[event.worker_id]: { ...worker, status: event.status },
								},
							},
						};
					}
				}
				return prev;
			});
		}
	}, [updateItem]);

	const handleWorkerCompleted = useCallback((data: unknown) => {
		const event = data as WorkerCompletedEvent;
		if (event.channel_id) {
			setLiveStates((prev) => {
				const state = prev[event.channel_id!];
				if (!state?.workers[event.worker_id]) return prev;
				const { [event.worker_id]: _, ...remainingWorkers } = state.workers;
				return {
					...prev,
					[event.channel_id!]: { ...state, workers: remainingWorkers },
				};
			});
			// Update timeline item with result
			updateItem(event.channel_id, event.worker_id, (item) => {
				if (item.type !== "worker_run") return item;
				return { ...item, result: event.result, status: "done", completed_at: new Date().toISOString() };
			});
		} else {
			setLiveStates((prev) => {
				for (const [channelId, state] of Object.entries(prev)) {
					if (state.workers[event.worker_id]) {
						const { [event.worker_id]: _, ...remainingWorkers } = state.workers;
						return {
							...prev,
							[channelId]: { ...state, workers: remainingWorkers },
						};
					}
				}
				return prev;
			});
		}
	}, [updateItem]);

	const handleBranchStarted = useCallback((data: unknown) => {
		const event = data as BranchStartedEvent;

		// Add to active branches (for activity bar)
		setLiveStates((prev) => {
			const existing = getOrCreate(prev, event.channel_id);
			return {
				...prev,
				[event.channel_id]: {
					...existing,
					branches: {
						...existing.branches,
						[event.branch_id]: {
							id: event.branch_id,
							description: event.description || "thinking...",
							startedAt: Date.now(),
							currentTool: null,
							lastTool: null,
							toolCalls: 0,
						},
					},
				},
			};
		});

		// Insert timeline item
		pushItem(event.channel_id, {
			type: "branch_run",
			id: event.branch_id,
			description: event.description || "thinking...",
			conclusion: null,
			started_at: new Date().toISOString(),
			completed_at: null,
		});
	}, [pushItem]);

	const handleBranchCompleted = useCallback((data: unknown) => {
		const event = data as BranchCompletedEvent;

		// Remove from active branches
		setLiveStates((prev) => {
			const state = prev[event.channel_id];
			if (!state?.branches[event.branch_id]) return prev;
			const { [event.branch_id]: _, ...remainingBranches } = state.branches;
			return {
				...prev,
				[event.channel_id]: { ...state, branches: remainingBranches },
			};
		});

		// Update timeline item with conclusion
		updateItem(event.channel_id, event.branch_id, (item) => {
			if (item.type !== "branch_run") return item;
			return { ...item, conclusion: event.conclusion, completed_at: new Date().toISOString() };
		});
	}, [updateItem]);

	const handleToolStarted = useCallback((data: unknown) => {
		const event = data as ToolStartedEvent;
		const channelId = event.channel_id;

		if (channelId) {
			setLiveStates((prev) => {
				const state = prev[channelId];
				if (!state) return prev;

				if (event.process_type === "worker") {
					const worker = state.workers[event.process_id];
					if (!worker) return prev;
					return {
						...prev,
						[channelId]: {
							...state,
							workers: {
								...state.workers,
								[event.process_id]: { ...worker, currentTool: event.tool_name },
							},
						},
					};
				}
				if (event.process_type === "branch") {
					const branch = state.branches[event.process_id];
					if (!branch) return prev;
					return {
						...prev,
						[channelId]: {
							...state,
							branches: {
								...state.branches,
								[event.process_id]: { ...branch, currentTool: event.tool_name },
							},
						},
					};
				}
				return prev;
			});
		} else {
			// Fallback scan for processes without a channel
			setLiveStates((prev) => {
				for (const [chId, state] of Object.entries(prev)) {
					if (event.process_type === "worker" && state.workers[event.process_id]) {
						const worker = state.workers[event.process_id];
						return {
							...prev,
							[chId]: {
								...state,
								workers: {
									...state.workers,
									[event.process_id]: { ...worker, currentTool: event.tool_name },
								},
							},
						};
					}
					if (event.process_type === "branch" && state.branches[event.process_id]) {
						const branch = state.branches[event.process_id];
						return {
							...prev,
							[chId]: {
								...state,
								branches: {
									...state.branches,
									[event.process_id]: { ...branch, currentTool: event.tool_name },
								},
							},
						};
					}
				}
				return prev;
			});
		}
	}, []);

	const handleToolCompleted = useCallback((data: unknown) => {
		const event = data as ToolCompletedEvent;
		const channelId = event.channel_id;

		if (channelId) {
			setLiveStates((prev) => {
				const state = prev[channelId];
				if (!state) return prev;

				if (event.process_type === "worker") {
					const worker = state.workers[event.process_id];
					if (!worker) return prev;
					return {
						...prev,
						[channelId]: {
							...state,
							workers: {
								...state.workers,
								[event.process_id]: {
									...worker,
									currentTool: null,
									toolCalls: worker.toolCalls + 1,
								},
							},
						},
					};
				}
				if (event.process_type === "branch") {
					const branch = state.branches[event.process_id];
					if (!branch) return prev;
					return {
						...prev,
						[channelId]: {
							...state,
							branches: {
								...state.branches,
								[event.process_id]: {
									...branch,
									currentTool: null,
									lastTool: event.tool_name,
									toolCalls: branch.toolCalls + 1,
								},
							},
						},
					};
				}
				return prev;
			});
		} else {
			setLiveStates((prev) => {
				for (const [chId, state] of Object.entries(prev)) {
					if (event.process_type === "worker" && state.workers[event.process_id]) {
						const worker = state.workers[event.process_id];
						return {
							...prev,
							[chId]: {
								...state,
								workers: {
									...state.workers,
									[event.process_id]: {
										...worker,
										currentTool: null,
										toolCalls: worker.toolCalls + 1,
									},
								},
							},
						};
					}
					if (event.process_type === "branch" && state.branches[event.process_id]) {
						const branch = state.branches[event.process_id];
						return {
							...prev,
							[chId]: {
								...state,
								branches: {
									...state.branches,
									[event.process_id]: {
										...branch,
										currentTool: null,
										lastTool: event.tool_name,
										toolCalls: branch.toolCalls + 1,
									},
								},
							},
						};
					}
				}
				return prev;
			});
		}
	}, []);

	const loadOlderMessages = useCallback((channelId: string) => {
		setLiveStates((prev) => {
			const state = prev[channelId];
			if (!state || state.loadingMore || !state.hasMore) return prev;

			const oldestItem = state.timeline[0];
			if (!oldestItem) return prev;
			const before = itemTimestamp(oldestItem);

			// Mark as loading, then kick off the fetch outside setState
			setTimeout(() => {
				api.channelMessages(channelId, PAGE_SIZE, before).then((data) => {
					setLiveStates((current) => {
						const existing = current[channelId];
						if (!existing) return current;
						return {
							...current,
							[channelId]: {
								...existing,
								timeline: [...data.items, ...existing.timeline],
								hasMore: data.has_more,
								loadingMore: false,
							},
						};
					});
				}).catch((error) => {
					console.warn(`Failed to load older messages for ${channelId}:`, error);
					setLiveStates((current) => {
						const existing = current[channelId];
						if (!existing) return current;
						return { ...current, [channelId]: { ...existing, loadingMore: false } };
					});
				});
			}, 0);

			return { ...prev, [channelId]: { ...state, loadingMore: true } };
		});
	}, []);

	const handlers = {
		inbound_message: handleInboundMessage,
		outbound_message: handleOutboundMessage,
		typing_state: handleTypingState,
		worker_started: handleWorkerStarted,
		worker_status: handleWorkerStatus,
		worker_completed: handleWorkerCompleted,
		branch_started: handleBranchStarted,
		branch_completed: handleBranchCompleted,
		tool_started: handleToolStarted,
		tool_completed: handleToolCompleted,
	};

	return { liveStates, handlers, syncStatusSnapshot, loadOlderMessages };
}
