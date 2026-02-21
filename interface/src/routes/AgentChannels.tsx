import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/api/client";
import { ChannelCard } from "@/components/ChannelCard";
import { Button, SearchInput } from "@/ui";
import type { ChannelLiveState } from "@/hooks/useChannelLiveState";
import { Settings02Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { useNavigate } from "@tanstack/react-router";

interface AgentChannelsProps {
	agentId: string;
	liveStates: Record<string, ChannelLiveState>;
}

export function AgentChannels({ agentId, liveStates }: AgentChannelsProps) {
	const navigate = useNavigate();
	const [searchQuery, setSearchQuery] = useState("");

	const { data: channelsData, isLoading } = useQuery({
		queryKey: ["channels"],
		queryFn: api.channels,
		refetchInterval: 10_000,
	});

	const channels = useMemo(() => {
		const agentChannels = (channelsData?.channels ?? []).filter(
			(c) => c.agent_id === agentId,
		);
		if (!searchQuery) return agentChannels;
		const query = searchQuery.toLowerCase();
		return agentChannels.filter(
			(c) =>
				c.id.toLowerCase().includes(query) ||
				(c.display_name && c.display_name.toLowerCase().includes(query)) ||
				(c.platform && c.platform.toLowerCase().includes(query)),
		);
	}, [channelsData, agentId, searchQuery]);

	const hasChannels = channels.length > 0;

	return (
		<div className="flex h-full flex-col">
			{hasChannels && (
				<div className="flex items-center gap-3 border-b border-app-line/50 bg-app-darkBox/20 px-4 py-2.5">
					<SearchInput
						placeholder="Search channels..."
						value={searchQuery}
						onChange={(event) => setSearchQuery(event.target.value)}
						className="flex-1"
					/>
				</div>
			)}
			<div className="flex-1 overflow-y-auto p-4">
				{isLoading ? (
					<div className="flex items-center gap-2 text-ink-dull">
						<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
						Loading channels...
					</div>
				) : hasChannels ? (
					<div className="grid gap-3 grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 2xl:grid-cols-4">
						{channels.map((channel) => (
							<ChannelCard
								key={channel.id}
								channel={channel}
								liveState={liveStates[channel.id]}
							/>
						))}
					</div>
				) : (
					<div className="flex h-full items-start justify-center pt-[15vh]">
					<div className="flex max-w-sm flex-col items-center rounded-xl border border-dashed border-app-line/50 bg-app-darkBox/20 p-6 text-center">
						<div className="mb-3 flex h-10 w-10 items-center justify-center rounded-full border border-app-line bg-app-darkBox">
							<HugeiconsIcon icon={Settings02Icon} className="h-5 w-5 text-ink-faint" />
						</div>
						<h3 className="mb-1 text-[13px] font-medium text-ink">No channels yet</h3>
						<p className="mb-3 max-w-md text-[13px] text-ink-faint leading-snug">
								Channels appear here once the bot receives its first message. Make sure a messaging platform is configured and bound to this agent in Settings.
							</p>
							<Button 
								onClick={() => navigate({ to: "/settings", search: { tab: "channels" } })} 
								variant="secondary" 
								size="sm"
							>
								Go to Settings
							</Button>
						</div>
					</div>
				)}
			</div>
		</div>
	);
}
