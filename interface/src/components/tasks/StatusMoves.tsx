import {Button} from "@spacedrive/primitives";
import type {TaskItem, TaskStatus} from "@/api/client";
import {STATUS_LABEL, type TransitionTable} from "./taskTransitions";

/**
 * The status moves this task can actually make, and nothing else.
 *
 * `@spacedrive/ai`'s `TaskDetail` renders a status `<select>` listing all five
 * statuses it knows, so it offers `ready → done` — a move the store refuses.
 * It takes no prop to narrow that list, so the drawer stops passing it
 * `onStatusChange` and shows this instead: one button per entry in the
 * server's own transition table, which cannot propose a doomed request.
 *
 * Blocked tasks are deliberately excluded. Every move out of `blocked` routes
 * through unblock, so three buttons doing one thing would be three lies;
 * `BlockedBanner` already offers that one action with the right verb.
 */
export interface StatusMovesProps {
	task: TaskItem;
	table: TransitionTable;
	onMove: (task: TaskItem, status: TaskStatus) => void;
	busy?: boolean;
}

export function StatusMoves({task, table, onMove, busy}: StatusMovesProps) {
	if (task.status === "blocked") return null;

	const targets = table.targetsOf(task.status);
	if (targets.length === 0) return null;

	return (
		<div className="border-t border-app-line/40 px-4 py-3">
			<h3 className="mb-2 text-xs font-medium uppercase tracking-wide text-ink-dull">
				Move to
			</h3>
			<div className="flex flex-wrap gap-2">
				{targets.map((status) => (
					<Button
						key={status}
						size="sm"
						variant={status === "done" ? "accent" : "gray"}
						disabled={busy}
						onClick={() => onMove(task, status)}
					>
						{STATUS_LABEL[status]}
					</Button>
				))}
			</div>
			{/* Names the rule rather than leaving a short list looking arbitrary. */}
			<p className="mt-1.5 text-[10px] text-ink-faint">
				Currently {STATUS_LABEL[task.status].toLowerCase()}. Only moves the task
				store permits are shown.
			</p>
		</div>
	);
}
