import {SettingSidebarButton} from "@/ui/SettingSidebarButton";
import {SECTIONS} from "./constants";
import {getIdentityField} from "./utils";
import type {SectionId} from "./types";

interface ConfigSidebarProps {
	activeSection: SectionId;
	onSectionChange: (section: SectionId) => void;
	identityData: {soul: string | null; identity: string | null; role: string | null};
}

export function ConfigSidebar({
	activeSection,
	onSectionChange,
	identityData,
}: ConfigSidebarProps) {
	return (
		<div className="flex w-52 flex-shrink-0 flex-col border-r border-app-line/50 bg-app-dark-box/20 overflow-y-auto">
			{/* General Group */}
			<div className="flex flex-col gap-0.5 px-2 pt-3">
				{SECTIONS.filter((s) => s.group === "general").map((section) => (
					<SettingSidebarButton
						key={section.id}
						onClick={() => onSectionChange(section.id)}
						active={activeSection === section.id}
					>
						<span className="flex-1">{section.label}</span>
					</SettingSidebarButton>
				))}
			</div>

			{/* Identity Group */}
			<div className="px-3 pb-1 pt-4">
				<span className="text-tiny font-medium uppercase tracking-wider text-ink-faint">
					Identity
				</span>
			</div>
			<div className="flex flex-col gap-0.5 px-2">
				{SECTIONS.filter((s) => s.group === "identity").map((section) => {
					const hasContent = !!getIdentityField(identityData, section.id)?.trim();
					return (
						<SettingSidebarButton
							key={section.id}
							onClick={() => onSectionChange(section.id)}
							active={activeSection === section.id}
						>
							<span className="flex-1">{section.label}</span>
							{!hasContent && (
								<span className="rounded bg-amber-500/10 px-1 py-0.5 text-tiny text-amber-400/70">
									empty
								</span>
							)}
						</SettingSidebarButton>
					);
				})}
			</div>

			{/* Config Group */}
			<div className="px-3 pb-1 pt-4 mt-2">
				<span className="text-tiny font-medium uppercase tracking-wider text-ink-faint">
					Configuration
				</span>
			</div>
			<div className="flex flex-col gap-0.5 px-2">
				{SECTIONS.filter((s) => s.group === "config").map((section) => (
					<SettingSidebarButton
						key={section.id}
						onClick={() => onSectionChange(section.id)}
						active={activeSection === section.id}
					>
						<span className="flex-1">{section.label}</span>
					</SettingSidebarButton>
				))}
			</div>

			{/* Data Group */}
			<div className="px-3 pb-1 pt-4 mt-2">
				<span className="text-tiny font-medium uppercase tracking-wider text-ink-faint">
					Data
				</span>
			</div>
			<div className="flex flex-col gap-0.5 px-2 pb-3">
				{SECTIONS.filter((s) => s.group === "data").map((section) => (
					<SettingSidebarButton
						key={section.id}
						onClick={() => onSectionChange(section.id)}
						active={activeSection === section.id}
					>
						<span className="flex-1">{section.label}</span>
					</SettingSidebarButton>
				))}
			</div>
		</div>
	);
}
