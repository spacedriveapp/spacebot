import {faListUl, faTableColumns} from "@fortawesome/free-solid-svg-icons";
import {ChoiceToggle, usePersistedChoice} from "@/lib/viewToggle";

/**
 * List or board, remembered.
 *
 * Which view someone wants is a property of how they work, not of the visit —
 * an operator watching a queue wants the board every time, and re-picking it on
 * every navigation is the kind of small friction that makes a view go unused.
 */
export type TaskViewMode = "list" | "board";

const STORAGE_KEY = "spacebot.tasks.viewMode";
const MODES: readonly TaskViewMode[] = ["list", "board"];

export function useTaskViewMode(): [TaskViewMode, (mode: TaskViewMode) => void] {
	return usePersistedChoice(STORAGE_KEY, MODES, "list");
}

export interface TaskViewToggleProps {
	value: TaskViewMode;
	onChange: (mode: TaskViewMode) => void;
}

export function TaskViewToggle({value, onChange}: TaskViewToggleProps) {
	return (
		<ChoiceToggle
			value={value}
			onChange={onChange}
			ariaLabel="Task view"
			variant="segmented"
			options={[
				{value: "list", icon: faListUl, label: "List"},
				{value: "board", icon: faTableColumns, label: "Board"},
			]}
		/>
	);
}
