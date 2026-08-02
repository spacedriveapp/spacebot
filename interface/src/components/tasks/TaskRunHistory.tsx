import { useQuery } from "@tanstack/react-query";
import { Badge } from "@spacedrive/primitives";
import { FontAwesomeIcon } from "@fortawesome/react-fontawesome";
import {
	faBan,
	faCheck,
	faClock,
	faPlugCircleXmark,
	faSpinner,
	faTriangleExclamation,
	faXmark,
} from "@fortawesome/free-solid-svg-icons";
import { api, type TaskRun, type TaskRunOutcome } from "@/api/client";

/** Visual treatment per attempt outcome. */
const OUTCOME_STYLE: Record<
	TaskRunOutcome,
	{ icon: typeof faCheck; className: string; label: string }
> = {
	completed: { icon: faCheck, className: "text-status-success", label: "Completed" },
	failed: { icon: faXmark, className: "text-status-error", label: "Failed" },
	timeout: { icon: faClock, className: "text-status-error", label: "Timed out" },
	cancelled: { icon: faBan, className: "text-ink-faint", label: "Cancelled" },
	blocked: { icon: faTriangleExclamation, className: "text-status-error", label: "Blocked" },
	// Rate limits are recorded but deliberately don't spend the failure budget,
	// so they read as neutral rather than as a failure.
	rate_limited: { icon: faClock, className: "text-status-warning", label: "Rate limited" },
	// The worker never reported back — the reaper wrote this row, not the run.
	// Distinct from "failed" because nothing observed the work; it just stopped.
	abandoned: {
		icon: faPlugCircleXmark,
		className: "text-status-error",
		label: "Abandoned",
	},
};

/** Badge treatment per outcome. A still-running attempt has no outcome yet. */
function badgeVariantFor(
	outcome?: TaskRunOutcome,
): "secondary" | "success" | "error" | "warning" {
	if (!outcome) return "secondary";
	if (outcome === "completed") return "success";
	// Rate limits are not the task's fault and don't spend the failure budget.
	if (outcome === "rate_limited") return "warning";
	if (outcome === "cancelled") return "secondary";
	return "error";
}

function formatDuration(startedAt: string, endedAt?: string): string | null {
	if (!endedAt) return null;
	const ms = new Date(endedAt).getTime() - new Date(startedAt).getTime();
	if (!Number.isFinite(ms) || ms < 0) return null;
	if (ms < 1000) return `${ms}ms`;
	const seconds = Math.round(ms / 1000);
	if (seconds < 60) return `${seconds}s`;
	const minutes = Math.floor(seconds / 60);
	const remainder = seconds % 60;
	return remainder === 0 ? `${minutes}m` : `${minutes}m ${remainder}s`;
}

export interface TaskRunHistoryProps {
	taskNumber: number;
	/** Called when an attempt's worker is clicked, to open its transcript. */
	onWorkerClick?: (workerId: string) => void;
}

/**
 * The per-attempt execution log for a task.
 *
 * Each row is one entry in `task_runs`. A task retried after a crash or timeout
 * has several; the currently-running attempt has no outcome and no end time.
 */
export function TaskRunHistory({ taskNumber, onWorkerClick }: TaskRunHistoryProps) {
	const { data, isLoading, error } = useQuery({
		queryKey: ["task-runs", taskNumber],
		queryFn: () => api.listTaskRuns(taskNumber),
		refetchInterval: 10_000,
	});

	if (isLoading) {
		return <p className="px-3 py-2 text-xs text-ink-faint">Loading attempts…</p>;
	}
	if (error) {
		return <p className="px-3 py-2 text-xs text-status-error">Failed to load attempts</p>;
	}

	return <TaskRunHistoryView runs={data?.runs ?? []} onWorkerClick={onWorkerClick} />;
}

export interface TaskRunHistoryViewProps {
	runs: TaskRun[];
	onWorkerClick?: (workerId: string) => void;
}

/** Presentational half — takes runs directly so it can render without a backend. */
export function TaskRunHistoryView({ runs, onWorkerClick }: TaskRunHistoryViewProps) {
	if (runs.length === 0) {
		return <p className="px-3 py-2 text-xs text-ink-faint">No attempts recorded yet</p>;
	}

	return (
		<div className="flex flex-col gap-1.5">
			{runs.map((run: TaskRun) => {
				const style = run.outcome ? OUTCOME_STYLE[run.outcome] : null;
				const duration = formatDuration(run.started_at, run.ended_at);
				const running = !run.outcome;

				return (
					<div
						key={run.id}
						className="flex items-start gap-2.5 rounded border border-app-line/60 bg-app-box/30 px-2.5 py-2"
					>
						<FontAwesomeIcon
							icon={running ? faSpinner : (style?.icon ?? faXmark)}
							className={`mt-0.5 h-3 w-3 shrink-0 ${
								running ? "animate-spin text-ink-faint" : (style?.className ?? "text-ink-faint")
							}`}
						/>

						<div className="flex min-w-0 flex-1 flex-col gap-1">
							<div className="flex flex-wrap items-center gap-2">
								<span className="text-xs font-medium text-ink">
									Attempt {run.attempt}
								</span>
								<Badge variant={badgeVariantFor(run.outcome)}>
									{running ? "Running" : (style?.label ?? run.outcome)}
								</Badge>
								{duration && <span className="text-[11px] text-ink-faint">{duration}</span>}
								{run.worker_id && (
									<button
										type="button"
										onClick={() => onWorkerClick?.(run.worker_id as string)}
										className="font-mono text-[11px] text-ink-faint underline-offset-2 hover:text-ink hover:underline"
										title="Open worker transcript"
									>
										{run.worker_id.slice(0, 8)}
									</button>
								)}
							</div>

							{run.summary && (
								<p className="line-clamp-3 text-[11px] leading-relaxed text-ink-dull">
									{run.summary}
								</p>
							)}
							{run.error && (
								<p
									className="line-clamp-3 break-all font-mono text-[11px] leading-relaxed text-ink-dull"
									title={run.error}
								>
									{run.error}
								</p>
							)}
						</div>
					</div>
				);
			})}
		</div>
	);
}
