import { useMemo, useState, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { FontAwesomeIcon } from "@fortawesome/react-fontawesome";
import {
	faArrowLeftLong,
	faArrowRightLong,
	faDiagramProject,
	faXmark,
} from "@fortawesome/free-solid-svg-icons";
import {
	Button,
	Input,
	OptionList,
	OptionListItem,
	Popover,
	SelectPill,
} from "@spacedrive/primitives";
import { api, type TaskDependenciesResponse, type TaskItem } from "@/api/client";

export interface DependencySectionProps {
	taskNumber: number;
	/** Opens another task in the drawer. */
	onSelectTask?: (taskNumber: number) => void;
}

export function DependencySection({
	taskNumber,
	onSelectTask,
}: DependencySectionProps) {
	const queryClient = useQueryClient();
	const { data, isLoading } = useQuery({
		queryKey: ["task-dependencies", taskNumber],
		queryFn: () => api.listTaskDependencies(taskNumber),
	});

	/**
	 * An edge changes more than this panel.
	 *
	 * Gaining an unfinished parent blocks the child server-side, and the board's
	 * edge badges and blocked section are built from the task list — so the list
	 * is invalidated too, and so is the *other* task's panel, which now has a
	 * child it did not have a moment ago.
	 */
	const invalidate = (parentTaskNumber: number) => {
		void queryClient.invalidateQueries({
			queryKey: ["task-dependencies", taskNumber],
		});
		void queryClient.invalidateQueries({
			queryKey: ["task-dependencies", parentTaskNumber],
		});
		void queryClient.invalidateQueries({ queryKey: ["tasks"] });
	};

	const add = useMutation({
		mutationFn: (parentTaskNumber: number) =>
			api.addTaskDependency(taskNumber, parentTaskNumber),
		onSuccess: (_data, parentTaskNumber) => invalidate(parentTaskNumber),
	});

	const remove = useMutation({
		mutationFn: (parentTaskNumber: number) =>
			api.removeTaskDependency(taskNumber, parentTaskNumber),
		onSuccess: (_data, parentTaskNumber) => invalidate(parentTaskNumber),
	});

	if (isLoading || !data) return null;
	return (
		<DependencySectionView
			data={data}
			taskNumber={taskNumber}
			// The one click from a task to its whole graph. It lives on this
			// heading because the list underneath is the same relationship seen one
			// hop at a time — `#41, #42` answers "what is next", and the canvas
			// answers "what is this part of", which is the question two hops out
			// that a list of numbers cannot be read for.
			headerAction={<TaskGraphLink taskNumber={taskNumber} />}
			onSelectTask={onSelectTask}
			onAddParent={(parent) => add.mutate(parent)}
			onRemoveParent={(parent) => remove.mutate(parent)}
			adding={add.isPending}
			addError={add.error instanceof Error ? add.error.message : null}
			onDismissAddError={() => add.reset()}
			removingParent={remove.isPending ? (remove.variables ?? null) : null}
		/>
	);
}

/**
 * The graph canvas for this task, one click away.
 *
 * Mirrors the `/tasks?task=N` convention going the other way: both routes are
 * keyed on the task number, so the drawer and the canvas can always reach each
 * other regardless of which workflow — if any — the task came from.
 */
function TaskGraphLink({ taskNumber }: { taskNumber: number }) {
	return (
		<Link
			to="/tasks/$taskNumber/graph"
			params={{ taskNumber: String(taskNumber) }}
			className="inline-flex shrink-0 items-center gap-1 text-[11px] normal-case tracking-normal text-accent hover:underline"
			title="See every task connected to this one, drawn as a graph"
		>
			<FontAwesomeIcon icon={faDiagramProject} className="text-[9px]" />
			Graph
		</Link>
	);
}

/** Split from the fetching wrapper so it can be rendered against fixtures. */
export function DependencySectionView({
	data,
	taskNumber,
	headerAction,
	onSelectTask,
	onAddParent,
	onRemoveParent,
	adding,
	addError,
	onDismissAddError,
	removingParent,
}: {
	data: TaskDependenciesResponse;
	/** Required to author edges; the fixture harness renders read-only. */
	taskNumber?: number;
	/**
	 * Rendered beside the heading. Kept as a prop rather than built here so this
	 * component stays renderable without a router — the UI lab mounts it against
	 * fixtures, and a `<Link>` outside a router throws.
	 */
	headerAction?: ReactNode;
	onSelectTask?: (taskNumber: number) => void;
	onAddParent?: (parentTaskNumber: number) => void;
	onRemoveParent?: (parentTaskNumber: number) => void;
	adding?: boolean;
	addError?: string | null;
	onDismissAddError?: () => void;
	removingParent?: number | null;
}) {
	const { parents, children, blocked_by } = data;
	const editable = onAddParent != null && taskNumber != null;

	// A task with no edges is exactly the one somebody may need to wire up, so
	// the empty section still renders once there is a way to add an edge.
	if (parents.length === 0 && children.length === 0 && !editable) return null;

	const blocking = new Set(blocked_by);

	return (
		<div className="border-t border-app-line/40 px-4 py-3">
			<div className="mb-2 flex items-baseline justify-between gap-2">
				<h3 className="text-xs font-medium uppercase tracking-wide text-ink-dull">
					Dependencies
				</h3>
				{headerAction}
			</div>

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
					onRemove={onRemoveParent}
					removing={removingParent}
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

			{editable && (
				<AddParent
					taskNumber={taskNumber}
					existingParents={parents}
					onAdd={onAddParent}
					adding={adding}
					error={addError ?? null}
					onDismissError={onDismissAddError}
				/>
			)}
		</div>
	);
}

/**
 * Add an upstream task this one must wait for.
 *
 * Only the parent side is authored here. Every edge is a parent from one end
 * and a child from the other, so offering both would be two ways to write the
 * same row — and "this task waits for X" is the direction a person actually
 * thinks in when they are looking at a card that ran too early.
 *
 * The picker lists titles rather than taking a bare number: the numbers are
 * not memorable, and typing #6 when you meant #8 produces a graph that is
 * wrong but perfectly valid, which nothing downstream will ever complain
 * about. Self and existing parents are filtered out so the only rejection the
 * server can still raise is a cycle, which it alone can decide.
 */
function AddParent({
	taskNumber,
	existingParents,
	onAdd,
	adding,
	error,
	onDismissError,
}: {
	taskNumber: number;
	existingParents: number[];
	onAdd: (parentTaskNumber: number) => void;
	adding?: boolean;
	error: string | null;
	onDismissError?: () => void;
}) {
	const [open, setOpen] = useState(false);
	const [filter, setFilter] = useState("");

	// The whole board, not the drawer's agent: an edge may point at any task.
	// Prefixed with "tasks" so the boards' own invalidations reach it.
	const { data } = useQuery({
		queryKey: ["tasks", "dependency-picker"],
		queryFn: () => api.listTasks({ limit: 200 }),
		staleTime: 15_000,
		enabled: open,
	});

	const candidates = useMemo(() => {
		const taken = new Set([taskNumber, ...existingParents]);
		const needle = filter.trim().toLowerCase();
		return (data?.tasks ?? [])
			.filter((task) => !taken.has(task.task_number))
			.filter(
				(task) =>
					needle === "" ||
					String(task.task_number).includes(needle) ||
					task.title.toLowerCase().includes(needle),
			)
			.slice(0, 40);
	}, [data, filter, taskNumber, existingParents]);

	// A number typed straight in, for a task older than the 200 the picker
	// loaded. Still filtered against self and existing parents.
	const typedNumber = /^#?\d+$/.test(filter.trim())
		? Number(filter.trim().replace("#", ""))
		: null;
	const typedIsNew =
		typedNumber != null &&
		typedNumber !== taskNumber &&
		!existingParents.includes(typedNumber) &&
		!candidates.some((task) => task.task_number === typedNumber);

	const submit = (parent: number) => {
		onDismissError?.();
		onAdd(parent);
		setOpen(false);
		setFilter("");
	};

	return (
		<div className="mt-2">
			<Popover.Root open={open} onOpenChange={setOpen}>
				<Popover.Trigger asChild>
					<SelectPill size="sm" disabled={adding}>
						{adding ? "Linking…" : "Add upstream task…"}
					</SelectPill>
				</Popover.Trigger>
				<Popover.Content align="start" sideOffset={4} className="w-[320px] p-1.5">
					<Input
						autoFocus
						size="sm"
						value={filter}
						onChange={(event) => setFilter(event.target.value)}
						placeholder="Filter by number or title…"
						className="mb-1.5"
					/>
					<OptionList className="max-h-64 overflow-y-auto">
						{typedIsNew && (
							<OptionListItem size="sm" onClick={() => submit(typedNumber)}>
								<span className="font-mono">#{typedNumber}</span>
								<span className="ml-2 text-ink-faint">not on this board</span>
							</OptionListItem>
						)}
						{candidates.map((task) => (
							<OptionListItem
								key={task.task_number}
								size="sm"
								onClick={() => submit(task.task_number)}
							>
								<PickerRow task={task} />
							</OptionListItem>
						))}
						{candidates.length === 0 && !typedIsNew && (
							<div className="px-2 py-1.5 text-xs text-ink-faint">
								{data ? "No matching tasks." : "Loading tasks…"}
							</div>
						)}
					</OptionList>
				</Popover.Content>
			</Popover.Root>

			{/* The server's own words. A cycle rejection names the exact path that
			    would close, which is the one thing that tells you which existing
			    edge to go delete — "failed to add dependency" tells you nothing. */}
			{error && (
				<div className="mt-2 flex items-start gap-2 rounded border border-status-error/30 bg-status-error/5 px-2 py-1.5">
					<p className="min-w-0 flex-1 break-words font-mono text-[11px] leading-relaxed text-status-error">
						{error}
					</p>
					{onDismissError && (
						<Button
							size="icon"
							variant="subtle"
							onClick={onDismissError}
							aria-label="Dismiss"
						>
							<FontAwesomeIcon icon={faXmark} className="text-[10px]" />
						</Button>
					)}
				</div>
			)}
		</div>
	);
}

function PickerRow({ task }: { task: TaskItem }) {
	return (
		<span className="flex min-w-0 items-baseline gap-2">
			<span className="shrink-0 font-mono text-ink-faint">
				#{task.task_number}
			</span>
			<span className="min-w-0 truncate">{task.title}</span>
		</span>
	);
}

function EdgeList({
	icon,
	label,
	numbers,
	blocking,
	onSelect,
	onRemove,
	removing,
}: {
	icon: typeof faArrowLeftLong;
	label: string;
	numbers: number[];
	blocking: Set<number>;
	onSelect?: (taskNumber: number) => void;
	/** Only upstream edges are removable — see AddParent. */
	onRemove?: (taskNumber: number) => void;
	removing?: number | null;
}) {
	return (
		<div className="mb-1.5 flex items-start gap-2 text-xs">
			<span className="mt-px flex w-24 shrink-0 items-center gap-1 text-ink-faint">
				<FontAwesomeIcon icon={icon} className="text-[9px]" />
				{label}
			</span>
			<span className="flex flex-wrap gap-1">
				{numbers.map((number) => (
					<span
						key={number}
						className={
							onRemove
								? "group inline-flex items-center gap-0.5 rounded bg-app-box/40 px-1"
								: undefined
						}
					>
						<TaskRef
							number={number}
							onSelect={onSelect}
							pending={blocking.has(number)}
						/>
						{onRemove && (
							<button
								type="button"
								onClick={() => onRemove(number)}
								disabled={removing === number}
								title={`Stop waiting on #${number}`}
								className="text-ink-faint opacity-0 transition-opacity hover:text-status-error focus:opacity-100 group-hover:opacity-100 disabled:opacity-50"
							>
								<FontAwesomeIcon icon={faXmark} className="text-[9px]" />
							</button>
						)}
					</span>
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
