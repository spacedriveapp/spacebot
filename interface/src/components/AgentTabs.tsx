import { Link, useMatchRoute } from "@tanstack/react-router";
import { motion } from "framer-motion";

const tabs = [
	{ label: "Overview", to: "/agents/$agentId" as const, exact: true },
	{ label: "Chat", to: "/agents/$agentId/chat" as const, exact: false },
	{ label: "Channels", to: "/agents/$agentId/channels" as const, exact: false },
	{ label: "Memories", to: "/agents/$agentId/memories" as const, exact: false },
	{ label: "Ingest", to: "/agents/$agentId/ingest" as const, exact: false },
	{ label: "Workers", to: "/agents/$agentId/workers" as const, exact: false },
	{ label: "Projects", to: "/agents/$agentId/projects" as const, exact: false },
	{ label: "Tasks", to: "/agents/$agentId/tasks" as const, exact: false },
	{ label: "Cortex", to: "/agents/$agentId/cortex" as const, exact: false },
	{ label: "Skills", to: "/agents/$agentId/skills" as const, exact: false },
	{ label: "Cron", to: "/agents/$agentId/cron" as const, exact: false },
	{ label: "Config", to: "/agents/$agentId/config" as const, exact: false },
];

export function AgentTabs({ agentId }: { agentId: string }) {
	const matchRoute = useMatchRoute();

	return (
		<div className="relative h-12 overflow-x-auto border-b border-app-line bg-app-darkBox/30 px-3 sm:px-6">
			<div className="flex h-full min-w-max items-stretch">
				{tabs.map((tab) => {
					const isActive = matchRoute({
						to: tab.to,
						params: { agentId },
						fuzzy: !tab.exact,
					});

					return (
						<Link
							key={tab.to}
							to={tab.to}
							params={{ agentId }}
							className={`relative flex items-center whitespace-nowrap px-3 text-sm transition-colors ${
								isActive ? "text-ink" : "text-ink-faint hover:text-ink-dull"
							}`}
						>
							{tab.label}
							{isActive && (
								<motion.div
									layoutId={`agent-tab-indicator-${agentId}`}
									className="absolute bottom-0 left-0 right-0 h-px bg-accent"
									transition={{ type: "spring", stiffness: 500, damping: 35 }}
								/>
							)}
						</Link>
					);
				})}
			</div>
		</div>
	);
}
