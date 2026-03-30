import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/api/client";
import { Input } from "@/ui/Input";
import { clsx } from "clsx";

// ---------------------------------------------------------------------------
// View types
// ---------------------------------------------------------------------------

const VIEWS = [
	{ key: "communities", label: "Communities" },
	{ key: "entry-points", label: "Entry Points" },
	{ key: "search", label: "Search" },
	{ key: "index-log", label: "Index Log" },
] as const;

type ViewKey = (typeof VIEWS)[number]["key"];

// ---------------------------------------------------------------------------
// Communities View
// ---------------------------------------------------------------------------

function CommunitiesView({ projectId }: { projectId: string }) {
	const { data, isLoading } = useQuery({
		queryKey: ["codegraph-communities", projectId],
		queryFn: () => api.codegraphCommunities(projectId),
	});

	if (isLoading) return <p className="text-sm text-ink-faint">Loading communities...</p>;

	const communities = data?.communities ?? [];

	if (communities.length === 0) {
		return (
			<div className="flex flex-col items-center justify-center py-12">
				<p className="text-sm text-ink-faint">No communities detected yet</p>
				<p className="mt-1 text-xs text-ink-faint">Communities will appear after indexing completes</p>
			</div>
		);
	}

	return (
		<div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
			{communities.map((community) => (
				<div
					key={community.id}
					className="rounded-xl border border-app-line bg-app-darkBox p-4"
				>
					<h4 className="font-plex text-sm font-semibold text-ink">{community.name}</h4>
					{community.description && (
						<p className="mt-1 text-xs text-ink-dull">{community.description}</p>
					)}
					<div className="mt-3 flex gap-3 text-xs text-ink-faint">
						<span>{community.node_count} nodes</span>
						<span>{community.file_count} files</span>
						<span>{community.function_count} functions</span>
					</div>
					{community.key_symbols.length > 0 && (
						<div className="mt-2 flex flex-wrap gap-1">
							{community.key_symbols.slice(0, 5).map((sym) => (
								<span
									key={sym}
									className="rounded bg-accent/10 px-1.5 py-0.5 text-xs text-accent"
								>
									{sym}
								</span>
							))}
						</div>
					)}
				</div>
			))}
		</div>
	);
}

// ---------------------------------------------------------------------------
// Entry Points View
// ---------------------------------------------------------------------------

function EntryPointsView({ projectId }: { projectId: string }) {
	const { data, isLoading } = useQuery({
		queryKey: ["codegraph-processes", projectId],
		queryFn: () => api.codegraphProcesses(projectId),
	});

	if (isLoading) return <p className="text-sm text-ink-faint">Loading entry points...</p>;

	const processes = data?.processes ?? [];

	if (processes.length === 0) {
		return (
			<div className="flex flex-col items-center justify-center py-12">
				<p className="text-sm text-ink-faint">No entry points detected yet</p>
			</div>
		);
	}

	return (
		<div className="overflow-x-auto">
			<table className="w-full text-sm">
				<thead>
					<tr className="border-b border-app-line text-left text-xs text-ink-faint">
						<th className="pb-2 pr-4 font-medium">Entry Function</th>
						<th className="pb-2 pr-4 font-medium">File</th>
						<th className="pb-2 pr-4 font-medium">Depth</th>
						<th className="pb-2 font-medium">Community</th>
					</tr>
				</thead>
				<tbody>
					{processes.map((proc) => (
						<tr key={proc.id} className="border-b border-app-line/50">
							<td className="py-2 pr-4 font-mono text-xs text-ink">
								{proc.entry_function}
							</td>
							<td className="py-2 pr-4 text-xs text-ink-dull">{proc.source_file}</td>
							<td className="py-2 pr-4 text-xs text-ink-dull">{proc.call_depth}</td>
							<td className="py-2 text-xs text-ink-dull">{proc.community ?? "—"}</td>
						</tr>
					))}
				</tbody>
			</table>
		</div>
	);
}

// ---------------------------------------------------------------------------
// Search View
// ---------------------------------------------------------------------------

