import {useCallback, useEffect, useRef, useState} from "react";
import {useQuery} from "@tanstack/react-query";
import {useVirtualizer} from "@tanstack/react-virtual";
import {AnimatePresence, motion} from "framer-motion";
import {
	api,
	MEMORY_TYPES,
	type MemoryItem,
	type MemorySort,
	type MemoryType,
} from "@/api/client";
import {MemoryGraph} from "@/components/MemoryGraph";
import {
	CircleButton,
	CircleButtonGroup,
	SearchBar,
	FilterButton,
	SelectPill,
	Popover,
	OptionList,
	OptionListItem,
} from "@spacedrive/primitives";
import {formatTimeAgo} from "@/lib/format";
import {List, TreeStructure} from "@phosphor-icons/react";

type ViewMode = "list" | "graph";

const SORT_OPTIONS: {value: MemorySort; label: string}[] = [
	{value: "recent", label: "Recent"},
	{value: "importance", label: "Importance"},
	{value: "most_accessed", label: "Most Accessed"},
];

const TYPE_COLORS: Record<MemoryType, string> = {
	fact: "bg-blue-500/15 text-blue-400",
	preference: "bg-pink-500/15 text-pink-400",
	decision: "bg-amber-500/15 text-amber-400",
	identity: "bg-purple-500/15 text-purple-400",
	event: "bg-green-500/15 text-green-400",
	observation: "bg-cyan-500/15 text-cyan-400",
	goal: "bg-orange-500/15 text-orange-400",
	todo: "bg-red-500/15 text-red-400",
};

function TypeBadge({type: memoryType}: {type: MemoryType}) {
	return (
		<span
			className={`inline-flex items-center rounded px-1.5 py-0.5 text-tiny font-medium ${TYPE_COLORS[memoryType]}`}
		>
			{memoryType}
		</span>
	);
}

function ImportanceBar({value}: {value: number}) {
	return (
		<div className="flex items-center gap-1.5">
			<div className="h-1.5 w-16 overflow-hidden rounded-full bg-app-dark-box">
				<div
					className="h-full rounded-full bg-accent/60"
					style={{width: `${Math.round(value * 100)}%`}}
				/>
			</div>
			<span className="text-tiny text-ink-faint">{value.toFixed(2)}</span>
		</div>
	);
}

interface AgentMemoriesProps {
	agentId: string;
}

