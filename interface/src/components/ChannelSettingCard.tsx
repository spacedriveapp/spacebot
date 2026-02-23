import {useState} from "react";
import {AnimatePresence, motion} from "framer-motion";
import {useMutation, useQuery, useQueryClient} from "@tanstack/react-query";
import {api, type PlatformStatus, type BindingInfo} from "@/api/client";
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
import {faChevronDown} from "@fortawesome/free-solid-svg-icons";

type Platform = "discord" | "slack" | "telegram" | "twitch" | "webhook";

interface ChannelSettingCardProps {
	platform: Platform;
	name: string;
	description: string;
	status?: PlatformStatus;
	expanded: boolean;
	onToggle: () => void;
}

export function ChannelSettingCard({
	platform,
	name,
	description,
	status,
	expanded,
	onToggle,
}: ChannelSettingCardProps) {
	const queryClient = useQueryClient();
	const configured = status?.configured ?? false;
	const enabled = status?.enabled ?? false;

	const [credentialInputs, setCredentialInputs] = useState<
		Record<string, string>
	>({});
	const [message, setMessage] = useState<{
		text: string;
		type: "success" | "error";
	} | null>(null);
	const [confirmDisconnect, setConfirmDisconnect] = useState(false);
	const [editingBinding, setEditingBinding] = useState<BindingInfo | null>(
		null,
	);
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

	const {data: bindingsData} = useQuery({
		queryKey: ["bindings"],
		queryFn: () => api.bindings(),
		staleTime: 5_000,
		enabled: expanded && configured,
	});

	const {data: agentsData} = useQuery({
		queryKey: ["agents"],
		queryFn: api.agents,
		staleTime: 10_000,
		enabled: expanded,
	});

	const platformBindings =
		bindingsData?.bindings?.filter((b) => b.channel === platform) ?? [];

	const toggleEnabled = useMutation({
		mutationFn: (newEnabled: boolean) =>
			api.togglePlatform(platform, newEnabled),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["messaging-status"]});
		},
		onError: (error) =>
			setMessage({text: `Failed: ${error.message}`, type: "error"}),
	});

	// --- Mutations ---

	const saveCreds = useMutation({
		mutationFn: api.createBinding,
		onSuccess: (result) => {
			if (result.success) {
				setCredentialInputs({});
				setMessage({text: result.message, type: "success"});
				queryClient.invalidateQueries({queryKey: ["messaging-status"]});
				queryClient.invalidateQueries({queryKey: ["bindings"]});
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) =>
			setMessage({text: `Failed: ${error.message}`, type: "error"}),
	});

	const addBindingMutation = useMutation({
		mutationFn: api.createBinding,
		onSuccess: (result) => {
			if (result.success) {
				setAddingBinding(false);
				resetBindingForm();
				setMessage({text: result.message, type: "success"});
				queryClient.invalidateQueries({queryKey: ["bindings"]});
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) =>
			setMessage({text: `Failed: ${error.message}`, type: "error"}),
	});

	const updateBindingMutation = useMutation({
		mutationFn: api.updateBinding,
		onSuccess: (result) => {
			if (result.success) {
				setEditingBinding(null);
				resetBindingForm();
				setMessage({text: result.message, type: "success"});
				queryClient.invalidateQueries({queryKey: ["bindings"]});
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) =>
			setMessage({text: `Failed: ${error.message}`, type: "error"}),
	});

	const deleteBindingMutation = useMutation({
		mutationFn: api.deleteBinding,
		onSuccess: (result) => {
			if (result.success) {
				setMessage({text: result.message, type: "success"});
				queryClient.invalidateQueries({queryKey: ["bindings"]});
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) =>
			setMessage({text: `Failed: ${error.message}`, type: "error"}),
	});

	const disconnectMutation = useMutation({
		mutationFn: () => api.disconnectPlatform(platform),
		onSuccess: () => {
			setConfirmDisconnect(false);
			setMessage(null);
			queryClient.invalidateQueries({queryKey: ["messaging-status"]});
			queryClient.invalidateQueries({queryKey: ["bindings"]});
		},
		onError: (error) =>
			setMessage({text: `Failed: ${error.message}`, type: "error"}),
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

	function handleSaveCredentials() {
		const request: any = {agent_id: "main", channel: platform};
		if (platform === "discord") {
			if (!credentialInputs.discord_token?.trim()) return;
			request.platform_credentials = {
				discord_token: credentialInputs.discord_token.trim(),
			};
		} else if (platform === "slack") {
			if (
				!credentialInputs.slack_bot_token?.trim() ||
				!credentialInputs.slack_app_token?.trim()
			)
				return;
			request.platform_credentials = {
				slack_bot_token: credentialInputs.slack_bot_token.trim(),
				slack_app_token: credentialInputs.slack_app_token.trim(),
			};
		} else if (platform === "telegram") {
			if (!credentialInputs.telegram_token?.trim()) return;
			request.platform_credentials = {
				telegram_token: credentialInputs.telegram_token.trim(),
			};
		} else if (platform === "twitch") {
			if (!credentialInputs.twitch_username?.trim() || !credentialInputs.twitch_oauth_token?.trim()) return;
			request.platform_credentials = {
				twitch_username: credentialInputs.twitch_username.trim(),
				twitch_oauth_token: credentialInputs.twitch_oauth_token.trim(),
				twitch_client_id: credentialInputs.twitch_client_id?.trim(),
				twitch_client_secret: credentialInputs.twitch_client_secret?.trim(),
				twitch_refresh_token: credentialInputs.twitch_refresh_token?.trim(),
			};
		}
		saveCreds.mutate(request);
	}

	function handleAddBinding() {
		const request: any = {agent_id: bindingForm.agent_id, channel: platform};
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
		addBindingMutation.mutate(request);
	}

	function handleUpdateBinding() {
		if (!editingBinding) return;
		const request: any = {
			original_agent_id: editingBinding.agent_id,
			original_channel: editingBinding.channel,
			original_guild_id: editingBinding.guild_id || undefined,
			original_workspace_id: editingBinding.workspace_id || undefined,
			original_chat_id: editingBinding.chat_id || undefined,
			agent_id: bindingForm.agent_id,
			channel: platform,
		};
		if (platform === "discord" && bindingForm.guild_id.trim())
			request.guild_id = bindingForm.guild_id.trim();
		if (platform === "slack" && bindingForm.workspace_id.trim())
			request.workspace_id = bindingForm.workspace_id.trim();
		if (platform === "telegram" && bindingForm.chat_id.trim())
			request.chat_id = bindingForm.chat_id.trim();
		request.channel_ids = bindingForm.channel_ids;
		request.require_mention =
			platform === "discord" ? bindingForm.require_mention : false;
		request.dm_allowed_users = bindingForm.dm_allowed_users;
		updateBindingMutation.mutate(request);
	}

	function handleDeleteBinding(binding: BindingInfo) {
		const request: any = {agent_id: binding.agent_id, channel: binding.channel};
		if (binding.guild_id) request.guild_id = binding.guild_id;
		if (binding.workspace_id) request.workspace_id = binding.workspace_id;
		if (binding.chat_id) request.chat_id = binding.chat_id;
		deleteBindingMutation.mutate(request);
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
			{/* Header — always visible, acts as toggle */}
			<div
				role="button"
				onClick={onToggle}
				className="flex w-full items-center gap-3 p-4 text-left cursor-pointer"
			>
				<PlatformIcon
					platform={platform}
					size="lg"
					className="text-ink-faint"
				/>
				<div className="flex-1 min-w-0">
					<div className="flex items-center gap-2">
						<span className="text-sm font-medium text-ink">{name}</span>
						{configured && (
							<span
								className={`text-tiny ${enabled ? "text-green-400" : "text-ink-faint"}`}
							>
								{enabled ? "● Active" : "○ Disabled"}
							</span>
						)}
					</div>
					<p className="mt-0.5 text-sm text-ink-dull">{description}</p>
				</div>
				<motion.div
					animate={{rotate: expanded ? 180 : 0}}
					transition={{duration: 0.2}}
					className="text-ink-faint"
				>
					<FontAwesomeIcon icon={faChevronDown} size="sm" />
				</motion.div>
			</div>

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
						<div className="border-t border-app-line/50 bg-app-darkBox px-4 pb-4 pt-4 flex flex-col gap-4">
							{/* Enable/Disable toggle */}
							{configured && (
								<div className="flex items-center justify-between">
									<div>
										<span className="text-sm font-medium text-ink">
											Enabled
										</span>
										<p className="mt-0.5 text-sm text-ink-dull">
											{enabled
												? `${name} is receiving messages`
												: `${name} is disconnected`}
										</p>
									</div>
									<Toggle
										checked={enabled}
										onCheckedChange={(checked) => toggleEnabled.mutate(checked)}
										disabled={toggleEnabled.isPending}
									/>
								</div>
							)}
							{/* Credentials */}
							<CredentialsSection
								platform={platform}
								configured={configured}
								credentialInputs={credentialInputs}
								setCredentialInputs={setCredentialInputs}
								onSave={handleSaveCredentials}
								saving={saveCreds.isPending}
							/>

							{/* Bindings (only when connected) */}
							{configured && (
								<BindingsSection
									platform={platform}
									bindings={platformBindings}
									agents={agentsData?.agents ?? []}
									isEditingOrAdding={isEditingOrAdding}
									editingBinding={editingBinding}
									bindingForm={bindingForm}
									setBindingForm={setBindingForm}
									onStartAdd={() => {
										setAddingBinding(true);
										setEditingBinding(null);
										resetBindingForm();
										setMessage(null);
									}}
									onStartEdit={startEditBinding}
									onCancelEdit={() => {
										setEditingBinding(null);
										setAddingBinding(false);
										setMessage(null);
									}}
									onAdd={handleAddBinding}
									onUpdate={handleUpdateBinding}
									onDelete={handleDeleteBinding}
									addPending={addBindingMutation.isPending}
									updatePending={updateBindingMutation.isPending}
									deletePending={deleteBindingMutation.isPending}
								/>
							)}

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

							{/* Disconnect */}
							{configured && platform !== "webhook" && (
								<DisconnectSection
									name={name}
									confirmDisconnect={confirmDisconnect}
									setConfirmDisconnect={setConfirmDisconnect}
									onDisconnect={() => disconnectMutation.mutate()}
									disconnecting={disconnectMutation.isPending}
								/>
							)}
						</div>
					</motion.div>
				)}
			</AnimatePresence>
		</div>
	);
}

// --- Disabled card for coming soon platforms ---

interface DisabledChannelCardProps {
	platform: string;
	name: string;
	description: string;
}

export function DisabledChannelCard({
	platform,
	name,
	description,
}: DisabledChannelCardProps) {
	return (
		<div className="rounded-lg border border-app-line bg-app-box p-4 opacity-40">
			<div className="flex items-center gap-3">
				<PlatformIcon
					platform={platform}
					size="lg"
					className="text-ink-faint/50"
				/>
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

// --- Sub-sections ---

function CredentialsSection({
	platform,
	configured,
	credentialInputs,
	setCredentialInputs,
	onSave,
	saving,
}: {
	platform: Platform;
	configured: boolean;
	credentialInputs: Record<string, string>;
	setCredentialInputs: (inputs: Record<string, string>) => void;
	onSave: () => void;
	saving: boolean;
}) {
	return (
		<div className="flex flex-col gap-3">
			<h3 className="text-sm font-medium text-ink">
				{configured ? "Update Credentials" : "Credentials"}
			</h3>

			{platform === "discord" && (
				<div>
					<label className="mb-1.5 block text-sm font-medium text-ink-dull">
						Bot Token
					</label>
					<Input
						type="password"
						size="lg"
						value={credentialInputs.discord_token ?? ""}
						onChange={(e) =>
							setCredentialInputs({
								...credentialInputs,
								discord_token: e.target.value,
							})
						}
						placeholder={
							configured
								? "Enter new token to update"
								: "MTk4NjIyNDgzNDcxOTI1MjQ4.D..."
						}
						onKeyDown={(e) => {
							if (e.key === "Enter") onSave();
						}}
					/>
					<p className="mt-1.5 text-xs text-ink-faint">
						Need help?{" "}
						<a href="https://docs.spacebot.sh/discord-setup" target="_blank" rel="noopener noreferrer" className="text-accent hover:underline">
							Read the Discord setup docs &rarr;
						</a>
					</p>
				</div>
			)}

			{platform === "slack" && (
				<>
					<div>
						<label className="mb-1.5 block text-sm font-medium text-ink-dull">
							Bot Token
						</label>
						<Input
							type="password"
							size="lg"
							value={credentialInputs.slack_bot_token ?? ""}
							onChange={(e) =>
								setCredentialInputs({
									...credentialInputs,
									slack_bot_token: e.target.value,
								})
							}
							placeholder={
								configured ? "Enter new token to update" : "xoxb-..."
							}
						/>
					</div>
					<div>
						<label className="mb-1.5 block text-sm font-medium text-ink-dull">
							App Token
						</label>
						<Input
							type="password"
							size="lg"
							value={credentialInputs.slack_app_token ?? ""}
							onChange={(e) =>
								setCredentialInputs({
									...credentialInputs,
									slack_app_token: e.target.value,
								})
							}
							placeholder={
								configured ? "Enter new token to update" : "xapp-..."
							}
							onKeyDown={(e) => {
								if (e.key === "Enter") onSave();
							}}
						/>
					</div>
					<p className="text-xs text-ink-faint">
						Need help?{" "}
						<a href="https://docs.spacebot.sh/slack-setup" target="_blank" rel="noopener noreferrer" className="text-accent hover:underline">
							Read the Slack setup docs &rarr;
						</a>
					</p>
				</>
			)}

			{platform === "telegram" && (
				<div>
					<label className="mb-1.5 block text-sm font-medium text-ink-dull">
						Bot Token
					</label>
					<Input
						type="password"
						size="lg"
						value={credentialInputs.telegram_token ?? ""}
						onChange={(e) =>
							setCredentialInputs({
								...credentialInputs,
								telegram_token: e.target.value,
							})
						}
						placeholder={
							configured
								? "Enter new token to update"
								: "123456789:ABCdefGHI..."
						}
						onKeyDown={(e) => {
							if (e.key === "Enter") onSave();
						}}
					/>
					<p className="mt-1.5 text-xs text-ink-faint">
						Need help?{" "}
						<a href="https://docs.spacebot.sh/telegram-setup" target="_blank" rel="noopener noreferrer" className="text-accent hover:underline">
							Read the Telegram setup docs &rarr;
						</a>
					</p>
				</div>
			)}

			{platform === "twitch" && (
				<>
					<div>
						<label className="mb-1.5 block text-sm font-medium text-ink-dull">
							Bot Username
						</label>
						<Input
							size="lg"
							value={credentialInputs.twitch_username ?? ""}
							onChange={(e) =>
								setCredentialInputs({
									...credentialInputs,
									twitch_username: e.target.value,
								})
							}
							placeholder={
								configured ? "Enter new username to update" : "my_bot"
							}
						/>
					</div>
					<div className="grid grid-cols-1 md:grid-cols-2 gap-4">
						<div>
							<label className="mb-1.5 block text-sm font-medium text-ink-dull">
								Client ID
							</label>
							<Input
								size="lg"
								value={credentialInputs.twitch_client_id ?? ""}
								onChange={(e) =>
									setCredentialInputs({
										...credentialInputs,
										twitch_client_id: e.target.value,
									})
								}
								placeholder={
									configured ? "Enter new client id to update" : "your-app-client-id"
								}
							/>
						</div>
						<div>
							<label className="mb-1.5 block text-sm font-medium text-ink-dull">
								Client Secret
							</label>
							<Input
								type="password"
								size="lg"
								value={credentialInputs.twitch_client_secret ?? ""}
								onChange={(e) =>
									setCredentialInputs({
										...credentialInputs,
										twitch_client_secret: e.target.value,
									})
								}
								placeholder={
									configured ? "Enter new client secret to update" : "your-app-client-secret"
								}
							/>
						</div>
					</div>
					<div className="grid grid-cols-1 md:grid-cols-2 gap-4">
						<div>
							<label className="mb-1.5 block text-sm font-medium text-ink-dull">
								OAuth Access Token
							</label>
							<Input
								type="password"
								size="lg"
								value={credentialInputs.twitch_oauth_token ?? ""}
								onChange={(e) =>
									setCredentialInputs({
										...credentialInputs,
										twitch_oauth_token: e.target.value,
									})
								}
								placeholder={
									configured ? "Enter new token to update" : "abcd1234..."
								}
								onKeyDown={(e) => {
									if (e.key === "Enter") onSave();
								}}
							/>
						</div>
						<div>
							<label className="mb-1.5 block text-sm font-medium text-ink-dull">
								OAuth Refresh Token
							</label>
							<Input
								type="password"
								size="lg"
								value={credentialInputs.twitch_refresh_token ?? ""}
								onChange={(e) =>
									setCredentialInputs({
										...credentialInputs,
										twitch_refresh_token: e.target.value,
									})
								}
								placeholder={
									configured ? "Enter new refresh token to update" : "refresh-token-from-twitch"
								}
							/>
						</div>
					</div>
					<p className="mt-1 text-xs text-ink-faint">
						Use tokens from your Twitch application with chat:read and chat:write scopes enabled. Tokens are stored in your Spacebot instance and refreshed automatically while running.
					</p>
					<p className="mt-1.5 text-xs text-ink-faint">
						Need help?{" "}
						<a href="https://docs.spacebot.sh/twitch-setup" target="_blank" rel="noopener noreferrer" className="text-accent hover:underline">
							Read the Twitch setup docs &rarr;
						</a>
					</p>
				</>
			)}

			{platform === "webhook" && (
				<p className="text-sm text-ink-dull">
					Webhook receiver requires no additional credentials.
				</p>
			)}

			{platform !== "webhook" &&
				Object.values(credentialInputs).some((v) => v?.trim()) && (
					<Button onClick={onSave} loading={saving} size="sm">
						{configured ? "Update Credentials" : "Connect"}
					</Button>
				)}
		</div>
	);
}

function BindingsSection({
	platform,
	bindings,
	agents,
	isEditingOrAdding,
	editingBinding,
	bindingForm,
	setBindingForm,
	onStartAdd,
	onStartEdit,
	onCancelEdit,
	onAdd,
	onUpdate,
	onDelete,
	addPending,
	updatePending,
	deletePending,
}: {
	platform: Platform;
	bindings: BindingInfo[];
	agents: {id: string}[];
	isEditingOrAdding: boolean;
	editingBinding: BindingInfo | null;
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
	onStartAdd: () => void;
	onStartEdit: (binding: BindingInfo) => void;
	onCancelEdit: () => void;
	onAdd: () => void;
	onUpdate: () => void;
	onDelete: (binding: BindingInfo) => void;
	addPending: boolean;
	updatePending: boolean;
	deletePending: boolean;
}) {
	return (
		<div className="flex flex-col gap-3 border-t border-app-line/50 pt-4">
			<div className="flex items-center justify-between">
				<h3 className="text-sm font-medium text-ink">Bindings</h3>
				<Button size="sm" variant="outline" onClick={onStartAdd}>
					Add
				</Button>
			</div>

			{/* Binding list */}
			{bindings.length > 0 ? (
				<div className="rounded-md border border-app-line bg-app-box">
					{bindings.map((binding, idx) => (
						<div
							key={idx}
							className="flex items-center gap-2 border-b border-app-line/50 px-3 py-2 last:border-b-0"
						>
							<div className="flex-1 min-w-0">
								<span className="text-sm text-ink">{binding.agent_id}</span>
								<div className="flex flex-wrap gap-1.5 mt-0.5 text-tiny text-ink-faint">
									{binding.guild_id && <span>Guild: {binding.guild_id}</span>}
									{binding.workspace_id && (
										<span>Workspace: {binding.workspace_id}</span>
									)}
									{binding.chat_id && <span>Chat: {binding.chat_id}</span>}
									{binding.channel_ids.length > 0 && (
										<span>
											{binding.channel_ids.length} channel
											{binding.channel_ids.length > 1 ? "s" : ""}
										</span>
									)}
									{binding.dm_allowed_users.length > 0 && (
										<span>
											{binding.dm_allowed_users.length} DM user
											{binding.dm_allowed_users.length > 1 ? "s" : ""}
										</span>
									)}
									{binding.require_mention && (
										<span>Mention only</span>
									)}
									{!binding.guild_id &&
										!binding.workspace_id &&
										!binding.chat_id &&
										binding.channel_ids.length === 0 && (
											<span>All conversations</span>
										)}
								</div>
							</div>
							<Button
								size="sm"
								variant="outline"
								onClick={() => onStartEdit(binding)}
							>
								Edit
							</Button>
							<Button
								size="sm"
								variant="outline"
								onClick={() => onDelete(binding)}
								loading={deletePending}
							>
								Remove
							</Button>
						</div>
					))}
				</div>
			) : (
				<p className="text-sm text-ink-faint py-2">
					No bindings. Messages will route to the default agent.
				</p>
			)}

			{/* Add/Edit binding modal */}
			<Dialog
				open={isEditingOrAdding}
				onOpenChange={(open) => {
					if (!open) onCancelEdit();
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
						agents={agents}
						bindingForm={bindingForm}
						setBindingForm={setBindingForm}
						editing={!!editingBinding}
						onSave={editingBinding ? onUpdate : onAdd}
						onCancel={onCancelEdit}
						saving={editingBinding ? updatePending : addPending}
					/>
				</DialogContent>
			</Dialog>
		</div>
	);
}

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
				<label className="mb-1 block text-sm font-medium text-ink-dull">
					Agent
				</label>
				<Select
					value={bindingForm.agent_id}
					onValueChange={(v) => setBindingForm({...bindingForm, agent_id: v})}
				>
					<SelectTrigger>
						<SelectValue />
					</SelectTrigger>
					<SelectContent>
						{agents.map((a) => (
							<SelectItem key={a.id} value={a.id}>
								{a.id}
							</SelectItem>
						)) ?? <SelectItem value="main">main</SelectItem>}
					</SelectContent>
				</Select>
			</div>

			{platform === "discord" && (
				<div>
					<label className="mb-1 block text-sm font-medium text-ink-dull">
						Guild ID
					</label>
					<Input
						size="lg"
						value={bindingForm.guild_id}
						onChange={(e) =>
							setBindingForm({...bindingForm, guild_id: e.target.value})
						}
						placeholder="Optional — leave empty for all servers"
					/>
				</div>
			)}

			{platform === "slack" && (
				<div>
					<label className="mb-1 block text-sm font-medium text-ink-dull">
						Workspace ID
					</label>
					<Input
						size="lg"
						value={bindingForm.workspace_id}
						onChange={(e) =>
							setBindingForm({...bindingForm, workspace_id: e.target.value})
						}
						placeholder="Optional — leave empty for all workspaces"
					/>
				</div>
			)}

			{platform === "telegram" && (
				<div>
					<label className="mb-1 block text-sm font-medium text-ink-dull">
						Chat ID
					</label>
					<Input
						size="lg"
						value={bindingForm.chat_id}
						onChange={(e) =>
							setBindingForm({...bindingForm, chat_id: e.target.value})
						}
						placeholder="Optional — leave empty for all chats"
					/>
				</div>
			)}

			{(platform === "discord" || platform === "slack") && (
				<div>
					<label className="mb-1 block text-sm font-medium text-ink-dull">
						Channel IDs
					</label>
					<TagInput
						value={bindingForm.channel_ids}
						onChange={(ids) =>
							setBindingForm({...bindingForm, channel_ids: ids})
						}
						placeholder="Add channel ID..."
					/>
				</div>
			)}

			{platform === "discord" && (
				<div className="flex items-center gap-2">
					<input
						type="checkbox"
						checked={bindingForm.require_mention}
						onChange={(e) =>
							setBindingForm({...bindingForm, require_mention: e.target.checked})
						}
						className="h-4 w-4 rounded border-app-line bg-app-box"
					/>
					<label className="text-sm text-ink-dull">
						Require @mention or reply to bot
					</label>
				</div>
			)}

			{platform === "twitch" && (
				<div>
					<label className="mb-1 block text-sm font-medium text-ink-dull">
						Channels
					</label>
					<TagInput
						value={bindingForm.channel_ids}
						onChange={(ids) =>
							setBindingForm({...bindingForm, channel_ids: ids})
						}
						placeholder="Add channel name..."
					/>
				</div>
			)}

			<div>
				<label className="mb-1 block text-sm font-medium text-ink-dull">
					DM Allowed Users
				</label>
				<TagInput
					value={bindingForm.dm_allowed_users}
					onChange={(users) =>
						setBindingForm({...bindingForm, dm_allowed_users: users})
					}
					placeholder="Add user ID..."
				/>
			</div>

			<DialogFooter>
				<Button size="sm" variant="ghost" onClick={onCancel}>
					Cancel
				</Button>
				<Button size="sm" onClick={onSave} loading={saving}>
					{editing ? "Update" : "Add Binding"}
				</Button>
			</DialogFooter>
		</div>
	);
}

function DisconnectSection({
	name,
	confirmDisconnect,
	setConfirmDisconnect,
	onDisconnect,
	disconnecting,
}: {
	name: string;
	confirmDisconnect: boolean;
	setConfirmDisconnect: (v: boolean) => void;
	onDisconnect: () => void;
	disconnecting: boolean;
}) {
	return (
		<div className="border-t border-app-line/50 pt-4">
			{!confirmDisconnect ? (
				<Button
					variant="outline"
					size="sm"
					onClick={() => setConfirmDisconnect(true)}
				>
					Disconnect {name}
				</Button>
			) : (
				<div className="flex flex-col gap-2">
					<p className="text-sm text-red-400">
						This will remove all credentials and bindings for {name}. The bot
						will stop responding immediately.
					</p>
					<div className="flex gap-2">
						<Button
							variant="ghost"
							size="sm"
							onClick={() => setConfirmDisconnect(false)}
						>
							Cancel
						</Button>
						<Button
							size="sm"
							onClick={onDisconnect}
							loading={disconnecting}
							className="bg-red-500/20 text-red-400 hover:bg-red-500/30"
						>
							Confirm Disconnect
						</Button>
					</div>
				</div>
			)}
		</div>
	);
}
