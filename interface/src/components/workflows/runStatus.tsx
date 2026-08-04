import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import type {IconDefinition} from "@fortawesome/fontawesome-svg-core";
import {
	faBan,
	faCircleCheck,
	faCircleExclamation,
	faTriangleExclamation,
} from "@fortawesome/free-solid-svg-icons";
import type {RunStatus} from "@/api/client";

/**
 * One table for how a run's status looks and what it means.
 *
 * Shared rather than re-tabulated at each call site, for the same reason
 * `boardColumns.styleFor` is shared: three surfaces show a run status — the run
 * header, the template's run list, and the workflows list — and three private
 * tables is how they end up disagreeing about whether `cancelled` is a failure.
 *
 * The four terminal statuses deliberately do not share a look. `failed`,
 * `stuck` and `cancelled` are three different things that happened and three
 * different recoveries: a failed run ran and something in it does not work, a
 * stuck run never got the chance and needs the graph or a person changed, and a
 * cancelled run is somebody having decided. Painting all three red would put
 * the reader back to reading rows, which is the problem run state exists to
 * remove.
 */
export interface RunStatusStyle {
	label: string;
	/** What this status means for the run, one sentence, as a tooltip. */
	hint: string;
	/** Shown in the pill. `null` means a dot instead — see `dot`. */
	icon: IconDefinition | null;
	/** Pill: border, background, text. */
	chip: string;
	/** Full-width reason strip under a header. */
	banner: string;
	/** Used only when `icon` is null. */
	dot: string;
	/**
	 * Whether a person is wanted. Drives the workflows list, which has to make a
	 * stuck run findable without opening every template.
	 */
	wantsAttention: boolean;
}

const RUN_STATUS_STYLE: Record<RunStatus, RunStatusStyle> = {
	running: {
		label: "Running",
		hint: "Tasks outstanding and progress recent — this run is still going.",
		icon: null,
		chip: "border-status-info/50 bg-status-info/10 text-status-info",
		banner: "border-app-line bg-app-box/40 text-ink-dull",
		dot: "bg-status-info",
		wantsAttention: false,
	},
	succeeded: {
		label: "Succeeded",
		hint: "Every task settled and no failure path was taken. This run is over.",
		icon: faCircleCheck,
		chip: "border-status-success/50 bg-status-success/10 text-status-success",
		banner: "border-status-success/40 bg-status-success/5 text-status-success",
		dot: "bg-status-success",
		wantsAttention: false,
	},
	failed: {
		label: "Failed",
		hint: "A task used up its failure budget, or a loop took its give-up path. The pipeline ran; something in it does not work.",
		icon: faCircleExclamation,
		chip: "border-status-error/50 bg-status-error/10 text-status-error",
		banner: "border-status-error/40 bg-status-error/5 text-status-error",
		dot: "bg-status-error",
		wantsAttention: true,
	},
	// The loud one, and the only status that gets a solid border and a bolded
	// label. A stuck run is not a run that went wrong on its own terms — it is a
	// run that stopped making progress and will stay that way silently until
	// somebody looks, which is precisely the failure mode this status exists to
	// end. Amber rather than red keeps it distinguishable from `failed` at a
	// glance, since the two want opposite responses.
	stuck: {
		label: "Stuck",
		hint: "Nothing is in flight and nothing at the frontier can move. This run will not advance on its own.",
		icon: faTriangleExclamation,
		chip: "border-status-warning bg-status-warning/15 font-semibold text-status-warning",
		banner: "border-status-warning/60 bg-status-warning/10 text-status-warning",
		dot: "bg-status-warning",
		wantsAttention: true,
	},
	// Settled by a decision, not by an outcome — so it is drawn the way `skipped`
	// is drawn on the board: neutral, hollow, and visibly not a result.
	cancelled: {
		label: "Cancelled",
		hint: "A person stopped this run. Its unstarted tasks were settled as skipped; anything in flight was left to finish.",
		icon: faBan,
		chip: "border-app-line bg-app-box/40 text-ink-faint",
		banner: "border-app-line bg-app-box/40 text-ink-dull",
		dot: "border border-ink-dull bg-transparent",
		wantsAttention: false,
	},
};

