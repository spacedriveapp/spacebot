import { useState } from "react";
import {
	Popover,
	SelectPill,
	OptionList,
	OptionListItem,
} from "@spacedrive/primitives";
import type { BindingNames } from "./RepoChip";

/**
 * Filter the board down to one repo.
 *
 * The point of multi-repo work is being able to ask "what's outstanding in
 * `api-gateway`?" without reading a board that mixes four services together.
 */
export const ALL_REPOS = "all";

export interface RepoFilterProps {
	names: BindingNames;
	/** Currently selected repo id, or `ALL_REPOS`. */
	value: string;
	onChange: (repoId: string) => void;
	/** Repo ids that actually appear on the board, so empty repos aren't listed. */
	presentRepoIds: Set<string>;
}

export function RepoFilter({
	names,
	value,
	onChange,
	presentRepoIds,
}: RepoFilterProps) {
	const [open, setOpen] = useState(false);

	const options = [...presentRepoIds]
		.map((id) => ({ id, label: names.repos.get(id) ?? id.slice(0, 8) }))
		.sort((a, b) => a.label.localeCompare(b.label));

	// Nothing on the board is repo-bound — the filter would be noise.
	if (options.length === 0) return null;

	const selectedLabel =
		value === ALL_REPOS
			? "All repos"
			: (names.repos.get(value) ?? value.slice(0, 8));

	return (
		<Popover.Root open={open} onOpenChange={setOpen}>
			<Popover.Trigger asChild>
				<SelectPill size="sm">{selectedLabel}</SelectPill>
			</Popover.Trigger>
			<Popover.Content align="start" sideOffset={4} className="min-w-[180px] p-1.5">
				<OptionList>
					<OptionListItem
						selected={value === ALL_REPOS}
						size="sm"
						onClick={() => {
							onChange(ALL_REPOS);
							setOpen(false);
						}}
					>
						All repos
					</OptionListItem>
					{options.map((option) => (
						<OptionListItem
							key={option.id}
							selected={value === option.id}
							size="sm"
							onClick={() => {
								onChange(option.id);
								setOpen(false);
							}}
						>
							{option.label}
						</OptionListItem>
					))}
				</OptionList>
			</Popover.Content>
		</Popover.Root>
	);
}
