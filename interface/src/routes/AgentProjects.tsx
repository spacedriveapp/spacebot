import { useState, useEffect, useCallback } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import {
	api,
	type Project,
	type ProjectWorktreeWithRepo,
	type ProjectRepo,
	type CreateProjectRequest,
	type CreateWorktreeRequest,
	type DirEntry,
} from "@/api/client";
import { Badge, Button } from "@/ui";
import {
	Dialog,
	DialogContent,
	DialogHeader,
	DialogTitle,
	DialogFooter,
	DialogDescription,
} from "@/ui/Dialog";
import { Input, Label, TextArea } from "@/ui/Input";
import { formatTimeAgo } from "@/lib/format";
import { clsx } from "clsx";
import { AnimatePresence, motion } from "framer-motion";
import { useServer } from "@/hooks/useServer";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatBytes(bytes: number): string {
	if (bytes === 0) return "0 B";
	if (bytes < 1024) return `${bytes} B`;
	if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
	if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
	return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

const STATUS_DOT: Record<string, string> = {
	active: "bg-emerald-500",
	archived: "bg-ink-faint",
};

// ---------------------------------------------------------------------------
// Project Card (list view)
// ---------------------------------------------------------------------------

function ProjectCard({
	project,
	onClick,
}: {
	project: Project;
	onClick: () => void;
}) {
	return (
		<motion.button
			layout
			initial={{ opacity: 0, y: 8 }}
			animate={{ opacity: 1, y: 0 }}
			exit={{ opacity: 0, y: -8 }}
			onClick={onClick}
			className="w-full cursor-pointer rounded-xl border border-app-line bg-app-darkBox p-5 text-left transition-colors hover:border-accent/30"
		>
			<div className="flex items-start justify-between gap-3">
				<div className="flex min-w-0 items-center gap-3">
					{project.icon ? (
						<span className="text-xl leading-none">{project.icon}</span>
					) : (
						<span className="flex h-8 w-8 items-center justify-center rounded-lg bg-accent/10 text-sm text-accent">
							{project.name.charAt(0).toUpperCase()}
						</span>
					)}
					<div className="min-w-0">
						<div className="flex items-center gap-2">
							<h3 className="truncate font-plex text-sm font-semibold text-ink">
								{project.name}
							</h3>
							<span
								className={clsx(
									"h-2 w-2 rounded-full",
									STATUS_DOT[project.status] ?? STATUS_DOT.active,
								)}
							/>
						</div>
						{project.description && (
							<p className="mt-0.5 line-clamp-1 text-xs text-ink-dull">
								{project.description}
							</p>
						)}
					</div>
				</div>
			</div>

			{project.tags.length > 0 && (
				<div className="mt-3 flex flex-wrap gap-1.5">
					{project.tags.map((tag) => (
						<Badge key={tag} variant="outline" size="sm">
							{tag}
						</Badge>
					))}
				</div>
			)}

			<div className="mt-3 flex items-center gap-4 text-[11px] text-ink-faint">
				<span className="font-mono">{project.root_path}</span>
				<span className="ml-auto">{formatTimeAgo(project.updated_at)}</span>
			</div>
		</motion.button>
	);
}

// ---------------------------------------------------------------------------
// Create Project Dialog
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Directory Browser (web mode fallback)
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
				<Button
					type="button"
					size="sm"
					onClick={() => onSelect(currentPath)}
				>
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
					<div className="px-3 py-4 text-center text-xs text-ink-faint">
						Loading...
					</div>
				)}
				{error && (
					<div className="px-3 py-4 text-center text-xs text-red-400">
						{error}
					</div>
				)}
				{!loading && !error && entries.length === 0 && (
					<div className="px-3 py-4 text-center text-xs text-ink-faint">
						No subdirectories
					</div>
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
// Browse Button (Tauri native dialog or web directory browser)
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

function CreateProjectDialog({
	open,
	onOpenChange,
	agentId,
}: {
	open: boolean;
	onOpenChange: (open: boolean) => void;
	agentId: string;
}) {
	const queryClient = useQueryClient();
	const { isTauri } = useServer();
	const [name, setName] = useState("");
	const [rootPath, setRootPath] = useState("");
	const [description, setDescription] = useState("");
	const [icon, setIcon] = useState("");
	const [tagsRaw, setTagsRaw] = useState("");
	const [showBrowser, setShowBrowser] = useState(false);

	const createMutation = useMutation({
		mutationFn: (request: CreateProjectRequest) =>
			api.createProject(agentId, request),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["projects", agentId] });
			onOpenChange(false);
			setName("");
			setRootPath("");
			setDescription("");
			setIcon("");
			setTagsRaw("");
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

	const handleSubmit = (e: React.FormEvent) => {
		e.preventDefault();
		if (!name.trim() || !rootPath.trim()) return;
		const tags = tagsRaw
			.split(",")
			.map((t) => t.trim())
			.filter(Boolean);
		createMutation.mutate({
			name: name.trim(),
			root_path: rootPath.trim(),
			description: description.trim() || undefined,
			icon: icon.trim() || undefined,
			tags: tags.length > 0 ? tags : undefined,
			auto_discover: true,
		});
	};

	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent>
				<DialogHeader>
					<DialogTitle>Create Project</DialogTitle>
				<DialogDescription>
					Register a project directory — either a single repo or a
					directory containing multiple repos.
				</DialogDescription>
				</DialogHeader>
				<form onSubmit={handleSubmit} className="space-y-4">
					<div>
						<Label>Name</Label>
						<Input
							value={name}
							onChange={(e) => setName(e.target.value)}
							placeholder="my-project"
							autoFocus
						/>
					</div>
					<div>
						<Label>Root Path</Label>
						<div className="flex gap-2">
							<Input
								value={rootPath}
								onChange={(e) => setRootPath(e.target.value)}
								placeholder="/home/user/projects/my-project"
								className="flex-1 font-mono"
							/>
							<Button
								type="button"
								variant="outline"
								size="default"
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
					<div>
						<Label>Description (optional)</Label>
						<TextArea
							value={description}
							onChange={(e) => setDescription(e.target.value)}
							placeholder="What this project is about..."
							rows={2}
						/>
					</div>
					<div className="flex gap-3">
						<div className="flex-1">
							<Label>Icon (optional)</Label>
							<Input
								value={icon}
								onChange={(e) => setIcon(e.target.value)}
								placeholder="e.g. a single emoji"
								maxLength={4}
							/>
						</div>
						<div className="flex-[2]">
							<Label>Tags (comma-separated)</Label>
							<Input
								value={tagsRaw}
								onChange={(e) => setTagsRaw(e.target.value)}
								placeholder="rust, backend, api"
							/>
						</div>
					</div>
					<DialogFooter>
						<Button
							type="button"
							variant="outline"
							onClick={() => onOpenChange(false)}
						>
							Cancel
						</Button>
						<Button
							type="submit"
							disabled={!name.trim() || !rootPath.trim()}
							loading={createMutation.isPending}
						>
							Create
						</Button>
					</DialogFooter>
				</form>
			</DialogContent>
		</Dialog>
	);
}

// ---------------------------------------------------------------------------
// Create Worktree Dialog
// ---------------------------------------------------------------------------

function CreateWorktreeDialog({
	open,
	onOpenChange,
	agentId,
	projectId,
	repos,
}: {
	open: boolean;
	onOpenChange: (open: boolean) => void;
	agentId: string;
	projectId: string;
	repos: ProjectRepo[];
}) {
	const queryClient = useQueryClient();
	const [repoId, setRepoId] = useState(repos[0]?.id ?? "");
	const [branch, setBranch] = useState("");
	const [worktreeName, setWorktreeName] = useState("");

	// Reset form state when the dialog opens (repos list may have changed).
	useEffect(() => {
		if (open) {
			setRepoId(repos[0]?.id ?? "");
			setBranch("");
			setWorktreeName("");
		}
	}, [open, repos]);

	const createMutation = useMutation({
		mutationFn: (request: CreateWorktreeRequest) =>
			api.createProjectWorktree(agentId, projectId, request),
		onSuccess: () => {
			queryClient.invalidateQueries({
				queryKey: ["project", agentId, projectId],
			});
			onOpenChange(false);
			setBranch("");
			setWorktreeName("");
		},
	});

	const handleSubmit = (e: React.FormEvent) => {
		e.preventDefault();
		if (!repoId || !branch.trim()) return;
		createMutation.mutate({
			repo_id: repoId,
			branch: branch.trim(),
			worktree_name: worktreeName.trim() || undefined,
		});
	};

	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent>
				<DialogHeader>
					<DialogTitle>Create Worktree</DialogTitle>
					<DialogDescription>
						Create a new git worktree from a repo in this project.
					</DialogDescription>
				</DialogHeader>
				<form onSubmit={handleSubmit} className="space-y-4">
					<div>
						<Label>Repository</Label>
						<select
							value={repoId}
							onChange={(e) => setRepoId(e.target.value)}
							className="h-8 w-full rounded-md border border-app-line bg-app-darkBox px-3 text-sm text-ink outline-none focus:border-accent/50"
						>
							{repos.map((r) => (
								<option key={r.id} value={r.id}>
									{r.name}
								</option>
							))}
						</select>
					</div>
					<div>
						<Label>Branch</Label>
						<Input
							value={branch}
							onChange={(e) => setBranch(e.target.value)}
							placeholder="feat/my-feature"
							autoFocus
						/>
					</div>
					<div>
						<Label>Directory Name (optional)</Label>
						<Input
							value={worktreeName}
							onChange={(e) => setWorktreeName(e.target.value)}
							placeholder="auto-derived from branch name"
						/>
					</div>
					<DialogFooter>
						<Button
							type="button"
							variant="outline"
							onClick={() => onOpenChange(false)}
						>
							Cancel
						</Button>
						<Button
							type="submit"
							disabled={!repoId || !branch.trim()}
							loading={createMutation.isPending}
						>
							Create
						</Button>
					</DialogFooter>
				</form>
			</DialogContent>
		</Dialog>
	);
}

