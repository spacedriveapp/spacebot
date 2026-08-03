import { FontAwesomeIcon } from "@fortawesome/react-fontawesome";
import { faBan } from "@fortawesome/free-solid-svg-icons";
import { Button } from "@spacedrive/primitives";
import type { TaskItem } from "@/api/client";
import { BlockKindChip } from "./BlockKindChip";

/**
 * States the real status of a blocked task, above the shared detail panel.
 *
 * That panel comes from `@spacedrive/ai`, which has no `blocked` status and
 * crashes when handed one, so the drawer passes it an adapted copy reading
 * `pending_approval`. Without this banner the drawer would quietly assert a
 * status the task does not have and offer an Approve button for an approval
 * nobody is waiting on. The adapted row stays — it cannot be removed from a
 * component we do not own — but it is no longer the only thing the reader sees.
 */
export interface BlockedBannerProps {
	task: TaskItem;
	onUnblock?: (task: TaskItem) => void;
	onRetry?: (task: TaskItem) => void;
	busy?: boolean;
}

export function BlockedBanner({
	task,
	onUnblock,
	onRetry,
	busy,
}: BlockedBannerProps) {
	if (task.status !== "blocked") return null;

	// A missing credential is not fixed by running the same task again, so the
	// verb follows the kind — the same split the board makes.
	const sticky =
		task.block_kind === "needs_input" || task.block_kind === "capability";

	return (
		<div className="border-b border-status-error/30 bg-status-error/5 px-4 py-3">
			<div className="mb-1.5 flex flex-wrap items-center gap-2">
				<FontAwesomeIcon icon={faBan} className="h-3 w-3 text-status-error" />
				<span className="text-xs font-semibold uppercase tracking-wide text-status-error">
					Blocked
				</span>
				<BlockKindChip kind={task.block_kind} reason={task.block_reason} />
			</div>

			{task.block_reason && (
				<p className="mb-2 break-words font-mono text-[11px] leading-relaxed text-ink-dull">
					{task.block_reason}
				</p>
			)}

			<p className="mb-2 text-[11px] text-ink-faint">
				Not picked up automatically. The status control below reads
				&ldquo;pending approval&rdquo; because the shared panel cannot render
				this state.
			</p>

			{sticky && onUnblock && (
				<Button size="sm" variant="gray" disabled={busy} onClick={() => onUnblock(task)}>
					Unblock
				</Button>
			)}
			{!sticky && onRetry && (
				<Button size="sm" variant="gray" disabled={busy} onClick={() => onRetry(task)}>
					Retry
				</Button>
			)}
		</div>
	);
}