export function AgentMemories({agentId}: AgentMemoriesProps) {
	const [viewMode, setViewMode] = useState<ViewMode>("list");
	const [searchQuery, setSearchQuery] = useState("");
	const [debouncedQuery, setDebouncedQuery] = useState("");
	const [sort, setSort] = useState<MemorySort>("recent");
	const [typeFilter, setTypeFilter] = useState<MemoryType | null>(null);
	const [expandedId, setExpandedId] = useState<string | null>(null);
	const [sortOpen, setSortOpen] = useState(false);

	const parentRef = useRef<HTMLDivElement>(null);

	// Debounce search input
	useEffect(() => {
		const timer = setTimeout(() => setDebouncedQuery(searchQuery), 300);
		return () => clearTimeout(timer);
	}, [searchQuery]);

	const isSearching = debouncedQuery.length > 0;

	// List query (when not searching)
	const listQuery = useQuery({
		queryKey: ["memories", agentId, sort, typeFilter],
		queryFn: () =>
			api.agentMemories(agentId, {
				limit: 200,
				sort,
				memory_type: typeFilter ?? undefined,
			}),
		enabled: !isSearching,
		staleTime: 10_000,
	});

	// Search query (when searching)
	const searchQueryResult = useQuery({
		queryKey: ["memories-search", agentId, debouncedQuery, typeFilter],
		queryFn: () =>
			api.searchMemories(agentId, debouncedQuery, {
				limit: 100,
				memory_type: typeFilter ?? undefined,
			}),
		enabled: isSearching,
		staleTime: 5_000,
	});

	// Unified data
	const memories: MemoryItem[] = isSearching
		? (searchQueryResult.data?.results.map((r) => r.memory) ?? [])
		: (listQuery.data?.memories ?? []);

	const scores: Record<string, number> | null = isSearching
		? Object.fromEntries(
				(searchQueryResult.data?.results ?? []).map((r) => [
					r.memory.id,
					r.score,
				]),
			)
		: null;

	const isLoading = isSearching
		? searchQueryResult.isLoading
		: listQuery.isLoading;
	const isError = isSearching ? searchQueryResult.isError : listQuery.isError;

	const virtualizer = useVirtualizer({
		count: memories.length,
		getScrollElement: () => parentRef.current,
		estimateSize: useCallback(
			(index: number) => {
				return expandedId === memories[index]?.id ? 200 : 48;
			},
			[expandedId, memories],
		),
		overscan: 10,
	});

	// Reset expanded when data changes
	useEffect(() => {
		setExpandedId(null);
	}, [debouncedQuery, sort, typeFilter]);

	return (
		<div className="flex h-full flex-col overflow-hidden">
			{/* Toolbar */}
			<div className="flex items-center gap-3 border-b border-app-line/50 bg-app-dark-box/20 px-6 py-3">
				{/* Search */}
				<SearchBar
					placeholder="Search memories..."
					value={searchQuery}
					onChange={setSearchQuery}
					className="flex-1"
				/>

				{/* Sort pill */}
				<Popover.Root open={sortOpen} onOpenChange={setSortOpen}>
					<Popover.Trigger asChild>
						<SelectPill>
							{SORT_OPTIONS.find((o) => o.value === sort)?.label ?? sort}
						</SelectPill>
					</Popover.Trigger>
					<Popover.Content
						align="end"
						sideOffset={8}
						className="min-w-[160px] p-2"
					>
						<OptionList>
							{SORT_OPTIONS.map((option) => (
								<OptionListItem
									key={option.value}
									selected={option.value === sort}
									onClick={() => {
										setSort(option.value);
										setSortOpen(false);
									}}
								>
									{option.label}
								</OptionListItem>
							))}
						</OptionList>
					</Popover.Content>
				</Popover.Root>

				{/* View mode toggle */}
				<CircleButtonGroup>
					<CircleButton
						icon={List}
						onClick={() => setViewMode("list")}
						variant={viewMode === "list" ? "active" : "default"}
						title="List view"
					/>
					<CircleButton
						icon={TreeStructure}
						onClick={() => setViewMode("graph")}
						variant={viewMode === "graph" ? "active" : "default"}
						title="Graph view"
					/>
				</CircleButtonGroup>
			</div>

			{/* Type filter pills */}
			<div className="flex items-center gap-1.5 border-b border-app-line/50 px-6 py-2">
				<FilterButton
					onClick={() => setTypeFilter(null)}
					active={typeFilter === null}
				>
					All
				</FilterButton>
				{MEMORY_TYPES.map((type_) => (
					<FilterButton
						key={type_}
						onClick={() => setTypeFilter(typeFilter === type_ ? null : type_)}
						active={typeFilter === type_}

					>
						{type_}
					</FilterButton>
				))}
				{memories.length > 0 && (
					<span className="ml-auto text-tiny text-ink-faint">
						{memories.length} {isSearching ? "results" : "memories"}
					</span>
				)}
			</div>

			{viewMode === "graph" ? (
				<MemoryGraph agentId={agentId} sort={sort} typeFilter={typeFilter} />
			) : (
				<>
					{/* Table header */}
					<div className="grid grid-cols-[80px_1fr_100px_120px_100px] gap-3 border-b border-app-line/50 px-6 py-2 text-tiny font-medium uppercase tracking-wider text-ink-faint">
						<span>Type</span>
						<span>{isSearching ? "Content / Score" : "Content"}</span>
						<span>Importance</span>
						<span>Source</span>
						<span>Created</span>
					</div>

					{/* Virtualized rows */}
					{isLoading ? (
						<div className="flex flex-1 items-center justify-center">
							<div className="flex items-center gap-2 text-ink-dull">
								<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
								{isSearching ? "Searching..." : "Loading memories..."}
							</div>
						</div>
					) : isError ? (
						<div className="flex flex-1 items-center justify-center">
							<p className="text-sm text-red-400">Failed to load memories</p>
						</div>
					) : memories.length === 0 ? (
						<div className="flex flex-1 items-center justify-center">
							<p className="text-sm text-ink-faint">
								{isSearching ? "No results found" : "No memories yet"}
							</p>
						</div>
					) : (
						<div ref={parentRef} className="flex-1 overflow-y-auto">
							<div
								className="relative w-full"
								style={{height: virtualizer.getTotalSize()}}
							>
								{virtualizer.getVirtualItems().map((virtualRow) => {
									const memory = memories[virtualRow.index];
									if (!memory) return null;
									const isExpanded = expandedId === memory.id;
									const score = scores?.[memory.id];

									return (
										<div
											key={memory.id}
											data-index={virtualRow.index}
											ref={virtualizer.measureElement}
											className="absolute left-0 top-0 w-full"
											style={{transform: `translateY(${virtualRow.start}px)`}}
										>
											<button
												type="button"
												onClick={() =>
													setExpandedId(isExpanded ? null : memory.id)
												}
												className="grid w-full grid-cols-[80px_1fr_100px_120px_100px] items-center gap-3 px-6 py-3 text-left transition-colors hover:bg-app-hover"
											>
												<TypeBadge type={memory.memory_type} />
												<div className="min-w-0">
													<p className="truncate text-sm text-ink-dull">
														{memory.content}
													</p>
													{score !== undefined && (
														<span className="text-tiny text-accent/70">
															score: {score.toFixed(3)}
														</span>
													)}
												</div>
												<ImportanceBar value={memory.importance} />
												<span className="truncate text-tiny text-ink-faint">
													{memory.source ?? "-"}
												</span>
												<span className="text-tiny text-ink-faint">
													{formatTimeAgo(memory.created_at)}
												</span>
											</button>

											{/* Expanded detail */}
											<AnimatePresence>
												{isExpanded && (
													<motion.div
														initial={{height: 0, opacity: 0}}
														animate={{height: "auto", opacity: 1}}
														exit={{height: 0, opacity: 0}}
														transition={{
															type: "spring",
															stiffness: 500,
															damping: 35,
														}}
														className="overflow-hidden border-t border-app-line/30 bg-app-dark-box/20 px-6"
													>
														<div className="py-4">
															<p className="whitespace-pre-wrap text-sm leading-relaxed text-ink-dull">
																{memory.content}
															</p>
															<div className="mt-3 flex flex-wrap gap-x-6 gap-y-1 text-tiny text-ink-faint">
																<span>ID: {memory.id}</span>
																<span>Accessed: {memory.access_count}x</span>
																<span>
																	Last accessed:{" "}
																	{formatTimeAgo(memory.last_accessed_at)}
																</span>
																<span>
																	Updated: {formatTimeAgo(memory.updated_at)}
																</span>
																{memory.channel_id && (
																	<span>Channel: {memory.channel_id}</span>
																)}
															</div>
														</div>
													</motion.div>
												)}
											</AnimatePresence>
										</div>
									);
								})}
							</div>
						</div>
					)}
				</>
			)}
		</div>
	);
}
