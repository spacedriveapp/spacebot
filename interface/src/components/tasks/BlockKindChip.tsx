import { FontAwesomeIcon } from "@fortawesome/react-fontawesome";
import {
	faArrowRightToBracket,
	faHourglassHalf,
	faKey,
	faRotate,
} from "@fortawesome/free-solid-svg-icons";
import type { BlockKind } from "@/api/client";

/**
 * Why a task is parked, and — more usefully — whether it is waiting on the
 * system or on you.
 *
 * The two sticky kinds are styled as demanding attention because nothing will
 * move them until a human acts. The two automatic kinds are muted: they clear
 * themselves, and dressing them up as problems would train people to ignore the
 * ones that are.
 */
const STYLES: Record<
	BlockKind,
	{ label: string; icon: typeof faKey; className: string; actionable: boolean }
> = {
	needs_input: {
		label: "Needs input",
		icon: faArrowRightToBracket,
		className: "border-status-warning/40 bg-status-warning/10 text-status-warning",
		actionable: true,
	},
	capability: {
		label: "Missing access",
		icon: faKey,
		className: "border-status-error/40 bg-status-error/10 text-status-error",
		actionable: true,
	},
	dependency: {
		label: "Waiting upstream",
		icon: faHourglassHalf,
		className: "border-app-line bg-app-box/60 text-ink-faint",
		actionable: false,
	},
	transient: {
		label: "Retrying",
		icon: faRotate,
		className: "border-app-line bg-app-box/60 text-ink-dull",
		actionable: false,
	},
};

export interface BlockKindChipProps {
	kind?: BlockKind | null;
	/** Shown on hover — usually the server's `block_reason`. */
	reason?: string | null;
}

export function BlockKindChip({ kind, reason }: BlockKindChipProps) {
	if (!kind) return null;
	const style = STYLES[kind];
	if (!style) return null;

	return (
		<span
			title={reason ?? style.label}
			className={`inline-flex shrink-0 items-center gap-1 rounded border px-1.5 py-px font-mono text-[10px] leading-4 ${style.className}`}
		>
			<FontAwesomeIcon icon={style.icon} className="text-[9px]" />
			{style.label}
		</span>
	);
}

/** Whether this block is one only a human can clear. */
export function isActionableBlock(kind?: BlockKind | null): boolean {
	return kind ? (STYLES[kind]?.actionable ?? false) : false;
}
