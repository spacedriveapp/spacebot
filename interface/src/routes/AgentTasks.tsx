import {useCallback, useEffect, useMemo, useRef, useState} from "react";
import {useMutation, useQuery, useQueryClient} from "@tanstack/react-query";
import {
	api,
	type CreateTaskRequest,
	type TaskItem,
	type TaskStatus,
} from "@/api/client";
import {useLiveContext} from "@/hooks/useLiveContext";
import {Button} from "@spacedrive/primitives";
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

const TASK_LIMIT = 200;

export function AgentTasks({agentId}: {agentId: string}) {
	const queryClient = useQueryClient();
	const {taskEventVersion} = useLiveContext();

	const queryKey = ["tasks", agentId];

	// SSE-driven cache invalidation
	const prevVersion = useRef(taskEventVersion);
	useEffect(() => {
		if (taskEventVersion !== prevVersion.current) {
			prevVersion.current = taskEventVersion;
			queryClient.invalidateQueries({queryKey});
		}
	}, [taskEventVersion, queryKey, queryClient]);

	const {data, isLoading, error} = useQuery({
		queryKey,
		queryFn: () => api.listTasks({agent_id: agentId, limit: TASK_LIMIT}),
		refetchInterval: 15_000,
	});

	const tasks = (data?.tasks ?? []) as unknown as Task[];
	const rawTasks = (data?.tasks ?? []) as TaskItem[];

	// `blocked` is not in @spacedrive/ai's TaskStatus union, so those tasks are
	// split out and rendered by BlockedTasksSection instead.
	// Edge counts arrive with the list, so badges cost no extra requests.
	const edgesByTask = useMemo(() => indexEdges(data?.edges), [data?.edges]);

	const blockedTasks = useMemo(
		() => (data?.tasks ?? []).filter((t) => t.status === "blocked"),
		[data],
	);
	const boardTasks = useMemo(
		() => tasks.filter((t) => (t as unknown as TaskItem).status !== "blocked"),
		[tasks],
	);

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

	const handleMove = useCallback(
		(task: TaskItem, status: TaskStatus) => {
			const move = planStatusChange(task, status, transitions);
			if (move.action === "refuse") {
				setMoveError(move.reason);
				return;
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
			updateMutation,
			approveMutation,
			executeMutation,
			unblockMutation,
		],
	);

	const handleStatusChange = useCallback(
		(task: Task, status: UiTaskStatus) => {
			// Resolve the real task by id: a blocked card reaches these components
			// with an adapted status, and branching on that would approve a task
			// nobody was asked to approve.
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
			createMutation.mutate({
				owner_agent_id: agentId,
				title: formData.title,
				description: formData.description || undefined,
				priority: formData.priority,
				status: "backlog",
			});
		},
		[createMutation, agentId],
	);

	return (
		<div className="flex h-full w-full">
			{/* List panel */}
			<div className="flex min-w-0 flex-1 flex-col">
				{/* Toolbar */}
				<div className="flex items-center justify-between border-b border-app-line px-4 py-2">
					<span className="text-sm text-ink-dull">
						{tasks.length} task{tasks.length !== 1 ? "s" : ""}
					</span>
					<Button size="md" onClick={() => setCreateOpen(!createOpen)}>
						{createOpen ? "Cancel" : "Create Task"}
					</Button>
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
				) : (
					<div className="flex-1 overflow-y-auto">
						{/* TaskList drops any status outside TASK_STATUS_ORDER,
						    so blocked tasks are rendered separately. */}
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
							edges={edgesByTask}
							onUnblock={(task) => unblockMutation.mutate(task.task_number)}
						/>
						<TaskList
							tasks={boardTasks}
							activeTaskId={activeTaskId ?? undefined}
							collapsedGroups={collapsedGroups}
							onToggleGroup={handleToggleGroup}
							onTaskClick={(task) => setActiveTaskId(task.id)}
							onStatusChange={handleStatusChange}
							onDelete={handleDelete}
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
					{/* GitHub metadata (not part of the shared TaskDetail) */}
					<DependencySection
						taskNumber={(activeTask as unknown as TaskItem).task_number}
					/>
					<ContractSection
						taskNumber={(activeTask as unknown as TaskItem).task_number}
					/>
					<ProvenanceSection
						taskNumber={(activeTask as unknown as TaskItem).task_number}
					/>
					<GithubSection
						metadata={(activeTask as unknown as TaskItem).metadata}
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
