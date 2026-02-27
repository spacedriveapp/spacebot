import {useState} from "react";
import {AnimatePresence, motion} from "framer-motion";
import {useMutation, useQuery, useQueryClient} from "@tanstack/react-query";
import {
	api,
	type AdapterInstanceStatus,
	type BindingInfo,
	type CreateMessagingInstanceRequest,
} from "@/api/client";
import {
	Button,
	Input,
	Select,
	SelectTrigger,
	SelectValue,
	SelectContent,
	SelectItem,
	Dialog,
	DialogContent,
	DialogHeader,
	DialogTitle,
	DialogFooter,
	Toggle,
} from "@/ui";
import {PlatformIcon} from "@/lib/platformIcons";
import {TagInput} from "@/components/TagInput";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {faChevronDown, faPlus} from "@fortawesome/free-solid-svg-icons";

type Platform = "discord" | "slack" | "telegram" | "twitch" | "email" | "webhook";

const PLATFORM_LABELS: Record<Platform, string> = {
	discord: "Discord",
	slack: "Slack",
	telegram: "Telegram",
	twitch: "Twitch",
	email: "Email",
	webhook: "Webhook",
};

const DOC_LINKS: Partial<Record<Platform, string>> = {
	discord: "https://docs.spacebot.sh/discord-setup",
	slack: "https://docs.spacebot.sh/slack-setup",
	telegram: "https://docs.spacebot.sh/telegram-setup",
	twitch: "https://docs.spacebot.sh/twitch-setup",
};

// --- Platform Catalog (Left Column) ---

interface PlatformCatalogProps {
	onAddInstance: (platform: Platform) => void;
}

export function PlatformCatalog({onAddInstance}: PlatformCatalogProps) {
	const PLATFORMS: {platform: Platform; description: string}[] = [
		{platform: "discord", description: "Discord bot integration"},
		{platform: "slack", description: "Slack bot integration"},
		{platform: "telegram", description: "Telegram bot integration"},
		{platform: "twitch", description: "Twitch chat integration"},
		{platform: "email", description: "IMAP/SMTP email integration"},
		{platform: "webhook", description: "HTTP webhook receiver"},
	];

	const COMING_SOON = [
		{platform: "whatsapp", name: "WhatsApp"},
		{platform: "matrix", name: "Matrix"},
		{platform: "imessage", name: "iMessage"},
		{platform: "irc", name: "IRC"},
		{platform: "lark", name: "Lark"},
		{platform: "dingtalk", name: "DingTalk"},
	];

	return (
		<div className="flex flex-col gap-1">
			<h3 className="text-xs font-semibold text-ink-faint uppercase tracking-wider mb-2">
				Available
			</h3>
			{PLATFORMS.map(({platform, description}) => (
				<button
					key={platform}
					type="button"
					onClick={() => onAddInstance(platform)}
					className="flex items-center gap-2.5 rounded-md px-3 py-2 text-left hover:bg-app-hover transition-colors group"
				>
					<PlatformIcon platform={platform} size="sm" className="text-ink-faint" />
					<span className="flex-1 text-sm text-ink">{PLATFORM_LABELS[platform]}</span>
					<FontAwesomeIcon
						icon={faPlus}
						className="text-ink-faint opacity-0 group-hover:opacity-100 transition-opacity text-xs"
					/>
				</button>
			))}

			<h3 className="text-xs font-semibold text-ink-faint uppercase tracking-wider mt-4 mb-2">
				Coming Soon
			</h3>
			{COMING_SOON.map(({platform, name}) => (
				<div
					key={platform}
					className="flex items-center gap-2.5 rounded-md px-3 py-2 opacity-40"
				>
					<PlatformIcon platform={platform} size="sm" className="text-ink-faint/50" />
					<span className="flex-1 text-sm text-ink-dull">{name}</span>
				</div>
			))}
		</div>
	);
}

// --- Instance Card (Right Column) ---

interface InstanceCardProps {
	instance: AdapterInstanceStatus;
	expanded: boolean;
	onToggleExpand: () => void;
}

