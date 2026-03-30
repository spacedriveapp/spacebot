import { useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { api, type CodeGraphProject, type CodeGraphIndexStatus } from "@/api/client";
import { Badge, Button } from "@/ui";
import {
	Dialog,
	DialogContent,
	DialogHeader,
	DialogTitle,
	DialogFooter,
	DialogDescription,
} from "@/ui/Dialog";
import { Input, Label } from "@/ui/Input";
import { clsx } from "clsx";
import { AnimatePresence, motion } from "framer-motion";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const STATUS_CONFIG: Record<
	CodeGraphIndexStatus,
	{ label: string; color: string; dot: string }
> = {
	indexed: { label: "Indexed", color: "text-emerald-400", dot: "bg-emerald-500" },
	indexing: { label: "Indexing", color: "text-blue-400", dot: "bg-blue-500" },
	stale: { label: "Stale", color: "text-amber-400", dot: "bg-amber-500" },
	error: { label: "Error", color: "text-red-400", dot: "bg-red-500" },
	pending: { label: "Pending", color: "text-ink-faint", dot: "bg-ink-faint" },
};

function StatusBadge({ status, progress }: { status: CodeGraphIndexStatus; progress?: CodeGraphProject["progress"] }) {
	const cfg = STATUS_CONFIG[status] ?? STATUS_CONFIG.pending;
	const label = status === "indexing" && progress
		? `Indexing ${progress.phase}`
		: cfg.label;

	return (
		<span className={clsx("inline-flex items-center gap-1.5 text-xs font-medium", cfg.color)}>
			<span className={clsx("h-1.5 w-1.5 rounded-full", cfg.dot)} />
			{label}
		</span>
	);
}

function formatNumber(n: number): string {
	if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
	return String(n);
}

// ---------------------------------------------------------------------------
// Project Card
// ---------------------------------------------------------------------------

function ProjectCard({ project }: { project: CodeGraphProject }) {
	return (
		<motion.div
			layout
			initial={{ opacity: 0, y: 8 }}
			animate={{ opacity: 1, y: 0 }}
			exit={{ opacity: 0, y: -8 }}
			className="rounded-xl border border-app-line bg-app-darkBox p-5 transition-colors hover:border-accent/30"
		>
			<div className="flex items-start justify-between gap-3">
				<div className="flex min-w-0 items-center gap-3">
					<span className="flex h-8 w-8 items-center justify-center rounded-lg bg-accent/10 text-sm text-accent">
						{project.name.charAt(0).toUpperCase()}
					</span>
					<div className="min-w-0">
						<h3 className="truncate font-plex text-sm font-semibold text-ink">
							{project.name}
						</h3>
						<p className="truncate text-xs text-ink-faint">{project.root_path}</p>
					</div>
				</div>
				<StatusBadge status={project.status} progress={project.progress} />
			</div>

			{/* Stats */}
			{project.last_index_stats && (
				<div className="mt-3 flex gap-4 text-xs text-ink-dull">
					<span>{formatNumber(project.last_index_stats.nodes_created)} symbols</span>
					<span>{formatNumber(project.last_index_stats.communities_detected)} communities</span>
					<span>{formatNumber(project.last_index_stats.files_found)} files</span>
				</div>
			)}

			{project.primary_language && (
				<div className="mt-2">
					<Badge variant="default" size="sm">{project.primary_language}</Badge>
				</div>
			)}

			{/* Actions */}
			<div className="mt-4 flex items-center gap-2">
				<Link
					to="/projects/$projectId"
					params={{ projectId: project.project_id }}
					className="text-xs font-medium text-accent hover:underline"
				>
					View Details
				</Link>
			</div>
		</motion.div>
	);
}

// ---------------------------------------------------------------------------
// Create Project Dialog
// ---------------------------------------------------------------------------

function CreateProjectDialog({
	open,
	onOpenChange,
}: {
	open: boolean;
	onOpenChange: (open: boolean) => void;
}) {
	const queryClient = useQueryClient();
	const [name, setName] = useState("");
	const [rootPath, setRootPath] = useState("");

	const mutation = useMutation({
		mutationFn: () => api.codegraphCreateProject(name, rootPath),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["codegraph-projects"] });
			onOpenChange(false);
			setName("");
			setRootPath("");
		},
	});

	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent>
				<DialogHeader>
					<DialogTitle>Add Project</DialogTitle>
					<DialogDescription>
						Add a project to index its code graph. Indexing starts automatically.
					</DialogDescription>
				</DialogHeader>
				<div className="flex flex-col gap-4 py-4">
					<div>
						<Label htmlFor="project-name">Project Name</Label>
						<Input
							id="project-name"
							value={name}
							onChange={(e) => setName(e.target.value)}
							placeholder="my-project"
						/>
					</div>
					<div>
						<Label htmlFor="root-path">Root Path</Label>
						<Input
							id="root-path"
							value={rootPath}
							onChange={(e) => setRootPath(e.target.value)}
							placeholder="/path/to/project"
						/>
					</div>
				</div>
				<DialogFooter>
					<Button
						variant="ghost"
						onClick={() => onOpenChange(false)}
					>
						Cancel
					</Button>
					<Button
						onClick={() => mutation.mutate()}
						disabled={!name || !rootPath || mutation.isPending}
					>
						{mutation.isPending ? "Adding..." : "Add Project"}
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}

