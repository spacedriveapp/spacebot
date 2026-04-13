import { useState, useRef, useEffect } from "react";
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

// ---------------------------------------------------------------------------
// Sub-tab definitions
// ---------------------------------------------------------------------------

const TABS = [
	{ key: "overview", label: "Overview" },
	{ key: "code-graph", label: "Code Graph" },
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

// ---------------------------------------------------------------------------
// Pipeline Phase Display
// ---------------------------------------------------------------------------

const PIPELINE_PHASES = [
	"extracting",
	"structure",
	"parsing",
	"imports",
	"calls",
	"heritage",
	"communities",
	"processes",
	"enriching",
	"complete",
] as const;

const PHASE_LABELS: Record<string, string> = {
	extracting: "Extracting Files",
	structure: "Building Structure",
	parsing: "Parsing Source",
	imports: "Resolving Imports",
	calls: "Resolving Calls",
	heritage: "Resolving Inheritance",
	communities: "Detecting Communities",
	processes: "Tracing Processes",
	enriching: "Enriching",
	complete: "Complete",
};

// Each phase gets a color — segments fill in left-to-right as phases complete.
const PHASE_SEGMENT_COLORS: Record<string, string> = {
	extracting: "bg-emerald-500",
	structure: "bg-emerald-400",
	parsing: "bg-teal-500",
	imports: "bg-blue-500",
	calls: "bg-blue-400",
	heritage: "bg-indigo-500",
	communities: "bg-violet-500",
	processes: "bg-purple-500",
	enriching: "bg-amber-500",
	complete: "bg-emerald-500",
};

function AnimatedNumber({ value, duration = 500 }: { value: number; duration?: number }) {
	const [display, setDisplay] = useState(value);
	const prevRef = useRef(value);

	useEffect(() => {
		const from = prevRef.current;
		const to = value;
		prevRef.current = value;

		if (from === to) return;

		const start = performance.now();
		let raf: number;

		const tick = (now: number) => {
			const elapsed = now - start;
			const t = Math.min(elapsed / duration, 1);
			const eased = 1 - Math.pow(1 - t, 3); // ease-out cubic
			setDisplay(Math.round(from + (to - from) * eased));
			if (t < 1) {
				raf = requestAnimationFrame(tick);
			}
		};

		raf = requestAnimationFrame(tick);
		return () => cancelAnimationFrame(raf);
	}, [value, duration]);

	return <>{display.toLocaleString()}</>;
}

function IndexingProgress({ project }: { project: CodeGraphProject }) {
	const progress = project.progress;

	// Cache the last non-null progress so animation can finish even if the
	// backend clears the progress field before we're done animating.
	const cachedProgressRef = useRef(progress);
	if (progress) cachedProgressRef.current = progress;
	const effectiveProgress = progress ?? cachedProgressRef.current;

	// Actual phase index from the API (0-based).
	const actualPhaseIdx = effectiveProgress
		? PIPELINE_PHASES.indexOf(effectiveProgress.phase as typeof PIPELINE_PHASES[number])
		: -1;

	// Animated display phase: steps through one at a time so the user sees
	// every phase even when the backend blows through stubs instantly.
	const [displayPhaseIdx, setDisplayPhaseIdx] = useState(() => Math.max(0, actualPhaseIdx));

	// Detect when a NEW indexing run starts (status transitions TO "indexing").
	// This is more reliable than checking for a specific phase, since fast
	// pipelines can blow through "extracting" before the first poll lands.
	const wasIndexingRef = useRef(project.status === "indexing");
	useEffect(() => {
		const isNowIndexing = project.status === "indexing";
		if (isNowIndexing && !wasIndexingRef.current) {
			// New indexing run — full reset
			setDisplayPhaseIdx(0);
			cachedProgressRef.current = undefined;
		}
		wasIndexingRef.current = isNowIndexing;
	}, [project.status]);

	// Step forward one phase at a time with a 400ms delay.
	useEffect(() => {
		if (actualPhaseIdx > displayPhaseIdx) {
			const timer = setTimeout(() => {
				setDisplayPhaseIdx((prev) => prev + 1);
			}, 400);
			return () => clearTimeout(timer);
		}
	}, [actualPhaseIdx, displayPhaseIdx]);

	if (!effectiveProgress) {
		return (
			<div className="rounded-xl border border-blue-500/30 bg-blue-500/5 p-5">
				<div className="flex items-center gap-3">
					<div className="h-2 w-2 animate-pulse rounded-full bg-blue-500" />
					<p className="text-sm font-medium text-blue-400">Preparing to index...</p>
				</div>
			</div>
		);
	}

	const showIdx = Math.max(0, Math.min(displayPhaseIdx, PIPELINE_PHASES.length - 1));
	// Only show as complete when the backend says "indexed" AND we've
	// animated through to the final phase. The "complete" phase is index 9
	// but "enriching" is 8 — don't mark done until status flips.
	const isAnimComplete = project.status === "indexed";

	// Use actual phase_progress if we're on the same phase the API reports,
	// otherwise treat earlier phases as fully complete (1.0).
	const phaseProgress =
		showIdx === actualPhaseIdx ? (effectiveProgress.phase_progress ?? 0) : 1.0;

	const overallProgress = isAnimComplete
		? 100
		: ((showIdx + phaseProgress) / PIPELINE_PHASES.length) * 100;

	const borderColor = isAnimComplete ? "border-emerald-500/30" : "border-blue-500/30";
	const bgColor = isAnimComplete ? "bg-emerald-500/5" : "bg-blue-500/5";
	const textColor = isAnimComplete ? "text-emerald-400" : "text-blue-400";

	return (
		<div className={clsx("rounded-xl border p-5 transition-colors duration-500", borderColor, bgColor)}>
			{/* Header */}
			<div className="mb-4 flex items-center justify-between">
				<div className="flex items-center gap-3">
					<div className={clsx(
						"h-2 w-2 rounded-full",
						isAnimComplete ? "bg-emerald-500" : "animate-pulse bg-blue-500",
					)} />
					<p className={clsx("text-sm font-semibold", textColor)}>
						{isAnimComplete
							? "Indexing Complete"
							: `Indexing — Phase ${showIdx + 1}/${PIPELINE_PHASES.length}`
						}
					</p>
				</div>
				<span className="text-xs text-ink-faint">{Math.round(overallProgress)}%</span>
			</div>

			{/* Segmented progress bar — each phase is a colored segment.
			    The full bar is visible as a gray track; segments fill in
			    with their phase color as they complete. */}
			<div className="mb-4 flex h-2 gap-0.5 overflow-hidden rounded-full">
				{PIPELINE_PHASES.map((phase, i) => {
					const isDone = isAnimComplete || i < showIdx;
					const isCurrent = !isAnimComplete && i === showIdx;
					// Current phase fills partially based on phase_progress.
					const fillPct = isDone ? 100 : isCurrent ? Math.round(phaseProgress * 100) : 0;
					const segColor = PHASE_SEGMENT_COLORS[phase] ?? "bg-accent";
					return (
						<div key={phase} className="relative flex-1 overflow-hidden rounded-full bg-app-line">
							<div
								className={clsx(
									"absolute inset-y-0 left-0 rounded-full transition-all duration-700 ease-out",
									segColor,
								)}
								style={{ width: `${fillPct}%` }}
							/>
						</div>
					);
				})}
			</div>

			{/* Phase labels */}
			<div className="mb-4 grid grid-cols-5 gap-1">
				{PIPELINE_PHASES.map((phase, i) => {
					const isCurrent = !isAnimComplete && i === showIdx;
					const isDone = isAnimComplete || i < showIdx;
					return (
						<div key={phase} className="flex flex-col items-center">
							<span
								className={clsx(
									"text-[10px] leading-tight",
									isCurrent ? "font-medium text-blue-400" : isDone ? "text-emerald-400/70" : "text-ink-faint/50",
								)}
							>
								{PHASE_LABELS[phase]?.split(" ")[0]}
							</span>
						</div>
					);
				})}
			</div>

			{/* Current message */}
			<p className="mb-3 text-xs text-ink-dull">
				{showIdx < actualPhaseIdx
					? PHASE_LABELS[PIPELINE_PHASES[showIdx]]
					: effectiveProgress.message}
			</p>

			{/* Live stats */}
			<div className="grid grid-cols-4 gap-3 sm:grid-cols-7">
				{[
					["Files", effectiveProgress.stats.files_found],
					["Parsed", effectiveProgress.stats.files_parsed],
					["Skipped", effectiveProgress.stats.files_skipped ?? 0],
					["Nodes", effectiveProgress.stats.nodes_created],
					["Edges", effectiveProgress.stats.edges_created],
					["Communities", effectiveProgress.stats.communities_detected],
					["Processes", effectiveProgress.stats.processes_traced],
				].map(([label, value]) => (
					<div key={label as string} className="text-center">
						<p className="text-sm font-semibold text-ink">
							<AnimatedNumber value={value as number} />
						</p>
						<p className="text-[10px] text-ink-faint">{label as string}</p>
					</div>
				))}
			</div>
		</div>
	);
}

// ---------------------------------------------------------------------------
// Overview Tab
// ---------------------------------------------------------------------------

function OverviewTab({ project }: { project: CodeGraphProject }) {
	const queryClient = useQueryClient();
	const prevStatusRef = useRef(project.status);
	const [showComplete, setShowComplete] = useState(false);

	// Detect indexing → indexed transition to keep the progress banner visible
	// long enough for the phase animation to finish + show the "Complete" state.
	useEffect(() => {
		if (prevStatusRef.current === "indexing" && project.status === "indexed") {
			setShowComplete(true);
			const timer = setTimeout(() => setShowComplete(false), 10_000);
			return () => clearTimeout(timer);
		}
		if (project.status === "indexing") {
			setShowComplete(false);
		}
		prevStatusRef.current = project.status;
	}, [project.status]);

	const reindexMutation = useMutation({
		mutationFn: () => api.codegraphReindex(project.project_id),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["codegraph-project", project.project_id] });
		},
	});

	const showProgress =
		project.status === "indexing" ||
		showComplete ||
		(project.status === "indexed" && !!project.progress);

	return (
		<div className="flex flex-col gap-6">
			{/* Live indexing progress — stays visible during animation + after completion */}
			{showProgress && <IndexingProgress project={project} />}

			{/* Status banner — shown when not actively indexing */}
			{!showProgress && (project.status === "indexed" || project.status === "error") && (
				<div
					className={clsx(
						"flex items-center gap-3 rounded-xl border p-4",
						project.status === "indexed"
							? "border-emerald-500/30 bg-emerald-500/5"
							: "border-red-500/30 bg-red-500/5",
					)}
				>
					<div
						className={clsx(
							"h-2.5 w-2.5 rounded-full",
							project.status === "indexed" ? "bg-emerald-500" : "bg-red-500",
						)}
					/>
					<div className="flex flex-col gap-1">
						<p
							className={clsx(
								"text-sm font-semibold",
								project.status === "indexed" ? "text-emerald-400" : "text-red-400",
							)}
						>
							{project.status === "indexed" ? "Index Completed" : "Index Failed"}
						</p>
						{project.status === "error" && project.error_message && (
							<p className="text-xs text-red-300/80">{project.error_message}</p>
						)}
					</div>
					{project.status === "indexed" && project.last_indexed_at && (
						<span className="ml-auto text-xs text-ink-faint">
							{new Date(project.last_indexed_at).toLocaleString()}
						</span>
					)}
				</div>
			)}

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
						<dd>
							<span
								className={clsx(
									"inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-xs font-medium",
									project.status === "indexed" && "bg-emerald-500/10 text-emerald-400",
									project.status === "indexing" && "bg-blue-500/10 text-blue-400",
									project.status === "error" && "bg-red-500/10 text-red-400",
									project.status === "pending" && "bg-yellow-500/10 text-yellow-400",
									project.status === "stale" && "bg-orange-500/10 text-orange-400",
								)}
							>
								<span
									className={clsx(
										"h-1.5 w-1.5 rounded-full",
										project.status === "indexed" && "bg-emerald-500",
										project.status === "indexing" && "animate-pulse bg-blue-500",
										project.status === "error" && "bg-red-500",
										project.status === "pending" && "bg-yellow-500",
										project.status === "stale" && "bg-orange-500",
									)}
								/>
								{project.status === "indexed" ? "Indexed" : project.status === "error" ? "Failed" : project.status}
							</span>
						</dd>
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
			{project.last_index_stats && project.status !== "indexing" && (
				<div className="rounded-xl border border-app-line bg-app-darkBox p-5">
					<h3 className="mb-3 font-plex text-sm font-semibold text-ink">Index Stats</h3>
					<div className="grid grid-cols-4 gap-4 sm:grid-cols-7">
						{[
							["Files", project.last_index_stats.files_found],
							["Parsed", project.last_index_stats.files_parsed],
							["Skipped", project.last_index_stats.files_skipped ?? 0],
							["Nodes", project.last_index_stats.nodes_created],
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
		refetchInterval: (query) => {
			const project = query.state.data?.project;
			// Poll fast during indexing so stats animate in real-time,
			// and briefly after completion for the transition.
			if (project?.status === "indexing") return 1_000;
			if (project?.status === "indexed" && project?.progress) return 2_000;
			return 10_000;
		},
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

			{/* Tab content — Overview is scroll+padded, Code Graph owns its own layout */}
			<div
				className={clsx(
					"min-h-0 flex-1",
					activeTab === "overview" && "overflow-y-auto p-6",
					activeTab === "code-graph" && "flex overflow-hidden",
				)}
			>
				{activeTab === "overview" && <OverviewTab project={project} />}
				{activeTab === "code-graph" && <CodeGraphTab projectId={projectId} />}
			</div>

			<RemoveProjectDialog
				project={project}
				open={removeOpen}
				onOpenChange={setRemoveOpen}
			/>
		</div>
	);
}
