import { Badge, Button } from "@spacedrive/primitives";
import { FontAwesomeIcon } from "@fortawesome/react-fontawesome";
import { faBan, faChevronDown, faRotateRight } from "@fortawesome/free-solid-svg-icons";
import type { TaskItem } from "@/api/client";
import { RepoChip, type BindingNames } from "./RepoChip";

/**
 * Blocked tasks, rendered locally rather than through `TaskList`.
 *
 * `@spacedrive/ai` hardcodes `TASK_STATUS_ORDER` to five statuses and drops any
 * task whose status isn't in that list:
 *
 *   for (const status of groups) map.set(status, []);
 *   for (const task of tasks) {
 *     const bucket = map.get(task.status);
 *     if (bucket) bucket.push(task);   // unknown status => silently dropped
 *   }
 *
 * So a `blocked` task handed to `TaskList` disappears from the board entirely.
 * Until the design system learns the status, blocked tasks get their own
 * section — which is arguably where they belong anyway: blocked means *stuck
 * and needing a human*, which is a different queue from backlog's *waiting its
 * turn*.
 */
export interface BlockedTasksSectionProps {
	tasks: TaskItem[];
	collapsed?: boolean;
	onToggle?: () => void;
	onRetry?: (task: TaskItem) => void;
	onTaskClick?: (task: TaskItem) => void;
	activeTaskId?: string | null;
	retryingTaskNumber?: number | null;
	resolveAgentName?: (agentId: string) => string;
	bindingNames?: BindingNames;
}

export function BlockedTasksSection({
	tasks,
	collapsed = false,
	onToggle,
	onRetry,
	onTaskClick,
	activeTaskId,
	retryingTaskNumber,
	resolveAgentName,
	bindingNames,
}: BlockedTasksSectionProps) {
	if (tasks.length === 0) return null;

	return (
		<div className="flex flex-col">
			<button
				type="button"
				onClick={onToggle}
				className="flex items-center gap-2 border-b border-app-line bg-app-box/40 px-3 py-2 text-left transition-colors hover:bg-app-box/70"
			>
				<FontAwesomeIcon
					icon={faChevronDown}
					className={`h-2.5 w-2.5 text-ink-faint transition-transform ${
						collapsed ? "-rotate-90" : ""
					}`}
				/>
				<FontAwesomeIcon icon={faBan} className="h-3 w-3 text-status-error" />
				<span className="text-xs font-semibold uppercase tracking-wide text-ink">
					Blocked
				</span>
				<span className="text-xs text-ink-faint">{tasks.length}</span>
				<span className="ml-auto text-[11px] text-ink-faint">
					Needs a human — not picked up automatically
				</span>
			</button>

			{!collapsed && (
				<div className="flex flex-col">
					{tasks.map((task) => {
						const isRetrying = retryingTaskNumber === task.task_number;
						return (
							<div
								key={task.id}
								className={`flex items-start gap-3 border-b border-app-line/60 border-l-2 border-l-status-error/50 px-3 py-2.5 transition-colors hover:bg-app-box/40 ${
									activeTaskId === task.id ? "bg-app-box/60" : ""
								}`}
							>
								<button
									type="button"
									onClick={() => onTaskClick?.(task)}
									className="flex min-w-0 flex-1 flex-col items-start gap-1 text-left"
								>
									<div className="flex w-full items-center gap-2">
										<span className="shrink-0 font-mono text-[11px] text-ink-faint">
											#{task.task_number}
										</span>
										<span className="truncate text-sm font-medium text-ink">
											{task.title}
										</span>
										<RepoChip task={task} names={bindingNames} />
										{task.consecutive_failures > 0 && (
											<Badge variant="error" size="sm" className="shrink-0">
												{task.consecutive_failures}
												{task.max_retries ? `/${task.max_retries}` : ""} failed
											</Badge>
										)}
									</div>

									{/* The reason is why a human is here, but it must not outshout
									    the title — muted, clamped, full text on hover. */}
									{task.last_error && (
										<p
											className="line-clamp-2 w-full break-all font-mono text-[11px] leading-relaxed text-ink-dull"
											title={task.last_error}
										>
											{task.last_error}
										</p>
									)}

									{resolveAgentName && (
										<span className="text-[11px] text-ink-faint">
											{resolveAgentName(task.assigned_agent_id)}
										</span>
									)}
								</button>

								{/* Always visible: on a queue that exists for human attention,
								    the primary action must not be hidden behind hover. */}
								{onRetry && (
									<Button
										variant="gray"
										size="sm"
										disabled={isRetrying}
										onClick={() => onRetry(task)}
										className="shrink-0"
										title="Clear the failure budget and requeue"
									>
										<FontAwesomeIcon
											icon={faRotateRight}
											className={`mr-1.5 h-3 w-3 ${isRetrying ? "animate-spin" : ""}`}
										/>
										{isRetrying ? "Retrying…" : "Retry"}
									</Button>
								)}
							</div>
						);
					})}
				</div>
			)}
		</div>
	);
}
