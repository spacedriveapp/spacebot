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

	return (
		<div>
			<Banner variant="accent" dot="static" className="border-accent/20">
				<span>
					Version <strong>{data.latest_version}</strong> is available
					<span className="text-ink-faint ml-1">(current: {data.current_version})</span>
				</span>
				{data.release_url && (
					<a
						href={data.release_url}
						target="_blank"
						rel="noopener noreferrer"
						className="underline hover:text-accent-faint"
					>
						Release notes
					</a>
				)}
				<BannerActions>
					{data.can_apply && (
						<Button
							onClick={() => {
								setApplyError(null);
								applyMutation.mutate();
							}}
							size="sm"
							loading={isApplying}
							className="bg-accent/20 text-xs text-accent-faint hover:bg-accent/30"
						>
							Update now
						</Button>
					)}
					{!data.can_apply && data.deployment === "docker" && (
						<span className="text-xs text-ink-faint">
							Mount docker.sock for one-click updates
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
				<div className="border-b border-error/20 bg-error/10 px-4 py-1 text-xs text-error">
					{applyError}
				</div>
			)}
		</div>
	);
}
