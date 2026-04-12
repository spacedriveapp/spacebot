import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { InlineWorkerCard, type TranscriptStep } from "@spacedrive/ai";
import { api, type WorkerListItem } from "@/api/client";

interface PortalWorkerCardProps {
	agentId: string;
	worker: WorkerListItem;
}

/**
 * Adapter that wraps @spacedrive/ai's InlineWorkerCard with data fetching.
 * Polls the worker detail endpoint for transcript updates while running.
 */
export function PortalWorkerCard({ agentId, worker }: PortalWorkerCardProps) {
	const queryClient = useQueryClient();
	const detailQuery = useQuery({
		queryKey: ["worker-detail", agentId, worker.id],
		queryFn: () => api.workerDetail(agentId, worker.id),
		refetchInterval: worker.status === "running" ? 1500 : false,
	});

	const copyLogs = async () => {
		const detail = detailQuery.data;
		if (!detail) return;

		const transcriptText = (detail.transcript ?? [])
			.map((step) => JSON.stringify(step, null, 2))
			.join("\n\n");

		const payload = [
			`Worker: ${detail.task}`,
			`Status: ${detail.status}`,
			`Started: ${detail.started_at}`,
			detail.completed_at ? `Completed: ${detail.completed_at}` : null,
			detail.result ? `Result:\n${detail.result}` : null,
			transcriptText ? `Transcript:\n${transcriptText}` : null,
		]
			.filter(Boolean)
			.join("\n\n");

		await navigator.clipboard.writeText(payload);
	};

	const cancelMutation = useMutation({
		mutationFn: () =>
			api.cancelProcess(worker.channel_id ?? "", "worker", worker.id),
		onSuccess: async () => {
			await Promise.all([
				queryClient.invalidateQueries({
					queryKey: ["worker-detail", agentId, worker.id],
				}),
				queryClient.invalidateQueries({
					queryKey: ["portal-workers", agentId, worker.channel_id],
				}),
			]);
		},
	});

	const isRunning = worker.status === "running";
	const canCancel = isRunning && !!worker.channel_id && !cancelMutation.isPending;

	return (
		<InlineWorkerCard
			title={worker.task}
			status={worker.status}
			toolCallCount={worker.tool_calls}
			liveStatus={worker.live_status}
			transcript={(detailQuery.data?.transcript ?? []) as TranscriptStep[]}
			isTranscriptLoading={detailQuery.isLoading}
			onCopyLogs={detailQuery.data ? () => void copyLogs() : undefined}
			onCancel={canCancel ? () => void cancelMutation.mutateAsync() : undefined}
		/>
	);
}
