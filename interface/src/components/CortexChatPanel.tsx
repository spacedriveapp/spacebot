import {memo, useCallback, useEffect, useRef, useState} from "react";
import {useCortexChat, type ToolActivity} from "@/hooks/useCortexChat";
import {Markdown} from "@/components/Markdown";
import {ToolCall, type ToolCallPair} from "@/components/ToolCall";
import {api, type CortexChatMessage, type CortexChatToolCall, type CortexChatThread} from "@/api/client";
import {Button} from "@/ui";
import {Popover, PopoverContent, PopoverTrigger} from "@/ui/Popover";
import {PlusSignIcon, Cancel01Icon, Clock01Icon, Delete02Icon} from "@hugeicons/core-free-icons";
import {HugeiconsIcon} from "@hugeicons/react";

interface CortexChatPanelProps {
	agentId: string;
	channelId?: string;
	onClose?: () => void;
	/** If set, automatically sent as the first message once the thread is ready. */
	initialPrompt?: string;
	/** If true, hides the header bar (useful when embedded in another dialog). */
	hideHeader?: boolean;
}

interface StarterPrompt {
	label: string;
	prompt: string;
}

const STARTER_PROMPTS: StarterPrompt[] = [
	{
		label: "Run health check",
		prompt:
			"Give me an agent health report with active risks, stale work, and the top 3 fixes to do now.",
	},
	{
		label: "Audit memories",
		prompt:
			"Audit memory quality, find stale or contradictory memories, and propose exact cleanup actions.",
	},
	{
		label: "Review workers",
		prompt:
			"List recent worker runs, inspect failures, and summarize root cause plus next actions.",
	},
	{
		label: "Draft task spec",
		prompt:
			"Turn this goal into a task spec with subtasks, then move it to ready when it is execution-ready: ",
	},
];

/** Convert a persisted CortexChatToolCall to the ToolCallPair format used by
 * the ToolCall component. */
function toToolCallPair(call: CortexChatToolCall): ToolCallPair {
	const parsedArgs = tryParseJson(call.args);
	const parsedResult = call.result ? tryParseJson(call.result) : null;
	return {
		id: call.id,
		name: call.tool,
		argsRaw: call.args,
		args: parsedArgs,
		resultRaw: call.result ?? null,
		result: parsedResult,
		status:
			call.status === "error"
				? "error"
				: call.status === "completed"
					? "completed"
					: "running",
	};
}

/** Convert a live ToolActivity (from SSE streaming) to a ToolCallPair. */
function activityToToolCallPair(activity: ToolActivity): ToolCallPair {
	const parsedArgs = tryParseJson(activity.args);
	const parsedResult = activity.result ? tryParseJson(activity.result) : null;
	return {
		id: activity.call_id,
		name: activity.tool,
		argsRaw: activity.args,
		args: parsedArgs,
		resultRaw: activity.result ?? null,
		result: parsedResult,
		status: activity.status === "done" ? "completed" : "running",
	};
}

function tryParseJson(text: string): Record<string, unknown> | null {
	if (!text || text.trim().length === 0) return null;
	try {
		const parsed = JSON.parse(text);
		if (
			typeof parsed === "object" &&
			parsed !== null &&
			!Array.isArray(parsed)
		) {
			return parsed as Record<string, unknown>;
		}
		return null;
	} catch {
		return null;
	}
}

function EmptyCortexState({
	channelId,
	onStarterPrompt,
	disabled,
}: {
	channelId?: string;
	onStarterPrompt: (prompt: string) => void;
	disabled: boolean;
}) {
	const contextHint = channelId
		? "Current channel transcript is injected for this send only."
		: "No channel transcript is injected. Operating at full agent scope.";

	return (
		<div className="mx-auto w-full max-w-md">
			<div className="rounded-2xl border border-app-line/40 bg-app-darkBox/15 p-5">
				<h3 className="font-plex text-base font-medium text-ink">
					Cortex chat
				</h3>
				<p className="mt-2 text-sm leading-relaxed text-ink-dull">
					System-level control for this agent: memory, tasks, worker inspection,
					and direct tool execution.
				</p>
				<p className="mt-2 text-tiny text-ink-faint">{contextHint}</p>

				<div className="mt-4 grid grid-cols-2 gap-2">
					{STARTER_PROMPTS.map((item) => (
						<button
							key={item.label}
							type="button"
							onClick={() => onStarterPrompt(item.prompt)}
							disabled={disabled}
							className="rounded-lg border border-app-line/35 bg-app-box/20 px-2.5 py-2 text-left text-tiny text-ink-dull transition-colors hover:border-app-line/60 hover:text-ink disabled:opacity-40"
						>
							{item.label}
						</button>
					))}
				</div>
			</div>
		</div>
	);
}

