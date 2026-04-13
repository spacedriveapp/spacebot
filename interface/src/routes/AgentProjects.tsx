import {useState, useEffect, useCallback} from "react";
import {useNavigate} from "@tanstack/react-router";
import {ArrowLeft, PencilSimple, Trash, Plus, FolderSimple, Clock, DotsSixVertical} from "@phosphor-icons/react";
import {useQuery, useMutation, useQueryClient} from "@tanstack/react-query";
import {
	DndContext,
	closestCenter,
	KeyboardSensor,
	PointerSensor,
	useSensor,
	useSensors,
	type DragEndEvent,
} from "@dnd-kit/core";
import {
	arrayMove,
	SortableContext,
	sortableKeyboardCoordinates,
	useSortable,
	rectSortingStrategy,
} from "@dnd-kit/sortable";
import {CSS} from "@dnd-kit/utilities";
import {
	api,
	type Project,
	type ProjectWorktreeWithRepo,
	type ProjectRepo,
	type CreateProjectRequest,
	type CreateWorktreeRequest,
	type UpdateProjectRequest,
	getApiBase,
} from "@/api/client";
import {
	Badge,
	Button,
	Card,
	CircleButton,
	DialogRoot,
	DialogContent,
	DialogHeader,
	DialogTitle,
	DialogFooter,
	DialogDescription,
	Input,
	Label,
	TextArea,
} from "@spacedrive/primitives";
import {formatTimeAgo} from "@/lib/format";
import {clsx} from "clsx";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatBytes(bytes: number): string {
	if (bytes === 0) return "0 B";
	if (bytes < 1024) return `${bytes} B`;
	if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
	if (bytes < 1024 * 1024 * 1024)
		return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
	return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

// ---------------------------------------------------------------------------
// Project Card (sortable list view)
// ---------------------------------------------------------------------------

function SortableProjectCard({
	project,
	onClick,
}: {
	project: Project;
	onClick: () => void;
}) {
	const {attributes, listeners, setNodeRef, transform, transition, isDragging} = useSortable({
		id: project.id,
	});

	const logoUrl = project.logo_path
		? `${getApiBase()}/agents/projects/${encodeURIComponent(project.id)}/logo`
		: null;
	const fallback = project.icon || project.name.slice(0, 1).toUpperCase();

	return (
		<div
			ref={setNodeRef}
			style={{
				transform: CSS.Transform.toString(transform),
				transition,
				zIndex: isDragging ? 10 : undefined,
			}}
		>
			<Card
				variant="dark"
				className={`group relative flex h-[100px] cursor-pointer flex-col justify-between p-4 transition-colors hover:bg-app-hover/50 ${isDragging ? "opacity-50 shadow-lg" : ""}`}
				onClick={onClick}
			>
				{/* Drag handle — visible on hover */}
				<button
					type="button"
					className="absolute top-2 right-2 cursor-grab touch-none rounded p-0.5 text-ink-faint/0 transition-all group-hover:text-ink-faint/50 hover:!text-ink-faint active:cursor-grabbing"
					onClick={(e) => e.stopPropagation()}
					{...attributes}
					{...listeners}
				>
					<DotsSixVertical className="size-4" weight="bold" />
				</button>

				<div className="flex items-center gap-3">
					{logoUrl ? (
						<img
							src={logoUrl}
							alt=""
							className="size-8 shrink-0 rounded-md object-contain"
							draggable={false}
						/>
					) : (
						<div className="flex size-8 shrink-0 items-center justify-center rounded-md bg-sidebar-selected/40 text-sm font-semibold text-sidebar-ink">
							{fallback}
						</div>
					)}
					<div className="min-w-0 flex-1">
						<div className="flex items-center gap-2">
							<h3 className="truncate font-plex text-sm font-medium text-ink">
								{project.name}
							</h3>
							{project.tags.map((tag) => (
								<Badge key={tag} variant="outline" size="sm">
									{tag}
								</Badge>
							))}
						</div>
						{project.description && (
							<p className="mt-0.5 line-clamp-1 text-xs text-ink-dull">
								{project.description}
							</p>
						)}
					</div>
				</div>

				<div className="flex items-center gap-3 text-[11px] text-ink-faint">
					<span className="flex min-w-0 items-center gap-1">
						<FolderSimple className="size-3 shrink-0" weight="bold" />
						<span className="truncate font-mono">{project.root_path}</span>
					</span>
					<span className="ml-auto flex shrink-0 items-center gap-1">
						<Clock className="size-3" weight="bold" />
						{formatTimeAgo(project.updated_at)}
					</span>
				</div>
			</Card>
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
	const [name, setName] = useState("");
	const [rootPath, setRootPath] = useState("");
	const [description, setDescription] = useState("");
	const [icon, setIcon] = useState("");
	const [tagsRaw, setTagsRaw] = useState("");

	const createMutation = useMutation({
		mutationFn: (request: CreateProjectRequest) => api.createProject(request),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["projects"]});
			onOpenChange(false);
			setName("");
			setRootPath("");
			setDescription("");
			setIcon("");
			setTagsRaw("");
		},
	});

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
		<DialogRoot open={open} onOpenChange={onOpenChange}>
			<DialogContent>
				<DialogHeader>
					<DialogTitle>Create Project</DialogTitle>
					<DialogDescription>
						Register a project directory — either a single repo or a directory
						containing multiple repos.
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
						<Input
							value={rootPath}
							onChange={(e) => setRootPath(e.target.value)}
							placeholder="/home/user/projects/my-project"
							className="font-mono"
						/>
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
						>
							Create
						</Button>
					</DialogFooter>
				</form>
			</DialogContent>
		</DialogRoot>
	);
}

