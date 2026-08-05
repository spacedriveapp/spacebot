import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {faCodeBranch} from "@fortawesome/free-solid-svg-icons";
import type {TaskItem} from "@/api/client";

/**
 * States the real status of a skipped task, above the shared detail panel.
 *
 * The same job `BlockedBanner` does, for the same reason. `@spacedrive/ai` knows
 * five statuses and `skipped` is not one of them, so the drawer hands it an
 * adapted copy reading `done` — terminality is the property that has to survive,
 * and `done` is the only terminal status that panel has. But `done` overstates
 * the outcome badly: it says the work happened and succeeded, when in fact a
 * condition ruled it out and nothing ran at all. Left uncorrected the drawer
 * shows a green tick over a task that never started.
 *
 * `skip_reason` is the whole point of the banner. A skipped task that does not
 * say *which* condition ruled it out leaves a reader with a branch that vanished
 * and no way to find out why — and since there is deliberately no un-skip, the
 * reason is the only account of it that will ever exist.
 */
export function SkippedBanner({task}: {task: TaskItem}) {
	if (task.status !== "skipped") return null;

	return (
		<div className="border-b border-app-line bg-app-box/40 px-4 py-3">
			<div className="mb-1.5 flex flex-wrap items-center gap-2">
				<FontAwesomeIcon
					icon={faCodeBranch}
					className="h-3 w-3 text-ink-dull"
				/>
				<span className="text-xs font-semibold uppercase tracking-wide text-ink-dull">
					Skipped
				</span>
			</div>

			{task.skip_reason ? (
				<p className="mb-2 break-words font-mono text-[11px] leading-relaxed text-ink-dull">
					{task.skip_reason}
				</p>
			) : (
				<p className="mb-2 text-[11px] text-ink-dull">
					A condition ruled this out. No reason was recorded.
				</p>
			)}

			<p className="text-[11px] text-ink-faint">
				This task never ran and never will — skipped is terminal, and there is
				no un-skip. Anything downstream that needed its output has skipped too;
				anything that only waited on its ordering has carried on. The panel
				below labels it &ldquo;done&rdquo; because it cannot render this state.
			</p>
		</div>
	);
}
