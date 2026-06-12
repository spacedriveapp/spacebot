import {useEffect, useState} from "react";
import {Link} from "@tanstack/react-router";
import {AnimatePresence, motion} from "framer-motion";
import {useMutation, useQuery, useQueryClient} from "@tanstack/react-query";
import {GearSix, X} from "@phosphor-icons/react";
import {api} from "@/api/client";
import type {ChannelInfo} from "@/api/client";
import type {
	ConversationSettings,
	ConversationDefaultsResponse,
} from "@/api/types";
import {isOpenCodeWorker, type ChannelLiveState} from "@/hooks/useChannelLiveState";
import {ConversationSettingsPanel} from "@/components/ConversationSettingsPanel";
import {
	CircleButton,
	PopoverRoot,
	PopoverTrigger,
	PopoverContent,
} from "@spacedrive/primitives";
import {formatTimeAgo, formatTimestamp} from "@/lib/format";
import {PlatformIcon} from "@/lib/platformIcons";
import {OpenCodeZenIcon} from "@/lib/providerIcons";

const VISIBLE_MESSAGES = 9;

export function ChannelCard({
	channel,
	liveState,
}: {
	channel: ChannelInfo;
	liveState: ChannelLiveState | undefined;
}) {
	const queryClient = useQueryClient();
	const isTyping = liveState?.isTyping ?? false;
	const timeline = liveState?.timeline ?? [];
	const messages = timeline.filter((item) => item.type === "message");
	const allWorkers = Object.values(liveState?.workers ?? {});
	const workers = allWorkers.filter((w) => !isOpenCodeWorker(w));
	const ocWorkers = allWorkers.filter((w) => isOpenCodeWorker(w));
	const branches = Object.values(liveState?.branches ?? {});
	const visible = messages.slice(-VISIBLE_MESSAGES);
	const botName = [...messages].reverse().find((m) => m.type === "message" && m.role !== "user")?.sender_name ?? "bot";
	const [showSettings, setShowSettings] = useState(false);
	const [settings, setSettings] = useState<ConversationSettings>({});

	const {data: defaults} = useQuery<ConversationDefaultsResponse>({
		queryKey: ["conversation-defaults", channel.agent_id],
		queryFn: () => api.getConversationDefaults(channel.agent_id),
		enabled: showSettings,
	});

	const {data: channelSettingsData} = useQuery({
		queryKey: ["channel-settings", channel.id, channel.agent_id],
		queryFn: () => api.getChannelSettings(channel.id, channel.agent_id),
		enabled: showSettings,
	});

	useEffect(() => {
		if (showSettings) {
			setSettings(channelSettingsData?.settings ?? {});
		}
	}, [channelSettingsData, showSettings]);

	const deleteChannel = useMutation({
		mutationFn: () => api.deleteChannel(channel.agent_id, channel.id),
		onSuccess: () => queryClient.invalidateQueries({queryKey: ["channels"]}),
	});

	const saveSettingsMutation = useMutation({
		mutationFn: async () => {
			const response = await api.updateChannelSettings(
				channel.id,
				channel.agent_id,
				settings,
			);
			if (!response.ok) throw new Error(`HTTP ${response.status}`);
		},
		onSuccess: () => {
			queryClient.invalidateQueries({
				queryKey: ["channel-settings", channel.id],
			});
			queryClient.invalidateQueries({queryKey: ["channels"]});
			setShowSettings(false);
		},
	});

	return (
		<Link
			to="/agents/$agentId/channels/$channelId"
			params={{agentId: channel.agent_id, channelId: channel.id}}
			className="group/card flex aspect-square flex-col overflow-hidden rounded-xl border border-app-line bg-app-dark-box transition-colors hover:border-app-line/80 hover:bg-app-box/60"
		>
			{/* Header */}
			<div className="flex items-start justify-between p-3 pb-2">
				<div className="min-w-0 flex-1">
					<div className="flex items-center gap-2">
						<PlatformIcon
							platform={channel.platform}
							className="shrink-0 text-ink-faint"
						/>
						<h3 className="truncate font-medium text-ink text-sm">
							{channel.display_name ?? channel.id}
						</h3>
					</div>
					<div className="mt-1 flex items-center gap-1.5 flex-wrap">
						{channel.display_name && (
							<span className="text-tiny text-ink-faint truncate max-w-[120px]">
								{channel.id}
							</span>
						)}
						<span className="text-tiny text-ink-faint">
							{formatTimeAgo(channel.last_activity_at)}
						</span>
						{channel.response_mode === "observe" && (
							<span className="inline-flex items-center rounded-md bg-status-warning/10 px-1.5 py-0.5 text-tiny font-medium text-status-warning">
								Observe
							</span>
						)}
						{channel.response_mode === "mention_only" && (
							<span className="inline-flex items-center rounded-md bg-red-500/10 px-1.5 py-0.5 text-tiny font-medium text-red-400">
								Mention Only
							</span>
						)}
					</div>
				</div>
				<div className="ml-2 flex shrink-0 items-center gap-1">
					<PopoverRoot open={showSettings} onOpenChange={setShowSettings}>
						<PopoverTrigger asChild>
							<CircleButton
								icon={GearSix}
								size="sm"
								onClick={(e) => {
									e.preventDefault();
									e.stopPropagation();
									setShowSettings((v) => !v);
								}}
								className="opacity-0 transition-opacity group-hover/card:opacity-100"
								title="Channel settings"
							/>
						</PopoverTrigger>
						<PopoverContent
							align="end"
							sideOffset={4}
							collisionPadding={16}
							className="max-h-[80vh] w-96 overflow-y-auto p-3"
							onClick={(e) => e.preventDefault()}
						>
							{defaults && channelSettingsData ? (
								<ConversationSettingsPanel
									defaults={defaults}
									currentSettings={settings}
									onChange={setSettings}
									onSave={() => saveSettingsMutation.mutate()}
									onCancel={() => setShowSettings(false)}
									saving={saveSettingsMutation.isPending}
								/>
							) : (
								<div className="py-4 text-center text-xs text-ink-faint">
									Loading...
								</div>
							)}
						</PopoverContent>
					</PopoverRoot>
					<CircleButton
						icon={X}
						size="sm"
						onClick={(e) => {
							e.preventDefault();
							e.stopPropagation();
							deleteChannel.mutate();
						}}
						className="opacity-0 transition-opacity group-hover/card:opacity-100"
						title="Delete channel"
					/>
				</div>
			</div>

			{/* Activity pills — always allocated */}
			<div className="flex items-start gap-1.5 px-3 pb-2">
				{workers.length > 0 && (
					<span className="inline-flex items-center gap-1 rounded-xl border border-app-line/50 bg-app-button px-1.5 py-0.5 text-xs text-ink-faint">
						<span className="h-1.5 w-1.5 animate-pulse rounded-full bg-status-warning" />
						{workers.length} worker{workers.length !== 1 ? "s" : ""}
					</span>
				)}
				{ocWorkers.length > 0 && (
					<span className="inline-flex items-center gap-1 rounded-xl border border-app-line/50 bg-app-button px-1.5 py-0.5 text-xs text-ink-faint">
						<OpenCodeZenIcon size={10} className="shrink-0" />
						{ocWorkers.length} OpenCode
					</span>
				)}
				{branches.length > 0 && (
					<span className="inline-flex items-center gap-1 rounded-xl border border-app-line/50 bg-app-button px-1.5 py-0.5 text-xs text-ink-faint">
						<span className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
						{branches.length} branch{branches.length !== 1 ? "es" : ""}
					</span>
				)}
			</div>

			{/* Message stream */}
			{(visible.length > 0 || isTyping) && (
				<div className="flex flex-1 flex-col overflow-hidden border-t border-app-line/50 p-3">
					{messages.length > VISIBLE_MESSAGES && (
						<span className="mb-1 shrink-0 text-tiny text-ink-faint">
							{messages.length - VISIBLE_MESSAGES} earlier messages
						</span>
					)}
					<div className="flex flex-1 flex-col justify-end overflow-hidden [mask-image:linear-gradient(to_bottom,transparent,black_32%)]">
						<AnimatePresence initial={false}>
							{visible.map((message) => {
								if (message.type !== "message") return null;
								return (
									<motion.div
										key={message.id}
										initial={{opacity: 0}}
										animate={{opacity: 1}}
										exit={{opacity: 0}}
										transition={{duration: 0.15}}
										className="flex h-5 shrink-0 gap-2 overflow-hidden"
									>
										<span className="shrink-0 text-tiny text-ink-faint">
											{formatTimestamp(new Date(message.created_at).getTime())}
										</span>
										<span
											className={`shrink-0 text-tiny font-medium ${
												message.role === "user"
													? "text-accent-faint"
													: "text-ink"
											}`}
										>
											{message.role === "user"
												? (message.sender_name ?? "user")
												: (message.sender_name ?? "bot")}
										</span>
										<span className="line-clamp-1 text-sm text-ink-faint">
											{message.content}
										</span>
									</motion.div>
								);
							})}
						</AnimatePresence>
						{isTyping && (
							<div className="flex h-5 shrink-0 items-center gap-2 text-xs">
								<span className="shrink-0 text-tiny text-ink-faint">now</span>
								<span className="shrink-0 text-tiny font-medium text-ink">{botName}</span>
								<span className="flex items-center gap-0.5 text-ink-faint">
									<span className="inline-block h-1 w-1 animate-pulse rounded-full bg-accent" />
									<span className="inline-block h-1 w-1 animate-pulse rounded-full bg-accent [animation-delay:0.2s]" />
									<span className="inline-block h-1 w-1 animate-pulse rounded-full bg-accent [animation-delay:0.4s]" />
								</span>
								<span className="text-ink-faint">is typing...</span>
							</div>
						)}
					</div>
				</div>
			)}
		</Link>
	);
}