function ToolActivityIndicator({activity}: {activity: ToolActivity[]}) {
	if (activity.length === 0) return null;

	return (
		<div className="flex flex-col gap-1.5 mt-2">
			{activity.map((tool) => (
				<ToolCall key={tool.call_id} pair={activityToToolCallPair(tool)} />
			))}
		</div>
	);
}

function ThinkingIndicator() {
	return (
		<div className="flex items-center gap-1.5 pt-3 pb-1">
			<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-ink-faint" />
			<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-ink-faint [animation-delay:0.2s]" />
			<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-ink-faint [animation-delay:0.4s]" />
		</div>
	);
}

const CortexChatInput = memo(function CortexChatInput({
	onSubmit,
	isStreaming,
}: {
	onSubmit: (text: string) => void;
	isStreaming: boolean;
}) {
	const textareaRef = useRef<HTMLTextAreaElement>(null);
	const [hasText, setHasText] = useState(false);

	useEffect(() => {
		textareaRef.current?.focus();
	}, []);

	const adjustHeight = () => {
		const textarea = textareaRef.current;
		if (!textarea) return;
		textarea.style.height = "auto";
		const scrollHeight = textarea.scrollHeight;
		const maxHeight = 160;
		textarea.style.height = `${Math.min(scrollHeight, maxHeight)}px`;
		textarea.style.overflowY = scrollHeight > maxHeight ? "auto" : "hidden";
	};

	const doSubmit = () => {
		const textarea = textareaRef.current;
		if (!textarea) return;
		const trimmed = textarea.value.trim();
		if (!trimmed) return;
		textarea.value = "";
		setHasText(false);
		adjustHeight();
		onSubmit(trimmed);
	};

	const handleInput = () => {
		const value = textareaRef.current?.value ?? "";
		setHasText(value.trim().length > 0);
		adjustHeight();
	};

	const handleKeyDown = (event: React.KeyboardEvent<HTMLTextAreaElement>) => {
		if (event.key === "Enter" && !event.shiftKey) {
			event.preventDefault();
			doSubmit();
		}
	};

	return (
		<div className="rounded-xl border border-app-line/50 bg-app-box/40 backdrop-blur-xl transition-colors duration-200 hover:border-app-line/70">
			<div className="flex items-end gap-2 p-2.5">
				<textarea
					ref={textareaRef}
					onInput={handleInput}
					onKeyDown={handleKeyDown}
					placeholder={
						isStreaming ? "Waiting for response..." : "Message the cortex..."
					}
					disabled={isStreaming}
					rows={1}
					className="flex-1 resize-none bg-transparent px-1 py-1 text-sm text-ink placeholder:text-ink-faint/60 focus:outline-none disabled:opacity-40"
					style={{maxHeight: "160px"}}
				/>
				<button
					type="button"
					onClick={doSubmit}
					disabled={isStreaming || !hasText}
					className="flex h-7 w-7 shrink-0 items-center justify-center rounded-full bg-accent text-white transition-all duration-150 hover:bg-accent-deep disabled:opacity-30 disabled:hover:bg-accent"
				>
					<svg
						width="14"
						height="14"
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
	);
});

function formatRelativeTime(dateStr: string): string {
	const date = new Date(dateStr);
	const now = new Date();
	const diff = now.getTime() - date.getTime();
	const minutes = Math.floor(diff / 60_000);
	if (minutes < 1) return "just now";
	if (minutes < 60) return `${minutes}m ago`;
	const hours = Math.floor(minutes / 60);
	if (hours < 24) return `${hours}h ago`;
	const days = Math.floor(hours / 24);
	if (days < 7) return `${days}d ago`;
	return date.toLocaleDateString(undefined, {month: "short", day: "numeric"});
}

