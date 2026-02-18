import { useCallback, useEffect, useState, useRef } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { api, type AgentConfigResponse, type AgentConfigUpdateRequest } from "@/api/client";
import { Button, SettingSidebarButton, Input, TextArea, Toggle, NumberStepper, cx } from "@/ui";
import { ModelSelect } from "@/components/ModelSelect";
import { Markdown } from "@/components/Markdown";
import { motion, AnimatePresence } from "framer-motion";
import { useSearch, useNavigate } from "@tanstack/react-router";


function supportsAdaptiveThinking(modelId: string): boolean {
	const id = modelId.toLowerCase();
	return id.includes("opus-4-6") || id.includes("opus-4.6")
		|| id.includes("sonnet-4-6") || id.includes("sonnet-4.6");
}

type SectionId = "soul" | "identity" | "user" | "routing" | "tuning" | "compaction" | "cortex" | "coalesce" | "memory" | "browser";

const SECTIONS: {
	id: SectionId;
	label: string;
	group: "identity" | "config";
	description: string;
	detail: string;
}[] = [
	{ id: "soul", label: "Soul", group: "identity", description: "SOUL.md", detail: "Defines the agent's personality, values, communication style, and behavioral boundaries. This is the core of who the agent is." },
	{ id: "identity", label: "Identity", group: "identity", description: "IDENTITY.md", detail: "The agent's name, nature, and purpose. How it introduces itself and what it understands its role to be." },
	{ id: "user", label: "User", group: "identity", description: "USER.md", detail: "Information about the human this agent interacts with. Name, preferences, context, and anything that helps the agent personalize responses." },
	{ id: "routing", label: "Model Routing", group: "config", description: "Which models each process uses", detail: "Controls which LLM model is used for each process type. Channels handle user-facing conversation, branches do thinking, workers execute tasks, the compactor summarizes context, and the cortex observes system state." },
	{ id: "tuning", label: "Tuning", group: "config", description: "Turn limits, context window, branches", detail: "Core limits that control how much work the agent does per message. Max turns caps LLM iterations per channel message. Context window sets the token budget. Branch limits control parallel thinking." },
	{ id: "compaction", label: "Compaction", group: "config", description: "Context compaction thresholds", detail: "Thresholds that trigger context summarization as the conversation grows. Background kicks in early, aggressive compresses harder, and emergency truncates without LLM involvement. All values are fractions of the context window." },
	{ id: "cortex", label: "Cortex", group: "config", description: "System observer settings", detail: "The cortex monitors active processes and generates memory bulletins. Tick interval controls observation frequency. Timeouts determine when stuck workers or branches get cancelled. The circuit breaker auto-disables after consecutive failures." },
	{ id: "coalesce", label: "Coalesce", group: "config", description: "Message batching", detail: "When multiple messages arrive in quick succession, coalescing batches them into a single LLM turn. This prevents the agent from responding to each message individually in fast-moving conversations." },
	{ id: "memory", label: "Memory Persistence", group: "config", description: "Auto-save interval", detail: "Spawns a silent background branch at regular intervals to recall existing memories and save new ones from the recent conversation. Runs without blocking the channel." },
	{ id: "browser", label: "Browser", group: "config", description: "Chrome automation", detail: "Controls browser automation tools available to workers. When enabled, workers can navigate web pages, take screenshots, and interact with sites. JavaScript evaluation is a separate permission." },
];

interface AgentConfigProps {
	agentId: string;
}

const isIdentityField = (id: SectionId): id is "soul" | "identity" | "user" => {
	return id === "soul" || id === "identity" || id === "user";
};

const getIdentityField = (data: { soul: string | null; identity: string | null; user: string | null }, field: SectionId): string | null => {
	if (isIdentityField(field)) {
		return data[field];
	}
	return null;
};

