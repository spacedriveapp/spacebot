import {useState} from "react";
import {useMutation, useQuery, useQueryClient} from "@tanstack/react-query";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {
	faCodeBranch,
	faHourglassHalf,
	faTriangleExclamation,
	faXmark,
} from "@fortawesome/free-solid-svg-icons";
import {Button} from "@spacedrive/primitives";
import {api, type TaskGate, type TaskItem} from "@/api/client";
import {
	DISPOSITION_HINT,
	RESULT_DOT,
	RESULT_HINT,
	RESULT_LABEL,
	RESULT_TONE,
	describeCondition,
	effectiveDisposition,
	needsAPerson,
} from "@/components/workflows/conditions";

/**
 * What this task is waiting on outside the graph, and what a *no* would mean.
 *
 * A dependency edge and a condition are both reasons a task is not running, and
 * only one of them was visible in this drawer. A task sitting in the backlog
 * with every parent finished and no explanation is the exact confusion the
 * whole conditions feature exists to remove — and it was still on screen here,
 * because nothing rendered the gates the poller was evaluating.
 *
 * The verdict is shown as well as the predicate, because they answer different
 * questions. The predicate says what is being asked; `last_result` says whether
 * anybody has managed to get an answer, which is the difference between a task
 * that is progressing and one that has quietly stopped.
 */
export function TaskConditionsSection({
	taskNumber,
	task,
}: {
	taskNumber: number;
	/** The task itself, so a condition that already settled it can say so. */
	task?: TaskItem;
}) {
	const queryClient = useQueryClient();
	const {data} = useQuery({
		queryKey: ["task-gates", taskNumber],
		queryFn: () => api.listTaskGates(taskNumber),
	});
	const [confirmRemove, setConfirmRemove] = useState<string | null>(null);

	const remove = useMutation({
		mutationFn: (gateId: string) => api.removeTaskGate(taskNumber, gateId),
		onSuccess: () => {
			void queryClient.invalidateQueries({queryKey: ["task-gates", taskNumber]});
			// Removing the last unsatisfied condition can make the task promotable
			// on the next sweep, so the board is no longer what it was.
			void queryClient.invalidateQueries({queryKey: ["tasks"]});
			setConfirmRemove(null);
		},
	});

	const gates = data?.gates ?? [];
	// Nothing rather than an empty heading: most tasks have no conditions, and a
	// permanent "Conditions — none" is a row of chrome on every drawer.
	if (gates.length === 0) return null;

	return (
		<div className="border-t border-app-line/40 px-4 py-3">
			<h3 className="mb-2 text-xs font-medium uppercase tracking-wide text-ink-dull">
				Conditions
			</h3>
			<ul className="flex flex-col gap-2">
				{gates.map((gate) => (
					<ConditionRow
						key={gate.id}
						gate={gate}
						ruledOut={ruledOutBy(task, gate)}
						confirming={confirmRemove === gate.id}
						busy={remove.isPending}
						onConfirm={() =>
							setConfirmRemove(confirmRemove === gate.id ? null : gate.id)
						}
						onRemove={() => remove.mutate(gate.id)}
					/>
				))}
			</ul>
			{remove.error instanceof Error && (
				<p className="mt-2 break-words rounded border border-status-error/30 bg-status-error/5 px-2 py-1 font-mono text-[11px] text-status-error">
					{remove.error.message}
				</p>
			)}
		</div>
	);
}

/**
 * Whether *this* condition is the one that settled the task.
 *
 * The condition records `routed` once it settles a task, so this is read from
 * the gate rather than inferred.
 *
 * It used to be matched by looking for the gate's label inside `skip_reason`,
 * because a routing gate left `pending` behind — it never became false in the
 * "definitively answered no" sense, it became irrelevant. That heuristic could
 * not identify an *unlabelled* condition, which then read "Not yet" beside a
 * task settled forever. The backend now records the verdict, so the guess is
 * gone: a task may carry several conditions and this names exactly the one
 * that did it.
 */
function ruledOutBy(task: TaskItem | undefined, gate: TaskGate): boolean {
	if (!task || task.status !== "skipped") return false;
	return gate.last_result === "routed";
}

