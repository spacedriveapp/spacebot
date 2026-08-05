import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {faHandPaper, faRotate} from "@fortawesome/free-solid-svg-icons";
import type {TaskItem} from "@/api/client";

/**
 * What a loop has to do with this task, on a card that has one line for it.
 *
 * Two different facts, and the second is the one that misleads without it:
 *
 *  - the task is a pass of a loop body, so its siblings are its own history
 *    rather than parallel work;
 *  - the task is *downstream* of a loop and held pending its verdict. That one
 *    sits in the backlog looking exactly like ordinary queued work, when in
 *    fact it is waiting to find out whether it will ever run at all — a loop
 *    that converges releases one arm and abandons the other, and this card is
 *    on one of them.
 *
 * `block_reason` already says so in words on the card below. The chip is what
 * makes it legible in the column, where there is no room for a sentence.
 */
export function LoopChips({task}: {task: TaskItem}) {
	return (
		<>
			{task.loop_group && task.loop_iteration != null && (
				<span
					className="inline-flex shrink-0 items-center gap-1 rounded border border-accent/40 bg-accent/10 px-1.5 py-px font-mono text-[10px] leading-4 text-accent"
					title={`Pass ${task.loop_iteration} of loop body \`${task.loop_group}\`. Passes are sequential: this one exists because the pass before it did not meet the loop's exit condition.`}
				>
					<FontAwesomeIcon icon={faRotate} className="text-[9px]" />
					{task.loop_group} · pass {task.loop_iteration}
				</span>
			)}
			{task.awaiting_loop_group && task.awaiting_loop_arm && (
				<span
					className="inline-flex shrink-0 items-center gap-1 rounded border border-status-warning/40 bg-status-warning/10 px-1.5 py-px font-mono text-[10px] leading-4 text-status-warning"
					title={
						task.awaiting_loop_arm === "on_exhausted"
							? `Held on the give-up arm of loop \`${task.awaiting_loop_group}\`. It runs only if that loop runs out of passes — if the loop converges, it never runs.`
							: `Held on the ordinary arm of loop \`${task.awaiting_loop_group}\`. It runs only if loop \`${task.awaiting_loop_group}\` converges — if the loop runs out of passes, it never runs.`
					}
				>
					<FontAwesomeIcon icon={faHandPaper} className="text-[9px]" />
					held ·{" "}
					{task.awaiting_loop_arm === "on_exhausted"
						? "gave-up arm"
						: "converged arm"}
				</span>
			)}
		</>
	);
}
