import {useEffect, useRef, useState} from "react";
import {Link} from "@tanstack/react-router";
import {usePortal, getPortalSessionId} from "@/hooks/usePortal";
import {isOpenCodeWorker, type ActiveWorker} from "@/hooks/useChannelLiveState";
import {useLiveContext} from "@/hooks/useLiveContext";
import {Markdown} from "@/components/Markdown";
import {ConversationSettingsPanel} from "@/components/ConversationSettingsPanel";
import {ConversationsSidebar} from "@/components/ConversationsSidebar";
import {Button} from "@/ui/Button";
import {Popover, PopoverTrigger, PopoverContent} from "@/ui/Popover";
import {api, type ConversationDefaultsResponse, type ConversationSettings} from "@/api/client";
import {useQuery, useMutation, useQueryClient} from "@tanstack/react-query";
import {Settings02Icon} from "@hugeicons/core-free-icons";
import {HugeiconsIcon} from "@hugeicons/react";

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
		textareaRef.current?.focus({preventScroll: true});
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
								disabled ? "Waiting for response..." : `Message ${agentId}...`
							}
							disabled={disabled}
							rows={1}
							className="flex-1 resize-none bg-transparent px-1 py-1.5 text-sm text-ink placeholder:text-ink-faint/60 focus:outline-none disabled:opacity-40"
							style={{maxHeight: "200px"}}
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

