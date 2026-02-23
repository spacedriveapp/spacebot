import { useState, useEffect, useRef } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { api, type GlobalSettingsResponse } from "@/api/client";
import { Button, Input, SettingSidebarButton, Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter, Select, SelectTrigger, SelectValue, SelectContent, SelectItem, Toggle } from "@/ui";
import { useSearch, useNavigate } from "@tanstack/react-router";
import { ChannelSettingCard, DisabledChannelCard } from "@/components/ChannelSettingCard";
import { ModelSelect } from "@/components/ModelSelect";
import { ProviderIcon } from "@/lib/providerIcons";
import { FontAwesomeIcon } from "@fortawesome/react-fontawesome";
import { faSearch } from "@fortawesome/free-solid-svg-icons";

import { parse as parseToml } from "smol-toml";

type SectionId = "providers" | "channels" | "api-keys" | "server" | "opencode" | "worker-logs" | "config-file";

const SECTIONS = [
	{
		id: "providers" as const,
		label: "Providers",
		group: "general" as const,
		description: "LLM provider credentials",
	},
	{
		id: "channels" as const,
		label: "Channels",
		group: "messaging" as const,
		description: "Messaging platforms and bindings",
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
		defaultModel: "openrouter/anthropic/claude-sonnet-4",
	},
	{
		id: "opencode-zen",
		name: "OpenCode Zen",
		description: "Multi-format gateway (Kimi, GLM, MiniMax, Qwen)",
		placeholder: "...",
		envVar: "OPENCODE_ZEN_API_KEY",
		defaultModel: "opencode-zen/kimi-k2.5",
	},
	{
		id: "anthropic",
		name: "Anthropic",
		description: "Claude models (Sonnet, Opus, Haiku)",
		placeholder: "sk-ant-...",
		envVar: "ANTHROPIC_API_KEY",
		defaultModel: "anthropic/claude-sonnet-4",
	},
	{
		id: "openai",
		name: "OpenAI",
		description: "GPT models",
		placeholder: "sk-...",
		envVar: "OPENAI_API_KEY",
		defaultModel: "openai/gpt-4.1",
	},
	{
		id: "zai-coding-plan",
		name: "Z.AI Coding Plan",
		description: "GLM coding models (glm-4.7, glm-5, glm-4.5-air)",
		placeholder: "...",
		envVar: "ZAI_CODING_PLAN_API_KEY",
		defaultModel: "glm-5",
	},
	{
		id: "zhipu",
		name: "Z.ai (GLM)",
		description: "GLM models (GLM-4, GLM-4-Flash)",
		placeholder: "...",
		envVar: "ZHIPU_API_KEY",
		defaultModel: "zhipu/glm-4-plus",
	},
	{
		id: "groq",
		name: "Groq",
		description: "Fast inference for Llama, Mixtral models",
		placeholder: "gsk_...",
		envVar: "GROQ_API_KEY",
		defaultModel: "groq/llama-3.3-70b-versatile",
	},
	{
		id: "together",
		name: "Together AI",
		description: "Wide model selection with competitive pricing",
		placeholder: "...",
		envVar: "TOGETHER_API_KEY",
		defaultModel: "together/meta-llama/Meta-Llama-3.1-405B-Instruct-Turbo",
	},
	{
		id: "fireworks",
		name: "Fireworks AI",
		description: "Fast inference for popular OSS models",
		placeholder: "...",
		envVar: "FIREWORKS_API_KEY",
		defaultModel: "fireworks/accounts/fireworks/models/llama-v3p3-70b-instruct",
	},
	{
		id: "deepseek",
		name: "DeepSeek",
		description: "DeepSeek Chat and Reasoner models",
		placeholder: "sk-...",
		envVar: "DEEPSEEK_API_KEY",
		defaultModel: "deepseek/deepseek-chat",
	},
	{
		id: "xai",
		name: "xAI",
		description: "Grok models",
		placeholder: "xai-...",
		envVar: "XAI_API_KEY",
		defaultModel: "xai/grok-2-latest",
	},
	{
		id: "mistral",
		name: "Mistral AI",
		description: "Mistral Large, Small, Codestral models",
		placeholder: "...",
		envVar: "MISTRAL_API_KEY",
		defaultModel: "mistral/mistral-large-latest",
	},
	{
		id: "gemini",
		name: "Google Gemini",
		description: "Google Gemini experimental and production models",
		placeholder: "AIza...",
		envVar: "GEMINI_API_KEY",
		defaultModel: "gemini/gemini-2.5-flash",
	},
	{
		id: "nvidia",
		name: "NVIDIA NIM",
		description: "NVIDIA-hosted models via NIM API",
		placeholder: "nvapi-...",
		envVar: "NVIDIA_API_KEY",
		defaultModel: "nvidia/meta/llama-3.1-405b-instruct",
	},
	{
		id: "minimax",
		name: "MiniMax",
		description: "MiniMax M1 (Anthropic message format)",
		placeholder: "eyJ...",
		envVar: "MINIMAX_API_KEY",
		defaultModel: "minimax/MiniMax-M1-80k",
	},
	{
		id: "minimax-cn",
		name: "MiniMax CN",
		description: "MiniMax M2.5 (Anthropic message format)",
		placeholder: "eyJ...",
		envVar: "MINIMAX_CN_API_KEY",
		defaultModel: "minimax-cn/MiniMax-M2.5",
	},
	{
		id: "moonshot",
		name: "Moonshot AI",
		description: "Kimi models (Kimi K2, Kimi K2.5)",
		placeholder: "sk-...",
		envVar: "MOONSHOT_API_KEY",
		defaultModel: "moonshot/kimi-k2.5",
	},
	{
		id: "ollama",
		name: "Ollama",
		description: "Local or remote Ollama API endpoint",
		placeholder: "http://localhost:11434",
		envVar: "OLLAMA_BASE_URL",
		defaultModel: "ollama/llama3.2",
	},
] as const;

