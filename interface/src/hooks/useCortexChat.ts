import { useCallback, useEffect, useRef, useState } from "react";
import { api, type CortexChatMessage, type CortexChatToolCall } from "@/api/client";
import { generateId } from "@/lib/id";

export interface ToolActivity {
	tool: string;
	call_id: string;
	args: string;
	status: "running" | "done";
	result?: string;
	result_preview?: string;
}

/** Parse SSE events from a ReadableStream response body. */
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

function generateThreadId(): string {
	return generateId();
}

export function useCortexChat(agentId: string, channelId?: string, options?: { freshThread?: boolean }) {
	const [messages, setMessages] = useState<CortexChatMessage[]>([]);
	const [threadId, setThreadId] = useState<string | null>(null);
	const [isStreaming, setIsStreaming] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const [toolActivity, setToolActivity] = useState<ToolActivity[]>([]);
	const loadedRef = useRef(false);

	// Load latest thread on mount, or start fresh if requested
	useEffect(() => {
		if (loadedRef.current) return;
		loadedRef.current = true;

		if (options?.freshThread) {
			setThreadId(generateThreadId());
			return;
		}

		api.cortexChatMessages(agentId).then((data) => {
			setThreadId(data.thread_id);
			setMessages(data.messages);
		}).catch((error) => {
			console.warn("Failed to load cortex chat history:", error);
			setThreadId(generateThreadId());
		});
	}, [agentId]);

	const sendMessage = useCallback(async (text: string) => {
		if (isStreaming || !threadId) return;

		setError(null);
		setIsStreaming(true);
		setToolActivity([]);

		// Optimistically add user message
		const userMessage: CortexChatMessage = {
			id: `tmp-${Date.now()}`,
			thread_id: threadId,
			role: "user",
			content: text,
			channel_context: channelId ?? null,
			created_at: new Date().toISOString(),
		};
		setMessages((prev) => [...prev, userMessage]);

		try {
			const response = await api.cortexChatSend(agentId, threadId, text, channelId);
			if (!response.ok) {
				throw new Error(`HTTP ${response.status}`);
			}

		await consumeSSE(response, (eventType, data) => {
			if (eventType === "tool_started") {
				try {
					const parsed = JSON.parse(data);
					setToolActivity((prev) => [
						...prev,
						{
							tool: parsed.tool,
							call_id: parsed.call_id ?? "",
							args: parsed.args ?? "",
							status: "running",
						},
					]);
				} catch { /* ignore */ }
			} else if (eventType === "tool_completed") {
				try {
					const parsed = JSON.parse(data);
					setToolActivity((prev) =>
						prev.map((t) =>
							t.call_id === parsed.call_id && t.status === "running"
								? {
									...t,
									status: "done",
									result: parsed.result,
									result_preview: parsed.result_preview,
								}
								: t,
						),
					);
				} catch { /* ignore */ }
			} else if (eventType === "done") {
				try {
					const parsed = JSON.parse(data);
					const toolCalls: CortexChatToolCall[] | undefined =
						Array.isArray(parsed.tool_calls) && parsed.tool_calls.length > 0
							? parsed.tool_calls
							: undefined;
					const assistantMessage: CortexChatMessage = {
						id: `resp-${Date.now()}`,
						thread_id: threadId,
						role: "assistant",
						content: parsed.full_text,
						channel_context: channelId ?? null,
						created_at: new Date().toISOString(),
						tool_calls: toolCalls,
					};
					setMessages((prev) => [...prev, assistantMessage]);
				} catch {
					setError("Failed to parse response");
				}
			} else if (eventType === "error") {
				try {
					const parsed = JSON.parse(data);
					setError(parsed.message);
				} catch {
					setError("Unknown error");
				}
			}
		});
		} catch (error) {
			setError(error instanceof Error ? error.message : "Request failed");
		} finally {
			setIsStreaming(false);
			setToolActivity([]);
		}
	}, [agentId, channelId, threadId, isStreaming]);

	// Listen for auto-triggered cortex chat messages (e.g. worker results)
	// delivered via the global SSE stream.
	useEffect(() => {
		const handler = (event: Event) => {
			const detail = (event as CustomEvent).detail;
			if (!detail || detail.agent_id !== agentId) return;
			if (threadId && detail.thread_id !== threadId) return;

			const toolCalls = Array.isArray(detail.tool_calls) && detail.tool_calls.length > 0
				? detail.tool_calls
				: undefined;

			const message: CortexChatMessage = {
				id: `auto-${Date.now()}`,
				thread_id: detail.thread_id,
				role: "assistant",
				content: detail.content,
				channel_context: channelId ?? null,
				created_at: new Date().toISOString(),
				tool_calls: toolCalls,
			};
			setMessages((prev) => [...prev, message]);
		};

		window.addEventListener("cortex-chat-message", handler);
		return () => window.removeEventListener("cortex-chat-message", handler);
	}, [agentId, threadId, channelId]);

	const newThread = useCallback(() => {
		setThreadId(generateThreadId());
		setMessages([]);
		setError(null);
		setToolActivity([]);
	}, []);

	const loadThread = useCallback(async (targetThreadId: string) => {
		if (isStreaming) return;
		try {
			const data = await api.cortexChatMessages(agentId, targetThreadId);
			setThreadId(data.thread_id);
			setMessages(data.messages);
			setError(null);
			setToolActivity([]);
		} catch (error) {
			console.warn("Failed to load thread:", error);
		}
	}, [agentId, isStreaming]);

	return { messages, threadId, isStreaming, error, toolActivity, sendMessage, newThread, loadThread };
}
