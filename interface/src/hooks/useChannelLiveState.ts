import { useCallback, useEffect, useRef, useState } from "react";
import { generateId } from "@/lib/id";
import {
	api,
	type BranchCompletedEvent,
	type BranchStartedEvent,
	type InboundMessageEvent,
	type OutboundMessageDeltaEvent,
	type OutboundMessageEvent,
	type TimelineItem,
	type ToolCompletedEvent,
	type ToolStartedEvent,
	type TypingStateEvent,
	type WorkerCompletedEvent,
	type WorkerStartedEvent,
	type WorkerStatusEvent,
	type WorkerIdleEvent,
	type ChannelInfo,
} from "../api/client";

export interface ActiveWorker {
	id: string;
	task: string;
	status: string;
	startedAt: number;
	toolCalls: number;
	currentTool: string | null;
	/** Whether the worker is idle (waiting for follow-up input). */
	isIdle: boolean;
	/** Whether this worker accepts follow-up input via route. */
	interactive: boolean;
	/** Worker type: "builtin", "opencode", "task", etc. */
	workerType: string;
}

/** Check whether a worker is an opencode worker (by type or task prefix). */
export function isOpenCodeWorker(worker: { workerType?: string; task?: string }): boolean {
	return worker.workerType === "opencode" || (worker.task?.startsWith("[opencode]") ?? false);
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
	streamingMessageId: string | null;
	historyLoaded: boolean;
	hasMore: boolean;
	loadingMore: boolean;
}

const PAGE_SIZE = 50;

/** Conversation/routing tools that should never appear in the channel tool call UI. */
const HIDDEN_CHANNEL_TOOLS = new Set([
	"reply", "react", "skip", "set_outcome",
	"spawn_worker", "branch", "route", "cancel",
]);

function emptyLiveState(): ChannelLiveState {
	return {
		isTyping: false,
		timeline: [],
		workers: {},
		branches: {},
		streamingMessageId: null,
		historyLoaded: false,
		hasMore: true,
		loadingMore: false,
	};
}

/** Get a sortable timestamp from any timeline item. */
function itemTimestamp(item: TimelineItem): string {
	switch (item.type) {
		case "message": return item.created_at;
		case "branch_run": return item.started_at;
		case "worker_run": return item.started_at;
		case "tool_call_run": return item.started_at;
	}
}

function itemKey(item: TimelineItem): string {
	return `${item.type}:${item.id}`;
}

