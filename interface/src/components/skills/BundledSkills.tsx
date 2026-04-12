import {cx} from "class-variance-authority";
import {Package, Lightning} from "@phosphor-icons/react";
import type {SelectedSkill} from "./types";
import type {SkillInfo} from "@/api/client";

interface BundledSkillsProps {
	installedSkills: SkillInfo[];
	selectedSkill: SelectedSkill | null;
	onSelectSkill: (skill: SkillInfo) => void;
}

export function BundledSkills({installedSkills, selectedSkill, onSelectSkill}: BundledSkillsProps) {
	const builtinSkills = installedSkills.filter((s) => s.source === "builtin");
	const selectedName =
		selectedSkill?.type === "installed" ? selectedSkill.skill.name : null;

	return (
		<div className="flex h-full flex-col">
			<div className="border-b border-app-line/50 px-5 py-3">
				<h2 className="text-sm font-medium text-ink">Built-in Skills</h2>
				<p className="mt-0.5 text-xs text-ink-faint">
					Skills compiled into the agent binary. These are always available and
					cannot be removed.
				</p>
			</div>

			<div className="flex-1 overflow-y-auto px-5 py-4">
				{builtinSkills.length === 0 && (
					<div className="flex flex-col items-center gap-3 py-12 text-center">
						<Lightning className="size-8 text-ink-dull/30" weight="duotone" />
						<div>
							<p className="text-sm text-ink-dull">No built-in skills yet</p>
							<p className="mt-1 text-xs text-ink-faint">
								Built-in skills will appear here as they are added to the binary.
							</p>
						</div>
					</div>
				)}
				{builtinSkills.length > 0 && (
					<div className="space-y-1">
						{builtinSkills.map((skill) => (
							<button
								key={skill.name}
								onClick={() => onSelectSkill(skill)}
								className={cx(
									"flex w-full items-center gap-3 rounded-md px-3 py-2.5 text-left transition-colors",
									selectedName === skill.name
										? "bg-app-line text-ink"
										: "hover:bg-app-dark-box/40 text-ink-dull hover:text-ink",
								)}
							>
								<div className="flex size-7 shrink-0 items-center justify-center rounded-md bg-app-dark-box/60">
									<Package className="size-3.5 text-ink-faint" weight="duotone" />
								</div>
								<div className="min-w-0 flex-1">
									<div className="truncate text-sm font-medium text-ink">
										{skill.name}
									</div>
									<div className="truncate text-xs text-ink-faint">
										{skill.description}
									</div>
								</div>
								<span className="shrink-0 rounded bg-app-dark-box/80 px-1.5 py-0.5 text-[10px] text-ink-dull">
									builtin
								</span>
							</button>
						))}
					</div>
				)}
			</div>
		</div>
	);
}
