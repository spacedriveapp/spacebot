import { createContext, useContext, useCallback, useRef, useState, useMemo, type ReactNode } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type AgentMessageEvent, type ChannelInfo, type ToolStartedEvent, type ToolCompletedEvent, type WorkerStatusEvent, type TranscriptStep } from "@/api/client";
import { useEventSource, type ConnectionState } from "@/hooks/useEventSource";
import { useChannelLiveState, type ChannelLiveState, type ActiveWorker } from "@/hooks/useChannelLiveState";

interface LiveContextValue {
	liveStates: Record<string, ChannelLiveState>;
	channels: ChannelInfo[];
	connectionState: ConnectionState;
	hasData: boolean;
	loadOlderMessages: (channelId: string) => void;
	/** Set of edge IDs ("from->to") with recent message activity */
	activeLinks: Set<string>;
	/** Flat map of all active workers across all channels, keyed by worker_id. */
	activeWorkers: Record<string, ActiveWorker & { channelId?: string; agentId: string }>;
	/** Monotonically increasing counter, bumped on every worker lifecycle SSE event. */
	workerEventVersion: number;
	/** Monotonically increasing counter, bumped on every task lifecycle SSE event. */
	taskEventVersion: number;
	/** Live transcript steps for running workers, keyed by worker_id. Built from SSE tool events. */
	liveTranscripts: Record<string, TranscriptStep[]>;
}

const LiveContext = createContext<LiveContextValue>({
	liveStates: {},
	channels: [],
	connectionState: "connecting",
	hasData: false,
	loadOlderMessages: () => {},
	activeLinks: new Set(),
	activeWorkers: {},
	workerEventVersion: 0,
	taskEventVersion: 0,
	liveTranscripts: {},
});

export function useLiveContext() {
	return useContext(LiveContext);
}

/** Duration (ms) an edge stays "active" after a message flows through it. */
const LINK_ACTIVE_DURATION = 3000;

