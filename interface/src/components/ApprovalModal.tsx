import {useQuery, useMutation, useQueryClient} from "@tanstack/react-query";
import {Link} from "@tanstack/react-router";
import {
	DialogRoot,
	DialogContent,
	DialogHeader,
	DialogTitle,
	DialogFooter,
	Badge,
	Button,
} from "@spacedrive/primitives";
import {TaskDetail} from "@spacedrive/ai";
import {CheckCircle, XCircle, Warning, WarningCircle} from "@phosphor-icons/react";
import {api, type NotificationItem, type NotificationKind} from "@/api/client";
import {NOTIFICATIONS_QUERY_KEY} from "@/hooks/useNotifications";

interface ApprovalModalProps {
	notification: NotificationItem | null;
	onClose: () => void;
}

const KIND_CONFIG: Record<NotificationKind, {icon: React.ElementType; iconClass: string; label: string}> = {
	task_approval: {icon: CheckCircle, iconClass: "text-status-warning", label: "Approval"},
	worker_failed: {icon: XCircle, iconClass: "text-status-error", label: "Failed"},
	cortex_observation: {icon: WarningCircle, iconClass: "text-status-warning", label: "Alert"},
	// A stopped pipeline, not a dead process and not an agent's remark — the two
	// kinds it would otherwise be read as. The triangle is the one shape no other
	// kind here uses, and the footer gains a way to reach the run it names.
	workflow_run_stopped: {icon: Warning, iconClass: "text-status-warning", label: "Run stopped"},
};

function timeAgo(isoString: string): string {
	const diff = Date.now() - new Date(isoString).getTime();
	const mins = Math.floor(diff / 60_000);
	if (mins < 1) return "just now";
	if (mins < 60) return `${mins}m ago`;
	const hours = Math.floor(mins / 60);
	if (hours < 24) return `${hours}h ago`;
	return `${Math.floor(hours / 24)}d ago`;
}

