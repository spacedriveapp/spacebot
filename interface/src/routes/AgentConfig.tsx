import { useCallback, useEffect, useState, useRef } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { api, type AgentConfigResponse, type AgentConfigUpdateRequest } from "@/api/client";
import { Button, Input, SettingSidebarButton, TextArea, Toggle, NumberStepper, Select, SelectTrigger, SelectValue, SelectContent, SelectItem, cx } from "@/ui";
import { ModelSelect } from "@/components/ModelSelect";
import { TagInput } from "@/components/TagInput";
import { Markdown } from "@/components/Markdown";
import { ProfileAvatar, seedGradient } from "@/components/ProfileAvatar";
import { motion, AnimatePresence } from "framer-motion";
import { useSearch, useNavigate } from "@tanstack/react-router";


function supportsAdaptiveThinking(modelId: string): boolean {
	const id = modelId.toLowerCase();
	return id.includes("opus-4-6") || id.includes("opus-4.6")
		|| id.includes("sonnet-4-6") || id.includes("sonnet-4.6");
}

type SectionId = "general" | "soul" | "identity" | "role" | "speech" | "routing" | "tuning" | "compaction" | "cortex" | "coalesce" | "memory" | "browser" | "channel" | "sandbox" | "projects";

const SECTIONS: {
	id: SectionId;
	label: string;
	group: "general" | "identity" | "config";
	description: string;
	detail: string;
}[] = [
	{ id: "general", label: "General", group: "general", description: "Agent metadata", detail: "The agent's display name and role. The display name is shown in the UI and messaging platforms. The role describes the agent's purpose (e.g. Research Assistant, Code Reviewer)." },
	{ id: "soul", label: "Soul", group: "identity", description: "SOUL.md", detail: "Defines the agent's personality, values, communication style, and behavioral boundaries. This is the core of who the agent is." },
	{ id: "identity", label: "Identity", group: "identity", description: "IDENTITY.md", detail: "The agent's name, nature, and purpose. How it introduces itself and what it understands its role to be." },
	{ id: "role", label: "Role", group: "identity", description: "ROLE.md", detail: "The agent's responsibilities, scope, expected outcomes, and escalation rules. Defines what the agent does and doesn't do." },
	{ id: "speech", label: "Speech", group: "identity", description: "SPEECH.md", detail: "Shapes how the agent sounds when speaking aloud. Use it to set cadence, phrasing, warmth, emphasis, and anti-repetition guidance for spoken responses." },
	{ id: "routing", label: "Model Routing", group: "config", description: "Which models each process uses", detail: "Controls which LLM model is used for each process type. Channels handle user-facing conversation, branches do thinking, workers execute tasks, the compactor summarizes context, cortex observes system state, and voice transcribes audio attachments before the channel turn." },
	{ id: "tuning", label: "Tuning", group: "config", description: "Turn limits, context window, branches", detail: "Core limits that control how much work the agent does per message. Max turns caps LLM iterations per channel message. Context window sets the token budget. Branch limits control parallel thinking." },
	{ id: "compaction", label: "Compaction", group: "config", description: "Context compaction thresholds", detail: "Thresholds that trigger context summarization as the conversation grows. Background kicks in early, aggressive compresses harder, and emergency truncates without LLM involvement. All values are fractions of the context window." },
	{ id: "cortex", label: "Cortex", group: "config", description: "System observer settings", detail: "The cortex monitors active processes and generates memory bulletins. Tick interval controls observation frequency. Timeouts determine when stuck workers or branches get cancelled. The circuit breaker auto-disables after consecutive failures." },
	{ id: "coalesce", label: "Coalesce", group: "config", description: "Message batching", detail: "When multiple messages arrive in quick succession, coalescing batches them into a single LLM turn. This prevents the agent from responding to each message individually in fast-moving conversations." },
	{ id: "memory", label: "Memory Persistence", group: "config", description: "Auto-save interval", detail: "Spawns a silent background branch at regular intervals to recall existing memories and save new ones from the recent conversation. Runs without blocking the channel." },
	{ id: "browser", label: "Browser", group: "config", description: "Chrome automation", detail: "Controls browser automation tools available to workers. When enabled, workers can navigate web pages, take screenshots, and interact with sites. JavaScript evaluation is a separate permission." },
	{ id: "channel", label: "Channel Behavior", group: "config", description: "Reply behavior", detail: "Listen-only mode suppresses unsolicited replies in busy channels. The agent still responds to slash commands, @mentions, and replies to its own messages." },
	{ id: "sandbox", label: "Sandbox", group: "config", description: "Process containment", detail: "OS-level filesystem containment for shell tool subprocesses. When enabled, worker processes run inside a kernel-enforced sandbox (bubblewrap on Linux, sandbox-exec on macOS) with an allowlist-only filesystem — only system paths, the workspace, and explicitly configured extra paths are accessible." },
	{ id: "projects", label: "Projects", group: "config", description: "Workspace management", detail: "Controls how the agent manages project workspaces, git repos, and worktrees. Use worktrees for parallel feature branches, auto-discover to scan for repos on project creation, and set a disk usage warning threshold." },
];

