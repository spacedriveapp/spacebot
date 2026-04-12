import {useCallback, useEffect, useState, useRef} from "react";
import {useQuery, useMutation, useQueryClient} from "@tanstack/react-query";
import {useSearch, useNavigate} from "@tanstack/react-router";
import {
	api,
	type AgentConfigResponse,
	type AgentConfigUpdateRequest,
} from "@/api/client";
import {
	ConfigSidebar,
	GeneralEditor,
	IdentityEditor,
	ConfigSectionEditor,
	SaveBar,
	SECTIONS,
	isIdentityField,
	getIdentityField,
	type SectionId,
	type SaveHandlerRef,
} from "@/components/agent-config";
import {AgentIngest} from "@/routes/AgentIngest";

interface AgentConfigProps {
	agentId: string;
}

export function AgentConfig({agentId}: AgentConfigProps) {
	const queryClient = useQueryClient();
	const navigate = useNavigate();
	const search = useSearch({from: "/agents/$agentId/config"}) as {tab?: string};
	const [activeSection, setActiveSection] = useState<SectionId>("general");
	const [dirty, setDirty] = useState(false);
	const [saving, setSaving] = useState(false);
	const saveHandlerRef = useRef<SaveHandlerRef>({});

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
		navigate({
			to: "/agents/$agentId/config",
			params: {agentId},
			search: {tab: section},
		});
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
		mutationFn: (update: {
			display_name?: string;
			role?: string;
			gradient_start?: string;
			gradient_end?: string;
		}) => api.updateAgent(agentId, update),
		onMutate: () => setSaving(true),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["agents"]});
			setDirty(false);
			setSaving(false);
		},
		onError: () => setSaving(false),
	});

	const identityMutation = useMutation({
		mutationFn: (update: {
			field: "soul" | "identity" | "role";
			content: string;
		}) =>
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
		mutationFn: (update: AgentConfigUpdateRequest) =>
			api.updateAgentConfig(update),
		onMutate: (update) => {
			setSaving(true);
			const previous = queryClient.getQueryData<AgentConfigResponse>([
				"agent-config",
				agentId,
			]);
			if (previous) {
				const {agent_id: _, ...sections} = update;
				const merged = {...previous} as unknown as Record<string, unknown>;
				const prev = previous as unknown as Record<string, unknown>;
				for (const [key, value] of Object.entries(sections)) {
					if (value !== undefined) {
						merged[key] = {
							...(prev[key] as Record<string, unknown> | undefined),
							...value,
						};
					}
				}
				queryClient.setQueryData(
					["agent-config", agentId],
					merged as unknown as AgentConfigResponse,
				);
			}
		},
		onSuccess: (result) => {
			const previous = queryClient.getQueryData<AgentConfigResponse>([
				"agent-config",
				agentId,
			]);
			queryClient.setQueryData(["agent-config", agentId], {
				...previous,
				...result,
			});
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

	const isLoading =
		agentsQuery.isLoading || identityQuery.isLoading || configQuery.isLoading;
	const isError =
		agentsQuery.isError || identityQuery.isError || configQuery.isError;

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
	const isIngestSection = active.id === "ingest";
	const currentAgent = agentsQuery.data?.agents.find((a) => a.id === agentId);
	const identityData = identityQuery.data ?? {soul: null, identity: null, role: null};

	return (
		<div className="flex h-full relative">
			<ConfigSidebar
				activeSection={activeSection}
				onSectionChange={handleSectionChange}
				identityData={identityData}
			/>

			<div className="flex flex-1 flex-col overflow-hidden">
				{isIngestSection ? (
					<AgentIngest agentId={agentId} />
				) : isGeneralSection ? (
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
						content={getIdentityField(identityData, active.id)}
						onDirtyChange={setDirty}
						saveHandlerRef={saveHandlerRef}
						onSave={(content) => {
							if (isIdentityField(active.id)) {
								identityMutation.mutate({field: active.id, content});
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
						onSave={(update) =>
							configMutation.mutate({agent_id: agentId, ...update})
						}
					/>
				)}
			</div>

			{!isIngestSection && (
				<SaveBar
					dirty={dirty}
					saving={saving}
					onSave={handleSave}
					onRevert={handleRevert}
				/>
			)}
		</div>
	);
}
