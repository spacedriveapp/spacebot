import { useState, useEffect, useCallback } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { api, type CodeGraphProject, type CodeGraphIndexStatus, type DirEntry } from "@/api/client";
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
import { useServer } from "@/hooks/useServer";

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
					<span>{formatNumber(project.last_index_stats.nodes_created)} nodes</span>
					<span>{formatNumber(project.last_index_stats.communities_detected)} communities</span>
					<span>{formatNumber(project.last_index_stats.files_found)} files</span>
				</div>
			)}

			{project.status === "error" && project.error_message && (
				<p className="mt-2 truncate text-xs text-red-400/80" title={project.error_message}>
					{project.error_message}
				</p>
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
// Native OS folder dialog (Tauri only)
// ---------------------------------------------------------------------------

async function openNativeFolderDialog(): Promise<string | null> {
	try {
		const { open } = await import("@tauri-apps/plugin-dialog");
		const selected = await open({
			directory: true,
			multiple: false,
			title: "Select Project Directory",
		});
		return typeof selected === "string" ? selected : null;
	} catch {
		return null;
	}
}

// ---------------------------------------------------------------------------
// Web fallback directory browser
// ---------------------------------------------------------------------------

function DirectoryBrowser({
	onSelect,
	onClose,
}: {
	onSelect: (path: string) => void;
	onClose: () => void;
}) {
	const [currentPath, setCurrentPath] = useState<string>("");
	const [entries, setEntries] = useState<DirEntry[]>([]);
	const [parentPath, setParentPath] = useState<string | null>(null);
	const [loading, setLoading] = useState(true);
	const [error, setError] = useState<string | null>(null);

	const loadDir = useCallback(async (path?: string) => {
		setLoading(true);
		setError(null);
		try {
			const result = await api.listDir(path);
			setCurrentPath(result.path);
			setParentPath(result.parent);
			setEntries(result.entries.filter((e) => e.is_dir));
		} catch (e) {
			setError(e instanceof Error ? e.message : "Failed to load directory");
		} finally {
			setLoading(false);
		}
	}, []);

	useEffect(() => {
		loadDir();
	}, [loadDir]);

	return (
		<div className="rounded-lg border border-app-line bg-app-darkBox">
			<div className="flex items-center gap-2 border-b border-app-line px-3 py-2">
				<button
					type="button"
					onClick={() => parentPath && loadDir(parentPath)}
					disabled={!parentPath}
					className="rounded px-1.5 py-0.5 text-xs text-ink-dull hover:bg-app-hover/40 disabled:opacity-30"
				>
					..
				</button>
				<span className="min-w-0 flex-1 truncate font-mono text-xs text-ink-dull">
					{currentPath}
				</span>
				<Button type="button" size="sm" onClick={() => onSelect(currentPath)}>
					Select
				</Button>
				<button
					type="button"
					onClick={onClose}
					className="rounded px-1.5 py-0.5 text-xs text-ink-faint hover:text-ink"
				>
					&times;
				</button>
			</div>
			<div className="max-h-48 overflow-y-auto">
				{loading && (
					<div className="px-3 py-4 text-center text-xs text-ink-faint">Loading...</div>
				)}
				{error && (
					<div className="px-3 py-4 text-center text-xs text-red-400">{error}</div>
				)}
				{!loading && !error && entries.length === 0 && (
					<div className="px-3 py-4 text-center text-xs text-ink-faint">No subdirectories</div>
				)}
				{!loading &&
					!error &&
					entries.map((entry) => (
						<button
							key={entry.path}
							type="button"
							onClick={() => loadDir(entry.path)}
							className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-ink hover:bg-app-hover/40"
						>
							<span className="text-xs text-accent">&#128193;</span>
							<span className="truncate">{entry.name}</span>
						</button>
					))}
			</div>
		</div>
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
	const { isTauri } = useServer();
	const [name, setName] = useState("");
	const [rootPath, setRootPath] = useState("");
	const [showBrowser, setShowBrowser] = useState(false);

	const mutation = useMutation({
		mutationFn: () => api.codegraphCreateProject(name, rootPath),
		onError: (err) => {
			console.error("Failed to create codegraph project:", err);
		},
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["codegraph-projects"] });
			onOpenChange(false);
			setName("");
			setRootPath("");
			setShowBrowser(false);
		},
	});

	const handleBrowse = async () => {
		if (isTauri) {
			const selected = await openNativeFolderDialog();
			if (selected) setRootPath(selected);
		} else {
			setShowBrowser((prev) => !prev);
		}
	};

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
						<div className="flex gap-2">
							<Input
								id="root-path"
								value={rootPath}
								onChange={(e) => setRootPath(e.target.value)}
								placeholder="/path/to/project"
								className="flex-1 font-mono"
							/>
							<Button
								type="button"
								variant="outline"
								onClick={handleBrowse}
								title="Browse for directory"
							>
								Browse
							</Button>
						</div>
						{showBrowser && !isTauri && (
							<div className="mt-2">
								<DirectoryBrowser
									onSelect={(path) => {
										setRootPath(path);
										setShowBrowser(false);
									}}
									onClose={() => setShowBrowser(false)}
								/>
							</div>
						)}
					</div>
				</div>
				{mutation.isError && (
					<p className="text-sm text-red-400">
						Error: {mutation.error instanceof Error ? mutation.error.message : "Failed to create project"}
					</p>
				)}
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
		refetchInterval: (query) => {
			const hasIndexing = query.state.data?.projects?.some((p) => p.status === "indexing");
			return hasIndexing ? 2_000 : 10_000;
		},
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
