import { useMemo, useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import {
	api,
	type BindingInfo,
	type CreateBindingRequest,
	type MessagingStatusResponse,
} from "@/api/client";
import { ChannelCard } from "@/components/ChannelCard";
import { Modal } from "@/ui/Modal";
import { platformColor } from "@/lib/format";
import type { ChannelLiveState } from "@/hooks/useChannelLiveState";

interface AgentChannelsProps {
	agentId: string;
	liveStates: Record<string, ChannelLiveState>;
}

type Platform = "discord" | "slack" | "webhook";
type ModalStep = "select" | "configure";

interface BindingFormData {
	platform: Platform;
	// Discord
	guild_id: string;
	channel_ids: string;
	dm_allowed_users: string;
	// Slack
	workspace_id: string;
	slack_channel_ids: string;
	slack_dm_allowed_users: string;
	// Credentials (only when platform not yet configured)
	discord_token: string;
	slack_bot_token: string;
	slack_app_token: string;
}

function defaultFormData(): BindingFormData {
	return {
		platform: "discord",
		guild_id: "",
		channel_ids: "",
		dm_allowed_users: "",
		workspace_id: "",
		slack_channel_ids: "",
		slack_dm_allowed_users: "",
		discord_token: "",
		slack_bot_token: "",
		slack_app_token: "",
	};
}

export function AgentChannels({ agentId, liveStates }: AgentChannelsProps) {
	const queryClient = useQueryClient();
	const [searchQuery, setSearchQuery] = useState("");
	const [isModalOpen, setIsModalOpen] = useState(false);
	const [modalStep, setModalStep] = useState<ModalStep>("select");
	const [formData, setFormData] = useState<BindingFormData>(defaultFormData());
	const [deleteConfirm, setDeleteConfirm] = useState<BindingInfo | null>(null);

	const { data: channelsData, isLoading } = useQuery({
		queryKey: ["channels"],
		queryFn: api.channels,
		refetchInterval: 10_000,
	});

	const { data: messagingStatus } = useQuery({
		queryKey: ["messaging-status"],
		queryFn: api.messagingStatus,
		staleTime: 30_000,
	});

	const { data: bindingsData } = useQuery({
		queryKey: ["bindings", agentId],
		queryFn: () => api.bindings(agentId),
		enabled: isModalOpen,
	});

	const createMutation = useMutation({
		mutationFn: (request: CreateBindingRequest) => api.createBinding(request),
		onSuccess: (result) => {
			queryClient.invalidateQueries({ queryKey: ["bindings", agentId] });
			queryClient.invalidateQueries({ queryKey: ["messaging-status"] });
			if (result.restart_required) {
				// Stay on modal to show the restart message
			} else {
				closeModal();
			}
		},
	});

	const deleteMutation = useMutation({
		mutationFn: (binding: BindingInfo) =>
			api.deleteBinding({
				agent_id: binding.agent_id,
				channel: binding.channel,
				guild_id: binding.guild_id ?? undefined,
				workspace_id: binding.workspace_id ?? undefined,
				chat_id: binding.chat_id ?? undefined,
			}),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["bindings", agentId] });
			setDeleteConfirm(null);
		},
	});

	const channels = useMemo(() => {
		const agentChannels = (channelsData?.channels ?? []).filter(
			(c) => c.agent_id === agentId,
		);
		if (!searchQuery) return agentChannels;
		const query = searchQuery.toLowerCase();
		return agentChannels.filter(
			(c) =>
				c.id.toLowerCase().includes(query) ||
				(c.display_name && c.display_name.toLowerCase().includes(query)) ||
				(c.platform && c.platform.toLowerCase().includes(query)),
		);
	}, [channelsData, agentId, searchQuery]);

	const openModal = () => {
		setFormData(defaultFormData());
		setModalStep("select");
		createMutation.reset();
		setIsModalOpen(true);
	};

	const closeModal = () => {
		setIsModalOpen(false);
		setFormData(defaultFormData());
		setModalStep("select");
		createMutation.reset();
	};

	const selectPlatform = (platform: Platform) => {
		setFormData((d) => ({ ...d, platform }));
		setModalStep("configure");
	};

	const handleSave = () => {
		const request: CreateBindingRequest = {
			agent_id: agentId,
			channel: formData.platform,
		};

		if (formData.platform === "discord") {
			if (formData.guild_id.trim()) request.guild_id = formData.guild_id.trim();
			if (formData.channel_ids.trim()) {
				request.channel_ids = formData.channel_ids
					.split(",")
					.map((s) => s.trim())
					.filter(Boolean);
			}
			if (formData.dm_allowed_users.trim()) {
				request.dm_allowed_users = formData.dm_allowed_users
					.split(",")
					.map((s) => s.trim())
					.filter(Boolean);
			}
			if (formData.discord_token.trim()) {
				request.platform_credentials = {
					discord_token: formData.discord_token.trim(),
				};
			}
		} else if (formData.platform === "slack") {
			if (formData.workspace_id.trim())
				request.workspace_id = formData.workspace_id.trim();
			if (formData.slack_channel_ids.trim()) {
				request.channel_ids = formData.slack_channel_ids
					.split(",")
					.map((s) => s.trim())
					.filter(Boolean);
			}
			if (formData.slack_dm_allowed_users.trim()) {
				request.dm_allowed_users = formData.slack_dm_allowed_users
					.split(",")
					.map((s) => s.trim())
					.filter(Boolean);
			}
			if (formData.slack_bot_token.trim() && formData.slack_app_token.trim()) {
				request.platform_credentials = {
					slack_bot_token: formData.slack_bot_token.trim(),
					slack_app_token: formData.slack_app_token.trim(),
				};
			}
		}

		createMutation.mutate(request);
	};

	const needsCredentials = (platform: Platform): boolean => {
		if (!messagingStatus) return false;
		const status = messagingStatus[platform];
		return !status.configured;
	};

	return (
		<div className="flex h-full flex-col">
			<div className="flex items-center gap-3 border-b border-app-line/50 bg-app-darkBox/20 px-6 py-3">
				<div className="relative flex-1">
					<input
						type="text"
						placeholder="Search channels..."
						value={searchQuery}
						onChange={(event) => setSearchQuery(event.target.value)}
						className="w-full rounded-md border border-app-line bg-app-darkBox px-3 py-1.5 pl-8 text-sm text-ink placeholder:text-ink-faint focus:border-accent/50 focus:outline-none"
					/>
					<svg
						className="absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-ink-faint"
						viewBox="0 0 16 16"
						fill="none"
						stroke="currentColor"
						strokeWidth="1.5"
					>
						<circle cx="6.5" cy="6.5" r="5" />
						<path d="M10.5 10.5L14 14" />
					</svg>
				</div>
			</div>
			<div className="flex-1 overflow-y-auto p-6">
				{isLoading ? (
					<div className="flex items-center gap-2 text-ink-dull">
						<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
						Loading channels...
					</div>
				) : (
					<div className="grid gap-3 grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 2xl:grid-cols-4">
						{channels.map((channel) => (
							<ChannelCard
								key={channel.id}
								channel={channel}
								liveState={liveStates[channel.id]}
							/>
						))}

						{/* Connect Channel card */}
						<button
							onClick={openModal}
							className="flex min-h-[100px] flex-col items-center justify-center gap-2 rounded-lg border-2 border-dashed border-app-line/60 bg-transparent transition-colors hover:border-accent/50 hover:bg-app-darkBox/30"
						>
							<div className="flex h-8 w-8 items-center justify-center rounded-full border border-app-line/80 text-ink-faint">
								<svg
									viewBox="0 0 16 16"
									fill="none"
									stroke="currentColor"
									strokeWidth="1.5"
									className="h-4 w-4"
								>
									<path d="M8 3v10M3 8h10" />
								</svg>
							</div>
							<span className="text-sm text-ink-faint">Connect Channel</span>
						</button>
					</div>
				)}
			</div>

			{/* Connect Channel Modal */}
			<Modal
				isOpen={isModalOpen}
				onClose={closeModal}
				title={
					modalStep === "select"
						? "Connect Channel"
						: `Connect ${formData.platform.charAt(0).toUpperCase() + formData.platform.slice(1)}`
				}
			>
				{modalStep === "select" ? (
					<PlatformSelect
						messagingStatus={messagingStatus}
						onSelect={selectPlatform}
					/>
				) : (
					<PlatformForm
						formData={formData}
						setFormData={setFormData}
						needsCredentials={needsCredentials(formData.platform)}
						onBack={() => setModalStep("select")}
						onSave={handleSave}
						isPending={createMutation.isPending}
						result={createMutation.data}
						existingBindings={bindingsData?.bindings ?? []}
						onDeleteBinding={setDeleteConfirm}
					/>
				)}
			</Modal>

			{/* Delete Binding Confirmation */}
			<Modal
				isOpen={!!deleteConfirm}
				onClose={() => setDeleteConfirm(null)}
				title="Remove Binding?"
			>
				{deleteConfirm && (
					<>
						<p className="mb-4 text-sm text-ink-dull">
							This will remove the{" "}
							<span className={`inline-flex rounded-md px-1.5 py-0.5 text-tiny font-medium ${platformColor(deleteConfirm.channel)}`}>
								{deleteConfirm.channel}
							</span>{" "}
							binding
							{deleteConfirm.guild_id && (
								<> for guild <code className="rounded bg-app-darkBox px-1 py-0.5 text-ink">{deleteConfirm.guild_id}</code></>
							)}
							{deleteConfirm.workspace_id && (
								<> for workspace <code className="rounded bg-app-darkBox px-1 py-0.5 text-ink">{deleteConfirm.workspace_id}</code></>
							)}
							. Messages from this source will fall back to the default agent.
						</p>
						<div className="flex justify-end gap-2">
							<button
								onClick={() => setDeleteConfirm(null)}
								className="rounded-lg px-3 py-1.5 text-sm text-ink-dull transition-colors hover:text-ink"
							>
								Cancel
							</button>
							<button
								onClick={() => deleteMutation.mutate(deleteConfirm)}
								disabled={deleteMutation.isPending}
								className="rounded-lg bg-red-600 px-4 py-1.5 text-sm font-medium text-white transition-colors hover:bg-red-700 disabled:opacity-50"
							>
								{deleteMutation.isPending ? "Removing..." : "Remove"}
							</button>
						</div>
					</>
				)}
			</Modal>
		</div>
	);
}