// ---------------------------------------------------------------------------
// Edit Project Dialog
// ---------------------------------------------------------------------------

function EditProjectDialog({
	open,
	onOpenChange,
	project,
}: {
	open: boolean;
	onOpenChange: (open: boolean) => void;
	project: Project;
}) {
	const queryClient = useQueryClient();
	const [name, setName] = useState(project.name);
	const [description, setDescription] = useState(project.description);
	const [icon, setIcon] = useState(project.icon);
	const [tagsRaw, setTagsRaw] = useState(project.tags.join(", "));
	const [logoPath, setLogoPath] = useState(project.logo_path ?? "");
	const [status, setStatus] = useState<"active" | "archived">(project.status);

	// Reset form state when the dialog opens or the project changes.
	useEffect(() => {
		if (open) {
			setName(project.name);
			setDescription(project.description);
			setIcon(project.icon);
			setTagsRaw(project.tags.join(", "));
			setLogoPath(project.logo_path ?? "");
			setStatus(project.status);
		}
	}, [open, project]);

	const updateMutation = useMutation({
		mutationFn: (request: UpdateProjectRequest) =>
			api.updateProject(project.id, request),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["project", project.id]});
			queryClient.invalidateQueries({queryKey: ["projects"]});
			onOpenChange(false);
		},
	});

	const handleSubmit = (e: React.FormEvent) => {
		e.preventDefault();
		if (!name.trim()) return;
		const tags = tagsRaw
			.split(",")
			.map((t) => t.trim())
			.filter(Boolean);
		updateMutation.mutate({
			name: name.trim(),
			description: description.trim(),
			icon: icon.trim(),
			tags,
			logo_path: logoPath.trim() || null,
			status,
		});
	};

	return (
		<DialogRoot open={open} onOpenChange={onOpenChange}>
			<DialogContent>
				<DialogHeader>
					<DialogTitle>Edit Project</DialogTitle>
					<DialogDescription>
						Update project metadata. Root path cannot be changed.
					</DialogDescription>
				</DialogHeader>
				<form onSubmit={handleSubmit} className="space-y-4">
					<div>
						<Label>Name</Label>
						<Input
							value={name}
							onChange={(e) => setName(e.target.value)}
							autoFocus
						/>
					</div>
					<div>
						<Label>Description</Label>
						<TextArea
							value={description}
							onChange={(e) => setDescription(e.target.value)}
							placeholder="What this project is about..."
							rows={2}
						/>
					</div>
					<div className="flex gap-3">
						<div className="flex-1">
							<Label>Icon</Label>
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
					<div>
						<Label>Logo Path (relative to project root)</Label>
						<Input
							value={logoPath}
							onChange={(e) => setLogoPath(e.target.value)}
							placeholder=".github/logo.png"
							className="font-mono"
						/>
						<p className="mt-1 text-tiny text-ink-faint">
							Leave blank to clear. Re-scan the project to auto-detect.
						</p>
					</div>
					<div>
						<Label>Status</Label>
						<div className="mt-1 flex gap-2">
							<button
								type="button"
								onClick={() => setStatus("active")}
								className={clsx(
									"rounded-md border px-3 py-1.5 text-xs font-medium transition-colors",
									status === "active"
										? "border-accent bg-accent/10 text-accent"
										: "border-app-line text-ink-dull hover:text-ink",
								)}
							>
								Active
							</button>
							<button
								type="button"
								onClick={() => setStatus("archived")}
								className={clsx(
									"rounded-md border px-3 py-1.5 text-xs font-medium transition-colors",
									status === "archived"
										? "border-accent bg-accent/10 text-accent"
										: "border-app-line text-ink-dull hover:text-ink",
								)}
							>
								Archived
							</button>
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
							disabled={!name.trim()}
						>
							Save
						</Button>
					</DialogFooter>
				</form>
			</DialogContent>
		</DialogRoot>
	);
}

