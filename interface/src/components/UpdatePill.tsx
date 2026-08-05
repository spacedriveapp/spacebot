import {useQuery} from "@tanstack/react-query";
import {Link} from "@tanstack/react-router";
import {api} from "@/api/client";

export function UpdatePill() {
	const {data} = useQuery({
		queryKey: ["update-check"],
		queryFn: api.updateCheck,
		staleTime: 60_000,
		refetchInterval: 300_000,
	});

	if (!data || !data.update_available || data.deployment === "hosted") {
		return null;
	}

	return (
		<Link
			to="/settings"
			search={{tab: "updates"}}
			className="inline-flex items-center gap-2 rounded-full border border-accent/30 bg-accent/10 px-3 py-1 text-xs font-medium text-accent transition-colors hover:border-accent/60 hover:bg-accent/20"
		>
			<span className="h-1.5 w-1.5 rounded-full bg-accent" />
			Update {data.latest_version ?? "available"}
		</Link>
	);
}