export function InstanceCard({instance, expanded, onToggleExpand}: InstanceCardProps) {
	const queryClient = useQueryClient();
	const [message, setMessage] = useState<{text: string; type: "success" | "error"} | null>(null);
	const [confirmRemove, setConfirmRemove] = useState(false);
	const [editingBinding, setEditingBinding] = useState<BindingInfo | null>(null);
	const [addingBinding, setAddingBinding] = useState(false);
	const [bindingForm, setBindingForm] = useState({
		agent_id: "main",
		guild_id: "",
		workspace_id: "",
		chat_id: "",
		channel_ids: [] as string[],
		require_mention: false,
		dm_allowed_users: [] as string[],
	});

	const platform = instance.platform as Platform;
	const instanceLabel = instance.name
		? `${PLATFORM_LABELS[platform]} "${instance.name}"`
		: PLATFORM_LABELS[platform];

	const {data: bindingsData} = useQuery({
		queryKey: ["bindings"],
		queryFn: () => api.bindings(),
		staleTime: 5_000,
		enabled: expanded,
	});

	const {data: agentsData} = useQuery({
		queryKey: ["agents"],
		queryFn: api.agents,
		staleTime: 10_000,
		enabled: expanded,
	});

	// Filter bindings for this specific instance
	const instanceBindings = (bindingsData?.bindings ?? []).filter((binding) => {
		if (binding.channel !== platform) return false;
		if (instance.name === null) return !binding.adapter;
		return binding.adapter === instance.name;
	});

	const toggleEnabled = useMutation({
		mutationFn: (newEnabled: boolean) =>
			api.togglePlatform(platform, newEnabled, instance.name ?? undefined),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["messaging-status"]});
		},
		onError: (error) => setMessage({text: `Failed: ${error.message}`, type: "error"}),
	});

	const deleteInstance = useMutation({
		mutationFn: () =>
			api.deleteMessagingInstance({platform, name: instance.name ?? undefined}),
		onSuccess: (result) => {
			if (result.success) {
				setConfirmRemove(false);
				queryClient.invalidateQueries({queryKey: ["messaging-status"]});
				queryClient.invalidateQueries({queryKey: ["bindings"]});
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) => setMessage({text: `Failed: ${error.message}`, type: "error"}),
	});

	const addBindingMutation = useMutation({
		mutationFn: api.createBinding,
		onSuccess: (result) => {
			if (result.success) {
				setAddingBinding(false);
				resetBindingForm();
				setMessage({text: result.message, type: "success"});
				queryClient.invalidateQueries({queryKey: ["bindings"]});
				queryClient.invalidateQueries({queryKey: ["messaging-status"]});
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) => setMessage({text: `Failed: ${error.message}`, type: "error"}),
	});

	const updateBindingMutation = useMutation({
		mutationFn: api.updateBinding,
		onSuccess: (result) => {
			if (result.success) {
				setEditingBinding(null);
				resetBindingForm();
				setMessage({text: result.message, type: "success"});
				queryClient.invalidateQueries({queryKey: ["bindings"]});
				queryClient.invalidateQueries({queryKey: ["messaging-status"]});
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) => setMessage({text: `Failed: ${error.message}`, type: "error"}),
	});

	const deleteBindingMutation = useMutation({
		mutationFn: api.deleteBinding,
		onSuccess: (result) => {
			if (result.success) {
				setMessage({text: result.message, type: "success"});
				queryClient.invalidateQueries({queryKey: ["bindings"]});
				queryClient.invalidateQueries({queryKey: ["messaging-status"]});
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) => setMessage({text: `Failed: ${error.message}`, type: "error"}),
	});

	function resetBindingForm() {
		setBindingForm({
			agent_id: agentsData?.agents?.[0]?.id ?? "main",
			guild_id: "",
			workspace_id: "",
			chat_id: "",
			channel_ids: [],
			require_mention: false,
			dm_allowed_users: [],
		});
	}

	function handleAddBinding() {
		const request: Record<string, unknown> = {
			agent_id: bindingForm.agent_id,
			channel: platform,
		};
		// Auto-populate adapter for named instances
		if (instance.name) {
			request.adapter = instance.name;
		}
		if (platform === "discord" && bindingForm.guild_id.trim())
			request.guild_id = bindingForm.guild_id.trim();
		if (platform === "slack" && bindingForm.workspace_id.trim())
			request.workspace_id = bindingForm.workspace_id.trim();
		if (platform === "telegram" && bindingForm.chat_id.trim())
			request.chat_id = bindingForm.chat_id.trim();
		if (bindingForm.channel_ids.length > 0)
			request.channel_ids = bindingForm.channel_ids;
		if (platform === "discord" && bindingForm.require_mention)
			request.require_mention = true;
		if (bindingForm.dm_allowed_users.length > 0)
			request.dm_allowed_users = bindingForm.dm_allowed_users;
		addBindingMutation.mutate(request as any);
	}

	function handleUpdateBinding() {
		if (!editingBinding) return;
		const request: Record<string, unknown> = {
			original_agent_id: editingBinding.agent_id,
			original_channel: editingBinding.channel,
			original_adapter: editingBinding.adapter || undefined,
			original_guild_id: editingBinding.guild_id || undefined,
			original_workspace_id: editingBinding.workspace_id || undefined,
			original_chat_id: editingBinding.chat_id || undefined,
			agent_id: bindingForm.agent_id,
			channel: platform,
		};
		if (instance.name) {
			request.adapter = instance.name;
		}
		if (platform === "discord" && bindingForm.guild_id.trim())
			request.guild_id = bindingForm.guild_id.trim();
		if (platform === "slack" && bindingForm.workspace_id.trim())
			request.workspace_id = bindingForm.workspace_id.trim();
		if (platform === "telegram" && bindingForm.chat_id.trim())
			request.chat_id = bindingForm.chat_id.trim();
		request.channel_ids = bindingForm.channel_ids;
		request.require_mention = platform === "discord" ? bindingForm.require_mention : false;
		request.dm_allowed_users = bindingForm.dm_allowed_users;
		updateBindingMutation.mutate(request as any);
	}

	function handleDeleteBinding(binding: BindingInfo) {
		const request: Record<string, unknown> = {
			agent_id: binding.agent_id,
			channel: binding.channel,
		};
		if (binding.adapter) request.adapter = binding.adapter;
		if (binding.guild_id) request.guild_id = binding.guild_id;
		if (binding.workspace_id) request.workspace_id = binding.workspace_id;
		if (binding.chat_id) request.chat_id = binding.chat_id;
		deleteBindingMutation.mutate(request as any);
	}

	function startEditBinding(binding: BindingInfo) {
		setEditingBinding(binding);
		setAddingBinding(false);
		setBindingForm({
			agent_id: binding.agent_id,
			guild_id: binding.guild_id || "",
			workspace_id: binding.workspace_id || "",
			chat_id: binding.chat_id || "",
			channel_ids: binding.channel_ids,
			require_mention: binding.require_mention,
			dm_allowed_users: binding.dm_allowed_users,
		});
	}

	const isEditingOrAdding = editingBinding !== null || addingBinding;

	return (
		<div className="rounded-lg border border-app-line bg-app-box">
			{/* Collapsed summary header */}
			<button
				type="button"
				onClick={onToggleExpand}
				aria-expanded={expanded}
				className="flex w-full items-center gap-3 p-3 text-left cursor-pointer"
			>
				<PlatformIcon platform={platform} size="sm" className="text-ink-faint" />
				<div className="flex-1 min-w-0">
					<div className="flex items-center gap-2">
						<span className="text-sm font-medium text-ink">
							{PLATFORM_LABELS[platform]}
						</span>
						<span className="rounded bg-app-selected/60 px-1.5 py-0.5 text-[10px] font-medium uppercase leading-none text-ink-faint">
							{instance.name || "default"}
						</span>
						<span className={`text-tiny ${instance.enabled ? "text-green-400" : "text-ink-faint"}`}>
							{instance.enabled ? "● Active" : "○ Disabled"}
						</span>
					</div>
					<p className="text-tiny text-ink-faint mt-0.5">
						{instance.binding_count} binding{instance.binding_count !== 1 ? "s" : ""}
					</p>
				</div>
				<motion.div
					animate={{rotate: expanded ? 180 : 0}}
					transition={{duration: 0.2}}
					className="text-ink-faint"
				>
					<FontAwesomeIcon icon={faChevronDown} size="sm" />
				</motion.div>
			</button>

			{/* Expanded content */}
			<AnimatePresence initial={false}>
				{expanded && (
					<motion.div
						initial={{height: 0, opacity: 0}}
						animate={{height: "auto", opacity: 1}}
						exit={{height: 0, opacity: 0}}
						transition={{duration: 0.25, ease: [0.4, 0, 0.2, 1]}}
						className="overflow-hidden"
					>
						<div className="border-t border-app-line/50 bg-app-darkBox px-4 pb-4 pt-3 flex flex-col gap-4">
							{/* Enable/Disable toggle */}
							<div className="flex items-center justify-between">
								<div>
									<span className="text-sm font-medium text-ink">Enabled</span>
									<p className="mt-0.5 text-sm text-ink-dull">
										{instance.enabled ? "Receiving messages" : "Adapter paused"}
									</p>
								</div>
								<Toggle
									checked={instance.enabled}
									onCheckedChange={(checked) => toggleEnabled.mutate(checked)}
									disabled={toggleEnabled.isPending}
								/>
							</div>

							{/* Bindings section (scoped to this instance) */}
							<div className="flex flex-col gap-3 border-t border-app-line/50 pt-3">
								<div className="flex items-center justify-between">
									<h3 className="text-sm font-medium text-ink">Bindings</h3>
									<Button
										size="sm"
										variant="outline"
										onClick={() => {
											setAddingBinding(true);
											setEditingBinding(null);
											resetBindingForm();
											setMessage(null);
										}}
									>
										Add
									</Button>
								</div>

								{instanceBindings.length > 0 ? (
									<div className="rounded-md border border-app-line bg-app-box">
										{instanceBindings.map((binding, idx) => (
											<div
												key={idx}
												className="flex items-center gap-2 border-b border-app-line/50 px-3 py-2 last:border-b-0"
											>
												<div className="flex-1 min-w-0">
													<span className="text-sm text-ink">{binding.agent_id}</span>
													<div className="flex flex-wrap gap-1.5 mt-0.5 text-tiny text-ink-faint">
														{binding.guild_id && <span>Guild: {binding.guild_id}</span>}
														{binding.workspace_id && <span>Workspace: {binding.workspace_id}</span>}
														{binding.chat_id && <span>Chat: {binding.chat_id}</span>}
														{binding.channel_ids.length > 0 && (
															<span>
																{binding.channel_ids.length} channel{binding.channel_ids.length > 1 ? "s" : ""}
															</span>
														)}
														{binding.dm_allowed_users.length > 0 && (
															<span>
																{binding.dm_allowed_users.length} DM user{binding.dm_allowed_users.length > 1 ? "s" : ""}
															</span>
														)}
														{binding.require_mention && <span>Mention only</span>}
														{!binding.guild_id &&
															!binding.workspace_id &&
															!binding.chat_id &&
															binding.channel_ids.length === 0 && (
																<span>All conversations</span>
															)}
													</div>
												</div>
												<Button size="sm" variant="outline" onClick={() => startEditBinding(binding)}>
													Edit
												</Button>
												<Button
													size="sm"
													variant="outline"
													onClick={() => handleDeleteBinding(binding)}
													loading={deleteBindingMutation.isPending}
												>
													Remove
												</Button>
											</div>
										))}
									</div>
								) : (
									<p className="text-sm text-ink-faint py-1">
										No bindings. Messages will route to the default agent.
									</p>
								)}

								{/* Add/Edit binding modal */}
								<Dialog
									open={isEditingOrAdding}
									onOpenChange={(open) => {
										if (!open) {
											setEditingBinding(null);
											setAddingBinding(false);
											setMessage(null);
										}
									}}
								>
									<DialogContent className="max-w-md">
										<DialogHeader>
											<DialogTitle>
												{editingBinding ? "Edit Binding" : "Add Binding"}
											</DialogTitle>
										</DialogHeader>
										<BindingForm
											platform={platform}
											agents={agentsData?.agents ?? []}
											bindingForm={bindingForm}
											setBindingForm={setBindingForm}
											editing={!!editingBinding}
											onSave={editingBinding ? handleUpdateBinding : handleAddBinding}
											onCancel={() => {
												setEditingBinding(null);
												setAddingBinding(false);
												setMessage(null);
											}}
											saving={editingBinding ? updateBindingMutation.isPending : addBindingMutation.isPending}
										/>
									</DialogContent>
								</Dialog>
							</div>

							{/* Status message */}
							{message && (
								<div
									className={`rounded-md border px-3 py-2 text-sm ${
										message.type === "success"
											? "border-green-500/20 bg-green-500/10 text-green-400"
											: "border-red-500/20 bg-red-500/10 text-red-400"
									}`}
								>
									{message.text}
								</div>
							)}

							{/* Remove instance */}
							<div className="border-t border-app-line/50 pt-3">
									{!confirmRemove ? (
										<Button
											variant="outline"
											size="sm"
											onClick={() => setConfirmRemove(true)}
										>
											Remove {instanceLabel}
										</Button>
									) : (
										<div className="flex flex-col gap-2">
											<p className="text-sm text-red-400">
												This will remove credentials and bindings for {instanceLabel}.
												The adapter will stop immediately.
											</p>
											<div className="flex gap-2">
												<Button variant="ghost" size="sm" onClick={() => setConfirmRemove(false)}>
													Cancel
												</Button>
												<Button
													size="sm"
													onClick={() => deleteInstance.mutate()}
													loading={deleteInstance.isPending}
													className="bg-red-500/20 text-red-400 hover:bg-red-500/30"
												>
													Confirm Remove
												</Button>
											</div>
										</div>
									)}
								</div>
						</div>
					</motion.div>
				)}
			</AnimatePresence>
		</div>
	);
}

// --- Add Instance Inline Card ---

interface AddInstanceCardProps {
	platform: Platform;
	isDefault: boolean;
	onCancel: () => void;
	onCreated: () => void;
}

export function AddInstanceCard({platform, isDefault, onCancel, onCreated}: AddInstanceCardProps) {
	const queryClient = useQueryClient();
	const [instanceName, setInstanceName] = useState("");
	const [credentialInputs, setCredentialInputs] = useState<Record<string, string>>({});
	const [message, setMessage] = useState<{text: string; type: "success" | "error"} | null>(null);

	const createInstance = useMutation({
		mutationFn: api.createMessagingInstance,
		onSuccess: (result) => {
			if (result.success) {
				queryClient.invalidateQueries({queryKey: ["messaging-status"]});
				queryClient.invalidateQueries({queryKey: ["bindings"]});
				onCreated();
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) => setMessage({text: `Failed: ${error.message}`, type: "error"}),
	});

	function handleSave() {
		const credentials: CreateMessagingInstanceRequest["credentials"] = {};

		if (platform === "discord") {
			if (!credentialInputs.discord_token?.trim()) {
				setMessage({text: "Bot token is required", type: "error"});
				return;
			}
			credentials.discord_token = credentialInputs.discord_token.trim();
		} else if (platform === "slack") {
			if (!credentialInputs.slack_bot_token?.trim() || !credentialInputs.slack_app_token?.trim()) {
				setMessage({text: "Both bot token and app token are required", type: "error"});
				return;
			}
			credentials.slack_bot_token = credentialInputs.slack_bot_token.trim();
			credentials.slack_app_token = credentialInputs.slack_app_token.trim();
		} else if (platform === "telegram") {
			if (!credentialInputs.telegram_token?.trim()) {
				setMessage({text: "Bot token is required", type: "error"});
				return;
			}
			credentials.telegram_token = credentialInputs.telegram_token.trim();
		} else if (platform === "twitch") {
			if (!credentialInputs.twitch_username?.trim() || !credentialInputs.twitch_oauth_token?.trim()) {
				setMessage({text: "Username and OAuth token are required", type: "error"});
				return;
			}
			credentials.twitch_username = credentialInputs.twitch_username.trim();
			credentials.twitch_oauth_token = credentialInputs.twitch_oauth_token.trim();
			if (credentialInputs.twitch_client_id?.trim())
				credentials.twitch_client_id = credentialInputs.twitch_client_id.trim();
			if (credentialInputs.twitch_client_secret?.trim())
				credentials.twitch_client_secret = credentialInputs.twitch_client_secret.trim();
			if (credentialInputs.twitch_refresh_token?.trim())
				credentials.twitch_refresh_token = credentialInputs.twitch_refresh_token.trim();
		} else if (platform === "email") {
			if (!credentialInputs.email_imap_host?.trim() || !credentialInputs.email_smtp_host?.trim()) {
				setMessage({text: "IMAP host and SMTP host are required", type: "error"});
				return;
			}
			if (!credentialInputs.email_imap_username?.trim() || !credentialInputs.email_imap_password?.trim()) {
				setMessage({text: "IMAP username and password are required", type: "error"});
				return;
			}
			if (!credentialInputs.email_smtp_username?.trim() || !credentialInputs.email_smtp_password?.trim()) {
				setMessage({text: "SMTP username and password are required", type: "error"});
				return;
			}
			if (!credentialInputs.email_from_address?.trim()) {
				setMessage({text: "From address is required", type: "error"});
				return;
			}
			credentials.email_imap_host = credentialInputs.email_imap_host.trim();
			credentials.email_imap_username = credentialInputs.email_imap_username.trim();
			credentials.email_imap_password = credentialInputs.email_imap_password.trim();
			credentials.email_smtp_host = credentialInputs.email_smtp_host.trim();
			credentials.email_smtp_username = credentialInputs.email_smtp_username.trim();
			credentials.email_smtp_password = credentialInputs.email_smtp_password.trim();
			credentials.email_from_address = credentialInputs.email_from_address.trim();
			if (credentialInputs.email_imap_port?.trim())
				credentials.email_imap_port = parseInt(credentialInputs.email_imap_port.trim(), 10) || undefined;
			if (credentialInputs.email_smtp_port?.trim())
				credentials.email_smtp_port = parseInt(credentialInputs.email_smtp_port.trim(), 10) || undefined;
		} else if (platform === "webhook") {
			if (credentialInputs.webhook_port?.trim())
				credentials.webhook_port = parseInt(credentialInputs.webhook_port.trim(), 10) || undefined;
			if (credentialInputs.webhook_bind?.trim())
				credentials.webhook_bind = credentialInputs.webhook_bind.trim();
			if (credentialInputs.webhook_auth_token?.trim())
				credentials.webhook_auth_token = credentialInputs.webhook_auth_token.trim();
		}

		if (!isDefault && !instanceName.trim()) {
			setMessage({text: "Instance name is required", type: "error"});
			return;
		}

		createInstance.mutate({
			platform,
			name: isDefault ? undefined : instanceName.trim(),
			credentials,
		});
	}

	const docLink = DOC_LINKS[platform];

	return (
		<div className="rounded-lg border border-accent/30 bg-app-box">
			<div className="p-4 flex flex-col gap-3">
				<div className="flex items-center gap-2">
					<PlatformIcon platform={platform} size="sm" className="text-ink-faint" />
					<span className="text-sm font-medium text-ink">
						{isDefault
							? `Add ${PLATFORM_LABELS[platform]}`
							: `Add ${PLATFORM_LABELS[platform]} Instance`}
					</span>
				</div>

				{/* Instance name (only for non-default) */}
				{!isDefault && (
					<div>
						<label className="mb-1 block text-sm font-medium text-ink-dull">
							Instance Name
						</label>
						<Input
							size="lg"
							value={instanceName}
							onChange={(e) => setInstanceName(e.target.value)}
							placeholder='e.g. "support", "sales"'
						/>
					</div>
				)}

				{/* Platform-specific credential fields */}
				{platform === "discord" && (
					<div>
						<label className="mb-1.5 block text-sm font-medium text-ink-dull">Bot Token</label>
						<Input
							type="password"
							size="lg"
							value={credentialInputs.discord_token ?? ""}
							onChange={(e) => setCredentialInputs({...credentialInputs, discord_token: e.target.value})}
							placeholder="MTk4NjIyNDgzNDcxOTI1MjQ4.D..."
							onKeyDown={(e) => { if (e.key === "Enter") handleSave(); }}
						/>
					</div>
				)}

				{platform === "slack" && (
					<>
						<div>
							<label className="mb-1.5 block text-sm font-medium text-ink-dull">Bot Token</label>
							<Input
								type="password"
								size="lg"
								value={credentialInputs.slack_bot_token ?? ""}
								onChange={(e) => setCredentialInputs({...credentialInputs, slack_bot_token: e.target.value})}
								placeholder="xoxb-..."
							/>
						</div>
						<div>
							<label className="mb-1.5 block text-sm font-medium text-ink-dull">App Token</label>
							<Input
								type="password"
								size="lg"
								value={credentialInputs.slack_app_token ?? ""}
								onChange={(e) => setCredentialInputs({...credentialInputs, slack_app_token: e.target.value})}
								placeholder="xapp-..."
								onKeyDown={(e) => { if (e.key === "Enter") handleSave(); }}
							/>
						</div>
					</>
				)}

				{platform === "telegram" && (
					<div>
						<label className="mb-1.5 block text-sm font-medium text-ink-dull">Bot Token</label>
						<Input
							type="password"
							size="lg"
							value={credentialInputs.telegram_token ?? ""}
							onChange={(e) => setCredentialInputs({...credentialInputs, telegram_token: e.target.value})}
							placeholder="123456789:ABCdefGHI..."
							onKeyDown={(e) => { if (e.key === "Enter") handleSave(); }}
						/>
					</div>
				)}

				{platform === "twitch" && (
					<>
						<div>
							<label className="mb-1.5 block text-sm font-medium text-ink-dull">Bot Username</label>
							<Input
								size="lg"
								value={credentialInputs.twitch_username ?? ""}
								onChange={(e) => setCredentialInputs({...credentialInputs, twitch_username: e.target.value})}
								placeholder="my_bot"
							/>
						</div>
						<div className="grid grid-cols-2 gap-3">
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink-dull">Client ID</label>
								<Input
									size="lg"
									value={credentialInputs.twitch_client_id ?? ""}
									onChange={(e) => setCredentialInputs({...credentialInputs, twitch_client_id: e.target.value})}
									placeholder="your-client-id"
								/>
							</div>
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink-dull">Client Secret</label>
								<Input
									type="password"
									size="lg"
									value={credentialInputs.twitch_client_secret ?? ""}
									onChange={(e) => setCredentialInputs({...credentialInputs, twitch_client_secret: e.target.value})}
									placeholder="your-client-secret"
								/>
							</div>
						</div>
						<div className="grid grid-cols-2 gap-3">
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink-dull">OAuth Token</label>
								<Input
									type="password"
									size="lg"
									value={credentialInputs.twitch_oauth_token ?? ""}
									onChange={(e) => setCredentialInputs({...credentialInputs, twitch_oauth_token: e.target.value})}
									placeholder="abcd1234..."
									onKeyDown={(e) => { if (e.key === "Enter") handleSave(); }}
								/>
							</div>
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink-dull">Refresh Token</label>
								<Input
									type="password"
									size="lg"
									value={credentialInputs.twitch_refresh_token ?? ""}
									onChange={(e) => setCredentialInputs({...credentialInputs, twitch_refresh_token: e.target.value})}
									placeholder="refresh-token"
								/>
							</div>
						</div>
					</>
				)}

				{platform === "email" && (
					<>
						<div className="grid grid-cols-2 gap-3">
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink-dull">IMAP Host</label>
								<Input
									size="lg"
									value={credentialInputs.email_imap_host ?? ""}
									onChange={(e) => setCredentialInputs({...credentialInputs, email_imap_host: e.target.value})}
									placeholder="imap.gmail.com"
								/>
							</div>
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink-dull">IMAP Port</label>
								<Input
									size="lg"
									value={credentialInputs.email_imap_port ?? ""}
									onChange={(e) => setCredentialInputs({...credentialInputs, email_imap_port: e.target.value})}
									placeholder="993"
								/>
							</div>
						</div>
						<div className="grid grid-cols-2 gap-3">
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink-dull">IMAP Username</label>
								<Input
									size="lg"
									value={credentialInputs.email_imap_username ?? ""}
									onChange={(e) => setCredentialInputs({...credentialInputs, email_imap_username: e.target.value})}
									placeholder="user@example.com"
								/>
							</div>
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink-dull">IMAP Password</label>
								<Input
									type="password"
									size="lg"
									value={credentialInputs.email_imap_password ?? ""}
									onChange={(e) => setCredentialInputs({...credentialInputs, email_imap_password: e.target.value})}
									placeholder="App password"
								/>
							</div>
						</div>
						<div className="grid grid-cols-2 gap-3">
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink-dull">SMTP Host</label>
								<Input
									size="lg"
									value={credentialInputs.email_smtp_host ?? ""}
									onChange={(e) => setCredentialInputs({...credentialInputs, email_smtp_host: e.target.value})}
									placeholder="smtp.gmail.com"
								/>
							</div>
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink-dull">SMTP Port</label>
								<Input
									size="lg"
									value={credentialInputs.email_smtp_port ?? ""}
									onChange={(e) => setCredentialInputs({...credentialInputs, email_smtp_port: e.target.value})}
									placeholder="587"
								/>
							</div>
						</div>
						<div className="grid grid-cols-2 gap-3">
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink-dull">SMTP Username</label>
								<Input
									size="lg"
									value={credentialInputs.email_smtp_username ?? ""}
									onChange={(e) => setCredentialInputs({...credentialInputs, email_smtp_username: e.target.value})}
									placeholder="user@example.com"
								/>
							</div>
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink-dull">SMTP Password</label>
								<Input
									type="password"
									size="lg"
									value={credentialInputs.email_smtp_password ?? ""}
									onChange={(e) => setCredentialInputs({...credentialInputs, email_smtp_password: e.target.value})}
									placeholder="App password"
								/>
							</div>
						</div>
						<div>
							<label className="mb-1.5 block text-sm font-medium text-ink-dull">From Address</label>
							<Input
								size="lg"
								value={credentialInputs.email_from_address ?? ""}
								onChange={(e) => setCredentialInputs({...credentialInputs, email_from_address: e.target.value})}
								placeholder="bot@example.com"
								onKeyDown={(e) => { if (e.key === "Enter") handleSave(); }}
							/>
						</div>
					</>
				)}

				{platform === "webhook" && (
					<>
						<div className="grid grid-cols-2 gap-3">
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink-dull">Port</label>
								<Input
									size="lg"
									value={credentialInputs.webhook_port ?? ""}
									onChange={(e) => setCredentialInputs({...credentialInputs, webhook_port: e.target.value})}
									placeholder="18789"
								/>
							</div>
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink-dull">Bind Address</label>
								<Input
									size="lg"
									value={credentialInputs.webhook_bind ?? ""}
									onChange={(e) => setCredentialInputs({...credentialInputs, webhook_bind: e.target.value})}
									placeholder="127.0.0.1"
								/>
							</div>
						</div>
						<div>
							<label className="mb-1.5 block text-sm font-medium text-ink-dull">Auth Token</label>
							<Input
								type="password"
								size="lg"
								value={credentialInputs.webhook_auth_token ?? ""}
								onChange={(e) => setCredentialInputs({...credentialInputs, webhook_auth_token: e.target.value})}
								placeholder="Optional — leave empty for no auth"
								onKeyDown={(e) => { if (e.key === "Enter") handleSave(); }}
							/>
						</div>
					</>
				)}

				{docLink && (
					<p className="text-xs text-ink-faint">
						Need help?{" "}
						<a href={docLink} target="_blank" rel="noopener noreferrer" className="text-accent hover:underline">
							Read the {PLATFORM_LABELS[platform]} setup docs &rarr;
						</a>
					</p>
				)}

				{message && (
					<div
						className={`rounded-md border px-3 py-2 text-sm ${
							message.type === "success"
								? "border-green-500/20 bg-green-500/10 text-green-400"
								: "border-red-500/20 bg-red-500/10 text-red-400"
						}`}
					>
						{message.text}
					</div>
				)}

				<div className="flex gap-2 justify-end">
					<Button size="sm" variant="ghost" onClick={onCancel}>
						Cancel
					</Button>
					<Button size="sm" onClick={handleSave} loading={createInstance.isPending}>
						{isDefault ? "Connect" : "Create Instance"}
					</Button>
				</div>
			</div>
		</div>
	);
}

// --- Binding Form (shared between add/edit) ---

function BindingForm({
	platform,
	agents,
	bindingForm,
	setBindingForm,
	editing,
	onSave,
	onCancel,
	saving,
}: {
	platform: Platform;
	agents: {id: string}[];
	bindingForm: {
		agent_id: string;
		guild_id: string;
		workspace_id: string;
		chat_id: string;
		channel_ids: string[];
		require_mention: boolean;
		dm_allowed_users: string[];
	};
	setBindingForm: (form: any) => void;
	editing: boolean;
	onSave: () => void;
	onCancel: () => void;
	saving: boolean;
}) {
	return (
		<div className="flex flex-col gap-3">
			<div>
				<label className="mb-1 block text-sm font-medium text-ink-dull">Agent</label>
				<Select
					value={bindingForm.agent_id}
					onValueChange={(v) => setBindingForm({...bindingForm, agent_id: v})}
				>
					<SelectTrigger><SelectValue /></SelectTrigger>
					<SelectContent>
						{agents.map((a) => (
							<SelectItem key={a.id} value={a.id}>{a.id}</SelectItem>
						)) ?? <SelectItem value="main">main</SelectItem>}
					</SelectContent>
				</Select>
			</div>

			{platform === "discord" && (
				<div>
					<label className="mb-1 block text-sm font-medium text-ink-dull">Guild ID</label>
					<Input
						size="lg"
						value={bindingForm.guild_id}
						onChange={(e) => setBindingForm({...bindingForm, guild_id: e.target.value})}
						placeholder="Optional -- leave empty for all servers"
					/>
				</div>
			)}

			{platform === "slack" && (
				<div>
					<label className="mb-1 block text-sm font-medium text-ink-dull">Workspace ID</label>
					<Input
						size="lg"
						value={bindingForm.workspace_id}
						onChange={(e) => setBindingForm({...bindingForm, workspace_id: e.target.value})}
						placeholder="Optional -- leave empty for all workspaces"
					/>
				</div>
			)}

			{platform === "telegram" && (
				<div>
					<label className="mb-1 block text-sm font-medium text-ink-dull">Chat ID</label>
					<Input
						size="lg"
						value={bindingForm.chat_id}
						onChange={(e) => setBindingForm({...bindingForm, chat_id: e.target.value})}
						placeholder="Optional -- leave empty for all chats"
					/>
				</div>
			)}

			{(platform === "discord" || platform === "slack") && (
				<div>
					<label className="mb-1 block text-sm font-medium text-ink-dull">Channel IDs</label>
					<TagInput
						value={bindingForm.channel_ids}
						onChange={(ids) => setBindingForm({...bindingForm, channel_ids: ids})}
						placeholder="Add channel ID..."
					/>
				</div>
			)}

			{platform === "discord" && (
				<div className="flex items-center gap-2">
					<input
						type="checkbox"
						checked={bindingForm.require_mention}
						onChange={(e) => setBindingForm({...bindingForm, require_mention: e.target.checked})}
						className="h-4 w-4 rounded border-app-line bg-app-box"
					/>
					<label className="text-sm text-ink-dull">Require @mention or reply to bot</label>
				</div>
			)}

			{platform === "twitch" && (
				<div>
					<label className="mb-1 block text-sm font-medium text-ink-dull">Channels</label>
					<TagInput
						value={bindingForm.channel_ids}
						onChange={(ids) => setBindingForm({...bindingForm, channel_ids: ids})}
						placeholder="Add channel name..."
					/>
				</div>
			)}

			<div>
				<label className="mb-1 block text-sm font-medium text-ink-dull">DM Allowed Users</label>
				<TagInput
					value={bindingForm.dm_allowed_users}
					onChange={(users) => setBindingForm({...bindingForm, dm_allowed_users: users})}
					placeholder="Add user ID..."
				/>
			</div>

			<DialogFooter>
				<Button size="sm" variant="ghost" onClick={onCancel}>Cancel</Button>
				<Button size="sm" onClick={onSave} loading={saving}>
					{editing ? "Update" : "Add Binding"}
				</Button>
			</DialogFooter>
		</div>
	);
}

// --- Backward compat: keep DisabledChannelCard export for any other consumers ---

export function DisabledChannelCard({
	platform,
	name,
	description,
}: {
	platform: string;
	name: string;
	description: string;
}) {
	return (
		<div className="rounded-lg border border-app-line bg-app-box p-4 opacity-40">
			<div className="flex items-center gap-3">
				<PlatformIcon platform={platform} size="lg" className="text-ink-faint/50" />
				<div className="flex-1">
					<span className="text-sm font-medium text-ink">{name}</span>
					<p className="mt-0.5 text-sm text-ink-dull">{description}</p>
				</div>
				<Button variant="outline" size="sm" disabled>
					Coming Soon
				</Button>
			</div>
		</div>
	);
}
