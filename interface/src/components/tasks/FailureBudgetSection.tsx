import { useState } from "react";
import {
	Input,
	OptionList,
	OptionListItem,
	Popover,
	SelectPill,
} from "@spacedrive/primitives";
import type { TaskItem } from "@/api/client";

/**
 * Last-resort budget, used only until `GET /tasks` has answered.
 *
 * The real number comes from the server as `default_failure_limit` on the task
 * list response, because a control that says "uses the default" without saying
 * what the default *is* tells a reader nothing they can act on — and a copy of
 * the number pinned in TypeScript would go quietly wrong the day the Rust
 * constant moves. That silent-drift shape is this codebase's most expensive
 * recurring bug, so the fallback exists for first paint and nothing else.
 */
const ASSUMED_FAILURE_LIMIT = 2;

/**
 * Budgets offered without typing. Small numbers, because the interesting
 * choices are "park immediately" and "give it a couple more goes" — anything
 * larger is a decision someone should have to type out.
 */
const PRESETS = [1, 2, 3, 4, 5, 8];

/** Below 1 the task parks on its first failure regardless, so 1 is the floor. */
const MIN_BUDGET = 1;

export interface FailureBudgetSectionProps {
	task: TaskItem;
	/** The instance default, as published on the task list response. */
	defaultLimit?: number;
	/**
	 * `null` returns the task to the instance default; a number overrides it.
	 * Not called at all when the reader only looks, so an unrelated edit
	 * elsewhere in the drawer never carries a budget with it.
	 */
	onChange: (maxRetries: number | null) => void;
	busy?: boolean;
}

export function FailureBudgetSection({
	task,
	defaultLimit,
	onChange,
	busy,
}: FailureBudgetSectionProps) {
	return (
		<FailureBudgetSectionView
			budget={task.max_retries ?? null}
			defaultLimit={defaultLimit}
			failures={task.consecutive_failures}
			parked={task.status === "blocked"}
			onChange={onChange}
			busy={busy}
		/>
	);
}

/**
 * How much failure this task is allowed before it stops and waits for a human.
 *
 * Deliberately never says "max retries". The column is named that, but the
 * server compares `consecutive_failures >= limit` *after* incrementing, so a
 * budget of 1 parks the task on its first failure and permits zero retries —
 * anyone reading "max retries: 1" and expecting a second attempt would be
 * wrong. The count of failures already spent sits next to the budget for the
 * same reason: a limit nobody can see the consumption of is not something you
 * can act on until it has already fired.
 *
 * Split from the fetching wrapper so it can be rendered against fixtures.
 */
export function FailureBudgetSectionView({
	budget,
	defaultLimit = ASSUMED_FAILURE_LIMIT,
	failures,
	parked,
	onChange,
	busy,
}: {
	/** The task's own override, or `null` when it rides the instance default. */
	budget: number | null;
	/** The instance default, from the server. */
	defaultLimit?: number;
	failures: number;
	/** Whether the task is already parked, so the budget reads as spent. */
	parked?: boolean;
	onChange?: (maxRetries: number | null) => void;
	busy?: boolean;
}) {
	const limit = budget ?? defaultLimit;
	const remaining = Math.max(0, limit - failures);
	const exhausted = remaining === 0;

	return (
		<div className="border-t border-app-line/40 px-4 py-3">
			<h3 className="mb-2 text-xs font-medium uppercase tracking-wide text-ink-dull">
				Failure budget
			</h3>

			<p className="mb-2 text-[11px] leading-relaxed text-ink-faint">
				Consecutive failures this task is allowed before it stops and waits for
				a person. At {MIN_BUDGET} it parks on its first failure.
			</p>

			<div className="mb-2 flex items-center gap-2">
				{onChange ? (
					<BudgetPicker
						budget={budget}
						defaultLimit={defaultLimit}
						onChange={onChange}
						busy={busy}
					/>
				) : (
					<span className="rounded bg-app-box/40 px-2 py-1 text-xs text-ink-dull">
						{budgetLabel(budget, defaultLimit)}
					</span>
				)}
				{budget != null && onChange && (
					<button
						type="button"
						disabled={busy}
						onClick={() => onChange(null)}
						className="text-[11px] text-ink-faint underline-offset-2 hover:text-ink-dull hover:underline disabled:opacity-50"
					>
						Reset to default
					</button>
				)}
			</div>

			<Consumption
				failures={failures}
				limit={limit}
				remaining={remaining}
				exhausted={exhausted}
				parked={parked}
			/>
		</div>
	);
}

