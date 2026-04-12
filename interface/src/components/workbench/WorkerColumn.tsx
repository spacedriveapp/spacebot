import {useState} from "react";
import {cx} from "class-variance-authority";
import {api} from "@/api/client";
import {LiveDuration} from "@/components/LiveDuration";
import {OpenCodeEmbed} from "@/components/OpenCodeEmbed";
import type {OrchestrationWorker} from "./types";

export function WorkerColumn({worker}: {worker: OrchestrationWorker}) {
	const isRunning = worker.status === "running";
	const isIdle = worker.status === "idle";
	const toolCalls = worker.live_tool_calls ?? worker.tool_calls;

	// Strip the [opencode] prefix from the task text for display
	const taskText = worker.task.replace(/^\[opencode\]\s*/i, "");

	return (
		<div className="flex h-full w-[560px] flex-shrink-0 flex-col overflow-hidden rounded-2xl border border-app-line bg-app-box">
			{/* Column header */}
			<div className="flex flex-col gap-1 h-[60px] border-b border-app-line px-3 py-2">
				<div className="flex items-center gap-2">
					{/* Status dot */}
					<span
						className={cx(
							"h-2 w-2 bg-ink-faint flex-shrink-0 rounded-full",
							isRunning && "animate-pulse bg-green-500",
							isIdle && "bg-yellow-500",
							!isRunning && !isIdle && "bg-ink-faint",
						)}
					/>
					{/* Task text */}
					<p
						className="min-w-0 flex-1 truncate text-xs font-medium text-ink"
						title={taskText}
					>
						{taskText}
					</p>
					{/* Cancel button for running workers */}
					{(isRunning || isIdle) && worker.channel_id && (
						<CancelButton channelId={worker.channel_id} workerId={worker.id} />
					)}
				</div>
				<div className="flex items-center gap-2 text-tiny text-ink-faint">
					<span>{worker.agent_name}</span>
					<span>&middot;</span>
					{isRunning ? (
						<LiveDuration startMs={new Date(worker.started_at).getTime()} />
					) : (
						<span>{worker.status}</span>
					)}
					{toolCalls > 0 && (
						<>
							<span>&middot;</span>
							<span>
								{toolCalls} tool{toolCalls !== 1 ? "s" : ""}
							</span>
						</>
					)}
				</div>
			</div>

			{/* OpenCode embed */}
			<div className="flex flex-1 bg-app flex-col overflow-hidden">
				<OpenCodeEmbed
					port={worker.opencode_port!}
					sessionId={worker.opencode_session_id ?? worker.id}
					directory={worker.directory ?? null}
				/>
			</div>
		</div>
	);
}

function CancelButton({
	channelId,
	workerId,
}: {
	channelId: string;
	workerId: string;
}) {
	const [cancelling, setCancelling] = useState(false);

	return (
		<button
			disabled={cancelling}
			onClick={(event) => {
				event.stopPropagation();
				setCancelling(true);
				api
					.cancelProcess(channelId, "worker", workerId)
					.catch(console.warn)
					.finally(() => setCancelling(false));
			}}
			className="flex-shrink-0 rounded-md border border-app-line px-1.5 py-0.5 text-tiny font-medium text-ink-dull transition-colors hover:border-red-500/50 hover:text-red-400 disabled:opacity-50"
			title="Cancel worker"
		>
			{cancelling ? "..." : "Cancel"}
		</button>
	);
}
