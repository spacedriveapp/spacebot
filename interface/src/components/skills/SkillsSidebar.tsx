import {useRef} from "react";
import {useVirtualizer} from "@tanstack/react-virtual";
import {Plus, Package, Books, Lightning} from "@phosphor-icons/react";
import {SettingSidebarButton} from "@/ui/SettingSidebarButton";
import {InstalledSkillRow} from "./InstalledSkillRow";
import type {SkillView, SelectedSkill} from "./types";
import type {SkillInfo} from "@/api/client";

interface SkillsSidebarProps {
	activeView: SkillView;
	onViewChange: (view: SkillView) => void;
	installedSkills: SkillInfo[];
	selectedSkill: SelectedSkill | null;
	onSelectInstalledSkill: (skill: SkillInfo) => void;
	isLoading: boolean;
}

const TOP_VIEWS: {view: SkillView; label: string; icon: React.ElementType}[] = [
	{view: "create", label: "Create Skill", icon: Plus},
	{view: "bundled", label: "Bundled Skills", icon: Package},
	{view: "directory", label: "Skills Directory", icon: Books},
];

export function SkillsSidebar({
	activeView,
	onViewChange,
	installedSkills,
	selectedSkill,
	onSelectInstalledSkill,
	isLoading,
}: SkillsSidebarProps) {
	const scrollRef = useRef<HTMLDivElement>(null);

	const virtualizer = useVirtualizer({
		count: installedSkills.length,
		getScrollElement: () => scrollRef.current,
		estimateSize: () => 40,
		overscan: 5,
	});

	const selectedInstalledName =
		selectedSkill?.type === "installed" ? selectedSkill.skill.name : null;

	return (
		<div className="flex w-full shrink-0 flex-col md:w-52 md:border-r md:border-app-line/50 md:bg-app-dark-box/20">
			{/* Top actions */}
			<div className="flex flex-col gap-0.5 px-2 pt-3">
				{TOP_VIEWS.map(({view, label, icon: Icon}) => (
					<SettingSidebarButton
						key={view}
						onClick={() => onViewChange(view)}
						active={activeView === view}
					>
						<Icon className="size-3.5 shrink-0" weight="bold" />
						<span className="flex-1">{label}</span>
					</SettingSidebarButton>
				))}
			</div>

			{/* Installed skills */}
			<div className="px-3 pb-1 pt-4">
				<span className="text-tiny font-medium uppercase tracking-wider text-ink-faint">
					Installed
					{installedSkills.length > 0 && (
						<span className="ml-1.5 text-ink-dull">({installedSkills.length})</span>
					)}
				</span>
			</div>

			<div ref={scrollRef} className="flex-1 overflow-y-auto px-2 pb-3">
				{isLoading && (
					<div className="px-2 py-3 text-xs text-ink-faint">Loading...</div>
				)}
				{!isLoading && installedSkills.length === 0 && (
					<div className="flex flex-col items-center gap-2 px-2 py-6 text-center">
						<Lightning className="size-5 text-ink-dull/40" weight="duotone" />
						<span className="text-xs text-ink-faint">No skills installed</span>
					</div>
				)}
				{installedSkills.length > 0 && (
					<div
						style={{
							height: `${virtualizer.getTotalSize()}px`,
							width: "100%",
							position: "relative",
						}}
					>
						{virtualizer.getVirtualItems().map((virtualItem) => {
							const skill = installedSkills[virtualItem.index]!;
							return (
								<div
									key={virtualItem.key}
									style={{
										position: "absolute",
										top: 0,
										left: 0,
										width: "100%",
										height: `${virtualItem.size}px`,
										transform: `translateY(${virtualItem.start}px)`,
									}}
								>
									<InstalledSkillRow
										skill={skill}
										isSelected={selectedInstalledName === skill.name}
										onClick={() => onSelectInstalledSkill(skill)}
									/>
								</div>
							);
						})}
					</div>
				)}
			</div>
		</div>
	);
}