export function AgentConfig({ agentId }: AgentConfigProps) {
	const queryClient = useQueryClient();
	const navigate = useNavigate();
	const search = useSearch({from: "/agents/$agentId/config"}) as {tab?: string};
	const [activeSection, setActiveSection] = useState<SectionId>("soul");
	const [dirty, setDirty] = useState(false);
	const [saving, setSaving] = useState(false);
	const saveHandlerRef = useRef<{ save?: () => void; revert?: () => void }>({});

	// Sync activeSection with URL search param
	useEffect(() => {
		if (search.tab) {
			const validSections: SectionId[] = ["soul", "identity", "user", "routing", "tuning", "compaction", "cortex", "coalesce", "memory", "browser"];
			if (validSections.includes(search.tab as SectionId)) {
				setActiveSection(search.tab as SectionId);
			}
		}
	}, [search.tab]);

	const handleSectionChange = (section: SectionId) => {
		setActiveSection(section);
		navigate({to: "/agents/$agentId/config", params: {agentId}, search: {tab: section}});
	};

	const identityQuery = useQuery({
		queryKey: ["agent-identity", agentId],
		queryFn: () => api.agentIdentity(agentId),
		staleTime: 10_000,
	});

	const configQuery = useQuery({
		queryKey: ["agent-config", agentId],
		queryFn: () => api.agentConfig(agentId),
		staleTime: 10_000,
	});

	const identityMutation = useMutation({
		mutationFn: (update: { field: "soul" | "identity" | "user"; content: string }) =>
			api.updateIdentity({
				agent_id: agentId,
				[update.field]: update.content,
			}),
		onMutate: () => setSaving(true),
		onSuccess: (result) => {
			queryClient.setQueryData(["agent-identity", agentId], result);
			setDirty(false);
			setSaving(false);
		},
		onError: () => setSaving(false),
	});

	const configMutation = useMutation({
		mutationFn: (update: AgentConfigUpdateRequest) => api.updateAgentConfig(update),
		onMutate: () => setSaving(true),
		onSuccess: (result) => {
			queryClient.setQueryData(["agent-config", agentId], result);
			setDirty(false);
			setSaving(false);
		},
		onError: () => setSaving(false),
	});

	const handleSave = useCallback(() => {
		saveHandlerRef.current.save?.();
	}, []);

	const handleRevert = useCallback(() => {
		saveHandlerRef.current.revert?.();
	}, []);

	const isLoading = identityQuery.isLoading || configQuery.isLoading;
	const isError = identityQuery.isError || configQuery.isError;

	if (isLoading) {
		return (
			<div className="flex h-full items-center justify-center">
				<div className="flex items-center gap-2 text-ink-dull">
					<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
					Loading configuration...
				</div>
			</div>
		);
	}

	if (isError) {
		return (
			<div className="flex h-full items-center justify-center">
				<p className="text-sm text-red-400">Failed to load configuration</p>
			</div>
		);
	}

	const active = SECTIONS.find((s) => s.id === activeSection)!;
	const isIdentitySection = active.group === "identity";

	return (
		<div className="flex h-full relative">
			{/* Sidebar */}
			<div className="flex w-52 flex-shrink-0 flex-col border-r border-app-line/50 bg-app-darkBox/20 overflow-y-auto">
				{/* Identity Group */}
				<div className="px-3 pb-1 pt-4">
					<span className="text-tiny font-medium uppercase tracking-wider text-ink-faint">Identity</span>
				</div>
					<div className="flex flex-col gap-0.5 px-2">
					{SECTIONS.filter((s) => s.group === "identity").map((section) => {
						const isActive = activeSection === section.id;
						const hasContent = !!getIdentityField(identityQuery.data ?? { soul: null, identity: null, user: null }, section.id)?.trim();
						return (
							<SettingSidebarButton
								key={section.id}
								onClick={() => handleSectionChange(section.id)}
								active={isActive}
							>
								<span className="flex-1">{section.label}</span>
								{!hasContent && (
									<span className="rounded bg-amber-500/10 px-1 py-0.5 text-tiny text-amber-400/70">empty</span>
								)}
							</SettingSidebarButton>
						);
					})}
				</div>

				{/* Config Group */}
				<div className="px-3 pb-1 pt-4 mt-2">
					<span className="text-tiny font-medium uppercase tracking-wider text-ink-faint">Configuration</span>
				</div>
					<div className="flex flex-col gap-0.5 px-2">
					{SECTIONS.filter((s) => s.group === "config").map((section) => {
						const isActive = activeSection === section.id;
						return (
							<SettingSidebarButton
								key={section.id}
								onClick={() => handleSectionChange(section.id)}
								active={isActive}
							>
								<span className="flex-1">{section.label}</span>
							</SettingSidebarButton>
						);
					})}
				</div>
			</div>

			{/* Editor */}
			<div className="flex flex-1 flex-col overflow-hidden">
				{isIdentitySection ? (
				<IdentityEditor
					key={active.id}
					label={active.label}
					description={active.description}
					content={getIdentityField(identityQuery.data ?? { soul: null, identity: null, user: null }, active.id)}
					onDirtyChange={setDirty}
					saveHandlerRef={saveHandlerRef}
					onSave={(content) => {
						// Only mutate for identity sections
						if (isIdentityField(active.id)) {
							identityMutation.mutate({ field: active.id, content });
						}
					}}
				/>
				) : (
					<ConfigSectionEditor
						sectionId={active.id}
						label={active.label}
						description={active.description}
						detail={active.detail}
						config={configQuery.data!}
						onDirtyChange={setDirty}
						saveHandlerRef={saveHandlerRef}
						onSave={(update) => configMutation.mutate({ agent_id: agentId, ...update })}
					/>
				)}
			</div>

			{/* Fixed bottom save bar */}
			<AnimatePresence>
				{dirty && (
					<motion.div
						initial={{ y: 100 }}
						animate={{ y: 0 }}
						exit={{ y: 100 }}
						transition={{ type: "spring", damping: 25, stiffness: 300 }}
						className="absolute bottom-4 right-4 flex items-center gap-4 rounded-lg border border-app-line/50 bg-app-darkBox px-4 py-3 shadow-lg"
					>
						<span className="text-sm text-ink-dull">You have unsaved changes</span>
						<div className="flex items-center gap-2">
							<Button
								onClick={handleRevert}
								variant="ghost"
								size="sm"
							>
								Revert
							</Button>
							<Button
								onClick={handleSave}
								size="sm"
								loading={saving}
							>
								Save Changes
							</Button>
						</div>
					</motion.div>
				)}
			</AnimatePresence>
		</div>
	);
}

