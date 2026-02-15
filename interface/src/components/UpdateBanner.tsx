import {useState} from "react";
import {useQuery, useMutation} from "@tanstack/react-query";
import {api} from "@/api/client";

export function UpdateBanner() {
	const [dismissed, setDismissed] = useState(false);

	const {data} = useQuery({
		queryKey: ["updateCheck"],
		queryFn: api.updateCheck,
		staleTime: 60_000,
		refetchInterval: 300_000,
	});

	const applyMutation = useMutation({
		mutationFn: api.updateApply,
		onSuccess: (result) => {
			if (result.status === "error") {
				setApplyError(result.error ?? "Update failed");
			}
		},
	});

	const [applyError, setApplyError] = useState<string | null>(null);

	if (!data || !data.update_available || dismissed) return null;

	const isApplying = applyMutation.isPending;

	return (
		<div className="border-b border-cyan-500/20 bg-cyan-500/10 px-4 py-2 text-sm text-cyan-400">
			<div className="flex items-center justify-between">
				<div className="flex items-center gap-2">
					<div className="h-1.5 w-1.5 rounded-full bg-current" />
					<span>
						Version <strong>{data.latest_version}</strong> is available
						<span className="text-ink-faint ml-1">(current: {data.current_version})</span>
					</span>
					{data.release_url && (
						<a
							href={data.release_url}
							target="_blank"
							rel="noopener noreferrer"
							className="underline hover:text-cyan-300"
						>
							Release notes
						</a>
					)}
				</div>
				<div className="flex items-center gap-2">
					{data.can_apply && (
						<button
							onClick={() => {
								setApplyError(null);
								applyMutation.mutate();
							}}
							disabled={isApplying}
							className="rounded bg-cyan-500/20 px-2.5 py-1 text-xs font-medium text-cyan-300 hover:bg-cyan-500/30 disabled:opacity-50"
						>
							{isApplying ? "Updating..." : "Update now"}
						</button>
					)}
					{!data.can_apply && data.deployment === "docker" && (
						<span className="text-xs text-ink-faint">
							Mount docker.sock for one-click updates
						</span>
					)}
					<button
						onClick={() => setDismissed(true)}
						className="text-ink-faint hover:text-ink ml-1"
					>
						<svg width="14" height="14" viewBox="0 0 14 14" fill="none">
							<path d="M3.5 3.5l7 7M10.5 3.5l-7 7" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
						</svg>
					</button>
				</div>
			</div>
			{applyError && (
				<div className="mt-1 text-xs text-red-400">{applyError}</div>
			)}
		</div>
	);
}
