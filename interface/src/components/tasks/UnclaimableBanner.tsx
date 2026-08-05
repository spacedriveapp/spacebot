import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {faUsersSlash, faUsersGear} from "@fortawesome/free-solid-svg-icons";
import type {AgentInfo, TaskItem} from "@/api/client";
import {
	agentsSatisfying,
	isAwaitingClaim,
	unclaimablePool,
} from "@/lib/capabilities";
import {CapabilityChips} from "./CapabilityChips";

/**
 * Says why a pooled task is not running, above the shared detail panel.
 *
 * The same job `BlockedBanner` and `SkippedBanner` do, for the fifth kind of
 * "did not move". A pooled task nothing can claim sits at `ready` looking
 * exactly like work about to start — the status is not wrong, nothing is
 * failing, and no amount of waiting fixes it. The sweep already knows, and
 * says so only to the cortex log.
 *
 * The two reasons are kept apart because the repairs are different, and
 * collapsing them sends the reader hunting for a missing label that is right
 * there on another agent. That distinction is the whole reason the server's
 * `UnclaimableTask` carries `undeclared` instead of a boolean, and it would be
 * thrown away by a banner that only said "unclaimable".
 *
 * When the pool *can* be served this renders the healthy case instead — who is
 * eligible — because "waiting for a free agent" and "waiting forever" look
 * identical on a board and only one of them is worth getting up for.
 */
export interface UnclaimableBannerProps {
	task: TaskItem;
	agents: readonly AgentInfo[];
}

export function UnclaimableBanner({task, agents}: UnclaimableBannerProps) {
	// Pushed tasks, and pooled ones already claimed, have an assignee and are
	// somebody's problem in the ordinary way.
	if (!isAwaitingClaim(task)) return null;

	const requires = task.required_capabilities ?? [];
	const eligibleNames = agentsSatisfying(requires, agents).map(
		(agent) => agent.display_name ?? agent.id,
	);

	// Reuse the pool derivation rather than re-deriving the reason here, so the
	// banner and the board report can never disagree about a task.
	const entry = unclaimablePool([task], agents)[0];

	// No entry and nobody eligible means the fleet has not loaded yet — the
	// derivation deliberately declares nothing unclaimable in that case, and
	// asserting either verdict here would be guessing.
	if (!entry && eligibleNames.length === 0) return null;

	if (!entry) {
		return (
			<div className="border-b border-app-line bg-app-box/40 px-4 py-3">
				<div className="mb-1.5 flex flex-wrap items-center gap-2">
					<FontAwesomeIcon
						icon={faUsersGear}
						className="h-3 w-3 text-ink-dull"
					/>
					<span className="text-xs font-semibold uppercase tracking-wide text-ink-dull">
						In the pool
					</span>
				</div>
				<CapabilityChips requires={requires} className="mb-2" />
				<p className="text-[11px] text-ink-faint">
					{eligibleNames.length === 1
						? `Waiting for ${eligibleNames[0]} to pick it up — the only agent declaring all of this.`
						: `Waiting to be claimed by whichever of ${eligibleNames.join(
								", ",
							)} asks first.`}{" "}
					Nobody is named until one does — the panel below shows no assignee
					for that reason, not because the task is broken.
				</p>
			</div>
		);
	}

	const split = entry.reason === "split";

	return (
		<div className="border-b border-status-error/30 bg-status-error/5 px-4 py-3">
			<div className="mb-1.5 flex flex-wrap items-center gap-2">
				<FontAwesomeIcon
					icon={faUsersSlash}
					className="h-3 w-3 text-status-error"
				/>
				<span className="text-xs font-semibold uppercase tracking-wide text-status-error">
					Nothing can claim this
				</span>
				<span className="rounded border border-status-error/40 bg-status-error/10 px-1.5 py-px font-mono text-[10px] leading-4 text-status-error">
					{split ? "Split across the fleet" : "Undeclared label"}
				</span>
			</div>

			<CapabilityChips
				requires={entry.requires}
				unsatisfied={entry.undeclared}
				className="mb-2"
			/>

			{split ? (
				<>
					<p className="mb-2 text-[11px] leading-relaxed text-ink-dull">
						Every one of these labels is declared by somebody, but no single
						agent holds all of them — and a claim is all-or-nothing. Nothing is
						misspelled and nothing is missing; the requirement asks for a
						combination the fleet has not got.
					</p>
					<p className="mb-2 text-[11px] text-ink-faint">
						Give one agent the labels it lacks, or split the step so each half
						asks for what one agent can already do.
					</p>
					<div className="flex flex-col gap-1">
						{agents.map((agent) => {
							const held = new Set(agent.capabilities ?? []);
							const missing = entry.requires.filter(
								(label) => !held.has(label),
							);
							if (missing.length === 0) return null;
							return (
								<p
									key={agent.id}
									className="font-mono text-[10px] leading-relaxed text-ink-dull"
								>
									{agent.display_name ?? agent.id} is missing{" "}
									<span className="text-status-error">
										{missing.join(", ")}
									</span>
								</p>
							);
						})}
					</div>
				</>
			) : (
				<>
					<p className="mb-2 text-[11px] leading-relaxed text-ink-dull">
						No agent in the fleet declares{" "}
						<span className="font-mono text-status-error">
							{entry.undeclared.join(", ")}
						</span>
						. Usually that is a typo — capabilities are case-sensitive, so{" "}
						<span className="font-mono">rust</span> and{" "}
						<span className="font-mono">Rust</span> are two different labels —
						or the specialist that would hold it does not exist yet, or has been
						deleted.
					</p>
					<p className="text-[11px] text-ink-faint">
						Declare it on an agent in its config, or correct the requirement on
						the step this task came from.
					</p>
				</>
			)}

			<p className="mt-2 text-[11px] text-ink-faint">
				This task is <span className="font-mono">ready</span> and will stay
				there. Nothing is failing and no retry applies — it is waiting for the
				fleet to change.
			</p>
		</div>
	);
}
