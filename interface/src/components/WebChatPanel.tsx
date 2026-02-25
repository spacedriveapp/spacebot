import {useEffect, useMemo, useRef, useState} from "react";
import {
	useWebChat,
	getPortalChatSessionId,
	type ToolActivity,
} from "@/hooks/useWebChat";
import type {ActiveWorker} from "@/hooks/useChannelLiveState";
import {useLiveContext} from "@/hooks/useLiveContext";
import {Markdown} from "@/components/Markdown";

interface WebChatPanelProps {
	agentId: string;
}

const ASSISTANT_DUPLICATE_WINDOW_MS = 12_000;
const ASSISTANT_FINGERPRINT_TTL_MS = 30_000;

function normalizeAssistantContent(content: string) {
	return content.trim().replace(/\s+/g, " ").toLowerCase();
}

function rememberAssistantContent(
	fingerprints: Map<string, number>,
	content: string,
	now: number,
) {
	const fingerprint = normalizeAssistantContent(content);
	if (!fingerprint) return;
	fingerprints.set(fingerprint, now);
}

function isRecentAssistantDuplicate(
	fingerprints: Map<string, number>,
	content: string,
	now: number,
) {
	const fingerprint = normalizeAssistantContent(content);
	if (!fingerprint) return false;
	const seenAt = fingerprints.get(fingerprint);
	return seenAt !== undefined && now - seenAt < ASSISTANT_DUPLICATE_WINDOW_MS;
}

function pruneAssistantFingerprints(fingerprints: Map<string, number>, now: number) {
	for (const [fingerprint, seenAt] of fingerprints.entries()) {
		if (now - seenAt > ASSISTANT_FINGERPRINT_TTL_MS) {
			fingerprints.delete(fingerprint);
		}
	}
}

function ToolActivityIndicator({activity}: {activity: ToolActivity[]}) {
	if (activity.length === 0) return null;

	return (
		<div className="flex flex-wrap items-center gap-1.5 mt-2">
			{activity.map((tool, index) => (
				<span
					key={`${tool.tool}-${index}`}
					className="inline-flex items-center gap-1.5 rounded-full bg-app-box/60 px-2.5 py-0.5"
				>
					{tool.status === "running" ? (
						<span className="h-1.5 w-1.5 animate-pulse rounded-full bg-amber-400" />
					) : (
						<span className="h-1.5 w-1.5 rounded-full bg-green-400" />
					)}
					<span className="font-mono text-tiny text-ink-faint">
						{tool.tool}
					</span>
				</span>
			))}
		</div>
	);
}

function ThinkingIndicator() {
	return (
		<div className="flex items-center gap-1.5 py-1">
			<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-ink-faint" />
			<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-ink-faint [animation-delay:0.2s]" />
			<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-ink-faint [animation-delay:0.4s]" />
		</div>
	);
}

function ActiveWorkersPanel({workers}: {workers: ActiveWorker[]}) {
	if (workers.length === 0) return null;

	return (
		<div className="rounded-lg border border-amber-500/25 bg-amber-500/5 px-3 py-2">
			<div className="mb-2 flex items-center gap-1.5 text-tiny text-amber-200">
				<div className="h-1.5 w-1.5 animate-pulse rounded-full bg-amber-400" />
				<span>
					{workers.length} active worker{workers.length !== 1 ? "s" : ""}
				</span>
			</div>
			<div className="flex flex-col gap-1.5">
				{workers.map((worker) => (
					<div
						key={worker.id}
						className="flex min-w-0 items-center gap-2 rounded-md bg-amber-500/10 px-2.5 py-1.5 text-tiny"
					>
						<span className="font-medium text-amber-300">Worker</span>
						<span className="min-w-0 flex-1 truncate text-ink-dull">
							{worker.task}
						</span>
						<span className="shrink-0 text-ink-faint">{worker.status}</span>
						{worker.currentTool && (
							<span className="max-w-40 shrink-0 truncate text-amber-400/80">
								{worker.currentTool}
							</span>
						)}
					</div>
				))}
			</div>
		</div>
	);
}

