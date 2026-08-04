import {useEffect, useMemo, useRef, useState, type KeyboardEvent} from "react";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {faPlus, faXmark} from "@fortawesome/free-solid-svg-icons";

/**
 * Picks capability labels, offering the ones the fleet already declares.
 *
 * This is a mitigation the design named, not decoration. Capabilities are
 * opaque strings and case is deliberately not folded — `rust` and `Rust` are
 * two capabilities and one of them matches nothing, silently, forever. The
 * design's answer is to "offer the existing set when authoring rather than
 * validating a taxonomy into existence", which is the whole reason
 * `AgentInfo.capabilities` is published to the client at all.
 *
 * So the shape here is deliberate: existing labels are one click and are what
 * Enter takes by default, while inventing a new one is a separate, differently
 * coloured row that has to be chosen on purpose. Typing a novel label and
 * blurring adds *nothing* — unlike the plain `TagInput` this is modelled on,
 * where blur commits — because the expensive mistake is creating `Rust` while
 * meaning to pick `rust`, and that mistake is exactly a typo followed by a
 * click somewhere else.
 */
export interface CapabilityPickerProps {
	value: string[];
	onChange: (next: string[]) => void;
	/** Every label declared anywhere in the fleet, for suggestions. */
	suggestions: readonly string[];
	placeholder?: string;
	className?: string;
	/** Rendered under each suggestion — usually who declares it. */
	describeSuggestion?: (label: string) => string | undefined;
	disabled?: boolean;
	inputId?: string;
}

