import { useQuery } from "@tanstack/react-query";
import { api } from "@/api/client";

export interface SpacebotConfig {
	// Routing
	workerModel: string;
	channelModel: string;
	cortexModel: string;

	// Tuning
	maxConcurrentWorkers: number;
	maxConcurrentBranches: number;

	// Projects
	useWorktrees: boolean;
	autoCreateWorktrees: boolean;

	// Messaging platforms
	enabledPlatforms: string[];

	// Auth
	hasProvider: boolean;
	isAnthropic: boolean;

	// Loading state
	isReady: boolean;
}

export function useSpacebotConfig(agentId: string): SpacebotConfig {
	const { data: configData } = useQuery({
		queryKey: ["agent-config", agentId],
		queryFn: () => api.agentConfig(agentId),
		staleTime: 60_000,
	});

	const { data: messagingData } = useQuery({
		queryKey: ["messaging-status"],
		queryFn: api.messagingStatus,
		staleTime: 30_000,
	});

	const { data: providersData } = useQuery({
		queryKey: ["providers"],
		queryFn: api.providers,
		staleTime: 30_000,
	});

	const enabledPlatforms: string[] = [];
	if (messagingData) {
		for (const inst of messagingData.instances ?? []) {
			if (inst.enabled && inst.platform) {
				enabledPlatforms.push(inst.platform);
			}
		}
	}

	return {
		workerModel: configData?.routing?.worker ?? "",
		channelModel: configData?.routing?.channel ?? "",
		cortexModel: configData?.routing?.cortex ?? "",
		maxConcurrentWorkers: configData?.tuning?.max_concurrent_workers ?? 2,
		maxConcurrentBranches: configData?.tuning?.max_concurrent_branches ?? 5,
		useWorktrees: configData?.projects?.use_worktrees ?? false,
		autoCreateWorktrees: configData?.projects?.auto_create_worktrees ?? false,
		enabledPlatforms,
		hasProvider: providersData?.has_any ?? false,
		isAnthropic: providersData?.providers?.anthropic ?? false,
		isReady: !!configData && !!providersData,
	};
}