// ---------------------------------------------------------------------------
// Delete Confirmation Dialog
// ---------------------------------------------------------------------------

function DeleteDialog({
	open,
	onOpenChange,
	title,
	description,
	onConfirm,
	isPending,
}: {
	open: boolean;
	onOpenChange: (open: boolean) => void;
	title: string;
	description: string;
	onConfirm: () => void;
	isPending: boolean;
}) {
	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent>
				<DialogHeader>
					<DialogTitle>{title}</DialogTitle>
					<DialogDescription>{description}</DialogDescription>
				</DialogHeader>
				<DialogFooter>
					<Button variant="outline" onClick={() => onOpenChange(false)}>
						Cancel
					</Button>
					<Button
						variant="destructive"
						onClick={onConfirm}
						loading={isPending}
					>
						Delete
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}

// ---------------------------------------------------------------------------
// Repo Card
// ---------------------------------------------------------------------------

function RepoCard({
	repo,
	worktreeCount,
	onAddWorktree,
	onDelete,
	isDeleting,
}: {
	repo: ProjectRepo;
	worktreeCount: number;
	onAddWorktree: () => void;
	onDelete: () => void;
	isDeleting: boolean;
}) {
	const isSingleRepo = repo.path === ".";

	return (
		<div className="rounded-lg border border-app-line bg-app-box p-4 transition-colors hover:border-app-line-hover">
			<div className="flex items-center justify-between gap-3">
				<div className="min-w-0 flex-1">
					<div className="flex items-center gap-2">
						<h4 className="truncate font-plex text-sm font-medium text-ink">
							{repo.name}
						</h4>
						{isSingleRepo && (
							<Badge variant="outline" size="sm">root</Badge>
						)}
					</div>
					<p className="mt-0.5 truncate font-mono text-[11px] text-ink-faint">
						{isSingleRepo ? "project root" : repo.path}
					</p>
				</div>
				<div className="flex shrink-0 items-center gap-1.5">
					<Button variant="outline" size="sm" onClick={onAddWorktree}>
						+ Worktree
					</Button>
					<Button
						variant="outline"
						size="icon"
						onClick={onDelete}
						disabled={isDeleting}
						className="h-6 w-6 text-red-500 hover:text-red-400"
						aria-label={`Delete repo ${repo.name}`}
					>
						<svg
							className="h-3 w-3"
							fill="none"
							viewBox="0 0 24 24"
							stroke="currentColor"
							strokeWidth={2}
						>
							<path
								strokeLinecap="round"
								strokeLinejoin="round"
								d="M19 7l-.867 12.142A2 2 0 0116.138 21H7.862a2 2 0 01-1.995-1.858L5 7m5 4v6m4-6v6m1-10V4a1 1 0 00-1-1h-4a1 1 0 00-1 1v3M4 7h16"
							/>
						</svg>
					</Button>
				</div>
			</div>
			<div className="mt-2 flex items-center gap-3 text-xs text-ink-faint">
				{repo.remote_url && (
					<span className="truncate">{repo.remote_url}</span>
				)}
				<Badge variant={repo.current_branch && repo.current_branch !== repo.default_branch ? "accent" : "outline"} size="sm">
					{repo.current_branch ?? repo.default_branch}
				</Badge>
				<span>{worktreeCount} worktree{worktreeCount !== 1 ? "s" : ""}</span>
				{repo.disk_usage_bytes != null && (
					<span className="ml-auto shrink-0 font-mono">
						{formatBytes(repo.disk_usage_bytes)}
					</span>
				)}
			</div>
		</div>
	);
}

