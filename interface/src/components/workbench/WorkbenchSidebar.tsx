import {useState} from "react";
import {cx} from "class-variance-authority";
import type {OrchestrationWorker, ProjectGroup, WorktreeGroup} from "./types";

export function WorkbenchSidebar({
	tree,
	totalCount,
	onSelectWorker,
	fillWidth = false,
}: {
	tree: ProjectGroup[];
	totalCount: number;
	onSelectWorker: (id: string) => void;
	fillWidth?: boolean;
}) {
	return (
		<aside
			className={
				fillWidth
					? "flex h-full w-full flex-col overflow-hidden bg-sidebar"
					: "flex h-full w-[270px] flex-shrink-0 flex-col overflow-hidden rounded-2xl border border-app-line bg-app"
			}
		>
			<div className="flex h-[60px] flex-col justify-center gap-0.5 border-b border-app-line px-3 pt-1">
				<h2 className="text-xs font-medium text-ink">Workbench</h2>
				<div className="text-tiny text-ink-faint">
					{totalCount === 1
						? "1 active worker"
						: `${totalCount} active workers`}
				</div>
			</div>
			<div className="flex-1 overflow-y-auto py-2">
				{tree.length === 0 ? (
					<div className="px-3 py-4 text-tiny text-ink-faint">No workers</div>
				) : (
					tree.map((project) => (
						<ProjectSection
							key={project.key}
							project={project}
							onSelectWorker={onSelectWorker}
						/>
					))
				)}
			</div>
		</aside>
	);
}

function ProjectSection({
	project,
	onSelectWorker,
}: {
	project: ProjectGroup;
	onSelectWorker: (id: string) => void;
}) {
	const [expanded, setExpanded] = useState(true);
	return (
		<div className="mb-1">
			<button
				onClick={() => setExpanded((v) => !v)}
				className="flex w-full items-center gap-1.5 px-3 py-1 text-left text-tiny font-semibold uppercase tracking-wide text-ink-faint hover:text-ink"
			>
				<svg
					width="10"
					height="10"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					strokeWidth="3"
					className={cx("shrink-0 transition-transform", expanded ? "rotate-90" : "")}
				>
					<polyline points="9 18 15 12 9 6" />
				</svg>
				<span className="flex-1 truncate">{project.name}</span>
				<span className="text-tiny text-ink-faint">{project.count}</span>
			</button>
			{expanded && (
				<div className="ml-[18px] mt-0.5 border-l border-app-line">
					{project.worktrees.map((worktree) => (
						<WorktreeSection
							key={worktree.key}
							worktree={worktree}
							onSelectWorker={onSelectWorker}
						/>
					))}
				</div>
			)}
		</div>
	);
}

function WorktreeSection({
	worktree,
	onSelectWorker,
}: {
	worktree: WorktreeGroup;
	onSelectWorker: (id: string) => void;
}) {
	return (
		<div className="mb-0.5">
			{worktree.name !== "main" && (
				<div className="flex items-center gap-1.5 px-3 py-0.5 text-tiny text-ink-faint">
					<svg
						width="8"
						height="8"
						viewBox="0 0 24 24"
						fill="none"
						stroke="currentColor"
						strokeWidth="1.5"
						className="shrink-0"
					>
						<circle cx="6" cy="6" r="3" />
						<circle cx="18" cy="18" r="3" />
						<path d="M6 9v6a3 3 0 0 0 3 3h6" />
					</svg>
					<span className="truncate font-medium text-ink-dull" title={worktree.directory ?? ""}>
						{worktree.name}
					</span>
				</div>
			)}
			<div className="flex flex-col">
				{worktree.workers.map((worker) => (
					<WorkerRow
						key={worker.id}
						worker={worker}
						onSelect={onSelectWorker}
					/>
				))}
			</div>
		</div>
	);
}

function WorkerRow({
	worker,
	onSelect,
}: {
	worker: OrchestrationWorker;
	onSelect: (id: string) => void;
}) {
	const isRunning = worker.status === "running";
	const isIdle = worker.status === "idle";
	const taskText = worker.task.replace(/^\[opencode\]\s*/i, "");

	return (
		<button
			onClick={() => onSelect(worker.id)}
			className="group flex w-full items-center gap-2.5 px-3 py-1.5 text-left hover:bg-app-selected/40"
			title={taskText}
		>
			<span
				className={cx(
					"h-2 w-2 flex-shrink-0 rounded-full",
					isRunning && "animate-pulse bg-green-500",
					isIdle && "bg-yellow-500",
					!isRunning && !isIdle && "bg-ink-faint/40",
				)}
			/>
			<span className="min-w-0 flex-1">
				<span className="block truncate text-xs text-ink">
					{taskText}
				</span>
				<span className="block truncate text-tiny text-ink-faint">
					{worker.agent_name}
				</span>
			</span>
		</button>
	);
}
