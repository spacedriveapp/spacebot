import { useCallback, useEffect, useRef, useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import {
  api,
  type TaskItem,
  type TaskStatus,
  type TaskPriority,
  type CreateTaskRequest,
} from "@/api/client";
import { useLiveContext } from "@/hooks/useLiveContext";
import { Badge } from "@/ui/Badge";
import { Button } from "@/ui/Button";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from "@/ui/Dialog";
import { Markdown } from "@/components/Markdown";
import { formatTimeAgo } from "@/lib/format";
import { AnimatePresence, motion } from "framer-motion";

const COLUMNS: { status: TaskStatus; label: string }[] = [
  { status: "pending_approval", label: "Pending Approval" },
  { status: "backlog", label: "Backlog" },
  { status: "ready", label: "Ready" },
  { status: "in_progress", label: "In Progress" },
  { status: "done", label: "Done" },
];

const STATUS_COLORS: Record<
  TaskStatus,
  "default" | "amber" | "accent" | "violet" | "green"
> = {
  pending_approval: "amber",
  backlog: "default",
  ready: "accent",
  in_progress: "violet",
  done: "green",
};

const PRIORITY_LABELS: Record<TaskPriority, string> = {
  critical: "Critical",
  high: "High",
  medium: "Medium",
  low: "Low",
};

const PRIORITY_COLORS: Record<
  TaskPriority,
  "red" | "amber" | "default" | "outline"
> = {
  critical: "red",
  high: "amber",
  medium: "default",
  low: "outline",
};

export function AgentTasks({ agentId }: { agentId: string }) {
  const queryClient = useQueryClient();
  const { taskEventVersion } = useLiveContext();

  // Invalidate on SSE task events
  const prevVersion = useRef(taskEventVersion);
  useEffect(() => {
    if (taskEventVersion !== prevVersion.current) {
      prevVersion.current = taskEventVersion;
      queryClient.invalidateQueries({ queryKey: ["tasks", agentId] });
    }
  }, [taskEventVersion, agentId, queryClient]);

  const { data, isLoading } = useQuery({
    queryKey: ["tasks", agentId],
    queryFn: () => api.listTasks(agentId, { limit: 200 }),
    refetchInterval: 15_000,
  });

  const tasks = data?.tasks ?? [];

  // Group tasks by status
  const tasksByStatus: Record<TaskStatus, TaskItem[]> = {
    pending_approval: [],
    backlog: [],
    ready: [],
    in_progress: [],
    done: [],
  };
  for (const task of tasks) {
    tasksByStatus[task.status]?.push(task);
  }

  // Create task dialog
  const [createOpen, setCreateOpen] = useState(false);
  // Detail dialog — store task number and derive from live list to stay current.
  const [selectedTaskNumber, setSelectedTaskNumber] = useState<number | null>(
    null,
  );
  const selectedTask =
    selectedTaskNumber !== null
      ? (tasks.find((t) => t.task_number === selectedTaskNumber) ?? null)
      : null;

  const createMutation = useMutation({
    mutationFn: (request: CreateTaskRequest) =>
      api.createTask(agentId, request),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["tasks", agentId] });
      setCreateOpen(false);
    },
  });

  const updateMutation = useMutation({
    mutationFn: ({
      taskNumber,
      ...request
    }: {
      taskNumber: number;
      status?: TaskStatus;
      priority?: TaskPriority;
    }) => api.updateTask(agentId, taskNumber, request),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["tasks", agentId] });
    },
  });

  const approveMutation = useMutation({
    mutationFn: (taskNumber: number) =>
      api.approveTask(agentId, taskNumber, "human"),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["tasks", agentId] });
    },
  });

  const executeMutation = useMutation({
    mutationFn: (taskNumber: number) => api.executeTask(agentId, taskNumber),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["tasks", agentId] });
    },
  });

  const deleteMutation = useMutation({
    mutationFn: (taskNumber: number) => api.deleteTask(agentId, taskNumber),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["tasks", agentId] });
      setSelectedTaskNumber(null);
    },
  });

  if (isLoading) {
    return (
      <div className="flex h-full items-center justify-center text-ink-faint">
        Loading tasks...
      </div>
    );
  }

  if (tasks.length === 0 && !createOpen) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-4 text-ink-faint">
        <p className="text-sm">No tasks yet</p>
        <Button size="sm" onClick={() => setCreateOpen(true)}>
          Create Task
        </Button>
        <CreateTaskDialog
          open={createOpen}
          onClose={() => setCreateOpen(false)}
          onCreate={(request) => createMutation.mutate(request)}
          isPending={createMutation.isPending}
        />
      </div>
    );
  }

  return (
    <div className="flex h-full flex-col">
      {/* Toolbar */}
      <div className="flex items-center justify-between border-b border-app-line px-4 py-2">
        <div className="flex items-center gap-3">
          <span className="text-sm text-ink-dull">
            {tasks.length} task{tasks.length !== 1 ? "s" : ""}
          </span>
          {tasksByStatus.pending_approval.length > 0 && (
            <Badge variant="amber" size="sm">
              {tasksByStatus.pending_approval.length} pending approval
            </Badge>
          )}
          {tasksByStatus.in_progress.length > 0 && (
            <Badge variant="violet" size="sm">
              {tasksByStatus.in_progress.length} in progress
            </Badge>
          )}
        </div>
        <Button size="sm" onClick={() => setCreateOpen(true)}>
          Create Task
        </Button>
      </div>

      {/* Kanban Board */}
      <div className="flex flex-1 flex-wrap content-start gap-3 overflow-y-auto p-4">
        {COLUMNS.map(({ status, label }) => (
          <KanbanColumn
            key={status}
            status={status}
            label={label}
            tasks={tasksByStatus[status]}
            onSelect={(task) => setSelectedTaskNumber(task.task_number)}
            onApprove={(task) => approveMutation.mutate(task.task_number)}
            onExecute={(task) => executeMutation.mutate(task.task_number)}
            onStatusChange={(task, newStatus) =>
              updateMutation.mutate({
                taskNumber: task.task_number,
                status: newStatus,
              })
            }
          />
        ))}
      </div>

      {/* Create Dialog */}
      <CreateTaskDialog
        open={createOpen}
        onClose={() => setCreateOpen(false)}
        onCreate={(request) => createMutation.mutate(request)}
        isPending={createMutation.isPending}
      />

      {/* Detail Dialog */}
      {selectedTask && (
        <TaskDetailDialog
          task={selectedTask}
          onClose={() => setSelectedTaskNumber(null)}
          onApprove={() => approveMutation.mutate(selectedTask.task_number)}
          onExecute={() => executeMutation.mutate(selectedTask.task_number)}
          onDelete={() => deleteMutation.mutate(selectedTask.task_number)}
          onStatusChange={(status) =>
            updateMutation.mutate({
              taskNumber: selectedTask.task_number,
              status,
            })
          }
        />
      )}
    </div>
  );
}