export function ApprovalModal({notification, onClose}: ApprovalModalProps) {
	const queryClient = useQueryClient();
	const open = notification !== null;

	const kind = notification
		? (notification.kind in KIND_CONFIG ? notification.kind : "cortex_observation") as NotificationKind
		: "cortex_observation";
	const config = KIND_CONFIG[kind];
	const Icon = config.icon;

	const taskNumber = notification?.related_entity_type === "task" && notification.related_entity_id
		? Number(notification.related_entity_id)
		: null;

	const {data: taskData, isLoading: isTaskLoading} = useQuery({
		queryKey: ["task", taskNumber],
		queryFn: () => api.getTask(taskNumber!),
		enabled: open && taskNumber !== null,
	});

	const removeNotificationFromCache = (id: string) => {
		queryClient.setQueriesData(
			{queryKey: NOTIFICATIONS_QUERY_KEY},
			(old: unknown) => {
				const data = old as {notifications?: NotificationItem[]} | undefined;
				if (!data?.notifications) return old;
				return {...data, notifications: data.notifications.filter((n) => n.id !== id)};
			},
		);
	};

	const approveMutation = useMutation({
		mutationFn: () => api.approveTask(taskNumber!, "human"),
		onSuccess: () => {
			if (notification) removeNotificationFromCache(notification.id);
			queryClient.invalidateQueries({queryKey: NOTIFICATIONS_QUERY_KEY});
			queryClient.invalidateQueries({queryKey: ["tasks"]});
			queryClient.invalidateQueries({queryKey: ["task", taskNumber]});
			onClose();
		},
	});

	const dismissMutation = useMutation({
		mutationFn: () => api.dismissNotification(notification!.id),
		onSuccess: () => {
			if (notification) removeNotificationFromCache(notification.id);
			queryClient.invalidateQueries({queryKey: NOTIFICATIONS_QUERY_KEY});
			onClose();
		},
	});

	const isTaskApproval = kind === "task_approval" && taskNumber !== null;

	// The run this notification is about, when there is one. Read from the
	// `related_entity_*` pair the server sets rather than by parsing `action_url`,
	// so a route rename is a compile error here instead of a dead link.
	const stoppedRunId =
		kind === "workflow_run_stopped" &&
		notification?.related_entity_type === "workflow_run"
			? (notification.related_entity_id ?? null)
			: null;

	return (
		<DialogRoot open={open} onOpenChange={(v) => {if (!v) onClose();}}>
			<DialogContent className="!flex max-h-[80vh] w-full max-w-xl !flex-col !gap-0 overflow-hidden !p-0">
				{/* Header */}
				<DialogHeader className="flex-shrink-0 border-b border-app-line/50 px-5 pt-5 pb-4">
					<div className="flex items-center gap-2.5">
						<Icon className={`size-5 shrink-0 ${config.iconClass}`} weight="fill" />
						<div className="min-w-0 flex-1">
							<DialogTitle className="truncate text-sm font-semibold text-ink">
								{notification?.title ?? "Notification"}
							</DialogTitle>
							<div className="mt-0.5 flex items-center gap-2 text-xs text-ink-faint">
								<Badge variant="default" size="sm">{config.label}</Badge>
								{notification?.agent_id && <span>{notification.agent_id}</span>}
								{notification && <span>·</span>}
								{notification && <span>{timeAgo(notification.created_at)}</span>}
							</div>
						</div>
					</div>
				</DialogHeader>

				{/* Body */}
				<div className="flex-1 overflow-y-auto">
					{isTaskApproval ? (
						isTaskLoading ? (
							<div className="flex items-center justify-center py-12">
								<span className="text-xs text-ink-dull">Loading task…</span>
							</div>
						) : taskData?.task ? (
							<TaskDetail task={taskData.task as any} />
						) : (
							<div className="flex items-center justify-center py-12">
								<span className="text-xs text-ink-dull">Task not found</span>
							</div>
						)
					) : stoppedRunId ? (
						// The reason, set apart rather than run in as body text. It names
						// the task and the hold — blocked for a person, a gate that can no
						// longer open, a placeholder that will never expand, inputs that
						// will never resolve — and it is the only thing here that says what
						// to do next.
						<div className="px-5 py-4">
							<p className="mb-1 text-xs font-medium uppercase tracking-wide text-ink-dull">
								Why it stopped
							</p>
							<p className="whitespace-pre-wrap rounded border border-status-warning/40 bg-status-warning/5 px-3 py-2 text-sm text-status-warning">
								{notification?.body ?? "No reason was recorded."}
							</p>
							<p className="mt-3 text-xs text-ink-faint">
								This run is not going to continue on its own. Open it to see
								where it stopped, and to cancel or delete it.
							</p>
						</div>
					) : (
						<div className="px-5 py-4">
							{notification?.body ? (
								<p className="whitespace-pre-wrap text-sm text-ink">{notification.body}</p>
							) : (
								<p className="text-sm italic text-ink-faint">No additional details</p>
							)}
						</div>
					)}
				</div>

				{/* Footer */}
				<DialogFooter className="flex-shrink-0 border-t border-app-line/50 px-5 py-3">
					<div className="flex w-full items-center justify-end gap-2">
						<Button
							variant="subtle"
							size="sm"
							onClick={() => dismissMutation.mutate()}
							disabled={dismissMutation.isPending || approveMutation.isPending}
						>
							Dismiss
						</Button>
						{isTaskApproval && (
							<Button
								variant="accent"
								size="sm"
								onClick={() => approveMutation.mutate()}
								disabled={approveMutation.isPending || !taskData?.task}
							>
								{approveMutation.isPending ? "Approving…" : "Approve"}
							</Button>
						)}
						{stoppedRunId && (
							<Link
								to="/workflow-runs/$runId"
								params={{runId: stoppedRunId}}
								onClick={onClose}
							>
								<Button variant="accent" size="sm">
									Open run
								</Button>
							</Link>
						)}
					</div>
				</DialogFooter>
			</DialogContent>
		</DialogRoot>
	);
}
