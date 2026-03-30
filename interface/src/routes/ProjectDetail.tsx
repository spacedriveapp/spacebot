import { useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { api, type CodeGraphProject } from "@/api/client";
import { Button } from "@/ui";
import {
	Dialog,
	DialogContent,
	DialogHeader,
	DialogTitle,
	DialogFooter,
	DialogDescription,
} from "@/ui/Dialog";
import { useSetTopBar } from "@/components/TopBar";
import { clsx } from "clsx";
import { CodeGraphTab } from "@/components/projects/CodeGraphTab";
import { ProjectMemoryTab } from "@/components/projects/ProjectMemoryTab";
import { ProjectSettingsTab } from "@/components/projects/ProjectSettingsTab";

// ---------------------------------------------------------------------------
// Sub-tab definitions
// ---------------------------------------------------------------------------

const TABS = [
	{ key: "overview", label: "Overview" },
	{ key: "code-graph", label: "Code Graph" },
	{ key: "memory", label: "Project Memory" },
	{ key: "settings", label: "Settings" },
] as const;

type TabKey = (typeof TABS)[number]["key"];

// ---------------------------------------------------------------------------
// Remove Project Dialog
// ---------------------------------------------------------------------------

function RemoveProjectDialog({
	project,
	open,
	onOpenChange,
}: {
	project: CodeGraphProject;
	open: boolean;
	onOpenChange: (open: boolean) => void;
}) {
	const queryClient = useQueryClient();
	const navigate = useNavigate();

	const { data: removeInfo } = useQuery({
		queryKey: ["codegraph-remove-info", project.project_id],
		queryFn: () => api.codegraphRemoveInfo(project.project_id),
		enabled: open,
	});

	const mutation = useMutation({
		mutationFn: () => api.codegraphDeleteProject(project.project_id),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["codegraph-projects"] });
			onOpenChange(false);
			navigate({ to: "/projects" });
		},
	});

	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent>
				<DialogHeader>
					<DialogTitle>Remove Project: {project.name}?</DialogTitle>
					<DialogDescription>This will permanently delete:</DialogDescription>
				</DialogHeader>
				<div className="flex flex-col gap-2 py-4 text-sm text-ink-dull">
					{removeInfo && (
						<>
							<p>Code graph index ({removeInfo.node_count.toLocaleString()} nodes, {removeInfo.edge_count.toLocaleString()} edges)</p>
							<p>{removeInfo.memory_count.toLocaleString()} project memories</p>
							<p>All index history and logs</p>
						</>
					)}
					<p className="mt-2 font-medium text-red-400">This cannot be undone.</p>
				</div>
				<DialogFooter>
					<Button variant="ghost" onClick={() => onOpenChange(false)}>
						Cancel
					</Button>
					<Button
						variant="destructive"
						onClick={() => mutation.mutate()}
						disabled={mutation.isPending}
					>
						{mutation.isPending ? "Removing..." : "Remove Project"}
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}

// ---------------------------------------------------------------------------
// Overview Tab
// ---------------------------------------------------------------------------

