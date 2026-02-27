import {useState} from "react";
import {useMutation, useQuery, useQueryClient} from "@tanstack/react-query";
import {api, type PlatformStatus, type BindingInfo} from "@/api/client";
import {
	Button,
	Input,
	Dialog,
	DialogContent,
	DialogHeader,
	DialogTitle,
	DialogDescription,
	Select,
	SelectTrigger,
	SelectValue,
	SelectContent,
	SelectItem,
} from "@/ui";
import {PlatformIcon} from "@/lib/platformIcons";
import {TagInput} from "@/components/TagInput";

type Platform = "discord" | "slack" | "telegram" | "twitch" | "email" | "webhook";

interface ChannelEditModalProps {
	platform: Platform;
	name: string;
	status?: PlatformStatus;
	open: boolean;
	onOpenChange: (open: boolean) => void;
}

export function ChannelEditModal({platform, name, status, open, onOpenChange}: ChannelEditModalProps) {
	const queryClient = useQueryClient();
	const configured = status?.configured ?? false;

	const [credentialInputs, setCredentialInputs] = useState<Record<string, string>>({});
	const [message, setMessage] = useState<{text: string; type: "success" | "error"} | null>(null);
	const [confirmDisconnect, setConfirmDisconnect] = useState(false);
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

	const {data: bindingsData} = useQuery({
		queryKey: ["bindings"],
		queryFn: () => api.bindings(),
		staleTime: 5_000,
	});

	const {data: agentsData} = useQuery({
		queryKey: ["agents"],
		queryFn: api.agents,
		staleTime: 10_000,
	});

	const platformBindings = bindingsData?.bindings?.filter((b) => b.channel === platform) ?? [];

	// Mutations
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
		onError: (error) => setMessage({text: `Failed: ${error.message}`, type: "error"}),
	});

	const addBinding = useMutation({
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
		onError: (error) => setMessage({text: `Failed: ${error.message}`, type: "error"}),
	});

	const updateBinding = useMutation({
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
		onError: (error) => setMessage({text: `Failed: ${error.message}`, type: "error"}),
	});

	const deleteBinding = useMutation({
		mutationFn: api.deleteBinding,
		onSuccess: (result) => {
			if (result.success) {
				setMessage({text: result.message, type: "success"});
				queryClient.invalidateQueries({queryKey: ["bindings"]});
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) => setMessage({text: `Failed: ${error.message}`, type: "error"}),
	});

	const disconnect = useMutation({
		mutationFn: () => api.disconnectPlatform(platform),
		onSuccess: () => {
			setConfirmDisconnect(false);
			queryClient.invalidateQueries({queryKey: ["messaging-status"]});
			queryClient.invalidateQueries({queryKey: ["bindings"]});
			onOpenChange(false);
		},
		onError: (error) => setMessage({text: `Failed: ${error.message}`, type: "error"}),
	});

	function resetBindingForm() {
		setBindingForm({agent_id: agentsData?.agents?.[0]?.id ?? "main", guild_id: "", workspace_id: "", chat_id: "", channel_ids: [], require_mention: false, dm_allowed_users: []});
	}

	function handleClose() {
		setMessage(null);
		setCredentialInputs({});
		setConfirmDisconnect(false);
		setEditingBinding(null);
		setAddingBinding(false);
		resetBindingForm();
		onOpenChange(false);
	}

	function handleSaveCredentials() {
		const request: any = {agent_id: "main", channel: platform};
		if (platform === "discord") {
			if (!credentialInputs.discord_token?.trim()) return;
			request.platform_credentials = {discord_token: credentialInputs.discord_token.trim()};
		} else if (platform === "slack") {
			if (!credentialInputs.slack_bot_token?.trim() || !credentialInputs.slack_app_token?.trim()) return;
			request.platform_credentials = {
				slack_bot_token: credentialInputs.slack_bot_token.trim(),
				slack_app_token: credentialInputs.slack_app_token.trim(),
			};
		} else if (platform === "telegram") {
			if (!credentialInputs.telegram_token?.trim()) return;
			request.platform_credentials = {telegram_token: credentialInputs.telegram_token.trim()};
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
		if (platform === "discord" && bindingForm.guild_id.trim()) request.guild_id = bindingForm.guild_id.trim();
		if (platform === "slack" && bindingForm.workspace_id.trim()) request.workspace_id = bindingForm.workspace_id.trim();
		if (platform === "telegram" && bindingForm.chat_id.trim()) request.chat_id = bindingForm.chat_id.trim();
		if (bindingForm.channel_ids.length > 0) request.channel_ids = bindingForm.channel_ids;
		if (platform === "discord" && bindingForm.require_mention) request.require_mention = true;
		if (bindingForm.dm_allowed_users.length > 0) request.dm_allowed_users = bindingForm.dm_allowed_users;
		addBinding.mutate(request);
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
		if (platform === "discord" && bindingForm.guild_id.trim()) request.guild_id = bindingForm.guild_id.trim();
		if (platform === "slack" && bindingForm.workspace_id.trim()) request.workspace_id = bindingForm.workspace_id.trim();
		if (platform === "telegram" && bindingForm.chat_id.trim()) request.chat_id = bindingForm.chat_id.trim();
		request.channel_ids = bindingForm.channel_ids;
		request.require_mention = platform === "discord" ? bindingForm.require_mention : false;
		request.dm_allowed_users = bindingForm.dm_allowed_users;
		updateBinding.mutate(request);
	}

	function handleDeleteBinding(binding: BindingInfo) {
		const request: any = {agent_id: binding.agent_id, channel: binding.channel};
		if (binding.guild_id) request.guild_id = binding.guild_id;
		if (binding.workspace_id) request.workspace_id = binding.workspace_id;
		if (binding.chat_id) request.chat_id = binding.chat_id;
		deleteBinding.mutate(request);
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
		<Dialog open={open} onOpenChange={(v) => { if (!v) handleClose(); }}>
			<DialogContent className="max-w-md max-h-[80vh] overflow-y-auto">
				<DialogHeader>
					<DialogTitle className="flex items-center gap-2">
						<PlatformIcon platform={platform} size="1x" className="text-ink-faint" />
						{name}
					</DialogTitle>
					<DialogDescription>
						{configured ? `Manage ${name} connection and bindings.` : `Connect ${name} to Spacebot.`}
					</DialogDescription>
				</DialogHeader>

				{/* -- Credentials -- */}
				<div className="flex flex-col gap-3">
					<h3 className="text-sm font-medium text-ink">
						{configured ? "Update Credentials" : "Credentials"}
					</h3>

					{platform === "discord" && (
						<div>
							<label className="mb-1.5 block text-sm font-medium text-ink-dull">Bot Token</label>
							<Input
								type="password"
								value={credentialInputs.discord_token ?? ""}
								onChange={(e) => setCredentialInputs({...credentialInputs, discord_token: e.target.value})}
								placeholder={configured ? "Enter new token to update" : "MTk4NjIyNDgzNDcxOTI1MjQ4.D..."}
								onKeyDown={(e) => { if (e.key === "Enter") handleSaveCredentials(); }}
							/>
						</div>
					)}

					{platform === "slack" && (
						<>
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink-dull">Bot Token</label>
								<Input
									type="password"
									value={credentialInputs.slack_bot_token ?? ""}
									onChange={(e) => setCredentialInputs({...credentialInputs, slack_bot_token: e.target.value})}
									placeholder={configured ? "Enter new token to update" : "xoxb-..."}
								/>
							</div>
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink-dull">App Token</label>
								<Input
									type="password"
									value={credentialInputs.slack_app_token ?? ""}
									onChange={(e) => setCredentialInputs({...credentialInputs, slack_app_token: e.target.value})}
									placeholder={configured ? "Enter new token to update" : "xapp-..."}
									onKeyDown={(e) => { if (e.key === "Enter") handleSaveCredentials(); }}
								/>
							</div>
						</>
					)}

					{platform === "telegram" && (
						<div>
							<label className="mb-1.5 block text-sm font-medium text-ink-dull">Bot Token</label>
							<Input
								type="password"
								value={credentialInputs.telegram_token ?? ""}
								onChange={(e) => setCredentialInputs({...credentialInputs, telegram_token: e.target.value})}
								placeholder={configured ? "Enter new token to update" : "123456789:ABCdefGHI..."}
								onKeyDown={(e) => { if (e.key === "Enter") handleSaveCredentials(); }}
							/>
						</div>
					)}

					{platform === "twitch" && (
						<>
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink-dull">Bot Username</label>
								<Input
									value={credentialInputs.twitch_username ?? ""}
									onChange={(e) => setCredentialInputs({...credentialInputs, twitch_username: e.target.value})}
									placeholder={configured ? "Enter new username to update" : "my_bot"}
								/>
							</div>
							<div className="grid grid-cols-1 md:grid-cols-2 gap-3">
								<div>
									<label className="mb-1.5 block text-sm font-medium text-ink-dull">Client ID</label>
									<Input
										value={credentialInputs.twitch_client_id ?? ""}
										onChange={(e) => setCredentialInputs({...credentialInputs, twitch_client_id: e.target.value})}
										placeholder={configured ? "Enter new client id to update" : "your-app-client-id"}
									/>
								</div>
								<div>
									<label className="mb-1.5 block text-sm font-medium text-ink-dull">Client Secret</label>
									<Input
										type="password"
										value={credentialInputs.twitch_client_secret ?? ""}
										onChange={(e) => setCredentialInputs({...credentialInputs, twitch_client_secret: e.target.value})}
										placeholder={configured ? "Enter new client secret to update" : "your-app-client-secret"}
									/>
								</div>
							</div>
							<div className="grid grid-cols-1 md:grid-cols-2 gap-3">
								<div>
									<label className="mb-1.5 block text-sm font-medium text-ink-dull">OAuth Access Token</label>
									<Input
										type="password"
										value={credentialInputs.twitch_oauth_token ?? ""}
										onChange={(e) => setCredentialInputs({...credentialInputs, twitch_oauth_token: e.target.value})}
										placeholder={configured ? "Enter new token to update" : "abcd1234..."}
										onKeyDown={(e) => { if (e.key === "Enter") handleSaveCredentials(); }}
									/>
								</div>
								<div>
									<label className="mb-1.5 block text-sm font-medium text-ink-dull">OAuth Refresh Token</label>
									<Input
										type="password"
										value={credentialInputs.twitch_refresh_token ?? ""}
										onChange={(e) => setCredentialInputs({...credentialInputs, twitch_refresh_token: e.target.value})}
										placeholder={configured ? "Enter new refresh token to update" : "refresh-token-from-twitch"}
									/>
								</div>
							</div>
							<p className="mt-1.5 text-xs text-ink-faint">
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

					{platform !== "webhook" && Object.values(credentialInputs).some((v) => v?.trim()) && (
						<Button onClick={handleSaveCredentials} loading={saveCreds.isPending} size="sm">
							{configured ? "Update Credentials" : "Connect"}
						</Button>
					)}
				</div>

				{/* -- Bindings -- */}
				{configured && (
					<div className="flex flex-col gap-3 border-t border-app-line pt-4">
						<div className="flex items-center justify-between">
							<h3 className="text-sm font-medium text-ink">Bindings</h3>
							{!isEditingOrAdding && (
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
							)}
						</div>

						{/* Binding list */}
						{!isEditingOrAdding && platformBindings.length > 0 && (
							<div className="rounded-md border border-app-line">
								{platformBindings.map((binding, idx) => (
									<div key={idx} className="flex items-center gap-2 border-b border-app-line/50 px-3 py-2 last:border-b-0">
										<div className="flex-1 min-w-0">
											<span className="text-sm text-ink">{binding.agent_id}</span>
											<div className="flex flex-wrap gap-1.5 mt-0.5 text-tiny text-ink-faint">
												{binding.guild_id && <span>Guild: {binding.guild_id}</span>}
												{binding.workspace_id && <span>Workspace: {binding.workspace_id}</span>}
												{binding.chat_id && <span>Chat: {binding.chat_id}</span>}
												{binding.channel_ids.length > 0 && <span>{binding.channel_ids.length} channel{binding.channel_ids.length > 1 ? "s" : ""}</span>}
												{binding.require_mention && <span>Mention only</span>}
												{binding.dm_allowed_users.length > 0 && <span>{binding.dm_allowed_users.length} DM user{binding.dm_allowed_users.length > 1 ? "s" : ""}</span>}
												{!binding.guild_id && !binding.workspace_id && !binding.chat_id && binding.channel_ids.length === 0 && (
													<span>All conversations</span>
												)}
											</div>
										</div>
										<Button size="sm" variant="ghost" onClick={() => startEditBinding(binding)}>
											Edit
										</Button>
										<Button size="sm" variant="ghost" onClick={() => handleDeleteBinding(binding)} loading={deleteBinding.isPending}>
											Remove
										</Button>
									</div>
								))}
							</div>
						)}

						{!isEditingOrAdding && platformBindings.length === 0 && (
							<p className="text-sm text-ink-faint py-2">
								No bindings. Messages will route to the default agent.
							</p>
						)}

						{/* Add/Edit binding form */}
						{isEditingOrAdding && (
							<div className="flex flex-col gap-3 rounded-md border border-app-line bg-app-darkBox/30 p-3">
								<div>
									<label className="mb-1 block text-sm font-medium text-ink-dull">Agent</label>
									<Select value={bindingForm.agent_id} onValueChange={(v) => setBindingForm({...bindingForm, agent_id: v})}>
										<SelectTrigger><SelectValue /></SelectTrigger>
										<SelectContent>
											{agentsData?.agents?.map((a) => (
												<SelectItem key={a.id} value={a.id}>{a.id}</SelectItem>
											)) ?? <SelectItem value="main">main</SelectItem>}
										</SelectContent>
									</Select>
								</div>

								{platform === "discord" && (
									<div>
										<label className="mb-1 block text-sm font-medium text-ink-dull">Guild ID</label>
										<Input
											value={bindingForm.guild_id}
											onChange={(e) => setBindingForm({...bindingForm, guild_id: e.target.value})}
											placeholder="Optional — leave empty for all servers"
										/>
									</div>
								)}

								{platform === "slack" && (
									<div>
										<label className="mb-1 block text-sm font-medium text-ink-dull">Workspace ID</label>
										<Input
											value={bindingForm.workspace_id}
											onChange={(e) => setBindingForm({...bindingForm, workspace_id: e.target.value})}
											placeholder="Optional — leave empty for all workspaces"
										/>
									</div>
								)}

								{platform === "telegram" && (
									<div>
										<label className="mb-1 block text-sm font-medium text-ink-dull">Chat ID</label>
										<Input
											value={bindingForm.chat_id}
											onChange={(e) => setBindingForm({...bindingForm, chat_id: e.target.value})}
											placeholder="Optional — leave empty for all chats"
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

								<div className="flex gap-2 justify-end">
									<Button
										size="sm"
										variant="ghost"
										onClick={() => { setEditingBinding(null); setAddingBinding(false); setMessage(null); }}
									>
										Cancel
									</Button>
									<Button
										size="sm"
										onClick={editingBinding ? handleUpdateBinding : handleAddBinding}
										loading={editingBinding ? updateBinding.isPending : addBinding.isPending}
									>
										{editingBinding ? "Update" : "Add Binding"}
									</Button>
								</div>
							</div>
						)}
					</div>
				)}

				{/* -- Status message -- */}
				{message && (
					<div className={`rounded-md border px-3 py-2 text-sm ${
						message.type === "success"
							? "border-green-500/20 bg-green-500/10 text-green-400"
							: "border-red-500/20 bg-red-500/10 text-red-400"
					}`}>
						{message.text}
					</div>
				)}

				{/* -- Disconnect -- */}
				{configured && platform !== "webhook" && (
					<div className="border-t border-app-line pt-4">
						{!confirmDisconnect ? (
							<Button variant="ghost" size="sm" onClick={() => setConfirmDisconnect(true)} className="text-red-400 hover:text-red-300">
								Disconnect {name}
							</Button>
						) : (
							<div className="flex flex-col gap-2">
								<p className="text-sm text-red-400">
									This will remove all credentials and bindings for {name}. The bot will stop responding immediately.
								</p>
								<div className="flex gap-2">
									<Button variant="ghost" size="sm" onClick={() => setConfirmDisconnect(false)}>
										Cancel
									</Button>
									<Button size="sm" onClick={() => disconnect.mutate()} loading={disconnect.isPending} className="bg-red-500/20 text-red-400 hover:bg-red-500/30">
										Confirm Disconnect
									</Button>
								</div>
							</div>
						)}
					</div>
				)}
			</DialogContent>
		</Dialog>
	);
}
