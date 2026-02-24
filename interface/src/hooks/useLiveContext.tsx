import { createContext, useContext, useCallback, useRef, useState, useMemo, type ReactNode } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type AgentMessageEvent, type ChannelInfo } from "@/api/client";
import { useEventSource, type ConnectionState } from "@/hooks/useEventSource";
import { useChannelLiveState, type ChannelLiveState } from "@/hooks/useChannelLiveState";

interface LiveContextValue {
	liveStates: Record<string, ChannelLiveState>;
	channels: ChannelInfo[];
	connectionState: ConnectionState;
	hasData: boolean;
	loadOlderMessages: (channelId: string) => void;
	/** Set of edge IDs ("from->to") with recent message activity */
	activeLinks: Set<string>;
}

const LiveContext = createContext<LiveContextValue>({
	liveStates: {},
	channels: [],
	connectionState: "connecting",
	hasData: false,
	loadOlderMessages: () => {},
	activeLinks: new Set(),
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

	// Merge channel handlers with agent message handlers
	const handlers = useMemo(
		() => ({
			...channelHandlers,
			agent_message_sent: handleAgentMessage,
			agent_message_received: handleAgentMessage,
		}),
		[channelHandlers, handleAgentMessage],
	);

	const onReconnect = useCallback(() => {
		syncStatusSnapshot();
		queryClient.invalidateQueries({ queryKey: ["channels"] });
		queryClient.invalidateQueries({ queryKey: ["status"] });
		queryClient.invalidateQueries({ queryKey: ["agents"] });
	}, [syncStatusSnapshot, queryClient]);

	const { connectionState } = useEventSource(api.eventsUrl, {
		handlers,
		onReconnect,
	});

	// Consider app "ready" once we have any data loaded
	const hasData = channels.length > 0 || channelsData !== undefined;

	return (
		<LiveContext.Provider value={{ liveStates, channels, connectionState, hasData, loadOlderMessages, activeLinks }}>
			{children}
		</LiveContext.Provider>
	);
}