// ---------------------------------------------------------------------------
// Worktree Card
// ---------------------------------------------------------------------------

function WorktreeCard({
	worktree,
	onDelete,
	isDeleting,
}: {
	worktree: ProjectWorktreeWithRepo;
	onDelete: () => void;
	isDeleting: boolean;
}) {
	return (
		<div className="rounded-lg border border-app-line bg-app-box p-4 transition-colors hover:border-app-line-hover">
			<div className="flex items-center justify-between gap-3">
				<div className="min-w-0 flex-1">
					<div className="flex items-center gap-2">
						<h4 className="truncate font-plex text-sm font-medium text-ink">
							{worktree.name}
						</h4>
						<Badge variant="accent" size="sm">
							{worktree.branch}
						</Badge>
						<Badge
							variant={worktree.created_by === "agent" ? "violet" : "default"}
							size="sm"
						>
							{worktree.created_by}
						</Badge>
					</div>
					<p className="mt-0.5 truncate text-xs text-ink-faint">
						from <span className="text-ink-dull">{worktree.repo_name}</span>
						{" \u00B7 "}
						<span className="font-mono">{worktree.path}</span>
						{worktree.disk_usage_bytes != null && (
							<>
								{" \u00B7 "}
								<span className="font-mono">
									{formatBytes(worktree.disk_usage_bytes)}
								</span>
							</>
						)}
					</p>
				</div>
				<Button
					variant="outline"
					size="icon"
					onClick={onDelete}
					disabled={isDeleting}
					className="h-6 w-6 text-red-500 hover:text-red-400"
					aria-label={`Delete worktree ${worktree.name}`}
				>
					<svg
						className="h-3 w-3"
						fill="none"
						viewBox="0 0 24 24"
						stroke="currentColor"
						strokeWidth={2}
					>
						<path
							strokeLinecap="round"
							strokeLinejoin="round"
							d="M19 7l-.867 12.142A2 2 0 0116.138 21H7.862a2 2 0 01-1.995-1.858L5 7m5 4v6m4-6v6m1-10V4a1 1 0 00-1-1h-4a1 1 0 00-1 1v3M4 7h16"
						/>
					</svg>
				</Button>
			</div>
		</div>
	);
}

