import {useCallback, useEffect, useMemo, useRef, useState} from "react";
import {useMutation, useQuery, useQueryClient} from "@tanstack/react-query";
import {
	api,
	type CreateTaskRequest,
	type TaskItem,
	type TaskStatus,
} from "@/api/client";
import {useLiveContext} from "@/hooks/useLiveContext";
import {
	Button,
	Popover,
	SelectPill,
	OptionList,
	OptionListItem,
} from "@spacedrive/primitives";
import {
	TaskList,
	TaskDetail,
	TaskCreateForm,
	type Task,
	type TaskStatus as UiTaskStatus,
	type TaskCreateFormData,
} from "@spacedrive/ai";
import {
	GithubMetadataBadges,
	getGithubReferences,
} from "@/components/TaskUtils";
import {BlockedTasksSection} from "@/components/tasks/BlockedTasksSection";
import {indexEdges} from "@/components/tasks/DependencyBadges";
import {TaskBoard} from "@/components/tasks/TaskBoard";
import {
	TaskViewToggle,
	useTaskViewMode,
} from "@/components/tasks/TaskViewToggle";
import {
	dependencyRefusal,
	namedDependencyRefusal,
} from "@/components/tasks/dependencyGate";
import {ContractSection} from "@/components/tasks/ContractSection";
import {toDesignSystemTask} from "@/components/tasks/designSystemTask";
import {BlockedBanner} from "@/components/tasks/BlockedBanner";
import {ProvenanceSection} from "@/components/tasks/ProvenanceSection";
import {DependencySection} from "@/components/tasks/DependencySection";
import {StatusMoves} from "@/components/tasks/StatusMoves";
import {
	planStatusChange,
	useTaskTransitions,
} from "@/components/tasks/taskTransitions";
import {TaskRunHistory} from "@/components/tasks/TaskRunHistory";
import {RepoChip} from "@/components/tasks/RepoChip";
import {ALL_REPOS, RepoFilter} from "@/components/tasks/RepoFilter";
import {useBindingNames} from "@/hooks/useBindingNames";

const TASK_LIMIT = 200;

function AgentPicker({
	agents,
	value,
	onChange,
}: {
	agents: {id: string; display_name?: string | null}[];
	value?: string;
	onChange: (id: string) => void;
}) {
	const [open, setOpen] = useState(false);
	const selected = agents.find((a) => a.id === value);

	return (
		<div className="flex items-center gap-2">
			<label className="text-xs text-ink-dull">Create as:</label>
			<Popover.Root open={open} onOpenChange={setOpen}>
				<Popover.Trigger asChild>
					<SelectPill size="sm">
						{selected?.display_name ?? selected?.id ?? "Select agent"}
					</SelectPill>
				</Popover.Trigger>
				<Popover.Content
					align="start"
					sideOffset={4}
					className="min-w-[180px] p-1.5"
				>
					<OptionList>
						{agents.map((agent) => (
							<OptionListItem
								key={agent.id}
								selected={agent.id === value}
								size="sm"
								onClick={() => {
									onChange(agent.id);
									setOpen(false);
								}}
							>
								{agent.display_name ?? agent.id}
							</OptionListItem>
						))}
					</OptionList>
				</Popover.Content>
			</Popover.Root>
		</div>
	);
}