export function CapabilityPicker({
	value,
	onChange,
	suggestions,
	placeholder = "Add a capability…",
	className,
	describeSuggestion,
	disabled,
	inputId,
}: CapabilityPickerProps) {
	const [draft, setDraft] = useState("");
	const [open, setOpen] = useState(false);
	const [active, setActive] = useState(0);
	const containerRef = useRef<HTMLDivElement>(null);

	const query = draft.trim();

	// Existing labels not already chosen, filtered by what has been typed.
	// Case-insensitive *matching* so a search finds `Rust` when you type `ru`;
	// the label added is always the fleet's own spelling, never the query's.
	const existing = useMemo(() => {
		const chosen = new Set(value);
		return suggestions
			.filter((label) => !chosen.has(label))
			.filter((label) =>
				query === "" ? true : label.toLowerCase().includes(query.toLowerCase()),
			);
	}, [suggestions, value, query]);

	// Offered only when the typed label is not already a fleet label *exactly*.
	// An exact match that differs in case still offers creation, because that is
	// a real and different capability — but the existing spelling is listed
	// above it, which is the point.
	const canCreate =
		query !== "" && !suggestions.includes(query) && !value.includes(query);

	// A label that differs from the query only in case. This is the exact drift
	// the design calls out — `rust` and `Rust` are two capabilities and one of
	// them matches nothing — and filtering alone does not catch it: when the
	// clashing label is one this agent already holds it is filtered out of the
	// suggestions entirely, so the create row would be the *only* thing on
	// screen and would look like the obvious choice. Named explicitly instead.
	const caseClash = canCreate
		? [...suggestions, ...value].find(
				(label) =>
					label !== query && label.toLowerCase() === query.toLowerCase(),
			)
		: undefined;

	const options = useMemo(
		() => [
			...existing.map((label) => ({kind: "existing" as const, label})),
			...(canCreate ? [{kind: "create" as const, label: query}] : []),
		],
		[existing, canCreate, query],
	);

	// Enter must land on an existing label whenever one matches, so the default
	// path is reuse and creation costs an extra deliberate keystroke.
	useEffect(() => {
		setActive(0);
	}, [query]);

	useEffect(() => {
		if (!open) return;
		const onDocMouseDown = (event: globalThis.MouseEvent) => {
			if (!containerRef.current?.contains(event.target as Node)) setOpen(false);
		};
		document.addEventListener("mousedown", onDocMouseDown);
		return () => document.removeEventListener("mousedown", onDocMouseDown);
	}, [open]);

	const add = (label: string) => {
		const trimmed = label.trim();
		if (trimmed === "" || value.includes(trimmed)) return;
		// Sorted to match the server's `normalise_capabilities`, so a set the UI
		// shows and a set the server stores never differ only in order.
		onChange([...value, trimmed].sort());
		setDraft("");
		setActive(0);
	};

	const remove = (label: string) => onChange(value.filter((it) => it !== label));

	const handleKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
		if (event.key === "ArrowDown") {
			event.preventDefault();
			setOpen(true);
			setActive((i) => Math.min(i + 1, options.length - 1));
			return;
		}
		if (event.key === "ArrowUp") {
			event.preventDefault();
			setActive((i) => Math.max(i - 1, 0));
			return;
		}
		if (event.key === "Enter") {
			event.preventDefault();
			const option = options[active];
			if (option) add(option.label);
			return;
		}
		if (event.key === "Escape") {
			setOpen(false);
			return;
		}
		// Backspace on an empty box removes the last chip — the one gesture worth
		// keeping from the plain tag input, because it undoes rather than creates.
		if (event.key === "Backspace" && draft === "" && value.length > 0) {
			remove(value[value.length - 1]);
		}
	};

	return (
		<div className={className} ref={containerRef}>
			<div
				className={`flex flex-wrap items-center gap-1.5 rounded-md border border-app-line/50 bg-app-dark-box/30 p-1.5 focus-within:border-accent/50 ${
					disabled ? "opacity-50" : ""
				}`}
			>
				{value.map((label) => (
					<span
						key={label}
						className="flex items-center gap-1 rounded border border-accent/30 bg-accent/10 px-1.5 py-0.5 font-mono text-[11px] leading-4 text-accent"
					>
						{label}
						<button
							type="button"
							disabled={disabled}
							onClick={() => remove(label)}
							aria-label={`Remove ${label}`}
							className="text-accent/60 transition-colors hover:text-accent"
						>
							<FontAwesomeIcon icon={faXmark} className="text-[10px]" />
						</button>
					</span>
				))}
				<input
					id={inputId}
					type="text"
					value={draft}
					disabled={disabled}
					onChange={(event) => {
						setDraft(event.target.value);
						setOpen(true);
					}}
					onFocus={() => setOpen(true)}
					onKeyDown={handleKeyDown}
					placeholder={value.length === 0 ? placeholder : ""}
					className="min-w-[120px] flex-1 border-none bg-transparent text-[11px] text-ink outline-none placeholder:text-ink-faint"
				/>
			</div>

			{open && options.length > 0 && (
				<div className="relative">
					<div className="absolute z-50 mt-1 max-h-56 w-full overflow-y-auto rounded-md border border-app-line bg-app-box shadow-lg">
						{existing.length > 0 && (
							<div className="border-b border-app-line/30 bg-app-box/95 px-2.5 py-1 text-[10px] font-semibold uppercase tracking-wide text-ink-dull">
								Declared in the fleet
							</div>
						)}
						{options.map((option, index) => {
							const isActive = index === active;
							if (option.kind === "create") {
								return (
									<button
										key="__create__"
										type="button"
										onMouseEnter={() => setActive(index)}
										onMouseDown={(event) => {
											event.preventDefault();
											add(option.label);
										}}
										className={`flex w-full items-start gap-2 border-t border-app-line/30 px-2.5 py-1.5 text-left transition-colors ${
											isActive ? "bg-app-selected" : ""
										}`}
									>
										<FontAwesomeIcon
											icon={faPlus}
											className="mt-0.5 text-[10px] text-status-warning"
										/>
										<span className="flex min-w-0 flex-col">
											<span className="truncate font-mono text-[11px] text-status-warning">
												{option.label}
											</span>
											{/* Named as a new thing, not offered as a match. Case is
											    not folded anywhere in this feature, so a near-miss
											    of an existing label is a different capability and
											    the reader has to be told before they take it. */}
											{caseClash ? (
												<span className="text-[10px] text-status-error">
													Differs only in case from{" "}
													<span className="font-mono">{caseClash}</span> — these
													are two separate capabilities, and only one of them
													will match.
												</span>
											) : (
												<span className="text-[10px] text-ink-faint">
													New capability — nothing in the fleet declares this
													yet
												</span>
											)}
										</span>
									</button>
								);
							}
							const description = describeSuggestion?.(option.label);
							return (
								<button
									key={option.label}
									type="button"
									onMouseEnter={() => setActive(index)}
									onMouseDown={(event) => {
										event.preventDefault();
										add(option.label);
									}}
									className={`flex w-full flex-col items-start px-2.5 py-1.5 text-left transition-colors ${
										isActive ? "bg-app-selected" : ""
									}`}
								>
									<span className="truncate font-mono text-[11px] text-ink">
										{option.label}
									</span>
									{description && (
										<span className="truncate text-[10px] text-ink-faint">
											{description}
										</span>
									)}
								</button>
							);
						})}
					</div>
				</div>
			)}
		</div>
	);
}