function ThreadList({
	agentId,
	currentThreadId,
	onSelectThread,
	onClose,
}: {
	agentId: string;
	currentThreadId: string | null;
	onSelectThread: (threadId: string) => void;
	onClose: () => void;
}) {
	const [threads, setThreads] = useState<CortexChatThread[]>([]);
	const [loading, setLoading] = useState(true);

	useEffect(() => {
		api.cortexChatThreads(agentId)
			.then((data) => setThreads(data.threads))
			.catch((error) => console.warn("Failed to load threads:", error))
			.finally(() => setLoading(false));
	}, [agentId]);

	const handleDelete = useCallback(
		async (event: React.MouseEvent, threadId: string) => {
			event.stopPropagation();
			try {
				await api.cortexChatDeleteThread(agentId, threadId);
				setThreads((prev) => prev.filter((t) => t.thread_id !== threadId));
			} catch (error) {
				console.warn("Failed to delete thread:", error);
			}
		},
		[agentId],
	);

	if (loading) {
		return (
			<div className="flex items-center justify-center py-6">
				<span className="text-tiny text-ink-faint">Loading threads...</span>
			</div>
		);
	}

	if (threads.length === 0) {
		return (
			<div className="flex items-center justify-center py-6">
				<span className="text-tiny text-ink-faint">No threads yet</span>
			</div>
		);
	}

	return (
		<div className="flex max-h-80 flex-col overflow-y-auto">
			{threads.map((thread) => {
				const isActive = thread.thread_id === currentThreadId;
				const preview =
					thread.preview.length > 80
						? `${thread.preview.slice(0, 80)}...`
						: thread.preview;

				return (
					<button
						key={thread.thread_id}
						type="button"
						onClick={() => {
							onSelectThread(thread.thread_id);
							onClose();
						}}
						className={`group flex items-start gap-2 px-3 py-2.5 text-left transition-colors hover:bg-app-hover/30 ${
							isActive ? "bg-app-hover/20" : ""
						}`}
					>
						<div className="min-w-0 flex-1">
							<p className="truncate text-sm text-ink">{preview}</p>
							<div className="mt-0.5 flex items-center gap-2 text-tiny text-ink-faint">
								<span>{thread.message_count} messages</span>
								<span>{formatRelativeTime(thread.last_message_at)}</span>
							</div>
						</div>
						{!isActive && (
							<button
								type="button"
								onClick={(event) => handleDelete(event, thread.thread_id)}
								className="mt-0.5 shrink-0 rounded p-0.5 text-ink-faint opacity-0 transition-all hover:bg-red-500/10 hover:text-red-400 group-hover:opacity-100"
								title="Delete thread"
							>
								<HugeiconsIcon icon={Delete02Icon} className="h-3 w-3" />
							</button>
						)}
					</button>
				);
			})}
		</div>
	);
}

const CortexMessageList = memo(function CortexMessageList({
	messages,
}: {
	messages: CortexChatMessage[];
}) {
	return (
		<>
			{messages.map((message) => (
				<div key={message.id}>
					{message.role === "user" ? (
						<div className="flex justify-end">
							<div className="max-w-[85%] rounded-2xl rounded-br-md bg-app-hover/30 px-3 py-2">
								<p className="text-sm text-ink">{message.content}</p>
							</div>
						</div>
					) : (
						<div className="flex flex-col gap-2">
							{message.tool_calls && message.tool_calls.length > 0 && (
								<div className="flex flex-col gap-1.5">
									{message.tool_calls.map((call) => (
										<ToolCall key={call.id} pair={toToolCallPair(call)} />
									))}
								</div>
							)}
							{message.content && (
								<div className="text-sm text-ink-dull">
									<Markdown>{message.content}</Markdown>
								</div>
							)}
						</div>
					)}
				</div>
			))}
		</>
	);
});

