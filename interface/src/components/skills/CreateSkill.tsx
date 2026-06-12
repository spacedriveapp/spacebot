import {Sparkle} from "@phosphor-icons/react";

export function CreateSkill() {
	return (
		<div className="flex h-full flex-col">
			<div className="border-b border-app-line/50 px-5 py-3">
				<h2 className="text-sm font-medium text-ink">Create Skill</h2>
				<p className="mt-0.5 text-xs text-ink-faint">
					Describe a skill and let the agent generate it for you.
				</p>
			</div>
			<div className="flex flex-1 flex-col items-center justify-center gap-4 p-8 text-center">
				<div className="flex size-12 items-center justify-center rounded-full bg-app-dark-box/60">
					<Sparkle className="size-6 text-ink-dull/40" weight="duotone" />
				</div>
				<div>
					<p className="text-sm font-medium text-ink-dull">Coming soon</p>
					<p className="mt-1 text-xs text-ink-faint">
						Skill creation via chat will be available in a future update.
					</p>
				</div>
			</div>
		</div>
	);
}
