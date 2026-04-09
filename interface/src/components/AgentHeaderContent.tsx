import {formatTokens, getTokenUsageColor} from "@/utils/tokens";

interface AgentHeaderContentProps {
	agentId: string;
	displayName?: string | null;
	contextUsage?: {
		estimatedTokens: number;
		contextWindow: number;
		usageRatio: number;
	} | null;
}

export function AgentHeaderContent({agentId, displayName, contextUsage}: AgentHeaderContentProps) {
	return (
		<div className="grid h-full w-full grid-cols-[minmax(0,1fr)_auto] items-center gap-4 px-6">
			<h1 className="min-w-0 truncate font-plex text-sm font-medium text-ink">
				{displayName ? (
					<>
						{displayName}
						<span className="ml-2 text-ink-faint">{agentId}</span>
					</>
				) : (
					agentId
				)}
			</h1>
			{contextUsage && contextUsage.estimatedTokens > 0 && (
				<div
					className={`justify-self-end whitespace-nowrap text-tiny font-mono ${getTokenUsageColor(contextUsage.usageRatio)}`}
					title={`${contextUsage.estimatedTokens.toLocaleString()} / ${contextUsage.contextWindow.toLocaleString()} tokens (${(contextUsage.usageRatio * 100).toFixed(1)}%)`}
				>
					{formatTokens(contextUsage.estimatedTokens)} / {formatTokens(contextUsage.contextWindow)}
				</div>
			)}
		</div>
	);
}
