import { createContext, useContext, useCallback, useEffect, useRef, useState, useMemo, type ReactNode } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type AgentMessageEvent, type ChannelInfo, type ToolStartedEvent, type ToolCompletedEvent, type ToolOutputEvent, type OpenCodePart, type OpenCodePartUpdatedEvent, type WorkerTextEvent } from "@/api/client";
import type { TranscriptStep as SchemaTranscriptStep } from "@/api/types";

type ToolResultStatus = "pending" | "final" | "waiting_for_input";

/** Extended TranscriptStep with live_output for streaming shell output */
type TranscriptStep = SchemaTranscriptStep & {
	live_output?: string;
	status?: ToolResultStatus;
};
import { useEventSource, type ConnectionState } from "@/hooks/useEventSource";
import { useChannelLiveState, type ChannelLiveState, type ActiveWorker } from "@/hooks/useChannelLiveState";
import { useServer } from "@/hooks/useServer";
import { NOTIFICATIONS_QUERY_KEY } from "@/hooks/useNotifications";

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
	/** Live OpenCode parts for running workers, keyed by worker_id. Parts are insertion-ordered Maps keyed by part ID. */
	liveOpenCodeParts: Record<string, Map<string, OpenCodePart>>;
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
	liveOpenCodeParts: {},
});

export function useLiveContext() {
	return useContext(LiveContext);
}

/** Duration (ms) an edge stays "active" after a message flows through it. */
const LINK_ACTIVE_DURATION = 3000;

function toolResultStatusFromText(result: string): ToolResultStatus {
	if (
		result.includes('"waiting_for_input":true') ||
		result.includes('"waiting_for_input": true') ||
		result.includes("Command appears to be waiting for interactive input.")
	) {
		return "waiting_for_input";
	}
	return "final";
}

function appendLiveOutput(existingOutput: string | undefined, line: string) {
	if (!existingOutput) return `${line}\n`;
	return `${existingOutput}${line}\n`;
}