// -- Kanban Column --

function KanbanColumn({
  status,
  label,
  tasks,
  onSelect,
  onApprove,
  onExecute,
  onStatusChange,
}: {
  status: TaskStatus;
  label: string;
  tasks: TaskItem[];
  onSelect: (task: TaskItem) => void;
  onApprove: (task: TaskItem) => void;
  onExecute: (task: TaskItem) => void;
  onStatusChange: (task: TaskItem, status: TaskStatus) => void;
}) {
  return (
    <div className="flex min-h-0 min-w-[14rem] flex-1 basis-[14rem] flex-col rounded-lg border border-app-line/50 bg-app-darkBox/20">
      {/* Column Header */}
      <div className="flex items-center gap-2 border-b border-app-line/30 px-3 py-2">
        <Badge variant={STATUS_COLORS[status]} size="sm">
          {label}
        </Badge>
        <span className="text-tiny text-ink-faint">{tasks.length}</span>
      </div>

      {/* Cards */}
      <div className="flex-1 space-y-2 overflow-y-auto p-2">
        <AnimatePresence mode="popLayout">
          {tasks.map((task) => (
            <TaskCard
              key={task.id}
              task={task}
              onSelect={() => onSelect(task)}
              onApprove={() => onApprove(task)}
              onExecute={() => onExecute(task)}
              onStatusChange={(newStatus) => onStatusChange(task, newStatus)}
            />
          ))}
        </AnimatePresence>
        {tasks.length === 0 && (
          <div className="py-4 text-center text-tiny text-ink-faint">
            No tasks
          </div>
        )}
      </div>
    </div>
  );
}

// -- Task Card --