export function CortexChatPanel({
	agentId,
	channelId,
	onClose,
	initialPrompt,
	hideHeader,
}: CortexChatPanelProps) {
	const {
		messages,
		threadId,
		isStreaming,
		error,
		toolActivity,
		sendMessage,
		newThread,
		loadThread,
	} = useCortexChat(agentId, channelId, {freshThread: !!initialPrompt});
	const [threadListOpen, setThreadListOpen] = useState(false);
	const messagesEndRef = useRef<HTMLDivElement>(null);
	const initialPromptSentRef = useRef(false);

	// Auto-send initial prompt once the fresh thread is ready
	useEffect(() => {
		if (
			initialPrompt &&
			threadId &&
			!initialPromptSentRef.current &&
			!isStreaming &&
			messages.length === 0
		) {
			initialPromptSentRef.current = true;
			sendMessage(initialPrompt);
		}
	}, [initialPrompt, threadId, isStreaming, messages.length, sendMessage]);

	useEffect(() => {
		messagesEndRef.current?.scrollIntoView({behavior: "smooth"});
	}, [messages.length, isStreaming, toolActivity.length]);

	const handleSubmit = useCallback(
		(text: string) => {
			if (isStreaming) return;
			sendMessage(text);
		},
		[isStreaming, sendMessage],
	);

	const handleStarterPrompt = (prompt: string) => {
		if (isStreaming || !threadId) return;
		sendMessage(prompt);
	};

	return (
		<div className="flex h-full w-full flex-col p-2">
			{/* Header */}
			{!hideHeader && (
				<div className="flex h-10 items-center justify-between border-b border-app-line/50 px-3">
					<div className="flex items-center gap-2">
						<span className="text-sm font-medium text-ink">Cortex</span>
						{channelId && (
							<span className="rounded-full bg-app-box px-2 py-0.5 text-tiny text-ink-faint">
								{channelId.length > 20
									? `${channelId.slice(0, 20)}...`
									: channelId}
							</span>
						)}
					</div>
				<div className="flex items-center gap-0.5">
					<Popover open={threadListOpen} onOpenChange={setThreadListOpen}>
						<PopoverTrigger asChild>
							<Button
								variant="ghost"
								size="icon"
								disabled={isStreaming}
								className="h-7 w-7"
								title="Thread history"
							>
								<HugeiconsIcon icon={Clock01Icon} className="h-3.5 w-3.5" />
							</Button>
						</PopoverTrigger>
						<PopoverContent
							align="end"
							sideOffset={4}
							className="w-72 p-0"
						>
							<div className="flex items-center justify-between border-b border-app-line/40 px-3 py-2">
								<span className="text-xs font-medium text-ink-dull">Threads</span>
							</div>
							<ThreadList
								agentId={agentId}
								currentThreadId={threadId}
								onSelectThread={loadThread}
								onClose={() => setThreadListOpen(false)}
							/>
						</PopoverContent>
					</Popover>
					<Button
						onClick={newThread}
						variant="ghost"
						size="icon"
						disabled={isStreaming}
						className="h-7 w-7"
						title="New thread"
					>
						<HugeiconsIcon icon={PlusSignIcon} className="h-3.5 w-3.5" />
					</Button>
					{onClose && (
						<Button
							onClick={onClose}
							variant="ghost"
							size="icon"
							className="h-7 w-7"
							title="Close"
						>
							<HugeiconsIcon icon={Cancel01Icon} className="h-3.5 w-3.5" />
						</Button>
					)}
				</div>
				</div>
			)}

			{/* Messages */}
			<div className="min-h-0 flex-1 overflow-y-auto">
				<div className="flex flex-col gap-5 p-3 pb-4">
					<CortexMessageList messages={messages} />

					{/* Streaming state */}
					{isStreaming && (
						<div>
							<ToolActivityIndicator activity={toolActivity} />
							{!toolActivity.some((t) => t.status === "running") && (
								<ThinkingIndicator />
							)}
						</div>
					)}

					{error && (
						<div className="rounded-lg border border-red-500/20 bg-red-500/5 px-3 py-2.5 text-sm text-red-400">
							{error}
						</div>
					)}
					<div ref={messagesEndRef} />
				</div>
			</div>

			{messages.length === 0 && !isStreaming && (
				<div className="px-3 pb-2">
					<EmptyCortexState
						channelId={channelId}
						onStarterPrompt={handleStarterPrompt}
						disabled={isStreaming || !threadId}
					/>
				</div>
			)}

			{/* Input */}
			<div className="border-t border-app-line/50 p-3">
				<CortexChatInput
					onSubmit={handleSubmit}
					isStreaming={isStreaming}
				/>
			</div>
		</div>
	);
}
