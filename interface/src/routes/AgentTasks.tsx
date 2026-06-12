import {useCallback, useEffect, useRef, useState} from "react";
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

	const [activeTaskId, setActiveTaskId] = useState<string | null>(null);
	const [collapsedGroups, setCollapsedGroups] = useState<Set<UiTaskStatus>>(
		() => new Set(),
	);
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

	const handleStatusChange = useCallback(
		(task: Task, status: UiTaskStatus) => {
			const t = task as unknown as TaskItem;
			// Route approve/execute through their dedicated endpoints
			if (t.status === "pending_approval" && status === "ready") {
				approveMutation.mutate(t.task_number);
			} else if (t.status === "backlog" && status === "in_progress") {
				executeMutation.mutate(t.task_number);
			} else {
				updateMutation.mutate({taskNumber: t.task_number, status});
			}
		},
		[updateMutation, approveMutation, executeMutation],
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
						<TaskList
							tasks={tasks}
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
					<TaskDetail
						task={activeTask}
						onStatusChange={handleStatusChange}
						onSubtaskToggle={handleSubtaskToggle}
						onDelete={handleDelete}
						onClose={() => setActiveTaskId(null)}
					/>
					{/* GitHub metadata (not part of the shared TaskDetail) */}
					<GithubSection
						metadata={(activeTask as unknown as TaskItem).metadata}
					/>
				</div>
			)}
		</div>
	);
}

function GithubSection({metadata}: {metadata: Record<string, unknown>}) {
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