export function LiveContextProvider({ children }: { children: ReactNode }) {
	const queryClient = useQueryClient();

	const { data: channelsData } = useQuery({
		queryKey: ["channels"],
		queryFn: api.channels,
		refetchInterval: 10_000,
	});

	const channels = channelsData?.channels ?? [];
	const { liveStates, handlers: channelHandlers, syncStatusSnapshot, loadOlderMessages } = useChannelLiveState(channels);

	// Flat active workers map + event version counter for the workers tab.
	// This is a separate piece of state from channel liveStates so the workers
	// tab can react to SSE events without scanning all channels.
	const [workerEventVersion, setWorkerEventVersion] = useState(0);
	const bumpWorkerVersion = useCallback(() => setWorkerEventVersion((v) => v + 1), []);

	const [taskEventVersion, setTaskEventVersion] = useState(0);
	const bumpTaskVersion = useCallback(() => setTaskEventVersion((v) => v + 1), []);

	// Live transcript accumulator: builds TranscriptStep[] from SSE tool events
	// for running workers. Cleared when worker completes.
	const [liveTranscripts, setLiveTranscripts] = useState<Record<string, TranscriptStep[]>>({});

	// Derive flat active workers from channel live states
	const pendingToolCallIdsRef = useRef<Record<string, Record<string, string[]>>>({});

	const activeWorkers = useMemo(() => {
		const channelAgentIds = new Map(channels.map((channel) => [channel.id, channel.agent_id]));
		const map: Record<string, ActiveWorker & { channelId?: string; agentId: string }> = {};
		for (const [channelId, state] of Object.entries(liveStates)) {
			const channelAgentId = channelAgentIds.get(channelId);
			if (!channelAgentId) continue;
			for (const [workerId, worker] of Object.entries(state.workers)) {
				map[workerId] = { ...worker, channelId, agentId: channelAgentId };
			}
		}
		return map;
	}, [liveStates, channels]);

	// Track recently active link edges
	const [activeLinks, setActiveLinks] = useState<Set<string>>(new Set());
	const timersRef = useRef<Map<string, ReturnType<typeof setTimeout>>>(new Map());

	const markEdgeActive = useCallback((from: string, to: string) => {
		// Activate both directions since the topology edge may be defined either way
		const forward = `${from}->${to}`;
		const reverse = `${to}->${from}`;
		setActiveLinks((prev) => {
			const next = new Set(prev);
			next.add(forward);
			next.add(reverse);
			return next;
		});

		for (const edgeId of [forward, reverse]) {
			const existing = timersRef.current.get(edgeId);
			if (existing) clearTimeout(existing);

			timersRef.current.set(
				edgeId,
				setTimeout(() => {
					timersRef.current.delete(edgeId);
					setActiveLinks((prev) => {
						const next = new Set(prev);
						next.delete(edgeId);
						return next;
					});
				}, LINK_ACTIVE_DURATION),
			);
		}
	}, []);

	const handleAgentMessage = useCallback(
		(data: unknown) => {
			const event = data as AgentMessageEvent;
			if (event.from_agent_id && event.to_agent_id) {
				markEdgeActive(event.from_agent_id, event.to_agent_id);
			}
		},
		[markEdgeActive],
	);

	// Wrap channel worker handlers to also bump the worker event version
	// and accumulate live transcript steps from SSE events.
	const wrappedWorkerStarted = useCallback((data: unknown) => {
		channelHandlers.worker_started(data);
		const event = data as { worker_id: string };
		setLiveTranscripts((prev) => ({ ...prev, [event.worker_id]: [] }));
		delete pendingToolCallIdsRef.current[event.worker_id];
		bumpWorkerVersion();
	}, [channelHandlers, bumpWorkerVersion]);

	const wrappedWorkerStatus = useCallback((data: unknown) => {
		channelHandlers.worker_status(data);
		const event = data as WorkerStatusEvent;
		// Push status text as an action step in the live transcript
		if (event.status && event.status !== "starting" && event.status !== "running") {
			setLiveTranscripts((prev) => {
				const steps = prev[event.worker_id] ?? [];
				const step: TranscriptStep = {
					type: "action",
					content: [{ type: "text", text: event.status }],
				};
				return { ...prev, [event.worker_id]: [...steps, step] };
			});
		}
		bumpWorkerVersion();
	}, [channelHandlers, bumpWorkerVersion]);

	const wrappedWorkerCompleted = useCallback((data: unknown) => {
		channelHandlers.worker_completed(data);
		const event = data as { worker_id: string };
		delete pendingToolCallIdsRef.current[event.worker_id];
		bumpWorkerVersion();
	}, [channelHandlers, bumpWorkerVersion]);

	const wrappedToolStarted = useCallback((data: unknown) => {
		channelHandlers.tool_started(data);
		const event = data as ToolStartedEvent;
		if (event.process_type === "worker") {
			const callId = crypto.randomUUID();
			const pendingByTool = pendingToolCallIdsRef.current[event.process_id] ?? {};
			const queue = pendingByTool[event.tool_name] ?? [];
			pendingByTool[event.tool_name] = [...queue, callId];
			pendingToolCallIdsRef.current[event.process_id] = pendingByTool;
			setLiveTranscripts((prev) => {
				const steps = prev[event.process_id] ?? [];
				const step: TranscriptStep = {
					type: "action",
					content: [{
						type: "tool_call",
						id: callId,
						name: event.tool_name,
						args: event.args || "",
					}],
				};
				return { ...prev, [event.process_id]: [...steps, step] };
			});
			bumpWorkerVersion();
		}
	}, [channelHandlers, bumpWorkerVersion]);

	const wrappedToolCompleted = useCallback((data: unknown) => {
		channelHandlers.tool_completed(data);
		const event = data as ToolCompletedEvent;
		if (event.process_type === "worker") {
			const pendingByTool = pendingToolCallIdsRef.current[event.process_id];
			const queue = pendingByTool?.[event.tool_name] ?? [];
			const [callId, ...rest] = queue;
			if (pendingByTool) {
				if (rest.length > 0) {
					pendingByTool[event.tool_name] = rest;
				} else {
					delete pendingByTool[event.tool_name];
				}
				if (Object.keys(pendingByTool).length === 0) {
					delete pendingToolCallIdsRef.current[event.process_id];
				}
			}
			setLiveTranscripts((prev) => {
				const steps = prev[event.process_id] ?? [];
				const step: TranscriptStep = {
					type: "tool_result",
					call_id: callId ?? `${event.process_id}:${event.tool_name}:${steps.length}`,
					name: event.tool_name,
					text: event.result || "",
				};
				return { ...prev, [event.process_id]: [...steps, step] };
			});
			bumpWorkerVersion();
		}
	}, [channelHandlers, bumpWorkerVersion]);

	// Merge channel handlers with agent message + task handlers
	const handlers = useMemo(
		() => ({
			...channelHandlers,
			worker_started: wrappedWorkerStarted,
			worker_status: wrappedWorkerStatus,
			worker_completed: wrappedWorkerCompleted,
			tool_started: wrappedToolStarted,
			tool_completed: wrappedToolCompleted,
			agent_message_sent: handleAgentMessage,
			agent_message_received: handleAgentMessage,
			task_updated: bumpTaskVersion,
		}),
		[channelHandlers, wrappedWorkerStarted, wrappedWorkerStatus, wrappedWorkerCompleted, wrappedToolStarted, wrappedToolCompleted, handleAgentMessage, bumpTaskVersion],
	);

	const onReconnect = useCallback(() => {
		syncStatusSnapshot();
		queryClient.invalidateQueries({ queryKey: ["channels"] });
		queryClient.invalidateQueries({ queryKey: ["status"] });
		queryClient.invalidateQueries({ queryKey: ["agents"] });
		queryClient.invalidateQueries({ queryKey: ["tasks"] });
		// Bump task version so any mounted task views refetch immediately.
		bumpTaskVersion();
	}, [syncStatusSnapshot, queryClient, bumpTaskVersion]);

	const { connectionState } = useEventSource(api.eventsUrl, {
		handlers,
		onReconnect,
	});

	// Consider app "ready" once we have any data loaded
	const hasData = channels.length > 0 || channelsData !== undefined;

	return (
		<LiveContext.Provider value={{ liveStates, channels, connectionState, hasData, loadOlderMessages, activeLinks, activeWorkers, workerEventVersion, taskEventVersion, liveTranscripts }}>
			{children}
		</LiveContext.Provider>
	);
}
