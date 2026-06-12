import {cx} from "class-variance-authority";
import {CheckCircle, DownloadSimple} from "@phosphor-icons/react";
import type {RegistrySkill} from "@/api/client";

interface RegistrySkillRowProps {
	skill: RegistrySkill;
	isInstalled: boolean;
	isSelected: boolean;
	isInstalling: boolean;
	isRemoving: boolean;
	onClick: () => void;
	onInstall: () => void;
	onRemove: () => void;
}

function formatInstalls(n: number): string {
	if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
	if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
	return String(n);
}

export function RegistrySkillRow({
	skill,
	isInstalled,
	isSelected,
	isInstalling,
	isRemoving,
	onClick,
	onInstall,
	onRemove,
}: RegistrySkillRowProps) {
	return (
		<button
			onClick={onClick}
			className={cx(
				"flex h-[56px] w-full items-center gap-3 rounded-md px-3 text-left transition-colors",
				isSelected
					? "bg-app-line text-ink"
					: "hover:bg-app-dark-box/40 text-ink-dull hover:text-ink",
			)}
		>
			<div className="min-w-0 flex-1">
				<div className="flex items-center gap-2">
					<span className="truncate text-sm font-medium leading-tight text-ink">{skill.name}</span>
					{skill.installs > 0 && (
						<span className="shrink-0 text-[11px] leading-tight text-ink-faint">
							{formatInstalls(skill.installs)}
						</span>
					)}
				</div>
				<p className="mt-0.5 truncate text-xs leading-tight text-ink-faint">
					{skill.description || <span className="font-mono text-ink-dull/60">{skill.source}</span>}
				</p>
			</div>

			<button
				onClick={(e) => {
					e.stopPropagation();
					if (isInstalled) onRemove();
					else onInstall();
				}}
				disabled={isInstalling || isRemoving}
				className={cx(
					"group shrink-0 rounded-md p-1.5 transition-colors",
					isInstalled
						? "text-green-400 hover:text-red-400"
						: "text-ink-faint hover:text-accent",
				)}
				title={isInstalled ? "Remove" : "Install"}
			>
				{isInstalling || isRemoving ? (
					<div className="h-4 w-4 animate-pulse rounded-full bg-current opacity-50" />
				) : isInstalled ? (
					<CheckCircle className="size-4" weight="fill" />
				) : (
					<DownloadSimple className="size-4" weight="bold" />
				)}
			</button>
		</button>
	);
}