// -- Identity Editor --

interface IdentityEditorProps {
	label: string;
	description: string;
	content: string | null;
	onDirtyChange: (dirty: boolean) => void;
	saveHandlerRef: React.MutableRefObject<{ save?: () => void; revert?: () => void }>;
	onSave: (value: string) => void;
}

function IdentityEditor({ label, description, content, onDirtyChange, saveHandlerRef, onSave }: IdentityEditorProps) {
	const [value, setValue] = useState(content ?? "");
	const [localDirty, setLocalDirty] = useState(false);
	const [mode, setMode] = useState<"edit" | "preview">("edit");

	useEffect(() => {
		if (!localDirty) {
			setValue(content ?? "");
		}
	}, [content, localDirty]);

	useEffect(() => {
		onDirtyChange(localDirty);
	}, [localDirty, onDirtyChange]);

	const handleChange = useCallback((event: React.ChangeEvent<HTMLTextAreaElement>) => {
		setValue(event.target.value);
		setLocalDirty(true);
	}, []);

	const handleSave = useCallback(() => {
		onSave(value);
		setLocalDirty(false);
	}, [onSave, value]);

	const handleRevert = useCallback(() => {
		setValue(content ?? "");
		setLocalDirty(false);
	}, [content]);

	const handleKeyDown = useCallback(
		(event: React.KeyboardEvent) => {
			if ((event.metaKey || event.ctrlKey) && event.key === "s") {
				event.preventDefault();
				if (localDirty) handleSave();
			}
		},
		[localDirty, handleSave],
	);

	// Register save/revert handlers
	useEffect(() => {
		saveHandlerRef.current.save = handleSave;
		saveHandlerRef.current.revert = handleRevert;
		return () => {
			saveHandlerRef.current.save = undefined;
			saveHandlerRef.current.revert = undefined;
		};
	}, [handleSave, handleRevert]);

	return (
		<>
			<div className="flex items-center justify-between border-b border-app-line/50 bg-app-darkBox/20 px-5 py-2.5">
				<div className="flex items-center gap-3">
					<h3 className="text-sm font-medium text-ink">{label}</h3>
					<span className="rounded bg-app-darkBox px-1.5 py-0.5 font-mono text-tiny text-ink-faint">{description}</span>
				</div>
				<div className="flex items-center gap-3">
					<div className="flex items-center rounded border border-app-line/50 text-tiny">
						<button
							onClick={() => setMode("edit")}
							className={cx("px-2 py-0.5 rounded-l transition-colors", mode === "edit" ? "bg-app-darkBox text-ink" : "text-ink-faint hover:text-ink")}
						>
							Edit
						</button>
						<button
							onClick={() => setMode("preview")}
							className={cx("px-2 py-0.5 rounded-r transition-colors", mode === "preview" ? "bg-app-darkBox text-ink" : "text-ink-faint hover:text-ink")}
						>
							Preview
						</button>
					</div>
					{localDirty ? (
						<span className="text-tiny text-amber-400">Unsaved changes</span>
					) : (
						<span className="text-tiny text-ink-faint/50">Cmd+S to save</span>
					)}
				</div>
			</div>
			<div className="flex-1 overflow-y-auto p-4">
				{mode === "edit" ? (
					<TextArea
						value={value}
						onChange={handleChange}
						onKeyDown={handleKeyDown}
						placeholder={`Write ${label.toLowerCase()} content here...`}
						className="h-full w-full resize-none border-transparent bg-app-darkBox/30 px-4 py-3 font-mono leading-relaxed placeholder:text-ink-faint/40"
						spellCheck={false}
					/>
				) : (
					<div className="prose-sm px-4 py-3">
						{value ? (
							<Markdown>{value}</Markdown>
						) : (
							<span className="text-ink-faint/40 text-sm">Nothing to preview.</span>
						)}
					</div>
				)}
			</div>
		</>
	);
}

