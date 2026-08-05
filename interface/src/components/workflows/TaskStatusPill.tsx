import type {TaskStatus} from "@/api/client";
import {styleFor} from "@/components/tasks/boardColumns";
import {STATUS_LABEL} from "@/components/tasks/taskTransitions";

/**
 * A task's status, safe for every status the server can send.
 *
 * Deliberately not `@spacedrive/ai`'s `TaskStatusIcon`: that component knows
 * five statuses, indexes a map with the sixth, and throws on the `undefined` it
 * gets back — so a workflow whose step is `blocked` would take the run view
 * down with it. `styleFor` and the `?? status` fallback below cannot throw, and
 * a status this build has never heard of renders as its own name rather than as
 * a blank space.
 */
export function TaskStatusPill({
	status,
	className = "",
}: {
	status: TaskStatus;
	className?: string;
}) {
	const style = styleFor(status);
	return (
		<span
			className={`inline-flex shrink-0 items-center gap-1.5 rounded-full border border-app-line bg-app-box/50 px-2 py-0.5 text-[10px] text-ink-dull ${className}`}
			title={style.hint}
		>
			<span className={`size-1.5 rounded-full ${style.dot}`} />
			{STATUS_LABEL[status] ?? status}
		</span>
	);
}
