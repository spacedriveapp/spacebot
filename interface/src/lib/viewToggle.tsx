import {useCallback, useState} from "react";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import type {IconProp} from "@fortawesome/fontawesome-svg-core";

/**
 * A two-way view choice, remembered.
 *
 * Which view someone wants is a property of how they work, not of the visit —
 * re-picking it on every navigation is the kind of small friction that makes a
 * view go unused. localStorage rather than the server: it is a per-browser
 * preference with no consequence if it is lost, and a config round-trip to
 * store it would be a dead knob of the sort this codebase already has three
 * of.
 */

export type ChoiceToggleVariant = "segmented" | "joined";

export interface ChoiceToggleOption<T extends string> {
	value: T;
	icon: IconProp;
	label: string;
	/** Tooltip explaining what this view is for. Omitted when the label says it. */
	hint?: string;
}

/**
 * Read the stored choice, tolerating anything that isn't one of the offered
 * values, and persist every update. Reads once during the initial render so
 * the first paint is already the right view — reading in an effect would
 * flash the default on every load.
 */
export function usePersistedChoice<T extends string>(
	storageKey: string,
	choices: readonly T[],
	fallback: T,
): [T, (next: T) => void] {
	const [choice, setChoice] = useState<T>(() => {
		try {
			const stored = localStorage.getItem(storageKey);
			return choices.includes(stored as T) ? (stored as T) : fallback;
		} catch {
			// Private-mode browsers throw on access. The default is not worth a crash.
			return fallback;
		}
	});

	const choose = useCallback(
		(next: T) => {
			setChoice(next);
			try {
				localStorage.setItem(storageKey, next);
			} catch {
				// Preference lost, view still switched. Nothing to tell the user.
			}
		},
		[storageKey],
	);

	return [choice, choose];
}

export interface ChoiceToggleProps<T extends string> {
	value: T;
	onChange: (next: T) => void;
	options: readonly ChoiceToggleOption<T>[];
	ariaLabel?: string;
	/**
	 * `segmented` — padded container with pill buttons. `joined` — flush
	 * buttons sharing one border.
	 */
	variant: ChoiceToggleVariant;
}

export function ChoiceToggle<T extends string>({
	value,
	onChange,
	options,
	ariaLabel,
	variant,
}: ChoiceToggleProps<T>) {
	return (
		<div
			role="group"
			aria-label={ariaLabel}
			className={
				variant === "segmented"
					? "flex items-center rounded border border-app-line bg-app-box/40 p-0.5"
					: "flex shrink-0 overflow-hidden rounded border border-app-line"
			}
		>
			{options.map((option) => (
				<ToggleButton
					key={option.value}
					option={option}
					active={value === option.value}
					onClick={() => onChange(option.value)}
					variant={variant}
				/>
			))}
		</div>
	);
}

function ToggleButton<T extends string>({
	option,
	active,
	onClick,
	variant,
}: {
	option: ChoiceToggleOption<T>;
	active: boolean;
	onClick: () => void;
	variant: ChoiceToggleVariant;
}) {
	return (
		<button
			type="button"
			onClick={onClick}
			title={option.hint}
			aria-pressed={active}
			className={
				variant === "segmented"
					? `inline-flex items-center gap-1.5 rounded px-2 py-1 text-xs transition-colors ${
							active
								? "bg-app-box text-ink shadow-sm"
								: "text-ink-faint hover:text-ink-dull"
						}`
					: `flex items-center gap-1.5 px-2 py-1 text-[10px] ${
							active
								? "bg-accent text-white"
								: "text-ink-faint hover:bg-app-box/60 hover:text-ink-dull"
						}`
			}
		>
			<FontAwesomeIcon
				icon={option.icon}
				className={variant === "segmented" ? "h-3 w-3" : "text-[9px]"}
			/>
			{option.label}
		</button>
	);
}
