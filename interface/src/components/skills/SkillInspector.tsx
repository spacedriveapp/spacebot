import {X, ArrowSquareOut, Trash} from "@phosphor-icons/react";
import {useQuery} from "@tanstack/react-query";
import {Button, Badge} from "@spacedrive/primitives";
import {api} from "@/api/client";
import type {SelectedSkill} from "./types";

interface SkillInspectorProps {
	agentId: string;
	selected: SelectedSkill;
	onClose: () => void;
	onInstall: (spec: string) => void;
	onRemove: (name: string) => void;
	installedNames: Set<string>;
	isInstalling: boolean;
	isRemoving: boolean;
	removingName: string | null;
}

function SkillContentBlock({content}: {content: string}) {
	return (
		<div className="rounded-md border border-app-line bg-app-dark-box">
			<div className="border-b border-app-line px-4 py-2">
				<span className="font-mono text-xs font-medium text-ink-faint">SKILL.md</span>
			</div>
			<pre className="overflow-x-auto whitespace-pre-wrap p-4 font-mono text-xs leading-relaxed text-ink-dull">
				{content}
			</pre>
		</div>
	);
}

export function SkillInspector({
	agentId,
	selected,
	onClose,
	onInstall,
	onRemove,
	installedNames,
	isInstalling,
	isRemoving,
	removingName,
}: SkillInspectorProps) {
	const isInstalled =
		selected.type === "installed" ||
		(selected.type === "registry" &&
			(installedNames.has(`${selected.skill.source}/${selected.skill.name}`.toLowerCase()) ||
				installedNames.has(selected.skill.name.toLowerCase())));

	const isBuiltin = selected.type === "installed" && selected.skill.source === "builtin";

	const installedContentQuery = useQuery({
		queryKey: ["skill-content", agentId, selected.type === "installed" ? selected.skill.name : null],
		queryFn: () => api.getSkillContent(agentId, (selected as {type: "installed"; skill: {name: string}}).skill.name),
		enabled: selected.type === "installed",
	});

	const registryContentQuery = useQuery({
		queryKey: [
			"registry-skill-content",
			selected.type === "registry" ? selected.skill.source : null,
			selected.type === "registry" ? selected.skill.skillId : null,
		],
		queryFn: () =>
			api.registrySkillContent(
				(selected as {type: "registry"; skill: {source: string; skillId: string}}).skill.source,
				(selected as {type: "registry"; skill: {source: string; skillId: string}}).skill.skillId,
			),
		enabled: selected.type === "registry",
	});

	const isContentLoading =
		(selected.type === "installed" && installedContentQuery.isLoading) ||
		(selected.type === "registry" && registryContentQuery.isLoading);

	const skillName = selected.skill.name;

	const skillDescription = selected.skill.description;

	const sourceRepo =
		selected.type === "installed"
			? selected.skill.source_repo
			: selected.type === "registry"
				? selected.skill.source
				: null;

	const content =
		selected.type === "installed"
			? installedContentQuery.data?.content
			: registryContentQuery.data?.content;

	const basePath =
		selected.type === "installed" ? installedContentQuery.data?.base_dir : null;

	return (
		<div className="flex h-full w-full flex-shrink-0 flex-col md:w-80 md:border-l md:border-app-line/50 md:bg-app-dark-box/10">
			{/* Header */}
			<div className="flex items-start justify-between gap-2 border-b border-app-line/50 px-4 py-3">
				<div className="min-w-0 flex-1">
					<div className="flex items-center gap-2">
						<h3 className="truncate text-sm font-semibold text-ink">{skillName}</h3>
						{selected.type === "installed" && (
							<Badge
								variant={isBuiltin ? "secondary" : selected.skill.source === "instance" ? "default" : "success"}
								size="sm"
								className="shrink-0"
							>
								{selected.skill.source}
							</Badge>
						)}
						{selected.type === "registry" && isInstalled && (
							<Badge variant="success" size="sm" className="shrink-0">
								installed
							</Badge>
						)}
					</div>
					{skillDescription && (
						<p className="mt-1 text-xs text-ink-faint">{skillDescription}</p>
					)}
				</div>
				<button
					onClick={onClose}
					className="shrink-0 rounded p-0.5 text-ink-faint transition-colors hover:text-ink"
				>
					<X className="size-4" weight="bold" />
				</button>
			</div>

			{/* Meta */}
			{(sourceRepo || basePath) && (
				<div className="space-y-1.5 border-b border-app-line/50 px-4 py-3">
					{sourceRepo && (
						<div className="flex items-center gap-1.5 text-xs text-ink-dull">
							<span className="text-ink-faint">Source:</span>
							<a
								href={`https://github.com/${sourceRepo}`}
								target="_blank"
								rel="noopener noreferrer"
								className="flex items-center gap-1 font-mono transition-colors hover:text-accent"
							>
								{sourceRepo}
								<ArrowSquareOut className="size-3" weight="bold" />
							</a>
						</div>
					)}
					{basePath && (
						<div className="flex items-start gap-1.5 text-xs text-ink-dull">
							<span className="shrink-0 text-ink-faint">Path:</span>
							<span className="font-mono break-all">{basePath}</span>
						</div>
					)}
					{selected.type === "registry" && selected.skill.installs > 0 && (
						<div className="text-xs text-ink-dull">
							<span className="text-ink-faint">Installs:</span>{" "}
							{selected.skill.installs.toLocaleString()}
						</div>
					)}
				</div>
			)}

			{/* Action — hidden for builtin skills */}
			{!isBuiltin && (
				<div className="border-b border-app-line/50 px-4 py-3">
					{selected.type === "installed" ? (
						<Button
							variant="outline"
							size="sm"
							onClick={() => onRemove(selected.skill.name)}
							disabled={isRemoving && removingName === selected.skill.name}
							className="w-full text-red-400 hover:text-red-400"
						>
							<Trash className="size-3.5" weight="bold" />
							{isRemoving && removingName === selected.skill.name
								? "Removing..."
								: "Remove Skill"}
						</Button>
					) : (
						<Button
							variant={isInstalled ? "outline" : "default"}
							size="sm"
							onClick={() => {
								if (isInstalled) {
									const name =
										selected.skill.name;
									onRemove(name);
								} else {
									onInstall(`${selected.skill.source}/${selected.skill.skillId}`);
								}
							}}
							disabled={isInstalling || isRemoving}
							className="w-full"
						>
							{isInstalling
								? "Installing..."
								: isInstalled
									? "Remove Skill"
									: "Install Skill"}
						</Button>
					)}
				</div>
			)}

			{/* Content */}
			<div className="flex-1 overflow-y-auto px-4 py-4">
				{isContentLoading && (
					<div className="flex items-center gap-2 text-xs text-ink-faint">
						<div className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
						Loading skill content...
					</div>
				)}
				{!isContentLoading && content && <SkillContentBlock content={content} />}
				{!isContentLoading && !content && selected.type === "registry" && (
					<p className="text-xs text-ink-faint">
						Could not fetch SKILL.md from GitHub.
					</p>
				)}
			</div>
		</div>
	);
}
