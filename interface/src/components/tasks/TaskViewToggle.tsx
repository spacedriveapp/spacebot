import {useCallback, useState} from "react";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {faListUl, faTableColumns} from "@fortawesome/free-solid-svg-icons";

/**
 * List or board, remembered.
 *
 * Which view someone wants is a property of how they work, not of the visit —
 * an operator watching a queue wants the board every time, and re-picking it on
 * every navigation is the kind of small friction that makes a view go unused.
 * localStorage rather than the server: it is a per-browser preference with no
 * consequence if it is lost, and a config round-trip to store it would be a
 * dead knob of the sort this codebase already has three of.
 */
export type TaskViewMode = "list" | "board";

const STORAGE_KEY = "spacebot.tasks.viewMode";

/** Read the stored mode, tolerating anything that isn't one of the two. */
function readStored(): TaskViewMode {
	try {
		return localStorage.getItem(STORAGE_KEY) === "board" ? "board" : "list";
	} catch {
		// Private-mode browsers throw on access. The default is not worth a crash.
		return "list";
	}
}

export function useTaskViewMode(): [TaskViewMode, (mode: TaskViewMode) => void] {
	// Read once during the initial render so the first paint is already the
	// right view — reading in an effect would flash the list on every load.
	const [mode, setMode] = useState<TaskViewMode>(readStored);

	const update = useCallback((next: TaskViewMode) => {
		setMode(next);
		try {
			localStorage.setItem(STORAGE_KEY, next);
		} catch {
			// Preference lost, view still switched. Nothing to tell the user.
		}
	}, []);

	return [mode, update];
}

export interface TaskViewToggleProps {
	value: TaskViewMode;
	onChange: (mode: TaskViewMode) => void;
}

export function TaskViewToggle({value, onChange}: TaskViewToggleProps) {
	return (
		<div
			role="group"
			aria-label="Task view"
			className="flex items-center rounded border border-app-line bg-app-box/40 p-0.5"
		>
			<ToggleButton
				icon={faListUl}
				label="List"
				selected={value === "list"}
				onClick={() => onChange("list")}
			/>
			<ToggleButton
				icon={faTableColumns}
				label="Board"
				selected={value === "board"}
				onClick={() => onChange("board")}
			/>
		</div>
	);
}

function ToggleButton({
	icon,
	label,
	selected,
	onClick,
}: {
	icon: typeof faListUl;
	label: string;
	selected: boolean;
	onClick: () => void;
}) {
	return (
		<button
			type="button"
			onClick={onClick}
			aria-pressed={selected}
			className={`inline-flex items-center gap-1.5 rounded px-2 py-1 text-xs transition-colors ${
				selected
					? "bg-app-box text-ink shadow-sm"
					: "text-ink-faint hover:text-ink-dull"
			}`}
		>
			<FontAwesomeIcon icon={icon} className="h-3 w-3" />
			{label}
		</button>
	);
}
