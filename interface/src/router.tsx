import {
	createRouter,
	createRootRoute,
	createRoute,
	Outlet,
	useLocation,
} from "@tanstack/react-router";
import {BASE_PATH} from "@/api/client";
import {ConnectionBanner} from "@/components/ConnectionBanner";
import {Sidebar} from "@/components/Sidebar";
import {Overview} from "@/routes/Overview";
import {Dashboard} from "@/routes/Dashboard";
import {AgentDetail} from "@/routes/AgentDetail";
import {AgentChannels} from "@/routes/AgentChannels";
import {AgentCortex} from "@/routes/AgentCortex";
import {ChannelDetail} from "@/routes/ChannelDetail";
import {AgentMemories} from "@/routes/AgentMemories";
import {AgentConfig} from "@/routes/AgentConfig";
import {AgentCron} from "@/routes/AgentCron";

import {AgentSkills} from "@/routes/AgentSkills";
import {AgentWorkers} from "@/routes/AgentWorkers";
import {AgentProjects} from "@/routes/AgentProjects";
import {AgentTasks} from "@/routes/AgentTasks";
import {GlobalTasks} from "@/routes/GlobalTasks";
import {Wiki} from "@/routes/Wiki";
import {AgentChat} from "@/routes/AgentChat";
import {Settings} from "@/routes/Settings";
import {Workbench} from "@/routes/Workbench";
import {SpacedriveExplorer} from "@/routes/SpacedriveExplorer";
import {useLiveContext} from "@/hooks/useLiveContext";

// ── Root layout ──────────────────────────────────────────────────────────

function RootLayout() {
	const {liveStates, connectionState, hasData} = useLiveContext();
	const location = useLocation();
	const bare =
		location.pathname.startsWith("/workbench") ||
		location.pathname.startsWith("/dashboard") ||
		location.pathname.startsWith("/spacedrive");

	return (
		<div className="flex h-screen flex-col overflow-hidden bg-sidebar">
			<ConnectionBanner state={connectionState} hasData={hasData} />
			<div className="flex min-h-0 flex-1">
				<Sidebar liveStates={liveStates} />
				<div className="flex min-w-0 flex-1 flex-col overflow-hidden py-[10px] pr-[10px]">
					{bare ? (
						<Outlet />
					) : (
						<div className="flex min-w-0 flex-1 flex-col overflow-hidden rounded-2xl border border-app-line bg-app">
							<Outlet />
						</div>
					)}
				</div>
			</div>
		</div>
	);
}

// ── Routes ───────────────────────────────────────────────────────────────

const rootRoute = createRootRoute({
	component: RootLayout,
});

const indexRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/",
	component: function IndexPage() {
		const {liveStates, activeLinks} = useLiveContext();
		return <Overview liveStates={liveStates} activeLinks={activeLinks} />;
	},
});

const dashboardRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/dashboard",
	component: Dashboard,
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
			<div className="flex flex-1 items-center justify-center">
				<p className="text-sm text-ink-faint">Logs coming soon</p>
			</div>
		);
	},
});

const workbenchRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/workbench",
	component: function WorkbenchPage() {
		return <Workbench />;
	},
});

const tasksRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/tasks",
	component: function TasksPage() {
		return <GlobalTasks />;
	},
});

const wikiRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/wiki",
	component: function WikiPage() {
		return <Wiki />;
	},
});

const spacedriveRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/spacedrive",
	component: SpacedriveExplorer,
});

const agentRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/agents/$agentId",
	component: function AgentPage() {
		const {agentId} = agentRoute.useParams();
		const {liveStates} = useLiveContext();
		return <AgentDetail agentId={agentId} liveStates={liveStates} />;
	},
});

const agentChatRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/agents/$agentId/chat",
	component: function AgentChatPage() {
		const {agentId} = agentChatRoute.useParams();
		return <AgentChat agentId={agentId} />;
	},
});

const agentChannelsRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/agents/$agentId/channels",
	component: function AgentChannelsPage() {
		const {agentId} = agentChannelsRoute.useParams();
		const {liveStates} = useLiveContext();
		return <AgentChannels agentId={agentId} liveStates={liveStates} />;
	},
});

const agentMemoriesRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/agents/$agentId/memories",
	component: function AgentMemoriesPage() {
		const {agentId} = agentMemoriesRoute.useParams();
		return <AgentMemories agentId={agentId} />;
	},
});


const agentWorkersRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/agents/$agentId/workers",
	validateSearch: (search: Record<string, unknown>): {worker?: string} => ({
		worker: typeof search.worker === "string" ? search.worker : undefined,
	}),
	component: function AgentWorkersPage() {
		const {agentId} = agentWorkersRoute.useParams();
		return <AgentWorkers agentId={agentId} />;
	},
});

const projectsRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/projects",
	validateSearch: (search: Record<string, unknown>): {id?: string} => ({
		id: typeof search.id === "string" ? search.id : undefined,
	}),
	component: function ProjectsPage() {
		const {id} = projectsRoute.useSearch();
		return <AgentProjects projectId={id} />;
	},
});

const agentTasksRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/agents/$agentId/tasks",
	component: function AgentTasksPage() {
		const {agentId} = agentTasksRoute.useParams();
		return <AgentTasks agentId={agentId} />;
	},
});

const agentCronRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/agents/$agentId/cron",
	component: function AgentCronPage() {
		const {agentId} = agentCronRoute.useParams();
		return <AgentCron agentId={agentId} />;
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
		return <AgentConfig agentId={agentId} />;
	},
});

const agentCortexRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/agents/$agentId/cortex",
	component: function AgentCortexPage() {
		const {agentId} = agentCortexRoute.useParams();
		return <AgentCortex agentId={agentId} />;
	},
});

const agentSkillsRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/agents/$agentId/skills",
	component: function AgentSkillsPage() {
		const {agentId} = agentSkillsRoute.useParams();
		return <AgentSkills agentId={agentId} />;
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
			<ChannelDetail
				agentId={agentId}
				channelId={channelId}
				channel={channel}
				liveState={liveStates[channelId]}
				onLoadMore={() => loadOlderMessages(channelId)}
			/>
		);
	},
});

const routeTree = rootRoute.addChildren([
	indexRoute,
	dashboardRoute,
	settingsRoute,
	logsRoute,
	workbenchRoute,
	tasksRoute,
	wikiRoute,
	spacedriveRoute,
	agentRoute,
	agentChatRoute,
	agentChannelsRoute,
	agentMemoriesRoute,

	agentWorkersRoute,
	projectsRoute,
	agentTasksRoute,
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
