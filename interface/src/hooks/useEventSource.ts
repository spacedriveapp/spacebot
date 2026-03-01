import { useEffect, useRef, useCallback, useState } from "react";

type EventHandler = (data: unknown) => void;

export type ConnectionState = "connecting" | "connected" | "reconnecting" | "disconnected";

interface UseEventSourceOptions {
	/** Map of SSE event types to handlers */
	handlers: Record<string, EventHandler>;
	/** Whether to connect (default true) */
	enabled?: boolean;
	/** Called when the connection recovers after a disconnect */
	onReconnect?: () => void;
}

const INITIAL_RETRY_MS = 1000;
const MAX_RETRY_MS = 30_000;
const BACKOFF_MULTIPLIER = 2;

/**
 * SSE hook with exponential backoff, connection state tracking,
 * and reconnect notification for state recovery.
 */
export function useEventSource(url: string, options: UseEventSourceOptions) {
	const { handlers, enabled = true, onReconnect } = options;
	const handlersRef = useRef(handlers);
	handlersRef.current = handlers;

	const onReconnectRef = useRef(onReconnect);
	onReconnectRef.current = onReconnect;

	const [connectionState, setConnectionState] = useState<ConnectionState>("connecting");

	const reconnectTimeout = useRef<ReturnType<typeof setTimeout> | null>(null);
	const eventSourceRef = useRef<EventSource | null>(null);
	const retryDelayRef = useRef(INITIAL_RETRY_MS);
	const hadConnectionRef = useRef(false);

	const connect = useCallback(() => {
		if (eventSourceRef.current) {
			eventSourceRef.current.close();
		}

		setConnectionState(hadConnectionRef.current ? "reconnecting" : "connecting");

		const source = new EventSource(url);
		eventSourceRef.current = source;

		source.onopen = () => {
			const wasReconnect = hadConnectionRef.current;
			hadConnectionRef.current = true;
			retryDelayRef.current = INITIAL_RETRY_MS;
			setConnectionState("connected");

			if (wasReconnect) {
				onReconnectRef.current?.();
			}
		};

		// Register a listener for each event type in handlers
		for (const eventType of Object.keys(handlersRef.current)) {
			source.addEventListener(eventType, (event: MessageEvent) => {
				try {
					const data = JSON.parse(event.data);
					handlersRef.current[eventType]?.(data);
				} catch {
					handlersRef.current[eventType]?.(event.data);
				}
			});
		}

		// Handle the lagged event from the server
		source.addEventListener("lagged", (event: MessageEvent) => {
			try {
				const data = JSON.parse(event.data);
				console.warn(`SSE lagged, skipped ${data.skipped} events`);
			} catch {
				console.warn("SSE lagged, skipped events");
			}
			// Trigger re-sync since we missed events
			onReconnectRef.current?.();
		});

		source.onerror = () => {
			source.close();
			setConnectionState("reconnecting");

			const delay = retryDelayRef.current;
			retryDelayRef.current = Math.min(delay * BACKOFF_MULTIPLIER, MAX_RETRY_MS);
			reconnectTimeout.current = setTimeout(connect, delay);
		};
	}, [url]);

	useEffect(() => {
		if (!enabled) {
			setConnectionState("disconnected");
			return;
		}

		connect();

		return () => {
			if (reconnectTimeout.current) {
				clearTimeout(reconnectTimeout.current);
			}
			eventSourceRef.current?.close();
		};
	}, [connect, enabled]);

	return { connectionState };
}
