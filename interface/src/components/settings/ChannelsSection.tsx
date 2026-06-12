import {useState} from "react";
import {useQuery} from "@tanstack/react-query";
import {api} from "@/api/client";
import {
	PlatformCatalog,
	InstanceCard,
	AddInstanceCard,
} from "@/components/ChannelSettingCard";
import type {Platform} from "./types";

export function ChannelsSection() {
	const [expandedKey, setExpandedKey] = useState<string | null>(null);
	const [addingPlatform, setAddingPlatform] = useState<Platform | null>(null);

	const {data: messagingStatus, isLoading} = useQuery({
		queryKey: ["messaging-status"],
		queryFn: api.messagingStatus,
		staleTime: 5_000,
	});

	const instances = messagingStatus?.instances ?? [];

	// Determine whether to show default or named add form
	function handleAddInstance(platform: Platform) {
		setAddingPlatform(platform);
	}

	function isDefaultAdd(): boolean {
		if (!addingPlatform) return true;
		return !instances.some(
			(inst) => inst.platform === addingPlatform && inst.name === null,
		);
	}

	return (
		<div className="mx-auto max-w-3xl px-6 py-6">
			<div className="mb-6">
				<h2 className="font-plex text-sm font-semibold text-ink">
					Messaging Platforms
				</h2>
				<p className="mt-1 text-sm text-ink-dull">
					Connect messaging platforms and configure how conversations route to
					agents.
				</p>
			</div>

			{isLoading ? (
				<div className="flex items-center gap-2 text-ink-dull">
					<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
					Loading channels...
				</div>
			) : (
				<div className="grid grid-cols-[200px_1fr] gap-6">
					{/* Left column: Platform catalog */}
					<div className="flex-shrink-0">
						<PlatformCatalog onAddInstance={handleAddInstance} />
					</div>

					{/* Right column: Configured instances */}
					<div className="flex flex-col gap-3 min-w-0">
						{/* Active add-instance card */}
						{addingPlatform && (
							<AddInstanceCard
								platform={addingPlatform}
								isDefault={isDefaultAdd()}
								onCancel={() => setAddingPlatform(null)}
								onCreated={() => setAddingPlatform(null)}
							/>
						)}

						{/* Configured instance cards */}
						{instances.length > 0 ? (
							instances.map((instance) => (
								<InstanceCard
									key={instance.runtime_key}
									instance={instance}
									expanded={expandedKey === instance.runtime_key}
									onToggleExpand={() =>
										setExpandedKey(
											expandedKey === instance.runtime_key
												? null
												: instance.runtime_key,
										)
									}
								/>
							))
						) : !addingPlatform ? (
							<div className="rounded-lg border border-app-line border-dashed bg-app-box/50 p-8 text-center">
								<p className="text-sm text-ink-dull">
									No messaging platforms configured yet.
								</p>
								<p className="mt-1 text-sm text-ink-faint">
									Click a platform on the left to get started.
								</p>
							</div>
						) : null}
					</div>
				</div>
			)}
		</div>
	);
}
