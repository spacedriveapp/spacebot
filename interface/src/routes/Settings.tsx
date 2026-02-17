import {useState, useEffect, useRef} from "react";
import {useQuery, useMutation, useQueryClient} from "@tanstack/react-query";
import {api, type PlatformStatus, type GlobalSettingsResponse, type BindingInfo} from "@/api/client";
import {Button, Input, SettingSidebarButton, Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter, Select, SelectTrigger, SelectValue, SelectContent, SelectItem} from "@/ui";
import {useSearch, useNavigate} from "@tanstack/react-router";
import {PlatformIcon} from "@/lib/platformIcons";
import {ProviderIcon} from "@/lib/providerIcons";
import {TagInput} from "@/components/TagInput";
import {parse as parseToml} from "smol-toml";

type SectionId = "providers" | "channels" | "bindings" | "api-keys" | "server" | "opencode" | "worker-logs" | "config-file";

const SECTIONS = [
	{
		id: "providers" as const,
		label: "Providers",
		group: "general" as const,
		description: "LLM provider API keys",
	},
	{
		id: "channels" as const,
		label: "Channels",
		group: "messaging" as const,
		description: "Messaging platform credentials",
	},
	{
		id: "bindings" as const,
		label: "Bindings",
		group: "messaging" as const,
		description: "Route conversations to agents",
	},
	{
		id: "api-keys" as const,
		label: "API Keys",
		group: "general" as const,
		description: "Third-party service keys",
	},
	{
		id: "server" as const,
		label: "Server",
		group: "system" as const,
		description: "API server configuration",
	},
	{
		id: "opencode" as const,
		label: "OpenCode",
		group: "system" as const,
		description: "OpenCode worker integration",
	},
	{
		id: "worker-logs" as const,
		label: "Worker Logs",
		group: "system" as const,
		description: "Worker execution logging",
	},
	{
		id: "config-file" as const,
		label: "Config File",
		group: "system" as const,
		description: "Raw config.toml editor",
	},
] satisfies {
	id: SectionId;
	label: string;
	group: string;
	description: string;
}[];

const PROVIDERS = [
	{
		id: "openrouter",
		name: "OpenRouter",
		description: "Multi-provider gateway with unified API",
		placeholder: "sk-or-...",
		envVar: "OPENROUTER_API_KEY",
	},
	{
		id: "opencode-zen",
		name: "OpenCode Zen",
		description: "Multi-format gateway (Kimi, GLM, MiniMax, Qwen)",
		placeholder: "...",
		envVar: "OPENCODE_ZEN_API_KEY",
	},
	{
		id: "anthropic",
		name: "Anthropic",
		description: "Claude models (Sonnet, Opus, Haiku)",
		placeholder: "sk-ant-...",
		envVar: "ANTHROPIC_API_KEY",
	},
	{
		id: "openai",
		name: "OpenAI",
		description: "GPT models",
		placeholder: "sk-...",
		envVar: "OPENAI_API_KEY",
	},
	{
		id: "ollama",
		name: "Ollama Cloud",
		description: "Hosted Ollama models via OpenAI-compatible API",
		placeholder: "ollama_...",
		envVar: "OLLAMA_API_KEY",
	},
	{
		id: "zhipu",
		name: "Z.ai (GLM)",
		description: "GLM models (GLM-4, GLM-4-Flash)",
		placeholder: "...",
		envVar: "ZHIPU_API_KEY",
	},
	{
		id: "groq",
		name: "Groq",
		description: "Fast inference for Llama, Mixtral models",
		placeholder: "gsk_...",
		envVar: "GROQ_API_KEY",
	},
	{
		id: "together",
		name: "Together AI",
		description: "Wide model selection with competitive pricing",
		placeholder: "...",
		envVar: "TOGETHER_API_KEY",
	},
	{
		id: "fireworks",
		name: "Fireworks AI",
		description: "Fast inference for popular OSS models",
		placeholder: "...",
		envVar: "FIREWORKS_API_KEY",
	},
	{
		id: "deepseek",
		name: "DeepSeek",
		description: "DeepSeek Chat and Reasoner models",
		placeholder: "sk-...",
		envVar: "DEEPSEEK_API_KEY",
	},
	{
		id: "xai",
		name: "xAI",
		description: "Grok models",
		placeholder: "xai-...",
		envVar: "XAI_API_KEY",
	},
	{
		id: "mistral",
		name: "Mistral AI",
		description: "Mistral Large, Small, Codestral models",
		placeholder: "...",
		envVar: "MISTRAL_API_KEY",
	},
] as const;

