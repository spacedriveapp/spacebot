import {useCallback, useState} from "react";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {faDiagramProject, faListUl} from "@fortawesome/free-solid-svg-icons";

export type WorkflowView = "canvas" | "list";

const STORAGE_KEY = "spacebot.workflows.view";

/**
 * Canvas or list, remembered.
 *
 * The canvas is the default because the shape of a pipeline is the thing worth
 * seeing first. The list is not a fallback for browsers that cannot draw one —
 * it is genuinely better past a dozen steps, where a graph becomes a thing to
 * pan around and a list is still one keyboard-navigable column you can read top
 * to bottom. Which of those someone wants is a property of how they work, not
 * of which workflow they happen to have open, so the choice is stored once
 * rather than per template.
 */
export function useWorkflowView(): [WorkflowView, (next: WorkflowView) => void] {
	const [view, setView] = useState<WorkflowView>(() => {
		try {
			return localStorage.getItem(STORAGE_KEY) === "list" ? "list" : "canvas";
		} catch {
			return "canvas";
		}
	});

	const choose = useCallback((next: WorkflowView) => {
		setView(next);
		try {
			localStorage.setItem(STORAGE_KEY, next);
		} catch {
			// A browser with storage denied still gets to switch views.
		}
	}, []);

	return [view, choose];
}

export function ViewToggle({
	value,
	onChange,
}: {
	value: WorkflowView;
	onChange: (next: WorkflowView) => void;
}) {
	return (
		<div className="flex shrink-0 overflow-hidden rounded border border-app-line">
			<ToggleButton
				active={value === "canvas"}
				onClick={() => onChange("canvas")}
				icon={faDiagramProject}
				label="Graph"
				hint="The steps as a graph, laid out by what waits for what"
			/>
			<ToggleButton
				active={value === "list"}
				onClick={() => onChange("list")}
				icon={faListUl}
				label="List"
				hint="The steps as one column, in the order they run"
			/>
		</div>
	);
}

function ToggleButton({
	active,
	onClick,
	icon,
	label,
	hint,
}: {
	active: boolean;
	onClick: () => void;
	icon: Parameters<typeof FontAwesomeIcon>[0]["icon"];
	label: string;
	hint: string;
}) {
	return (
		<button
			type="button"
			onClick={onClick}
			title={hint}
			aria-pressed={active}
			className={`flex items-center gap-1.5 px-2 py-1 text-[10px] ${
				active
					? "bg-accent text-white"
					: "text-ink-faint hover:bg-app-box/60 hover:text-ink-dull"
			}`}
		>
			<FontAwesomeIcon icon={icon} className="text-[9px]" />
			{label}
		</button>
	);
}