export function LiveContextProvider({ children, onBootstrapped }: { children: ReactNode; onBootstrapped?: () => void }) {
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

	// Live OpenCode parts: per-worker insertion-ordered Map keyed by part ID.
	// Updated via opencode_part_updated SSE events. Cleared when worker completes.
	const [liveOpenCodeParts, setLiveOpenCodeParts] = useState<Record<string, Map<string, OpenCodePart>>>({});

	// Derive flat active workers from channel live states
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
		setLiveOpenCodeParts((prev) => ({ ...prev, [event.worker_id]: new Map() }));
		bumpWorkerVersion();
	}, [channelHandlers, bumpWorkerVersion]);

	const wrappedWorkerStatus = useCallback((data: unknown) => {
		channelHandlers.worker_status(data);
		// Status text comes from set_status tool calls which already appear as
		// paired tool_started/tool_completed events in the transcript. No need
		// to duplicate them as standalone text steps.
		bumpWorkerVersion();
	}, [channelHandlers, bumpWorkerVersion]);

	const wrappedWorkerIdle = useCallback((data: unknown) => {
		channelHandlers.worker_idle(data);
		bumpWorkerVersion();
	}, [channelHandlers, bumpWorkerVersion]);

	const wrappedWorkerCompleted = useCallback((data: unknown) => {
		channelHandlers.worker_completed(data);
		const event = data as { worker_id: string };
		// Clean up live OpenCode parts — persisted transcript takes over
		setLiveOpenCodeParts((prev) => {
			const next = { ...prev };
			delete next[event.worker_id];
			return next;
		});
		bumpWorkerVersion();
	}, [channelHandlers, bumpWorkerVersion]);

	const wrappedToolStarted = useCallback((data: unknown) => {
		channelHandlers.tool_started(data);
		const event = data as ToolStartedEvent;
		if (event.process_type === "worker") {
			const callId = event.call_id || `${event.process_id}:${event.tool_name}:started`;
			setLiveTranscripts((prev) => {
				const steps = prev[event.process_id] ?? [];
				const pendingResultIndexByCallId = steps.findIndex(
					(step) => step.type === "tool_result" && step.call_id === callId,
				);
				const pendingResultIndex = pendingResultIndexByCallId >= 0
					? pendingResultIndexByCallId
					: steps.findIndex(
						(step) =>
							step.type === "tool_result" &&
							step.name === event.tool_name &&
							(step as TranscriptStep).status === "pending" &&
							step.text === "",
					);
				const pendingResult = pendingResultIndex >= 0 ? steps[pendingResultIndex] : null;
				const nextSteps = pendingResult
					? steps.filter((_, index) => index !== pendingResultIndex)
					: steps;
				const step: TranscriptStep = {
					type: "action",
					content: [{
						type: "tool_call",
						id: callId,
						name: event.tool_name,
						args: event.args || "",
					}],
				};
				const retargetedResult = pendingResult
					? {...pendingResult, call_id: callId}
					: null;
				return {
					...prev,
					[event.process_id]: retargetedResult
						? [...nextSteps, step, retargetedResult]
						: [...nextSteps, step],
				};
			});
			bumpWorkerVersion();
		}
	}, [channelHandlers, bumpWorkerVersion]);

	const wrappedToolCompleted = useCallback((data: unknown) => {
		channelHandlers.tool_completed(data);
		const event = data as ToolCompletedEvent;
		if (event.process_type === "worker") {
			const callId = event.call_id || `${event.process_id}:${event.tool_name}:completed`;
			setLiveTranscripts((prev) => {
				const steps = prev[event.process_id] ?? [];
				const existingIndex = steps.findIndex(
					(step) => step.type === "tool_result" && step.call_id === callId,
				);
				const step: TranscriptStep = {
					type: "tool_result",
					call_id: callId,
					name: event.tool_name,
					text: event.result || "",
					status: toolResultStatusFromText(event.result || ""),
				};
				if (existingIndex >= 0) {
					const newSteps = [...steps];
					newSteps[existingIndex] = step;
					return { ...prev, [event.process_id]: newSteps };
				}
				return { ...prev, [event.process_id]: [...steps, step] };
			});
			bumpWorkerVersion();
		}
	}, [channelHandlers, bumpWorkerVersion]);

	const handleToolOutput = useCallback((data: unknown) => {
		const event = data as ToolOutputEvent;
		if (event.process_type === "worker") {
			setLiveTranscripts((prev) => {
				const steps = prev[event.process_id] ?? [];
				// Use the stable call_id from the event to find or create the result step
				const existingIndex = steps.findIndex(
					(s) => s.type === "tool_result" && s.call_id === event.call_id
				);
				if (existingIndex >= 0) {
					// Append to existing result step with buffer size limit
					const step = steps[existingIndex];
					const existingOutput = (step as TranscriptStep).live_output ?? "";
					const combined = appendLiveOutput(existingOutput, event.line);
					// Cap at ~50KB to prevent unbounded growth during long-running commands
					const MAX_LIVE_OUTPUT_SIZE = 50000;
					const newOutput = combined.length > MAX_LIVE_OUTPUT_SIZE
						? combined.slice(-MAX_LIVE_OUTPUT_SIZE)
						: combined;
					const updatedStep = { ...step, live_output: newOutput, status: "pending" as const };
					const newSteps = [...steps];
					newSteps[existingIndex] = updatedStep;
					return { ...prev, [event.process_id]: newSteps };
				}
				// Create new result step with the event's call_id
				const step: TranscriptStep = {
					type: "tool_result",
					call_id: event.call_id,
					name: event.tool_name,
					text: "",
					live_output: appendLiveOutput(undefined, event.line),
					status: "pending",
				};
				return { ...prev, [event.process_id]: [...steps, step] };
			});
			bumpWorkerVersion();
		}
	}, [bumpWorkerVersion]);

	// Handle OpenCode part updates — upsert parts into the per-worker ordered map
	const handleOpenCodePartUpdated = useCallback((data: unknown) => {
		const event = data as OpenCodePartUpdatedEvent;
		setLiveOpenCodeParts((prev) => {
			const existing = prev[event.worker_id] ?? new Map<string, OpenCodePart>();
			const next = new Map(existing);
			next.set(event.part.id, event.part);
			return { ...prev, [event.worker_id]: next };
		});
		bumpWorkerVersion();
	}, [bumpWorkerVersion]);

	// Handle worker text — model reasoning text emitted between tool calls
	const handleWorkerText = useCallback((data: unknown) => {
		const event = data as WorkerTextEvent;
		setLiveTranscripts((prev) => {
			const steps = prev[event.worker_id] ?? [];
			const step: TranscriptStep = {
				type: "action",
				content: [{ type: "text", text: event.text }],
			};
			return { ...prev, [event.worker_id]: [...steps, step] };
		});
		bumpWorkerVersion();
	}, [bumpWorkerVersion]);

	const handleCortexChatMessage = useCallback((data: unknown) => {
		// Forward cortex chat auto-triggered messages to any listening useCortexChat hooks
		// via a DOM custom event. This avoids coupling useLiveContext to cortex chat state.
		window.dispatchEvent(new CustomEvent("cortex-chat-message", { detail: data }));
	}, []);

	const handleNotificationCreated = useCallback(() => {
		queryClient.invalidateQueries({ queryKey: NOTIFICATIONS_QUERY_KEY });
	}, [queryClient]);

	const handleNotificationUpdated = useCallback(() => {
		queryClient.invalidateQueries({ queryKey: NOTIFICATIONS_QUERY_KEY });
	}, [queryClient]);

	// Merge channel handlers with agent message + task handlers
	const handlers = useMemo(
		() => ({
			...channelHandlers,
			worker_started: wrappedWorkerStarted,
			worker_status: wrappedWorkerStatus,
			worker_idle: wrappedWorkerIdle,
			worker_completed: wrappedWorkerCompleted,
			tool_started: wrappedToolStarted,
			tool_completed: wrappedToolCompleted,
			tool_output: handleToolOutput,
			opencode_part_updated: handleOpenCodePartUpdated,
			worker_text: handleWorkerText,
			agent_message_sent: handleAgentMessage,
			agent_message_received: handleAgentMessage,
			task_updated: bumpTaskVersion,
			cortex_chat_message: handleCortexChatMessage,
			notification_created: handleNotificationCreated,
			notification_updated: handleNotificationUpdated,
		}),
		[channelHandlers, wrappedWorkerStarted, wrappedWorkerStatus, wrappedWorkerIdle, wrappedWorkerCompleted, wrappedToolStarted, wrappedToolCompleted, handleToolOutput, handleOpenCodePartUpdated, handleWorkerText, handleAgentMessage, bumpTaskVersion, handleCortexChatMessage, handleNotificationCreated, handleNotificationUpdated],
	);

	const onReconnect = useCallback(() => {
		syncStatusSnapshot();
		queryClient.invalidateQueries({ queryKey: ["channels"] });
		queryClient.invalidateQueries({ queryKey: ["status"] });
		queryClient.invalidateQueries({ queryKey: ["agents"] });
		queryClient.invalidateQueries({ queryKey: ["tasks"] });
		queryClient.invalidateQueries({ queryKey: NOTIFICATIONS_QUERY_KEY });
		// Bump task version so any mounted task views refetch immediately.
		bumpTaskVersion();
	}, [syncStatusSnapshot, queryClient, bumpTaskVersion]);

	const { serverUrl, state: serverState } = useServer();
	// Compute the SSE events URL from the current server URL so the
	// EventSource reconnects whenever the user changes servers.
	const eventsUrl = useMemo(() => api.getEventsUrl(), [serverUrl]);

	const { connectionState } = useEventSource(eventsUrl, {
		handlers,
		onReconnect,
		enabled: serverState === "connected",
	});

	// Consider app "ready" once we have any data loaded
	const hasData = channels.length > 0 || channelsData !== undefined;

	// Signal bootstrap completion to ServerProvider so the connection
	// screen is dismissed only after initial queries have resolved.
	const bootstrappedRef = useRef(false);
	useEffect(() => {
		if (hasData && !bootstrappedRef.current) {
			bootstrappedRef.current = true;
			onBootstrapped?.();
		}
	}, [hasData, onBootstrapped]);

	return (
		<LiveContext.Provider value={{ liveStates, channels, connectionState, hasData, loadOlderMessages, activeLinks, activeWorkers, workerEventVersion, taskEventVersion, liveTranscripts, liveOpenCodeParts }}>
			{children}
		</LiveContext.Provider>
	);
}