// ---------------------------------------------------------------------------
// Project Detail View
// ---------------------------------------------------------------------------

function ProjectDetail({
	agentId,
	projectId,
	onBack,
}: {
	agentId: string;
	projectId: string;
	onBack: () => void;
}) {
	const queryClient = useQueryClient();

	const { data: project, isLoading } = useQuery({
		queryKey: ["project", agentId, projectId],
		queryFn: () => api.getProject(agentId, projectId),
		refetchInterval: 10_000,
	});

	const scanMutation = useMutation({
		mutationFn: () => api.scanProject(agentId, projectId),
		onSuccess: () => {
			queryClient.invalidateQueries({
				queryKey: ["project", agentId, projectId],
			});
		},
	});

	const deleteProjectMutation = useMutation({
		mutationFn: () => api.deleteProject(agentId, projectId),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["projects", agentId] });
			onBack();
		},
	});

	const deleteRepoMutation = useMutation({
		mutationFn: (repoId: string) =>
			api.deleteProjectRepo(agentId, projectId, repoId),
		onSuccess: () => {
			queryClient.invalidateQueries({
				queryKey: ["project", agentId, projectId],
			});
		},
	});

	const deleteWorktreeMutation = useMutation({
		mutationFn: (worktreeId: string) =>
			api.deleteProjectWorktree(agentId, projectId, worktreeId),
		onSuccess: () => {
			queryClient.invalidateQueries({
				queryKey: ["project", agentId, projectId],
			});
		},
	});

	const [showCreateWorktree, setShowCreateWorktree] = useState(false);
	const [showDeleteProject, setShowDeleteProject] = useState(false);
	const [deleteRepoTarget, setDeleteRepoTarget] = useState<string | null>(null);
	const [deleteWorktreeTarget, setDeleteWorktreeTarget] = useState<string | null>(null);
	// Track which repo's "Add Worktree" was clicked to pre-select in dialog
	const [worktreeRepoPreselect, setWorktreeRepoPreselect] = useState<string | null>(null);

	if (isLoading) {
		return (
			<div className="flex h-full items-center justify-center">
				<div className="h-6 w-6 animate-spin rounded-full border-2 border-accent border-t-transparent" />
			</div>
		);
	}

	if (!project) {
		return (
			<div className="flex h-full flex-col items-center justify-center gap-3">
				<p className="text-sm text-ink-faint">Project not found</p>
				<Button variant="outline" size="sm" onClick={onBack}>
					Back
				</Button>
			</div>
		);
	}

	const repos = project.repos ?? [];
	const worktrees = project.worktrees ?? [];

	const worktreeCountByRepo = (repoId: string) =>
		worktrees.filter((w) => w.repo_id === repoId).length;

	const totalDiskUsage =
		repos.reduce((sum, r) => sum + (r.disk_usage_bytes ?? 0), 0) +
		worktrees.reduce((sum, w) => sum + (w.disk_usage_bytes ?? 0), 0);

	return (
		<div className="h-full overflow-y-auto">
			<div className="mx-auto max-w-4xl space-y-6 p-6">
				{/* Header */}
				<div>
					<button
						onClick={onBack}
						className="mb-3 flex items-center gap-1 text-xs text-ink-faint transition-colors hover:text-ink-dull"
					>
						<svg
							className="h-3.5 w-3.5"
							fill="none"
							viewBox="0 0 24 24"
							stroke="currentColor"
							strokeWidth={2}
						>
							<path
								strokeLinecap="round"
								strokeLinejoin="round"
								d="M15 19l-7-7 7-7"
							/>
						</svg>
						All Projects
					</button>

					<div className="flex items-start justify-between gap-4">
						<div className="min-w-0 flex-1">
							<div className="flex items-center gap-3">
								{project.icon ? (
									<span className="text-2xl leading-none">
										{project.icon}
									</span>
								) : (
									<span className="flex h-10 w-10 items-center justify-center rounded-lg bg-accent/10 text-base font-semibold text-accent">
										{project.name.charAt(0).toUpperCase()}
									</span>
								)}
								<div>
									<h2 className="font-plex text-lg font-semibold text-ink">
										{project.name}
									</h2>
									{project.description && (
										<p className="mt-0.5 text-sm text-ink-dull">
											{project.description}
										</p>
									)}
								</div>
							</div>

							<div className="mt-3 flex flex-wrap items-center gap-2">
								<Badge
									variant={
										project.status === "active" ? "green" : "default"
									}
									size="sm"
								>
									{project.status}
								</Badge>
								{project.tags.map((tag) => (
									<Badge key={tag} variant="outline" size="sm">
										{tag}
									</Badge>
								))}
								<span className="font-mono text-[11px] text-ink-faint">
									{project.root_path}
								</span>
								{totalDiskUsage > 0 && (
									<span className="rounded-md bg-app-button px-2 py-0.5 font-mono text-[11px] text-ink-dull">
										{formatBytes(totalDiskUsage)}
									</span>
								)}
							</div>
						</div>

						<div className="flex shrink-0 items-center gap-2">
							<Button
								variant="outline"
								size="sm"
								onClick={() => scanMutation.mutate()}
								loading={scanMutation.isPending}
							>
								Scan
							</Button>
							<Button
								variant="outline"
								size="sm"
								className="text-red-500 hover:text-red-400"
								onClick={() => setShowDeleteProject(true)}
							>
								Delete
							</Button>
						</div>
					</div>
				</div>

				{/* Repos Section */}
				<section>
					<div className="mb-3 flex items-center justify-between">
						<h3 className="font-plex text-sm font-semibold text-ink">
							Repositories
							<span className="ml-2 text-ink-faint">({repos.length})</span>
						</h3>
					</div>
					{repos.length === 0 ? (
						<p className="rounded-lg border border-dashed border-app-line p-6 text-center text-sm text-ink-faint">
							No repositories discovered. Try scanning, or add one manually.
						</p>
					) : (
						<div className="space-y-2">
							{repos.map((repo) => (
								<RepoCard
									key={repo.id}
									repo={repo}
									worktreeCount={worktreeCountByRepo(repo.id)}
									onAddWorktree={() => {
										setWorktreeRepoPreselect(repo.id);
										setShowCreateWorktree(true);
									}}
									onDelete={() => setDeleteRepoTarget(repo.id)}
									isDeleting={deleteRepoMutation.isPending}
								/>
							))}
						</div>
					)}
				</section>

				{/* Worktrees Section */}
				<section>
					<div className="mb-3 flex items-center justify-between">
						<h3 className="font-plex text-sm font-semibold text-ink">
							Worktrees
							<span className="ml-2 text-ink-faint">
								({worktrees.length})
							</span>
						</h3>
						<Button
							variant="outline"
							size="sm"
							onClick={() => {
								setWorktreeRepoPreselect(null);
								setShowCreateWorktree(true);
							}}
							disabled={repos.length === 0}
						>
							+ Worktree
						</Button>
					</div>
					{worktrees.length === 0 ? (
						<p className="rounded-lg border border-dashed border-app-line p-6 text-center text-sm text-ink-faint">
							No worktrees. Create one to work on a feature branch.
						</p>
					) : (
						<div className="space-y-2">
							{worktrees.map((wt) => (
								<WorktreeCard
									key={wt.id}
									worktree={wt}
									onDelete={() => setDeleteWorktreeTarget(wt.id)}
									isDeleting={deleteWorktreeMutation.isPending}
								/>
							))}
						</div>
					)}
				</section>

				{/* Stats */}
				<div className="flex items-center gap-4 text-xs text-ink-faint">
					<span>Created {formatTimeAgo(project.created_at)}</span>
					<span>Updated {formatTimeAgo(project.updated_at)}</span>
				</div>
			</div>

			{/* Dialogs */}
			{repos.length > 0 && (
				<CreateWorktreeDialog
					open={showCreateWorktree}
					onOpenChange={setShowCreateWorktree}
					agentId={agentId}
					projectId={projectId}
					repos={
						worktreeRepoPreselect
							? [
									repos.find((r) => r.id === worktreeRepoPreselect)!,
									...repos.filter(
										(r) => r.id !== worktreeRepoPreselect,
									),
								]
							: repos
					}
				/>
			)}

			<DeleteDialog
				open={showDeleteProject}
				onOpenChange={setShowDeleteProject}
				title="Delete Project"
				description="This will remove the project record and all associated repos and worktrees from the database. Files on disk are not affected."
				onConfirm={() => deleteProjectMutation.mutate()}
				isPending={deleteProjectMutation.isPending}
			/>

			<DeleteDialog
				open={deleteRepoTarget !== null}
				onOpenChange={(open) => {
					if (!open) setDeleteRepoTarget(null);
				}}
				title="Remove Repository"
				description="This will unregister the repository from this project. Files on disk are not affected."
				onConfirm={() => {
					if (deleteRepoTarget) {
						deleteRepoMutation.mutate(deleteRepoTarget);
						setDeleteRepoTarget(null);
					}
				}}
				isPending={deleteRepoMutation.isPending}
			/>

			<DeleteDialog
				open={deleteWorktreeTarget !== null}
				onOpenChange={(open) => {
					if (!open) setDeleteWorktreeTarget(null);
				}}
				title="Remove Worktree"
				description="This will run `git worktree remove` and delete the worktree directory from disk."
				onConfirm={() => {
					if (deleteWorktreeTarget) {
						deleteWorktreeMutation.mutate(deleteWorktreeTarget);
						setDeleteWorktreeTarget(null);
					}
				}}
				isPending={deleteWorktreeMutation.isPending}
			/>
		</div>
	);
}

