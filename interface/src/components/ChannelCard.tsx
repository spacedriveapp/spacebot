import { Link } from "@tanstack/react-router";
import { AnimatePresence, motion } from "framer-motion";
import type { ChannelInfo } from "@/api/client";
import type { ActiveBranch, ActiveWorker, ChannelLiveState } from "@/hooks/useChannelLiveState";
import { LiveDuration } from "@/components/LiveDuration";
import { formatTimeAgo, formatTimestamp, platformIcon, platformColor } from "@/lib/format";

const VISIBLE_MESSAGES = 6;

function WorkerBadge({ worker }: { worker: ActiveWorker }) {
	return (
		<div className="flex items-center gap-2 rounded-md bg-warning/10 px-2.5 py-1.5 text-tiny">
			<div className="h-1.5 w-1.5 animate-pulse rounded-full bg-warning" />
			<div className="min-w-0 flex-1">
				<div className="flex items-center gap-1.5">
					<span className="font-medium text-warning">Worker</span>
					<span className="truncate text-ink-dull">{worker.task}</span>
				</div>
				<div className="mt-0.5 flex items-center gap-2 text-ink-faint">
					<span>{worker.status}</span>
					{worker.currentTool && (
						<>
							<span className="text-ink-faint/50">·</span>
							<span className="text-warning/70">{worker.currentTool}</span>
						</>
					)}
					{worker.toolCalls > 0 && (
						<>
							<span className="text-ink-faint/50">·</span>
							<span>{worker.toolCalls} tools</span>
						</>
					)}
				</div>
			</div>
		</div>
	);
}

function BranchBadge({ branch }: { branch: ActiveBranch }) {
	const displayTool = branch.currentTool ?? branch.lastTool;
	return (
		<div className="flex items-center gap-2 rounded-md bg-violet-500/10 px-2.5 py-1.5 text-tiny">
			<div className="h-1.5 w-1.5 animate-pulse rounded-full bg-violet-400" />
			<div className="min-w-0 flex-1">
				<div className="flex items-center gap-1.5">
					<span className="font-medium text-violet-300">Branch</span>
					<span className="truncate text-ink-dull">{branch.description}</span>
				</div>
				<div className="mt-0.5 flex items-center gap-2 text-ink-faint">
					<LiveDuration startMs={branch.startedAt} />
					{displayTool && (
						<>
							<span className="text-ink-faint/50">·</span>
							<span className={branch.currentTool ? "text-violet-400/70" : "text-violet-400/40"}>{displayTool}</span>
						</>
					)}
					{branch.toolCalls > 0 && (
						<>
							<span className="text-ink-faint/50">·</span>
							<span>{branch.toolCalls} tools</span>
						</>
					)}
				</div>
			</div>
		</div>
	);
}

export function ChannelCard({
	channel,
	liveState,
}: {
	channel: ChannelInfo;
	liveState: ChannelLiveState | undefined;
}) {
	const isTyping = liveState?.isTyping ?? false;
	const timeline = liveState?.timeline ?? [];
	const messages = timeline.filter((item) => item.type === "message");
	const workers = Object.values(liveState?.workers ?? {});
	const branches = Object.values(liveState?.branches ?? {});
	const visible = messages.slice(-VISIBLE_MESSAGES);
	const hasActivity = workers.length > 0 || branches.length > 0;

	return (
		<Link
			to="/agents/$agentId/channels/$channelId"
			params={{ agentId: channel.agent_id, channelId: channel.id }}
			className="flex flex-col rounded-lg border border-app-line bg-app-darkBox transition-colors hover:border-app-line/80 hover:bg-app-darkBox/80"
		>
			{/* Header */}
			<div className="flex items-start justify-between p-4 pb-2">
				<div className="min-w-0 flex-1">
					<div className="flex items-center gap-2">
						<h3 className="truncate font-medium text-ink">
							{channel.display_name ?? channel.id}
						</h3>
						{isTyping && (
							<div className="flex items-center gap-1">
								<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
								<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-accent [animation-delay:0.2s]" />
								<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-accent [animation-delay:0.4s]" />
							</div>
						)}
					</div>
					<div className="mt-1 flex items-center gap-2">
						<span className={`inline-flex items-center rounded-md px-1.5 py-0.5 text-tiny font-medium ${platformColor(channel.platform)}`}>
							{platformIcon(channel.platform)}
						</span>
						<span className="text-tiny text-ink-faint">
							{formatTimeAgo(channel.last_activity_at)}
						</span>
						{hasActivity && (
							<span className="text-tiny text-ink-faint">
								{workers.length > 0 && `${workers.length}w`}
								{workers.length > 0 && branches.length > 0 && " "}
								{branches.length > 0 && `${branches.length}b`}
							</span>
						)}
					</div>
				</div>
				<div className="ml-2 flex-shrink-0">
					<div className={`h-2 w-2 rounded-full ${
						hasActivity ? "bg-warning animate-pulse" :
						isTyping ? "bg-accent animate-pulse" :
						"bg-success/60"
					}`} />
				</div>
			</div>

			{/* Active workers and branches */}
			{hasActivity && (
				<div className="flex flex-col gap-1.5 px-4 pb-2">
					{workers.map((worker) => (
						<WorkerBadge key={worker.id} worker={worker} />
					))}
					{branches.map((branch) => (
						<BranchBadge key={branch.id} branch={branch} />
					))}
				</div>
			)}

			{/* Message stream */}
			{visible.length > 0 && (
				<div className="flex flex-col border-t border-app-line/50 p-3">
					{messages.length > VISIBLE_MESSAGES && (
						<span className="mb-1 text-tiny text-ink-faint">
							{messages.length - VISIBLE_MESSAGES} earlier messages
						</span>
					)}
					<AnimatePresence initial={false}>
						{visible.map((message) => {
							if (message.type !== "message") return null;
							return (
								<motion.div
									key={message.id}
									initial={{ opacity: 0, height: 0, marginTop: 0 }}
									animate={{ opacity: 1, height: "auto", marginTop: 4 }}
									exit={{ opacity: 0, height: 0, marginTop: 0 }}
									transition={{ type: "spring", stiffness: 500, damping: 35 }}
									className="flex gap-2 overflow-hidden text-sm first:!mt-0"
								>
									<span className="flex-shrink-0 text-tiny text-ink-faint">
										{formatTimestamp(new Date(message.created_at).getTime())}
									</span>
									<span className={`flex-shrink-0 text-tiny font-medium ${
										message.role === "user" ? "text-accent-faint" : "text-success"
									}`}>
										{message.role === "user" ? (message.sender_name ?? "user") : "bot"}
									</span>
									<p className="line-clamp-1 text-sm text-ink-faint">{message.content}</p>
								</motion.div>
							);
						})}
					</AnimatePresence>
				</div>
			)}
		</Link>
	);
}