export function Settings() {
	const queryClient = useQueryClient();
	const navigate = useNavigate();
	const search = useSearch({from: "/settings"}) as {tab?: string};
	const [activeSection, setActiveSection] = useState<SectionId>("providers");

	// Sync activeSection with URL search param
	useEffect(() => {
		if (search.tab && SECTIONS.some(s => s.id === search.tab)) {
			setActiveSection(search.tab as SectionId);
		}
	}, [search.tab]);

	const handleSectionChange = (section: SectionId) => {
		setActiveSection(section);
		navigate({to: "/settings", search: {tab: section}});
	};
	const [editingProvider, setEditingProvider] = useState<string | null>(null);
	const [keyInput, setKeyInput] = useState("");
	const [message, setMessage] = useState<{
		text: string;
		type: "success" | "error";
	} | null>(null);

	// Fetch providers data (only when on providers tab)
	const {data, isLoading} = useQuery({
		queryKey: ["providers"],
		queryFn: api.providers,
		staleTime: 5_000,
		enabled: activeSection === "providers",
	});

	// Fetch global settings (only when on api-keys, server, or worker-logs tabs)
	const {data: globalSettings, isLoading: globalSettingsLoading} = useQuery({
		queryKey: ["global-settings"],
		queryFn: api.globalSettings,
		staleTime: 5_000,
		enabled: activeSection === "api-keys" || activeSection === "server" || activeSection === "opencode" || activeSection === "worker-logs",
	});

	const updateMutation = useMutation({
		mutationFn: ({provider, apiKey}: {provider: string; apiKey: string}) =>
			api.updateProvider(provider, apiKey),
		onSuccess: (result) => {
			if (result.success) {
				setEditingProvider(null);
				setKeyInput("");
				setMessage({text: result.message, type: "success"});
				queryClient.invalidateQueries({queryKey: ["providers"]});
				// Agents will auto-start on the backend, refetch agent list after a short delay
				setTimeout(() => {
					queryClient.invalidateQueries({queryKey: ["agents"]});
					queryClient.invalidateQueries({queryKey: ["overview"]});
				}, 3000);
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) => {
			setMessage({text: `Failed: ${error.message}`, type: "error"});
		},
	});

	const removeMutation = useMutation({
		mutationFn: (provider: string) => api.removeProvider(provider),
		onSuccess: (result) => {
			if (result.success) {
				setMessage({text: result.message, type: "success"});
				queryClient.invalidateQueries({queryKey: ["providers"]});
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) => {
			setMessage({text: `Failed: ${error.message}`, type: "error"});
		},
	});

	const editingProviderData = PROVIDERS.find((p) => p.id === editingProvider);

	const handleSave = () => {
		if (!keyInput.trim() || !editingProvider) return;
		updateMutation.mutate({provider: editingProvider, apiKey: keyInput.trim()});
	};

	const handleClose = () => {
		setEditingProvider(null);
		setKeyInput("");
	};

	const isConfigured = (providerId: string): boolean => {
		if (!data) return false;
		return data.providers[providerId as keyof typeof data.providers] ?? false;
	};

	return (
		<div className="flex h-full">
			{/* Sidebar */}
			<div className="flex w-52 flex-shrink-0 flex-col border-r border-app-line/50 bg-app-darkBox/20 overflow-y-auto">
				<div className="px-3 pb-1 pt-4">
					<span className="text-tiny font-medium uppercase tracking-wider text-ink-faint">
						Settings
					</span>
				</div>
				<div className="flex flex-col gap-0.5 px-2">
					{SECTIONS.map((section) => (
						<SettingSidebarButton
							key={section.id}
							onClick={() => handleSectionChange(section.id)}
							active={activeSection === section.id}
						>
							<span className="flex-1">{section.label}</span>
						</SettingSidebarButton>
					))}
				</div>
			</div>

			{/* Content */}
			<div className="flex flex-1 flex-col overflow-hidden">
				<header className="flex h-12 items-center border-b border-app-line bg-app-darkBox/50 px-6">
					<h1 className="font-plex text-sm font-medium text-ink">
						{SECTIONS.find((s) => s.id === activeSection)?.label}
					</h1>
				</header>
				<div className="flex-1 overflow-y-auto">
					{activeSection === "providers" ? (
					<div className="mx-auto max-w-2xl px-6 py-6">
						{/* Section header */}
						<div className="mb-6">
							<h2 className="font-plex text-sm font-semibold text-ink">
								LLM Providers
							</h2>
							<p className="mt-1 text-sm text-ink-dull">
								Configure API keys for LLM providers. At least one provider is
								required for agents to function.
							</p>
						</div>

						{isLoading ? (
							<div className="flex items-center gap-2 text-ink-dull">
								<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
								Loading providers...
							</div>
						) : (
							<div className="flex flex-col gap-3">
								{PROVIDERS.map((provider) => (
									<ProviderCard
										key={provider.id}
										provider={provider.id}
										name={provider.name}
										description={provider.description}
										configured={isConfigured(provider.id)}
										onEdit={() => {
											setEditingProvider(provider.id);
											setKeyInput("");
											setMessage(null);
										}}
										onRemove={() => removeMutation.mutate(provider.id)}
										removing={removeMutation.isPending}
									/>
								))}
							</div>
						)}

						{/* Info note */}
						<div className="mt-6 rounded-md border border-app-line bg-app-darkBox/20 px-4 py-3">
							<p className="text-sm text-ink-faint">
								Keys are written to{" "}
								<code className="rounded bg-app-box px-1 py-0.5 text-tiny text-ink-dull">
									config.toml
								</code>{" "}
								in your instance directory. You can also set them via
								environment variables (
								<code className="rounded bg-app-box px-1 py-0.5 text-tiny text-ink-dull">
									ANTHROPIC_API_KEY
								</code>
								, etc.).
							</p>
						</div>
					</div>
					) : activeSection === "channels" ? (
						<ChannelsSection />
					) : activeSection === "bindings" ? (
						<BindingsSection />
					) : activeSection === "api-keys" ? (
						<ApiKeysSection settings={globalSettings} isLoading={globalSettingsLoading} />
					) : activeSection === "server" ? (
						<ServerSection settings={globalSettings} isLoading={globalSettingsLoading} />
					) : activeSection === "opencode" ? (
						<OpenCodeSection settings={globalSettings} isLoading={globalSettingsLoading} />
					) : activeSection === "worker-logs" ? (
						<WorkerLogsSection settings={globalSettings} isLoading={globalSettingsLoading} />
					) : activeSection === "config-file" ? (
						<ConfigFileSection />
					) : null}
				</div>
			</div>

			<Dialog open={!!editingProvider} onOpenChange={(open) => { if (!open) handleClose(); }}>
				<DialogContent className="max-w-md">
					<DialogHeader>
						<DialogTitle>{isConfigured(editingProvider ?? "") ? "Update" : "Add"} API Key</DialogTitle>
						<DialogDescription>
							Enter your {editingProviderData?.name} API key. It will be saved to your instance config.
						</DialogDescription>
					</DialogHeader>
					<Input
						type="password"
						value={keyInput}
						onChange={(e) => setKeyInput(e.target.value)}
						placeholder={editingProviderData?.placeholder}
						autoFocus
						onKeyDown={(e) => {
							if (e.key === "Enter") handleSave();
						}}
					/>
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
					<DialogFooter>
						<Button onClick={handleClose} variant="ghost" size="sm">
							Cancel
						</Button>
						<Button
							onClick={handleSave}
							disabled={!keyInput.trim()}
							loading={updateMutation.isPending}
							size="sm"
						>
							Save
						</Button>
					</DialogFooter>
				</DialogContent>
			</Dialog>
		</div>
	);
}

function ChannelsSection() {
	const queryClient = useQueryClient();
	const [editingPlatform, setEditingPlatform] = useState<"discord" | "slack" | "telegram" | "webhook" | null>(null);
	const [platformInputs, setPlatformInputs] = useState<Record<string, string>>({});
	const [message, setMessage] = useState<{
		text: string;
		type: "success" | "error";
	} | null>(null);

	const {data: messagingStatus, isLoading: statusLoading} = useQuery({
		queryKey: ["messaging-status"],
		queryFn: api.messagingStatus,
		staleTime: 5_000,
	});

	const createPlatformMutation = useMutation({
		mutationFn: api.createBinding,
		onSuccess: (result) => {
			if (result.success) {
				setEditingPlatform(null);
				setPlatformInputs({});
				setMessage({text: result.message, type: "success"});
				queryClient.invalidateQueries({queryKey: ["messaging-status"]});
				queryClient.invalidateQueries({queryKey: ["bindings"]});
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) => {
			setMessage({text: `Failed: ${error.message}`, type: "error"});
		},
	});

	const isLoading = statusLoading;

	const handleClose = () => {
		setEditingPlatform(null);
		setPlatformInputs({});
		setMessage(null);
	};

	const handleSavePlatform = () => {
		if (!editingPlatform) return;

		const request: any = {
			agent_id: "main",
			channel: editingPlatform,
		};

		if (editingPlatform === "discord") {
			if (!platformInputs.discord_token?.trim()) return;
			request.platform_credentials = {
				discord_token: platformInputs.discord_token.trim(),
			};
		} else if (editingPlatform === "slack") {
			if (!platformInputs.slack_bot_token?.trim() || !platformInputs.slack_app_token?.trim()) return;
			request.platform_credentials = {
				slack_bot_token: platformInputs.slack_bot_token.trim(),
				slack_app_token: platformInputs.slack_app_token.trim(),
			};
		} else if (editingPlatform === "telegram") {
			if (!platformInputs.telegram_token?.trim()) return;
			request.platform_credentials = {
				telegram_token: platformInputs.telegram_token.trim(),
			};
		}

		createPlatformMutation.mutate(request);
	};



	return (
		<div className="mx-auto max-w-2xl px-6 py-6">
			{/* Section header */}
			<div className="mb-6">
				<h2 className="font-plex text-sm font-semibold text-ink">
					Messaging Platforms
				</h2>
				<p className="mt-1 text-sm text-ink-dull">
					Configure messaging platform credentials. Once enabled, create bindings to route conversations to agents.
				</p>
			</div>

			{isLoading ? (
				<div className="flex items-center gap-2 text-ink-dull">
					<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
					Loading channels...
				</div>
			) : (
				<>
					{/* Platform Status Cards */}
					<div className="mb-6 flex flex-col gap-3">
						<PlatformCard
							platform="discord"
							name="Discord"
							description="Discord bot integration"
							status={messagingStatus?.discord}
							onSetup={() => {
								setEditingPlatform("discord");
								setPlatformInputs({});
								setMessage(null);
							}}
						/>
						<PlatformCard
							platform="slack"
							name="Slack"
							description="Slack bot integration"
							status={messagingStatus?.slack}
							onSetup={() => {
								setEditingPlatform("slack");
								setPlatformInputs({});
								setMessage(null);
							}}
						/>
						<PlatformCard
							platform="telegram"
							name="Telegram"
							description="Telegram bot integration"
							status={messagingStatus?.telegram}
							onSetup={() => {
								setEditingPlatform("telegram");
								setPlatformInputs({});
								setMessage(null);
							}}
						/>
						<PlatformCard
							platform="webhook"
							name="Webhook"
							description="HTTP webhook receiver"
							status={messagingStatus?.webhook}
							onSetup={() => {
								setEditingPlatform("webhook");
								setPlatformInputs({});
								setMessage(null);
							}}
						/>
						
						{/* Coming Soon Platforms */}
						<PlatformCard
							platform="email"
							name="Email"
							description="IMAP polling for inbound, SMTP for outbound"
							disabled
						/>
						<PlatformCard
							platform="whatsapp"
							name="WhatsApp"
							description="Meta Cloud API integration"
							disabled
						/>
						<PlatformCard
							platform="matrix"
							name="Matrix"
							description="Decentralized chat protocol"
							disabled
						/>
						<PlatformCard
							platform="imessage"
							name="iMessage"
							description="macOS-only AppleScript bridge"
							disabled
						/>
						<PlatformCard
							platform="irc"
							name="IRC"
							description="TLS socket connection"
							disabled
						/>
						<PlatformCard
							platform="lark"
							name="Lark"
							description="Feishu/Lark webhook integration"
							disabled
						/>
						<PlatformCard
							platform="dingtalk"
							name="DingTalk"
							description="Chinese enterprise webhook integration"
							disabled
						/>
					</div>
				</>
			)}

			{/* Info note */}
			<div className="mt-6 rounded-md border border-app-line bg-app-darkBox/20 px-4 py-3">
				<p className="text-sm text-ink-faint">
					Platform credentials are stored in{" "}
					<code className="rounded bg-app-box px-1 py-0.5 text-tiny text-ink-dull">
						config.toml
					</code>
					. Once configured, go to the Bindings tab to route conversations to agents.
				</p>
			</div>

			{/* Platform Setup Modal */}
			<Dialog open={!!editingPlatform} onOpenChange={(open) => { if (!open) handleClose(); }}>
				<DialogContent className="max-w-md">
					<DialogHeader>
						<DialogTitle>
							{editingPlatform === "discord" && "Configure Discord"}
							{editingPlatform === "slack" && "Configure Slack"}
							{editingPlatform === "telegram" && "Configure Telegram"}
							{editingPlatform === "webhook" && "Configure Webhook"}
						</DialogTitle>
						<DialogDescription>
							{editingPlatform === "discord" && "Enter your Discord bot token to enable Discord integration."}
							{editingPlatform === "slack" && "Enter your Slack bot and app tokens to enable Slack integration."}
							{editingPlatform === "telegram" && "Enter your Telegram bot token to enable Telegram integration."}
							{editingPlatform === "webhook" && "Configure webhook receiver settings."}
						</DialogDescription>
					</DialogHeader>
					
					{editingPlatform === "discord" && (
						<div className="flex flex-col gap-3">
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink">Bot Token</label>
								<Input
									type="password"
									value={platformInputs.discord_token ?? ""}
									onChange={(e) => setPlatformInputs({...platformInputs, discord_token: e.target.value})}
									placeholder="MTk4NjIyNDgzNDcxOTI1MjQ4.D..."
									autoFocus
									onKeyDown={(e) => {
										if (e.key === "Enter") handleSavePlatform();
									}}
								/>
								<p className="mt-1 text-tiny text-ink-faint">
									Get this from the Discord Developer Portal
								</p>
							</div>
						</div>
					)}

					{editingPlatform === "slack" && (
						<div className="flex flex-col gap-3">
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink">Bot Token</label>
								<Input
									type="password"
									value={platformInputs.slack_bot_token ?? ""}
									onChange={(e) => setPlatformInputs({...platformInputs, slack_bot_token: e.target.value})}
									placeholder="xoxb-..."
									autoFocus
									onKeyDown={(e) => {
										if (e.key === "Enter" && platformInputs.slack_app_token?.trim()) handleSavePlatform();
									}}
								/>
							</div>
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink">App Token</label>
								<Input
									type="password"
									value={platformInputs.slack_app_token ?? ""}
									onChange={(e) => setPlatformInputs({...platformInputs, slack_app_token: e.target.value})}
									placeholder="xapp-..."
									onKeyDown={(e) => {
										if (e.key === "Enter") handleSavePlatform();
									}}
								/>
							</div>
							<p className="text-tiny text-ink-faint">
								Get these from your Slack app settings
							</p>
						</div>
					)}

					{editingPlatform === "telegram" && (
						<div className="flex flex-col gap-3">
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink">Bot Token</label>
								<Input
									type="password"
									value={platformInputs.telegram_token ?? ""}
									onChange={(e) => setPlatformInputs({...platformInputs, telegram_token: e.target.value})}
									placeholder="123456789:ABCdefGHIjklMNOpqrsTUVwxyz"
									autoFocus
									onKeyDown={(e) => {
										if (e.key === "Enter") handleSavePlatform();
									}}
								/>
								<p className="mt-1 text-tiny text-ink-faint">
									Get this from @BotFather on Telegram
								</p>
							</div>
						</div>
					)}

					{editingPlatform === "webhook" && (
						<div className="flex flex-col gap-3">
							<p className="text-sm text-ink-dull">
								Webhook receiver is configured in <code className="rounded bg-app-box px-1 py-0.5 text-tiny">config.toml</code>. 
								No additional setup required here.
							</p>
						</div>
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

					<DialogFooter>
						<Button onClick={handleClose} variant="ghost" size="sm">
							Cancel
						</Button>
						{editingPlatform !== "webhook" && (
							<Button
								onClick={handleSavePlatform}
								disabled={
									editingPlatform === "discord" ? !platformInputs.discord_token?.trim() :
									editingPlatform === "slack" ? (!platformInputs.slack_bot_token?.trim() || !platformInputs.slack_app_token?.trim()) :
									editingPlatform === "telegram" ? !platformInputs.telegram_token?.trim() :
									false
								}
								loading={createPlatformMutation.isPending}
								size="sm"
							>
								Save
							</Button>
						)}
					</DialogFooter>
				</DialogContent>
			</Dialog>
		</div>
	);
}

function BindingsSection() {
	const queryClient = useQueryClient();
	const [addingBinding, setAddingBinding] = useState(false);
	const [editingBinding, setEditingBinding] = useState<BindingInfo | null>(null);
	const [bindingForm, setBindingForm] = useState({
		agent_id: "main",
		channel: "discord" as "discord" | "slack" | "telegram" | "webhook",
		guild_id: "",
		workspace_id: "",
		chat_id: "",
		channel_ids: [] as string[],
		dm_allowed_users: [] as string[],
	});
	const [message, setMessage] = useState<{
		text: string;
		type: "success" | "error";
	} | null>(null);

	const {data: bindingsData, isLoading: bindingsLoading} = useQuery({
		queryKey: ["bindings"],
		queryFn: () => api.bindings(),
		staleTime: 5_000,
	});

	const {data: agentsData} = useQuery({
		queryKey: ["agents"],
		queryFn: api.agents,
		staleTime: 10_000,
	});

	const addBindingMutation = useMutation({
		mutationFn: api.createBinding,
		onSuccess: (result) => {
			if (result.success) {
				setAddingBinding(false);
				setBindingForm({
					agent_id: "main",
					channel: "discord",
					guild_id: "",
					workspace_id: "",
					chat_id: "",
					channel_ids: [],
					dm_allowed_users: [],
				});
				setMessage({text: result.message, type: "success"});
				queryClient.invalidateQueries({queryKey: ["bindings"]});
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) => {
			setMessage({text: `Failed: ${error.message}`, type: "error"});
		},
	});

	const updateBindingMutation = useMutation({
		mutationFn: api.updateBinding,
		onSuccess: (result) => {
			if (result.success) {
				setEditingBinding(null);
				setBindingForm({
					agent_id: "main",
					channel: "discord",
					guild_id: "",
					workspace_id: "",
					chat_id: "",
					channel_ids: [],
					dm_allowed_users: [],
				});
				setMessage({text: result.message, type: "success"});
				queryClient.invalidateQueries({queryKey: ["bindings"]});
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) => {
			setMessage({text: `Failed: ${error.message}`, type: "error"});
		},
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
		onError: (error) => {
			setMessage({text: `Failed: ${error.message}`, type: "error"});
		},
	});

	const handleAddBinding = () => {
		const request: any = {
			agent_id: bindingForm.agent_id,
			channel: bindingForm.channel,
		};

		if (bindingForm.channel === "discord" && bindingForm.guild_id.trim()) {
			request.guild_id = bindingForm.guild_id.trim();
		}
		if (bindingForm.channel === "slack" && bindingForm.workspace_id.trim()) {
			request.workspace_id = bindingForm.workspace_id.trim();
		}
		if (bindingForm.channel === "telegram" && bindingForm.chat_id.trim()) {
			request.chat_id = bindingForm.chat_id.trim();
		}

		if (bindingForm.channel_ids.length > 0) {
			request.channel_ids = bindingForm.channel_ids;
		}

		if (bindingForm.dm_allowed_users.length > 0) {
			request.dm_allowed_users = bindingForm.dm_allowed_users;
		}

		addBindingMutation.mutate(request);
	};

	const handleEditBinding = (binding: BindingInfo) => {
		setEditingBinding(binding);
		setBindingForm({
			agent_id: binding.agent_id,
			channel: binding.channel as any,
			guild_id: binding.guild_id || "",
			workspace_id: binding.workspace_id || "",
			chat_id: binding.chat_id || "",
			channel_ids: binding.channel_ids,
			dm_allowed_users: binding.dm_allowed_users,
		});
	};

	const handleUpdateBinding = () => {
		if (!editingBinding) return;

		const request: any = {
			original_agent_id: editingBinding.agent_id,
			original_channel: editingBinding.channel,
			original_guild_id: editingBinding.guild_id || undefined,
			original_workspace_id: editingBinding.workspace_id || undefined,
			original_chat_id: editingBinding.chat_id || undefined,
			agent_id: bindingForm.agent_id,
			channel: bindingForm.channel,
		};

		if (bindingForm.channel === "discord" && bindingForm.guild_id.trim()) {
			request.guild_id = bindingForm.guild_id.trim();
		}
		if (bindingForm.channel === "slack" && bindingForm.workspace_id.trim()) {
			request.workspace_id = bindingForm.workspace_id.trim();
		}
		if (bindingForm.channel === "telegram" && bindingForm.chat_id.trim()) {
			request.chat_id = bindingForm.chat_id.trim();
		}

		request.channel_ids = bindingForm.channel_ids;
		request.dm_allowed_users = bindingForm.dm_allowed_users;

		updateBindingMutation.mutate(request);
	};

	const handleDeleteBinding = (binding: any) => {
		const request: any = {
			agent_id: binding.agent_id,
			channel: binding.channel,
		};

		if (binding.guild_id) request.guild_id = binding.guild_id;
		if (binding.workspace_id) request.workspace_id = binding.workspace_id;
		if (binding.chat_id) request.chat_id = binding.chat_id;

		deleteBindingMutation.mutate(request);
	};

	return (
		<div className="mx-auto max-w-2xl px-6 py-6">
			<div className="mb-6">
				<h2 className="font-plex text-sm font-semibold text-ink">Conversation Bindings</h2>
				<p className="mt-1 text-sm text-ink-dull">
					Route conversations from messaging platforms to agents. The first matching binding wins.
				</p>
			</div>

			{bindingsLoading ? (
				<div className="flex items-center gap-2 text-ink-dull">
					<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
					Loading bindings...
				</div>
			) : (
				<>
					<div className="mb-4 flex items-center justify-between">
						<h3 className="font-plex text-sm font-medium text-ink">Active Bindings</h3>
						<Button
							size="sm"
							variant="outline"
							onClick={() => {
								setAddingBinding(true);
								setEditingBinding(null);
								setBindingForm({
									agent_id: agentsData?.agents?.[0]?.id ?? "main",
									channel: "discord",
									guild_id: "",
									workspace_id: "",
									chat_id: "",
									channel_ids: [],
									dm_allowed_users: [],
								});
								setMessage(null);
							}}
						>
							Add Binding
						</Button>
					</div>

					{bindingsData?.bindings && bindingsData.bindings.length > 0 ? (
						<div className="rounded-lg border border-app-line bg-app-box">
							{bindingsData.bindings.map((binding, idx) => (
								<div
									key={idx}
									className="flex items-center gap-3 border-b border-app-line/50 px-4 py-3 last:border-b-0"
								>
									<PlatformIcon platform={binding.channel} size="1x" className="text-ink-faint" />
									<div className="flex-1">
										<div className="flex items-center gap-2">
											<span className="text-sm font-medium text-ink">
												{binding.agent_id}
											</span>
											<span className="text-sm text-ink-faint">→</span>
											<span className="text-sm text-ink-dull">
												{binding.channel}
											</span>
										</div>
										<div className="mt-1 flex items-center gap-2 text-tiny text-ink-faint">
											{binding.guild_id && (
												<span>Guild: {binding.guild_id}</span>
											)}
											{binding.workspace_id && (
												<span>Workspace: {binding.workspace_id}</span>
											)}
											{binding.chat_id && (
												<span>Chat: {binding.chat_id}</span>
											)}
											{binding.channel_ids.length > 0 && (
												<span>Channels: {binding.channel_ids.length}</span>
											)}
											{binding.dm_allowed_users.length > 0 && (
												<span>DM Users: {binding.dm_allowed_users.length}</span>
											)}
										</div>
									</div>
									<div className="flex items-center gap-2">
										<Button 
											size="sm" 
											variant="ghost"
											onClick={() => handleEditBinding(binding)}
										>
											Edit
										</Button>
										<Button 
											size="sm" 
											variant="ghost"
											onClick={() => handleDeleteBinding(binding)}
											loading={deleteBindingMutation.isPending}
										>
											Remove
										</Button>
									</div>
								</div>
							))}
						</div>
					) : (
						<div className="flex flex-col items-center justify-center rounded-lg border border-dashed border-app-line/50 bg-app-darkBox/20 py-12">
							<p className="text-sm text-ink-faint">No bindings configured</p>
							<p className="mt-1 text-tiny text-ink-faint/70">
								Add a binding to route messages to an agent
							</p>
						</div>
					)}

					{message && (
						<div
							className={`mt-4 rounded-md border px-3 py-2 text-sm ${
								message.type === "success"
									? "border-green-500/20 bg-green-500/10 text-green-400"
									: "border-red-500/20 bg-red-500/10 text-red-400"
							}`}
						>
							{message.text}
						</div>
					)}
				</>
			)}

			<div className="mt-6 rounded-md border border-app-line bg-app-darkBox/20 px-4 py-3">
				<p className="text-sm text-ink-faint">
					Bindings match conversations to agents based on platform, server/workspace, channel, and user filters. 
					Configure platform credentials in the <strong>Channels</strong> tab first.
				</p>
			</div>

			<Dialog open={addingBinding || editingBinding !== null} onOpenChange={(open) => { 
				if (!open) {
					setAddingBinding(false);
					setEditingBinding(null);
					setMessage(null);
				}
			}}>
				<DialogContent className="max-w-md">
					<DialogHeader>
						<DialogTitle>{editingBinding ? "Edit Binding" : "Add Binding"}</DialogTitle>
						<DialogDescription>
							Route messages from a specific platform location to an agent.
						</DialogDescription>
					</DialogHeader>

					<div className="flex flex-col gap-4">
						<div>
							<label className="mb-1.5 block text-sm font-medium text-ink">Agent</label>
							<Select
								value={bindingForm.agent_id}
								onValueChange={(value) => setBindingForm({...bindingForm, agent_id: value})}
							>
								<SelectTrigger>
									<SelectValue />
								</SelectTrigger>
								<SelectContent>
									{agentsData?.agents?.map((agent) => (
										<SelectItem key={agent.id} value={agent.id}>
											{agent.id}
										</SelectItem>
									)) ?? (
										<SelectItem value="main">main</SelectItem>
									)}
								</SelectContent>
							</Select>
						</div>

						<div>
							<label className="mb-1.5 block text-sm font-medium text-ink">Platform</label>
							<Select
								value={bindingForm.channel}
								onValueChange={(value: any) => setBindingForm({...bindingForm, channel: value})}
							>
								<SelectTrigger>
									<SelectValue />
								</SelectTrigger>
								<SelectContent>
									<SelectItem value="discord">Discord</SelectItem>
									<SelectItem value="slack">Slack</SelectItem>
									<SelectItem value="telegram">Telegram</SelectItem>
									<SelectItem value="webhook">Webhook</SelectItem>
								</SelectContent>
							</Select>
						</div>

						{bindingForm.channel === "discord" && (
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink">Guild ID</label>
								<Input
									value={bindingForm.guild_id}
									onChange={(e) => setBindingForm({...bindingForm, guild_id: e.target.value})}
									placeholder="123456789 (optional)"
								/>
								<p className="mt-1 text-tiny text-ink-faint">
									Leave empty to match any server
								</p>
							</div>
						)}

						{bindingForm.channel === "slack" && (
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink">Workspace ID</label>
								<Input
									value={bindingForm.workspace_id}
									onChange={(e) => setBindingForm({...bindingForm, workspace_id: e.target.value})}
									placeholder="T0123456789 (optional)"
								/>
								<p className="mt-1 text-tiny text-ink-faint">
									Leave empty to match any workspace
								</p>
							</div>
						)}

						{bindingForm.channel === "telegram" && (
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink">Chat ID</label>
								<Input
									value={bindingForm.chat_id}
									onChange={(e) => setBindingForm({...bindingForm, chat_id: e.target.value})}
									placeholder="-1001234567890 (optional)"
								/>
								<p className="mt-1 text-tiny text-ink-faint">
									Leave empty to match any chat
								</p>
							</div>
						)}

						{(bindingForm.channel === "discord" || bindingForm.channel === "slack") && (
							<div>
								<label className="mb-1.5 block text-sm font-medium text-ink">Channel IDs</label>
								<TagInput
									value={bindingForm.channel_ids}
									onChange={(ids) => setBindingForm({...bindingForm, channel_ids: ids})}
									placeholder="Add channel ID..."
								/>
								<p className="mt-1 text-tiny text-ink-faint">
									Leave empty to match all channels
								</p>
							</div>
						)}

						<div>
							<label className="mb-1.5 block text-sm font-medium text-ink">DM Allowed Users</label>
							<TagInput
								value={bindingForm.dm_allowed_users}
								onChange={(users) => setBindingForm({...bindingForm, dm_allowed_users: users})}
								placeholder="Add user ID..."
							/>
							<p className="mt-1 text-tiny text-ink-faint">
								User IDs allowed to send DMs
							</p>
						</div>
					</div>

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

					<DialogFooter>
						<Button 
							onClick={() => {
								setAddingBinding(false);
								setEditingBinding(null);
								setMessage(null);
							}} 
							variant="ghost" 
							size="sm"
						>
							Cancel
						</Button>
						<Button
							onClick={editingBinding ? handleUpdateBinding : handleAddBinding}
							loading={editingBinding ? updateBindingMutation.isPending : addBindingMutation.isPending}
							size="sm"
						>
							{editingBinding ? "Update Binding" : "Add Binding"}
						</Button>
					</DialogFooter>
				</DialogContent>
			</Dialog>
		</div>
	);
}

interface PlatformCardProps {
	platform: string;
	name: string;
	description: string;
	status?: PlatformStatus;
	disabled?: boolean;
	onSetup?: () => void;
}

function PlatformCard({ platform, name, description, status, disabled = false, onSetup }: PlatformCardProps) {
	const configured = status?.configured ?? false;
	const enabled = status?.enabled ?? false;

	return (
		<div className={`rounded-lg border border-app-line bg-app-box p-4 ${disabled ? "opacity-40" : ""}`}>
			<div className="flex items-center gap-3">
				<PlatformIcon platform={platform} size="lg" className={disabled ? "text-ink-faint/50" : "text-ink-faint"} />
				<div className="flex-1">
					<div className="flex items-center gap-2">
						<span className="text-sm font-medium text-ink">{name}</span>
						{!disabled && configured && (
							<span className={`text-tiny ${enabled ? "text-green-400" : "text-ink-faint"}`}>
								{enabled ? "● Active" : "○ Disabled"}
							</span>
						)}
					</div>
					<p className="mt-0.5 text-sm text-ink-dull">{description}</p>
				</div>
				<div className="flex gap-2">
					{disabled ? (
						<Button variant="outline" size="sm" disabled>
							Coming Soon
						</Button>
					) : onSetup && (
						<Button onClick={onSetup} variant="outline" size="sm">
							{configured ? "Configure" : "Setup"}
						</Button>
					)}
				</div>
			</div>
		</div>
	);
}

interface GlobalSettingsSectionProps {
	settings: GlobalSettingsResponse | undefined;
	isLoading: boolean;
}

function ApiKeysSection({settings, isLoading}: GlobalSettingsSectionProps) {
	const queryClient = useQueryClient();
	const [editingBraveKey, setEditingBraveKey] = useState(false);
	const [braveKeyInput, setBraveKeyInput] = useState("");
	const [message, setMessage] = useState<{ text: string; type: "success" | "error" } | null>(null);

	const updateMutation = useMutation({
		mutationFn: api.updateGlobalSettings,
		onSuccess: (result) => {
			if (result.success) {
				setEditingBraveKey(false);
				setBraveKeyInput("");
				setMessage({text: result.message, type: "success"});
				queryClient.invalidateQueries({queryKey: ["global-settings"]});
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) => {
			setMessage({text: `Failed: ${error.message}`, type: "error"});
		},
	});

	const handleSaveBraveKey = () => {
		updateMutation.mutate({brave_search_key: braveKeyInput.trim() || null});
	};

	const handleRemoveBraveKey = () => {
		updateMutation.mutate({brave_search_key: null});
	};

	return (
		<div className="mx-auto max-w-2xl px-6 py-6">
			<div className="mb-6">
				<h2 className="font-plex text-sm font-semibold text-ink">Third-Party API Keys</h2>
				<p className="mt-1 text-sm text-ink-dull">
					Configure API keys for third-party services used by workers.
				</p>
			</div>

			{isLoading ? (
				<div className="flex items-center gap-2 text-ink-dull">
					<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
					Loading settings...
				</div>
			) : (
				<div className="flex flex-col gap-3">
					<div className="rounded-lg border border-app-line bg-app-box p-4">
						<div className="flex items-center gap-3">
							<div className="flex h-8 w-8 items-center justify-center rounded-lg bg-app-darkBox text-ink-faint">
								<span className="text-lg">🔍</span>
							</div>
							<div className="flex-1">
								<div className="flex items-center gap-2">
									<span className="text-sm font-medium text-ink">Brave Search</span>
									{settings?.brave_search_key && (
										<span className="text-tiny text-green-400">● Configured</span>
									)}
								</div>
								<p className="mt-0.5 text-sm text-ink-dull">
									Powers web search capabilities for workers
								</p>
							</div>
							<div className="flex gap-2">
								<Button
									onClick={() => {
										setEditingBraveKey(true);
										setBraveKeyInput(settings?.brave_search_key || "");
										setMessage(null);
									}}
									variant="outline"
									size="sm"
								>
									{settings?.brave_search_key ? "Update" : "Add key"}
								</Button>
								{settings?.brave_search_key && (
									<Button
										onClick={handleRemoveBraveKey}
										variant="outline"
										size="sm"
										loading={updateMutation.isPending}
									>
										Remove
									</Button>
								)}
							</div>
						</div>
					</div>
				</div>
			)}

			{message && (
				<div
					className={`mt-4 rounded-md border px-3 py-2 text-sm ${
						message.type === "success"
							? "border-green-500/20 bg-green-500/10 text-green-400"
							: "border-red-500/20 bg-red-500/10 text-red-400"
					}`}
				>
					{message.text}
				</div>
			)}

			<Dialog open={editingBraveKey} onOpenChange={(open) => { if (!open) setEditingBraveKey(false); }}>
				<DialogContent className="max-w-md">
					<DialogHeader>
						<DialogTitle>{settings?.brave_search_key ? "Update" : "Add"} Brave Search Key</DialogTitle>
						<DialogDescription>
							Enter your Brave Search API key. Get one at brave.com/search/api
						</DialogDescription>
					</DialogHeader>
					<Input
						type="password"
						value={braveKeyInput}
						onChange={(e) => setBraveKeyInput(e.target.value)}
						placeholder="BSA..."
						autoFocus
						onKeyDown={(e) => {
							if (e.key === "Enter") handleSaveBraveKey();
						}}
					/>
					<DialogFooter>
						<Button onClick={() => setEditingBraveKey(false)} variant="ghost" size="sm">
							Cancel
						</Button>
						<Button
							onClick={handleSaveBraveKey}
							disabled={!braveKeyInput.trim()}
							loading={updateMutation.isPending}
							size="sm"
						>
							Save
						</Button>
					</DialogFooter>
				</DialogContent>
			</Dialog>
		</div>
	);
}

function ServerSection({settings, isLoading}: GlobalSettingsSectionProps) {
	const queryClient = useQueryClient();
	const [apiEnabled, setApiEnabled] = useState(settings?.api_enabled ?? true);
	const [apiPort, setApiPort] = useState(settings?.api_port.toString() ?? "19898");
	const [apiBind, setApiBind] = useState(settings?.api_bind ?? "127.0.0.1");
	const [message, setMessage] = useState<{ text: string; type: "success" | "error"; requiresRestart?: boolean } | null>(null);

	// Update form state when settings load
	useEffect(() => {
		if (settings) {
			setApiEnabled(settings.api_enabled);
			setApiPort(settings.api_port.toString());
			setApiBind(settings.api_bind);
		}
	}, [settings]);

	const updateMutation = useMutation({
		mutationFn: api.updateGlobalSettings,
		onSuccess: (result) => {
			if (result.success) {
				setMessage({text: result.message, type: "success", requiresRestart: result.requires_restart});
				queryClient.invalidateQueries({queryKey: ["global-settings"]});
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) => {
			setMessage({text: `Failed: ${error.message}`, type: "error"});
		},
	});

	const handleSave = () => {
		const port = parseInt(apiPort, 10);
		if (isNaN(port) || port < 1024 || port > 65535) {
			setMessage({text: "Port must be between 1024 and 65535", type: "error"});
			return;
		}

		updateMutation.mutate({
			api_enabled: apiEnabled,
			api_port: port,
			api_bind: apiBind.trim(),
		});
	};

	return (
		<div className="mx-auto max-w-2xl px-6 py-6">
			<div className="mb-6">
				<h2 className="font-plex text-sm font-semibold text-ink">API Server Configuration</h2>
				<p className="mt-1 text-sm text-ink-dull">
					Configure the HTTP API server. Changes require a restart to take effect.
				</p>
			</div>

			{isLoading ? (
				<div className="flex items-center gap-2 text-ink-dull">
					<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
					Loading settings...
				</div>
			) : (
				<div className="flex flex-col gap-4">
					<div className="rounded-lg border border-app-line bg-app-box p-4">
						<label className="flex items-center gap-3">
							<input
								type="checkbox"
								checked={apiEnabled}
								onChange={(e) => setApiEnabled(e.target.checked)}
								className="h-4 w-4"
							/>
							<div>
								<span className="text-sm font-medium text-ink">Enable API Server</span>
								<p className="mt-0.5 text-sm text-ink-dull">
									Disable to prevent the HTTP API from starting
								</p>
							</div>
						</label>
					</div>

					<div className="rounded-lg border border-app-line bg-app-box p-4">
						<label className="block">
							<span className="text-sm font-medium text-ink">Port</span>
							<p className="mt-0.5 text-sm text-ink-dull">Port number for the API server</p>
							<Input
								type="number"
								value={apiPort}
								onChange={(e) => setApiPort(e.target.value)}
								min="1024"
								max="65535"
								className="mt-2"
							/>
						</label>
					</div>

					<div className="rounded-lg border border-app-line bg-app-box p-4">
						<label className="block">
							<span className="text-sm font-medium text-ink">Bind Address</span>
							<p className="mt-0.5 text-sm text-ink-dull">
								IP address to bind to (127.0.0.1 for local, 0.0.0.0 for all interfaces)
							</p>
							<Input
								type="text"
								value={apiBind}
								onChange={(e) => setApiBind(e.target.value)}
								placeholder="127.0.0.1"
								className="mt-2"
							/>
						</label>
					</div>

					<Button onClick={handleSave} loading={updateMutation.isPending}>
						Save Changes
					</Button>
				</div>
			)}

			{message && (
				<div
					className={`mt-4 rounded-md border px-3 py-2 text-sm ${
						message.type === "success"
							? "border-green-500/20 bg-green-500/10 text-green-400"
							: "border-red-500/20 bg-red-500/10 text-red-400"
					}`}
				>
					{message.text}
					{message.requiresRestart && (
						<div className="mt-1 text-yellow-400">
							⚠️ Restart required for changes to take effect
						</div>
					)}
				</div>
			)}
		</div>
	);
}

function WorkerLogsSection({settings, isLoading}: GlobalSettingsSectionProps) {
	const queryClient = useQueryClient();
	const [logMode, setLogMode] = useState(settings?.worker_log_mode ?? "errors_only");
	const [message, setMessage] = useState<{ text: string; type: "success" | "error" } | null>(null);

	// Update form state when settings load
	useEffect(() => {
		if (settings) {
			setLogMode(settings.worker_log_mode);
		}
	}, [settings]);

	const updateMutation = useMutation({
		mutationFn: api.updateGlobalSettings,
		onSuccess: (result) => {
			if (result.success) {
				setMessage({text: result.message, type: "success"});
				queryClient.invalidateQueries({queryKey: ["global-settings"]});
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) => {
			setMessage({text: `Failed: ${error.message}`, type: "error"});
		},
	});

	const handleSave = () => {
		updateMutation.mutate({worker_log_mode: logMode});
	};

	const modes = [
		{
			value: "errors_only",
			label: "Errors Only",
			description: "Only log failed worker runs (saves disk space)",
		},
		{
			value: "all_separate",
			label: "All (Separate)",
			description: "Log all runs with separate directories for success/failure",
		},
		{
			value: "all_combined",
			label: "All (Combined)",
			description: "Log all runs to the same directory",
		},
	];

	return (
		<div className="mx-auto max-w-2xl px-6 py-6">
			<div className="mb-6">
				<h2 className="font-plex text-sm font-semibold text-ink">Worker Execution Logs</h2>
				<p className="mt-1 text-sm text-ink-dull">
					Control how worker execution logs are stored in the logs directory.
				</p>
			</div>

			{isLoading ? (
				<div className="flex items-center gap-2 text-ink-dull">
					<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
					Loading settings...
				</div>
			) : (
				<div className="flex flex-col gap-4">
					<div className="flex flex-col gap-3">
						{modes.map((mode) => (
							<div
								key={mode.value}
								className={`rounded-lg border p-4 cursor-pointer transition-colors ${
									logMode === mode.value
										? "border-accent bg-accent/5"
										: "border-app-line bg-app-box hover:border-app-line/80"
								}`}
								onClick={() => setLogMode(mode.value)}
							>
								<label className="flex items-start gap-3 cursor-pointer">
									<input
										type="radio"
										value={mode.value}
										checked={logMode === mode.value}
										onChange={(e) => setLogMode(e.target.value)}
										className="mt-0.5"
									/>
									<div className="flex-1">
										<span className="text-sm font-medium text-ink">{mode.label}</span>
										<p className="mt-0.5 text-sm text-ink-dull">{mode.description}</p>
									</div>
								</label>
							</div>
						))}
					</div>

					<Button onClick={handleSave} loading={updateMutation.isPending}>
						Save Changes
					</Button>
				</div>
			)}

			{message && (
				<div
					className={`mt-4 rounded-md border px-3 py-2 text-sm ${
						message.type === "success"
							? "border-green-500/20 bg-green-500/10 text-green-400"
							: "border-red-500/20 bg-red-500/10 text-red-400"
					}`}
				>
					{message.text}
				</div>
			)}
		</div>
	);
}

const PERMISSION_OPTIONS = [
	{value: "allow", label: "Allow", description: "Tool can run without restriction"},
	{value: "deny", label: "Deny", description: "Tool is completely disabled"},
];

function OpenCodeSection({settings, isLoading}: GlobalSettingsSectionProps) {
	const queryClient = useQueryClient();
	const [enabled, setEnabled] = useState(settings?.opencode?.enabled ?? false);
	const [path, setPath] = useState(settings?.opencode?.path ?? "opencode");
	const [maxServers, setMaxServers] = useState(settings?.opencode?.max_servers?.toString() ?? "5");
	const [startupTimeout, setStartupTimeout] = useState(settings?.opencode?.server_startup_timeout_secs?.toString() ?? "30");
	const [maxRetries, setMaxRetries] = useState(settings?.opencode?.max_restart_retries?.toString() ?? "5");
	const [editPerm, setEditPerm] = useState(settings?.opencode?.permissions?.edit ?? "allow");
	const [bashPerm, setBashPerm] = useState(settings?.opencode?.permissions?.bash ?? "allow");
	const [webfetchPerm, setWebfetchPerm] = useState(settings?.opencode?.permissions?.webfetch ?? "allow");
	const [message, setMessage] = useState<{ text: string; type: "success" | "error" } | null>(null);

	useEffect(() => {
		if (settings?.opencode) {
			setEnabled(settings.opencode.enabled);
			setPath(settings.opencode.path);
			setMaxServers(settings.opencode.max_servers.toString());
			setStartupTimeout(settings.opencode.server_startup_timeout_secs.toString());
			setMaxRetries(settings.opencode.max_restart_retries.toString());
			setEditPerm(settings.opencode.permissions.edit);
			setBashPerm(settings.opencode.permissions.bash);
			setWebfetchPerm(settings.opencode.permissions.webfetch);
		}
	}, [settings?.opencode]);

	const updateMutation = useMutation({
		mutationFn: api.updateGlobalSettings,
		onSuccess: (result) => {
			if (result.success) {
				setMessage({text: result.message, type: "success"});
				queryClient.invalidateQueries({queryKey: ["global-settings"]});
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) => {
			setMessage({text: `Failed: ${error.message}`, type: "error"});
		},
	});

	const handleSave = () => {
		const servers = parseInt(maxServers, 10);
		if (isNaN(servers) || servers < 1) {
			setMessage({text: "Max servers must be at least 1", type: "error"});
			return;
		}
		const timeout = parseInt(startupTimeout, 10);
		if (isNaN(timeout) || timeout < 1) {
			setMessage({text: "Startup timeout must be at least 1", type: "error"});
			return;
		}
		const retries = parseInt(maxRetries, 10);
		if (isNaN(retries) || retries < 0) {
			setMessage({text: "Max retries cannot be negative", type: "error"});
			return;
		}

		updateMutation.mutate({
			opencode: {
				enabled,
				path: path.trim() || "opencode",
				max_servers: servers,
				server_startup_timeout_secs: timeout,
				max_restart_retries: retries,
				permissions: {
					edit: editPerm,
					bash: bashPerm,
					webfetch: webfetchPerm,
				},
			},
		});
	};

	return (
		<div className="mx-auto max-w-2xl px-6 py-6">
			<div className="mb-6">
				<h2 className="font-plex text-sm font-semibold text-ink">OpenCode Workers</h2>
				<p className="mt-1 text-sm text-ink-dull">
					Spawn <a href="https://opencode.ai" target="_blank" rel="noopener noreferrer" className="text-accent hover:underline">OpenCode</a> coding agents as worker subprocesses. Requires the <code className="rounded bg-app-box px-1 py-0.5 text-tiny text-ink-dull">opencode</code> binary on PATH or a custom path below.
				</p>
			</div>

			{isLoading ? (
				<div className="flex items-center gap-2 text-ink-dull">
					<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
					Loading settings...
				</div>
			) : (
				<div className="flex flex-col gap-4">
					{/* Enable toggle */}
					<div className="rounded-lg border border-app-line bg-app-box p-4">
						<label className="flex items-center gap-3">
							<input
								type="checkbox"
								checked={enabled}
								onChange={(e) => setEnabled(e.target.checked)}
								className="h-4 w-4"
							/>
							<div>
								<span className="text-sm font-medium text-ink">Enable OpenCode Workers</span>
								<p className="mt-0.5 text-sm text-ink-dull">
									Allow agents to spawn OpenCode coding sessions
								</p>
							</div>
						</label>
					</div>

					{enabled && (
						<>
							{/* Binary path */}
							<div className="rounded-lg border border-app-line bg-app-box p-4">
								<label className="block">
									<span className="text-sm font-medium text-ink">Binary Path</span>
									<p className="mt-0.5 text-sm text-ink-dull">
										Path to the OpenCode binary, or just the name if it's on PATH
									</p>
									<Input
										type="text"
										value={path}
										onChange={(e) => setPath(e.target.value)}
										placeholder="opencode"
										className="mt-2"
									/>
								</label>
							</div>

							{/* Pool settings */}
							<div className="rounded-lg border border-app-line bg-app-box p-4">
								<span className="text-sm font-medium text-ink">Server Pool</span>
								<p className="mt-0.5 text-sm text-ink-dull">
									Controls how many OpenCode server processes can run concurrently
								</p>
								<div className="mt-3 grid grid-cols-3 gap-3">
									<label className="block">
										<span className="text-tiny font-medium text-ink-dull">Max Servers</span>
										<Input
											type="number"
											value={maxServers}
											onChange={(e) => setMaxServers(e.target.value)}
											min="1"
											max="20"
											className="mt-1"
										/>
									</label>
									<label className="block">
										<span className="text-tiny font-medium text-ink-dull">Startup Timeout (s)</span>
										<Input
											type="number"
											value={startupTimeout}
											onChange={(e) => setStartupTimeout(e.target.value)}
											min="1"
											className="mt-1"
										/>
									</label>
									<label className="block">
										<span className="text-tiny font-medium text-ink-dull">Max Retries</span>
										<Input
											type="number"
											value={maxRetries}
											onChange={(e) => setMaxRetries(e.target.value)}
											min="0"
											className="mt-1"
										/>
									</label>
								</div>
							</div>

							{/* Permissions */}
							<div className="rounded-lg border border-app-line bg-app-box p-4">
								<span className="text-sm font-medium text-ink">Permissions</span>
								<p className="mt-0.5 text-sm text-ink-dull">
									Control which tools OpenCode workers can use
								</p>
								<div className="mt-3 flex flex-col gap-3">
									{([
										{label: "File Edit", value: editPerm, setter: setEditPerm},
										{label: "Shell / Bash", value: bashPerm, setter: setBashPerm},
										{label: "Web Fetch", value: webfetchPerm, setter: setWebfetchPerm},
									] as const).map(({label, value, setter}) => (
										<div key={label} className="flex items-center justify-between">
											<span className="text-sm text-ink">{label}</span>
											<Select value={value} onValueChange={(v) => setter(v)}>
												<SelectTrigger className="w-28">
													<SelectValue />
												</SelectTrigger>
												<SelectContent>
													{PERMISSION_OPTIONS.map((opt) => (
														<SelectItem key={opt.value} value={opt.value}>
															{opt.label}
														</SelectItem>
													))}
												</SelectContent>
											</Select>
										</div>
									))}
								</div>
							</div>
						</>
					)}

					<Button onClick={handleSave} loading={updateMutation.isPending}>
						Save Changes
					</Button>
				</div>
			)}

			{message && (
				<div
					className={`mt-4 rounded-md border px-3 py-2 text-sm ${
						message.type === "success"
							? "border-green-500/20 bg-green-500/10 text-green-400"
							: "border-red-500/20 bg-red-500/10 text-red-400"
					}`}
				>
					{message.text}
				</div>
			)}
		</div>
	);
}

function ConfigFileSection() {
	const queryClient = useQueryClient();
	const editorRef = useRef<HTMLDivElement>(null);
	const viewRef = useRef<import("@codemirror/view").EditorView | null>(null);
	const [originalContent, setOriginalContent] = useState("");
	const [currentContent, setCurrentContent] = useState("");
	const [validationError, setValidationError] = useState<string | null>(null);
	const [message, setMessage] = useState<{ text: string; type: "success" | "error" } | null>(null);
	const [editorLoaded, setEditorLoaded] = useState(false);

	const {data, isLoading} = useQuery({
		queryKey: ["raw-config"],
		queryFn: api.rawConfig,
		staleTime: 5_000,
	});

	const updateMutation = useMutation({
		mutationFn: (content: string) => api.updateRawConfig(content),
		onSuccess: (result) => {
			if (result.success) {
				setOriginalContent(currentContent);
				setMessage({text: result.message, type: "success"});
				setValidationError(null);
				// Invalidate all config-related queries so other tabs pick up changes
				queryClient.invalidateQueries({queryKey: ["providers"]});
				queryClient.invalidateQueries({queryKey: ["global-settings"]});
				queryClient.invalidateQueries({queryKey: ["agents"]});
				queryClient.invalidateQueries({queryKey: ["overview"]});
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) => {
			setMessage({text: `Failed: ${error.message}`, type: "error"});
		},
	});

	const isDirty = currentContent !== originalContent;

	// Initialize CodeMirror when data loads
	useEffect(() => {
		if (!data?.content || !editorRef.current || editorLoaded) return;

		const content = data.content;
		setOriginalContent(content);
		setCurrentContent(content);

		// Lazy-load CodeMirror to avoid SSR issues and keep initial bundle small
		Promise.all([
			import("@codemirror/view"),
			import("@codemirror/state"),
			import("codemirror"),
			import("@codemirror/theme-one-dark"),
			import("@codemirror/language"),
			import("@codemirror/legacy-modes/mode/toml"),
		]).then(([viewMod, stateMod, cm, themeMod, langMod, tomlMode]) => {
			if (!editorRef.current) return;

			const tomlLang = langMod.StreamLanguage.define(tomlMode.toml);

			const updateListener = viewMod.EditorView.updateListener.of((update) => {
				if (update.docChanged) {
					const newContent = update.state.doc.toString();
					setCurrentContent(newContent);
					try {
						parseToml(newContent);
						setValidationError(null);
					} catch (error: any) {
						setValidationError(error.message || "Invalid TOML");
					}
				}
			});

			const theme = viewMod.EditorView.theme({
				"&": {
					height: "100%",
					fontSize: "13px",
				},
				".cm-scroller": {
					fontFamily: "'IBM Plex Mono', monospace",
					overflow: "auto",
				},
				".cm-gutters": {
					backgroundColor: "transparent",
					borderRight: "1px solid hsl(var(--color-app-line) / 0.3)",
				},
				".cm-activeLineGutter": {
					backgroundColor: "transparent",
				},
			});

			const state = stateMod.EditorState.create({
				doc: content,
				extensions: [
					cm.basicSetup,
					tomlLang,
					themeMod.oneDark,
					theme,
					updateListener,
					viewMod.keymap.of([{
						key: "Mod-s",
						run: () => {
							// Trigger save via DOM event since we can't access React state here
							editorRef.current?.dispatchEvent(new CustomEvent("cm-save"));
							return true;
						},
					}]),
				],
			});

			const view = new viewMod.EditorView({
				state,
				parent: editorRef.current,
			});

			viewRef.current = view;
			setEditorLoaded(true);
		});

		return () => {
			viewRef.current?.destroy();
			viewRef.current = null;
		};
	}, [data?.content]);

	// Handle Cmd+S from CodeMirror
	useEffect(() => {
		const element = editorRef.current;
		if (!element) return;

		const handler = () => {
			if (isDirty && !validationError) {
				updateMutation.mutate(currentContent);
			}
		};

		element.addEventListener("cm-save", handler);
		return () => element.removeEventListener("cm-save", handler);
	}, [isDirty, validationError, currentContent]);

	const handleSave = () => {
		if (!isDirty || validationError) return;
		setMessage(null);
		updateMutation.mutate(currentContent);
	};

	const handleRevert = () => {
		if (!viewRef.current) return;
		const view = viewRef.current;
		view.dispatch({
			changes: {from: 0, to: view.state.doc.length, insert: originalContent},
		});
		setCurrentContent(originalContent);
		setValidationError(null);
		setMessage(null);
	};

	return (
		<div className="flex h-full flex-col">
			{/* Description + actions */}
			<div className="flex items-center justify-between px-6 py-4 border-b border-app-line/30">
				<p className="text-sm text-ink-dull">
					Edit the raw configuration file. Changes are validated as TOML before saving.
				</p>
				<div className="flex items-center gap-2 flex-shrink-0 ml-4">
					{isDirty && (
						<Button onClick={handleRevert} variant="ghost" size="sm">
							Revert
						</Button>
					)}
					<Button
						onClick={handleSave}
						disabled={!isDirty || !!validationError}
						loading={updateMutation.isPending}
						size="sm"
					>
						Save
					</Button>
				</div>
			</div>

			{/* Validation / status bar */}
			{(validationError || message) && (
				<div className={`border-b px-6 py-2 text-sm ${
					validationError
						? "border-red-500/20 bg-red-500/5 text-red-400"
						: message?.type === "success"
							? "border-green-500/20 bg-green-500/5 text-green-400"
							: "border-red-500/20 bg-red-500/5 text-red-400"
				}`}>
					{validationError ? `Syntax error: ${validationError}` : message?.text}
				</div>
			)}

			{/* Editor */}
			<div className="flex-1 overflow-hidden">
				{isLoading ? (
					<div className="flex items-center gap-2 p-6 text-ink-dull">
						<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
						Loading config...
					</div>
				) : (
					<div ref={editorRef} className="h-full" />
				)}
			</div>
		</div>
	);
}

interface ProviderCardProps {
	provider: string;
	name: string;
	description: string;
	configured: boolean;
	onEdit: () => void;
	onRemove: () => void;
	removing: boolean;
}

function ProviderCard({ provider, name, description, configured, onEdit, onRemove, removing }: ProviderCardProps) {
	return (
		<div className="rounded-lg border border-app-line bg-app-box p-4">
			<div className="flex items-center gap-3">
				<ProviderIcon provider={provider} size={32} />
				<div className="flex-1">
					<div className="flex items-center gap-2">
						<span className="text-sm font-medium text-ink">{name}</span>
						{configured && (
							<span className="text-tiny text-green-400">
								● Configured
							</span>
						)}
					</div>
					<p className="mt-0.5 text-sm text-ink-dull">{description}</p>
				</div>
				<div className="flex gap-2">
					<Button onClick={onEdit} variant="outline" size="sm">
						{configured ? "Update" : "Add key"}
					</Button>
					{configured && (
						<Button onClick={onRemove} variant="outline" size="sm" loading={removing}>
							Remove
						</Button>
					)}
				</div>
			</div>
		</div>
	);
}