function SearchView({ projectId }: { projectId: string }) {
	const [query, setQuery] = useState("");

	const { data, isLoading } = useQuery({
		queryKey: ["codegraph-search", projectId, query],
		queryFn: () => api.codegraphSearch(projectId, query),
		enabled: query.length >= 2,
	});

	const results = data?.results ?? [];

	return (
		<div className="flex flex-col gap-4">
			<Input
				placeholder="Search symbols, functions, classes..."
				value={query}
				onChange={(e) => setQuery(e.target.value)}
			/>

			{query.length < 2 ? (
				<p className="text-center text-sm text-ink-faint">
					Type at least 2 characters to search
				</p>
			) : isLoading ? (
				<p className="text-center text-sm text-ink-faint">Searching...</p>
			) : results.length === 0 ? (
				<p className="text-center text-sm text-ink-faint">No results found</p>
			) : (
				<div className="flex flex-col gap-2">
					{results.map((result) => (
						<div
							key={result.node_id}
							className="rounded-lg border border-app-line bg-app-darkBox p-3"
						>
							<div className="flex items-center gap-2">
								<span className="rounded bg-accent/10 px-1.5 py-0.5 text-xs text-accent">
									{result.label}
								</span>
								<span className="font-mono text-sm text-ink">{result.name}</span>
								<span className="ml-auto text-xs text-ink-faint">
									score: {result.score.toFixed(3)}
								</span>
							</div>
							{result.source_file && (
								<p className="mt-1 text-xs text-ink-dull">
									{result.source_file}
									{result.line_start != null && `:${result.line_start}`}
								</p>
							)}
							{result.snippet && (
								<pre className="mt-2 overflow-x-auto rounded bg-app/50 p-2 text-xs text-ink-dull">
									{result.snippet}
								</pre>
							)}
						</div>
					))}
				</div>
			)}
		</div>
	);
}

// ---------------------------------------------------------------------------
// Index Log View
// ---------------------------------------------------------------------------

function IndexLogView({ projectId }: { projectId: string }) {
	const { data, isLoading } = useQuery({
		queryKey: ["codegraph-index-log", projectId],
		queryFn: () => api.codegraphIndexLog(projectId),
		refetchInterval: 5_000,
	});

	if (isLoading) return <p className="text-sm text-ink-faint">Loading index log...</p>;

	const entries = data?.entries ?? [];

	if (entries.length === 0) {
		return (
			<div className="flex flex-col items-center justify-center py-12">
				<p className="text-sm text-ink-faint">No index runs yet</p>
			</div>
		);
	}

	return (
		<div className="flex flex-col gap-3">
			{entries.map((entry) => (
				<div
					key={entry.run_id}
					className="rounded-lg border border-app-line bg-app-darkBox p-4"
				>
					<div className="flex items-center justify-between">
						<span className={clsx(
							"text-xs font-medium",
							entry.status === "indexed" && "text-emerald-400",
							entry.status === "indexing" && "text-blue-400",
							entry.status === "error" && "text-red-400",
						)}>
							{entry.status.toUpperCase()}
						</span>
						<span className="text-xs text-ink-faint">
							{new Date(entry.started_at).toLocaleString()}
						</span>
					</div>

					{entry.current_phase && (
						<p className="mt-2 text-sm text-ink-dull">
							Phase: {entry.current_phase}
						</p>
					)}

					{entry.stats && (
						<div className="mt-2 flex gap-4 text-xs text-ink-faint">
							<span>{entry.stats.files_parsed} files</span>
							<span>{entry.stats.nodes_created} nodes</span>
							<span>{entry.stats.edges_created} edges</span>
						</div>
					)}

					{entry.error && (
						<p className="mt-2 text-xs text-red-400">{entry.error}</p>
					)}
				</div>
			))}
		</div>
	);
}

// ---------------------------------------------------------------------------
// Main Component
// ---------------------------------------------------------------------------

export function CodeGraphTab({ projectId }: { projectId: string }) {
	const [activeView, setActiveView] = useState<ViewKey>("communities");

	return (
		<div className="flex flex-col gap-4">
			{/* View switcher */}
			<div className="flex gap-2">
				{VIEWS.map((view) => (
					<button
						key={view.key}
						onClick={() => setActiveView(view.key)}
						className={clsx(
							"rounded-md px-3 py-1.5 text-xs font-medium transition-colors",
							activeView === view.key
								? "bg-accent/20 text-accent"
								: "text-ink-dull hover:bg-app-selected/50",
						)}
					>
						{view.label}
					</button>
				))}
			</div>

			{/* View content */}
			{activeView === "communities" && <CommunitiesView projectId={projectId} />}
			{activeView === "entry-points" && <EntryPointsView projectId={projectId} />}
			{activeView === "search" && <SearchView projectId={projectId} />}
			{activeView === "index-log" && <IndexLogView projectId={projectId} />}
		</div>
	);
}
