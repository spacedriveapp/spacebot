import {useState, useRef, useCallback} from "react";
import {useQuery, useMutation, useQueryClient} from "@tanstack/react-query";
import {useNavigate, useSearch} from "@tanstack/react-router";
import {api, type FilesystemEntry, type IngestFileInfo} from "@/api/client";
import {formatTimeAgo} from "@/lib/format";
import {Button} from "@/ui";
import {
	Dialog,
	DialogContent,
	DialogHeader,
	DialogTitle,
	DialogFooter,
} from "@/ui";
import {Badge} from "@/ui";
import {clsx} from "clsx";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {
	faFolder,
	faFile,
	faDownload,
	faTrash,
	faPen,
	faPlus,
	faFolderPlus,
	faUpload,
	faChevronRight,
	faFloppyDisk,
	faXmark,
	faArrowLeft,
	faLock,
} from "@fortawesome/free-solid-svg-icons";

function formatFileSize(bytes: number): string {
	if (bytes < 1024) return `${bytes} B`;
	if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
	if (bytes < 1024 * 1024 * 1024)
		return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
	return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

const PROTECTED_ROOT_DIRS = ["ingest", "skills"];
const PROTECTED_IDENTITY_FILES = [
	"soul.md",
	"identity.md",
	"user.md",
	"role.md",
];

function isProtectedRootDir(currentPath: string, name: string): boolean {
	return (
		currentPath === "" &&
		PROTECTED_ROOT_DIRS.includes(name.toLowerCase())
	);
}

function isProtectedIdentityFile(currentPath: string, name: string): boolean {
	return (
		currentPath === "" &&
		PROTECTED_IDENTITY_FILES.includes(name.toLowerCase())
	);
}

function isTextFile(name: string): boolean {
	const textExtensions = [
		"txt",
		"md",
		"markdown",
		"json",
		"jsonl",
		"csv",
		"tsv",
		"xml",
		"yaml",
		"yml",
		"toml",
		"html",
		"htm",
		"css",
		"js",
		"ts",
		"tsx",
		"jsx",
		"rs",
		"py",
		"rb",
		"go",
		"java",
		"c",
		"cpp",
		"h",
		"hpp",
		"sh",
		"bash",
		"zsh",
		"fish",
		"sql",
		"graphql",
		"gql",
		"env",
		"ini",
		"cfg",
		"conf",
		"log",
		"rst",
		"org",
		"tex",
		"diff",
		"patch",
		"makefile",
		"dockerfile",
		"gitignore",
		"editorconfig",
	];
	const ext = name.split(".").pop()?.toLowerCase() ?? "";
	const baseName = name.toLowerCase();
	// Files with no extension or known text basenames
	return (
		textExtensions.includes(ext) ||
		textExtensions.includes(baseName) ||
		!name.includes(".")
	);
}

function fileExtension(name: string): string {
	const ext = name.split(".").pop()?.toUpperCase() ?? "";
	if (ext === name.toUpperCase() || ext.length > 5) return "";
	return ext;
}

// -- Ingest status components (reused from AgentIngest) --

function IngestStatusBadge({status}: {status: IngestFileInfo["status"]}) {
	const styles: Record<string, string> = {
		queued: "bg-amber-500/20 text-amber-400",
		processing: "bg-blue-500/20 text-blue-400",
		completed: "bg-green-500/20 text-green-400",
		failed: "bg-red-500/20 text-red-400",
	};
	return (
		<span
			className={`inline-flex items-center rounded-md px-2 py-0.5 text-xs font-medium ${styles[status] ?? styles.queued}`}
		>
			{(status === "processing" || status === "queued") && (
				<span
					className={`mr-1.5 h-1.5 w-1.5 animate-pulse rounded-full ${status === "queued" ? "bg-amber-400" : "bg-blue-400"}`}
				/>
			)}
			{status}
		</span>
	);
}

// -- Main component --

interface AgentFilesProps {
	agentId: string;
}

export function AgentFiles({agentId}: AgentFilesProps) {
	const queryClient = useQueryClient();
	const navigate = useNavigate();
	const search = useSearch({from: "/agents/$agentId/files"});
	const currentPath = search.path ?? "";

	const fileInputRef = useRef<HTMLInputElement>(null);
	const [selectedFile, setSelectedFile] = useState<string | null>(null);
	const [editMode, setEditMode] = useState(false);
	const [editContent, setEditContent] = useState("");
	const [renamingEntry, setRenamingEntry] = useState<string | null>(null);
	const [renameValue, setRenameValue] = useState("");
	const [createDialog, setCreateDialog] = useState<
		"file" | "folder" | null
	>(null);
	const [createName, setCreateName] = useState("");

	// Navigate to a directory path
	const navigateTo = useCallback(
		(path: string) => {
			setSelectedFile(null);
			setEditMode(false);
			navigate({
				to: "/agents/$agentId/files",
				params: {agentId},
				search: path ? {path} : {},
			});
		},
		[agentId, navigate],
	);

	// -- Queries --

	const {data: listing, isLoading, error} = useQuery({
		queryKey: ["files-list", agentId, currentPath],
		queryFn: () => api.filesystemList(agentId, currentPath),
		refetchInterval: 10_000,
	});

	const {data: filePreview, isLoading: previewLoading} = useQuery({
		queryKey: ["files-read", agentId, selectedFile],
		queryFn: () => api.filesystemRead(agentId, selectedFile!),
		enabled: !!selectedFile,
	});

	// Show ingest status when browsing the ingest/ directory
	const isIngestDir =
		currentPath === "ingest" || currentPath.startsWith("ingest/");
	const {data: ingestData} = useQuery({
		queryKey: ["ingest-files", agentId],
		queryFn: () => api.ingestFiles(agentId),
		refetchInterval: 5_000,
		enabled: isIngestDir,
	});

	// -- Mutations --

	const writeMutation = useMutation({
		mutationFn: ({
			path,
			content,
			isDirectory,
		}: {
			path: string;
			content?: string;
			isDirectory?: boolean;
		}) => api.filesystemWrite(agentId, path, content, isDirectory),
		onSuccess: () => {
			queryClient.invalidateQueries({
				queryKey: ["files-list", agentId, currentPath],
			});
			setCreateDialog(null);
			setCreateName("");
		},
	});

	const deleteMutation = useMutation({
		mutationFn: (path: string) => api.filesystemDelete(agentId, path),
		onSuccess: () => {
			queryClient.invalidateQueries({
				queryKey: ["files-list", agentId, currentPath],
			});
			if (selectedFile) setSelectedFile(null);
		},
	});

	const renameMutation = useMutation({
		mutationFn: ({
			oldPath,
			newPath,
		}: {
			oldPath: string;
			newPath: string;
		}) => api.filesystemRename(agentId, oldPath, newPath),
		onSuccess: () => {
			queryClient.invalidateQueries({
				queryKey: ["files-list", agentId, currentPath],
			});
			setRenamingEntry(null);
		},
	});

	const uploadMutation = useMutation({
		mutationFn: (files: File[]) =>
			api.filesystemUpload(agentId, currentPath, files),
		onSuccess: () => {
			queryClient.invalidateQueries({
				queryKey: ["files-list", agentId, currentPath],
			});
			if (isIngestDir) {
				queryClient.invalidateQueries({
					queryKey: ["ingest-files", agentId],
				});
			}
		},
	});

	const saveFileMutation = useMutation({
		mutationFn: ({path, content}: {path: string; content: string}) =>
			api.filesystemWrite(agentId, path, content),
		onSuccess: () => {
			queryClient.invalidateQueries({
				queryKey: ["files-read", agentId, selectedFile],
			});
			setEditMode(false);
		},
	});

	// -- Event handlers --

	const handleUpload = useCallback(
		(files: FileList | File[]) => {
			const fileArray = Array.from(files);
			if (fileArray.length > 0) {
				uploadMutation.mutate(fileArray);
			}
		},
		[uploadMutation],
	);

	const handleCreate = useCallback(() => {
		if (!createName.trim()) return;
		const fullPath = currentPath
			? `${currentPath}/${createName.trim()}`
			: createName.trim();
		writeMutation.mutate({
			path: fullPath,
			content: createDialog === "file" ? "" : undefined,
			isDirectory: createDialog === "folder",
		});
	}, [createDialog, createName, currentPath, writeMutation]);

	const handleRenameSubmit = useCallback(
		(entry: string) => {
			if (!renameValue.trim() || renameValue === entry) {
				setRenamingEntry(null);
				return;
			}
			const oldPath = currentPath ? `${currentPath}/${entry}` : entry;
			const newPath = currentPath
				? `${currentPath}/${renameValue.trim()}`
				: renameValue.trim();
			renameMutation.mutate({oldPath, newPath});
		},
		[currentPath, renameValue, renameMutation],
	);

	const handleEntryClick = useCallback(
		(entry: FilesystemEntry) => {
			if (entry.entry_type === "directory") {
				const path = currentPath
					? `${currentPath}/${entry.name}`
					: entry.name;
				navigateTo(path);
			} else if (isTextFile(entry.name)) {
				const path = currentPath
					? `${currentPath}/${entry.name}`
					: entry.name;
				setSelectedFile(path);
				setEditMode(false);
			}
		},
		[currentPath, navigateTo],
	);

	// -- Breadcrumb segments --

	const pathSegments = currentPath ? currentPath.split("/") : [];
	const breadcrumbs = [
		{label: "workspace", path: ""},
		...pathSegments.map((segment, index) => ({
			label: segment,
			path: pathSegments.slice(0, index + 1).join("/"),
		})),
	];

	const entries = listing?.entries ?? [];

	return (
		<div className="flex h-full flex-col">
			{/* Toolbar */}
			<div className="flex items-center gap-2 border-b border-app-line px-4 py-2">
				{/* Breadcrumb */}
				<div className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto text-sm">
					{breadcrumbs.map((crumb, index) => (
						<span key={crumb.path} className="flex items-center gap-1 whitespace-nowrap">
							{index > 0 && (
								<FontAwesomeIcon
									icon={faChevronRight}
									className="h-2.5 w-2.5 text-ink-faint"
								/>
							)}
							<button
								type="button"
								onClick={() => navigateTo(crumb.path)}
								className={clsx(
									"rounded px-1.5 py-0.5 transition-colors hover:bg-app-box/50",
									index === breadcrumbs.length - 1
										? "font-medium text-ink"
										: "text-ink-dull hover:text-ink",
								)}
							>
								{crumb.label}
							</button>
						</span>
					))}
				</div>

				{/* Actions */}
				<div className="flex items-center gap-1.5">
					<Button
						variant="ghost"
						size="sm"
						onClick={() => {
							setCreateDialog("file");
							setCreateName("");
						}}
					>
						<FontAwesomeIcon
							icon={faPlus}
							className="mr-1.5 h-3 w-3"
						/>
						New File
					</Button>
					<Button
						variant="ghost"
						size="sm"
						onClick={() => {
							setCreateDialog("folder");
							setCreateName("");
						}}
					>
						<FontAwesomeIcon
							icon={faFolderPlus}
							className="mr-1.5 h-3 w-3"
						/>
						New Folder
					</Button>
					<Button
						variant="ghost"
						size="sm"
						onClick={() => fileInputRef.current?.click()}
						loading={uploadMutation.isPending}
					>
						<FontAwesomeIcon
							icon={faUpload}
							className="mr-1.5 h-3 w-3"
						/>
						Upload
					</Button>
					<input
						ref={fileInputRef}
						type="file"
						multiple
						className="hidden"
						onChange={(e) => {
							if (e.target.files) {
								handleUpload(e.target.files);
								e.target.value = "";
							}
						}}
					/>
				</div>
			</div>

			{/* Main content area */}
			<div className="flex flex-1 overflow-hidden">
				{/* File listing */}
				<div
					className={clsx(
						"flex flex-col overflow-auto border-r border-app-line",
						selectedFile ? "w-1/2" : "w-full",
					)}
				>
					{/* Back row when in a subdirectory */}
					{currentPath && (
						<button
							type="button"
							onClick={() => {
								const parent = currentPath.includes("/")
									? currentPath.substring(
											0,
											currentPath.lastIndexOf("/"),
										)
									: "";
								navigateTo(parent);
							}}
							className="flex items-center gap-3 border-b border-app-line/50 px-4 py-2.5 text-sm text-ink-dull transition-colors hover:bg-app-box/20 hover:text-ink"
						>
							<FontAwesomeIcon
								icon={faArrowLeft}
								className="h-3 w-3 text-ink-faint"
							/>
							..
						</button>
					)}

					{/* Loading state */}
					{isLoading && (
						<div className="flex flex-1 items-center justify-center py-12">
							<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
						</div>
					)}

					{/* Error state */}
					{error && (
						<div className="m-4 rounded-xl bg-red-500/10 px-4 py-3 text-sm text-red-400">
							Failed to load directory contents
						</div>
					)}

					{/* Empty state */}
					{!isLoading && !error && entries.length === 0 && (
						<div className="flex flex-1 flex-col items-center justify-center py-12 text-center">
							<FontAwesomeIcon
								icon={faFolder}
								className="mb-3 h-8 w-8 text-ink-faint/50"
							/>
							<p className="text-sm text-ink-dull">
								Empty directory
							</p>
							<p className="mt-1 text-xs text-ink-faint">
								Create a file or upload one to get started
							</p>
						</div>
					)}

					{/* File entries */}
					{entries.map((entry) => {
						const entryPath = currentPath
							? `${currentPath}/${entry.name}`
							: entry.name;
						const isSelected = selectedFile === entryPath;
						const isDir = entry.entry_type === "directory";
						const protectedDir = isProtectedRootDir(
							currentPath,
							entry.name,
						);
						const protectedFile = isProtectedIdentityFile(
							currentPath,
							entry.name,
						);
						const isRenaming = renamingEntry === entry.name;

						return (
							<div
								key={entry.name}
								className={clsx(
									"group flex items-center gap-3 border-b border-app-line/30 px-4 py-2.5 transition-colors",
									isSelected
										? "bg-accent/5"
										: "hover:bg-app-box/20",
									isDir ? "cursor-pointer" : "",
								)}
								onClick={() => handleEntryClick(entry)}
								onKeyDown={(e) => {
									if (e.key === "Enter")
										handleEntryClick(entry);
								}}
								role="button"
								tabIndex={0}
							>
								{/* Icon */}
								<FontAwesomeIcon
									icon={isDir ? faFolder : faFile}
									className={clsx(
										"h-4 w-4 flex-shrink-0",
										isDir
											? "text-amber-400/70"
											: "text-ink-faint",
									)}
								/>

								{/* Name */}
								<div className="flex min-w-0 flex-1 items-center gap-2">
									{isRenaming ? (
										<input
											type="text"
											value={renameValue}
											onChange={(e) =>
												setRenameValue(e.target.value)
											}
											onBlur={() =>
												handleRenameSubmit(entry.name)
											}
											onKeyDown={(e) => {
												if (e.key === "Enter")
													handleRenameSubmit(
														entry.name,
													);
												if (e.key === "Escape")
													setRenamingEntry(null);
												e.stopPropagation();
											}}
											onClick={(e) => e.stopPropagation()}
											className="h-6 w-full rounded border border-accent/50 bg-app-darkBox px-2 text-sm text-ink outline-none"
											autoFocus
										/>
									) : (
										<span className="truncate text-sm text-ink">
											{entry.name}
											{isDir && "/"}
										</span>
									)}
									{(protectedDir || protectedFile) && (
										<FontAwesomeIcon
											icon={faLock}
											className="h-2.5 w-2.5 flex-shrink-0 text-ink-faint/50"
											title={
												protectedDir
													? "Protected directory"
													: "Identity file (edit via Config)"
											}
										/>
									)}
								</div>

								{/* Extension badge for files */}
								{!isDir && (
									<span className="flex-shrink-0 text-xs text-ink-faint">
										{fileExtension(entry.name)}
									</span>
								)}

								{/* Size */}
								<span className="w-16 flex-shrink-0 text-right text-xs tabular-nums text-ink-faint">
									{isDir ? "--" : formatFileSize(entry.size)}
								</span>

								{/* Modified */}
								<span className="w-16 flex-shrink-0 text-right text-xs text-ink-faint">
									{entry.modified_at
										? formatTimeAgo(entry.modified_at)
										: "--"}
								</span>

								{/* Actions (visible on hover) */}
								<div className="flex w-20 flex-shrink-0 items-center justify-end gap-1 opacity-0 transition-opacity group-hover:opacity-100">
									{!isDir && (
										<a
											href={api.filesystemDownloadUrl(
												agentId,
												entryPath,
											)}
											onClick={(e) => e.stopPropagation()}
											className="flex h-6 w-6 items-center justify-center rounded text-ink-faint transition-colors hover:bg-app-box hover:text-ink"
											title="Download"
										>
											<FontAwesomeIcon
												icon={faDownload}
												className="h-3 w-3"
											/>
										</a>
									)}
									{!protectedDir && !protectedFile && (
										<>
											<button
												type="button"
												onClick={(e) => {
													e.stopPropagation();
													setRenamingEntry(
														entry.name,
													);
													setRenameValue(entry.name);
												}}
												className="flex h-6 w-6 items-center justify-center rounded text-ink-faint transition-colors hover:bg-app-box hover:text-ink"
												title="Rename"
											>
												<FontAwesomeIcon
													icon={faPen}
													className="h-3 w-3"
												/>
											</button>
											<button
												type="button"
												onClick={(e) => {
													e.stopPropagation();
													if (
														window.confirm(
															`Delete ${entry.name}${isDir ? " and all its contents" : ""}?`,
														)
													) {
														deleteMutation.mutate(
															entryPath,
														);
													}
												}}
												className="flex h-6 w-6 items-center justify-center rounded text-ink-faint transition-colors hover:bg-red-500/10 hover:text-red-400"
												title="Delete"
											>
												<FontAwesomeIcon
													icon={faTrash}
													className="h-3 w-3"
												/>
											</button>
										</>
									)}
								</div>
							</div>
						);
					})}
				</div>

				{/* Preview panel */}
				{selectedFile && (
					<div className="flex w-1/2 flex-col overflow-hidden">
						{/* Preview header */}
						<div className="flex items-center gap-2 border-b border-app-line px-4 py-2">
							<span className="min-w-0 flex-1 truncate text-sm font-medium text-ink">
								{selectedFile.split("/").pop()}
							</span>
							{filePreview && (
								<span className="text-xs text-ink-faint">
									{formatFileSize(filePreview.size)}
									{filePreview.truncated && " (truncated)"}
								</span>
							)}
							<div className="flex items-center gap-1">
								{!editMode && !isProtectedIdentityFile(
									"",
									selectedFile.split("/").pop() ?? "",
								) && (
									<Button
										variant="ghost"
										size="sm"
										onClick={() => {
											setEditContent(
												filePreview?.content ?? "",
											);
											setEditMode(true);
										}}
										disabled={previewLoading}
									>
										<FontAwesomeIcon
											icon={faPen}
											className="mr-1.5 h-3 w-3"
										/>
										Edit
									</Button>
								)}
								{editMode && (
									<>
										<Button
											variant="default"
											size="sm"
											loading={
												saveFileMutation.isPending
											}
											onClick={() => {
												saveFileMutation.mutate({
													path: selectedFile,
													content: editContent,
												});
											}}
										>
											<FontAwesomeIcon
												icon={faFloppyDisk}
												className="mr-1.5 h-3 w-3"
											/>
											Save
										</Button>
										<Button
											variant="ghost"
											size="sm"
											onClick={() => setEditMode(false)}
										>
											<FontAwesomeIcon
												icon={faXmark}
												className="mr-1.5 h-3 w-3"
											/>
											Cancel
										</Button>
									</>
								)}
								<a
									href={api.filesystemDownloadUrl(
										agentId,
										selectedFile,
									)}
									className="inline-flex"
								>
									<Button variant="ghost" size="sm">
										<FontAwesomeIcon
											icon={faDownload}
											className="mr-1.5 h-3 w-3"
										/>
										Download
									</Button>
								</a>
								<Button
									variant="ghost"
									size="sm"
									onClick={() => {
										setSelectedFile(null);
										setEditMode(false);
									}}
								>
									<FontAwesomeIcon
										icon={faXmark}
										className="h-3 w-3"
									/>
								</Button>
							</div>
						</div>

						{/* Preview content */}
						<div className="flex-1 overflow-auto">
							{previewLoading && (
								<div className="flex items-center justify-center py-12">
									<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
								</div>
							)}
							{!previewLoading && filePreview && (
								<>
									{editMode ? (
										<textarea
											value={editContent}
											onChange={(e) =>
												setEditContent(e.target.value)
											}
											className="h-full w-full resize-none bg-app-darkBox/30 p-4 font-mono text-xs leading-relaxed text-ink outline-none"
											spellCheck={false}
										/>
									) : (
										<pre className="whitespace-pre-wrap break-words p-4 font-mono text-xs leading-relaxed text-ink-dull">
											{filePreview.content}
										</pre>
									)}
								</>
							)}
						</div>
					</div>
				)}
			</div>

			{/* Ingest progress bar (only when browsing ingest/) */}
			{isIngestDir && ingestData && ingestData.files.length > 0 && (
				<IngestStatusBar files={ingestData.files} />
			)}

			{/* Create file/folder dialog */}
			<Dialog
				open={createDialog !== null}
				onOpenChange={(open) => {
					if (!open) setCreateDialog(null);
				}}
			>
				<DialogContent>
					<DialogHeader>
						<DialogTitle>
							{createDialog === "folder"
								? "New Folder"
								: "New File"}
						</DialogTitle>
					</DialogHeader>
					<input
						type="text"
						value={createName}
						onChange={(e) => setCreateName(e.target.value)}
						onKeyDown={(e) => {
							if (e.key === "Enter") handleCreate();
						}}
						placeholder={
							createDialog === "folder"
								? "Folder name"
								: "File name"
						}
						className="w-full rounded-lg border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink placeholder-ink-faint outline-none focus:border-accent/50"
						autoFocus
					/>
					<DialogFooter>
						<Button
							variant="outline"
							size="sm"
							onClick={() => setCreateDialog(null)}
						>
							Cancel
						</Button>
						<Button
							size="sm"
							onClick={handleCreate}
							disabled={
								!createName.trim() ||
								writeMutation.isPending
							}
							loading={writeMutation.isPending}
						>
							Create
						</Button>
					</DialogFooter>
				</DialogContent>
			</Dialog>
		</div>
	);
}

// -- Ingest status bar --

function IngestStatusBar({files}: {files: IngestFileInfo[]}) {
	const active = files.filter(
		(f) => f.status === "queued" || f.status === "processing",
	);
	const completed = files.filter((f) => f.status === "completed").length;
	const failed = files.filter((f) => f.status === "failed").length;

	return (
		<div className="border-t border-app-line bg-app-darkBox/30 px-4 py-2">
			<div className="mb-1.5 flex items-center gap-2">
				<span className="text-xs font-medium text-ink-dull">
					Ingestion
				</span>
				<Badge variant="green" size="md">
					{completed} completed
				</Badge>
				{failed > 0 && (
					<Badge variant="red" size="md">
						{failed} failed
					</Badge>
				)}
				{active.length > 0 && (
					<Badge variant="accent" size="md">
						{active.length} active
					</Badge>
				)}
			</div>
			{active.length > 0 && (
				<div className="flex flex-col gap-1">
					{active.map((file) => {
						const progress =
							file.total_chunks > 0
								? Math.round(
										(file.chunks_completed /
											file.total_chunks) *
											100,
									)
								: 0;
						return (
							<div
								key={file.content_hash}
								className="flex items-center gap-3 text-xs"
							>
								<span className="min-w-0 flex-1 truncate text-ink-dull">
									{file.filename}
								</span>
								<IngestStatusBadge status={file.status} />
								{file.status === "processing" &&
									file.total_chunks > 0 && (
										<div className="flex items-center gap-2">
											<div className="h-1 w-24 overflow-hidden rounded-full bg-app-line">
												<div
													className="h-full rounded-full bg-blue-400 transition-all duration-500"
													style={{
														width: `${progress}%`,
													}}
												/>
											</div>
											<span className="tabular-nums text-ink-faint">
												{file.chunks_completed}/
												{file.total_chunks}
											</span>
										</div>
									)}
							</div>
						);
					})}
				</div>
			)}
		</div>
	);
}
