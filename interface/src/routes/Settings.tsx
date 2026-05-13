import {useState, useEffect, useRef} from "react";
import {useQuery, useMutation, useQueryClient} from "@tanstack/react-query";
import {api} from "@/api/client";
import {
	Button,
	Input,
	DialogRoot,
	DialogContent,
	DialogHeader,
	DialogTitle,
	DialogDescription,
	DialogFooter,
} from "@spacedrive/primitives";
import {SettingSidebarButton} from "@/ui/SettingSidebarButton";
import {useSearch, useNavigate} from "@tanstack/react-router";
import {useIsMobile} from "@/hooks/useMediaQuery";
import {CaretLeft} from "@phosphor-icons/react";
import {ModelSelect} from "@/components/ModelSelect";
import {
	InstanceSection,
	AppearanceSection,
	ChannelsSection,
	SecretsSection,
	ApiKeysSection,
	ServerSection,
	WorkerLogsSection,
	OpenCodeSection,
	UpdatesSection,
	ChangelogSection,
	ConfigFileSection,
	ProviderCard,
	ChatGptOAuthDialog,
	SECTIONS,
	PROVIDERS,
	CHATGPT_OAUTH_DEFAULT_MODEL,
	type SectionId,
} from "@/components/settings";

export function Settings() {
	const queryClient = useQueryClient();
	const navigate = useNavigate();
	const search = useSearch({from: "/settings"}) as {tab?: string};
	const [activeSection, setActiveSection] = useState<SectionId>("providers");
	const isMobile = useIsMobile();
	// On mobile we start on the list. ?tab=… deep-links straight to detail.
	const [mobileShowDetail, setMobileShowDetail] = useState<boolean>(
		!!search.tab,
	);

	// Sync activeSection with URL search param
	useEffect(() => {
		if (search.tab && SECTIONS.some((s) => s.id === search.tab)) {
			setActiveSection(search.tab as SectionId);
			setMobileShowDetail(true);
		}
	}, [search.tab]);

	const handleSectionChange = (section: SectionId) => {
		setActiveSection(section);
		setMobileShowDetail(true);
		navigate({to: "/settings", search: {tab: section}});
	};

	const showList = !isMobile || !mobileShowDetail;
	const showDetail = !isMobile || mobileShowDetail;
	const [editingProvider, setEditingProvider] = useState<string | null>(null);
	const [keyInput, setKeyInput] = useState("");
	const [modelInput, setModelInput] = useState("");
	const [testedSignature, setTestedSignature] = useState<string | null>(null);
	const [testResult, setTestResult] = useState<{
		success: boolean;
		message: string;
		sample?: string | null;
	} | null>(null);
	const [isPollingOpenAiBrowserOAuth, setIsPollingOpenAiBrowserOAuth] =
		useState(false);
	const [openAiBrowserOAuthMessage, setOpenAiBrowserOAuthMessage] = useState<{
		text: string;
		type: "success" | "error";
	} | null>(null);
	const [openAiOAuthDialogOpen, setOpenAiOAuthDialogOpen] = useState(false);
	const [deviceCodeInfo, setDeviceCodeInfo] = useState<{
		userCode: string;
		verificationUrl: string;
	} | null>(null);
	const [deviceCodeCopied, setDeviceCodeCopied] = useState(false);
	const [message, setMessage] = useState<{
		text: string;
		type: "success" | "error";
	} | null>(null);

	const [azureBaseUrl, setAzureBaseUrl] = useState("");
	const [azureApiVersion, setAzureApiVersion] = useState("");
	const [azureDeployment, setAzureDeployment] = useState("");
	const fetchAbortControllerRef = useRef<AbortController | null>(null);

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
		enabled:
			activeSection === "instance" ||
			activeSection === "api-keys" ||
			activeSection === "server" ||
			activeSection === "opencode" ||
			activeSection === "worker-logs",
	});

	const updateMutation = useMutation({
		mutationFn: ({
			provider,
			apiKey,
			model,
			baseUrl,
			apiVersion,
			deployment,
		}: {
			provider: string;
			apiKey: string;
			model: string;
			baseUrl?: string;
			apiVersion?: string;
			deployment?: string;
		}) =>
			api.updateProvider(
				provider,
				apiKey,
				model,
				baseUrl,
				apiVersion,
				deployment,
			),
		onSuccess: (result) => {
			if (result.success) {
				setEditingProvider(null);
				setKeyInput("");
				setModelInput("");
				setTestedSignature(null);
				setTestResult(null);
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

	const testModelMutation = useMutation({
		mutationFn: ({
			provider,
			apiKey,
			model,
			baseUrl,
			apiVersion,
			deployment,
		}: {
			provider: string;
			apiKey: string;
			model: string;
			baseUrl?: string;
			apiVersion?: string;
			deployment?: string;
		}) =>
			api.testProviderModel(
				provider,
				apiKey,
				model,
				baseUrl,
				apiVersion,
				deployment,
			),
	});
	const startOpenAiBrowserOAuthMutation = useMutation({
		mutationFn: (params: {model: string}) =>
			api.startOpenAiOAuthBrowser(params),
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

	const currentSignature = `${editingProvider ?? ""}|${keyInput.trim()}|${editingProvider === "azure" ? azureDeployment.trim() : modelInput.trim()}`;

	const oauthAutoStartRef = useRef(false);
	const oauthAbortRef = useRef<AbortController | null>(null);

	const handleTestModel = async (): Promise<boolean> => {
		if (!editingProvider || !modelInput.trim()) return false;

		if (editingProvider === "azure") {
			if (!keyInput.trim()) {
				setTestResult({
					success: false,
					message: "API key is required for Azure OpenAI",
				});
				return false;
			}
			if (!azureBaseUrl.trim()) {
				setTestResult({
					success: false,
					message: "Base URL is required for Azure OpenAI",
				});
				return false;
			}
			if (!azureApiVersion.trim()) {
				setTestResult({
					success: false,
					message: "API Version is required for Azure OpenAI",
				});
				return false;
			}
			if (!azureDeployment.trim()) {
				setTestResult({
					success: false,
					message: "Deployment Name is required for Azure OpenAI",
				});
				return false;
			}
			const normalizedBaseUrl = azureBaseUrl.trim().replace(/\/+$/, "");
			if (!normalizedBaseUrl.endsWith(".openai.azure.com")) {
				setTestResult({
					success: false,
					message:
						"Base URL must end with '.openai.azure.com' (e.g., https://{resource-name}.openai.azure.com)",
				});
				return false;
			}
		}

		setMessage(null);
		setTestResult(null);
		try {
			const azureModel =
				editingProvider === "azure"
					? `azure/${azureDeployment.trim()}`
					: modelInput.trim();
			const result = await testModelMutation.mutateAsync({
				provider: editingProvider,
				apiKey: keyInput.trim(),
				model: azureModel,
				baseUrl:
					editingProvider === "azure"
						? azureBaseUrl.trim().replace(/\/+$/, "")
						: undefined,
				apiVersion:
					editingProvider === "azure" ? azureApiVersion.trim() : undefined,
				deployment:
					editingProvider === "azure" ? azureDeployment.trim() : undefined,
			});
			setTestResult({
				success: result.success,
				message: result.message,
				sample: result.sample,
			});
			if (result.success) {
				setTestedSignature(currentSignature);
				return true;
			} else {
				setTestedSignature(null);
				return false;
			}
		} catch (error: any) {
			setTestResult({success: false, message: `Failed: ${error.message}`});
			setTestedSignature(null);
			return false;
		}
	};

	const handleSave = async () => {
		if (!editingProvider || !modelInput.trim()) return;

		if (editingProvider === "azure") {
			if (!keyInput.trim()) {
				setMessage({
					text: "API key is required for Azure OpenAI",
					type: "error",
				});
				return;
			}
			if (!azureBaseUrl.trim()) {
				setMessage({
					text: "Base URL is required for Azure OpenAI",
					type: "error",
				});
				return;
			}
			if (!azureApiVersion.trim()) {
				setMessage({
					text: "API Version is required for Azure OpenAI",
					type: "error",
				});
				return;
			}
			if (!azureDeployment.trim()) {
				setMessage({
					text: "Deployment Name is required for Azure OpenAI",
					type: "error",
				});
				return;
			}
			const normalizedBaseUrl = azureBaseUrl.trim().replace(/\/+$/, "");
			if (!normalizedBaseUrl.endsWith(".openai.azure.com")) {
				setMessage({
					text: "Base URL must end with '.openai.azure.com'",
					type: "error",
				});
				return;
			}
		}

		if (testedSignature !== currentSignature) {
			const testPassed = await handleTestModel();
			if (!testPassed) return;
		}

		if (editingProvider === "azure") {
			const azureModel = `azure/${azureDeployment.trim()}`;
			updateMutation.mutate({
				provider: editingProvider,
				apiKey: keyInput.trim(),
				model: azureModel,
				baseUrl: azureBaseUrl.trim().replace(/\/+$/, ""),
				apiVersion: azureApiVersion.trim(),
				deployment: azureDeployment.trim(),
			});
		} else {
			updateMutation.mutate({
				provider: editingProvider,
				apiKey: keyInput.trim(),
				model: modelInput.trim(),
			});
		}
	};

	const monitorOpenAiBrowserOAuth = async (
		stateToken: string,
		signal: AbortSignal,
	) => {
		setIsPollingOpenAiBrowserOAuth(true);
		setOpenAiBrowserOAuthMessage(null);
		try {
			for (let attempt = 0; attempt < 360; attempt += 1) {
				if (signal.aborted) return;
				const status = await api.openAiOAuthBrowserStatus(stateToken);
				if (signal.aborted) return;
				if (status.done) {
					setDeviceCodeInfo(null);
					setDeviceCodeCopied(false);
					if (status.success) {
						setOpenAiBrowserOAuthMessage({
							text: status.message || "ChatGPT OAuth configured.",
							type: "success",
						});
						queryClient.invalidateQueries({queryKey: ["providers"]});
						setTimeout(() => {
							queryClient.invalidateQueries({queryKey: ["agents"]});
							queryClient.invalidateQueries({queryKey: ["overview"]});
						}, 3000);
					} else {
						setOpenAiBrowserOAuthMessage({
							text: status.message || "Sign-in failed.",
							type: "error",
						});
					}
					return;
				}
				await new Promise((resolve) => {
					const onAbort = () => {
						clearTimeout(timer);
						resolve(undefined);
					};
					const timer = setTimeout(() => {
						signal.removeEventListener("abort", onAbort);
						resolve(undefined);
					}, 2000);
					signal.addEventListener("abort", onAbort, {once: true});
				});
			}
			if (signal.aborted) return;
			setDeviceCodeInfo(null);
			setDeviceCodeCopied(false);
			setOpenAiBrowserOAuthMessage({
				text: "Sign-in timed out. Please try again.",
				type: "error",
			});
		} catch (error: any) {
			if (signal.aborted) return;
			setDeviceCodeInfo(null);
			setDeviceCodeCopied(false);
			setOpenAiBrowserOAuthMessage({
				text: `Failed to verify sign-in: ${error.message}`,
				type: "error",
			});
		} finally {
			setIsPollingOpenAiBrowserOAuth(false);
		}
	};

	const handleStartChatGptOAuth = async () => {
		setOpenAiBrowserOAuthMessage(null);
		setDeviceCodeInfo(null);
		setDeviceCodeCopied(false);
		try {
			const result = await startOpenAiBrowserOAuthMutation.mutateAsync({
				model: CHATGPT_OAUTH_DEFAULT_MODEL,
			});
			if (
				!result.success ||
				!result.user_code ||
				!result.verification_url ||
				!result.state
			) {
				setOpenAiBrowserOAuthMessage({
					text: result.message || "Failed to start device sign-in",
					type: "error",
				});
				return;
			}

			oauthAbortRef.current?.abort();
			const abort = new AbortController();
			oauthAbortRef.current = abort;

			setDeviceCodeInfo({
				userCode: result.user_code,
				verificationUrl: result.verification_url,
			});
			void monitorOpenAiBrowserOAuth(result.state, abort.signal);
		} catch (error: any) {
			setOpenAiBrowserOAuthMessage({
				text: `Failed: ${error.message}`,
				type: "error",
			});
		}
	};

	useEffect(() => {
		if (!openAiOAuthDialogOpen) {
			oauthAutoStartRef.current = false;
			oauthAbortRef.current?.abort();
			oauthAbortRef.current = null;
			setDeviceCodeInfo(null);
			setDeviceCodeCopied(false);
			setOpenAiBrowserOAuthMessage(null);
			setIsPollingOpenAiBrowserOAuth(false);
			return;
		}

		if (oauthAutoStartRef.current) return;
		oauthAutoStartRef.current = true;
		void handleStartChatGptOAuth();
	}, [openAiOAuthDialogOpen]);

	const handleCopyDeviceCode = async () => {
		if (!deviceCodeInfo) return;
		try {
			if (navigator.clipboard?.writeText) {
				await navigator.clipboard.writeText(deviceCodeInfo.userCode);
			} else {
				const textarea = document.createElement("textarea");
				textarea.value = deviceCodeInfo.userCode;
				textarea.setAttribute("readonly", "");
				textarea.style.position = "absolute";
				textarea.style.left = "-9999px";
				document.body.appendChild(textarea);
				textarea.select();
				document.execCommand("copy");
				document.body.removeChild(textarea);
			}
			setDeviceCodeCopied(true);
		} catch (error: any) {
			setOpenAiBrowserOAuthMessage({
				text: `Failed to copy code: ${error.message}`,
				type: "error",
			});
		}
	};

	const handleOpenDeviceLogin = () => {
		if (!deviceCodeInfo || !deviceCodeCopied) return;
		window.open(
			deviceCodeInfo.verificationUrl,
			"spacebot-openai-device",
			"popup=true,width=780,height=960,noopener,noreferrer",
		);
	};

	const handleClose = () => {
		fetchAbortControllerRef.current?.abort();
		setEditingProvider(null);
		setKeyInput("");
		setModelInput("");
		setTestedSignature(null);
		setTestResult(null);
		setAzureBaseUrl("");
		setAzureApiVersion("");
		setAzureDeployment("");
	};

	const isConfigured = (providerId: string): boolean => {
		if (!data) return false;
		const statusKey = providerId.replace(
			/-/g,
			"_",
		) as keyof typeof data.providers;
		return data.providers[statusKey] ?? false;
	};

	return (
		<div className="flex h-full min-h-0 overflow-hidden">
			{/* Sidebar */}
			{showList && (
			<div
				className={
					isMobile
						? "flex min-h-0 w-full shrink-0 flex-col overflow-y-auto"
						: "flex min-h-0 w-52 shrink-0 flex-col overflow-y-auto border-r border-app-line/50 bg-app-dark-box/20"
				}
			>
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
			)}

			{/* Content */}
			{showDetail && (
			<div className="flex min-h-0 flex-1 flex-col overflow-hidden">
				{isMobile && (
					<div className="flex shrink-0 items-center gap-2 border-b border-app-line/50 px-2 py-1.5">
						<button
							type="button"
							aria-label="Back to settings"
							onClick={() => setMobileShowDetail(false)}
							className="flex h-11 w-11 items-center justify-center rounded-lg text-ink hover:bg-app-box/60 active:bg-app-box/80"
						>
							<CaretLeft size={20} weight="regular" />
						</button>
						<span className="truncate text-sm font-medium text-ink">
							{SECTIONS.find((s) => s.id === activeSection)?.label}
						</span>
					</div>
				)}
				<div className="min-h-0 flex-1 overflow-y-auto overscroll-contain">
					{activeSection === "instance" ? (
						<InstanceSection
							settings={globalSettings}
							isLoading={globalSettingsLoading}
						/>
					) : activeSection === "appearance" ? (
						<AppearanceSection />
					) : activeSection === "providers" ? (
						<div className="mx-auto max-w-2xl px-6 py-6">
							{/* Section header */}
							<div className="mb-6">
								<h2 className="font-plex text-sm font-semibold text-ink">
									LLM Providers
								</h2>
								<p className="mt-1 text-sm text-ink-dull">
									Configure credentials/endpoints for LLM providers. At least
									one provider is required for agents to function.
								</p>
							</div>

							<div className="mb-4 rounded-md border border-app-line bg-app-dark-box/20 px-4 py-3">
								<p className="text-sm text-ink-faint">
									When you add a provider, choose a model and run a completion
									test before saving. Saving applies that model to all five
									default routing roles and to your default agent.
								</p>
							</div>

							{isLoading ? (
								<div className="flex items-center gap-2 text-ink-dull">
									<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
									Loading providers...
								</div>
							) : (
								<div className="flex flex-col gap-3">
									{PROVIDERS.map((provider) => [
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
												if (provider.id === "azure") {
													// Reset Azure fields before hydrating
													setAzureBaseUrl("");
													setAzureApiVersion("");
													setAzureDeployment("");

													// Cancel previous request
													fetchAbortControllerRef.current?.abort();

													// Create new abort controller
													const abortController = new AbortController();
													fetchAbortControllerRef.current = abortController;

													api
														.getProviderConfig("azure", {
															signal: abortController.signal,
														})
														.then((result) => {
															// Check if aborted
															if (abortController.signal.aborted) return;
															if (!result.success) return;

															setAzureBaseUrl(result.base_url ?? "");
															setAzureApiVersion(result.api_version ?? "");
															const deployment = result.deployment ?? "";
															setAzureDeployment(deployment);
															if (deployment) {
																setModelInput(`azure/${deployment}`);
															}
														})
														.catch((error) => {
															if (error.name === "AbortError") return;
															console.error(
																"Failed to fetch Azure config:",
																error,
															);
														});
												}
											}}
											onRemove={() => removeMutation.mutate(provider.id)}
											removing={removeMutation.isPending}
										/>,
										provider.id === "openai" ? (
											<ProviderCard
												key="openai-chatgpt"
												provider="openai-chatgpt"
												name="ChatGPT Plus (OAuth)"
												description="Sign in with your ChatGPT Plus account using a device code."
												configured={isConfigured("openai-chatgpt")}
												defaultModel={CHATGPT_OAUTH_DEFAULT_MODEL}
												onEdit={() => setOpenAiOAuthDialogOpen(true)}
												onRemove={() => removeMutation.mutate("openai-chatgpt")}
												removing={removeMutation.isPending}
												actionLabel="Sign in"
												showRemove={isConfigured("openai-chatgpt")}
											/>
										) : null,
									])}
								</div>
							)}

							{/* Info note */}
							<div className="mt-6 rounded-md border border-app-line bg-app-dark-box/20 px-4 py-3">
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

							<ChatGptOAuthDialog
								open={openAiOAuthDialogOpen}
								onOpenChange={setOpenAiOAuthDialogOpen}
								isRequesting={startOpenAiBrowserOAuthMutation.isPending}
								isPolling={isPollingOpenAiBrowserOAuth}
								message={openAiBrowserOAuthMessage}
								deviceCodeInfo={deviceCodeInfo}
								deviceCodeCopied={deviceCodeCopied}
								onCopyDeviceCode={handleCopyDeviceCode}
								onOpenDeviceLogin={handleOpenDeviceLogin}
								onRestart={handleStartChatGptOAuth}
							/>
						</div>
					) : activeSection === "channels" ? (
						<ChannelsSection />
					) : activeSection === "api-keys" ? (
						<ApiKeysSection
							settings={globalSettings}
							isLoading={globalSettingsLoading}
						/>
					) : activeSection === "secrets" ? (
						<SecretsSection />
					) : activeSection === "server" ? (
						<ServerSection
							settings={globalSettings}
							isLoading={globalSettingsLoading}
						/>
					) : activeSection === "opencode" ? (
						<OpenCodeSection
							settings={globalSettings}
							isLoading={globalSettingsLoading}
						/>
					) : activeSection === "worker-logs" ? (
						<WorkerLogsSection
							settings={globalSettings}
							isLoading={globalSettingsLoading}
						/>
					) : activeSection === "updates" ? (
						<UpdatesSection />
					) : activeSection === "config-file" ? (
						<ConfigFileSection />
					) : activeSection === "changelog" ? (
						<ChangelogSection />
					) : null}
				</div>
			</div>
			)}

			<DialogRoot
				open={!!editingProvider}
				onOpenChange={(open) => {
					if (!open) handleClose();
				}}
			>
				<DialogContent className="max-w-md">
					<DialogHeader>
						<DialogTitle>
							{isConfigured(editingProvider ?? "") ? "Update" : "Add"}{" "}
							{editingProvider === "ollama" || editingProvider === "azure"
								? "Endpoint"
								: "API Key"}
						</DialogTitle>
						<DialogDescription>
							{editingProvider === "ollama"
								? `Enter your ${editingProviderData?.name} base URL. It will be saved to your instance config.`
								: editingProvider === "azure"
									? "Enter your Azure OpenAI configuration. API key, base URL, API version, and deployment name are required."
									: editingProvider === "openai"
										? "Enter an OpenAI API key. The model below will be applied to routing."
										: `Enter your ${editingProviderData?.name} API key. It will be saved to your instance config.`}
						</DialogDescription>
					</DialogHeader>
					{editingProvider === "azure" ? (
						<>
							<Input
								type="password"
								value={keyInput}
								onChange={(e) => {
									setKeyInput(e.target.value);
									setTestedSignature(null);
								}}
								placeholder="Azure API key"
								autoFocus
								onKeyDown={(e) => {
									if (e.key === "Enter") handleSave();
								}}
							/>
							<div className="space-y-1.5 mt-3">
								<label className="text-sm font-medium text-ink">Base URL</label>
								<Input
									type="text"
									value={azureBaseUrl}
									onChange={(e) => {
										setAzureBaseUrl(e.target.value);
										setTestedSignature(null);
									}}
									placeholder="https://{resource-name}.openai.azure.com"
									onKeyDown={(e) => {
										if (e.key === "Enter") handleSave();
									}}
								/>
								<p className="text-tiny text-ink-faint">
									Must end with '.openai.azure.com'
								</p>
							</div>
							<div className="space-y-1.5 mt-3">
								<label className="text-sm font-medium text-ink">
									API Version
								</label>
								<Input
									type="text"
									value={azureApiVersion}
									onChange={(e) => {
										setAzureApiVersion(e.target.value);
										setTestedSignature(null);
									}}
									placeholder="2024-06-01"
									onKeyDown={(e) => {
										if (e.key === "Enter") handleSave();
									}}
								/>
								<p className="text-tiny text-ink-faint">
									For example: 2024-06-01, 2024-10-01-preview
								</p>
							</div>
							<div className="space-y-1.5 mt-3">
								<label className="text-sm font-medium text-ink">
									Deployment Name
								</label>
								<Input
									type="text"
									value={azureDeployment}
									onChange={(e) => {
										setAzureDeployment(e.target.value);
										setTestedSignature(null);
									}}
									placeholder="gpt-4o"
									onKeyDown={(e) => {
										if (e.key === "Enter") handleSave();
									}}
								/>
								<p className="text-tiny text-ink-faint">
									Your Azure OpenAI deployment name
								</p>
							</div>
							<div className="space-y-1.5 mt-3">
								<label className="text-sm font-medium text-ink">Model</label>
								<Input
									type="text"
									value={`azure/${azureDeployment || "deployment"}`}
									disabled
									className="bg-app-dark-box/50"
								/>
								<p className="text-tiny text-ink-faint">
									Model is auto-generated from deployment name
								</p>
							</div>
						</>
					) : (
						<>
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
						</>
					)}
					<div className="flex items-center gap-2 mt-3">
						<Button
							onClick={handleTestModel}
							disabled={
								!editingProvider ||
								!modelInput.trim() ||
								(editingProvider === "azure" &&
									(!azureBaseUrl.trim() ||
										!azureApiVersion.trim() ||
										!azureDeployment.trim()))
							}
							variant="outline"
							size="md"
						>
							Test model
						</Button>
						{testedSignature === currentSignature && testResult?.success && (
							<span className="text-xs text-green-400">Verified</span>
						)}
					</div>
					{testResult && (
						<div
							className={`rounded-md border px-3 py-2 text-sm ${
								testResult.success
									? "border-green-500/20 bg-green-500/10 text-green-400"
									: "border-red-500/20 bg-red-500/10 text-red-400"
							}`}
						>
							<div>{testResult.message}</div>
							{testResult.success && testResult.sample ? (
								<div className="mt-1 text-xs text-ink-dull">
									Sample: {testResult.sample}
								</div>
							) : null}
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
						{editingProvider === "azure" ? (
							<>
								{isConfigured(editingProvider ?? "") && (
									<Button
										onClick={() =>
											removeMutation.mutate(editingProvider, {
												onSuccess: (result) => {
													if (result.success) handleClose();
												},
											})
										}
										variant="accent"
										size="md"
									>
										Remove
									</Button>
								)}
								<Button onClick={handleClose} variant="outline" size="md">
									Cancel
								</Button>
								<Button
									onClick={handleSave}
									disabled={
										!keyInput.trim() ||
										!azureBaseUrl.trim() ||
										!azureApiVersion.trim() ||
										!azureDeployment.trim()
									}
									size="md"
								>
									Save
								</Button>
							</>
						) : (
							<>
								<Button onClick={handleClose} variant="outline" size="md">
									Cancel
								</Button>
								<Button
									onClick={handleSave}
									disabled={!modelInput.trim()}
									size="md"
								>
									Save
								</Button>
							</>
						)}
					</DialogFooter>
				</DialogContent>
			</DialogRoot>
		</div>
	);
}
