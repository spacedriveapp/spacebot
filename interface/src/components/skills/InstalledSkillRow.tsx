import {cx} from "class-variance-authority";
import {Badge} from "@spacedrive/primitives";
import type {SkillInfo} from "@/api/client";

interface InstalledSkillRowProps {
	skill: SkillInfo;
	isSelected: boolean;
	onClick: () => void;
}

export function InstalledSkillRow({
	skill,
	isSelected,
	onClick,
}: InstalledSkillRowProps) {
	return (
		<button
			onClick={onClick}
			className={cx(
				"flex h-[40px] w-full flex-col justify-center rounded-md px-2.5 text-left transition-colors",
				isSelected
					? "bg-app-dark-box text-ink"
					: "text-ink-dull hover:bg-app-dark-box/50 hover:text-ink",
			)}
		>
			<div className="flex items-center gap-1.5 min-w-0">
				<span className="truncate text-sm leading-tight font-medium">{skill.name}</span>
				<Badge size="sm">{skill.source}</Badge>
			</div>
			<span className="truncate text-xs leading-tight text-ink-faint">
				{skill.description || "\u00A0"}
			</span>
		</button>
	);
}
