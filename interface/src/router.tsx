import {useState, useCallback} from "react";
import {
	createRouter,
	createRootRoute,
	createRoute,
	Outlet,
	useRouterState,
} from "@tanstack/react-router";
import {PageTransition} from "@/ui/PageTransition";
import {AnimatedTabContent} from "@/ui/AnimatedTabContent";
import {useKeyboardShortcuts} from "@/hooks/useKeyboardShortcuts";
import {BASE_PATH} from "@/api/client";
import {ConnectionBanner} from "@/components/ConnectionBanner";
import {SetupBanner} from "@/components/SetupBanner";
import {UpdateBanner} from "@/components/UpdateBanner";
import {Sidebar} from "@/components/Sidebar";
import {Overview} from "@/routes/Overview";
import {AgentDetail} from "@/routes/AgentDetail";
import {AgentChannels} from "@/routes/AgentChannels";
import {AgentCortex} from "@/routes/AgentCortex";
import {ChannelDetail} from "@/routes/ChannelDetail";
import {AgentMemories} from "@/routes/AgentMemories";
import {AgentConfig} from "@/routes/AgentConfig";
import {AgentCron} from "@/routes/AgentCron";
import {AgentIngest} from "@/routes/AgentIngest";
import {AgentSkills} from "@/routes/AgentSkills";
import {AgentChat} from "@/routes/AgentChat";
import {Settings} from "@/routes/Settings";
import {useLiveContext} from "@/hooks/useLiveContext";
import {AgentTabs} from "@/components/AgentTabs";

function RootLayout() {
	const {liveStates, connectionState, hasData} = useLiveContext();
	const [sidebarCollapsed, setSidebarCollapsed] = useState(true);
	const routeKey = useRouterState({
		select: (s) => {
			const p = s.location.pathname;
			const agentMatch = p.match(/^\/agents\/[^/]+/);
			return agentMatch ? agentMatch[0] : p;
		},
	});
	const toggleSidebar = useCallback(() => setSidebarCollapsed((c) => !c), []);
	useKeyboardShortcuts({ onToggleSidebar: toggleSidebar });

	return (
		<div className="flex h-screen bg-app">
			<Sidebar
				liveStates={liveStates}
				collapsed={sidebarCollapsed}
				onToggle={toggleSidebar}
			/>
			<div className="flex flex-1 flex-col overflow-hidden">
				<ConnectionBanner state={connectionState} hasData={hasData} />
				<UpdateBanner />
				<SetupBanner />
				<div className="flex-1 overflow-hidden">
					<PageTransition key={routeKey}>
						<Outlet />
					</PageTransition>
				</div>
			</div>
		</div>
	);
}

function AgentHeader({agentId}: {agentId: string}) {
	return (
		<>
			<header className="flex h-10 items-center border-b border-app-line bg-app-darkBox/50 px-6">
				<h1 className="font-plex text-sm font-medium text-ink">{agentId}</h1>
			</header>
			<AgentTabs agentId={agentId} />
		</>
	);
}

const rootRoute = createRootRoute({
	component: RootLayout,
});

const indexRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/",
	component: function IndexPage() {
		const {liveStates} = useLiveContext();
		return <Overview liveStates={liveStates} />;
	},
});

const settingsRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/settings",
	validateSearch: (search: Record<string, unknown>): {tab?: string} => {
		return {
			tab: typeof search.tab === "string" ? search.tab : undefined,
		};
	},
	component: function SettingsPage() {
		return <Settings />;
	},
});

const logsRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/logs",
	component: function LogsPage() {
		return (
			<div className="flex h-full flex-col">
				<header className="flex h-10 items-center border-b border-app-line bg-app-darkBox/50 px-6">
					<h1 className="font-plex text-sm font-medium text-ink">Logs</h1>
				</header>
				<div className="flex flex-1 items-center justify-center">
					<p className="text-sm text-ink-faint">Logs coming soon</p>
				</div>
			</div>
		);
	},
});

const agentRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/agents/$agentId",
	component: function AgentPage() {
		const {agentId} = agentRoute.useParams();
		const {liveStates} = useLiveContext();
		return (
			<div className="flex h-full flex-col">
				<AgentHeader agentId={agentId} />
				<div className="flex-1 overflow-hidden">
					<AnimatedTabContent tabKey="overview">
						<AgentDetail agentId={agentId} liveStates={liveStates} />
					</AnimatedTabContent>
				</div>
			</div>
		);
	},
});

const agentChatRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/agents/$agentId/chat",
	component: function AgentChatPage() {
		const {agentId} = agentChatRoute.useParams();
		return (
			<div className="flex h-full flex-col">
				<AgentHeader agentId={agentId} />
				<div className="flex-1 overflow-hidden">
					<AgentChat agentId={agentId} />
				</div>
			</div>
		);
	},
});

const agentChannelsRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/agents/$agentId/channels",
	component: function AgentChannelsPage() {
		const {agentId} = agentChannelsRoute.useParams();
		const {liveStates} = useLiveContext();
		return (
			<div className="flex h-full flex-col">
				<AgentHeader agentId={agentId} />
				<div className="flex-1 overflow-hidden">
					<AnimatedTabContent tabKey="channels">
						<AgentChannels agentId={agentId} liveStates={liveStates} />
					</AnimatedTabContent>
				</div>
			</div>
		);
	},
});

const agentMemoriesRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/agents/$agentId/memories",
	component: function AgentMemoriesPage() {
		const {agentId} = agentMemoriesRoute.useParams();
		return (
			<div className="flex h-full flex-col">
				<AgentHeader agentId={agentId} />
				<div className="flex-1 overflow-hidden">
					<AnimatedTabContent tabKey="memories">
						<AgentMemories agentId={agentId} />
					</AnimatedTabContent>
				</div>
			</div>
		);
	},
});

const agentIngestRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/agents/$agentId/ingest",
	component: function AgentIngestPage() {
		const {agentId} = agentIngestRoute.useParams();
		return (
			<div className="flex h-full flex-col">
				<AgentHeader agentId={agentId} />
				<div className="flex-1 overflow-hidden">
					<AnimatedTabContent tabKey="ingest">
						<AgentIngest agentId={agentId} />
					</AnimatedTabContent>
				</div>
			</div>
		);
	},
});

const agentWorkersRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/agents/$agentId/workers",
	component: function AgentWorkersPage() {
		const {agentId} = agentWorkersRoute.useParams();
		return (
			<div className="flex h-full flex-col">
				<AgentHeader agentId={agentId} />
				<div className="flex flex-1 items-center justify-center">
					<p className="text-sm text-ink-faint">
						Workers control interface coming soon
					</p>
				</div>
			</div>
		);
	},
});

const agentCronRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/agents/$agentId/cron",
	component: function AgentCronPage() {
		const {agentId} = agentCronRoute.useParams();
		return (
			<div className="flex h-full flex-col">
				<AgentHeader agentId={agentId} />
				<div className="flex-1 overflow-hidden">
					<AnimatedTabContent tabKey="cron">
						<AgentCron agentId={agentId} />
					</AnimatedTabContent>
				</div>
			</div>
		);
	},
});

const agentConfigRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/agents/$agentId/config",
	validateSearch: (search: Record<string, unknown>): {tab?: string} => {
		return {
			tab: typeof search.tab === "string" ? search.tab : undefined,
		};
	},
	component: function AgentConfigPage() {
		const {agentId} = agentConfigRoute.useParams();
		return (
			<div className="flex h-full flex-col">
				<AgentHeader agentId={agentId} />
				<div className="flex-1 overflow-hidden">
					<AnimatedTabContent tabKey="config">
						<AgentConfig agentId={agentId} />
					</AnimatedTabContent>
				</div>
			</div>
		);
	},
});

const agentCortexRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/agents/$agentId/cortex",
	component: function AgentCortexPage() {
		const {agentId} = agentCortexRoute.useParams();
		return (
			<div className="flex h-full flex-col">
				<AgentHeader agentId={agentId} />
				<div className="flex-1 overflow-hidden">
					<AnimatedTabContent tabKey="cortex">
						<AgentCortex agentId={agentId} />
					</AnimatedTabContent>
				</div>
			</div>
		);
	},
});

const agentSkillsRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/agents/$agentId/skills",
	component: function AgentSkillsPage() {
		const {agentId} = agentSkillsRoute.useParams();
		return (
			<div className="flex h-full flex-col">
				<AgentHeader agentId={agentId} />
				<div className="flex-1 overflow-hidden">
					<AnimatedTabContent tabKey="skills">
						<AgentSkills agentId={agentId} />
					</AnimatedTabContent>
				</div>
			</div>
		);
	},
});

const channelRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/agents/$agentId/channels/$channelId",
	component: function ChannelPage() {
		const {agentId, channelId} = channelRoute.useParams();
		const {liveStates, channels, loadOlderMessages} = useLiveContext();
		const channel = channels.find((c) => c.id === channelId);
		return (
			<div className="flex h-full flex-col">
				<AgentHeader agentId={agentId} />
				<div className="flex-1 overflow-hidden">
					<ChannelDetail
						agentId={agentId}
						channelId={channelId}
						channel={channel}
						liveState={liveStates[channelId]}
						onLoadMore={() => loadOlderMessages(channelId)}
					/>
				</div>
			</div>
		);
	},
});

const routeTree = rootRoute.addChildren([
	indexRoute,
	settingsRoute,
	logsRoute,
	agentRoute,
	agentChatRoute,
	agentChannelsRoute,
	agentMemoriesRoute,
	agentIngestRoute,
	agentWorkersRoute,
	agentCortexRoute,
	agentSkillsRoute,
	agentCronRoute,
	agentConfigRoute,
	channelRoute,
]);

export const router = createRouter({
	routeTree,
	basepath: BASE_PATH || "/",
});

declare module "@tanstack/react-router" {
	interface Register {
		router: typeof router;
	}
}
