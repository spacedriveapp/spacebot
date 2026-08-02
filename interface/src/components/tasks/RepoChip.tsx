import { FontAwesomeIcon } from "@fortawesome/react-fontawesome";
import { faCodeBranch, faFolderTree } from "@fortawesome/free-solid-svg-icons";
import type { TaskItem } from "@/api/client";

/**
 * Lookup tables for turning a task's binding ids into names.
 *
 * The board loads projects/repos/worktrees once and passes maps down, rather
 * than each chip fetching its own — a board of 200 tasks would otherwise fan
 * out into hundreds of requests.
 */
export interface BindingNames {
	projects: Map<string, string>;
	repos: Map<string, string>;
	worktrees: Map<string, string>;
}

export const EMPTY_BINDING_NAMES: BindingNames = {
	projects: new Map(),
	repos: new Map(),
	worktrees: new Map(),
};

export interface RepoChipProps {
	task: Pick<TaskItem, "project_id" | "repo_id" | "worktree_id">;
	names?: BindingNames;
	className?: string;
}

/**
 * Which codebase a task acts on, at a glance.
 *
 * Shows the most specific binding available: worktree beats repo beats project,
 * because that mirrors how the working directory is actually resolved. Renders
 * nothing for unbound tasks.
 */
export function RepoChip({ task, names, className }: RepoChipProps) {
	const lookup = names ?? EMPTY_BINDING_NAMES;

	// Most specific wins, matching resolve_directory_from_project's priority.
	if (task.worktree_id) {
		const label = lookup.worktrees.get(task.worktree_id) ?? task.worktree_id.slice(0, 8);
		return (
			<Chip icon={faCodeBranch} label={label} title="Worktree" className={className} />
		);
	}
	if (task.repo_id) {
		const label = lookup.repos.get(task.repo_id) ?? task.repo_id.slice(0, 8);
		return <Chip icon={faFolderTree} label={label} title="Repo" className={className} />;
	}
	if (task.project_id) {
		const label = lookup.projects.get(task.project_id) ?? task.project_id.slice(0, 8);
		return <Chip icon={faFolderTree} label={label} title="Project" className={className} />;
	}
	return null;
}

function Chip({
	icon,
	label,
	title,
	className,
}: {
	icon: typeof faCodeBranch;
	label: string;
	title: string;
	className?: string;
}) {
	return (
		<span
			title={`${title}: ${label}`}
			className={`inline-flex shrink-0 items-center gap-1 rounded border border-app-line bg-app-box/60 px-1.5 py-0.5 font-mono text-[10px] leading-none text-ink-dull ${className ?? ""}`}
		>
			<FontAwesomeIcon icon={icon} className="h-2.5 w-2.5 opacity-60" />
			{label}
		</span>
	);
}