// -- Sub-components --

function PlatformSelect({
	messagingStatus,
	onSelect,
}: {
	messagingStatus: MessagingStatusResponse | undefined;
	onSelect: (platform: Platform) => void;
}) {
	const platforms: { id: Platform; label: string; description: string }[] = [
		{
			id: "discord",
			label: "Discord",
			description: "Route messages from a Discord server or DMs.",
		},
		{
			id: "slack",
			label: "Slack",
			description: "Route messages from a Slack workspace.",
		},
		{
			id: "webhook",
			label: "Webhook",
			description: "Route programmatic messages via HTTP.",
		},
	];

	return (
		<div className="flex flex-col gap-2">
			{platforms.map((platform) => {
				const status = messagingStatus?.[platform.id];
				return (
					<button
						key={platform.id}
						onClick={() => onSelect(platform.id)}
						className="flex items-center gap-3 rounded-lg border border-app-line bg-app-darkBox p-4 text-left transition-colors hover:border-accent/50 hover:bg-app-darkBox/80"
					>
						<span
							className={`inline-flex rounded-md px-2 py-1 text-xs font-medium ${platformColor(platform.id)}`}
						>
							{platform.label}
						</span>
						<div className="min-w-0 flex-1">
							<p className="text-sm text-ink-dull">
								{platform.description}
							</p>
						</div>
						{status && (
							<span
								className={`text-tiny ${
									status.enabled
										? "text-green-400"
										: status.configured
											? "text-yellow-400"
											: "text-ink-faint"
								}`}
							>
								{status.enabled
									? "Active"
									: status.configured
										? "Configured"
										: "Not configured"}
							</span>
						)}
					</button>
				);
			})}
		</div>
	);
}

