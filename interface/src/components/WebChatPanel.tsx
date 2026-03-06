import { useEffect, useRef, useState } from "react";
import { Link } from "@tanstack/react-router";
import { useWebChat } from "@/hooks/useWebChat";
import { isOpenCodeWorker, type ActiveWorker } from "@/hooks/useChannelLiveState";
import { useLiveContext } from "@/hooks/useLiveContext";
import { Markdown } from "@/components/Markdown";
import { LiveDuration } from "@/components/LiveDuration";
import type { TimelineWorkerRun } from "@/api/client";

interface WebChatPanelProps {
	agentId: string;
}

function ActiveWorkersPanel({ workers, agentId }: { workers: ActiveWorker[]; agentId: string }) {
	if (workers.length === 0) return null;

	// Use neutral chrome when all workers are opencode, amber when all builtin, mixed stays amber
	const allOpenCode = workers.every(isOpenCodeWorker);
	const borderColor = allOpenCode ? "border-zinc-500/25 bg-zinc-500/5" : "border-amber-500/25 bg-amber-500/5";
	const headerColor = allOpenCode ? "text-zinc-200" : "text-amber-200";
	const dotColor = allOpenCode ? "bg-zinc-400" : "bg-amber-400";

	return (
		<div className={`rounded-lg border px-3 py-2 ${borderColor}`}>
			<div className={`mb-2 flex items-center gap-1.5 text-tiny ${headerColor}`}>
				<div className={`h-1.5 w-1.5 animate-pulse rounded-full ${dotColor}`} />
				<span>
					{workers.length} active worker{workers.length !== 1 ? "s" : ""}
				</span>
			</div>
			<div className="flex flex-col gap-1.5">
				{workers.map((worker) => {
					const oc = isOpenCodeWorker(worker);
					return (
						<Link
							key={worker.id}
							to="/agents/$agentId/workers"
							params={{ agentId }}
							search={{ worker: worker.id }}
							className={`flex min-w-0 items-center gap-2 rounded-md px-2.5 py-1.5 text-tiny transition-colors ${
								oc ? "bg-zinc-500/10 hover:bg-zinc-500/20" : "bg-amber-500/10 hover:bg-amber-500/20"
							}`}
						>
							<div className={`h-1.5 w-1.5 animate-pulse rounded-full ${oc ? "bg-zinc-400" : "bg-amber-400"}`} />
							<span className={`font-medium ${oc ? "text-zinc-300" : "text-amber-300"}`}>Worker</span>
							<span className="min-w-0 flex-1 truncate text-ink-dull">
								{worker.task}
							</span>
							<span className="shrink-0 text-ink-faint">{worker.status}</span>
							{worker.currentTool && (
								<span className={`max-w-40 shrink-0 truncate ${oc ? "text-zinc-400/80" : "text-amber-400/80"}`}>
									{worker.currentTool}
								</span>
							)}
						</Link>
					);
				})}
			</div>
		</div>
	);
}

