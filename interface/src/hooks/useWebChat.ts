import { useCallback, useState } from "react";
import { api } from "@/api/client";

export function getPortalChatSessionId(agentId: string) {
	return `portal:chat:${agentId}`;
}

/**
 * Sends messages to the webchat endpoint. The response arrives via the global
 * SSE event bus (same timeline used by regular channels) — no per-request SSE.
 */
export function useWebChat(agentId: string) {
	const sessionId = getPortalChatSessionId(agentId);
	const [isSending, setIsSending] = useState(false);
	const [error, setError] = useState<string | null>(null);

	const sendMessage = useCallback(
		async (text: string) => {
			if (isSending) return;

			setError(null);
			setIsSending(true);

			try {
				const response = await api.webChatSend(agentId, sessionId, text);
				if (!response.ok) {
					throw new Error(`HTTP ${response.status}`);
				}
			} catch (error) {
				setError(
					error instanceof Error ? error.message : "Request failed",
				);
			} finally {
				setIsSending(false);
			}
		},
		[agentId, sessionId, isSending],
	);

	return { sessionId, isSending, error, sendMessage };
}
