import {useQuery, useMutation, useQueryClient} from "@tanstack/react-query";
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
import {CheckCircle, XCircle, WarningCircle} from "@phosphor-icons/react";
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
					</div>
				</DialogFooter>
			</DialogContent>
		</DialogRoot>
	);
}