function TaskCard({
  task,
  onSelect,
  onApprove,
  onExecute,
  onStatusChange,
}: {
  task: TaskItem;
  onSelect: () => void;
  onApprove: () => void;
  onExecute: () => void;
  onStatusChange: (status: TaskStatus) => void;
}) {
  const subtasksDone = task.subtasks.filter((s) => s.completed).length;
  const subtasksTotal = task.subtasks.length;

  return (
    <motion.div
      layout
      initial={{ opacity: 0, scale: 0.95 }}
      animate={{ opacity: 1, scale: 1 }}
      exit={{ opacity: 0, scale: 0.95 }}
      transition={{ duration: 0.15 }}
      className="cursor-pointer rounded-md border border-app-line/30 bg-app p-3 transition-colors hover:border-app-line"
      onClick={onSelect}
    >
      {/* Title row */}
      <div className="flex items-start justify-between gap-2">
        <span className="text-sm font-medium text-ink leading-tight">
          #{task.task_number} {task.title}
        </span>
      </div>

      {/* Meta row */}
      <div className="mt-2 flex flex-wrap items-center gap-1.5">
        <Badge variant={PRIORITY_COLORS[task.priority]} size="sm">
          {PRIORITY_LABELS[task.priority]}
        </Badge>
        {subtasksTotal > 0 && (
          <span className="text-tiny text-ink-faint">
            {subtasksDone}/{subtasksTotal}
          </span>
        )}
        {task.worker_id && (
          <Badge variant="violet" size="sm">
            Worker
          </Badge>
        )}
      </div>

      {/* Subtask progress bar */}
      {subtasksTotal > 0 && (
        <div className="mt-2 h-1 overflow-hidden rounded-full bg-app-line/30">
          <div
            className="h-full rounded-full bg-accent transition-all"
            style={{ width: `${(subtasksDone / subtasksTotal) * 100}%` }}
          />
        </div>
      )}

      {/* Quick actions */}
      <div className="mt-2 flex gap-1" onClick={(e) => e.stopPropagation()}>
        {task.status === "pending_approval" && (
          <button
            className="rounded px-1.5 py-0.5 text-tiny text-accent hover:bg-accent/10"
            onClick={onApprove}
          >
            Approve
          </button>
        )}
        {(task.status === "backlog" || task.status === "pending_approval") && (
          <button
            className="rounded px-1.5 py-0.5 text-tiny text-violet-400 hover:bg-violet-400/10"
            onClick={onExecute}
          >
            Execute
          </button>
        )}
        {task.status === "in_progress" && (
          <button
            className="rounded px-1.5 py-0.5 text-tiny text-emerald-400 hover:bg-emerald-400/10"
            onClick={() => onStatusChange("done")}
          >
            Mark Done
          </button>
        )}
      </div>

      {/* Footer */}
      <div className="mt-1.5 text-tiny text-ink-faint">
        {formatTimeAgo(task.created_at)} by {task.created_by}
      </div>
    </motion.div>
  );
}

// -- Create Task Dialog --

