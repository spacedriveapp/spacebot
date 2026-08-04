import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {faChevronDown, faUsersSlash} from "@fortawesome/free-solid-svg-icons";
import type {AgentInfo, TaskItem} from "@/api/client";
import {explainUnclaimable, unclaimablePool} from "@/lib/capabilities";
import {CapabilityChips} from "./CapabilityChips";

/**
 * Pooled tasks nothing in the fleet can claim, at the top of the board.
 *
 * A pooled task addressed to a capability nobody declares is `ready` forever
 * and looks, on the board, exactly like a task about to start: right status,
 * no error, no failure count, no block. The scheduler knows — `unclaimable_pool`
 * computes precisely this — and tells only the cortex log. This is the same
 * "parked and silent" failure the design doc names, and the same one this
 * codebase has now fixed four times over; the fix each time was to put the hold
 * on the screen somebody is already looking at.
 *
 * It sits above the columns rather than inside one because the repair is not to
 * the task. Every other queue on this board is work you move; this is a list of
 * things wrong with the *fleet*, and grouping by reason is what makes it
 * actionable — the two reasons have different repairs and a single list would
 * tell the reader the wrong thing to do about half of them.
 */
export interface UnclaimablePoolSectionProps {
	tasks: readonly TaskItem[];
	agents: readonly AgentInfo[];
	collapsed?: boolean;
	onToggle?: () => void;
	onTaskClick?: (task: TaskItem) => void;
	activeTaskId?: string | null;
}

export function UnclaimablePoolSection({
	tasks,
	agents,
	collapsed = false,
	onToggle,
	onTaskClick,
	activeTaskId,
}: UnclaimablePoolSectionProps) {
	const entries = unclaimablePool(tasks, agents);
	if (entries.length === 0) return null;

	const undeclared = entries.filter((entry) => entry.reason === "undeclared");
	const split = entries.filter((entry) => entry.reason === "split");

	// Summarised in the header so a collapsed section still names the repair.
	const summary = [
		undeclared.length > 0 && `${undeclared.length} undeclared`,
		split.length > 0 && `${split.length} split across the fleet`,
	]
		.filter(Boolean)
		.join(", ");

	return (
		<div className="flex flex-col border-b border-app-line">
			<button
				type="button"
				onClick={onToggle}
				className="flex items-center gap-2 border-b border-app-line border-l-2 border-l-status-error/60 bg-status-error/5 px-3 py-2 text-left transition-colors hover:bg-status-error/10"
			>
				<FontAwesomeIcon
					icon={faChevronDown}
					className={`h-2.5 w-2.5 text-ink-faint transition-transform ${
						collapsed ? "-rotate-90" : ""
					}`}
				/>
				<FontAwesomeIcon
					icon={faUsersSlash}
					className="h-3 w-3 text-status-error"
				/>
				<span className="text-xs font-semibold uppercase tracking-wide text-status-error">
					Nothing can claim
				</span>
				<span className="text-xs text-ink-faint">{entries.length}</span>
				<span className="ml-auto text-[11px] text-ink-faint">
					{summary} — waiting on the fleet, not on a turn
				</span>
			</button>

			{!collapsed && (
				<div className="flex flex-col">
					{[
						{
							key: "undeclared" as const,
							items: undeclared,
							heading: "No agent declares these labels",
							repair:
								"A typo, or a specialist that does not exist yet. Declare the label on an agent, or correct the requirement.",
						},
						{
							key: "split" as const,
							items: split,
							heading: "Split across the fleet",
							repair:
								"Every label is held by somebody, but a claim is all-or-nothing and no single agent holds them all. Give one agent the rest, or split the step.",
						},
					]
						.filter((group) => group.items.length > 0)
						.map((group) => (
							<div key={group.key} className="flex flex-col">
								<div className="border-b border-app-line/40 bg-app-box/40 px-3 py-1.5">
									<p className="text-[11px] font-medium text-ink-dull">
										{group.heading}
									</p>
									<p className="text-[10px] text-ink-faint">{group.repair}</p>
								</div>
								{group.items.map((entry) => (
									<button
										key={entry.task.id}
										type="button"
										onClick={() => onTaskClick?.(entry.task)}
										className={`flex flex-col items-start gap-1 border-b border-app-line/60 border-l-2 border-l-status-error/50 px-3 py-2 text-left transition-colors hover:bg-app-box/40 ${
											activeTaskId === entry.task.id ? "bg-app-box/60" : ""
										}`}
									>
										<div className="flex w-full items-center gap-2">
											<span className="shrink-0 font-mono text-[11px] text-ink-faint">
												#{entry.task.task_number}
											</span>
											<span className="truncate text-sm font-medium text-ink">
												{entry.task.title}
											</span>
										</div>
										<CapabilityChips
											requires={entry.requires}
											unsatisfied={entry.undeclared}
										/>
										{/* The same sentence the server would have written to the
										    cortex log, so the two accounts of one problem match. */}
										<p className="w-full break-words font-mono text-[10px] leading-relaxed text-ink-dull">
											{explainUnclaimable(entry)}
										</p>
									</button>
								))}
							</div>
						))}
				</div>
			)}
		</div>
	);
}
