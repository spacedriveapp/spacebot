import {useState, useEffect} from "react";
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
import {ModelSelect} from "@/components/ModelSelect";
import {ProviderIcon} from "@/lib/providerIcons";
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
	SECTIONS,
	API_TYPES,
	DEFAULT_NEW_PROVIDER,
	type ApiTypeId,
	type SectionId,
} from "@/components/settings";

export function Settings() {
	const queryClient = useQueryClient();
	const navigate = useNavigate();
	const search = useSearch({from: "/settings"}) as {tab?: string};
	const [activeSection, setActiveSection] = useState<SectionId>("providers");

	// Sync activeSection with URL search param
	useEffect(() => {
		if (search.tab && SECTIONS.some((s) => s.id === search.tab)) {
			setActiveSection(search.tab as SectionId);
		}
	}, [search.tab]);

	const handleSectionChange = (section: SectionId) => {
		setActiveSection(section);
		navigate({to: "/settings", search: {tab: section}});
	};
	// `editingProvider` holds the id being edited, or "" for a brand-new one —
	// null means the dialog is closed.
	const [editingProvider, setEditingProvider] = useState<string | null>(null);
	const [providerIdInput, setProviderIdInput] = useState("");
	const [apiTypeInput, setApiTypeInput] = useState<ApiTypeId>(
		DEFAULT_NEW_PROVIDER.apiType,
	);
	const [baseUrlInput, setBaseUrlInput] = useState("");
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
			apiType,
			baseUrl,
		}: {
			provider: string;
			apiKey: string;
			model: string;
			apiType: string;
			baseUrl?: string;
		}) => api.updateProvider(provider, apiKey, model, apiType, baseUrl),
		onSuccess: (result) => {
			if (result.success) {
				handleClose();
				setMessage({text: result.message, type: "success"});
				queryClient.invalidateQueries({queryKey: ["providers"]});
				queryClient.invalidateQueries({queryKey: ["models"]});
				// Agents auto-start on the backend; refetch after a short delay.
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
			apiType,
			baseUrl,
		}: {
			provider: string;
			apiKey: string;
			model: string;
			apiType: string;
			baseUrl?: string;
		}) => api.testProviderModel(provider, apiKey, model, apiType, baseUrl),
	});

	const removeMutation = useMutation({
		mutationFn: (provider: string) => api.removeProvider(provider),
		onSuccess: (result) => {
			if (result.success) {
				setMessage({text: result.message, type: "success"});
				queryClient.invalidateQueries({queryKey: ["providers"]});
				queryClient.invalidateQueries({queryKey: ["models"]});
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) => {
			setMessage({text: `Failed: ${error.message}`, type: "error"});
		},
	});

	const providers = data?.providers ?? [];
	const apiTypeMeta =
		API_TYPES.find((entry) => entry.id === apiTypeInput) ?? API_TYPES[0];
	const normalizedProviderId = providerIdInput.trim().toLowerCase();

	// Re-testing is only skippable when nothing that affects the request has
	// changed — a verified badge over a different base URL is a lie.
	const currentSignature = [
		normalizedProviderId,
		apiTypeInput,
		baseUrlInput.trim(),
		keyInput.trim(),
		modelInput.trim(),
	].join("|");

	const buildTestArgs = () => ({
		provider: normalizedProviderId,
		apiKey: keyInput.trim(),
		model: modelInput.trim(),
		apiType: apiTypeInput,
		baseUrl: baseUrlInput.trim() || undefined,
	});

	const validationError = (): string | null => {
		if (!normalizedProviderId) return "Provider name is required";
		if (normalizedProviderId.includes("/") || /\s/.test(normalizedProviderId)) {
			return "Provider name cannot contain '/' or whitespace";
		}
		if (apiTypeInput === "openai_compatible" && !baseUrlInput.trim()) {
			return "Base URL is required for OpenAI-compatible providers";
		}
		if (!modelInput.trim()) return "Model is required";
		if (!modelInput.trim().startsWith(`${normalizedProviderId}/`)) {
			return `Model must be prefixed with the provider name, e.g. ${normalizedProviderId}/claude-sonnet-4`;
		}
		return null;
	};

	const handleTestModel = async (): Promise<boolean> => {
		const invalid = validationError();
		if (invalid) {
			setTestResult({success: false, message: invalid});
			return false;
		}

		setMessage(null);
		setTestResult(null);
		try {
			const result = await testModelMutation.mutateAsync(buildTestArgs());
			setTestResult({
				success: result.success,
				message: result.message,
				sample: result.sample,
			});
			setTestedSignature(result.success ? currentSignature : null);
			return result.success;
		} catch (error: any) {
			setTestResult({success: false, message: `Failed: ${error.message}`});
			setTestedSignature(null);
			return false;
		}
	};

	const handleSave = async () => {
		const invalid = validationError();
		if (invalid) {
			setMessage({text: invalid, type: "error"});
			return;
		}

		if (testedSignature !== currentSignature) {
			const testPassed = await handleTestModel();
			if (!testPassed) return;
		}

		updateMutation.mutate(buildTestArgs());
	};

	const handleClose = () => {
		setEditingProvider(null);
		setProviderIdInput("");
		setBaseUrlInput("");
		setKeyInput("");
		setModelInput("");
		setTestedSignature(null);
		setTestResult(null);
	};

	const openNewProviderDialog = () => {
		setEditingProvider("");
		setProviderIdInput(DEFAULT_NEW_PROVIDER.id);
		setApiTypeInput(DEFAULT_NEW_PROVIDER.apiType);
		setBaseUrlInput(DEFAULT_NEW_PROVIDER.baseUrl);
		setKeyInput("");
		setModelInput("");
		setTestedSignature(null);
		setTestResult(null);
		setMessage(null);
	};

	const openEditProviderDialog = (provider: {
		id: string;
		api_type: string;
		base_url: string;
	}) => {
		setEditingProvider(provider.id);
		setProviderIdInput(provider.id);
		setApiTypeInput(
			provider.api_type === "anthropic" ? "anthropic" : "openai_compatible",
		);
		setBaseUrlInput(provider.base_url);
		// Never prefill a credential — the API deliberately never returns one.
		setKeyInput("");
		setModelInput("");
		setTestedSignature(null);
		setTestResult(null);
		setMessage(null);
	};

	return (
		<div className="flex h-full min-h-0 overflow-hidden">
			{/* Sidebar */}
			<div className="flex min-h-0 w-52 flex-shrink-0 flex-col overflow-y-auto border-r border-app-line/50 bg-app-dark-box/20">
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
			<div className="flex min-h-0 flex-1 flex-col overflow-hidden">
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
									A provider is an endpoint plus a credential. Spacebot speaks
									two APIs: Anthropic's Messages API natively, and
									OpenAI-compatible <code>/chat/completions</code> for everything
									else. At least one provider is required for agents to function.
								</p>
							</div>

							<div className="mb-4 rounded-md border border-app-line bg-app-dark-box/20 px-4 py-3">
								<p className="text-sm text-ink-faint">
									The name you give a provider becomes its routing prefix, so a
									provider called <code>litellm</code> serves models as{" "}
									<code>litellm/&lt;model&gt;</code>. Saving runs a completion
									test, then applies that model to all five default routing roles
									and to your default agent.
								</p>
							</div>

							{isLoading ? (
								<div className="flex items-center gap-2 text-ink-dull">
									<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
									Loading providers...
								</div>
							) : (
								<div className="flex flex-col gap-3">
									{data?.anthropic_oauth && (
										<div className="rounded-lg border border-app-line bg-app-box p-4">
											<div className="flex items-center gap-3">
												<ProviderIcon provider="anthropic" size={32} />
												<div className="flex-1">
													<div className="flex items-center gap-2">
														<span className="text-sm font-medium text-ink">
															Anthropic OAuth
														</span>
														<span className="h-2 w-2 rounded-full bg-status-success" />
													</div>
													<p className="mt-0.5 text-sm text-ink-dull">
														Signed in with a Claude Pro/Max subscription via{" "}
														<code>spacebot auth login</code>. This overrides the{" "}
														<code>anthropic</code> provider's API key.
													</p>
												</div>
												<Button
													onClick={() =>
														removeMutation.mutate("anthropic-oauth")
													}
													variant="outline"
													size="md"
													loading={removeMutation.isPending}
												>
													Sign out
												</Button>
											</div>
										</div>
									)}

									{providers.map((provider) => (
										<ProviderCard
											key={provider.id}
											provider={provider.id}
											apiType={provider.api_type}
											baseUrl={provider.base_url}
											displayName={provider.display_name}
											hasKey={provider.has_key}
											onEdit={() => openEditProviderDialog(provider)}
											onRemove={() => removeMutation.mutate(provider.id)}
											removing={removeMutation.isPending}
										/>
									))}

									{providers.length === 0 && !data?.anthropic_oauth && (
										<div className="rounded-lg border border-dashed border-app-line px-4 py-8 text-center text-sm text-ink-faint">
											No providers configured yet.
										</div>
									)}

									<Button
										onClick={openNewProviderDialog}
										variant="outline"
										size="md"
										className="self-start"
									>
										Add provider
									</Button>
								</div>
							)}

							{message && !editingProvider && (
								<div
									className={`mt-4 rounded-md border px-3 py-2 text-sm ${
										message.type === "success"
											? "border-status-success/20 bg-status-success/10 text-status-success"
											: "border-status-error/20 bg-status-error/10 text-status-error"
									}`}
								>
									{message.text}
								</div>
							)}

							{/* Info note */}
							<div className="mt-6 rounded-md border border-app-line bg-app-dark-box/20 px-4 py-3">
								<p className="text-sm text-ink-faint">
									Providers are written as{" "}
									<code className="rounded bg-app-box px-1 py-0.5 text-tiny text-ink-dull">
										[llm.provider.&lt;id&gt;]
									</code>{" "}
									blocks in{" "}
									<code className="rounded bg-app-box px-1 py-0.5 text-tiny text-ink-dull">
										config.toml
									</code>
									. Two environment variable pairs also bootstrap one without a
									config file:{" "}
									<code className="rounded bg-app-box px-1 py-0.5 text-tiny text-ink-dull">
										ANTHROPIC_API_KEY
									</code>{" "}
									/{" "}
									<code className="rounded bg-app-box px-1 py-0.5 text-tiny text-ink-dull">
										ANTHROPIC_BASE_URL
									</code>{" "}
									and{" "}
									<code className="rounded bg-app-box px-1 py-0.5 text-tiny text-ink-dull">
										LITELLM_API_KEY
									</code>{" "}
									/{" "}
									<code className="rounded bg-app-box px-1 py-0.5 text-tiny text-ink-dull">
										LITELLM_BASE_URL
									</code>
									.
								</p>
							</div>
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

			<DialogRoot
				open={editingProvider !== null}
				onOpenChange={(open) => {
					if (!open) handleClose();
				}}
			>
				<DialogContent className="max-w-md">
					<DialogHeader>
						<DialogTitle>
							{editingProvider ? "Update provider" : "Add provider"}
						</DialogTitle>
						<DialogDescription>
							{apiTypeMeta.description}
						</DialogDescription>
					</DialogHeader>

					<div className="space-y-1.5">
						<label className="text-sm font-medium text-ink">
							Provider name
						</label>
						<Input
							type="text"
							value={providerIdInput}
							onChange={(e) => {
								setProviderIdInput(e.target.value);
								setTestedSignature(null);
							}}
							placeholder="litellm"
							disabled={!!editingProvider}
							autoFocus={!editingProvider}
						/>
						<p className="text-tiny text-ink-faint">
							Used as the routing prefix:{" "}
							<code>{normalizedProviderId || "litellm"}/&lt;model&gt;</code>
						</p>
					</div>

					<div className="space-y-1.5 mt-3">
						<label className="text-sm font-medium text-ink">API type</label>
						<div className="flex gap-2">
							{API_TYPES.map((entry) => (
								<Button
									key={entry.id}
									onClick={() => {
										setApiTypeInput(entry.id);
										setTestedSignature(null);
										if (!baseUrlInput.trim()) {
											setBaseUrlInput(entry.baseUrlPlaceholder);
										}
									}}
									variant={apiTypeInput === entry.id ? "accent" : "outline"}
									size="md"
								>
									{entry.label}
								</Button>
							))}
						</div>
					</div>

					<div className="space-y-1.5 mt-3">
						<label className="text-sm font-medium text-ink">Base URL</label>
						<Input
							type="text"
							value={baseUrlInput}
							onChange={(e) => {
								setBaseUrlInput(e.target.value);
								setTestedSignature(null);
							}}
							placeholder={apiTypeMeta.baseUrlPlaceholder}
							onKeyDown={(e) => {
								if (e.key === "Enter") handleSave();
							}}
						/>
						<p className="text-tiny text-ink-faint">
							{apiTypeMeta.baseUrlHint}
						</p>
					</div>

					<div className="space-y-1.5 mt-3">
						<label className="text-sm font-medium text-ink">API key</label>
						<Input
							type="password"
							value={keyInput}
							onChange={(e) => {
								setKeyInput(e.target.value);
								setTestedSignature(null);
							}}
							placeholder={apiTypeMeta.keyPlaceholder}
							onKeyDown={(e) => {
								if (e.key === "Enter") handleSave();
							}}
						/>
					</div>

					<ModelSelect
						label="Model"
						description="Pick the exact model ID to verify and apply to routing"
						value={modelInput}
						onChange={(value) => {
							setModelInput(value);
							setTestedSignature(null);
						}}
						provider={normalizedProviderId || undefined}
					/>

					<div className="flex items-center gap-2 mt-3">
						<Button
							onClick={handleTestModel}
							disabled={!!validationError()}
							loading={testModelMutation.isPending}
							variant="outline"
							size="md"
						>
							Test model
						</Button>
						{testedSignature === currentSignature && testResult?.success && (
							<span className="text-xs text-status-success">Verified</span>
						)}
					</div>
					{testResult && (
						<div
							className={`rounded-md border px-3 py-2 text-sm ${
								testResult.success
									? "border-status-success/20 bg-status-success/10 text-status-success"
									: "border-status-error/20 bg-status-error/10 text-status-error"
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
					{message && editingProvider !== null && (
						<div
							className={`rounded-md border px-3 py-2 text-sm ${
								message.type === "success"
									? "border-status-success/20 bg-status-success/10 text-status-success"
									: "border-status-error/20 bg-status-error/10 text-status-error"
							}`}
						>
							{message.text}
						</div>
					)}
					<DialogFooter>
						<Button onClick={handleClose} variant="outline" size="md">
							Cancel
						</Button>
						<Button
							onClick={handleSave}
							disabled={!!validationError()}
							loading={updateMutation.isPending}
							size="md"
						>
							Save
						</Button>
					</DialogFooter>
				</DialogContent>
			</DialogRoot>
		</div>
	);
}