function FloatingChatInput({
	value,
	onChange,
	onSubmit,
	isStreaming,
	agentId,
}: {
	value: string;
	onChange: (value: string) => void;
	onSubmit: () => void;
	isStreaming: boolean;
	agentId: string;
}) {
	const textareaRef = useRef<HTMLTextAreaElement>(null);

	useEffect(() => {
		textareaRef.current?.focus();
	}, []);

	useEffect(() => {
		const textarea = textareaRef.current;
		if (!textarea) return;

		const adjustHeight = () => {
			textarea.style.height = "auto";
			const scrollHeight = textarea.scrollHeight;
			const maxHeight = 200;
			textarea.style.height = `${Math.min(scrollHeight, maxHeight)}px`;
			textarea.style.overflowY = scrollHeight > maxHeight ? "auto" : "hidden";
		};

		adjustHeight();
		textarea.addEventListener("input", adjustHeight);
		return () => textarea.removeEventListener("input", adjustHeight);
	}, [value]);

	const handleKeyDown = (event: React.KeyboardEvent<HTMLTextAreaElement>) => {
		if (event.key === "Enter" && !event.shiftKey) {
			event.preventDefault();
			onSubmit();
		}
	};

	return (
		<div className="absolute inset-x-0 bottom-0 flex justify-center px-4 pb-4 pt-8 bg-gradient-to-t from-app via-app/80 to-transparent pointer-events-none">
			<div className="w-full max-w-2xl pointer-events-auto">
				<div className="rounded-2xl border border-app-line/50 bg-app-box/40 backdrop-blur-xl shadow-xl transition-colors duration-200 hover:border-app-line/70">
					<div className="flex items-end gap-2 p-3">
						<textarea
							ref={textareaRef}
							value={value}
							onChange={(event) => onChange(event.target.value)}
							onKeyDown={handleKeyDown}
							placeholder={
								isStreaming
									? "Waiting for response..."
									: `Message ${agentId}...`
							}
							disabled={isStreaming}
							rows={1}
							className="flex-1 resize-none bg-transparent px-1 py-1.5 text-sm text-ink placeholder:text-ink-faint/60 focus:outline-none disabled:opacity-40"
							style={{maxHeight: "200px"}}
						/>
						<button
							type="button"
							onClick={onSubmit}
							disabled={isStreaming || !value.trim()}
							className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-accent text-white transition-all duration-150 hover:bg-accent-deep disabled:opacity-30 disabled:hover:bg-accent"
						>
							<svg
								width="16"
								height="16"
								viewBox="0 0 24 24"
								fill="none"
								stroke="currentColor"
								strokeWidth="2"
								strokeLinecap="round"
								strokeLinejoin="round"
							>
								<path d="M12 19V5M5 12l7-7 7 7" />
							</svg>
						</button>
					</div>
				</div>
			</div>
		</div>
	);
}