// ---------------------------------------------------------------------------
// Main Component
// ---------------------------------------------------------------------------

export function GlobalProjects() {
	const [createOpen, setCreateOpen] = useState(false);
	const [statusFilter, setStatusFilter] = useState<CodeGraphIndexStatus | "all">("all");

	const { data, isLoading } = useQuery({
		queryKey: ["codegraph-projects"],
		queryFn: () => api.codegraphProjects(),
		refetchInterval: 10_000,
	});

	const projects = data?.projects ?? [];
	const filtered = statusFilter === "all"
		? projects
		: projects.filter((p) => p.status === statusFilter);

	return (
		<div className="flex h-full flex-col overflow-y-auto p-6">
			{/* Header */}
			<div className="mb-6 flex items-center justify-between">
				<div>
					<h2 className="font-plex text-lg font-semibold text-ink">Projects</h2>
					<p className="text-sm text-ink-faint">
						{projects.length} project{projects.length !== 1 ? "s" : ""} indexed
					</p>
				</div>
				<Button onClick={() => setCreateOpen(true)}>+ Add Project</Button>
			</div>

			{/* Filter */}
			<div className="mb-4 flex gap-2">
				{(["all", "indexed", "indexing", "stale", "error"] as const).map((s) => (
					<button
						key={s}
						onClick={() => setStatusFilter(s)}
						className={clsx(
							"rounded-md px-3 py-1 text-xs font-medium transition-colors",
							statusFilter === s
								? "bg-accent/20 text-accent"
								: "text-ink-dull hover:bg-app-selected/50",
						)}
					>
						{s === "all" ? "All" : s.charAt(0).toUpperCase() + s.slice(1)}
					</button>
				))}
			</div>

			{/* Grid */}
			{isLoading ? (
				<div className="flex flex-1 items-center justify-center">
					<p className="text-sm text-ink-faint">Loading projects...</p>
				</div>
			) : filtered.length === 0 ? (
				<div className="flex flex-1 flex-col items-center justify-center gap-3">
					<p className="text-sm text-ink-faint">
						{projects.length === 0
							? "No projects indexed yet"
							: "No projects match this filter"}
					</p>
					{projects.length === 0 && (
						<Button onClick={() => setCreateOpen(true)} variant="ghost">
							Add your first project
						</Button>
					)}
				</div>
			) : (
				<AnimatePresence mode="popLayout">
					<div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
						{filtered.map((project) => (
							<ProjectCard key={project.project_id} project={project} />
						))}
					</div>
				</AnimatePresence>
			)}

			<CreateProjectDialog open={createOpen} onOpenChange={setCreateOpen} />
		</div>
	);
}
