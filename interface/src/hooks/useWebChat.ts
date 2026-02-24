import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "@/api/client";

export interface ToolActivity {
	tool: string;
	status: "running" | "done";
}

export interface WebChatMessage {
	id: string;
	role: "user" | "assistant";
	content: string;
}

export function getPortalChatSessionId(agentId: string) {
	return `portal:chat:${agentId}`;
}

async function consumeSSE(
	response: Response,
	onEvent: (eventType: string, data: string) => void,
) {
	const reader = response.body?.getReader();
	if (!reader) return;

	const decoder = new TextDecoder();
	let buffer = "";

	while (true) {
		const { done, value } = await reader.read();
		if (done) break;

		buffer += decoder.decode(value, { stream: true });
		const lines = buffer.split("\n");
		buffer = lines.pop() ?? "";

		let currentEvent = "";
		let currentData = "";

		for (const line of lines) {
			if (line.startsWith("event: ")) {
				currentEvent = line.slice(7);
			} else if (line.startsWith("data: ")) {
				currentData = line.slice(6);
			} else if (line === "" && currentEvent) {
				onEvent(currentEvent, currentData);
				currentEvent = "";
				currentData = "";
			}
		}
	}
}

export function useWebChat(agentId: string) {
	const sessionId = getPortalChatSessionId(agentId);
	const [messages, setMessages] = useState<WebChatMessage[]>([]);
	const [isStreaming, setIsStreaming] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const [toolActivity, setToolActivity] = useState<ToolActivity[]>([]);
	const streamingTextRef = useRef("");

	useEffect(() => {
		let cancelled = false;
		(async () => {
			try {
				const response = await api.webChatHistory(agentId, sessionId);
				if (!response.ok || cancelled) return;
				const history: { id: string; role: string; content: string }[] = await response.json();
				if (cancelled) return;
				setMessages(
					history.map((m) => ({
						id: m.id,
						role: m.role as "user" | "assistant",
						content: m.content,
					})),
				);
			} catch { /* ignore — fresh session */ }
		})();
		return () => { cancelled = true; };
	}, [agentId, sessionId]);

	const sendMessage = useCallback(async (text: string) => {
		if (isStreaming) return;

		setError(null);
		setIsStreaming(true);
		setToolActivity([]);
		streamingTextRef.current = "";

		const userMessage: WebChatMessage = {
			id: `user-${Date.now()}`,
			role: "user",
			content: text,
		};
		setMessages((prev) => [...prev, userMessage]);

		const assistantId = `assistant-${Date.now()}`;

		try {
			const response = await api.webChatSend(agentId, sessionId, text);
			if (!response.ok) {
				throw new Error(`HTTP ${response.status}`);
			}

			await consumeSSE(response, (eventType, data) => {
				if (eventType === "tool_started") {
					try {
						const parsed = JSON.parse(data);
						setToolActivity((prev) => [
							...prev,
							{ tool: parsed.ToolStarted?.tool_name ?? "tool", status: "running" },
						]);
					} catch { /* ignore */ }
				} else if (eventType === "tool_completed") {
					try {
						const parsed = JSON.parse(data);
						const toolName = parsed.ToolCompleted?.tool_name ?? "tool";
						setToolActivity((prev) =>
							prev.map((t) =>
								t.tool === toolName && t.status === "running"
									? { ...t, status: "done" }
									: t,
							),
						);
					} catch { /* ignore */ }
				} else if (eventType === "text") {
					try {
						const parsed = JSON.parse(data);
						const content = parsed.Text ?? "";
						setMessages((prev) => {
							const existing = prev.find((m) => m.id === assistantId);
							if (existing) {
								return prev.map((m) =>
									m.id === assistantId ? { ...m, content } : m,
								);
							}
							return [...prev, { id: assistantId, role: "assistant", content }];
						});
					} catch { /* ignore */ }
				} else if (eventType === "stream_chunk") {
					try {
						const parsed = JSON.parse(data);
						const chunk = parsed.StreamChunk ?? "";
						streamingTextRef.current += chunk;
						const accumulated = streamingTextRef.current;
						setMessages((prev) => {
							const existing = prev.find((m) => m.id === assistantId);
							if (existing) {
								return prev.map((m) =>
									m.id === assistantId ? { ...m, content: accumulated } : m,
								);
							}
							return [...prev, { id: assistantId, role: "assistant", content: accumulated }];
						});
					} catch { /* ignore */ }
				}
			});
		} catch (error) {
			setError(error instanceof Error ? error.message : "Request failed");
		} finally {
			setIsStreaming(false);
			setToolActivity([]);
		}
	}, [agentId, sessionId, isStreaming]);

	return { messages, isStreaming, error, toolActivity, sendMessage };
}
