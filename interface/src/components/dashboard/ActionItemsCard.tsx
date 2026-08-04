import {useState} from "react";
import {Link} from "@tanstack/react-router";
import {
	CheckCircle,
	Clock,
	Warning,
	WarningCircle,
	XCircle,
} from "@phosphor-icons/react";
import {
	Card,
	CardHeader,
	CardContent,
	Badge,
	Button,
} from "@spacedrive/primitives";
import {useNotifications} from "@/hooks/useNotifications";
import type {NotificationItem, NotificationKind} from "@/api/client";
import {ApprovalModal} from "@/components/ApprovalModal";

const TYPE_CONFIG: Record<
	NotificationKind,
	{
		icon: React.ElementType;
		iconClass: string;
		badgeVariant: "warning" | "destructive" | "secondary";
		label: string;
		action: string;
	}
> = {
	task_approval: {
		icon: Clock,
		iconClass: "text-ink-faint",
		badgeVariant: "warning",
		label: "Review",
		action: "Review",
	},
	worker_failed: {
		icon: XCircle,
		iconClass: "text-status-error",
		badgeVariant: "destructive",
		label: "Failed",
		action: "View",
	},
	cortex_observation: {
		icon: WarningCircle,
		iconClass: "text-status-warning",
		badgeVariant: "secondary",
		label: "Alert",
		action: "Review",
	},
	// A pipeline that will not continue on its own. Its own row style rather than
	// the generic `cortex_observation` fallback it used to land in, because that
	// one is an agent's remark and this is a stopped run: the triangle separates
	// it from the circle every other alert wears, and the action goes to the run
	// rather than to a modal, because the run is where the recovery is.
	workflow_run_stopped: {
		icon: Warning,
		iconClass: "text-status-warning",
		badgeVariant: "warning",
		label: "Run stopped",
		action: "Open run",
	},
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

/**
 * The run a notification is about, when it is about one.
 *
 * Read from `related_entity_*` rather than by parsing `action_url`: the pair is
 * what the server sets deliberately, and a URL is a string that a route rename
 * would quietly turn into a dead link.
 */
function stoppedRunId(item: NotificationItem): string | null {
	return item.related_entity_type === "workflow_run"
		? (item.related_entity_id ?? null)
		: null;
}

export function ActionItemsCard() {
	const {notifications, dismiss} = useNotifications("unread");
	const [activeNotification, setActiveNotification] =
		useState<NotificationItem | null>(null);
	const [atBottom, setAtBottom] = useState(false);

	const handleScroll = (e: React.UIEvent<HTMLDivElement>) => {
		const el = e.currentTarget;
		setAtBottom(el.scrollHeight - el.scrollTop - el.clientHeight < 4);
	};

	return (
		<Card variant="dark" className="flex h-full min-h-0 flex-col">
			<CardHeader className="flex-row items-center justify-between p-4 pb-3">
				<div className="flex items-center gap-2">
					<h2 className="font-plex text-sm font-medium text-ink-dull">Inbox</h2>
					{notifications.length > 0 && (
						<Badge variant="default" size="sm">
							{notifications.length}
						</Badge>
					)}
				</div>
			</CardHeader>

			<div className="relative min-h-0 flex-1">
				<div
					className={`pointer-events-none absolute inset-x-0 bottom-0 z-10 h-8 rounded-b-2xl bg-gradient-to-t from-app-dark-box to-transparent transition-opacity duration-150 ${atBottom || notifications.length === 0 ? "opacity-0" : "opacity-100"}`}
				/>
				<CardContent
					className="flex h-full flex-col gap-2 overflow-y-auto px-6 pb-4 pt-0"
					onScroll={handleScroll}
				>
					{notifications.length === 0 ? (
						<div className="flex flex-1 items-center justify-center">
							<div className="text-center">
								<CheckCircle className="mx-auto mb-2 h-8 w-8 text-ink-faint" />
								<p className="text-sm text-ink-faint">All caught up</p>
							</div>
						</div>
					) : (
						notifications.map((item) => {
							const kind = (
								item.kind in TYPE_CONFIG ? item.kind : "cortex_observation"
							) as NotificationKind;
							const config = TYPE_CONFIG[kind];
							const Icon = config.icon;
							const runId =
								kind === "workflow_run_stopped" ? stoppedRunId(item) : null;
							return (
								<div
									key={item.id}
									className={`flex items-start gap-3 rounded-lg border px-3 py-2.5 transition-colors ${
										kind === "workflow_run_stopped"
											? "border-status-warning/40 bg-status-warning/5 hover:bg-status-warning/10"
											: "border-app-line/50 bg-app-hover/20 hover:bg-app-hover/40"
									}`}
								>
									<Icon
										weight="fill"
										className={`mt-0.5 h-4 w-4 shrink-0 ${config.iconClass}`}
									/>
									<div className="min-w-0 flex-1">
										<p className="truncate text-sm text-ink">{item.title}</p>
										{/* The reason, on the card. "Stuck" alone is what sends
										    somebody reading rows, and the whole point of putting a
										    reason on the transition was to stop that. Clipped to two
										    lines because these name a task and a hold and run long;
										    it opens the modal, which shows the whole sentence. */}
										{kind === "workflow_run_stopped" && item.body && (
											<button
												type="button"
												onClick={() => setActiveNotification(item)}
												title="See the whole reason"
												className="mt-0.5 line-clamp-2 text-left text-tiny text-ink-dull hover:text-ink"
											>
												{item.body}
											</button>
										)}
										<div className="mt-1 flex items-center gap-2">
											<Badge size="sm">{config.label}</Badge>
											{item.agent_id && (
												<span className="text-tiny text-ink-faint">
													{item.agent_id}
												</span>
											)}
											{item.agent_id && (
												<span className="text-tiny text-ink-faint/50">·</span>
											)}
											<span className="text-tiny text-ink-faint">
												{timeAgo(item.created_at)}
											</span>
										</div>
									</div>
									{/* A stopped run is answered by looking at the run, so the
									    primary action goes straight there. Dismiss sits beside it
									    rather than inside the modal, because this row already shows
									    everything the modal would have been opened to read. */}
									{runId ? (
										<div className="flex shrink-0 items-center gap-1">
											<Link to="/workflow-runs/$runId" params={{runId}}>
												<Button size="xs" variant="subtle">
													{config.action}
												</Button>
											</Link>
											<Button
												size="xs"
												variant="subtle"
												onClick={() => dismiss(item.id)}
												title="Dismiss without opening the run"
											>
												Dismiss
											</Button>
										</div>
									) : (
										<Button
											size="xs"
											variant="subtle"
											className="shrink-0"
											onClick={() => setActiveNotification(item)}
										>
											{config.action}
										</Button>
									)}
								</div>
							);
						})
					)}
				</CardContent>
			</div>

			<ApprovalModal
				notification={activeNotification}
				onClose={() => setActiveNotification(null)}
			/>
		</Card>
	);
}
