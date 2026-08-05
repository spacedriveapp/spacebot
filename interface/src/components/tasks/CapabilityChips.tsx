import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {faUsersGear} from "@fortawesome/free-solid-svg-icons";

/**
 * What a pooled task asked for, as chips.
 *
 * A pooled task has no assignee until somebody claims it, and until this
 * existed the board rendered that as an empty line — indistinguishable from a
 * task whose agent had been deleted. "Addressed by capability" is a different
 * thing from "addressed to nobody" and has to read as one.
 *
 * `unsatisfied` tints the labels that are the reason nothing can take it, so
 * the chip row doubles as the diagnosis on a board where a banner may be
 * scrolled away.
 */
export interface CapabilityChipsProps {
	requires: readonly string[];
	/** Labels to mark as the problem. Usually the `undeclared` set. */
	unsatisfied?: readonly string[];
	/** Prefix the row with the pool icon and "Requires". */
	labelled?: boolean;
	className?: string;
}

export function CapabilityChips({
	requires,
	unsatisfied,
	labelled = true,
	className,
}: CapabilityChipsProps) {
	if (requires.length === 0) {
		// A pooled task requiring nothing is claimable by anybody, which is a
		// real state the server allows and not the same as a pushed task.
		return (
			<span
				className={`inline-flex items-center gap-1 text-[10px] text-ink-faint ${className ?? ""}`}
			>
				<FontAwesomeIcon icon={faUsersGear} className="text-[9px]" />
				Pooled — requires nothing, any agent may claim it
			</span>
		);
	}

	const problem = new Set(unsatisfied ?? []);

	return (
		<span
			className={`inline-flex flex-wrap items-center gap-1 ${className ?? ""}`}
		>
			{labelled && (
				<span className="inline-flex items-center gap-1 text-[10px] text-ink-faint">
					<FontAwesomeIcon icon={faUsersGear} className="text-[9px]" />
					Requires
				</span>
			)}
			{requires.map((label) => (
				<span
					key={label}
					title={
						problem.has(label) ? `No agent declares "${label}"` : undefined
					}
					className={`inline-flex shrink-0 items-center rounded border px-1.5 py-px font-mono text-[10px] leading-4 ${
						problem.has(label)
							? "border-status-error/40 bg-status-error/10 text-status-error"
							: "border-accent/30 bg-accent/10 text-accent"
					}`}
				>
					{label}
				</span>
			))}
		</span>
	);
}
