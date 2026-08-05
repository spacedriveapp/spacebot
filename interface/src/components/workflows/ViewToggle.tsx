import {faDiagramProject, faListUl} from "@fortawesome/free-solid-svg-icons";
import {ChoiceToggle, usePersistedChoice} from "@/lib/viewToggle";

export type WorkflowView = "canvas" | "list";

const STORAGE_KEY = "spacebot.workflows.view";
const VIEWS: readonly WorkflowView[] = ["canvas", "list"];

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
	return usePersistedChoice(STORAGE_KEY, VIEWS, "canvas");
}

export function ViewToggle({
	value,
	onChange,
}: {
	value: WorkflowView;
	onChange: (next: WorkflowView) => void;
}) {
	return (
		<ChoiceToggle
			value={value}
			onChange={onChange}
			ariaLabel="Workflow view"
			variant="joined"
			options={[
				{
					value: "canvas",
					icon: faDiagramProject,
					label: "Graph",
					hint: "The steps as a graph, laid out by what waits for what",
				},
				{
					value: "list",
					icon: faListUl,
					label: "List",
					hint: "The steps as one column, in the order they run",
				},
			]}
		/>
	);
}
