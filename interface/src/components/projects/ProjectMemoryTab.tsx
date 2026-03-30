import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/api/client";
import { clsx } from "clsx";

const MEMORY_TYPES = ["all", "fact", "observation", "goal", "decision", "preference"] as const;

export function ProjectMemoryTab({ projectId }: { projectId: string }) {
	const [typeFilter, setTypeFilter] = useState<string>("all");

	const { data, isLoading } = useQuery({
		queryKey: ["codegraph-memories", projectId],
		queryFn: () => api.codegraphMemories(projectId),
		refetchInterval: 30_000,
	});

	const memories = data?.memories ?? [];
	const filtered = typeFilter === "all"
		? memories
		: memories.filter((m) => m.memory_type === typeFilter);

	return (
		<div className="flex flex-col gap-4">
			{/* Type filter */}
			<div className="flex gap-2">
				{MEMORY_TYPES.map((t) => (
					<button
						key={t}
						onClick={() => setTypeFilter(t)}
						className={clsx(
							"rounded-md px-3 py-1 text-xs font-medium transition-colors",
							typeFilter === t
								? "bg-accent/20 text-accent"
								: "text-ink-dull hover:bg-app-selected/50",
						)}
					>
						{t === "all" ? "All" : t.charAt(0).toUpperCase() + t.slice(1)}
					</button>
				))}
			</div>

			{/* Memory list */}
			{isLoading ? (
				<p className="text-sm text-ink-faint">Loading memories...</p>
			) : filtered.length === 0 ? (
				<div className="flex flex-col items-center justify-center py-12">
					<p className="text-sm text-ink-faint">
						{memories.length === 0
							? "No project memories yet. They will be created after indexing."
							: "No memories match this filter"}
					</p>
				</div>
			) : (
				<div className="flex flex-col gap-3">
					{filtered.map((memory) => (
						<div
							key={memory.id}
							className="rounded-xl border border-app-line bg-app-darkBox p-4"
						>
							<div className="flex items-center gap-2">
								<span className="rounded bg-accent/10 px-1.5 py-0.5 text-xs text-accent">
									{memory.memory_type}
								</span>
								<span className="text-xs text-ink-faint">
									Verified {new Date(memory.last_verified_at).toLocaleDateString()}
								</span>
								<span className="ml-auto text-xs text-ink-faint">
									Relevance: {memory.relevance_score.toFixed(2)}
								</span>
							</div>
							<p className="mt-2 text-sm text-ink">{memory.content}</p>
							<div className="mt-2 flex items-center gap-2 text-xs text-ink-faint">
								<span>Source: {memory.source}</span>
								{memory.tags.length > 0 && (
									<>
										<span>|</span>
										{memory.tags.map((tag) => (
											<span key={tag} className="rounded bg-app-selected px-1 py-0.5">
												{tag}
											</span>
										))}
									</>
								)}
							</div>
						</div>
					))}
				</div>
			)}
		</div>
	);
}