/** For a status a newer server knows and this build does not. */
const UNKNOWN_STYLE: RunStatusStyle = {
	label: "Unknown",
	hint: "Unrecognised run status — this page is older than the server.",
	icon: null,
	chip: "border-app-line bg-app-box/50 text-ink-dull",
	banner: "border-app-line bg-app-box/40 text-ink-dull",
	dot: "bg-ink-faint",
	wantsAttention: false,
};

/**
 * The style for one status, which can never throw and never renders blank.
 *
 * A status this bundle has never heard of comes out under its own name rather
 * than as an empty pill, the same guarantee `TaskStatusPill` makes.
 */
export function runStatusStyle(status: RunStatus): RunStatusStyle {
	return RUN_STATUS_STYLE[status] ?? {...UNKNOWN_STYLE, label: status};
}

/**
 * A run's status, tolerating a server older than this bundle.
 *
 * `status` is required by the schema, so this is not defensive typing for its
 * own sake: the dashboard ships separately from the binary, and a run row
 * written before the column existed comes back without one. Reading that as
 * `running` is the honest fallback — it is what every caller assumed back when
 * there was no status at all — and it is strictly better than rendering an
 * empty pill on every historical run.
 */
export function runStatusOf(run: {status?: RunStatus}): RunStatus {
	return run.status ?? "running";
}

/** `false` only while the run may still move on its own. */
export function isRunFinished(status: RunStatus): boolean {
	return status !== "running";
}

/** A run somebody has to do something about. */
export function runWantsAttention(run: {status?: RunStatus}): boolean {
	return runStatusStyle(runStatusOf(run)).wantsAttention;
}

/**
 * The status of a run, wherever a run appears.
 *
 * `running` is the only status without an icon, on purpose: it is the one that
 * is not an outcome, and a pulsing dot says "still going" in a way a glyph
 * cannot.
 */
export function RunStatusPill({
	status,
	className = "",
}: {
	status: RunStatus;
	className?: string;
}) {
	const style = runStatusStyle(status);
	return (
		<span
			className={`inline-flex shrink-0 items-center gap-1.5 rounded-full border px-2 py-0.5 text-[10px] uppercase tracking-wide ${style.chip} ${className}`}
			title={style.hint}
		>
			{style.icon ? (
				<FontAwesomeIcon icon={style.icon} className="text-[9px]" />
			) : (
				<span
					className={`size-1.5 rounded-full ${style.dot} ${
						status === "running" ? "animate-pulse" : ""
					}`}
				/>
			)}
			{style.label}
		</span>
	);
}

/**
 * Why the run is where it is, in the server's own words.
 *
 * The reason is the entire point of attaching one to a terminal transition —
 * "stuck" alone sends somebody reading rows — so it is reproduced verbatim
 * rather than summarised. It names the task and the hold: blocked for a person,
 * a gate that can no longer open, a placeholder that will never expand, or
 * inputs that will never resolve.
 */
export function RunReasonBanner({
	status,
	reason,
	className = "",
}: {
	status: RunStatus;
	reason: string;
	className?: string;
}) {
	const style = runStatusStyle(status);
	return (
		<div
			className={`flex items-start gap-2 border-b px-4 py-2 text-[11px] ${style.banner} ${className}`}
		>
			{style.icon && (
				<FontAwesomeIcon
					icon={style.icon}
					className="mt-0.5 shrink-0 text-[11px]"
				/>
			)}
			<div className="min-w-0">
				<span className="font-medium">{style.label}.</span>{" "}
				<span className="break-words opacity-90">{reason}</span>
			</div>
		</div>
	);
}
