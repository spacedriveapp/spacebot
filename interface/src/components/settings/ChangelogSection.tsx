import {useQuery} from "@tanstack/react-query";
import {api} from "@/api/client";
import {Markdown} from "@/components/Markdown";
import type {ChangelogRelease} from "./types";

function parseChangelog(raw: string): ChangelogRelease[] {
	const releases: ChangelogRelease[] = [];
	const versionPattern = /^## (v\d+\.\S+)/;
	let current: ChangelogRelease | null = null;
	const lines: string[] = [];

	for (const line of raw.split("\n")) {
		const match = line.match(versionPattern);
		if (match) {
			if (current) {
				current.body = lines.join("\n").trim();
				releases.push(current);
				lines.length = 0;
			}
			current = {version: match[1], body: ""};
			continue;
		}
		if (current) lines.push(line);
	}
	if (current) {
		current.body = lines.join("\n").trim();
		releases.push(current);
	}
	return releases;
}

export function ChangelogSection() {
	const {data: changelog, isLoading} = useQuery<string>({
		queryKey: ["changelog"],
		queryFn: api.changelog,
		staleTime: 60_000 * 60, // 1 hour — changelog is baked into the binary
	});

	const releases = changelog ? parseChangelog(changelog) : [];

	return (
		<div className="mx-auto max-w-2xl px-6 py-6">
			<div className="mb-6">
				<h2 className="font-plex text-sm font-semibold text-ink">Changelog</h2>
				<p className="mt-1 text-sm text-ink-dull">
					Release history for this Spacebot build.
				</p>
			</div>

			{isLoading ? (
				<div className="flex items-center gap-2 text-ink-dull">
					<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
					Loading changelog...
				</div>
			) : releases.length > 0 ? (
				<div className="flex flex-col gap-4">
					{releases.map((release) => (
						<div
							key={release.version}
							className="rounded-lg border border-app-line bg-app-box p-5"
						>
							<h3 className="font-plex text-2xl font-bold text-ink mb-3">
								{release.version}
							</h3>
							{release.body && (
								<Markdown className="text-sm text-ink-dull">
									{release.body}
								</Markdown>
							)}
						</div>
					))}
				</div>
			) : (
				<p className="text-sm text-ink-faint">No changelog available.</p>
			)}
		</div>
	);
}
