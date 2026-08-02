import { useQuery } from "@tanstack/react-query";
import { FontAwesomeIcon } from "@fortawesome/react-fontawesome";
import { faArrowLeftLong, faArrowRightLong } from "@fortawesome/free-solid-svg-icons";
import { api, type TaskDependenciesResponse } from "@/api/client";

export interface DependencySectionProps {
	taskNumber: number;
	/** Opens another task in the drawer. */
	onSelectTask?: (taskNumber: number) => void;
}

export function DependencySection({
	taskNumber,
	onSelectTask,
}: DependencySectionProps) {
	const { data, isLoading } = useQuery({
		queryKey: ["task-dependencies", taskNumber],
		queryFn: () => api.listTaskDependencies(taskNumber),
	});

	if (isLoading || !data) return null;
	return <DependencySectionView data={data} onSelectTask={onSelectTask} />;
}

/** Split from the fetching wrapper so it can be rendered against fixtures. */
export function DependencySectionView({
	data,
	onSelectTask,
}: {
	data: TaskDependenciesResponse;
	onSelectTask?: (taskNumber: number) => void;
}) {
	const { parents, children, blocked_by } = data;
	if (parents.length === 0 && children.length === 0) return null;

	const blocking = new Set(blocked_by);

	return (
		<div className="border-t border-app-line/40 px-4 py-3">
			<h3 className="mb-2 text-xs font-medium uppercase tracking-wide text-ink-dull">
				Dependencies
			</h3>

			{/* Naming the specific tasks is the point. "Blocked" tells you nothing
			    you could act on; "waiting on #41" tells you where to go. */}
			{blocked_by.length > 0 && (
				<p className="mb-2 rounded border border-status-warning/30 bg-status-warning/5 px-2 py-1.5 text-xs text-status-warning">
					Waiting on{" "}
					{blocked_by.map((number, index) => (
						<span key={number}>
							{index > 0 && ", "}
							<TaskRef number={number} onSelect={onSelectTask} />
						</span>
					))}
				</p>
			)}

			{parents.length > 0 && (
				<EdgeList
					icon={faArrowLeftLong}
					label="Upstream"
					numbers={parents}
					blocking={blocking}
					onSelect={onSelectTask}
				/>
			)}
			{children.length > 0 && (
				<EdgeList
					icon={faArrowRightLong}
					label="Downstream"
					numbers={children}
					blocking={new Set()}
					onSelect={onSelectTask}
				/>
			)}
		</div>
	);
}

function EdgeList({
	icon,
	label,
	numbers,
	blocking,
	onSelect,
}: {
	icon: typeof faArrowLeftLong;
	label: string;
	numbers: number[];
	blocking: Set<number>;
	onSelect?: (taskNumber: number) => void;
}) {
	return (
		<div className="mb-1.5 flex items-start gap-2 text-xs">
			<span className="mt-px flex w-24 shrink-0 items-center gap-1 text-ink-faint">
				<FontAwesomeIcon icon={icon} className="text-[9px]" />
				{label}
			</span>
			<span className="flex flex-wrap gap-1">
				{numbers.map((number) => (
					<TaskRef
						key={number}
						number={number}
						onSelect={onSelect}
						pending={blocking.has(number)}
					/>
				))}
			</span>
		</div>
	);
}

function TaskRef({
	number,
	onSelect,
	pending,
}: {
	number: number;
	onSelect?: (taskNumber: number) => void;
	pending?: boolean;
}) {
	const className = `font-mono ${
		pending ? "text-status-warning" : "text-ink-dull"
	} ${onSelect ? "hover:underline" : ""}`;

	if (!onSelect) return <span className={className}>#{number}</span>;
	return (
		<button type="button" onClick={() => onSelect(number)} className={className}>
			#{number}
		</button>
	);
}