function ConditionRow({
	gate,
	ruledOut,
	confirming,
	busy,
	onConfirm,
	onRemove,
}: {
	gate: TaskGate;
	ruledOut: boolean;
	confirming: boolean;
	busy: boolean;
	onConfirm: () => void;
	onRemove: () => void;
}) {
	const disposition = effectiveDisposition(gate);
	const routes = disposition === "route";
	// A settled task is not waiting for anybody, whatever the gate row says.
	const stuck = !ruledOut && needsAPerson(gate);

	return (
		<li className="rounded border border-app-line bg-app-box/40 px-2 py-1.5">
			<div className="flex items-start gap-1.5">
				<FontAwesomeIcon
					icon={routes ? faCodeBranch : faHourglassHalf}
					className={`mt-[3px] shrink-0 text-[9px] ${
						routes ? "text-status-warning" : "text-status-info"
					}`}
					title={DISPOSITION_HINT[disposition]}
				/>
				<div className="min-w-0 flex-1">
					<p className="break-words text-[11px] text-ink">
						{describeCondition(gate)}
					</p>
					<div className="mt-0.5 flex flex-wrap items-center gap-x-1.5 text-[10px] text-ink-faint">
						{ruledOut ? (
							<span
								className="inline-flex items-center gap-1"
								title="This is the condition that settled the task: it was not met, and it routes rather than waits, so the task was skipped instead of held."
							>
								<span className="size-1.5 shrink-0 rounded-full bg-ink-faint" />
								<span className="text-ink-dull">Ruled this task out</span>
							</span>
						) : (
							<>
								<span
									className="inline-flex items-center gap-1"
									title={RESULT_HINT[gate.last_result]}
								>
									<span
										className={`size-1.5 shrink-0 rounded-full ${RESULT_DOT[gate.last_result]}`}
									/>
									<span className={RESULT_TONE[gate.last_result]}>
										{RESULT_LABEL[gate.last_result]}
									</span>
								</span>
								<span>·</span>
								<span title={DISPOSITION_HINT[disposition]}>
									{routes
										? "skips this task if false"
										: "holds this task until true"}
								</span>
							</>
						)}
						{gate.last_checked_at && (
							<>
								<span>·</span>
								<span title={gate.last_checked_at}>
									checked every {gate.poll_interval_secs}s
								</span>
							</>
						)}
					</div>
					{gate.last_detail && (
						<p
							className="mt-0.5 break-words font-mono text-[10px] leading-relaxed text-ink-faint"
							title={gate.last_detail}
						>
							{gate.last_detail}
						</p>
					)}
				</div>
				<button
					type="button"
					onClick={onConfirm}
					title="Remove this condition"
					className="shrink-0 text-ink-faint hover:text-status-error"
				>
					<FontAwesomeIcon icon={faXmark} className="text-[10px]" />
				</button>
			</div>

			{/* A condition that will not open on its own is the case this section
			    exists for. `failed` is decided and polling will not change it;
			    `erroring` is our problem rather than an answer — it never rules a
			    task out by itself, which is exactly why it can sit there
			    indefinitely with nobody told. Both need a person, and the two are
			    worded apart because the thing to go and do is different. */}
			{stuck && (
				<p className="mt-1.5 flex items-start gap-1.5 border-t border-app-line/40 pt-1.5 text-[10px] text-status-warning">
					<FontAwesomeIcon
						icon={faTriangleExclamation}
						className="mt-0.5 shrink-0 text-[9px]"
					/>
					<span>
						{gate.last_result === "failed"
							? "Answered no and will not be asked again. This task is waiting for a person."
							: `Could not be evaluated ${gate.consecutive_errors} times running. That is a problem reaching it, not an answer from it — so it will never rule this task out on its own, and it needs a person.`}
					</span>
				</p>
			)}

			{confirming && (
				<div className="mt-1.5 flex flex-wrap items-center gap-2 border-t border-app-line/40 pt-1.5">
					<span className="text-[10px] text-ink-dull">
						{routes
							? "The task stops being conditional on this and becomes eligible to run."
							: "The task stops waiting for this."}{" "}
						There is no undo — the condition is deleted, not disabled.
					</span>
					<Button
						size="sm"
						variant="colored"
						className="border-status-error bg-status-error"
						disabled={busy}
						onClick={onRemove}
					>
						Remove condition
					</Button>
				</div>
			)}
		</li>
	);
}