interface AgentConfigProps {
	agentId: string;
}

const isIdentityField = (id: SectionId): id is "soul" | "identity" | "role" | "speech" => {
	return id === "soul" || id === "identity" || id === "role" || id === "speech";
};

const getIdentityField = (data: { soul: string | null; identity: string | null; role: string | null; speech: string | null }, field: SectionId): string | null => {
	if (isIdentityField(field)) {
		return data[field];
	}
	return null;
};

export function AgentConfig({ agentId }: AgentConfigProps) {
	const queryClient = useQueryClient();
	const navigate = useNavigate();
	const search = useSearch({from: "/agents/$agentId/config"}) as {tab?: string};
	const [activeSection, setActiveSection] = useState<SectionId>("general");
	const [dirty, setDirty] = useState(false);
	const [saving, setSaving] = useState(false);
	const saveHandlerRef = useRef<{ save?: () => void; revert?: () => void }>({});

	// Sync activeSection with URL search param
	useEffect(() => {
		if (search.tab) {
			const validSections = SECTIONS.map((section) => section.id);
			if (validSections.includes(search.tab as SectionId)) {
				setActiveSection(search.tab as SectionId);
			}
		}
	}, [search.tab]);

	const handleSectionChange = (section: SectionId) => {
		setActiveSection(section);
		navigate({to: "/agents/$agentId/config", params: {agentId}, search: {tab: section}});
	};

	const agentsQuery = useQuery({
		queryKey: ["agents"],
		queryFn: () => api.agents(),
		staleTime: 10_000,
	});

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

	const agentMutation = useMutation({
		mutationFn: (update: { display_name?: string; role?: string; gradient_start?: string; gradient_end?: string }) =>
			api.updateAgent(agentId, update),
		onMutate: () => setSaving(true),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["agents"] });
			setDirty(false);
			setSaving(false);
		},
		onError: () => setSaving(false),
	});

	const identityMutation = useMutation({
		mutationFn: (update: { field: "soul" | "identity" | "role" | "speech"; content: string }) =>
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
		onMutate: (update) => {
			setSaving(true);
			// Optimistically merge the sent values into the cache so the UI
			// reflects the change immediately (covers fields the backend
			// doesn't yet return in its response, like sandbox).
			const previous = queryClient.getQueryData<AgentConfigResponse>(["agent-config", agentId]);
			if (previous) {
				const { agent_id: _, ...sections } = update;
				const merged = { ...previous } as unknown as Record<string, unknown>;
				const prev = previous as unknown as Record<string, unknown>;
				for (const [key, value] of Object.entries(sections)) {
					if (value !== undefined) {
						merged[key] = {
							...(prev[key] as Record<string, unknown> | undefined),
							...value,
						};
					}
				}
				queryClient.setQueryData(["agent-config", agentId], merged as unknown as AgentConfigResponse);
			}
		},
		onSuccess: (result) => {
			// Merge server response with cache to preserve fields the backend
			// doesn't yet return (e.g. sandbox).
			const previous = queryClient.getQueryData<AgentConfigResponse>(["agent-config", agentId]);
			queryClient.setQueryData(["agent-config", agentId], { ...previous, ...result });
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

	const isLoading = agentsQuery.isLoading || identityQuery.isLoading || configQuery.isLoading;
	const isError = agentsQuery.isError || identityQuery.isError || configQuery.isError;

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
	const isGeneralSection = active.group === "general";
	const isIdentitySection = active.group === "identity";
	const currentAgent = agentsQuery.data?.agents.find((a) => a.id === agentId);

	return (
		<div className="flex h-full relative">
			{/* Sidebar */}
			<div className="flex w-52 flex-shrink-0 flex-col border-r border-app-line/50 bg-app-darkBox/20 overflow-y-auto">
			{/* General Group */}
			<div className="flex flex-col gap-0.5 px-2 pt-3">
				{SECTIONS.filter((s) => s.group === "general").map((section) => {
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

			{/* Identity Group */}
			<div className="px-3 pb-1 pt-4">
				<span className="text-tiny font-medium uppercase tracking-wider text-ink-faint">Identity</span>
			</div>
					<div className="flex flex-col gap-0.5 px-2">
					{SECTIONS.filter((s) => s.group === "identity").map((section) => {
						const isActive = activeSection === section.id;
						const hasContent = !!getIdentityField(identityQuery.data ?? { soul: null, identity: null, role: null, speech: null }, section.id)?.trim();
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
			{isGeneralSection ? (
			<GeneralEditor
				key={active.id}
				agentId={agentId}
				displayName={currentAgent?.display_name ?? ""}
				role={currentAgent?.role ?? ""}
				gradientStart={currentAgent?.gradient_start ?? ""}
				gradientEnd={currentAgent?.gradient_end ?? ""}
				detail={active.detail}
				onDirtyChange={setDirty}
				saveHandlerRef={saveHandlerRef}
				onSave={(update) => agentMutation.mutate(update)}
			/>
			) : isIdentitySection ? (
			<IdentityEditor
				key={active.id}
				label={active.label}
				description={active.description}
				content={getIdentityField(identityQuery.data ?? { soul: null, identity: null, role: null, speech: null }, active.id)}
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

// -- General Editor --

interface GeneralEditorProps {
	agentId: string;
	displayName: string;
	role: string;
	gradientStart: string;
	gradientEnd: string;
	detail: string;
	onDirtyChange: (dirty: boolean) => void;
	saveHandlerRef: React.MutableRefObject<{ save?: () => void; revert?: () => void }>;
	onSave: (update: { display_name?: string; role?: string; gradient_start?: string; gradient_end?: string }) => void;
}

const GRADIENT_PRESETS: { label: string; start: string; end: string }[] = [
	{ label: "Purple", start: "hsl(270, 70%, 55%)", end: "hsl(310, 60%, 45%)" },
	{ label: "Blue", start: "hsl(220, 70%, 55%)", end: "hsl(260, 60%, 45%)" },
	{ label: "Teal", start: "hsl(170, 70%, 45%)", end: "hsl(200, 60%, 40%)" },
	{ label: "Green", start: "hsl(140, 60%, 45%)", end: "hsl(170, 50%, 40%)" },
	{ label: "Orange", start: "hsl(25, 80%, 55%)", end: "hsl(45, 70%, 45%)" },
	{ label: "Red", start: "hsl(0, 70%, 55%)", end: "hsl(20, 60%, 45%)" },
	{ label: "Pink", start: "hsl(330, 70%, 55%)", end: "hsl(350, 60%, 45%)" },
	{ label: "Gold", start: "hsl(45, 80%, 55%)", end: "hsl(30, 70%, 40%)" },
	{ label: "Indigo", start: "hsl(240, 60%, 55%)", end: "hsl(280, 50%, 45%)" },
	{ label: "Slate", start: "hsl(220, 15%, 50%)", end: "hsl(220, 10%, 35%)" },
];

function GeneralEditor({ agentId, displayName, role, gradientStart, gradientEnd, detail, onDirtyChange, saveHandlerRef, onSave }: GeneralEditorProps) {
	const queryClient = useQueryClient();
	const [localDisplayName, setLocalDisplayName] = useState(displayName);
	const [localRole, setLocalRole] = useState(role);
	const [localGradientStart, setLocalGradientStart] = useState(gradientStart);
	const [localGradientEnd, setLocalGradientEnd] = useState(gradientEnd);
	const [localDirty, setLocalDirty] = useState(false);
	const [avatarPreview, setAvatarPreview] = useState<string | null>(null);
	const fileInputRef = useRef<HTMLInputElement>(null);

	// Check if agent has an uploaded avatar
	const avatarUrl = api.agentAvatarUrl(agentId);
	const { data: avatarExists } = useQuery({
		queryKey: ["agentAvatar", agentId],
		queryFn: async () => {
			try {
				const response = await fetch(avatarUrl, { method: "HEAD" });
				return response.ok;
			} catch {
				return false;
			}
		},
	});

	const uploadAvatarMutation = useMutation({
		mutationFn: (file: File) => api.uploadAvatar(agentId, file),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["agentAvatar", agentId] });
			queryClient.invalidateQueries({ queryKey: ["agents"] });
			queryClient.invalidateQueries({ queryKey: ["topology"] });
			setAvatarPreview(null);
		},
	});

	const deleteAvatarMutation = useMutation({
		mutationFn: () => api.deleteAvatar(agentId),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["agentAvatar", agentId] });
			queryClient.invalidateQueries({ queryKey: ["agents"] });
			queryClient.invalidateQueries({ queryKey: ["topology"] });
			setAvatarPreview(null);
		},
	});

	useEffect(() => {
		if (!localDirty) {
			setLocalDisplayName(displayName);
			setLocalRole(role);
			setLocalGradientStart(gradientStart);
			setLocalGradientEnd(gradientEnd);
		}
	}, [displayName, role, gradientStart, gradientEnd, localDirty]);

	useEffect(() => {
		onDirtyChange(localDirty);
	}, [localDirty, onDirtyChange]);

	const handleSave = useCallback(() => {
		onSave({
			display_name: localDisplayName,
			role: localRole,
			gradient_start: localGradientStart || undefined,
			gradient_end: localGradientEnd || undefined,
		});
		setLocalDirty(false);
	}, [onSave, localDisplayName, localRole, localGradientStart, localGradientEnd]);

	const handleRevert = useCallback(() => {
		setLocalDisplayName(displayName);
		setLocalRole(role);
		setLocalGradientStart(gradientStart);
		setLocalGradientEnd(gradientEnd);
		setLocalDirty(false);
	}, [displayName, role, gradientStart, gradientEnd]);

	useEffect(() => {
		saveHandlerRef.current.save = handleSave;
		saveHandlerRef.current.revert = handleRevert;
		return () => {
			saveHandlerRef.current.save = undefined;
			saveHandlerRef.current.revert = undefined;
		};
	}, [handleSave, handleRevert]);

	const handleFileChange = (event: React.ChangeEvent<HTMLInputElement>) => {
		const file = event.target.files?.[0];
		if (!file) return;
		// Show preview immediately
		const reader = new FileReader();
		reader.onload = () => setAvatarPreview(reader.result as string);
		reader.readAsDataURL(file);
		// Upload
		uploadAvatarMutation.mutate(file);
	};

	const [seedC1, seedC2] = seedGradient(agentId);
	const previewC1 = localGradientStart || seedC1;
	const previewC2 = localGradientEnd || seedC2;

	return (
		<>
			<div className="flex items-center justify-between border-b border-app-line/50 bg-app-darkBox/20 px-5 py-2.5">
				<div className="flex items-center gap-3">
					<h3 className="text-sm font-medium text-ink">General</h3>
					<span className="text-tiny text-ink-faint">Agent metadata</span>
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
				<div className="grid gap-4">
					<div className="flex flex-col gap-1.5">
						<label className="text-sm font-medium text-ink">Agent ID</label>
						<p className="text-tiny text-ink-faint">The config key identifier. This cannot be changed.</p>
						<Input
							value={agentId}
							disabled
							className="border-app-line/50 bg-app-darkBox/30 text-ink-faint"
						/>
					</div>
					<div className="flex flex-col gap-1.5">
						<label className="text-sm font-medium text-ink">Display Name</label>
						<p className="text-tiny text-ink-faint">Human-friendly name shown in the UI and messaging platforms.</p>
						<Input
							value={localDisplayName}
							onChange={(e) => { setLocalDisplayName(e.target.value); setLocalDirty(true); }}
							placeholder={agentId}
						/>
					</div>
					<div className="flex flex-col gap-1.5">
						<label className="text-sm font-medium text-ink">Role</label>
						<p className="text-tiny text-ink-faint">Describes the agent's purpose. Shown in the topology view and agent listings.</p>
						<Input
							value={localRole}
							onChange={(e) => { setLocalRole(e.target.value); setLocalDirty(true); }}
							placeholder="e.g. Research Assistant, Code Reviewer"
						/>
					</div>

					{/* Avatar */}
					<div className="flex flex-col gap-1.5">
						<label className="text-sm font-medium text-ink">Avatar</label>
						<p className="text-tiny text-ink-faint">Upload a custom image, or use the generated gradient avatar.</p>
						<div className="flex items-center gap-4">
							<ProfileAvatar
								seed={agentId}
								name={localDisplayName || agentId}
								size={64}
								className="rounded-full shrink-0"
								gradientStart={localGradientStart || undefined}
								gradientEnd={localGradientEnd || undefined}
								avatarUrl={avatarPreview ?? (avatarExists ? `${avatarUrl}&t=${Date.now()}` : undefined)}
							/>
							<div className="flex flex-col gap-2">
								<div className="flex items-center gap-2">
									<Button
										variant="outline"
										size="sm"
										onClick={() => fileInputRef.current?.click()}
									>
										{avatarExists ? "Change Image" : "Upload Image"}
									</Button>
									{avatarExists && (
										<Button
											variant="ghost"
											size="sm"
											onClick={() => deleteAvatarMutation.mutate()}
											className="text-ink-faint hover:text-red-400"
										>
											Remove
										</Button>
									)}
								</div>
								<p className="text-tiny text-ink-faint/60">PNG, JPEG, WebP, GIF, or SVG. Max 5 MB.</p>
							</div>
							<input
								ref={fileInputRef}
								type="file"
								accept="image/png,image/jpeg,image/gif,image/webp,image/svg+xml"
								onChange={handleFileChange}
								className="hidden"
							/>
						</div>
					</div>

					{/* Gradient Color */}
					<div className="flex flex-col gap-1.5">
						<label className="text-sm font-medium text-ink">Gradient</label>
						<p className="text-tiny text-ink-faint">Choose a gradient for the avatar and banner. Only used when no image is uploaded.</p>
						<div className="flex flex-wrap gap-2">
							{/* Auto (seed-based) option */}
							<button
								onClick={() => { setLocalGradientStart(""); setLocalGradientEnd(""); setLocalDirty(true); }}
								className={cx(
									"flex items-center gap-2 rounded-md border px-3 py-1.5 text-sm transition-colors",
									!localGradientStart
										? "border-accent bg-accent/10 text-ink"
										: "border-app-line/50 text-ink-faint hover:border-app-line"
								)}
							>
								<span
									className="h-5 w-5 rounded-full shrink-0"
									style={{ background: `linear-gradient(135deg, ${seedC1}, ${seedC2})` }}
								/>
								Auto
							</button>
							{GRADIENT_PRESETS.map((preset) => (
								<button
									key={preset.label}
									onClick={() => { setLocalGradientStart(preset.start); setLocalGradientEnd(preset.end); setLocalDirty(true); }}
									className={cx(
										"flex items-center gap-2 rounded-md border px-3 py-1.5 text-sm transition-colors",
										localGradientStart === preset.start && localGradientEnd === preset.end
											? "border-accent bg-accent/10 text-ink"
											: "border-app-line/50 text-ink-faint hover:border-app-line"
									)}
								>
									<span
										className="h-5 w-5 rounded-full shrink-0"
										style={{ background: `linear-gradient(135deg, ${preset.start}, ${preset.end})` }}
									/>
									{preset.label}
								</button>
							))}
						</div>
						{/* Live preview */}
						<div className="mt-2 flex items-center gap-3">
							<span className="text-tiny text-ink-faint">Preview:</span>
							<span
								className="h-6 w-20 rounded"
								style={{ background: `linear-gradient(135deg, ${previewC1}, ${previewC2})` }}
							/>
						</div>
					</div>
				</div>
			</div>
		</>
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

const SANDBOX_DEFAULTS = { mode: "enabled" as const, writable_paths: [] as string[] };

function ConfigSectionEditor({ sectionId, label, description, detail, config, onDirtyChange, saveHandlerRef, onSave }: ConfigSectionEditorProps) {
	type ConfigValues = Record<string, string | number | boolean | string[]>;
	const sandbox = config.sandbox ?? SANDBOX_DEFAULTS;
	const channel = config.channel ?? { listen_only_mode: false };
	const [localValues, setLocalValues] = useState<ConfigValues>(() => {
		// Initialize from config based on section
		switch (sectionId) {
			case "routing":
				return { ...config.routing } as ConfigValues;
			case "tuning":
				return { ...config.tuning } as ConfigValues;
			case "compaction":
				return { ...config.compaction } as ConfigValues;
			case "cortex":
				return { ...config.cortex } as ConfigValues;
			case "coalesce":
				return { ...config.coalesce } as ConfigValues;
			case "memory":
				return { ...config.memory_persistence } as ConfigValues;
			case "browser":
				return { ...config.browser } as ConfigValues;
			case "channel":
				return { ...channel } as ConfigValues;
			case "sandbox":
				return { mode: sandbox.mode, writable_paths: sandbox.writable_paths } as ConfigValues;
			case "projects":
				return { ...config.projects } as ConfigValues;
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
				case "channel":
					setLocalValues({ ...channel });
					break;
				case "sandbox":
					setLocalValues({ mode: sandbox.mode, writable_paths: sandbox.writable_paths });
					break;
				case "projects":
					setLocalValues({ ...config.projects });
					break;
			}
		}
	}, [config, sectionId, localDirty]);

	const handleChange = useCallback((field: string, value: string | number | boolean | string[]) => {
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
			case "channel":
				setLocalValues({ ...channel });
				break;
			case "sandbox":
				setLocalValues({ mode: sandbox.mode, writable_paths: sandbox.writable_paths });
				break;
			case "projects":
				setLocalValues({ ...config.projects });
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
					{ key: "voice", label: "Voice Model", description: "Model for transcribing audio attachments" },
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
									capability={key === "voice" ? "voice_transcription" : undefined}
								/>
								{supportsAdaptiveThinking(localValues[key] as string) && (
									<div className="ml-4 flex flex-col gap-1">
										<label className="text-xs font-medium text-ink-dull">Thinking Effort</label>
										<Select
											value={(localValues[`${key}_thinking_effort`] as string) || "auto"}
											onValueChange={(value) => handleChange(`${key}_thinking_effort`, value)}
										>
											<SelectTrigger className="border-app-line/50 bg-app-darkBox/30">
												<SelectValue />
											</SelectTrigger>
											<SelectContent>
												<SelectItem value="auto">Auto</SelectItem>
												<SelectItem value="max">Max</SelectItem>
												<SelectItem value="high">High</SelectItem>
												<SelectItem value="medium">Medium</SelectItem>
												<SelectItem value="low">Low</SelectItem>
											</SelectContent>
										</Select>
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
						<ConfigToggleField
							label="Persist Session"
							description="Keep the browser alive across worker lifetimes. Cookies, tabs, and login sessions survive between tasks. Requires agent restart to take effect."
							value={localValues.persist_session as boolean}
							onChange={(v) => handleChange("persist_session", v)}
						/>
						<div className="flex flex-col gap-1.5">
							<label className="text-sm font-medium text-ink">Close Policy</label>
							<p className="text-tiny text-ink-faint">What happens when a worker calls &quot;close&quot; or finishes.</p>
							<Select
								value={localValues.close_policy as string}
								onValueChange={(v) => handleChange("close_policy", v)}
							>
								<SelectTrigger className="border-app-line/50 bg-app-darkBox/30">
									<SelectValue />
								</SelectTrigger>
								<SelectContent>
									<SelectItem value="close_browser">Close Browser</SelectItem>
									<SelectItem value="close_tabs">Close Tabs</SelectItem>
									<SelectItem value="detach">Detach</SelectItem>
								</SelectContent>
							</Select>
						</div>
					</div>
				);
			case "sandbox":
				return (
					<div className="grid gap-4">
						<div className="flex flex-col gap-1.5">
							<label className="text-sm font-medium text-ink">Mode</label>
							<p className="text-tiny text-ink-faint">Kernel-enforced filesystem containment for shell subprocesses.</p>
							<Select
								value={localValues.mode as string}
								onValueChange={(v) => handleChange("mode", v)}
							>
								<SelectTrigger className="border-app-line/50 bg-app-darkBox/30">
									<SelectValue />
								</SelectTrigger>
								<SelectContent>
									<SelectItem value="enabled">Enabled</SelectItem>
									<SelectItem value="disabled">Disabled</SelectItem>
								</SelectContent>
							</Select>
						</div>
						<div className="flex flex-col gap-1.5">
						<label className="text-sm font-medium text-ink">Extra Allowed Paths</label>
						<p className="text-tiny text-ink-faint">Additional directories workers can read and write beyond the workspace. The workspace is always accessible. Press Enter to add a path.</p>
							<TagInput
								value={(localValues.writable_paths as string[]) ?? []}
								onChange={(paths) => handleChange("writable_paths", paths)}
								placeholder="/home/user/projects/myapp"
							/>
						</div>
					</div>
				);
			case "channel":
				return (
					<div className="grid gap-4">
						<ConfigToggleField
							label="Listen-Only Mode"
							description="Only respond when explicitly invoked (slash command, @mention, or reply-to-bot)."
							value={localValues.listen_only_mode as boolean}
							onChange={(v) => handleChange("listen_only_mode", v)}
						/>
					</div>
				);
			case "projects":
				return (
					<div className="grid gap-4">
						<ConfigToggleField
							label="Use Worktrees"
							description="Enable git worktree support for parallel feature branches within projects."
							value={localValues.use_worktrees as boolean}
							onChange={(v) => handleChange("use_worktrees", v)}
						/>
						<div className="flex flex-col gap-1.5">
							<label className="text-sm font-medium text-ink">Worktree Name Template</label>
							<p className="text-tiny text-ink-faint">Template for naming new worktrees. Use <code className="text-ink-dull">{"{branch}"}</code> for the branch name.</p>
							<Input
								value={localValues.worktree_name_template as string}
								onChange={(e) => handleChange("worktree_name_template", e.target.value)}
								placeholder="{branch}"
								className="border-app-line/50 bg-app-darkBox/30"
							/>
						</div>
						<ConfigToggleField
							label="Auto-Create Worktrees"
							description="Automatically create a worktree when spawning a worker on a new branch."
							value={localValues.auto_create_worktrees as boolean}
							onChange={(v) => handleChange("auto_create_worktrees", v)}
						/>
						<ConfigToggleField
							label="Auto-Discover Repos"
							description="Scan the project root for git repositories when a project is created."
							value={localValues.auto_discover_repos as boolean}
							onChange={(v) => handleChange("auto_discover_repos", v)}
						/>
						<ConfigToggleField
							label="Auto-Discover Worktrees"
							description="Scan discovered repos for existing git worktrees on project creation."
							value={localValues.auto_discover_worktrees as boolean}
							onChange={(v) => handleChange("auto_discover_worktrees", v)}
						/>
						<NumberStepper
							label="Disk Usage Warning"
							description={`Warn when total project disk usage exceeds this threshold (${Math.round((localValues.disk_usage_warning_threshold as number) / 1073741824)} GB)`}
							value={localValues.disk_usage_warning_threshold as number}
							onChange={(v) => handleChange("disk_usage_warning_threshold", v)}
							min={0}
							step={1073741824}
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
