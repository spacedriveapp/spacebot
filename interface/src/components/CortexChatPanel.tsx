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
		<div className="flex flex-col gap-1 px-3 py-2">
			{activity.map((tool, index) => (
				<div
					key={`${tool.tool}-${index}`}
					className="flex items-center gap-2 rounded bg-app-darkBox/40 px-2 py-1"
				>
					{tool.status === "running" ? (
						<span className="h-1.5 w-1.5 animate-pulse rounded-full bg-warning" />
					) : (
						<span className="h-1.5 w-1.5 rounded-full bg-success" />
					)}
					<span className="font-mono text-tiny text-ink-faint">{tool.tool}</span>
					{tool.status === "done" && tool.result_preview && (
						<span className="min-w-0 flex-1 truncate text-tiny text-ink-faint/60">
							{tool.result_preview.slice(0, 80)}
						</span>
					)}
				</div>
			))}
		</div>
	);
}

export function CortexChatPanel({ agentId, channelId, onClose }: CortexChatPanelProps) {
	const { messages, isStreaming, error, toolActivity, sendMessage, newThread } = useCortexChat(agentId, channelId);
	const [input, setInput] = useState("");
	const messagesEndRef = useRef<HTMLDivElement>(null);
	const inputRef = useRef<HTMLInputElement>(null);

	// Auto-scroll on new messages or tool activity
	useEffect(() => {
		messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
	}, [messages.length, isStreaming, toolActivity.length]);

	// Focus input on mount
	useEffect(() => {
		inputRef.current?.focus();
	}, []);

	const handleSubmit = (event: React.FormEvent) => {
		event.preventDefault();
		const trimmed = input.trim();
		if (!trimmed || isStreaming) return;
		setInput("");
		sendMessage(trimmed);
	};

	return (
		<div className="flex h-full w-full flex-col bg-app-darkBox/30">
			{/* Header */}
			<div className="flex h-12 items-center justify-between border-b border-app-line/50 px-4">
				<div className="flex items-center gap-2">
					<span className="text-sm font-medium text-ink">Cortex</span>
					{channelId && (
						<span className="rounded bg-accent/10 px-1.5 py-0.5 text-tiny text-accent">
							{channelId.length > 24 ? `${channelId.slice(0, 24)}...` : channelId}
						</span>
					)}
				</div>
				<div className="flex items-center gap-1">
					<Button
						onClick={newThread}
						variant="ghost"
						size="icon"
						disabled={isStreaming}
						className="h-7 w-7"
						title="New chat"
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
				<div className="flex flex-col gap-3 p-4">
					{messages.length === 0 && !isStreaming && (
						<p className="py-8 text-center text-sm text-ink-faint">
							Ask the cortex anything
						</p>
					)}
					{messages.map((message) => (
						<div
							key={message.id}
							className={`rounded-md px-3 py-2 ${
								message.role === "user"
									? "ml-8 bg-accent/10"
									: "mr-2 bg-app-darkBox/50"
							}`}
						>
							<span className={`text-tiny font-medium ${
								message.role === "user" ? "text-accent-faint" : "text-accent"
							}`}>
								{message.role === "user" ? "admin" : "cortex"}
							</span>
							<div className="mt-0.5 text-sm text-ink-dull">
								{message.role === "assistant" ? (
									<Markdown>{message.content}</Markdown>
								) : (
									<p>{message.content}</p>
								)}
							</div>
						</div>
					))}
					{isStreaming && (
						<div className="mr-2 rounded-md bg-app-darkBox/50 px-3 py-2">
							<span className="text-tiny font-medium text-accent">cortex</span>
							<ToolActivityIndicator activity={toolActivity} />
							{toolActivity.length === 0 && (
								<div className="mt-1 flex items-center gap-1">
									<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
									<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-accent [animation-delay:0.2s]" />
									<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-accent [animation-delay:0.4s]" />
									<span className="ml-1 text-tiny text-ink-faint">thinking...</span>
								</div>
							)}
						</div>
					)}
					{error && (
						<div className="rounded-md border border-error/20 bg-error/10 px-3 py-2 text-sm text-error">
							{error}
						</div>
					)}
					<div ref={messagesEndRef} />
				</div>
			</div>

			{/* Input */}
			<form onSubmit={handleSubmit} className="border-t border-app-line/50 p-3">
				<div className="flex gap-2">
					<input
						ref={inputRef}
						type="text"
						value={input}
						onChange={(event) => setInput(event.target.value)}
						placeholder={isStreaming ? "Waiting for response..." : "Message the cortex..."}
						disabled={isStreaming}
						className="flex-1 rounded-md border border-app-line bg-app-darkBox px-3 py-1.5 text-sm text-ink placeholder:text-ink-faint focus:border-accent/60 focus:ring-2 focus:ring-accent/20 focus:outline-none disabled:opacity-50"
					/>
				<Button
					type="submit"
					disabled={isStreaming || !input.trim()}
					size="sm"
					className="bg-accent/20 text-accent hover:bg-accent/30"
				>
					Send
				</Button>
				</div>
			</form>
		</div>
	);
}
