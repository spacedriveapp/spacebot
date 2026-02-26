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

	const reconnectTimeout = useRef<ReturnType<typeof setTimeout>>();
	const eventSourceRef = useRef<EventSource>();
	const retryDelayRef = useRef(INITIAL_RETRY_MS);
	const hadConnectionRef = useRef(false);
	const shouldReconnectRef = useRef(enabled);
	shouldReconnectRef.current = enabled;

	const connect = useCallback(() => {
		if (!shouldReconnectRef.current) return;

		if (reconnectTimeout.current) {
			clearTimeout(reconnectTimeout.current);
			reconnectTimeout.current = undefined;
		}

		if (eventSourceRef.current) {
			eventSourceRef.current.close();
			eventSourceRef.current = undefined;
		}

		setConnectionState(hadConnectionRef.current ? "reconnecting" : "connecting");

		const source = new EventSource(url);
		eventSourceRef.current = source;

		source.onopen = () => {
			if (eventSourceRef.current !== source) return;

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
				if (eventSourceRef.current !== source) return;

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
			if (eventSourceRef.current !== source) return;

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
			if (eventSourceRef.current !== source) return;

			source.close();
			eventSourceRef.current = undefined;

			if (!shouldReconnectRef.current) {
				setConnectionState("disconnected");
				return;
			}

			setConnectionState("reconnecting");

			const delay = retryDelayRef.current;
			retryDelayRef.current = Math.min(delay * BACKOFF_MULTIPLIER, MAX_RETRY_MS);
			if (reconnectTimeout.current) {
				clearTimeout(reconnectTimeout.current);
			}
			reconnectTimeout.current = setTimeout(connect, delay);
		};
	}, [url]);

	useEffect(() => {
		shouldReconnectRef.current = enabled;

		if (!enabled) {
			if (reconnectTimeout.current) {
				clearTimeout(reconnectTimeout.current);
				reconnectTimeout.current = undefined;
			}
			eventSourceRef.current?.close();
			eventSourceRef.current = undefined;
			setConnectionState("disconnected");
			return;
		}

		connect();

		return () => {
			shouldReconnectRef.current = false;
			if (reconnectTimeout.current) {
				clearTimeout(reconnectTimeout.current);
				reconnectTimeout.current = undefined;
			}
			eventSourceRef.current?.close();
			eventSourceRef.current = undefined;
		};
	}, [connect, enabled]);

	return { connectionState };
}
