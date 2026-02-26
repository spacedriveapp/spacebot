import { useState } from "react";
import { useQuery, useMutation } from "@tanstack/react-query";
import { api } from "@/api/client";
import { Banner, BannerActions, Button } from "@/ui";
import { Cancel01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";

export function UpdateBanner() {
	const [dismissed, setDismissed] = useState(false);

	const { data } = useQuery({
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

	// Platform-managed instances get updates via rollout, not self-service
	if (!data || !data.update_available || dismissed || data.deployment === "hosted") return null;

	const isApplying = applyMutation.isPending;
	const isDocker = data.deployment === "docker";
	const isNative = data.deployment === "native";

	return (
		<div>
			<Banner variant="cyan" dot="static" className="border-cyan-500/20">
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
				<BannerActions>
					{isDocker && data.can_apply && (
						<Button
							onClick={() => {
								setApplyError(null);
								applyMutation.mutate();
							}}
							size="sm"
							loading={isApplying}
							className="bg-cyan-500/20 text-xs text-cyan-300 hover:bg-cyan-500/30"
						>
							Update now
						</Button>
					)}
					{isDocker && !data.can_apply && (
						<span className="text-xs text-ink-faint">
							{data.cannot_apply_reason ?? "Mount a container runtime socket for one-click updates"}
						</span>
					)}
					{isNative && (
						<span className="text-xs text-ink-faint">
							{data.cannot_apply_reason ?? "Native/source installs update manually (rebuild + restart)"}
						</span>
					)}
					<Button
						onClick={() => setDismissed(true)}
						variant="ghost"
						size="icon"
						className="h-7 w-7"
					>
						<HugeiconsIcon icon={Cancel01Icon} size={14} />
					</Button>
				</BannerActions>
			</Banner>
			{applyError && (
				<div className="border-b border-red-500/20 bg-red-500/10 px-4 py-1 text-xs text-red-400">
					{applyError}
				</div>
			)}
		</div>
	);
}