export function WebChatPanel({agentId}: WebChatPanelProps) {
	const {messages, isStreaming, error, toolActivity, sendMessage} =
		useWebChat(agentId);
	const {liveStates} = useLiveContext();
	const [input, setInput] = useState("");
	const [sseMessages, setSseMessages] = useState<{id: string; role: "assistant"; content: string}[]>([]);
	const messagesEndRef = useRef<HTMLDivElement>(null);
	const sessionId = getPortalChatSessionId(agentId);
	const activeWorkers = Object.values(liveStates[sessionId]?.workers ?? {});
	const hasActiveWorkers = activeWorkers.length > 0;
	const recentAssistantFingerprintsRef = useRef(new Map<string, number>());
	const fingerprintedMessageIdsRef = useRef(new Set<string>());

	// Pick up assistant messages from the global SSE stream that arrived
	// after the webchat request SSE closed (e.g. worker completion retriggers).
	const timeline = liveStates[sessionId]?.timeline;
	const seenIdsRef = useRef(new Set<string>());
	useEffect(() => {
		if (!timeline) return;
		const now = Date.now();
		pruneAssistantFingerprints(recentAssistantFingerprintsRef.current, now);

		// Seed seen IDs from webchat messages so we don't duplicate
		for (const m of messages) {
			seenIdsRef.current.add(m.id);
			if (m.role === "assistant" && !fingerprintedMessageIdsRef.current.has(m.id)) {
				fingerprintedMessageIdsRef.current.add(m.id);
				rememberAssistantContent(recentAssistantFingerprintsRef.current, m.content, now);
			}
		}

		const newMessages: {id: string; role: "assistant"; content: string}[] = [];
		for (const item of timeline) {
			if (
				item.type === "message" &&
				item.role === "assistant" &&
				!seenIdsRef.current.has(item.id)
			) {
				seenIdsRef.current.add(item.id);
				if (isRecentAssistantDuplicate(recentAssistantFingerprintsRef.current, item.content, now)) {
					continue;
				}
				fingerprintedMessageIdsRef.current.add(item.id);
				rememberAssistantContent(recentAssistantFingerprintsRef.current, item.content, now);
				newMessages.push({
					id: item.id,
					role: "assistant",
					content: item.content,
				});
			}
		}
		if (newMessages.length > 0) {
			setSseMessages((prev) => [...prev, ...newMessages]);
		}
	}, [timeline, messages]);

	// Clear SSE messages when a new webchat send starts (they'll be in history on next load)
	useEffect(() => {
		if (isStreaming) setSseMessages([]);
	}, [isStreaming]);

	const allMessages = useMemo(() => {
		const messageIds = new Set(messages.map((message) => message.id));
		const dedupedSse = sseMessages.filter((message) => !messageIds.has(message.id));
		return [...messages, ...dedupedSse];
	}, [messages, sseMessages]);

	useEffect(() => {
		messagesEndRef.current?.scrollIntoView({behavior: "smooth"});
	}, [allMessages.length, isStreaming, toolActivity.length, activeWorkers.length]);

	const handleSubmit = () => {
		const trimmed = input.trim();
		if (!trimmed || isStreaming) return;
		setInput("");
		sendMessage(trimmed);
	};

	return (
		<div className="relative flex h-full w-full flex-col">
			{/* Messages */}
			<div className="flex-1 overflow-y-auto">
				<div className="mx-auto flex max-w-2xl flex-col gap-6 px-4 py-6 pb-32">
					{hasActiveWorkers && (
						<div className="sticky top-0 z-10 bg-app/90 pb-2 pt-2 backdrop-blur-sm">
							<ActiveWorkersPanel workers={activeWorkers} />
						</div>
					)}

					{allMessages.length === 0 && !isStreaming && (
						<div className="flex flex-col items-center justify-center py-24">
							<p className="text-sm text-ink-faint">
								Start a conversation with {agentId}
							</p>
						</div>
					)}

					{allMessages.map((message) => (
						<div key={message.id}>
							{message.role === "user" ? (
								<div className="flex justify-end">
									<div className="max-w-[85%] rounded-2xl rounded-br-md bg-accent/10 px-4 py-2.5">
										<p className="text-sm text-ink">{message.content}</p>
									</div>
								</div>
							) : (
								<div className="text-sm text-ink-dull">
									<Markdown>{message.content}</Markdown>
								</div>
							)}
						</div>
					))}

				{/* Streaming state */}
				{isStreaming &&
						allMessages[allMessages.length - 1]?.role !== "assistant" && (
							<div>
								<ToolActivityIndicator activity={toolActivity} />
								{toolActivity.length === 0 && <ThinkingIndicator />}
							</div>
						)}

				{/* Inline tool activity during streaming assistant message */}
				{isStreaming &&
						allMessages[allMessages.length - 1]?.role === "assistant" &&
						toolActivity.length > 0 && (
							<ToolActivityIndicator activity={toolActivity} />
						)}

					{error && (
						<div className="rounded-lg border border-red-500/20 bg-red-500/5 px-4 py-3 text-sm text-red-400">
							{error}
						</div>
					)}
					<div ref={messagesEndRef} />
				</div>
			</div>

			{/* Floating input */}
			<FloatingChatInput
				value={input}
				onChange={setInput}
				onSubmit={handleSubmit}
				isStreaming={isStreaming}
				agentId={agentId}
			/>
		</div>
	);
}
