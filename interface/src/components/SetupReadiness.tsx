import {useMemo} from "react";
import {useQueries} from "@tanstack/react-query";
import {Link} from "@tanstack/react-router";
import {
	api,
	type McpAgentStatus,
	type MessagingStatusResponse,
	type ProvidersResponse,
	type SecretStoreStatus,
	type WarmupStatusResponse,
} from "@/api/client";
import {Banner, BannerActions, cx} from "@/ui";

type SetupSeverity = "blocker" | "warning" | "info";
type SettingsTab = "providers" | "secrets" | "channels" | "system-health";

interface SetupReadinessItem {
	id: string;
	severity: SetupSeverity;
	title: string;
	description: string;
	tab?: SettingsTab;
}

interface SetupReadinessState {
	items: SetupReadinessItem[];
	actionableItems: SetupReadinessItem[];
	blockerCount: number;
	warningCount: number;
	isLoading: boolean;
}

function pluralize(count: number, singular: string, plural = `${singular}s`) {
	return `${count} ${count === 1 ? singular : plural}`;
}

function formatAgentList(agentIds: string[]) {
	if (agentIds.length === 0) return "";
	if (agentIds.length === 1) return agentIds[0];
	if (agentIds.length === 2) return `${agentIds[0]} and ${agentIds[1]}`;
	return `${agentIds.slice(0, 2).join(", ")}, and ${agentIds.length - 2} more`;
}

export function classifySetupReadiness(params: {
	providers?: ProvidersResponse;
	secrets?: SecretStoreStatus;
	messaging?: MessagingStatusResponse;
	warmup?: WarmupStatusResponse;
	mcp?: McpAgentStatus[];
	probeErrors?: string[];
}): SetupReadinessItem[] {
	const items: SetupReadinessItem[] = [];
	const {providers, secrets, messaging, warmup, mcp, probeErrors} = params;

	if (probeErrors && probeErrors.length > 0) {
		items.push({
			id: "probe_error",
			severity: "warning",
			title: "Some readiness checks failed to load",
			description: `${pluralize(probeErrors.length, "check")} failed: ${probeErrors.join(", ")}. Existing readiness results may be incomplete until those probes recover.`,
		});
	}

	if (providers && !providers.has_any) {
		items.push({
			id: "provider",
			severity: "blocker",
			title: "No LLM provider configured",
			description: "Add a provider key or OAuth connection before Spacebot can do useful work.",
			tab: "providers",
		});
	}

	if (secrets?.state === "locked") {
		items.push({
			id: "secrets_locked",
			severity: "warning",
			title: "Secrets store is locked",
			description: "Unlock the secret store before editing or using encrypted instance secrets.",
			tab: "secrets",
		});
	} else if (
		secrets &&
		!secrets.platform_managed &&
		!secrets.encrypted &&
		secrets.secret_count > 0
	) {
		items.push({
			id: "secrets_unencrypted",
			severity: "warning",
			title: "Stored secrets are unencrypted",
			description: "Enable encryption so instance secrets are protected at rest.",
			tab: "secrets",
		});
	}

	if (providers?.has_any && warmup?.statuses.length) {
		const degradedAgents = warmup.statuses
			.filter((entry) => entry.status.state === "degraded")
			.map((entry) => entry.agent_id);
		const warmAgents = warmup.statuses.filter(
			(entry) => entry.status.state === "warm",
		);

		if (degradedAgents.length > 0) {
			items.push({
				id: "warmup_degraded",
				severity: "warning",
				title: "Warmup is degraded",
				description: `Warmup is degraded for ${formatAgentList(degradedAgents)}.`,
				tab: "system-health",
			});
		} else if (warmAgents.length === 0) {
			items.push({
				id: "warmup_pending",
				severity: "info",
				title: "Warmup is still settling",
				description: "Models and bulletin state are still warming up, so first responses may be slower.",
				tab: "system-health",
			});
		}
	}

	if (mcp) {
		const enabledServers = mcp.flatMap((agentStatus) =>
			agentStatus.servers
				.filter((server) => server.enabled)
				.map((server) => ({agent_id: agentStatus.agent_id, ...server})),
		);
		const failedServers = enabledServers.filter(
			(server) =>
				server.state !== "connected" && server.state !== "connecting",
		);
		const connectingServers = enabledServers.filter(
			(server) => server.state === "connecting",
		);

		if (failedServers.length > 0) {
			items.push({
				id: "mcp_failed",
				severity: "warning",
				title: "Some MCP servers are disconnected",
				description: `${pluralize(failedServers.length, "enabled MCP server")} ${failedServers.length === 1 ? "is" : "are"} not connected.`,
				tab: "system-health",
			});
		} else if (connectingServers.length > 0) {
			items.push({
				id: "mcp_connecting",
				severity: "info",
				title: "MCP servers are still connecting",
				description: `${pluralize(connectingServers.length, "enabled MCP server")} ${connectingServers.length === 1 ? "is" : "are"} still connecting.`,
				tab: "system-health",
			});
		}
	}

	if (messaging) {
		const configuredInstances = messaging.instances.filter(
			(instance) => instance.configured,
		);
		const boundInstances = configuredInstances.filter(
			(instance) => instance.binding_count > 0,
		);
		const hasUnboundConfiguredInstance = configuredInstances.some(
			(instance) => instance.binding_count === 0,
		);

		if (configuredInstances.length === 0) {
			items.push({
				id: "messaging_missing",
				severity: "info",
				title: "No messaging platforms configured",
				description: "Connect Discord, Telegram, Slack, or another adapter when you want Spacebot to handle real conversations.",
				tab: "channels",
			});
		} else if (hasUnboundConfiguredInstance) {
			items.push({
				id: "messaging_unbound",
				severity: "info",
				title: "Messaging is configured but not routed",
				description: boundInstances.length === 0
					? "Add bindings so configured platforms actually deliver conversations to an agent."
					: "Some configured platforms still need bindings before they deliver conversations to an agent.",
				tab: "channels",
			});
		}
	}

	return items;
}

