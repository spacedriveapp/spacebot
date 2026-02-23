import { useEffect, useRef, useState } from "react";
import { useCortexChat, type ToolActivity } from "@/hooks/useCortexChat";
import { Markdown } from "@/components/Markdown";
import { Button } from "@/ui";
import { PlusSignIcon, Cancel01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";

interface CortexChatPanelProps {
	agentId: string;
	channelId?: string;
	onClose?: () => void;
}

function ToolActivityIndicator({ activity }: { activity: ToolActivity[] }) {
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
					<span className="font-mono text-tiny text-ink-faint">{tool.tool}</span>
					{tool.status === "done" && tool.result_preview && (
						<span className="min-w-0 max-w-[120px] truncate text-tiny text-ink-faint/60">
							{tool.result_preview.slice(0, 80)}
						</span>
					)}
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

function CortexChatInput({
	value,
	onChange,
	onSubmit,
	isStreaming,
}: {
	value: string;
	onChange: (value: string) => void;
	onSubmit: () => void;
	isStreaming: boolean;
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
			const maxHeight = 160;
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
		<div className="rounded-xl border border-app-line/50 bg-app-box/40 backdrop-blur-xl transition-colors duration-200 hover:border-app-line/70">
			<div className="flex items-end gap-2 p-2.5">
				<textarea
					ref={textareaRef}
					value={value}
					onChange={(event) => onChange(event.target.value)}
					onKeyDown={handleKeyDown}
					placeholder={isStreaming ? "Waiting for response..." : "Message the cortex..."}
					disabled={isStreaming}
					rows={1}
					className="flex-1 resize-none bg-transparent px-1 py-1 text-sm text-ink placeholder:text-ink-faint/60 focus:outline-none disabled:opacity-40"
					style={{ maxHeight: "160px" }}
				/>
				<button
					type="button"
					onClick={onSubmit}
					disabled={isStreaming || !value.trim()}
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
}

export function CortexChatPanel({ agentId, channelId, onClose }: CortexChatPanelProps) {
	const { messages, isStreaming, error, toolActivity, sendMessage, newThread } = useCortexChat(agentId, channelId);
	const [input, setInput] = useState("");
	const messagesEndRef = useRef<HTMLDivElement>(null);

	useEffect(() => {
		messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
	}, [messages.length, isStreaming, toolActivity.length]);

	const handleSubmit = () => {
		const trimmed = input.trim();
		if (!trimmed || isStreaming) return;
		setInput("");
		sendMessage(trimmed);
	};

	return (
		<div className="flex h-full w-full flex-col">
			{/* Header */}
			<div className="flex h-10 items-center justify-between border-b border-app-line/50 px-3">
				<div className="flex items-center gap-2">
					<span className="text-sm font-medium text-ink">Cortex</span>
					{channelId && (
						<span className="rounded-full bg-app-box px-2 py-0.5 text-tiny text-ink-faint">
							{channelId.length > 20 ? `${channelId.slice(0, 20)}...` : channelId}
						</span>
					)}
				</div>
				<div className="flex items-center gap-0.5">
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

			{/* Messages */}
			<div className="flex-1 overflow-y-auto">
				<div className="flex flex-col gap-5 p-3 pb-4">
					{messages.length === 0 && !isStreaming && (
						<div className="flex items-center justify-center py-12">
							<p className="text-sm text-ink-faint">Ask the cortex anything</p>
						</div>
					)}

					{messages.map((message) => (
						<div key={message.id}>
							{message.role === "user" ? (
								<div className="flex justify-end">
									<div className="max-w-[85%] rounded-2xl rounded-br-md bg-accent/10 px-3 py-2">
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
					{isStreaming && (
						<div>
							<ToolActivityIndicator activity={toolActivity} />
							{toolActivity.length === 0 && <ThinkingIndicator />}
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

			{/* Input */}
			<div className="border-t border-app-line/50 p-3">
				<CortexChatInput
					value={input}
					onChange={setInput}
					onSubmit={handleSubmit}
					isStreaming={isStreaming}
				/>
			</div>
		</div>
	);
}