function ChatWorkerRunItem({ item, live, agentId }: { item: TimelineWorkerRun; live?: ActiveWorker; agentId: string }) {
	const [expanded, setExpanded] = useState(!!live);
	const wasLiveRef = useRef(!!live);

	// Auto-expand when a worker becomes live after initial mount
	useEffect(() => {
		if (live && !wasLiveRef.current) {
			setExpanded(true);
		}
		wasLiveRef.current = !!live;
	}, [live]);

	const oc = isOpenCodeWorker(live ?? { task: item.task });
	const isLive = !!live;

	return (
		<div className={`rounded-lg border px-3 py-2 transition-colors ${
			oc ? "border-zinc-500/20 bg-zinc-500/5 hover:bg-zinc-500/10" : "border-amber-500/20 bg-amber-500/5 hover:bg-amber-500/10"
		}`}>
			<div className="flex min-w-0 items-center gap-2">
				<button
					type="button"
					onClick={() => {
						if (isLive || item.result) setExpanded(!expanded);
					}}
					className="min-w-0 flex-1 text-left"
				>
					<div className="flex min-w-0 items-center gap-2">
						<div className={`h-1.5 w-1.5 rounded-full ${
							isLive
								? `animate-pulse ${oc ? "bg-zinc-400" : "bg-amber-400"}`
								: `${oc ? "bg-zinc-400/50" : "bg-amber-400/50"}`
						}`} />
						<span className={`text-tiny font-medium ${oc ? "text-zinc-300" : "text-amber-300"}`}>Worker</span>
						<span className={`min-w-0 flex-1 text-tiny text-ink-dull ${
							expanded ? "whitespace-normal break-words" : "truncate"
						}`}>{item.task}</span>
						{(isLive || item.result) && (
							<span className="flex-shrink-0 text-tiny text-ink-faint">
								{expanded ? "\u25BE" : "\u25B8"}
							</span>
						)}
					</div>
				</button>
				<Link
					to="/agents/$agentId/workers"
					params={{ agentId }}
					search={{ worker: item.id }}
					className={`flex-shrink-0 rounded border px-1.5 py-0.5 text-tiny font-medium transition-colors ${
						oc
							? "border-zinc-400/30 text-zinc-300 hover:border-zinc-400/60 hover:bg-zinc-500/15"
							: "border-amber-400/30 text-amber-300 hover:border-amber-400/60 hover:bg-amber-500/15"
					}`}
				>
					Open
				</Link>
			</div>
			{expanded && isLive && live && (
				<div className="mt-1.5 flex items-center gap-3 pl-4 text-tiny text-ink-faint">
					<LiveDuration startMs={live.startedAt} />
					<span className="truncate">{live.status}</span>
					{live.currentTool && (
						<span className={`truncate ${oc ? "text-zinc-400/70" : "text-amber-400/70"}`}>{live.currentTool}</span>
					)}
					{live.toolCalls > 0 && (
						<span>{live.toolCalls} tool calls</span>
					)}
				</div>
			)}
			{expanded && !isLive && item.result && (
				<div className={`mt-1.5 rounded-md border px-3 py-2 ${
					oc ? "border-zinc-500/10 bg-zinc-500/5" : "border-amber-500/10 bg-amber-500/5"
				}`}>
					<div className="text-sm text-ink-dull">
						<Markdown className="whitespace-pre-wrap break-words">{item.result}</Markdown>
					</div>
				</div>
			)}
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

function FloatingChatInput({
	value,
	onChange,
	onSubmit,
	disabled,
	agentId,
}: {
	value: string;
	onChange: (value: string) => void;
	onSubmit: () => void;
	disabled: boolean;
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
								disabled
									? "Waiting for response..."
									: `Message ${agentId}...`
							}
							disabled={disabled}
							rows={1}
							className="flex-1 resize-none bg-transparent px-1 py-1.5 text-sm text-ink placeholder:text-ink-faint/60 focus:outline-none disabled:opacity-40"
							style={{ maxHeight: "200px" }}
						/>
						<button
							type="button"
							onClick={onSubmit}
							disabled={disabled || !value.trim()}
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

export function WebChatPanel({ agentId }: WebChatPanelProps) {
	const { sessionId, isSending, error, sendMessage } = useWebChat(agentId);
	const { liveStates } = useLiveContext();
	const [input, setInput] = useState("");
	const messagesEndRef = useRef<HTMLDivElement>(null);

	const liveState = liveStates[sessionId];
	const timeline = liveState?.timeline ?? [];
	const isTyping = liveState?.isTyping ?? false;
	const activeWorkers = Object.values(liveState?.workers ?? {});
	const hasActiveWorkers = activeWorkers.length > 0;

	// Auto-scroll on new messages or typing state changes
	useEffect(() => {
		messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
	}, [timeline.length, isTyping, activeWorkers.length]);

	const handleSubmit = () => {
		const trimmed = input.trim();
		if (!trimmed || isSending) return;
		setInput("");
		sendMessage(trimmed);
	};

	return (
		<div className="relative flex h-full w-full flex-col">
			{/* Messages */}
			<div className="flex-1 overflow-x-hidden overflow-y-auto">
				<div className="mx-auto flex max-w-2xl flex-col gap-6 px-4 py-6 pb-32">
					{hasActiveWorkers && (
						<div className="sticky top-0 z-10 bg-app/90 pb-2 pt-2 backdrop-blur-sm">
							<ActiveWorkersPanel workers={activeWorkers} agentId={agentId} />
						</div>
					)}

					{timeline.length === 0 && !isTyping && (
						<div className="flex flex-col items-center justify-center py-24">
							<p className="text-sm text-ink-faint">
								Start a conversation with {agentId}
							</p>
						</div>
					)}

					{timeline.map((item) => {
						if (item.type === "worker_run") {
							const live = liveState?.workers[item.id];
							return (
								<ChatWorkerRunItem
									key={item.id}
									item={item}
									live={live}
									agentId={agentId}
								/>
							);
						}
						if (item.type !== "message") return null;
						return (
							<div key={item.id}>
								{item.role === "user" ? (
									<div className="flex justify-end">
										<div className="max-w-[85%] min-w-0 overflow-hidden rounded-2xl rounded-br-md bg-accent/10 px-4 py-2.5">
											<p className="text-sm text-ink break-all whitespace-pre-wrap">{item.content}</p>
										</div>
									</div>
								) : (
									<div className="text-sm text-ink-dull">
										<Markdown>{item.content}</Markdown>
									</div>
								)}
							</div>
						);
					})}

					{/* Typing indicator */}
					{isTyping && <ThinkingIndicator />}

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
				disabled={isSending || isTyping}
				agentId={agentId}
			/>
		</div>
	);
}