// ---------------------------------------------------------------------------
// Create Worktree Dialog
// ---------------------------------------------------------------------------

function CreateWorktreeDialog({
	open,
	onOpenChange,
	projectId,
	repos,
}: {
	open: boolean;
	onOpenChange: (open: boolean) => void;
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
			api.createProjectWorktree(projectId, request),
		onSuccess: () => {
			queryClient.invalidateQueries({
				queryKey: ["project", projectId],
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
		<DialogRoot open={open} onOpenChange={onOpenChange}>
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
							className="h-8 w-full rounded-md border border-app-line bg-app-dark-box px-3 text-sm text-ink outline-none focus:border-accent/50"
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
						>
							Create
						</Button>
					</DialogFooter>
				</form>
			</DialogContent>
		</DialogRoot>
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
}: {
	open: boolean;
	onOpenChange: (open: boolean) => void;
	title: string;
	description: string;
	onConfirm: () => void;
	isPending?: boolean;
}) {
	return (
		<DialogRoot open={open} onOpenChange={onOpenChange}>
			<DialogContent>
				<DialogHeader>
					<DialogTitle>{title}</DialogTitle>
					<DialogDescription>{description}</DialogDescription>
				</DialogHeader>
				<DialogFooter>
					<Button variant="outline" onClick={() => onOpenChange(false)}>
						Cancel
					</Button>
					<Button variant="accent" onClick={onConfirm}>
						Delete
					</Button>
				</DialogFooter>
			</DialogContent>
		</DialogRoot>
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
							<Badge variant="outline" size="sm">
								root
							</Badge>
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
					<CircleButton
						icon={Trash}
						title={`Delete repo ${repo.name}`}
						onClick={onDelete}
						disabled={isDeleting}
					/>
				</div>
			</div>
			<div className="mt-2 flex items-center gap-3 text-xs text-ink-faint">
				{repo.remote_url && <span className="truncate">{repo.remote_url}</span>}
				<Badge
					variant={
						repo.current_branch && repo.current_branch !== repo.default_branch
							? "accent"
							: "outline"
					}
					size="sm"
				>
					{repo.current_branch ?? repo.default_branch}
				</Badge>
				<span>
					{worktreeCount} worktree{worktreeCount !== 1 ? "s" : ""}
				</span>
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
						<Badge variant="default" size="sm">
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
				<CircleButton
					icon={Trash}
					title={`Delete worktree ${worktree.name}`}
					onClick={onDelete}
					disabled={isDeleting}
				/>
			</div>
		</div>
	);
}

// ---------------------------------------------------------------------------
// Project Detail View
// ---------------------------------------------------------------------------

function ProjectDetail({
	projectId,
	onBack,
}: {
	projectId: string;
	onBack: () => void;
}) {
	const queryClient = useQueryClient();

	const {data: project, isLoading} = useQuery({
		queryKey: ["project", projectId],
		queryFn: () => api.getProject(projectId),
		refetchInterval: 10_000,
	});

	const deleteProjectMutation = useMutation({
		mutationFn: () => api.deleteProject(projectId),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["projects"]});
			onBack();
		},
	});

	const deleteRepoMutation = useMutation({
		mutationFn: (repoId: string) => api.deleteProjectRepo(projectId, repoId),
		onSuccess: () => {
			queryClient.invalidateQueries({
				queryKey: ["project", projectId],
			});
		},
	});

	const deleteWorktreeMutation = useMutation({
		mutationFn: (worktreeId: string) =>
			api.deleteProjectWorktree(projectId, worktreeId),
		onSuccess: () => {
			queryClient.invalidateQueries({
				queryKey: ["project", projectId],
			});
		},
	});

	const [showCreateWorktree, setShowCreateWorktree] = useState(false);
	const [showDeleteProject, setShowDeleteProject] = useState(false);
	const [showEditProject, setShowEditProject] = useState(false);
	const [deleteRepoTarget, setDeleteRepoTarget] = useState<string | null>(null);
	const [deleteWorktreeTarget, setDeleteWorktreeTarget] = useState<
		string | null
	>(null);
	// Track which repo's "Add Worktree" was clicked to pre-select in dialog
	const [worktreeRepoPreselect, setWorktreeRepoPreselect] = useState<
		string | null
	>(null);

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
		<div className="flex h-full flex-col">
			{/* Top Bar */}
			<div className="flex items-center justify-between border-b border-app-line px-4 py-2">
				<div className="flex items-center gap-2">
					<CircleButton icon={ArrowLeft} title="All Projects" onClick={onBack} />
					{(() => {
						const logoUrl = project.logo_path
							? `${getApiBase()}/agents/projects/${encodeURIComponent(project.id)}/logo`
							: null;
						const fallback = project.icon || project.name.slice(0, 1).toUpperCase();
						return logoUrl ? (
							<img
								src={logoUrl}
								alt=""
								className="size-[22px] shrink-0 rounded-md object-contain"
								draggable={false}
							/>
						) : (
							<div className="flex size-[22px] shrink-0 items-center justify-center rounded-md bg-sidebar-selected/40 text-[10px] font-semibold text-sidebar-ink">
								{fallback}
							</div>
						);
					})()}
					<h2 className="text-sm font-medium text-ink">{project.name}</h2>
					{project.tags.map((tag) => (
						<Badge key={tag} variant="outline" size="sm">
							{tag}
						</Badge>
					))}
					{totalDiskUsage > 0 && (
						<Badge variant="default" size="sm">
							{formatBytes(totalDiskUsage)}
						</Badge>
					)}
				</div>

				<div className="flex items-center gap-2">
					<CircleButton icon={PencilSimple} title="Edit" onClick={() => setShowEditProject(true)} />
					<CircleButton icon={Trash} title="Delete" onClick={() => setShowDeleteProject(true)} />
				</div>
			</div>

			<div className="flex-1 overflow-y-auto">
			<div className="mx-auto max-w-4xl space-y-6 p-6">
				{/* Project Info */}
				{project.description && (
					<p className="text-sm text-ink-dull">{project.description}</p>
				)}

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
							<span className="ml-2 text-ink-faint">({worktrees.length})</span>
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

				{/* Meta */}
				<div className="flex items-center gap-4 text-xs text-ink-faint">
					<span className="font-mono">{project.root_path}</span>
					<span>Created {formatTimeAgo(project.created_at)}</span>
					<span>Updated {formatTimeAgo(project.updated_at)}</span>
				</div>
			</div>
			</div>

			{/* Dialogs */}
			{repos.length > 0 && (
				<CreateWorktreeDialog
					open={showCreateWorktree}
					onOpenChange={setShowCreateWorktree}
					projectId={projectId}
					repos={
						worktreeRepoPreselect
							? [
									repos.find((r) => r.id === worktreeRepoPreselect)!,
									...repos.filter((r) => r.id !== worktreeRepoPreselect),
								]
							: repos
					}
				/>
			)}

			<EditProjectDialog
				open={showEditProject}
				onOpenChange={setShowEditProject}
				project={project}
			/>

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

export function AgentProjects({ projectId }: { projectId?: string }) {
	const navigate = useNavigate();
	const queryClient = useQueryClient();
	const selectedProjectId = projectId ?? null;

	const setSelectedProjectId = useCallback(
		(id: string | null) => {
			navigate({
				to: "/projects",
				search: id ? { id } : {},
			});
		},
		[navigate],
	);

	const [showCreate, setShowCreate] = useState(false);
	const [localProjects, setLocalProjects] = useState<Project[]>([]);

	const {data, isLoading} = useQuery({
		queryKey: ["projects"],
		queryFn: () => api.listProjects(),
		refetchInterval: 15_000,
	});

	// Sync local order state from server whenever fresh data arrives,
	// but only if we're not in the middle of a drag.
	useEffect(() => {
		setLocalProjects(data?.projects ?? []);
	}, [data]);

	const reorderMutation = useMutation({
		mutationFn: (ids: string[]) => api.reorderProjects(ids),
		onError: () => {
			// Revert to last known server state on failure.
			setLocalProjects(data?.projects ?? []);
		},
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["projects"]});
		},
	});

	const sensors = useSensors(
		useSensor(PointerSensor, {activationConstraint: {distance: 8}}),
		useSensor(KeyboardSensor, {coordinateGetter: sortableKeyboardCoordinates}),
	);

	const handleDragEnd = useCallback(
		(event: DragEndEvent) => {
			const {active, over} = event;
			if (!over || active.id === over.id) return;

			setLocalProjects((items) => {
				const oldIndex = items.findIndex((p) => p.id === active.id);
				const newIndex = items.findIndex((p) => p.id === over.id);
				const reordered = arrayMove(items, oldIndex, newIndex);
				reorderMutation.mutate(reordered.map((p) => p.id));
				return reordered;
			});
		},
		[reorderMutation],
	);

	if (selectedProjectId) {
		return (
			<ProjectDetail
				projectId={selectedProjectId}
				onBack={() => setSelectedProjectId(null)}
			/>
		);
	}

	return (
		<div className="flex h-full flex-col">
			{/* Top Bar */}
			<div className="flex items-center justify-between border-b border-app-line px-4 py-2">
				<div className="flex items-center gap-2">
					<h2 className="text-sm font-medium text-ink">Projects</h2>
					<span className="text-xs text-ink-faint">
						{localProjects.length} project{localProjects.length !== 1 ? "s" : ""}
					</span>
				</div>
				<Button variant="gray" size="md" onClick={() => setShowCreate(true)}>
					<Plus className="mr-1 size-3.5" weight="bold" />
					New Project
				</Button>
			</div>

			<div className="flex-1 overflow-y-auto">
				<div className="mx-auto max-w-4xl p-6">
					{isLoading ? (
						<div className="flex items-center justify-center py-20">
							<div className="h-6 w-6 animate-spin rounded-full border-2 border-accent border-t-transparent" />
						</div>
					) : localProjects.length === 0 ? (
						<div className="flex flex-col items-center justify-center rounded-lg border border-dashed border-app-line py-20">
							<FolderSimple className="mb-3 size-8 text-ink-faint" weight="bold" />
							<p className="text-sm text-ink-faint">
								No projects registered yet.
							</p>
							<Button
								variant="gray"
								size="md"
								className="mt-4"
								onClick={() => setShowCreate(true)}
							>
								Create your first project
							</Button>
						</div>
					) : (
						<DndContext
							sensors={sensors}
							collisionDetection={closestCenter}
							onDragEnd={handleDragEnd}
						>
							<SortableContext
								items={localProjects.map((p) => p.id)}
								strategy={rectSortingStrategy}
							>
								<div className="grid gap-3 sm:grid-cols-2">
									{localProjects.map((project) => (
										<SortableProjectCard
											key={project.id}
											project={project}
											onClick={() => setSelectedProjectId(project.id)}
										/>
									))}
								</div>
							</SortableContext>
						</DndContext>
					)}
				</div>
			</div>

			<CreateProjectDialog open={showCreate} onOpenChange={setShowCreate} />
		</div>
	);
}
