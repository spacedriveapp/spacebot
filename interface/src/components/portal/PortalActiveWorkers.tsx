import { Link } from "@tanstack/react-router";
import { isOpenCodeWorker, type ActiveWorker } from "@/hooks/useChannelLiveState";

interface PortalActiveWorkersProps {
	workers: ActiveWorker[];
	agentId: string;
}

export function PortalActiveWorkers({ workers, agentId }: PortalActiveWorkersProps) {
	if (workers.length === 0) return null;

	return (
		<div className="min-w-0 overflow-hidden rounded-lg border border-app-line/50 bg-app-box/30 px-3 py-2">
			<div className="mb-2 flex items-center gap-1.5 text-tiny text-ink-dull">
				<div className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
				<span>
					{workers.length} active worker{workers.length !== 1 ? "s" : ""}
				</span>
			</div>
			<div className="flex flex-col gap-1.5">
				{workers.map((worker) => {
					const oc = isOpenCodeWorker(worker);
					return (
						<Link
							key={worker.id}
							to="/agents/$agentId/workers"
							params={{ agentId }}
							search={{ worker: worker.id }}
							className="flex min-w-0 items-center gap-2 rounded-md bg-app-box/50 px-2.5 py-1.5 text-tiny transition-colors hover:bg-app-hover"
						>
							<div className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
							<span className="font-medium text-ink">
								{oc ? "OpenCode" : "Worker"}
							</span>
							<span className="min-w-0 flex-1 truncate text-ink-dull">
								{worker.task}
							</span>
							<span className="shrink-0 text-ink-faint">{worker.status}</span>
							{worker.currentTool && (
								<span className="max-w-40 shrink-0 truncate text-ink-faint">
									{worker.currentTool}
								</span>
							)}
						</Link>
					);
				})}
			</div>
		</div>
	);
}