export function useSetupReadiness(): SetupReadinessState {
	const [providersQuery, secretsQuery, messagingQuery, warmupQuery, mcpQuery] = useQueries({
		queries: [
			{
				queryKey: ["providers"],
				queryFn: api.providers,
				staleTime: 10_000,
			},
			{
				queryKey: ["secrets-status"],
				queryFn: api.secretsStatus,
				staleTime: 10_000,
				retry: false,
			},
			{
				queryKey: ["messaging-status"],
				queryFn: api.messagingStatus,
				staleTime: 10_000,
			},
			{
				queryKey: ["warmup-status"],
				queryFn: api.warmupStatus,
				staleTime: 10_000,
			},
			{
				queryKey: ["mcp-status"],
				queryFn: api.mcpStatus,
				staleTime: 10_000,
			},
		],
	});

	return useMemo(() => {
		const probeErrors = [
			["providers", providersQuery],
			["secrets", secretsQuery],
			["messaging", messagingQuery],
			["warmup", warmupQuery],
			["mcp", mcpQuery],
		]
			.filter(([, query]) => query.isError)
			.map(([label]) => label);
		const items = classifySetupReadiness({
			providers: providersQuery.data,
			secrets: secretsQuery.data,
			messaging: messagingQuery.data,
			warmup: warmupQuery.data,
			mcp: mcpQuery.data,
			probeErrors,
		});
		const actionableItems = items.filter((item) => item.severity !== "info");
		const blockerCount = items.filter((item) => item.severity === "blocker").length;
		const warningCount = items.filter((item) => item.severity === "warning").length;
		const isLoading = [providersQuery, secretsQuery, messagingQuery, warmupQuery, mcpQuery]
			.some((query) => query.isLoading && !query.isError && !query.data);

		return {
			items,
			actionableItems,
			blockerCount,
			warningCount,
			isLoading,
		};
	}, [mcpQuery, messagingQuery, providersQuery, secretsQuery, warmupQuery]);
}

export function SetupBanner() {
	const readiness = useSetupReadiness();

	if (readiness.isLoading || readiness.actionableItems.length === 0) {
		return null;
	}

	const primaryItem = readiness.actionableItems[0];
	const remainingCount = readiness.actionableItems.length - 1;
	const bannerVariant = readiness.blockerCount > 0 ? "error" : "warning";

	return (
		<Banner variant={bannerVariant} dot="static">
			<span className="font-medium">{primaryItem.title}.</span>
			<span className="opacity-80">{primaryItem.description}</span>
			{remainingCount > 0 && (
				<span className="opacity-80">
					{` ${pluralize(remainingCount, "more issue")} need attention.`}
				</span>
			)}
			<BannerActions>
				{primaryItem.tab ? (
					<Link
						to="/settings"
						search={{tab: primaryItem.tab}}
						className="underline underline-offset-2 hover:text-white"
					>
						Open Settings
					</Link>
				) : (
					<Link
						to="/"
						className="underline underline-offset-2 hover:text-white"
					>
						View Overview
					</Link>
				)}
			</BannerActions>
		</Banner>
	);
}

const severityStyles: Record<SetupSeverity, string> = {
	blocker: "border-red-500/20 bg-red-500/8 text-red-300",
	warning: "border-amber-500/20 bg-amber-500/8 text-amber-300",
	info: "border-blue-500/20 bg-blue-500/8 text-blue-300",
};

const severityLabelStyles: Record<SetupSeverity, string> = {
	blocker: "bg-red-500/15 text-red-300",
	warning: "bg-amber-500/15 text-amber-300",
	info: "bg-blue-500/15 text-blue-300",
};

export function SetupReadinessCard() {
	const readiness = useSetupReadiness();

	if (readiness.isLoading || readiness.items.length === 0) {
		return null;
	}

	return (
		<div className="border-b border-app-line bg-app-darkBox/35 px-6 py-4">
			<div className="mx-auto flex max-w-5xl flex-col gap-4 rounded-xl border border-app-line/70 bg-app-box/70 p-4">
				<div className="flex flex-col gap-1">
					<h2 className="font-plex text-sm font-semibold text-ink">Setup Readiness</h2>
					<p className="text-sm text-ink-dull">
						Spacebot surfaces setup and runtime readiness here in the control plane instead of through a separate doctor command.
					</p>
				</div>

				<div className="grid gap-3 md:grid-cols-2">
					{readiness.items.map((item) => (
						<div
							key={item.id}
							className={cx(
								"rounded-lg border p-3",
								severityStyles[item.severity],
							)}
						>
							<div className="flex items-start justify-between gap-3">
								<div className="flex min-w-0 flex-col gap-1">
									<div className="flex items-center gap-2">
										<span
											className={cx(
												"rounded-full px-2 py-0.5 text-[10px] font-semibold uppercase tracking-[0.12em]",
												severityLabelStyles[item.severity],
											)}
										>
											{item.severity}
										</span>
										<h3 className="text-sm font-medium text-ink">{item.title}</h3>
									</div>
									<p className="text-sm text-ink-dull">{item.description}</p>
								</div>
								{item.tab ? (
									<Link
										to="/settings"
										search={{tab: item.tab}}
										className="shrink-0 text-xs font-medium text-accent hover:text-accent/80"
									>
										Fix
									</Link>
								) : null}
							</div>
						</div>
					))}
				</div>
			</div>
		</div>
	);
}
