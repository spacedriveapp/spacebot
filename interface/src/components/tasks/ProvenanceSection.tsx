import { useQuery } from "@tanstack/react-query";
import { FontAwesomeIcon } from "@fortawesome/react-fontawesome";
import { faCodeBranch, faSitemap } from "@fortawesome/free-solid-svg-icons";
import { api, type TaskProvenanceResponse } from "@/api/client";

export interface ProvenanceSectionProps {
	taskNumber: number;
	onSelectTask?: (taskNumber: number) => void;
}

export function ProvenanceSection({
	taskNumber,
	onSelectTask,
}: ProvenanceSectionProps) {
	const { data } = useQuery({
		queryKey: ["task-provenance", taskNumber],
		queryFn: () => api.getTaskProvenance(taskNumber),
	});

	if (!data) return null;
	return <ProvenanceSectionView data={data} onSelectTask={onSelectTask} />;
}

/**
 * Where a card came from and what it spawned.
 *
 * A worker-filed card is otherwise indistinguishable from one a human wrote,
 * which makes a board that suddenly grew six new items impossible to explain.
 * Both directions matter: "who asked for this" and "what did this ask for".
 */
export function ProvenanceSectionView({
	data,
	onSelectTask,
}: {
	data: TaskProvenanceResponse;
	onSelectTask?: (taskNumber: number) => void;
}) {
	const { filed_by_task_number, filed, remaining_fan_out } = data;

	// A human-created card that spawned nothing has no provenance to show.
	if (filed_by_task_number == null && filed.length === 0) return null;

	return (
		<div className="border-t border-app-line/40 px-4 py-3">
			<h3 className="mb-2 text-xs font-medium uppercase tracking-wide text-ink-dull">
				Provenance
			</h3>

			{filed_by_task_number != null && (
				<p className="mb-2 flex items-center gap-1.5 text-xs text-ink-faint">
					<FontAwesomeIcon icon={faCodeBranch} className="text-[10px]" />
					Filed by{" "}
					<TaskRef number={filed_by_task_number} onSelect={onSelectTask} />
					<span>while it ran</span>
				</p>
			)}

			{filed.length > 0 && (
				<div>
					<div className="mb-1 flex items-baseline gap-2">
						<h4 className="flex items-center gap-1.5 text-[11px] font-medium text-ink-faint">
							<FontAwesomeIcon icon={faSitemap} className="text-[9px]" />
							Filed {filed.length}
						</h4>
						{/* Showing the remaining budget makes a truncated
						    decomposition legible instead of looking like the
						    worker simply stopped caring. */}
						<span
							className={`text-[10px] ${
								remaining_fan_out === 0 ? "text-status-warning" : "text-ink-faint"
							}`}
						>
							{remaining_fan_out === 0
								? "fan-out limit reached"
								: `${remaining_fan_out} more allowed`}
						</span>
					</div>
					<ul className="space-y-0.5">
						{filed.map((task) => (
							<li key={task.id} className="flex items-baseline gap-2 text-xs">
								<TaskRef number={task.task_number} onSelect={onSelectTask} />
								<span className="min-w-0 flex-1 truncate text-ink-dull">
									{task.title}
								</span>
								<span className="shrink-0 font-mono text-[10px] text-ink-faint">
									{task.status}
								</span>
							</li>
						))}
					</ul>
				</div>
			)}
		</div>
	);
}

function TaskRef({
	number,
	onSelect,
}: {
	number: number;
	onSelect?: (taskNumber: number) => void;
}) {
	const className = "shrink-0 font-mono text-ink-dull";
	if (!onSelect) return <span className={className}>#{number}</span>;
	return (
		<button
			type="button"
			onClick={() => onSelect(number)}
			className={`${className} hover:underline`}
		>
			#{number}
		</button>
	);
}