function PlatformForm({
	formData,
	setFormData,
	needsCredentials,
	onBack,
	onSave,
	isPending,
	result,
	existingBindings,
	onDeleteBinding,
}: {
	formData: BindingFormData;
	setFormData: React.Dispatch<React.SetStateAction<BindingFormData>>;
	needsCredentials: boolean;
	onBack: () => void;
	onSave: () => void;
	isPending: boolean;
	result: { success: boolean; restart_required: boolean; message: string } | undefined;
	existingBindings: BindingInfo[];
	onDeleteBinding: (binding: BindingInfo) => void;
}) {
	const platformBindings = existingBindings.filter(
		(b) => b.channel === formData.platform,
	);

	return (
		<div className="flex flex-col gap-4">
			{/* Success message */}
			{result?.success && (
				<div
					className={`rounded-lg px-3 py-2 text-sm ${
						result.restart_required
							? "bg-yellow-500/10 text-yellow-400"
							: "bg-green-500/10 text-green-400"
					}`}
				>
					{result.message}
				</div>
			)}

			{/* Credential setup when platform not configured */}
			{needsCredentials && (
				<div className="rounded-lg border border-yellow-500/30 bg-yellow-500/5 p-3">
					<p className="mb-3 text-xs text-yellow-400">
						{formData.platform === "discord"
							? "Discord is not configured yet. Add your bot token to connect."
							: "Slack is not configured yet. Add your bot and app tokens to connect."}
						{" "}A restart will be required after saving.
					</p>
					{formData.platform === "discord" && (
						<Field label="Bot Token">
							<input
								type="password"
								value={formData.discord_token}
								onChange={(e) =>
									setFormData((d) => ({
										...d,
										discord_token: e.target.value,
									}))
								}
								placeholder="Bot token from Discord Developer Portal"
								className="w-full rounded-lg border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
							/>
						</Field>
					)}
					{formData.platform === "slack" && (
						<div className="flex flex-col gap-3">
							<Field label="Bot Token">
								<input
									type="password"
									value={formData.slack_bot_token}
									onChange={(e) =>
										setFormData((d) => ({
											...d,
											slack_bot_token: e.target.value,
										}))
									}
									placeholder="xoxb-..."
									className="w-full rounded-lg border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
								/>
							</Field>
							<Field label="App Token">
								<input
									type="password"
									value={formData.slack_app_token}
									onChange={(e) =>
										setFormData((d) => ({
											...d,
											slack_app_token: e.target.value,
										}))
									}
									placeholder="xapp-..."
									className="w-full rounded-lg border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
								/>
							</Field>
						</div>
					)}
				</div>
			)}

			{/* Binding fields */}
			{formData.platform === "discord" && (
				<>
					<Field label="Guild ID (Server ID)">
						<input
							value={formData.guild_id}
							onChange={(e) =>
								setFormData((d) => ({ ...d, guild_id: e.target.value }))
							}
							placeholder="e.g. 123456789012345678"
							className="w-full rounded-lg border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
						/>
					</Field>
					<Field label="Channel IDs (optional, comma-separated)">
						<input
							value={formData.channel_ids}
							onChange={(e) =>
								setFormData((d) => ({
									...d,
									channel_ids: e.target.value,
								}))
							}
							placeholder="Leave empty for all channels in guild"
							className="w-full rounded-lg border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
						/>
					</Field>
					<Field label="DM Allowed Users (optional, comma-separated)">
						<input
							value={formData.dm_allowed_users}
							onChange={(e) =>
								setFormData((d) => ({
									...d,
									dm_allowed_users: e.target.value,
								}))
							}
							placeholder="User IDs that can DM the bot"
							className="w-full rounded-lg border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
						/>
					</Field>
				</>
			)}

			{formData.platform === "slack" && (
				<>
					<Field label="Workspace ID (Team ID)">
						<input
							value={formData.workspace_id}
							onChange={(e) =>
								setFormData((d) => ({
									...d,
									workspace_id: e.target.value,
								}))
							}
							placeholder="e.g. T01234ABCDE"
							className="w-full rounded-lg border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
						/>
					</Field>
					<Field label="Channel IDs (optional, comma-separated)">
						<input
							value={formData.slack_channel_ids}
							onChange={(e) =>
								setFormData((d) => ({
									...d,
									slack_channel_ids: e.target.value,
								}))
							}
							placeholder="Leave empty for all channels"
							className="w-full rounded-lg border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
						/>
					</Field>
					<Field label="DM Allowed Users (optional, comma-separated)">
						<input
							value={formData.slack_dm_allowed_users}
							onChange={(e) =>
								setFormData((d) => ({
									...d,
									slack_dm_allowed_users: e.target.value,
								}))
							}
							placeholder="User IDs that can DM the bot"
							className="w-full rounded-lg border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
						/>
					</Field>
				</>
			)}

			{formData.platform === "webhook" && (
				<p className="text-sm text-ink-dull">
					No additional configuration needed. The webhook adapter accepts
					messages on the configured HTTP port.
				</p>
			)}

			{/* Existing bindings for this platform */}
			{platformBindings.length > 0 && (
				<div className="border-t border-app-line/50 pt-3">
					<h3 className="mb-2 text-xs font-medium text-ink-dull">
						Existing {formData.platform} bindings
					</h3>
					<div className="flex flex-col gap-1.5">
						{platformBindings.map((binding, i) => (
							<div
								key={i}
								className="flex items-center justify-between rounded-lg bg-app-darkBox px-3 py-2"
							>
								<div className="min-w-0 flex-1 text-sm text-ink-dull">
									{binding.guild_id && (
										<span>
											Guild{" "}
											<code className="text-ink">
												{binding.guild_id}
											</code>
										</span>
									)}
									{binding.workspace_id && (
										<span>
											Workspace{" "}
											<code className="text-ink">
												{binding.workspace_id}
											</code>
										</span>
									)}
									{!binding.guild_id &&
										!binding.workspace_id && (
											<span>All messages</span>
										)}
									{binding.channel_ids.length > 0 && (
										<span className="ml-2 text-tiny text-ink-faint">
											({binding.channel_ids.length}{" "}
											channel
											{binding.channel_ids.length !== 1
												? "s"
												: ""}
											)
										</span>
									)}
								</div>
								<button
									onClick={() => onDeleteBinding(binding)}
									className="rounded-md px-2 py-1 text-tiny text-ink-faint transition-colors hover:bg-app-lightBox hover:text-red-400"
									title="Remove binding"
								>
									Remove
								</button>
							</div>
						))}
					</div>
				</div>
			)}

			{/* Actions */}
			<div className="mt-2 flex justify-between">
				<button
					onClick={onBack}
					className="rounded-lg px-3 py-1.5 text-sm text-ink-dull transition-colors hover:text-ink"
				>
					Back
				</button>
				<div className="flex gap-2">
					<button
						onClick={onSave}
						disabled={isPending}
						className="rounded-lg bg-accent px-4 py-1.5 text-sm font-medium text-white transition-colors hover:bg-accent/80 disabled:opacity-50"
					>
						{isPending ? "Saving..." : "Add Binding"}
					</button>
				</div>
			</div>
		</div>
	);
}

function Field({
	label,
	children,
}: {
	label: string;
	children: React.ReactNode;
}) {
	return (
		<div className="space-y-1.5">
			<label className="text-xs font-medium text-ink-dull">{label}</label>
			{children}
		</div>
	);
}