export function Settings() {
	const queryClient = useQueryClient();
	const navigate = useNavigate();
	const search = useSearch({ from: "/settings" }) as { tab?: string };
	const [activeSection, setActiveSection] = useState<SectionId>("providers");

	// Sync activeSection with URL search param
	useEffect(() => {
		if (search.tab && SECTIONS.some(s => s.id === search.tab)) {
			setActiveSection(search.tab as SectionId);
		}
	}, [search.tab]);

	const handleSectionChange = (section: SectionId) => {
		setActiveSection(section);
		navigate({ to: "/settings", search: { tab: section } });
	};
	const [editingProvider, setEditingProvider] = useState<string | null>(null);
	const [keyInput, setKeyInput] = useState("");
	const [modelInput, setModelInput] = useState("");
	const [testedSignature, setTestedSignature] = useState<string | null>(null);
	const [testResult, setTestResult] = useState<{
		success: boolean;
		message: string;
		sample?: string | null;
	} | null>(null);
	const [message, setMessage] = useState<{
		text: string;
		type: "success" | "error";
	} | null>(null);

	// Fetch providers data (only when on providers tab)
	const { data, isLoading } = useQuery({
		queryKey: ["providers"],
		queryFn: api.providers,
		staleTime: 5_000,
		enabled: activeSection === "providers",
	});

	// Fetch global settings (only when on api-keys, server, or worker-logs tabs)
	const { data: globalSettings, isLoading: globalSettingsLoading } = useQuery({
		queryKey: ["global-settings"],
		queryFn: api.globalSettings,
		staleTime: 5_000,
		enabled: activeSection === "api-keys" || activeSection === "server" || activeSection === "opencode" || activeSection === "worker-logs",
	});

	const updateMutation = useMutation({
		mutationFn: ({ provider, apiKey, model }: { provider: string; apiKey: string; model: string }) =>
			api.updateProvider(provider, apiKey, model),
		onSuccess: (result) => {
			if (result.success) {
				setEditingProvider(null);
				setKeyInput("");
				setModelInput("");
				setTestedSignature(null);
				setTestResult(null);
				setMessage({ text: result.message, type: "success" });
				queryClient.invalidateQueries({ queryKey: ["providers"] });
				// Agents will auto-start on the backend, refetch agent list after a short delay
				setTimeout(() => {
					queryClient.invalidateQueries({ queryKey: ["agents"] });
					queryClient.invalidateQueries({ queryKey: ["overview"] });
				}, 3000);
			} else {
				setMessage({ text: result.message, type: "error" });
			}
		},
		onError: (error) => {
			setMessage({ text: `Failed: ${error.message}`, type: "error" });
		},
	});

	const testModelMutation = useMutation({
		mutationFn: ({ provider, apiKey, model }: { provider: string; apiKey: string; model: string }) =>
			api.testProviderModel(provider, apiKey, model),
	});

	const removeMutation = useMutation({
		mutationFn: (provider: string) => api.removeProvider(provider),
		onSuccess: (result) => {
			if (result.success) {
				setMessage({ text: result.message, type: "success" });
				queryClient.invalidateQueries({ queryKey: ["providers"] });
			} else {
				setMessage({ text: result.message, type: "error" });
			}
		},
		onError: (error) => {
			setMessage({ text: `Failed: ${error.message}`, type: "error" });
		},
	});

	const editingProviderData = PROVIDERS.find((p) => p.id === editingProvider);

	const currentSignature = `${editingProvider ?? ""}|${keyInput.trim()}|${modelInput.trim()}`;

	const handleTestModel = async (): Promise<boolean> => {
		if (!editingProvider || !keyInput.trim() || !modelInput.trim()) return false;
		setMessage(null);
		setTestResult(null);
		try {
			const result = await testModelMutation.mutateAsync({
				provider: editingProvider,
				apiKey: keyInput.trim(),
				model: modelInput.trim(),
			});
			setTestResult({ success: result.success, message: result.message, sample: result.sample });
			if (result.success) {
				setTestedSignature(currentSignature);
				return true;
			} else {
				setTestedSignature(null);
				return false;
			}
		} catch (error: any) {
			setTestResult({ success: false, message: `Failed: ${error.message}` });
			setTestedSignature(null);
			return false;
		}
	};

	const handleSave = async () => {
		if (!keyInput.trim() || !editingProvider || !modelInput.trim()) return;

		if (testedSignature !== currentSignature) {
			const testPassed = await handleTestModel();
			if (!testPassed) return;
		}

		updateMutation.mutate({
			provider: editingProvider,
			apiKey: keyInput.trim(),
			model: modelInput.trim(),
		});
	};

	const handleClose = () => {
		setEditingProvider(null);
		setKeyInput("");
		setModelInput("");
		setTestedSignature(null);
		setTestResult(null);
	};

	const isConfigured = (providerId: string): boolean => {
		if (!data) return false;
		const statusKey = providerId.replace(/-/g, "_") as keyof typeof data.providers;
		return data.providers[statusKey] ?? false;
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
									Configure credentials/endpoints for LLM providers. At least one provider is
									required for agents to function.
								</p>
							</div>

							<div className="mb-4 rounded-md border border-app-line bg-app-darkBox/20 px-4 py-3">
								<p className="text-sm text-ink-faint">
									When you add a provider, choose a model and run a completion test before saving.
									Saving applies that model to all five default routing roles and to your default agent.
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
											defaultModel={provider.defaultModel}
											onEdit={() => {
												setEditingProvider(provider.id);
												setKeyInput("");
												setModelInput(provider.defaultModel ?? "");
												setTestedSignature(null);
												setTestResult(null);
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
									Provider values are written to{" "}
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
						<DialogTitle>
							{isConfigured(editingProvider ?? "") ? "Update" : "Add"}{" "}
							{editingProvider === "ollama" ? "Endpoint" : "API Key"}
						</DialogTitle>
						<DialogDescription>
							{editingProvider === "ollama"
								? `Enter your ${editingProviderData?.name} base URL. It will be saved to your instance config.`
								: `Enter your ${editingProviderData?.name} API key. It will be saved to your instance config.`}
						</DialogDescription>
					</DialogHeader>
					<Input
						type={editingProvider === "ollama" ? "text" : "password"}
						value={keyInput}
						onChange={(e) => {
							setKeyInput(e.target.value);
							setTestedSignature(null);
						}}
						placeholder={editingProviderData?.placeholder}
						autoFocus
						onKeyDown={(e) => {
							if (e.key === "Enter") handleSave();
						}}
					/>
					<ModelSelect
						label="Model"
						description="Pick the exact model ID to verify and apply to routing"
						value={modelInput}
						onChange={(value) => {
							setModelInput(value);
							setTestedSignature(null);
						}}
						provider={editingProvider ?? undefined}
					/>
					<div className="flex items-center gap-2">
						<Button
							onClick={handleTestModel}
							disabled={!editingProvider || !keyInput.trim() || !modelInput.trim()}
							loading={testModelMutation.isPending}
							variant="outline"
							size="sm"
						>
							Test model
						</Button>
						{testedSignature === currentSignature && testResult?.success && (
							<span className="text-xs text-green-400">Verified</span>
						)}
					</div>
					{testResult && (
						<div
							className={`rounded-md border px-3 py-2 text-sm ${testResult.success
									? "border-green-500/20 bg-green-500/10 text-green-400"
									: "border-red-500/20 bg-red-500/10 text-red-400"
								}`}
						>
							<div>{testResult.message}</div>
							{testResult.success && testResult.sample ? (
								<div className="mt-1 text-xs text-ink-dull">Sample: {testResult.sample}</div>
							) : null}
						</div>
					)}
					{message && (
						<div
							className={`rounded-md border px-3 py-2 text-sm ${message.type === "success"
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
							disabled={!keyInput.trim() || !modelInput.trim()}
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
	const [expandedPlatform, setExpandedPlatform] = useState<string | null>(null);

	const { data: messagingStatus, isLoading } = useQuery({
		queryKey: ["messaging-status"],
		queryFn: api.messagingStatus,
		staleTime: 5_000,
	});

	const PLATFORMS = [
		{ platform: "discord" as const, name: "Discord", description: "Discord bot integration" },
		{ platform: "slack" as const, name: "Slack", description: "Slack bot integration" },
		{ platform: "telegram" as const, name: "Telegram", description: "Telegram bot integration" },
		{ platform: "twitch" as const, name: "Twitch", description: "Twitch chat integration" },
		{ platform: "webhook" as const, name: "Webhook", description: "HTTP webhook receiver" },
	] as const;

	const COMING_SOON = [
		{ platform: "email", name: "Email", description: "IMAP polling for inbound, SMTP for outbound" },
		{ platform: "whatsapp", name: "WhatsApp", description: "Meta Cloud API integration" },
		{ platform: "matrix", name: "Matrix", description: "Decentralized chat protocol" },
		{ platform: "imessage", name: "iMessage", description: "macOS-only AppleScript bridge" },
		{ platform: "irc", name: "IRC", description: "TLS socket connection" },
		{ platform: "lark", name: "Lark", description: "Feishu/Lark webhook integration" },
		{ platform: "dingtalk", name: "DingTalk", description: "Chinese enterprise webhook integration" },
	];

	return (
		<div className="mx-auto max-w-2xl px-6 py-6">
			<div className="mb-6">
				<h2 className="font-plex text-sm font-semibold text-ink">Messaging Platforms</h2>
				<p className="mt-1 text-sm text-ink-dull">
					Connect messaging platforms and configure how conversations route to agents.
				</p>
			</div>

			{isLoading ? (
				<div className="flex items-center gap-2 text-ink-dull">
					<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
					Loading channels...
				</div>
			) : (
				<div className="flex flex-col gap-3">
					{PLATFORMS.map(({ platform: p, name: n, description: d }) => (
						<ChannelSettingCard
							key={p}
							platform={p}
							name={n}
							description={d}
							status={messagingStatus?.[p]}
							expanded={expandedPlatform === p}
							onToggle={() => setExpandedPlatform(expandedPlatform === p ? null : p)}
						/>
					))}
					{COMING_SOON.map(({ platform: p, name: n, description: d }) => (
						<DisabledChannelCard key={p} platform={p} name={n} description={d} />
					))}
				</div>
			)}
		</div>
	);
}



interface GlobalSettingsSectionProps {
	settings: GlobalSettingsResponse | undefined;
	isLoading: boolean;
}

function ApiKeysSection({ settings, isLoading }: GlobalSettingsSectionProps) {
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
				setMessage({ text: result.message, type: "success" });
				queryClient.invalidateQueries({ queryKey: ["global-settings"] });
			} else {
				setMessage({ text: result.message, type: "error" });
			}
		},
		onError: (error) => {
			setMessage({ text: `Failed: ${error.message}`, type: "error" });
		},
	});

	const handleSaveBraveKey = () => {
		updateMutation.mutate({ brave_search_key: braveKeyInput.trim() || null });
	};

	const handleRemoveBraveKey = () => {
		updateMutation.mutate({ brave_search_key: null });
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
							<FontAwesomeIcon icon={faSearch} className="text-ink-faint" />
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
					className={`mt-4 rounded-md border px-3 py-2 text-sm ${message.type === "success"
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

function ServerSection({ settings, isLoading }: GlobalSettingsSectionProps) {
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
				setMessage({ text: result.message, type: "success", requiresRestart: result.requires_restart });
				queryClient.invalidateQueries({ queryKey: ["global-settings"] });
			} else {
				setMessage({ text: result.message, type: "error" });
			}
		},
		onError: (error) => {
			setMessage({ text: `Failed: ${error.message}`, type: "error" });
		},
	});

	const handleSave = () => {
		const port = parseInt(apiPort, 10);
		if (isNaN(port) || port < 1024 || port > 65535) {
			setMessage({ text: "Port must be between 1024 and 65535", type: "error" });
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
						<div className="flex items-center justify-between">
							<div>
								<span className="text-sm font-medium text-ink">Enable API Server</span>
								<p className="mt-0.5 text-sm text-ink-dull">
									Disable to prevent the HTTP API from starting
								</p>
							</div>
							<Toggle
								size="sm"
								checked={apiEnabled}
								onCheckedChange={setApiEnabled}
							/>
						</div>
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
					className={`mt-4 rounded-md border px-3 py-2 text-sm ${message.type === "success"
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

function WorkerLogsSection({ settings, isLoading }: GlobalSettingsSectionProps) {
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
				setMessage({ text: result.message, type: "success" });
				queryClient.invalidateQueries({ queryKey: ["global-settings"] });
			} else {
				setMessage({ text: result.message, type: "error" });
			}
		},
		onError: (error) => {
			setMessage({ text: `Failed: ${error.message}`, type: "error" });
		},
	});

	const handleSave = () => {
		updateMutation.mutate({ worker_log_mode: logMode });
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
								className={`rounded-lg border p-4 cursor-pointer transition-colors ${logMode === mode.value
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
					className={`mt-4 rounded-md border px-3 py-2 text-sm ${message.type === "success"
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
	{ value: "allow", label: "Allow", description: "Tool can run without restriction" },
	{ value: "deny", label: "Deny", description: "Tool is completely disabled" },
];

function OpenCodeSection({ settings, isLoading }: GlobalSettingsSectionProps) {
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
				setMessage({ text: result.message, type: "success" });
				queryClient.invalidateQueries({ queryKey: ["global-settings"] });
			} else {
				setMessage({ text: result.message, type: "error" });
			}
		},
		onError: (error) => {
			setMessage({ text: `Failed: ${error.message}`, type: "error" });
		},
	});

	const handleSave = () => {
		const servers = parseInt(maxServers, 10);
		if (isNaN(servers) || servers < 1) {
			setMessage({ text: "Max servers must be at least 1", type: "error" });
			return;
		}
		const timeout = parseInt(startupTimeout, 10);
		if (isNaN(timeout) || timeout < 1) {
			setMessage({ text: "Startup timeout must be at least 1", type: "error" });
			return;
		}
		const retries = parseInt(maxRetries, 10);
		if (isNaN(retries) || retries < 0) {
			setMessage({ text: "Max retries cannot be negative", type: "error" });
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
										{ label: "File Edit", value: editPerm, setter: setEditPerm },
										{ label: "Shell / Bash", value: bashPerm, setter: setBashPerm },
										{ label: "Web Fetch", value: webfetchPerm, setter: setWebfetchPerm },
									] as const).map(({ label, value, setter }) => (
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
					className={`mt-4 rounded-md border px-3 py-2 text-sm ${message.type === "success"
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

	const { data, isLoading } = useQuery({
		queryKey: ["raw-config"],
		queryFn: api.rawConfig,
		staleTime: 5_000,
	});

	const updateMutation = useMutation({
		mutationFn: (content: string) => api.updateRawConfig(content),
		onSuccess: (result) => {
			if (result.success) {
				setOriginalContent(currentContent);
				setMessage({ text: result.message, type: "success" });
				setValidationError(null);
				// Invalidate all config-related queries so other tabs pick up changes
				queryClient.invalidateQueries({ queryKey: ["providers"] });
				queryClient.invalidateQueries({ queryKey: ["global-settings"] });
				queryClient.invalidateQueries({ queryKey: ["agents"] });
				queryClient.invalidateQueries({ queryKey: ["overview"] });
			} else {
				setMessage({ text: result.message, type: "error" });
			}
		},
		onError: (error) => {
			setMessage({ text: `Failed: ${error.message}`, type: "error" });
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
			changes: { from: 0, to: view.state.doc.length, insert: originalContent },
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
				<div className={`border-b px-6 py-2 text-sm ${validationError
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
	defaultModel: string;
	onEdit: () => void;
	onRemove: () => void;
	removing: boolean;
}

function ProviderCard({ provider, name, description, configured, defaultModel, onEdit, onRemove, removing }: ProviderCardProps) {
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
					<p className="mt-1 text-tiny text-ink-faint">
						Default model: <span className="text-ink-dull">{defaultModel}</span>
					</p>
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
