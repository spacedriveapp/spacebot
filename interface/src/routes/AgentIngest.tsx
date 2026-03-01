import {useState, useRef, useCallback} from "react";
import {useQuery, useMutation, useQueryClient} from "@tanstack/react-query";
import {api, type IngestFileInfo} from "@/api/client";
import {formatTimeAgo} from "@/lib/format";
import {Badge} from "@/ui";
import {clsx} from "clsx";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {faTrash} from "@fortawesome/free-solid-svg-icons";

function formatFileSize(bytes: number): string {
	if (bytes < 1024) return `${bytes} B`;
	if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
	return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function StatusBadge({status}: {status: IngestFileInfo["status"]}) {
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

interface AgentIngestProps {
	agentId: string;
}

export function AgentIngest({agentId}: AgentIngestProps) {
	const queryClient = useQueryClient();
	const fileInputRef = useRef<HTMLInputElement>(null);
	const [isDragging, setIsDragging] = useState(false);
	const dragCounter = useRef(0);

	const {data, isLoading, error} = useQuery({
		queryKey: ["ingest-files", agentId],
		queryFn: () => api.ingestFiles(agentId),
		refetchInterval: 5_000,
	});

	const uploadMutation = useMutation({
		mutationFn: (files: File[]) => api.uploadIngestFiles(agentId, files),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["ingest-files", agentId]});
		},
	});

	const deleteMutation = useMutation({
		mutationFn: (contentHash: string) =>
			api.deleteIngestFile(agentId, contentHash),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["ingest-files", agentId]});
		},
	});

	const handleFiles = useCallback(
		(files: FileList | File[]) => {
			const fileArray = Array.from(files);
			if (fileArray.length > 0) {
				uploadMutation.mutate(fileArray);
			}
		},
		[uploadMutation],
	);

	const handleDragEnter = useCallback((e: React.DragEvent) => {
		e.preventDefault();
		e.stopPropagation();
		dragCounter.current += 1;
		setIsDragging(true);
	}, []);

	const handleDragLeave = useCallback((e: React.DragEvent) => {
		e.preventDefault();
		e.stopPropagation();
		dragCounter.current -= 1;
		if (dragCounter.current === 0) {
			setIsDragging(false);
		}
	}, []);

	const handleDragOver = useCallback((e: React.DragEvent) => {
		e.preventDefault();
		e.stopPropagation();
	}, []);

	const handleDrop = useCallback(
		(e: React.DragEvent) => {
			e.preventDefault();
			e.stopPropagation();
			dragCounter.current = 0;
			setIsDragging(false);
			if (e.dataTransfer.files.length > 0) {
				handleFiles(e.dataTransfer.files);
			}
		},
		[handleFiles],
	);

	const files = data?.files ?? [];
	const queued = files.filter((f) => f.status === "queued").length;
	const processing = files.filter((f) => f.status === "processing").length;
	const completed = files.filter((f) => f.status === "completed").length;
	const failed = files.filter((f) => f.status === "failed").length;

	return (
		<div
			className="flex h-full flex-col"
			onDragEnter={handleDragEnter}
			onDragLeave={handleDragLeave}
			onDragOver={handleDragOver}
			onDrop={handleDrop}
		>
			{/* Stats bar */}
			<div className="flex items-center gap-2 border-b border-app-line px-6 py-3">
				<Badge variant="accent" size="md">
					{files.length} total
				</Badge>
				{queued > 0 && (
					<Badge variant="amber" size="md">
						{queued} queued
					</Badge>
				)}
				{processing > 0 && (
					<Badge variant="accent" size="md">
						{processing} processing
					</Badge>
				)}
				<Badge variant="green" size="md">
					{completed} completed
				</Badge>
				{failed > 0 && (
					<Badge variant="red" size="md">
						{failed} failed
					</Badge>
				)}
				<div className="flex-1" />
				<span className="text-xs text-ink-faint">
					.pdf .txt .md .json .csv .yaml .toml .html .log +more
				</span>
			</div>

			<div className="flex-1 overflow-auto p-6">
				{/* Drop zone */}
				<button
					type="button"
					onClick={() => fileInputRef.current?.click()}
					className={clsx(
						"mb-6 flex h-auto w-full cursor-pointer flex-col items-center justify-center rounded-lg border-2 border-dashed py-10 transition-colors hover:bg-app-box/20",
						isDragging
							? "border-accent/30 bg-accent/[0.02]"
							: "border-app-line bg-transparent hover:border-app-line/80",
					)}
				>
					<div
						className={`mb-3 text-3xl ${isDragging ? "text-accent" : "text-ink-faint"}`}
					>
						{uploadMutation.isPending ? (
							<span className="inline-block h-6 w-6 animate-spin rounded-full border-2 border-accent border-t-transparent" />
						) : (
							"\u2191"
						)}
					</div>
					<p className="mb-1 text-sm font-medium text-ink">
						{isDragging
							? "Drop files here"
							: uploadMutation.isPending
								? "Uploading..."
								: "Drop files here or click to browse"}
					</p>
					<p className="text-xs text-ink-faint">
						Supported files, including PDFs, are chunked and processed into
						structured memories
					</p>
					{uploadMutation.isError && (
						<p className="mt-2 text-xs text-red-400">
							Upload failed. Please try again.
						</p>
					)}
				</button>

				<input
					ref={fileInputRef}
					type="file"
					multiple
					className="hidden"
					accept=".pdf,.txt,.md,.markdown,.json,.jsonl,.csv,.tsv,.log,.xml,.yaml,.yml,.toml,.rst,.org,.html,.htm"
					onChange={(e) => {
						if (e.target.files) {
							handleFiles(e.target.files);
							e.target.value = "";
						}
					}}
				/>

				{/* File list */}
				{isLoading && (
					<div className="flex items-center justify-center py-12">
						<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
					</div>
				)}

				{error && (
					<div className="rounded-xl bg-red-500/10 px-4 py-3 text-sm text-red-400">
						Failed to load ingestion files
					</div>
				)}

				{!isLoading && !error && files.length === 0 && (
					<div className="flex flex-col items-center justify-center py-8 text-center">
						<p className="text-sm text-ink-dull">
							No files ingested yet. Drop a supported file above to get started.
						</p>
					</div>
				)}

				{files.length > 0 && (
					<div className="flex flex-col gap-2">
						{files.map((file) => (
							<FileRow
								key={file.content_hash}
								file={file}
								onDelete={() => deleteMutation.mutate(file.content_hash)}
								isDeleting={deleteMutation.isPending}
							/>
						))}
					</div>
				)}
			</div>
		</div>
	);
}

