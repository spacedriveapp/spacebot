import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { api } from "@/api/client";
import { Banner } from "@spacedrive/primitives";

export function SetupBanner() {
	const { data } = useQuery({
		queryKey: ["providers"],
		queryFn: api.providers,
		staleTime: 10_000,
	});

	if (!data || data.has_any) return null;

	return (
		<Banner variant="warning">
			No LLM provider configured.{" "}
			<Link to="/settings" className="underline hover:text-amber-300">
				Add an API key in Settings
			</Link>{" "}
			to get started.
		</Banner>
	);
}