/** Reads as a sentence, because the number alone is the part that misleads. */
function budgetLabel(budget: number | null, defaultLimit: number): string {
	if (budget == null) return `Default — stops after ${defaultLimit}`;
	return budget === 1 ? "Stops after the first" : `Stops after ${budget}`;
}

/**
 * What the budget has cost so far.
 *
 * Pips rather than a bare fraction: "1 of 2" needs reading, two squares with
 * one filled does not. Long budgets fall back to the fraction alone — twenty
 * squares in a 400px drawer is noise.
 */
function Consumption({
	failures,
	limit,
	remaining,
	exhausted,
	parked,
}: {
	failures: number;
	limit: number;
	remaining: number;
	exhausted: boolean;
	parked?: boolean;
}) {
	const tone = exhausted
		? "text-status-error"
		: failures > 0
			? "text-status-warning"
			: "text-ink-faint";

	return (
		<div className="flex items-center gap-2">
			{limit <= 10 && (
				<span className="flex shrink-0 items-center gap-0.5">
					{Array.from({ length: limit }, (_, index) => (
						<span
							key={index}
							className={`h-1.5 w-3 rounded-[1px] ${
								index < failures
									? exhausted
										? "bg-status-error"
										: "bg-status-warning"
									: "bg-app-line"
							}`}
						/>
					))}
				</span>
			)}
			<span className={`text-[11px] ${tone}`}>
				{failures === 0
					? `No failures since the last success — ${limit} allowed`
					: exhausted
						? parked
							? `${failures} of ${limit} spent — budget gone, parked for a person`
							: `${failures} of ${limit} spent — the next failure parks it`
						: `${failures} of ${limit} spent — ${remaining} left`}
			</span>
		</div>
	);
}

/**
 * Picks a budget, or hands the task back to the instance default.
 *
 * The three states the API distinguishes are all reachable: leaving this alone
 * sends nothing, "Instance default" sends an explicit `null`, and a preset or
 * typed number sends that number. The typed path exists because the column
 * takes any integer and the presets stop at 8.
 */
function BudgetPicker({
	budget,
	defaultLimit,
	onChange,
	busy,
}: {
	budget: number | null;
	defaultLimit: number;
	onChange: (maxRetries: number | null) => void;
	busy?: boolean;
}) {
	const [open, setOpen] = useState(false);
	const [typed, setTyped] = useState("");

	const parsed = /^\d+$/.test(typed.trim()) ? Number(typed.trim()) : null;
	const typedIsNew =
		parsed != null &&
		parsed >= MIN_BUDGET &&
		parsed !== budget &&
		!PRESETS.includes(parsed);

	const submit = (value: number | null) => {
		onChange(value);
		setOpen(false);
		setTyped("");
	};

	return (
		<Popover.Root open={open} onOpenChange={setOpen}>
			<Popover.Trigger asChild>
				<SelectPill size="sm" disabled={busy}>
					{busy ? "Saving…" : budgetLabel(budget, defaultLimit)}
				</SelectPill>
			</Popover.Trigger>
			<Popover.Content align="start" sideOffset={4} className="w-[260px] p-1.5">
				<Input
					autoFocus
					size="sm"
					value={typed}
					onChange={(event) => setTyped(event.target.value)}
					onKeyDown={(event) => {
						if (event.key === "Enter" && typedIsNew) submit(parsed);
					}}
					placeholder="Or type a number…"
					className="mb-1.5"
				/>
				<OptionList className="max-h-64 overflow-y-auto">
					{typedIsNew && (
						<OptionListItem size="sm" onClick={() => submit(parsed)}>
							Stops after {parsed}
						</OptionListItem>
					)}
					<OptionListItem
						size="sm"
						selected={budget == null}
						onClick={() => submit(null)}
					>
						<span className="flex min-w-0 items-baseline gap-2">
							<span>Instance default</span>
							<span className="text-ink-faint">
								stops after {defaultLimit}
							</span>
						</span>
					</OptionListItem>
					{PRESETS.map((value) => (
						<OptionListItem
							key={value}
							size="sm"
							selected={budget === value}
							onClick={() => submit(value)}
						>
							<span className="flex min-w-0 items-baseline gap-2">
								<span>{budgetLabel(value, defaultLimit)}</span>
								{value === 1 && (
									<span className="text-ink-faint">no retries at all</span>
								)}
							</span>
						</OptionListItem>
					))}
				</OptionList>
			</Popover.Content>
		</Popover.Root>
	);
}
