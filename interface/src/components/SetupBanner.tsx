import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { api } from "@/api/client";
import { Banner } from "@/ui";

export function SetupBanner() {
	const { data } = useQuery({
		queryKey: ["providers"],
		queryFn: api.providers,
		staleTime: 10_000,
	});

	if (!data || data.has_any) return null;

	return (
		<Banner variant="warning" dot="static">
			No LLM provider configured.{" "}
			<Link to="/settings" className="underline hover:text-warning">
				Add an API key in Settings
			</Link>{" "}
			to get started.
		</Banner>
	);
}
