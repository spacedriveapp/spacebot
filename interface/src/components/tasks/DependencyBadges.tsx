import { FontAwesomeIcon } from "@fortawesome/react-fontawesome";
import { faArrowLeftLong, faArrowRightLong } from "@fortawesome/free-solid-svg-icons";
import type { TaskEdgeSummary } from "@/api/client";

/**
 * Upstream and downstream edge counts for a card.
 *
 * Upstream turns warning-coloured when something is still outstanding, because
 * that is the difference between "this ran after three others" (history) and
 * "this is waiting on three others" (why nothing is happening). Without that
 * distinction the badge is trivia.
 */
export interface DependencyBadgesProps {
	summary?: TaskEdgeSummary;
	onClick?: () => void;
}

export function DependencyBadges({ summary, onClick }: DependencyBadgesProps) {
	if (!summary || (summary.parents === 0 && summary.children === 0)) return null;

	const waiting = summary.blocked_by > 0;
	const Wrapper = onClick ? "button" : "span";

	return (
		<Wrapper
			{...(onClick ? { type: "button" as const, onClick } : {})}
			title={
				waiting
					? `Waiting on ${summary.blocked_by} of ${summary.parents} upstream task(s)`
					: `${summary.parents} upstream, ${summary.children} downstream`
			}
			className={`inline-flex shrink-0 items-center gap-1.5 rounded border border-app-line bg-app-box/60 px-1.5 py-px font-mono text-[10px] leading-4 ${
				onClick ? "hover:border-app-line-hover hover:bg-app-box" : ""
			}`}
		>
			{summary.parents > 0 && (
				<span
					className={`inline-flex items-center gap-0.5 ${
						waiting ? "text-status-warning" : "text-ink-faint"
					}`}
				>
					<FontAwesomeIcon icon={faArrowLeftLong} className="text-[9px]" />
					{waiting ? `${summary.blocked_by}/${summary.parents}` : summary.parents}
				</span>
			)}
			{summary.children > 0 && (
				<span className="inline-flex items-center gap-0.5 text-ink-faint">
					<FontAwesomeIcon icon={faArrowRightLong} className="text-[9px]" />
					{summary.children}
				</span>
			)}
		</Wrapper>
	);
}

/** Index edge summaries by task number for O(1) lookup while rendering rows. */
export function indexEdges(
	edges: TaskEdgeSummary[] | undefined,
): Map<number, TaskEdgeSummary> {
	return new Map((edges ?? []).map((edge) => [edge.task_number, edge]));
}