function assistantMessageItem(
	id: string,
	agentId: string,
	content: string,
): TimelineItem {
	return {
		type: "message",
		id,
		role: "assistant",
		sender_name: agentId,
		sender_id: null,
		content,
		created_at: new Date().toISOString(),
	};
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
							isIdle: w.status === "idle",
							interactive: w.interactive,
							workerType: existingWorker?.workerType ?? (w.task.startsWith("[opencode]") ? "opencode" : "builtin"),
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

	// Track pending channel tool call IDs per channel+tool_name (FIFO queue).
	const pendingChannelToolCallsRef = useRef<Record<string, string[]>>({});

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
			id: `in-${generateId()}`,
			role: "user",
			sender_name: event.sender_name ?? event.sender_id,
			sender_id: event.sender_id,
			content: event.text,
			created_at: new Date().toISOString(),
			...(event.attachments && event.attachments.length > 0
				? {attachments: event.attachments}
				: {}),
		});
	}, [pushItem]);

	const handleOutboundMessage = useCallback((data: unknown) => {
		const event = data as OutboundMessageEvent;
		setLiveStates((prev) => {
			const existing = getOrCreate(prev, event.channel_id);
			const streamingMessageId = existing.streamingMessageId;
			if (streamingMessageId) {
				const streamIndex = existing.timeline.findIndex(
					(item) => item.type === "message" && item.id === streamingMessageId,
				);

				const timeline = [...existing.timeline];
				if (streamIndex >= 0) {
					const streamItem = timeline[streamIndex];
					if (streamItem.type === "message") {
						timeline[streamIndex] = { ...streamItem, content: event.text };
					}
				} else {
					timeline.push(
						assistantMessageItem(
							`out-${generateId()}`,
							event.agent_id,
							event.text,
						),
					);
				}

				return {
					...prev,
					[event.channel_id]: {
						...existing,
						timeline,
						streamingMessageId: null,
						isTyping: false,
					},
				};
			}

			return {
				...prev,
				[event.channel_id]: {
					...existing,
					timeline: [
						...existing.timeline,
						assistantMessageItem(
							`out-${generateId()}`,
							event.agent_id,
							event.text,
						),
					],
					isTyping: false,
				},
			};
		});
	}, []);

	const handleOutboundMessageDelta = useCallback((data: unknown) => {
		const event = data as OutboundMessageDeltaEvent;
		setLiveStates((prev) => {
			const existing = getOrCreate(prev, event.channel_id);
			const streamMessageId = existing.streamingMessageId;

			if (streamMessageId) {
				const streamIndex = existing.timeline.findIndex(
					(item) => item.type === "message" && item.id === streamMessageId,
				);

				if (streamIndex >= 0) {
					const timeline = [...existing.timeline];
					const streamItem = timeline[streamIndex];
					if (streamItem.type === "message") {
						timeline[streamIndex] = {
							...streamItem,
							content: event.aggregated_text,
						};
					}
					return {
						...prev,
						[event.channel_id]: { ...existing, timeline },
					};
				}

				const messageId = `stream-${generateId()}`;
				return {
					...prev,
					[event.channel_id]: {
						...existing,
						timeline: [
							...existing.timeline,
							assistantMessageItem(
								messageId,
								event.agent_id,
								event.aggregated_text,
							),
						],
						streamingMessageId: messageId,
					},
				};
			}

			const messageId = `stream-${generateId()}`;
			return {
				...prev,
				[event.channel_id]: {
					...existing,
					timeline: [
						...existing.timeline,
						assistantMessageItem(
							messageId,
							event.agent_id,
							event.aggregated_text,
						),
					],
					streamingMessageId: messageId,
				},
			};
		});
	}, []);

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
						isIdle: false,
						interactive: event.interactive ?? false,
						workerType: event.worker_type ?? "builtin",
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
							[event.worker_id]: { ...worker, status: event.status, isIdle: false },
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
									[event.worker_id]: { ...worker, status: event.status, isIdle: false },
								},
							},
						};
					}
				}
				return prev;
			});
		}
	}, [updateItem]);

	const handleWorkerIdle = useCallback((data: unknown) => {
		const event = data as WorkerIdleEvent;
		if (event.channel_id) {
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
							[event.worker_id]: { ...worker, isIdle: true },
						},
					},
				};
			});
			// Update timeline item status to idle
			updateItem(event.channel_id, event.worker_id, (item) => {
				if (item.type !== "worker_run") return item;
				return { ...item, status: "idle" };
			});
		} else {
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
									[event.worker_id]: { ...worker, isIdle: true },
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

		if (channelId && event.process_type === "channel") {
			// Skip conversation/routing tools — they're infrastructure, not user-visible.
			if (HIDDEN_CHANNEL_TOOLS.has(event.tool_name)) return;

			const toolCallId = event.call_id || `tool-${generateId()}`;
			const queueKey = `${channelId}:${event.tool_name}`;
			const queue = pendingChannelToolCallsRef.current[queueKey] ?? [];
			pendingChannelToolCallsRef.current[queueKey] = [...queue, toolCallId];

			pushItem(channelId, {
				type: "tool_call_run",
				id: toolCallId,
				tool_name: event.tool_name,
				args: event.args,
				result: null,
				status: "running",
				started_at: new Date().toISOString(),
				completed_at: null,
			});
			return;
		}

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
	}, [pushItem]);

	const handleToolCompleted = useCallback((data: unknown) => {
		const event = data as ToolCompletedEvent;
		const channelId = event.channel_id;

		if (channelId && event.process_type === "channel") {
			const queueKey = `${channelId}:${event.tool_name}`;
			const queue = pendingChannelToolCallsRef.current[queueKey] ?? [];
			const toolCallId = event.call_id || queue[0];
			if (toolCallId) {
				pendingChannelToolCallsRef.current[queueKey] = queue.filter(
					(id) => id !== toolCallId,
				);
				updateItem(channelId, toolCallId, (item) => {
					if (item.type !== "tool_call_run") return item;
					return { ...item, result: event.result, status: "completed", completed_at: new Date().toISOString() };
				});
			}
			return;
		}

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
						const existingKeys = new Set(existing.timeline.map(itemKey));
						const olderItems = data.items.filter((item) => !existingKeys.has(itemKey(item)));
						const hasMore = olderItems.length === 0 ? false : data.has_more;
						return {
							...current,
							[channelId]: {
								...existing,
								timeline: [...olderItems, ...existing.timeline],
								hasMore,
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
		outbound_message_delta: handleOutboundMessageDelta,
		typing_state: handleTypingState,
		worker_started: handleWorkerStarted,
		worker_status: handleWorkerStatus,
		worker_idle: handleWorkerIdle,
		worker_completed: handleWorkerCompleted,
		branch_started: handleBranchStarted,
		branch_completed: handleBranchCompleted,
		tool_started: handleToolStarted,
		tool_completed: handleToolCompleted,
	};

	return { liveStates, handlers, syncStatusSnapshot, loadOlderMessages };
}
