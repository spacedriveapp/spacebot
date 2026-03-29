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
import { TaskDependencyGraph } from "@/components/TaskDependencyGraph";
import { DiffViewer } from "@/components/DiffViewer";
import { useSpacebotConfig } from "@/hooks/useSpacebotConfig";
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

/** Extract dependency task numbers from metadata */
function getDependencies(task: TaskItem): number[] {
  const deps = task.metadata?.depends_on;
  if (Array.isArray(deps)) return deps.filter((n): n is number => typeof n === "number");
  return [];
}

/** Check if a task is blocked (has incomplete dependencies) */
function isBlocked(task: TaskItem, allTasks: TaskItem[]): boolean {
  const deps = getDependencies(task);
  if (deps.length === 0) return false;
  return deps.some((depNum) => {
    const depTask = allTasks.find((t) => t.task_number === depNum);
    return depTask && depTask.status !== "done";
  });
}

// -- GitHub Issue type (from registry API) --
interface GitHubIssue {
  number: number;
  title: string;
  state: string;
  url: string;
  repository: string;
  labels: string[];
  assignees: string[];
  created_at: string;
  updated_at: string;
}

interface IssuesResponse {
  issues: GitHubIssue[];
  repos: string[];
}

export function AgentTasks({ agentId }: { agentId: string }) {
  const queryClient = useQueryClient();
  const { taskEventVersion } = useLiveContext();
  const config = useSpacebotConfig(agentId);

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

  // Fetch GitHub issues from registry
  const { data: issuesData } = useQuery({
    queryKey: ["registry-issues", agentId],
    queryFn: async () => {
      const resp = await fetch(
        `/api/registry/issues?agent_id=${agentId}&limit=100`
      );
      if (!resp.ok) return { issues: [], repos: [] } as IssuesResponse;
      return resp.json() as Promise<IssuesResponse>;
    },
    refetchInterval: 60_000,
    staleTime: 30_000,
  });

  const githubIssues = issuesData?.issues ?? [];
  const availableRepos = issuesData?.repos ?? [];

  const tasks = data?.tasks ?? [];

  // Project filter
  const [projectFilter, setProjectFilter] = useState<string>("all");
  // Show/hide GitHub issues
  const [showIssues, setShowIssues] = useState(true);

  // Filter GitHub issues by project
  const filteredIssues = projectFilter === "all"
    ? githubIssues
    : githubIssues.filter((i) => i.repository === projectFilter);

  // Filter tasks by project (if a project filter is active)
  const filteredTasks = projectFilter === "all"
    ? tasks
    : tasks.filter((t) => {
        const repo = t.metadata?.github_repository as string | undefined;
        return repo === projectFilter;
      });

  // Group filtered tasks by status
  const tasksByStatus: Record<TaskStatus, TaskItem[]> = {
    pending_approval: [],
    backlog: [],
    ready: [],
    in_progress: [],
    done: [],
  };
  for (const task of filteredTasks) {
    tasksByStatus[task.status]?.push(task);
  }

  // View mode: "board" (kanban) or "graph" (dependency graph)
  const [viewMode, setViewMode] = useState<"board" | "graph">("board");
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

  const importIssueMutation = useMutation({
    mutationFn: (issue: GitHubIssue) =>
      api.createTask(agentId, {
        title: `[${issue.repository.split("/").pop()}#${issue.number}] ${issue.title}`,
        description: `GitHub: ${issue.url}`,
        status: "backlog",
        priority: "medium",
        metadata: {
          github_issue_url: issue.url,
          github_issue_number: issue.number,
          github_repository: issue.repository,
        },
      }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["tasks", agentId] });
    },
  });

  // Track which issues are already imported as tasks
  const importedIssueUrls = new Set(
    tasks
      .map((t) => t.metadata?.github_issue_url as string | undefined)
      .filter(Boolean)
  );

  if (isLoading) {
    return (
      <div className="flex h-full items-center justify-center text-ink-faint">
        Loading tasks...
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
              {tasksByStatus.pending_approval.length} pending
            </Badge>
          )}
          {tasksByStatus.in_progress.length > 0 && (
            <Badge variant="violet" size="sm">
              {tasksByStatus.in_progress.length} active
            </Badge>
          )}
          {(() => {
            const blockedCount = tasks.filter((t) => isBlocked(t, tasks)).length;
            return blockedCount > 0 ? (
              <Badge variant="red" size="sm">
                {blockedCount} blocked
              </Badge>
            ) : null;
          })()}
          {tasksByStatus.done.length > 0 && (
            <Badge variant="green" size="sm">
              {tasksByStatus.done.length} done
            </Badge>
          )}
        </div>
        <div className="flex items-center gap-2">
          {/* View Toggle */}
          <div className="flex rounded-md border border-app-line">
            <button
              className={`px-2.5 py-1 text-xs font-medium transition-colors ${
                viewMode === "board"
                  ? "bg-app-selected text-ink"
                  : "text-ink-faint hover:text-ink"
              }`}
              onClick={() => setViewMode("board")}
            >
              Board
            </button>
            <button
              className={`px-2.5 py-1 text-xs font-medium transition-colors ${
                viewMode === "graph"
                  ? "bg-app-selected text-ink"
                  : "text-ink-faint hover:text-ink"
              }`}
              onClick={() => setViewMode("graph")}
            >
              Graph
            </button>
          </div>
          {/* Project Filter */}
          {availableRepos.length > 0 && (
            <select
              className="rounded-md border border-app-line bg-app-darkBox px-2 py-1 text-xs text-ink focus:border-accent focus:outline-none"
              value={projectFilter}
              onChange={(e) => setProjectFilter(e.target.value)}
            >
              <option value="all">All Projects</option>
              {availableRepos.map((repo) => (
                <option key={repo} value={repo}>
                  {repo.split("/").pop()}
                </option>
              ))}
            </select>
          )}
          {/* Issues Toggle */}
          <button
            className={`rounded-md border px-2 py-1 text-xs font-medium transition-colors ${
              showIssues
                ? "border-accent/50 bg-accent/10 text-accent"
                : "border-app-line text-ink-faint hover:text-ink"
            }`}
            onClick={() => setShowIssues(!showIssues)}
            title="Show/hide GitHub issues"
          >
            Issues {showIssues && filteredIssues.length > 0 ? `(${filteredIssues.length})` : ""}
          </button>
          <Button size="sm" onClick={() => setCreateOpen(true)}>
            Create Task
          </Button>
        </div>
      </div>

      {/* Config Status Bar */}
      {config.isReady && (
        <div className="flex items-center gap-2 border-b border-app-line/50 bg-app-darkBox/20 px-4 py-1.5">
          {/* Worker Model */}
          <span className="rounded bg-app-line/30 px-1.5 py-0.5 text-tiny text-ink-faint" title="Worker model">
            {config.workerModel.split("/").pop() ?? "no model"}
          </span>
          {/* Concurrency */}
          <span
            className={`rounded px-1.5 py-0.5 text-tiny ${
              tasksByStatus.in_progress.length >= config.maxConcurrentWorkers
                ? "bg-amber-500/10 text-amber-400"
                : "bg-app-line/30 text-ink-faint"
            }`}
            title="Active / max concurrent workers"
          >
            {tasksByStatus.in_progress.length}/{config.maxConcurrentWorkers} workers
          </span>
          {/* Worktrees */}
          <span
            className={`rounded px-1.5 py-0.5 text-tiny ${
              config.useWorktrees
                ? "bg-emerald-500/10 text-emerald-400"
                : "bg-app-line/30 text-ink-faint"
            }`}
          >
            Worktrees {config.useWorktrees ? "ON" : "OFF"}
          </span>
          {/* Platforms */}
          {config.enabledPlatforms.map((p) => (
            <span key={p} className="rounded bg-app-line/30 px-1.5 py-0.5 text-tiny text-ink-faint capitalize">
              {p}
            </span>
          ))}
          {/* Auth */}
          {config.isAnthropic && (
            <span className="rounded bg-accent/10 px-1.5 py-0.5 text-tiny text-accent">
              Claude Max
            </span>
          )}
        </div>
      )}

      {/* Dependency Graph View */}
      {viewMode === "graph" && (
        <div className="flex-1 overflow-hidden">
          <TaskDependencyGraph
            tasks={tasks}
            onSelectTask={(num) => setSelectedTaskNumber(num)}
          />
        </div>
      )}

      {/* Kanban Board */}
      <div className={`flex flex-1 flex-wrap content-start gap-3 overflow-y-auto p-4 ${viewMode === "graph" ? "hidden" : ""}`}>
        {COLUMNS.map(({ status, label }) => (
          <KanbanColumn
            key={status}
            status={status}
            label={label}
            tasks={tasksByStatus[status]}
            allTasks={tasks}
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

        {/* GitHub Issues Column */}
        {showIssues && filteredIssues.length > 0 && (
          <div className="flex min-h-0 min-w-[14rem] flex-1 basis-[14rem] flex-col rounded-lg border border-accent/20 bg-accent/5">
            <div className="flex items-center gap-2 border-b border-accent/10 px-3 py-2">
              <Badge variant="accent" size="sm">
                GitHub Issues
              </Badge>
              <span className="text-tiny text-ink-faint">{filteredIssues.length}</span>
            </div>
            <div className="flex-1 space-y-2 overflow-y-auto p-2">
              {filteredIssues.map((issue) => {
                const isImported = importedIssueUrls.has(issue.url);
                return (
                  <div
                    key={`${issue.repository}-${issue.number}`}
                    className="rounded-md border border-app-line/30 bg-app p-3 transition-colors hover:border-accent/50"
                  >
                    <a
                      href={issue.url}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="block"
                    >
                      <div className="flex items-start justify-between gap-2">
                        <span className="text-sm font-medium text-ink leading-tight">
                          #{issue.number} {issue.title}
                        </span>
                      </div>
                      <div className="mt-1.5 flex flex-wrap items-center gap-1">
                        <span className="rounded bg-accent/10 px-1 py-0.5 text-tiny text-accent">
                          {issue.repository.split("/").pop()}
                        </span>
                        {issue.labels.slice(0, 3).map((label) => (
                          <span
                            key={label}
                            className="rounded bg-app-line/50 px-1 py-0.5 text-tiny text-ink-faint"
                          >
                            {label}
                          </span>
                        ))}
                      </div>
                      {issue.assignees.length > 0 && (
                        <div className="mt-1 text-tiny text-ink-faint">
                          {issue.assignees.join(", ")}
                        </div>
                      )}
                    </a>
                    <div className="mt-1.5 flex items-center justify-between">
                      <span className="text-tiny text-ink-faint">
                        {formatTimeAgo(issue.updated_at)}
                      </span>
                      {isImported ? (
                        <span className="rounded bg-emerald-500/10 px-1.5 py-0.5 text-tiny text-emerald-400">
                          Imported
                        </span>
                      ) : (
                        <button
                          className="rounded bg-accent/10 px-1.5 py-0.5 text-tiny text-accent hover:bg-accent/20"
                          onClick={() => importIssueMutation.mutate(issue)}
                        >
                          Import
                        </button>
                      )}
                    </div>
                  </div>
                );
              })}
            </div>
          </div>
        )}
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
          allTasks={tasks}
          agentId={agentId}
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
  allTasks,
  onSelect,
  onApprove,
  onExecute,
  onStatusChange,
}: {
  status: TaskStatus;
  label: string;
  tasks: TaskItem[];
  allTasks: TaskItem[];
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
              allTasks={allTasks}
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
  allTasks,
  onSelect,
  onApprove,
  onExecute,
  onStatusChange,
}: {
  task: TaskItem;
  allTasks: TaskItem[];
  onSelect: () => void;
  onApprove: () => void;
  onExecute: () => void;
  onStatusChange: (status: TaskStatus) => void;
}) {
  const subtasksDone = task.subtasks.filter((s) => s.completed).length;
  const subtasksTotal = task.subtasks.length;
  const deps = getDependencies(task);
  const blocked = isBlocked(task, allTasks);

  return (
    <motion.div
      layout
      initial={{ opacity: 0, scale: 0.95 }}
      animate={{ opacity: 1, scale: 1 }}
      exit={{ opacity: 0, scale: 0.95 }}
      transition={{ duration: 0.15 }}
      className={`cursor-pointer rounded-md border p-3 transition-colors hover:border-app-line ${
        blocked
          ? "border-red-500/30 bg-red-950/10"
          : "border-app-line/30 bg-app"
      }`}
      onClick={onSelect}
    >
      {/* Title row */}
      <div className="flex items-start justify-between gap-2">
        <span className="text-sm font-medium text-ink leading-tight">
          #{task.task_number} {task.title}
        </span>
        {blocked && (
          <span className="shrink-0 text-tiny text-red-400" title="Blocked by dependencies">
            Blocked
          </span>
        )}
      </div>

      {/* Dependencies */}
      {deps.length > 0 && (
        <div className="mt-1.5 flex flex-wrap gap-1">
          {deps.map((depNum) => {
            const depTask = allTasks.find((t) => t.task_number === depNum);
            const depDone = depTask?.status === "done";
            return (
              <span
                key={depNum}
                className={`inline-flex items-center gap-0.5 rounded px-1 py-0.5 text-tiny ${
                  depDone
                    ? "bg-emerald-500/10 text-emerald-400"
                    : "bg-amber-500/10 text-amber-400"
                }`}
              >
                {depDone ? "\u2713" : "\u23F3"} #{depNum}
              </span>
            );
          })}
        </div>
      )}

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
          <Badge variant="violet" size="sm" title={task.worker_id}>
            {"\u2699"} {task.worker_id.slice(0, 8)}
          </Badge>
        )}
        {!!task.metadata?.worktree && (
          <Badge variant="outline" size="sm" title={String(task.metadata.worktree)}>
            {"\uD83C\uDF33"} worktree
          </Badge>
        )}
        {!!task.metadata?.assigned_agent && (
          <Badge variant="accent" size="sm">
            {String(task.metadata.assigned_agent)}
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
  const [dependsOn, setDependsOn] = useState("");
  const [assignedAgent, setAssignedAgent] = useState("");

  const handleSubmit = useCallback(() => {
    if (!title.trim()) return;
    const deps = dependsOn
      .split(",")
      .map((s) => parseInt(s.trim().replace("#", ""), 10))
      .filter((n) => !isNaN(n));
    const metadata: Record<string, unknown> = {};
    if (deps.length > 0) metadata.depends_on = deps;
    if (assignedAgent.trim()) metadata.assigned_agent = assignedAgent.trim();
    onCreate({
      title: title.trim(),
      description: description.trim() || undefined,
      priority,
      status,
      metadata: Object.keys(metadata).length > 0 ? metadata : undefined,
    });
    setTitle("");
    setDescription("");
    setPriority("medium");
    setStatus("backlog");
    setDependsOn("");
    setAssignedAgent("");
  }, [title, description, priority, status, dependsOn, assignedAgent, onCreate]);

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
                Depends On
              </label>
              <input
                className="w-full rounded-md border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
                placeholder="#1, #3"
                value={dependsOn}
                onChange={(e) => setDependsOn(e.target.value)}
              />
            </div>
            <div className="flex-1">
              <label className="mb-1 block text-xs text-ink-dull">
                Assign Agent
              </label>
              <input
                className="w-full rounded-md border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
                placeholder="e.g. claude-code, codex"
                value={assignedAgent}
                onChange={(e) => setAssignedAgent(e.target.value)}
              />
            </div>
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
  allTasks,
  agentId,
  onClose,
  onApprove,
  onExecute,
  onDelete,
  onStatusChange,
}: {
  task: TaskItem;
  allTasks: TaskItem[];
  agentId: string;
  onClose: () => void;
  onApprove: () => void;
  onExecute: () => void;
  onDelete: () => void;
  onStatusChange: (status: TaskStatus) => void;
}) {
  const deps = getDependencies(task);
  const blocked = isBlocked(task, allTasks);

  // Fetch diff if task has worktree or branch metadata
  const hasDiffSource = task.metadata?.worktree || task.metadata?.branch;
  const { data: diffData } = useQuery({
    queryKey: ["task-diff", agentId, task.task_number],
    queryFn: async () => {
      const resp = await fetch(
        `/api/agents/tasks/${task.task_number}/diff?agent_id=${agentId}`
      );
      if (!resp.ok) return { diff: "", branch: null, worktree: null };
      return resp.json() as Promise<{
        diff: string;
        branch: string | null;
        worktree: string | null;
      }>;
    },
    enabled: !!hasDiffSource,
    staleTime: 10_000,
  });

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
            {!!task.metadata?.assigned_agent && (
              <Badge variant="accent" size="md">
                Agent: {String(task.metadata.assigned_agent)}
              </Badge>
            )}
            {!!task.metadata?.worktree && (
              <Badge variant="default" size="md">
                Worktree: {String(task.metadata.worktree)}
              </Badge>
            )}
          </div>

          {/* Dependencies */}
          {deps.length > 0 && (
            <div>
              <label className="mb-1 block text-xs text-ink-dull">
                Dependencies {blocked && <span className="text-red-400">(blocked)</span>}
              </label>
              <div className="flex flex-wrap gap-1.5">
                {deps.map((depNum) => {
                  const depTask = allTasks.find((t) => t.task_number === depNum);
                  const depDone = depTask?.status === "done";
                  return (
                    <span
                      key={depNum}
                      className={`inline-flex items-center gap-1 rounded-md px-2 py-1 text-xs ${
                        depDone
                          ? "bg-emerald-500/10 text-emerald-400"
                          : "bg-amber-500/10 text-amber-400"
                      }`}
                    >
                      {depDone ? "\u2713" : "\u23F3"} #{depNum}
                      {depTask && (
                        <span className="text-ink-faint"> {depTask.title.slice(0, 30)}{depTask.title.length > 30 ? "..." : ""}</span>
                      )}
                    </span>
                  );
                })}
              </div>
            </div>
          )}

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

          {/* Worktree Actions */}
          {!task.metadata?.worktree && task.status !== "done" && (
            <button
              className="w-full rounded-md border border-dashed border-app-line px-3 py-2 text-xs text-ink-faint hover:border-accent hover:text-accent transition-colors"
              onClick={async () => {
                const resp = await fetch(
                  `/api/agents/tasks/${task.task_number}/worktree`,
                  {
                    method: "POST",
                    headers: { "Content-Type": "application/json" },
                    body: JSON.stringify({ agent_id: agentId }),
                  }
                );
                if (resp.ok) {
                  // Invalidate to refresh task with new metadata
                  window.location.reload();
                }
              }}
            >
              Create Worktree for Task #{task.task_number}
            </button>
          )}

          {/* Inline Diff Review */}
          {diffData?.diff && (
            <div>
              <label className="mb-1 block text-xs text-ink-dull">
                Changes {diffData.branch && `(branch: ${diffData.branch})`}
                {diffData.worktree && `(worktree)`}
              </label>
              <DiffViewer diff={diffData.diff} maxHeight="300px" />
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