export function GlobalTasks() {
	const queryClient = useQueryClient();
	const {taskEventVersion} = useLiveContext();

	const queryKey = ["tasks"];

	// SSE-driven cache invalidation
	const prevVersion = useRef(taskEventVersion);
	useEffect(() => {
		if (taskEventVersion !== prevVersion.current) {
			prevVersion.current = taskEventVersion;
			queryClient.invalidateQueries({queryKey});
		}
	}, [taskEventVersion, queryKey, queryClient]);

	const {data: agentsData} = useQuery({
		queryKey: ["agents"],
		queryFn: api.agents,
		staleTime: 10_000,
	});

	const agents = agentsData?.agents ?? [];
	const [selectedOwnerId, setSelectedOwnerId] = useState<string | undefined>();
	const effectiveOwner = selectedOwnerId ?? agents[0]?.id;

	const agentNameMap = useMemo(() => {
		const map: Record<string, string> = {};
		for (const agent of agents) {
			map[agent.id] = agent.display_name ?? agent.id;
		}
		return map;
	}, [agents]);

	const resolveAgentName = useCallback(
		(agentId: string) => agentNameMap[agentId] ?? agentId,
		[agentNameMap],
	);

	const {data, isLoading, error} = useQuery({
		queryKey,
		queryFn: () => api.listTasks({limit: TASK_LIMIT}),
		refetchInterval: 15_000,
	});

	const tasks = (data?.tasks ?? []) as unknown as Task[];

	// Edge counts arrive with the list, so badges cost no extra requests.
	const edgesByTask = useMemo(() => indexEdges(data?.edges), [data?.edges]);

	const {names: bindingNames} = useBindingNames();
	const [repoFilter, setRepoFilter] = useState<string>(ALL_REPOS);

	const rawTasks = (data?.tasks ?? []) as TaskItem[];

	// Repo ids present on the board, so the filter only lists repos with work.
	const presentRepoIds = useMemo(() => {
		const ids = new Set<string>();
		for (const task of rawTasks) {
			if (task.repo_id) ids.add(task.repo_id);
		}
		return ids;
	}, [rawTasks]);

	const matchesRepo = useCallback(
		(task: TaskItem) => repoFilter === ALL_REPOS || task.repo_id === repoFilter,
		[repoFilter],
	);

	// `blocked` is not in @spacedrive/ai's TaskStatus union, so those tasks are
	// split out and rendered by BlockedTasksSection instead. This split is the
	// list view's problem only — the board owns its own columns and gives
	// `blocked` a real one, so it takes the unsplit set below.
	const blockedTasks = useMemo(
		() => rawTasks.filter((t) => t.status === "blocked" && matchesRepo(t)),
		[rawTasks, matchesRepo],
	);
	const listViewTasks = useMemo(
		() =>
			tasks.filter((t) => {
				const item = t as unknown as TaskItem;
				return item.status !== "blocked" && matchesRepo(item);
			}),
		[tasks, matchesRepo],
	);
	const boardTasks = useMemo(
		() => rawTasks.filter(matchesRepo),
		[rawTasks, matchesRepo],
	);

	const [viewMode, setViewMode] = useTaskViewMode();

	const [activeTaskId, setActiveTaskId] = useState<string | null>(null);
	const [collapsedGroups, setCollapsedGroups] = useState<Set<UiTaskStatus>>(
		() => new Set(),
	);
	const [blockedCollapsed, setBlockedCollapsed] = useState(false);
	const [createOpen, setCreateOpen] = useState(false);

	const activeTask = tasks.find((t) => t.id === activeTaskId);

	const invalidate = useCallback(
		() => queryClient.invalidateQueries({queryKey}),
		[queryClient, queryKey],
	);

	const updateMutation = useMutation({
		mutationFn: ({
			taskNumber,
			...req
		}: {
			taskNumber: number;
			status?: TaskStatus;
			complete_subtask?: number;
		}) => api.updateTask(taskNumber, req),
		onSuccess: () => void invalidate(),
	});

	const approveMutation = useMutation({
		mutationFn: (taskNumber: number) => api.approveTask(taskNumber, "human"),
		onSuccess: () => void invalidate(),
	});

	const executeMutation = useMutation({
		mutationFn: (taskNumber: number) => api.executeTask(taskNumber),
		onSuccess: () => void invalidate(),
	});

	const deleteMutation = useMutation({
		mutationFn: (taskNumber: number) => api.deleteTask(taskNumber),
		onSuccess: () => {
			setActiveTaskId(null);
			void invalidate();
		},
	});

	const createMutation = useMutation({
		mutationFn: (req: CreateTaskRequest) => api.createTask(req),
		onSuccess: () => {
			setCreateOpen(false);
			void invalidate();
		},
	});

	const retryMutation = useMutation({
		mutationFn: (taskNumber: number) => api.retryTask(taskNumber),
		onSuccess: () => void invalidate(),
	});

	// Distinct from retry: retry re-runs the work, unblock says the obstacle a
	// human was asked about is gone. A missing credential is not fixed by
	// running the same task again.
	const unblockMutation = useMutation({
		mutationFn: (taskNumber: number) => api.unblockTask(taskNumber),
		onSuccess: () => void invalidate(),
	});

	// The store's legal moves. TaskList's row menu offers all five statuses it
	// knows regardless of the one the card is in, so the table is what stops a
	// doomed request from ever being sent.
	const transitions = useTaskTransitions();
	const [moveError, setMoveError] = useState<string | null>(null);

	// A late-arriving refusal must not overwrite a newer one. The enrichment
	// below is a request, so a second move can land while it is in flight.
	const moveErrorToken = useRef(0);

	/**
	 * Replace a count-based refusal with one that names the parents.
	 *
	 * `edges` only carries `blocked_by` as a number, so the immediate refusal
	 * can say "waiting on 2". The numbers live one request away, and "#3 Build
	 * and publish the api-gateway image" is the difference between a refusal
	 * someone can act on and one they can only be annoyed by. Shared query key
	 * with `DependencySection`, so opening the drawer afterwards is free.
	 */
	const nameOutstandingParents = useCallback(
		async (task: TaskItem, status: TaskStatus, token: number) => {
			try {
				const dependencies = await queryClient.fetchQuery({
					queryKey: ["task-dependencies", task.task_number],
					queryFn: () => api.listTaskDependencies(task.task_number),
				});
				const detailed = namedDependencyRefusal(
					task,
					status,
					dependencies,
					(number) =>
						rawTasks.find((candidate) => candidate.task_number === number)
							?.title,
				);
				if (detailed && moveErrorToken.current === token) {
					setMoveError(detailed);
				}
			} catch {
				// The count-based refusal is already on screen and is true. A
				// failed lookup is not worth replacing it with an error.
			}
		},
		[queryClient, rawTasks],
	);

	/**
	 * Show why a move was turned down, wherever the refusal was decided.
	 *
	 * The board turns some moves down itself — it is the only view where the
	 * target is a place the user pointed at — so this is shared rather than
	 * being reached only through `handleMove`. Enrichment is attempted for the
	 * dependency case alone, because it is the only refusal whose full text
	 * costs a request.
	 */
	const refuseMove = useCallback(
		(task: TaskItem, status: TaskStatus, reason: string) => {
			const token = ++moveErrorToken.current;
			setMoveError(reason);
			if (dependencyRefusal(task, status, edgesByTask.get(task.task_number))) {
				void nameOutstandingParents(task, status, token);
			}
		},
		[edgesByTask, nameOutstandingParents],
	);

	const handleMove = useCallback(
		(task: TaskItem, status: TaskStatus) => {
			const move = planStatusChange(task, status, transitions);
			if (move.action === "refuse") {
				refuseMove(task, status, move.reason);
				return;
			}

			// The second check, which nothing in this app made until now: the
			// transition table says `backlog → ready` is legal and the API
			// accepts it, but `claim_next_ready` re-checks the dependency
			// invariant and skips the card, so it sits in Ready forever looking
			// picked up. Refuse it here instead of letting the board lie.
			//
			// Unblock is exempt: it re-runs the same check server-side and lands
			// the task in `backlog` rather than `ready` when a parent is
			// outstanding, which is already the honest outcome.
			if (move.action !== "unblock") {
				const gated = dependencyRefusal(
					task,
					status,
					edgesByTask.get(task.task_number),
				);
				if (gated) {
					refuseMove(task, status, gated);
					return;
				}
			}

			setMoveError(null);
			switch (move.action) {
				// Leaving a blocked state is unblock's job, not a status write —
				// it has to clear the reason and re-check dependencies.
				case "unblock":
					unblockMutation.mutate(task.task_number);
					break;
				case "approve":
					approveMutation.mutate(task.task_number);
					break;
				case "execute":
					executeMutation.mutate(task.task_number);
					break;
				case "update":
					updateMutation.mutate({
						taskNumber: task.task_number,
						status: move.status,
					});
					break;
			}
		},
		[
			transitions,
			edgesByTask,
			refuseMove,
			updateMutation,
			approveMutation,
			executeMutation,
			unblockMutation,
		],
	);

	const handleStatusChange = useCallback(
		(task: Task, status: UiTaskStatus) => {
			// Resolve the real task by id: a blocked task reaches the drawer with
			// an adapted status, and branching on that would approve a task that
			// was never awaiting approval.
			const adapted = task as unknown as TaskItem;
			handleMove(rawTasks.find((c) => c.id === adapted.id) ?? adapted, status);
		},
		[handleMove, rawTasks],
	);

	const handleDelete = useCallback(
		(task: Task) => {
			deleteMutation.mutate((task as unknown as TaskItem).task_number);
		},
		[deleteMutation],
	);

	const handleSubtaskToggle = useCallback(
		(task: Task, index: number, _completed: boolean) => {
			updateMutation.mutate({
				taskNumber: (task as unknown as TaskItem).task_number,
				complete_subtask: index,
			});
		},
		[updateMutation],
	);

	const handleToggleGroup = useCallback((status: UiTaskStatus) => {
		setCollapsedGroups((prev) => {
			const next = new Set(prev);
			if (next.has(status)) next.delete(status);
			else next.add(status);
			return next;
		});
	}, []);

	const handleCreate = useCallback(
		(formData: TaskCreateFormData) => {
			if (!effectiveOwner) return;
			createMutation.mutate({
				owner_agent_id: effectiveOwner,
				title: formData.title,
				description: formData.description || undefined,
				priority: formData.priority,
				status: "backlog",
			});
		},
		[createMutation, effectiveOwner],
	);

	return (
		<div className="flex h-full w-full">
			{/* List panel */}
			<div className="flex min-w-0 flex-1 flex-col">
				{/* Toolbar */}
				<div className="flex items-center justify-between border-b border-app-line px-4 py-2">
					<div className="flex items-center gap-3">
						<TaskViewToggle value={viewMode} onChange={setViewMode} />
						<span className="text-sm text-ink-dull">
							{tasks.length} task{tasks.length !== 1 ? "s" : ""}
						</span>
						<RepoFilter
							names={bindingNames}
							value={repoFilter}
							onChange={setRepoFilter}
							presentRepoIds={presentRepoIds}
						/>
						{agents.length > 1 && (
							<AgentPicker
								agents={agents}
								value={effectiveOwner}
								onChange={setSelectedOwnerId}
							/>
						)}
					</div>
					{effectiveOwner && (
						<Button size="md" onClick={() => setCreateOpen(!createOpen)}>
							{createOpen ? "Cancel" : "Create Task"}
						</Button>
					)}
				</div>

				{/* Create form */}
				{createOpen && (
					<div className="border-b border-app-line px-3 py-2">
						<TaskCreateForm
							onSubmit={handleCreate}
							onCancel={() => setCreateOpen(false)}
							isSubmitting={createMutation.isPending}
						/>
					</div>
				)}

				{/* The row menu comes from @spacedrive/ai and cannot be narrowed to
				    the legal moves, so a refusal is explained here instead of
				    being sent and bounced by the API as a bare 400. */}
				{moveError && (
					<div className="flex items-start gap-2 border-b border-status-warning/30 bg-status-warning/5 px-4 py-2">
						<p className="min-w-0 flex-1 text-xs text-status-warning">
							{moveError}
						</p>
						<button
							type="button"
							onClick={() => setMoveError(null)}
							className="shrink-0 text-xs text-ink-faint hover:text-ink-dull"
						>
							Dismiss
						</button>
					</div>
				)}

				{/* Task list */}
				{isLoading ? (
					<div className="py-8 text-center text-sm text-ink-faint">
						Loading tasks...
					</div>
				) : error ? (
					<div className="py-8 text-center text-sm text-red-400">
						Failed to load tasks.
						<div className="mt-1 font-mono text-[10px] text-ink-faint">
							{(error as Error).message}
						</div>
					</div>
				) : tasks.length === 0 ? (
					<div className="flex flex-1 items-center justify-center">
						<div className="text-center">
							<p className="text-sm text-ink-dull">No tasks yet.</p>
							<p className="mt-1 text-xs text-ink-faint">
								Create one to get started.
							</p>
						</div>
					</div>
				) : viewMode === "board" ? (
					/* The board renders every status itself, so it takes the
					   unsplit set — no BlockedTasksSection, no adapted statuses.
					   That workaround layer exists for TaskList alone. */
					<div className="min-h-0 flex-1">
						<TaskBoard
							tasks={boardTasks}
							edges={edgesByTask}
							bindingNames={bindingNames}
							transitions={transitions}
							activeTaskId={activeTaskId}
							onTaskClick={(task) => setActiveTaskId(task.id)}
							onMove={handleMove}
							onRefuse={refuseMove}
							resolveAgentName={resolveAgentName}
						/>
					</div>
				) : (
					<div className="flex-1 overflow-y-auto">
						{/* Blocked tasks render separately: TaskList groups by
						    TASK_STATUS_ORDER and silently drops any status it
						    doesn't know, so a blocked task handed to it would
						    vanish from the board entirely. */}
						<BlockedTasksSection
							tasks={blockedTasks}
							collapsed={blockedCollapsed}
							onToggle={() => setBlockedCollapsed((v) => !v)}
							onRetry={(task) => retryMutation.mutate(task.task_number)}
							retryingTaskNumber={
								retryMutation.isPending
									? (retryMutation.variables ?? null)
									: null
							}
							onTaskClick={(task) => setActiveTaskId(task.id)}
							activeTaskId={activeTaskId}
							resolveAgentName={resolveAgentName}
							bindingNames={bindingNames}
							edges={edgesByTask}
							onUnblock={(task) => unblockMutation.mutate(task.task_number)}
						/>
						<TaskList
							tasks={listViewTasks}
							activeTaskId={activeTaskId ?? undefined}
							collapsedGroups={collapsedGroups}
							onToggleGroup={handleToggleGroup}
							onTaskClick={(task) => setActiveTaskId(task.id)}
							onStatusChange={handleStatusChange}
							onDelete={handleDelete}
							resolveAgentName={resolveAgentName}
						/>
					</div>
				)}
			</div>

			{/* Detail panel */}
			{activeTask && (
				<div className="w-[400px] shrink-0 overflow-y-auto border-l border-app-line">
					<BlockedBanner
						task={activeTask as unknown as TaskItem}
						onUnblock={(t) => unblockMutation.mutate(t.task_number)}
						onRetry={(t) => retryMutation.mutate(t.task_number)}
						busy={unblockMutation.isPending || retryMutation.isPending}
					/>
					{/* No onStatusChange: TaskDetail would render a <select> of all
					    five statuses it knows and offer moves the store refuses.
					    StatusMoves below shows the legal ones and nothing else. */}
					<TaskDetail
						task={
							toDesignSystemTask(
								activeTask as unknown as TaskItem,
							) as unknown as Task
						}
						resolveAgentName={resolveAgentName}
						onSubtaskToggle={handleSubtaskToggle}
						onDelete={handleDelete}
						onClose={() => setActiveTaskId(null)}
					/>
					<StatusMoves
						task={activeTask as unknown as TaskItem}
						table={transitions}
						onMove={handleMove}
						busy={
							updateMutation.isPending ||
							approveMutation.isPending ||
							executeMutation.isPending
						}
					/>
					<GithubSection
						metadata={(activeTask as unknown as TaskItem).metadata}
					/>
					<DependencySection
						taskNumber={(activeTask as unknown as TaskItem).task_number}
						onSelectTask={(number) => {
							const target = rawTasks.find((t) => t.task_number === number);
							if (target) setActiveTaskId(target.id);
						}}
					/>
					<ContractSection
						taskNumber={(activeTask as unknown as TaskItem).task_number}
						onSelectTask={(number) => {
							const target = rawTasks.find((t) => t.task_number === number);
							if (target) setActiveTaskId(target.id);
						}}
					/>
					<ProvenanceSection
						taskNumber={(activeTask as unknown as TaskItem).task_number}
						onSelectTask={(number) => {
							const target = rawTasks.find((t) => t.task_number === number);
							if (target) setActiveTaskId(target.id);
						}}
					/>
					<BindingSection
						task={activeTask as unknown as TaskItem}
						names={bindingNames}
					/>
					<div className="border-t border-app-line/40 px-4 py-3">
						<h3 className="mb-2 text-xs font-medium uppercase tracking-wide text-ink-dull">
							Attempts
						</h3>
						<TaskRunHistory
							taskNumber={(activeTask as unknown as TaskItem).task_number}
						/>
					</div>
				</div>
			)}
		</div>
	);
}

