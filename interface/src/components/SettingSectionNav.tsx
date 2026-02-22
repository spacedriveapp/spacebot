import React from "react";
import {SettingSidebarButton} from "@/ui";

interface SectionGroup {
	label: string;
	sections: {
		id: string;
		label: string;
		badge?: React.ReactNode;
	}[];
}

interface SettingSectionNavProps {
	groups: SectionGroup[];
	activeSection: string;
	onSectionChange: (id: string) => void;
}

export function SettingSectionNav({groups, activeSection, onSectionChange}: SettingSectionNavProps) {
	return (
		<>
			{/* Desktop sidebar */}
			<div className="hidden md:flex w-52 flex-shrink-0 flex-col border-r border-app-line/50 bg-app-darkBox/20 overflow-y-auto">
				{groups.map((group) => (
					<React.Fragment key={group.label}>
						<div className="px-3 pb-1 pt-4">
							<span className="text-tiny font-medium uppercase tracking-wider text-ink-faint">
								{group.label}
							</span>
						</div>
						<div className="flex flex-col gap-0.5 px-2">
							{group.sections.map((section) => (
								<SettingSidebarButton
									key={section.id}
									onClick={() => onSectionChange(section.id)}
									active={activeSection === section.id}
								>
									<span className="flex-1">{section.label}</span>
									{section.badge}
								</SettingSidebarButton>
							))}
						</div>
					</React.Fragment>
				))}
			</div>

			{/* Mobile tab bar */}
			<div className="flex md:hidden h-10 items-stretch overflow-x-auto no-scrollbar border-b border-app-line/50 bg-app-darkBox/20 px-2">
				{groups.map((group, groupIndex) => (
					<React.Fragment key={group.label}>
						{groupIndex > 0 && (
							<div className="w-px bg-app-line/30 self-stretch my-1" />
						)}
						{group.sections.map((section) => {
							const isActive = activeSection === section.id;
							return (
								<button
									key={section.id}
									onClick={() => onSectionChange(section.id)}
									className={`relative whitespace-nowrap px-3 text-sm transition-colors ${
										isActive ? "text-ink" : "text-ink-faint hover:text-ink-dull"
									}`}
								>
									{section.label}
									{isActive && (
										<div className="absolute bottom-0 left-0 right-0 h-px bg-accent" />
									)}
								</button>
							);
						})}
					</React.Fragment>
				))}
			</div>
		</>
	);
}