// -- Config Section Editors --

interface ConfigSectionEditorProps {
	sectionId: SectionId;
	label: string;
	description: string;
	detail: string;
	config: AgentConfigResponse;
	onDirtyChange: (dirty: boolean) => void;
	saveHandlerRef: React.MutableRefObject<{ save?: () => void; revert?: () => void }>;
	onSave: (update: Partial<AgentConfigUpdateRequest>) => void;
}

function ConfigSectionEditor({ sectionId, label, description, detail, config, onDirtyChange, saveHandlerRef, onSave }: ConfigSectionEditorProps) {
	const [localValues, setLocalValues] = useState<Record<string, string | number | boolean>>(() => {
		// Initialize from config based on section
		switch (sectionId) {
			case "routing":
				return { ...config.routing };
			case "tuning":
				return { ...config.tuning };
			case "compaction":
				return { ...config.compaction };
			case "cortex":
				return { ...config.cortex };
			case "coalesce":
				return { ...config.coalesce };
			case "memory":
				return { ...config.memory_persistence };
			case "browser":
				return { ...config.browser };
			default:
				return {};
		}
	});

	const [localDirty, setLocalDirty] = useState(false);

	useEffect(() => {
		onDirtyChange(localDirty);
	}, [localDirty, onDirtyChange]);

	// Reset local values when config changes externally
	useEffect(() => {
		if (!localDirty) {
			switch (sectionId) {
				case "routing":
					setLocalValues({ ...config.routing });
					break;
				case "tuning":
					setLocalValues({ ...config.tuning });
					break;
				case "compaction":
					setLocalValues({ ...config.compaction });
					break;
				case "cortex":
					setLocalValues({ ...config.cortex });
					break;
				case "coalesce":
					setLocalValues({ ...config.coalesce });
					break;
				case "memory":
					setLocalValues({ ...config.memory_persistence });
					break;
				case "browser":
					setLocalValues({ ...config.browser });
					break;
			}
		}
	}, [config, sectionId, localDirty]);

	const handleChange = useCallback((field: string, value: string | number | boolean) => {
		setLocalValues((prev) => ({ ...prev, [field]: value }));
		setLocalDirty(true);
	}, []);

	const handleSave = useCallback(() => {
		onSave({ [sectionId]: localValues });
		setLocalDirty(false);
	}, [onSave, sectionId, localValues]);

	const handleRevert = useCallback(() => {
		switch (sectionId) {
			case "routing":
				setLocalValues({ ...config.routing });
				break;
			case "tuning":
				setLocalValues({ ...config.tuning });
				break;
			case "compaction":
				setLocalValues({ ...config.compaction });
				break;
			case "cortex":
				setLocalValues({ ...config.cortex });
				break;
			case "coalesce":
				setLocalValues({ ...config.coalesce });
				break;
			case "memory":
				setLocalValues({ ...config.memory_persistence });
				break;
			case "browser":
				setLocalValues({ ...config.browser });
				break;
		}
		setLocalDirty(false);
	}, [config, sectionId]);

	// Register save/revert handlers
	useEffect(() => {
		saveHandlerRef.current.save = handleSave;
		saveHandlerRef.current.revert = handleRevert;
		return () => {
			saveHandlerRef.current.save = undefined;
			saveHandlerRef.current.revert = undefined;
		};
	}, [handleSave, handleRevert]);

	const renderFields = () => {
		switch (sectionId) {
			case "routing": {
				const modelSlots = [
					{ key: "channel", label: "Channel Model", description: "Model for user-facing channels" },
					{ key: "branch", label: "Branch Model", description: "Model for thinking branches" },
					{ key: "worker", label: "Worker Model", description: "Model for task workers" },
					{ key: "compactor", label: "Compactor Model", description: "Model for summarization" },
					{ key: "cortex", label: "Cortex Model", description: "Model for system observation" },
				];
				return (
					<div className="grid gap-4">
						{modelSlots.map(({ key, label, description }) => (
							<div key={key} className="flex flex-col gap-2">
								<ModelSelect
									label={label}
									description={description}
									value={localValues[key] as string}
									onChange={(v) => handleChange(key, v)}
								/>
								{supportsAdaptiveThinking(localValues[key] as string) && (
									<div className="ml-4 flex flex-col gap-1">
										<label className="text-xs font-medium text-ink-dull">Thinking Effort</label>
										<select
											value={(localValues[`${key}_thinking_effort`] as string) || "auto"}
											onChange={(e) => handleChange(`${key}_thinking_effort`, e.target.value)}
											className="w-full rounded-md border border-app-line/50 bg-app-darkBox/30 px-3 py-1.5 text-sm text-ink focus:outline-none focus:ring-1 focus:ring-accent"
										>
											<option value="auto">Auto</option>
											<option value="max">Max</option>
											<option value="high">High</option>
											<option value="medium">Medium</option>
											<option value="low">Low</option>
										</select>
									</div>
								)}
							</div>
						))}
						<NumberStepper
							label="Rate Limit Cooldown"
							description="Seconds to deprioritize rate-limited models"
							value={localValues.rate_limit_cooldown_secs as number}
							onChange={(v) => handleChange("rate_limit_cooldown_secs", v)}
							min={0}
							suffix="s"
						/>
					</div>
				);
			}
			case "tuning":
				return (
					<div className="grid gap-4">
						<NumberStepper
							label="Max Concurrent Branches"
							description="Maximum branches per channel"
							value={localValues.max_concurrent_branches as number}
							onChange={(v) => handleChange("max_concurrent_branches", v)}
							min={1}
							max={20}
						/>
						<NumberStepper
							label="Max Concurrent Workers"
							description="Maximum workers per channel"
							value={localValues.max_concurrent_workers as number}
							onChange={(v) => handleChange("max_concurrent_workers", v)}
							min={1}
							max={20}
						/>
						<NumberStepper
							label="Max Turns"
							description="Max LLM turns per channel message"
							value={localValues.max_turns as number}
							onChange={(v) => handleChange("max_turns", v)}
							min={1}
							max={50}
						/>
						<NumberStepper
							label="Branch Max Turns"
							description="Max turns for thinking branches"
							value={localValues.branch_max_turns as number}
							onChange={(v) => handleChange("branch_max_turns", v)}
							min={1}
							max={100}
						/>
						<NumberStepper
							label="Context Window"
							description="Context window size in tokens"
							value={localValues.context_window as number}
							onChange={(v) => handleChange("context_window", v)}
							min={1000}
							step={1000}
							suffix=" tokens"
						/>
						<NumberStepper
							label="History Backfill"
							description="Messages to fetch on new channel"
							value={localValues.history_backfill_count as number}
							onChange={(v) => handleChange("history_backfill_count", v)}
							min={0}
							max={500}
							suffix=" messages"
						/>
					</div>
				);
			case "compaction":
				return (
					<div className="grid gap-4">
						<NumberStepper
							label="Background Threshold"
							description="Start background summarization (fraction of context window)"
							value={localValues.background_threshold as number}
							onChange={(v) => handleChange("background_threshold", v)}
							min={0}
							max={1}
							step={0.01}
							type="float"
							showProgress
						/>
						<NumberStepper
							label="Aggressive Threshold"
							description="Start aggressive summarization"
							value={localValues.aggressive_threshold as number}
							onChange={(v) => handleChange("aggressive_threshold", v)}
							min={0}
							max={1}
							step={0.01}
							type="float"
							showProgress
						/>
						<NumberStepper
							label="Emergency Threshold"
							description="Emergency truncation (no LLM, drop oldest 50%)"
							value={localValues.emergency_threshold as number}
							onChange={(v) => handleChange("emergency_threshold", v)}
							min={0}
							max={1}
							step={0.01}
							type="float"
							showProgress
						/>
					</div>
				);
			case "cortex":
				return (
					<div className="grid gap-4">
						<NumberStepper
							label="Tick Interval"
							description="How often the cortex checks system state"
							value={localValues.tick_interval_secs as number}
							onChange={(v) => handleChange("tick_interval_secs", v)}
							min={1}
							suffix="s"
						/>
						<NumberStepper
							label="Worker Timeout"
							description="Worker timeout before cancellation"
							value={localValues.worker_timeout_secs as number}
							onChange={(v) => handleChange("worker_timeout_secs", v)}
							min={10}
							suffix="s"
						/>
						<NumberStepper
							label="Branch Timeout"
							description="Branch timeout before cancellation"
							value={localValues.branch_timeout_secs as number}
							onChange={(v) => handleChange("branch_timeout_secs", v)}
							min={5}
							suffix="s"
						/>
						<NumberStepper
							label="Circuit Breaker"
							description="Consecutive failures before auto-disable"
							value={localValues.circuit_breaker_threshold as number}
							onChange={(v) => handleChange("circuit_breaker_threshold", v)}
							min={1}
							max={10}
						/>
						<NumberStepper
							label="Bulletin Interval"
							description="Seconds between memory bulletin refreshes"
							value={localValues.bulletin_interval_secs as number}
							onChange={(v) => handleChange("bulletin_interval_secs", v)}
							min={60}
							suffix="s"
						/>
						<NumberStepper
							label="Bulletin Max Words"
							description="Target word count for memory bulletin"
							value={localValues.bulletin_max_words as number}
							onChange={(v) => handleChange("bulletin_max_words", v)}
							min={100}
							max={5000}
							suffix=" words"
						/>
						<NumberStepper
							label="Bulletin Max Turns"
							description="Max LLM turns for bulletin generation"
							value={localValues.bulletin_max_turns as number}
							onChange={(v) => handleChange("bulletin_max_turns", v)}
							min={5}
							max={50}
						/>
					</div>
				);
			case "coalesce":
				return (
					<div className="grid gap-4">
						<ConfigToggleField
							label="Enabled"
							description="Enable message coalescing for multi-user channels"
							value={localValues.enabled as boolean}
							onChange={(v) => handleChange("enabled", v)}
						/>
						<NumberStepper
							label="Debounce"
							description="Initial debounce window after first message"
							value={localValues.debounce_ms as number}
							onChange={(v) => handleChange("debounce_ms", v)}
							min={100}
							max={10000}
							suffix="ms"
						/>
						<NumberStepper
							label="Max Wait"
							description="Maximum time to wait before flushing"
							value={localValues.max_wait_ms as number}
							onChange={(v) => handleChange("max_wait_ms", v)}
							min={500}
							max={30000}
							suffix="ms"
						/>
						<NumberStepper
							label="Min Messages"
							description="Min messages to trigger coalesce mode"
							value={localValues.min_messages as number}
							onChange={(v) => handleChange("min_messages", v)}
							min={1}
							max={10}
						/>
						<ConfigToggleField
							label="Multi-User Only"
							description="Apply only to multi-user conversations (skip for DMs)"
							value={localValues.multi_user_only as boolean}
							onChange={(v) => handleChange("multi_user_only", v)}
						/>
					</div>
				);
			case "memory":
				return (
					<div className="grid gap-4">
						<ConfigToggleField
							label="Enabled"
							description="Enable automatic memory persistence branches"
							value={localValues.enabled as boolean}
							onChange={(v) => handleChange("enabled", v)}
						/>
						<NumberStepper
							label="Message Interval"
							description="Number of user messages between automatic saves"
							value={localValues.message_interval as number}
							onChange={(v) => handleChange("message_interval", v)}
							min={1}
							max={200}
							suffix=" messages"
						/>
					</div>
				);
			case "browser":
				return (
					<div className="grid gap-4">
						<ConfigToggleField
							label="Enabled"
							description="Enable browser automation tools for workers"
							value={localValues.enabled as boolean}
							onChange={(v) => handleChange("enabled", v)}
						/>
						<ConfigToggleField
							label="Headless"
							description="Run Chrome in headless mode"
							value={localValues.headless as boolean}
							onChange={(v) => handleChange("headless", v)}
						/>
						<ConfigToggleField
							label="JavaScript Evaluation"
							description="Allow JavaScript evaluation via browser tool"
							value={localValues.evaluate_enabled as boolean}
							onChange={(v) => handleChange("evaluate_enabled", v)}
						/>
					</div>
				);
			default:
				return null;
		}
	};

	return (
		<>
			<div className="flex items-center justify-between border-b border-app-line/50 bg-app-darkBox/20 px-5 py-2.5">
				<div className="flex items-center gap-3">
					<h3 className="text-sm font-medium text-ink">{label}</h3>
					<span className="text-tiny text-ink-faint">{description}</span>
				</div>
				{localDirty ? (
					<span className="text-tiny text-amber-400">Unsaved changes</span>
				) : (
					<span className="text-tiny text-ink-faint/50">Changes saved to config.toml</span>
				)}
			</div>
			<div className="flex-1 overflow-y-auto px-8 py-8">
				<div className="mb-6 rounded-lg border border-app-line/30 bg-app-darkBox/20 px-5 py-4">
					<p className="text-sm leading-relaxed text-ink-dull">{detail}</p>
				</div>
				{renderFields()}
			</div>
		</>
	);
}

// -- Form Field Components --

interface ConfigFieldProps {
	label: string;
	description: string;
	value: string;
	onChange: (value: string) => void;
}

function ConfigField({ label, description, value, onChange }: ConfigFieldProps) {
	return (
		<div className="flex flex-col gap-1.5">
			<label className="text-sm font-medium text-ink">{label}</label>
			<p className="text-tiny text-ink-faint">{description}</p>
			<Input
				type="text"
				value={value}
				onChange={(e) => onChange(e.target.value)}
				className="mt-1 border-app-line/50 bg-app-darkBox/30"
			/>
		</div>
	);
}

interface ConfigToggleFieldProps {
	label: string;
	description: string;
	value: boolean;
	onChange: (value: boolean) => void;
}

function ConfigToggleField({ label, description, value, onChange }: ConfigToggleFieldProps) {
	return (
		<div className="flex items-center justify-between py-2">
			<div className="flex flex-col gap-0.5">
				<label className="text-sm font-medium text-ink">{label}</label>
				<p className="text-tiny text-ink-faint">{description}</p>
			</div>
			<Toggle checked={value} onCheckedChange={onChange} size="lg" />
		</div>
	);
}
