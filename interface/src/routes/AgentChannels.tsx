import {useMemo, useState} from "react";
import {useQuery} from "@tanstack/react-query";
import {api} from "@/api/client";
import {ChannelCard} from "@/components/ChannelCard";
import {Button, SearchBar} from "@spacedrive/primitives";
import type {ChannelLiveState} from "@/hooks/useChannelLiveState";
import {GearSix} from "@phosphor-icons/react";
import {useNavigate} from "@tanstack/react-router";

interface AgentChannelsProps {
	agentId: string;
	liveStates: Record<string, ChannelLiveState>;
}

export function AgentChannels({agentId, liveStates}: AgentChannelsProps) {
	const navigate = useNavigate();
	const [searchQuery, setSearchQuery] = useState("");

	const {data: channelsData, isLoading} = useQuery({
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
				<div className="flex items-center gap-3 border-b border-app-line/50 bg-app-dark-box/20 px-6 py-3">
					<SearchBar
						placeholder="Search channels..."
						value={searchQuery}
						onChange={setSearchQuery}
						className="flex-1"
					/>
				</div>
			)}
			<div className="flex-1 overflow-y-auto p-6">
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
						<div className="flex max-w-sm flex-col items-center rounded-xl border border-dashed border-app-line/50 bg-app-dark-box/20 p-8 text-center">
							<div className="mb-4 flex h-12 w-12 items-center justify-center rounded-full border border-app-line bg-app-dark-box">
								<GearSix className="h-6 w-6 text-ink-faint" />
							</div>
							<h3 className="mb-1 font-plex text-sm font-medium text-ink">
								No channels yet
							</h3>
							<p className="mb-5 max-w-md text-sm text-ink-faint">
								Channels appear here once the bot receives its first message.
								Make sure a messaging platform is configured and bound to this
								agent in Settings.
							</p>
							<Button
								onClick={() =>
									navigate({to: "/settings", search: {tab: "channels"}})
								}
								variant="gray"
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