// ---------------------------------------------------------------------------
// Main Page
// ---------------------------------------------------------------------------

export function AgentProjects({ agentId }: { agentId: string }) {
	const [selectedProjectId, setSelectedProjectId] = useState<string | null>(
		null,
	);
	const [showCreate, setShowCreate] = useState(false);

	const { data, isLoading } = useQuery({
		queryKey: ["projects", agentId],
		queryFn: () => api.listProjects(agentId),
		refetchInterval: 15_000,
	});

	const projects = data?.projects ?? [];

	if (selectedProjectId) {
		return (
			<ProjectDetail
				agentId={agentId}
				projectId={selectedProjectId}
				onBack={() => setSelectedProjectId(null)}
			/>
		);
	}

	return (
		<div className="h-full overflow-y-auto">
			<div className="mx-auto max-w-4xl p-6">
				<div className="mb-6 flex items-center justify-between">
					<div>
						<h2 className="font-plex text-base font-semibold text-ink">
							Projects
						</h2>
						<p className="mt-0.5 text-xs text-ink-faint">
							Workspaces, repos, and worktrees this agent knows about.
						</p>
					</div>
					<Button size="sm" onClick={() => setShowCreate(true)}>
						+ Project
					</Button>
				</div>

				{isLoading ? (
					<div className="flex items-center justify-center py-20">
						<div className="h-6 w-6 animate-spin rounded-full border-2 border-accent border-t-transparent" />
					</div>
				) : projects.length === 0 ? (
					<div className="flex flex-col items-center justify-center rounded-xl border border-dashed border-app-line py-20">
						<p className="text-sm text-ink-faint">
							No projects registered yet.
						</p>
						<Button
							variant="outline"
							size="sm"
							className="mt-4"
							onClick={() => setShowCreate(true)}
						>
							Create your first project
						</Button>
					</div>
				) : (
					<div className="grid gap-3 sm:grid-cols-2">
						<AnimatePresence mode="popLayout">
							{projects.map((project) => (
								<ProjectCard
									key={project.id}
									project={project}
									onClick={() => setSelectedProjectId(project.id)}
								/>
							))}
						</AnimatePresence>
					</div>
				)}
			</div>

			<CreateProjectDialog
				open={showCreate}
				onOpenChange={setShowCreate}
				agentId={agentId}
			/>
		</div>
	);
}
