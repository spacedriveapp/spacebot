import {memo, useCallback, useEffect, useRef, useState} from "react";
import {Link} from "@tanstack/react-router";
import {useWebChat} from "@/hooks/useWebChat";
import {isOpenCodeWorker, type ActiveWorker} from "@/hooks/useChannelLiveState";
import type {TimelineItem} from "@/api/client";
import {useLiveContext} from "@/hooks/useLiveContext";
import {Markdown} from "@/components/Markdown";

interface WebChatPanelProps {
	agentId: string;
}

function ActiveWorkersPanel({
	workers,
	agentId,
}: {
	workers: ActiveWorker[];
	agentId: string;
}) {
	if (workers.length === 0) return null;

	// Use neutral chrome when all workers are opencode, amber when all builtin, mixed stays amber
	const allOpenCode = workers.every(isOpenCodeWorker);
	const borderColor = allOpenCode
		? "border-zinc-500/25 bg-zinc-500/5"
		: "border-amber-500/25 bg-amber-500/5";
	const headerColor = allOpenCode ? "text-zinc-200" : "text-amber-200";
	const dotColor = allOpenCode ? "bg-zinc-400" : "bg-amber-400";

	return (
		<div className={`rounded-lg border px-3 py-2 ${borderColor}`}>
			<div
				className={`mb-2 flex items-center gap-1.5 text-tiny ${headerColor}`}
			>
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
							params={{agentId}}
							search={{worker: worker.id}}
							className={`flex min-w-0 items-center gap-2 rounded-md px-2.5 py-1.5 text-tiny transition-colors ${
								oc
									? "bg-zinc-500/10 hover:bg-zinc-500/20"
									: "bg-amber-500/10 hover:bg-amber-500/20"
							}`}
						>
							<div
								className={`h-1.5 w-1.5 animate-pulse rounded-full ${oc ? "bg-zinc-400" : "bg-amber-400"}`}
							/>
							<span
								className={`font-medium ${oc ? "text-zinc-300" : "text-amber-300"}`}
							>
								Worker
							</span>
							<span className="min-w-0 flex-1 truncate text-ink-dull">
								{worker.task}
							</span>
							<span className="shrink-0 text-ink-faint">{worker.status}</span>
							{worker.currentTool && (
								<span
									className={`max-w-40 shrink-0 truncate ${oc ? "text-zinc-400/80" : "text-amber-400/80"}`}
								>
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

function ThinkingIndicator() {
	return (
		<div className="flex items-center gap-1.5 py-1">
			<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-ink-faint" />
			<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-ink-faint [animation-delay:0.2s]" />
			<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-ink-faint [animation-delay:0.4s]" />
		</div>
	);
}

const FloatingChatInput = memo(function FloatingChatInput({
	onSubmit,
	disabled,
	agentId,
}: {
	onSubmit: (text: string) => void;
	disabled: boolean;
	agentId: string;
}) {
	const textareaRef = useRef<HTMLTextAreaElement>(null);
	const [hasText, setHasText] = useState(false);

	useEffect(() => {
		textareaRef.current?.focus({preventScroll: true});
	}, []);

	const adjustHeight = () => {
		const textarea = textareaRef.current;
		if (!textarea) return;
		textarea.style.height = "auto";
		const scrollHeight = textarea.scrollHeight;
		const maxHeight = 200;
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
		<div className="absolute inset-x-0 bottom-0 flex justify-center px-4 pb-4 pt-8 bg-gradient-to-t from-app via-app/80 to-transparent pointer-events-none">
			<div className="w-full max-w-2xl pointer-events-auto">
				<div className="rounded-2xl border border-app-line/50 bg-app-box/40 backdrop-blur-xl shadow-xl transition-colors duration-200 hover:border-app-line/70">
					<div className="flex items-end gap-2 p-3">
						<textarea
							ref={textareaRef}
							onInput={handleInput}
							onKeyDown={handleKeyDown}
							placeholder={
								disabled ? "Waiting for response..." : `Message ${agentId}...`
							}
							disabled={disabled}
							rows={1}
							className="flex-1 resize-none bg-transparent px-1 py-1.5 text-sm text-ink placeholder:text-ink-faint/60 focus:outline-none disabled:opacity-40"
							style={{maxHeight: "200px"}}
						/>
						<button
							type="button"
							onClick={doSubmit}
							disabled={disabled || !hasText}
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
});

const MessageList = memo(function MessageList({
	timeline,
	isTyping,
	error,
}: {
	timeline: TimelineItem[];
	isTyping: boolean;
	error: string | null;
}) {
	return (
		<>
			{timeline.map((item) => {
				if (item.type !== "message") return null;
				return (
					<div key={item.id}>
						{item.role === "user" ? (
							<div className="flex justify-end">
								<div className="max-w-[85%] min-w-0 overflow-hidden rounded-2xl rounded-br-md bg-app-hover/30 px-4 py-2.5">
									<p className="text-sm text-ink break-all whitespace-pre-wrap">
										{item.content}
									</p>
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
		</>
	);
});

export function WebChatPanel({agentId}: WebChatPanelProps) {
	const {sessionId, isSending, error, sendMessage} = useWebChat(agentId);
	const {liveStates} = useLiveContext();
	const scrollRef = useRef<HTMLDivElement>(null);

	const liveState = liveStates[sessionId];
	const timeline = liveState?.timeline ?? [];
	const isTyping = liveState?.isTyping ?? false;
	const activeWorkers = Object.values(liveState?.workers ?? {});
	const hasActiveWorkers = activeWorkers.length > 0;

	// Auto-scroll on new messages or typing state changes.
	// Use direct scrollTo on the container instead of scrollIntoView,
	// which can propagate scroll to ancestor overflow-hidden containers
	// and shift the entire layout (hiding the top navbar).
	useEffect(() => {
		const el = scrollRef.current;
		if (el) {
			el.scrollTo({top: el.scrollHeight, behavior: "smooth"});
		}
	}, [timeline.length, isTyping, activeWorkers.length]);

	const handleSubmit = useCallback(
		(text: string) => {
			sendMessage(text);
		},
		[sendMessage],
	);

	return (
		<div className="relative flex h-full w-full flex-col">
			{/* Messages */}
			<div ref={scrollRef} className="flex-1 overflow-x-hidden overflow-y-auto">
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

					<MessageList
						timeline={timeline}
						isTyping={isTyping}
						error={error}
					/>
				</div>
			</div>

			{/* Floating input */}
			<FloatingChatInput
				onSubmit={handleSubmit}
				disabled={isSending || isTyping}
				agentId={agentId}
			/>
		</div>
	);
}