function OverviewTab({ project }: { project: CodeGraphProject }) {
	const queryClient = useQueryClient();

	const reindexMutation = useMutation({
		mutationFn: () => api.codegraphReindex(project.project_id),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["codegraph-project", project.project_id] });
		},
	});

	return (
		<div className="flex flex-col gap-6">
			{/* Basic info */}
			<div className="rounded-xl border border-app-line bg-app-darkBox p-5">
				<h3 className="mb-3 font-plex text-sm font-semibold text-ink">Project Info</h3>
				<dl className="grid grid-cols-2 gap-x-6 gap-y-3 text-sm">
					<div>
						<dt className="text-ink-faint">Name</dt>
						<dd className="text-ink">{project.name}</dd>
					</div>
					<div>
						<dt className="text-ink-faint">Root Path</dt>
						<dd className="truncate text-ink">{project.root_path}</dd>
					</div>
					<div>
						<dt className="text-ink-faint">Status</dt>
						<dd className="text-ink">{project.status}</dd>
					</div>
					<div>
						<dt className="text-ink-faint">Language</dt>
						<dd className="text-ink">{project.primary_language ?? "Detecting..."}</dd>
					</div>
					{project.last_indexed_at && (
						<div>
							<dt className="text-ink-faint">Last Indexed</dt>
							<dd className="text-ink">{new Date(project.last_indexed_at).toLocaleString()}</dd>
						</div>
					)}
					<div>
						<dt className="text-ink-faint">Schema Version</dt>
						<dd className="text-ink">{project.schema_version}</dd>
					</div>
				</dl>
			</div>

			{/* Stats */}
			{project.last_index_stats && (
				<div className="rounded-xl border border-app-line bg-app-darkBox p-5">
					<h3 className="mb-3 font-plex text-sm font-semibold text-ink">Index Stats</h3>
					<div className="grid grid-cols-3 gap-4">
						{[
							["Files", project.last_index_stats.files_found],
							["Parsed", project.last_index_stats.files_parsed],
							["Symbols", project.last_index_stats.nodes_created],
							["Edges", project.last_index_stats.edges_created],
							["Communities", project.last_index_stats.communities_detected],
							["Processes", project.last_index_stats.processes_traced],
						].map(([label, value]) => (
							<div key={label as string} className="text-center">
								<p className="text-lg font-semibold text-ink">{(value as number).toLocaleString()}</p>
								<p className="text-xs text-ink-faint">{label as string}</p>
							</div>
						))}
					</div>
				</div>
			)}

			{/* Actions */}
			<div className="flex gap-3">
				<Button
					onClick={() => reindexMutation.mutate()}
					disabled={reindexMutation.isPending || project.status === "indexing"}
				>
					{reindexMutation.isPending ? "Starting..." : "Re-index"}
				</Button>
			</div>
		</div>
	);
}

// ---------------------------------------------------------------------------
// Main Component
// ---------------------------------------------------------------------------

export function ProjectDetail({ projectId, initialTab }: { projectId: string; initialTab?: string }) {
	const [activeTab, setActiveTab] = useState<TabKey>((initialTab as TabKey) || "overview");
	const [removeOpen, setRemoveOpen] = useState(false);

	const { data, isLoading } = useQuery({
		queryKey: ["codegraph-project", projectId],
		queryFn: () => api.codegraphProject(projectId),
		refetchInterval: 5_000,
	});

	const project = data?.project;

	useSetTopBar(
		<div className="flex h-full items-center gap-4 px-6">
			<Link to="/projects" className="text-ink-faint hover:text-ink">
				Projects
			</Link>
			<span className="text-ink-faint">/</span>
			<h1 className="font-plex text-sm font-medium text-ink">
				{project?.name ?? projectId}
			</h1>
		</div>,
	);

	if (isLoading || !project) {
		return (
			<div className="flex flex-1 items-center justify-center">
				<p className="text-sm text-ink-faint">Loading project...</p>
			</div>
		);
	}

	return (
		<div className="flex h-full flex-col overflow-hidden">
			{/* Tab bar */}
			<div className="flex items-center justify-between border-b border-app-line px-6">
				<div className="flex">
					{TABS.map((tab) => (
						<button
							key={tab.key}
							onClick={() => setActiveTab(tab.key)}
							className={clsx(
								"border-b-2 px-4 py-3 text-sm font-medium transition-colors",
								activeTab === tab.key
									? "border-accent text-accent"
									: "border-transparent text-ink-dull hover:text-ink",
							)}
						>
							{tab.label}
						</button>
					))}
				</div>
				<Button
					variant="ghost"
					size="sm"
					className="text-red-400 hover:text-red-300"
					onClick={() => setRemoveOpen(true)}
				>
					Remove
				</Button>
			</div>

			{/* Tab content */}
			<div className="flex-1 overflow-y-auto p-6">
				{activeTab === "overview" && <OverviewTab project={project} />}
				{activeTab === "code-graph" && <CodeGraphTab projectId={projectId} />}
				{activeTab === "memory" && <ProjectMemoryTab projectId={projectId} />}
				{activeTab === "settings" && <ProjectSettingsTab projectId={projectId} />}
			</div>

			<RemoveProjectDialog
				project={project}
				open={removeOpen}
				onOpenChange={setRemoveOpen}
			/>
		</div>
	);
}
