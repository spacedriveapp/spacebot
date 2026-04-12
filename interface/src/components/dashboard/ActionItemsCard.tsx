import {useState} from "react";
import {
	CheckCircle,
	Clock,
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

export function ActionItemsCard() {
	const {notifications} = useNotifications("unread");
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
							return (
								<div
									key={item.id}
									className="flex items-start gap-3 rounded-lg border border-app-line/50 bg-app-hover/20 px-3 py-2.5 transition-colors hover:bg-app-hover/40"
								>
									<Icon
										weight="fill"
										className={`mt-0.5 h-4 w-4 shrink-0 ${config.iconClass}`}
									/>
									<div className="min-w-0 flex-1">
										<p className="truncate text-sm text-ink">{item.title}</p>
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
									<Button
										size="xs"
										variant="subtle"
										className="shrink-0"
										onClick={() => setActiveNotification(item)}
									>
										{config.action}
									</Button>
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
