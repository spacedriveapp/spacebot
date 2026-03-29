import { useSetTopBar } from "@/components/TopBar";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/api/client";
import { useNavigate } from "@tanstack/react-router";

export function Welcome() {
	const navigate = useNavigate();

	useSetTopBar(
		<div className="flex h-full items-center px-6">
			<h1 className="font-plex text-sm font-medium text-ink">Welcome</h1>
		</div>,
	);

	const { data: providersData } = useQuery({
		queryKey: ["providers"],
		queryFn: api.providers,
		staleTime: 5_000,
	});

	const { data: agentsData } = useQuery({
		queryKey: ["agents"],
		queryFn: api.agents,
		staleTime: 10_000,
	});

	const hasProvider = providersData?.has_any ?? false;
	const isAnthropic = providersData?.providers?.anthropic ?? false;
	const agents = agentsData?.agents ?? [];
	const hasAgent = agents.length > 0;
	const firstAgentId = hasAgent ? agents[0].id : "";

	// Detect auth method
	const providerLabel = isAnthropic
		? "Anthropic (Claude Max / OAuth)"
		: hasProvider
			? "LLM provider configured"
			: null;

	return (
		<div className="flex flex-1 flex-col items-center justify-center overflow-y-auto">
			<div className="mx-auto w-full max-w-lg px-6 text-center">
				{/* Logo/Title */}
				<h1 className="font-plex text-5xl font-bold text-ink">
					Spacebot
				</h1>
				<p className="mt-3 text-lg text-ink-dull">
					Autonomous agent network for teams and communities
				</p>

				{/* Status Card */}
				<div className="mt-10 rounded-lg border border-app-line bg-app-box p-6 text-left">
					<h2 className="font-plex text-sm font-semibold text-ink">
						Getting Started
					</h2>

					<div className="mt-4 space-y-3">
						{/* Step 1: Provider */}
						<div className="flex items-center gap-3">
							<span
								className={`flex h-6 w-6 items-center justify-center rounded-full text-xs font-bold ${
									hasProvider
										? "bg-emerald-500/20 text-emerald-400"
										: "bg-accent/20 text-accent"
								}`}
							>
								{hasProvider ? "\u2713" : "1"}
							</span>
							<div className="flex-1">
								<p className="text-sm text-ink">
									{providerLabel ?? "Add an LLM provider"}
								</p>
								{!hasProvider && (
									<p className="mt-0.5 text-xs text-ink-faint">
										API key, Claude Max OAuth, or another provider
									</p>
								)}
							</div>
							{!hasProvider && (
								<button
									className="rounded-md bg-accent px-3 py-1 text-xs font-medium text-white hover:bg-accent-deep"
									onClick={() =>
										navigate({ to: "/settings", search: { tab: "providers" } })
									}
								>
									Configure
								</button>
							)}
						</div>

						{/* Step 2: Agent */}
						<div className="flex items-center gap-3">
							<span
								className={`flex h-6 w-6 items-center justify-center rounded-full text-xs font-bold ${
									hasAgent
										? "bg-emerald-500/20 text-emerald-400"
										: hasProvider
											? "bg-accent/20 text-accent"
											: "bg-app-line/50 text-ink-faint"
								}`}
							>
								{hasAgent ? "\u2713" : "2"}
							</span>
							<div className="flex-1">
								<p className={`text-sm ${hasAgent ? "text-ink" : "text-ink-dull"}`}>
									{hasAgent
										? `Agent active (${firstAgentId})`
										: "Create your first agent"}
								</p>
							</div>
							{hasProvider && hasAgent && (
								<a
									href={`/agents/${firstAgentId}`}
									className="rounded-md bg-app-button px-3 py-1 text-xs text-ink hover:bg-app-hover"
								>
									View
								</a>
							)}
						</div>

						{/* Step 3: Connect */}
						<div className="flex items-center gap-3">
							<span className="flex h-6 w-6 items-center justify-center rounded-full bg-app-line/50 text-xs font-bold text-ink-faint">
								3
							</span>
							<p className="flex-1 text-sm text-ink-dull">
								Connect a messaging platform or use the web chat
							</p>
						</div>
					</div>
				</div>

				{/* Quick Links */}
				<div className="mt-6 flex items-center justify-center gap-4">
					{hasAgent && (
						<a
							href={`/agents/${firstAgentId}/tasks`}
							className="rounded-md bg-accent px-4 py-2 text-sm font-medium text-white hover:bg-accent-deep"
						>
							Kanban Board
						</a>
					)}
					<button
						className="rounded-md bg-app-button px-4 py-2 text-sm text-ink hover:bg-app-hover"
						onClick={() => navigate({ to: "/settings" })}
					>
						Settings
					</button>
					<button
						className="rounded-md bg-app-button px-4 py-2 text-sm text-ink hover:bg-app-hover"
						onClick={() => navigate({ to: "/pricing" })}
					>
						Pricing
					</button>
				</div>

				{/* Tagline */}
				<p className="mt-8 text-xs text-ink-faint">
					Works with Claude Code, Codex, VS Code, and more.
					Open source. BYOK.
				</p>
			</div>
		</div>
	);
}