/** Which codebase the selected task acts on. Hidden for unbound tasks. */
function BindingSection({
	task,
	names,
}: {
	task: TaskItem;
	names: ReturnType<typeof useBindingNames>["names"];
}) {
	if (!task.project_id && !task.repo_id && !task.worktree_id) return null;

	const project = task.project_id
		? (names.projects.get(task.project_id) ?? task.project_id)
		: null;

	return (
		<div className="border-t border-app-line/40 px-4 py-3">
			<h3 className="mb-2 text-xs font-medium uppercase tracking-wide text-ink-dull">
				Codebase
			</h3>
			<div className="flex flex-wrap items-center gap-1.5">
				<RepoChip task={task} names={names} />
				{/* The chip shows the most specific binding; name the project too
				    when it isn't already what's displayed. */}
				{project && (task.repo_id || task.worktree_id) && (
					<span className="text-[11px] text-ink-faint">in {project}</span>
				)}
			</div>
		</div>
	);
}

function GithubSection({metadata}: {metadata: unknown}) {
	const refs = getGithubReferences(metadata);
	if (refs.length === 0) return null;

	return (
		<div className="border-t border-app-line/40 px-4 py-3">
			<h3 className="mb-2 text-xs font-medium uppercase tracking-wide text-ink-dull">
				GitHub Links
			</h3>
			<GithubMetadataBadges references={refs} />
		</div>
	);
}