export function WebChatPanel({agentId}: WebChatPanelProps) {
	const queryClient = useQueryClient();
	const [activeConversationId, setActiveConversationId] = useState<string>(getPortalSessionId(agentId));
	const {isSending, error, sendMessage} = usePortal(agentId, activeConversationId);
	const {liveStates} = useLiveContext();
	const [input, setInput] = useState("");
	const scrollRef = useRef<HTMLDivElement>(null);
	const [showSettings, setShowSettings] = useState(false);
	const [settings, setSettings] = useState<ConversationSettings>({});

	// Fetch conversations list
	const { data: conversationsData, isLoading: conversationsLoading } = useQuery({
		queryKey: ["portal-conversations", agentId],
		queryFn: async () => {
			const response = await api.listPortalConversations(agentId);
			if (!response.ok) throw new Error(`HTTP ${response.status}`);
			return response.json();
		},
	});

	const conversations = conversationsData?.conversations ?? [];

	// Reset settings when switching conversations, hydrating from cached data if available
	useEffect(() => {
		const activeConv = conversations.find(
			(c: any) => c.id === activeConversationId
		);
		setSettings(activeConv?.settings ?? {});
		setShowSettings(false);
	}, [activeConversationId, agentId, conversationsData]);

	// Fetch conversation defaults
	const {data: defaults, isLoading: defaultsLoading, error: defaultsError} = useQuery<ConversationDefaultsResponse>({
		queryKey: ["conversation-defaults", agentId],
		queryFn: () => api.getConversationDefaults(agentId),
	});

	const agentsQuery = useQuery({
		queryKey: ["agents"],
		queryFn: () => api.agents(),
		staleTime: 10_000,
	});
	const agentDisplayName = agentsQuery.data?.agents.find((a) => a.id === agentId)?.display_name;

	const liveState = liveStates[activeConversationId];
	const timeline = liveState?.timeline ?? [];
	const isTyping = liveState?.isTyping ?? false;
	const activeWorkers = Object.values(liveState?.workers ?? {});
	const hasActiveWorkers = activeWorkers.length > 0;

	// Mutations
	const createConversationMutation = useMutation({
		mutationFn: async () => {
			const response = await api.createPortalConversation(agentId);
			if (!response.ok) throw new Error(`HTTP ${response.status}`);
			return response.json();
		},
		onSuccess: (data) => {
			setActiveConversationId(data.conversation.id);
			queryClient.invalidateQueries({ queryKey: ["portal-conversations", agentId] });
		},
	});

	const deleteConversationMutation = useMutation({
		mutationFn: async (id: string) => {
			const response = await api.deletePortalConversation(agentId, id);
			if (!response.ok) throw new Error(`HTTP ${response.status}`);
			return response.json();
		},
		onSuccess: (_, deletedId) => {
			if (activeConversationId === deletedId) {
				setActiveConversationId(getPortalSessionId(agentId));
			}
			queryClient.invalidateQueries({ queryKey: ["portal-conversations", agentId] });
		},
	});

	const renameConversationMutation = useMutation({
		mutationFn: async ({ id, title }: { id: string; title: string }) => {
			const response = await api.updatePortalConversation(agentId, id, title);
			if (!response.ok) throw new Error(`HTTP ${response.status}`);
			return response.json();
		},
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["portal-conversations", agentId] });
		},
	});

	const archiveConversationMutation = useMutation({
		mutationFn: async ({ id, archived }: { id: string; archived: boolean }) => {
			const response = await api.updatePortalConversation(agentId, id, undefined, archived);
			if (!response.ok) throw new Error(`HTTP ${response.status}`);
			return response.json();
		},
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["portal-conversations", agentId] });
		},
	});

	// Auto-scroll on new messages or typing state changes.
	useEffect(() => {
		const el = scrollRef.current;
		if (el) {
			el.scrollTo({top: el.scrollHeight, behavior: "smooth"});
		}
	}, [timeline.length, isTyping, activeWorkers.length]);

	const handleSubmit = () => {
		const trimmed = input.trim();
		if (!trimmed || isSending) return;
		setInput("");
		sendMessage(trimmed);
	};

	const saveSettingsMutation = useMutation({
		mutationFn: async () => {
			if (!activeConversationId) return;
			const response = await api.updatePortalConversation(agentId, activeConversationId, undefined, undefined, settings);
			if (!response.ok) throw new Error(`HTTP ${response.status}`);
			return response.json();
		},
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["portal-conversations", agentId] });
			setShowSettings(false);
		},
	});

	return (
		<div className="flex h-full w-full">
			{/* Sidebar */}
			<ConversationsSidebar
				conversations={conversations}
				activeConversationId={activeConversationId}
				onSelectConversation={setActiveConversationId}
				onCreateConversation={() => createConversationMutation.mutate()}
				onDeleteConversation={(id) => deleteConversationMutation.mutate(id)}
				onRenameConversation={(id, title) => renameConversationMutation.mutate({ id, title })}
				onArchiveConversation={(id, archived) => archiveConversationMutation.mutate({ id, archived })}
				isLoading={conversationsLoading}
			/>

			{/* Main Chat Area */}
			<div className="relative flex flex-1 flex-col">
				{/* Header */}
				<div className="flex items-center justify-between border-b border-app-line px-4 py-2">
					<div className="flex items-center gap-2">
						<h2 className="text-sm font-medium">{agentDisplayName || agentId}</h2>
						{defaults && (
							<span className="text-xs text-ink-faint">
								{defaults.available_models.find((m) => m.id === (settings.model || defaults.model))?.name
									?? settings.model ?? defaults.model}
							</span>
						)}
						{settings.response_mode === "quiet" && (
							<span className="rounded-md bg-amber-500/10 px-1.5 py-0.5 text-[10px] font-medium text-amber-400">
								Quiet
							</span>
						)}
						{settings.response_mode === "mention_only" && (
							<span className="rounded-md bg-red-500/10 px-1.5 py-0.5 text-[10px] font-medium text-red-400">
								Mention Only
							</span>
						)}
					</div>
					<Popover open={showSettings} onOpenChange={setShowSettings}>
						<PopoverTrigger asChild>
							<Button variant="ghost" size="icon" className="h-7 w-7">
								<HugeiconsIcon icon={Settings02Icon} className="h-3.5 w-3.5" />
							</Button>
						</PopoverTrigger>
						<PopoverContent align="end" sideOffset={4} collisionPadding={16} className="max-h-[80vh] w-96 overflow-y-auto p-3">
							{defaultsLoading ? (
								<div className="py-4 text-center text-xs text-ink-faint">Loading...</div>
							) : defaults ? (
								<ConversationSettingsPanel
									defaults={defaults}
									currentSettings={settings}
									onChange={setSettings}
									onSave={() => saveSettingsMutation.mutate()}
									onCancel={() => setShowSettings(false)}
									saving={saveSettingsMutation.isPending}
								/>
							) : (
								<div className="py-4 text-center text-xs text-red-400">
									{defaultsError instanceof Error ? defaultsError.message : "Failed to load settings"}
								</div>
							)}
						</PopoverContent>
					</Popover>
				</div>

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
									Start a conversation with {agentDisplayName || agentId}
								</p>
							</div>
						)}

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
		</div>
	);
}