function CreateTaskDialog({
  open,
  onClose,
  onCreate,
  isPending,
}: {
  open: boolean;
  onClose: () => void;
  onCreate: (request: CreateTaskRequest) => void;
  isPending: boolean;
}) {
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [priority, setPriority] = useState<TaskPriority>("medium");
  const [status, setStatus] = useState<TaskStatus>("backlog");

  const handleSubmit = useCallback(() => {
    if (!title.trim()) return;
    onCreate({
      title: title.trim(),
      description: description.trim() || undefined,
      priority,
      status,
    });
    setTitle("");
    setDescription("");
    setPriority("medium");
    setStatus("backlog");
  }, [title, description, priority, status, onCreate]);

  return (
    <Dialog open={open} onOpenChange={(v) => !v && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Create Task</DialogTitle>
        </DialogHeader>
        <div className="flex flex-col gap-3 py-2">
          <div>
            <label className="mb-1 block text-xs text-ink-dull">Title</label>
            <input
              className="w-full rounded-md border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
              placeholder="Task title..."
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && handleSubmit()}
              autoFocus
            />
          </div>
          <div>
            <label className="mb-1 block text-xs text-ink-dull">
              Description
            </label>
            <textarea
              className="w-full rounded-md border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
              placeholder="Optional description..."
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              rows={3}
            />
          </div>
          <div className="flex gap-4">
            <div className="flex-1">
              <label className="mb-1 block text-xs text-ink-dull">
                Priority
              </label>
              <select
                className="w-full rounded-md border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink focus:border-accent focus:outline-none"
                value={priority}
                onChange={(e) => setPriority(e.target.value as TaskPriority)}
              >
                <option value="critical">Critical</option>
                <option value="high">High</option>
                <option value="medium">Medium</option>
                <option value="low">Low</option>
              </select>
            </div>
            <div className="flex-1">
              <label className="mb-1 block text-xs text-ink-dull">
                Initial Status
              </label>
              <select
                className="w-full rounded-md border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink focus:border-accent focus:outline-none"
                value={status}
                onChange={(e) => setStatus(e.target.value as TaskStatus)}
              >
                <option value="pending_approval">Pending Approval</option>
                <option value="backlog">Backlog</option>
                <option value="ready">Ready</option>
              </select>
            </div>
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" size="sm" onClick={onClose}>
            Cancel
          </Button>
          <Button
            size="sm"
            onClick={handleSubmit}
            disabled={!title.trim() || isPending}
          >
            {isPending ? "Creating..." : "Create"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

// -- Task Detail Dialog --

function TaskDetailDialog({
  task,
  onClose,
  onApprove,
  onExecute,
  onDelete,
  onStatusChange,
}: {
  task: TaskItem;
  onClose: () => void;
  onApprove: () => void;
  onExecute: () => void;
  onDelete: () => void;
  onStatusChange: (status: TaskStatus) => void;
}) {
  return (
    <Dialog open={true} onOpenChange={(v) => !v && onClose()}>
      <DialogContent className="!flex max-h-[85vh] max-w-lg !flex-col overflow-hidden">
        <DialogHeader className="shrink-0">
          <DialogTitle>
            #{task.task_number} {task.title}
          </DialogTitle>
        </DialogHeader>
        <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto py-2 pr-1">
          {/* Status + Priority */}
          <div className="flex items-center gap-2">
            <Badge variant={STATUS_COLORS[task.status]} size="md">
              {task.status.replace("_", " ")}
            </Badge>
            <Badge variant={PRIORITY_COLORS[task.priority]} size="md">
              {PRIORITY_LABELS[task.priority]}
            </Badge>
            {task.worker_id && (
              <Badge variant="violet" size="md">
                Worker: {task.worker_id.slice(0, 8)}
              </Badge>
            )}
          </div>

          {/* Description */}
          {task.description && (
            <div>
              <label className="mb-1 block text-xs text-ink-dull">
                Description
              </label>
              <Markdown className="break-words text-sm text-ink">
                {task.description}
              </Markdown>
            </div>
          )}

          {/* Subtasks */}
          {task.subtasks.length > 0 && (
            <div>
              <label className="mb-1 block text-xs text-ink-dull">
                Subtasks ({task.subtasks.filter((s) => s.completed).length}/
                {task.subtasks.length})
              </label>
              <ul className="space-y-1">
                {task.subtasks.map((subtask, index) => (
                  <li key={index} className="flex items-center gap-2 text-sm">
                    <span
                      className={
                        subtask.completed
                          ? "text-emerald-400"
                          : "text-ink-faint"
                      }
                    >
                      {subtask.completed ? "[x]" : "[ ]"}
                    </span>
                    <span
                      className={
                        subtask.completed
                          ? "text-ink-dull line-through"
                          : "text-ink"
                      }
                    >
                      {subtask.title}
                    </span>
                  </li>
                ))}
              </ul>
            </div>
          )}

          {/* Metadata */}
          <div className="grid grid-cols-1 gap-2 text-xs text-ink-dull sm:grid-cols-2">
            <div>Created: {formatTimeAgo(task.created_at)}</div>
            <div>By: {task.created_by}</div>
            {task.approved_at && (
              <div>Approved: {formatTimeAgo(task.approved_at)}</div>
            )}
            {task.approved_by && <div>By: {task.approved_by}</div>}
            {task.completed_at && (
              <div>Completed: {formatTimeAgo(task.completed_at)}</div>
            )}
            <div>Updated: {formatTimeAgo(task.updated_at)}</div>
          </div>
        </div>

        <DialogFooter className="shrink-0">
          <div className="flex w-full flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
            <button
              className="text-xs text-red-400 hover:text-red-300"
              onClick={onDelete}
            >
              Delete
            </button>
            <div className="flex flex-wrap gap-2 sm:justify-end">
              {task.status === "pending_approval" && (
                <Button size="sm" onClick={onApprove}>
                  Approve
                </Button>
              )}
              {(task.status === "backlog" ||
                task.status === "pending_approval") && (
                <Button size="sm" onClick={onExecute}>
                  Execute
                </Button>
              )}
              {task.status === "ready" && (
                <Badge variant="accent" size="md">
                  Waiting for cortex pickup
                </Badge>
              )}
              {task.status === "in_progress" && (
                <Button size="sm" onClick={() => onStatusChange("done")}>
                  Mark Done
                </Button>
              )}
              {task.status === "done" && (
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => onStatusChange("backlog")}
                >
                  Reopen
                </Button>
              )}
              <Button size="sm" variant="outline" onClick={onClose}>
                Close
              </Button>
            </div>
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
