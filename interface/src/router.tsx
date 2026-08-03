import {lazy} from "react";
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
import {TaskGraphView} from "@/routes/TaskGraphView";
import {Workflows} from "@/routes/Workflows";
import {WorkflowDetail} from "@/routes/WorkflowDetail";
import {WorkflowRunView} from "@/routes/WorkflowRunView";
import {Wiki} from "@/routes/Wiki";
import {AgentChat} from "@/routes/AgentChat";
import {Settings} from "@/routes/Settings";
import {Workbench} from "@/routes/Workbench";
import {useLiveContext} from "@/hooks/useLiveContext";

// ── Root layout ──────────────────────────────────────────────────────────

function RootLayout() {
	const {liveStates, connectionState, hasData} = useLiveContext();
	const location = useLocation();
	const bare = location.pathname.startsWith("/workbench") || location.pathname.startsWith("/dashboard");

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
	// `?task=<number>` opens a task's drawer directly. The drawer is the task UI
	// and it was previously reachable only by clicking a row, which left the run
	// view with nowhere to link a task to.
	validateSearch: (search: Record<string, unknown>): {task?: number} => {
		const raw = search.task;
		const parsed = typeof raw === "string" ? Number(raw) : raw;
		return typeof parsed === "number" && Number.isInteger(parsed) && parsed > 0
			? {task: parsed}
			: {};
	},
	component: function TasksPage() {
		const {task} = tasksRoute.useSearch();
		return <GlobalTasks initialTaskNumber={task} />;
	},
});

/**
 * One task's dependency graph.
 *
 * Keyed on the task number rather than on a run or a workflow, because that is
 * the only identifier that always exists: a graph can outlive the template it
 * was compiled from, and plenty of graphs were never compiled from one at all.
 * The number is what the drawer, the run view and every refusal already speak
 * in, so `/tasks?task=N` and `/tasks/N/graph` are two views of the same thing
 * and each links to the other.
 */
const taskGraphRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/tasks/$taskNumber/graph",
	component: function TaskGraphPage() {
		const {taskNumber} = taskGraphRoute.useParams();
		const parsed = Number(taskNumber);
		if (!Number.isInteger(parsed) || parsed <= 0) {
			return (
				<div className="flex flex-1 items-center justify-center">
					<p className="text-sm text-ink-faint">
						`{taskNumber}` is not a task number.
					</p>
				</div>
			);
		}
		return <TaskGraphView taskNumber={parsed} />;
	},
});

const workflowsRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/workflows",
	component: function WorkflowsPage() {
		return <Workflows />;
	},
});

const workflowDetailRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/workflows/$workflowId",
	component: function WorkflowDetailPage() {
		const {workflowId} = workflowDetailRoute.useParams();
		return <WorkflowDetail workflowId={workflowId} />;
	},
});

// Not nested under the workflow: a run outlives the template it came from, so
// the URL must not require one that may have been deleted.
const workflowRunRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/workflow-runs/$runId",
	component: function WorkflowRunPage() {
		const {runId} = workflowRunRoute.useParams();
		return <WorkflowRunView runId={runId} />;
	},
});

// Development-only visual harness for task components.
//
// Both the route *and* the import live inside the DEV branch, which is the
// only arrangement that actually keeps the module out of production. Guarding
// just the route entry leaves a static `import {UiLab}` at the top of this
// file running unconditionally, and it cannot be tree-shaken because building
// its fixtures is a top-level side effect — so it executed on every production
// page load and threw, taking the whole app down with it.
//
// Vite folds `import.meta.env.DEV` to `false` when building for production, so
// this collapses to `[]` and the dynamic import goes with it.
const devOnlyRoutes = import.meta.env.DEV
	? [
			createRoute({
				getParentRoute: () => rootRoute,
				path: "/__uilab",
				component: lazy(() =>
					import("@/routes/UiLab").then((m) => ({default: m.UiLab})),
				),
			}),
		]
	: [];

const wikiRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/wiki",
	component: function WikiPage() {
		return <Wiki />;
	},
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
	taskGraphRoute,
	workflowsRoute,
	workflowDetailRoute,
	workflowRunRoute,
	wikiRoute,
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
	...devOnlyRoutes,
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
