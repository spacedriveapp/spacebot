import { Banner } from "@/ui";
import type { ConnectionState } from "@/hooks/useEventSource";
import { useDesktopInstance } from "@/hooks/useDesktopInstance";

const stateConfig: Record<Exclude<ConnectionState, "connected">, { label: string; variant: "info" | "warning" | "error" }> = {
	connecting: { label: "Connecting...", variant: "info" },
	reconnecting: { label: "Reconnecting... Dashboard may show stale data.", variant: "warning" },
	disconnected: { label: "Disconnected from server.", variant: "error" },
};

export function ConnectionBanner({ state, hasData }: { state: ConnectionState; hasData: boolean }) {
	const { currentUrl, isDesktop, openDialog } = useDesktopInstance();
	// Don't show "Connecting..." if we already have data loaded
	if (state === "connecting" && hasData) return null;
	
	if (state === "connected") return null;

	const { label, variant } = stateConfig[state];

	return (
		<Banner variant={variant} dot="pulse">
			<div className="flex w-full items-center justify-between gap-4">
				<span>{label}</span>
				{isDesktop ? (
					<button
						type="button"
						onClick={openDialog}
						className="shrink-0 text-xs text-current underline underline-offset-4 opacity-90 hover:opacity-100"
					>
						{currentUrl}
					</button>
				) : null}
			</div>
		</Banner>
	);
}
