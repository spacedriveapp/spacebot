import {useQuery} from "@tanstack/react-query";
import {api} from "@/api/client";

export function SpacedriveExplorer() {
	const {data} = useQuery({
		queryKey: ["integrations"],
		queryFn: api.integrations,
		staleTime: 10_000,
	});

	const spacedrive = data?.integrations.find((i) => i.id === "spacedrive");
	const enabled = spacedrive?.enabled ?? false;
	const url = (spacedrive?.config?.web_url as string) || null;

	if (!enabled) {
		return (
			<div className="flex flex-1 items-center justify-center text-sm text-ink-faint">
				Spacedrive integration is disabled.
			</div>
		);
	}

	if (!url) {
		return (
			<div className="flex flex-1 items-center justify-center text-sm text-ink-faint">
				Spacedrive is enabled but no <code className="mx-1">web_url</code> is configured.
			</div>
		);
	}

	return (
		<iframe
			src={url}
			title="Spacedrive Explorer"
			className="size-full flex-1 rounded-2xl border border-app-line bg-app"
		/>
	);
}