function FileRow({
	file,
	onDelete,
	isDeleting,
}: {
	file: IngestFileInfo;
	onDelete: () => void;
	isDeleting: boolean;
}) {
	const progress =
		file.total_chunks > 0
			? Math.round((file.chunks_completed / file.total_chunks) * 100)
			: 0;

	return (
		<div className="group flex items-center gap-4 rounded-lg border border-app-line bg-app-darkBox/30 px-4 py-3">
			{/* File icon */}
			<div className="flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-lg bg-app-box text-xs text-ink-faint">
				{file.filename.split(".").pop()?.toUpperCase() ?? "TXT"}
			</div>

			{/* Info */}
			<div className="flex min-w-0 flex-1 flex-col gap-0.5">
				<span className="truncate text-sm font-medium text-ink">
					{file.filename}
				</span>
				<div className="flex items-center gap-3 text-xs text-ink-faint">
					<span>{formatFileSize(file.file_size)}</span>
					{file.total_chunks > 0 && (
						<span>
							{file.total_chunks} chunk{file.total_chunks !== 1 ? "s" : ""}
						</span>
					)}
					<span>{formatTimeAgo(file.started_at)}</span>
				</div>

				{/* Progress bar for in-progress files */}
				{file.status === "processing" && (
					<div className="mt-1.5 flex items-center gap-2">
						<div className="h-1 flex-1 overflow-hidden rounded-full bg-app-line">
							<div
								className="h-full rounded-full bg-blue-400 transition-all duration-500"
								style={{width: `${progress}%`}}
							/>
						</div>
						<span className="text-xs tabular-nums text-ink-faint">
							{file.chunks_completed}/{file.total_chunks}
						</span>
					</div>
				)}
			</div>

			{/* Status badge - centered on the right */}
			<div className="flex-shrink-0">
				<StatusBadge status={file.status} />
			</div>

			{/* Delete button (always visible) */}
			{file.status !== "processing" && (
				<button
					onClick={onDelete}
					disabled={isDeleting}
					className="flex-shrink-0 flex h-8 w-8 items-center justify-center rounded-lg text-ink-faint hover:bg-app-box transition-colors disabled:opacity-50"
					title="Delete file"
				>
					{isDeleting ? (
						<span className="inline-block h-3 w-3 animate-spin rounded-full border-2 border-ink-faint border-t-transparent" />
					) : (
						<FontAwesomeIcon icon={faTrash} className="h-3.5 w-3.5" />
					)}
				</button>
			)}
		</div>
	);
}
