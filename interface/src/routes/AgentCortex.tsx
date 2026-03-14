import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { AnimatePresence, motion } from "framer-motion";
import {
	api,
	type CortexEvent,
	type CortexEventType,
} from "@/api/client";
import { CortexChatPanel } from "@/components/CortexChatPanel";
import { ResponsiveSplitPane } from "@/components/ResponsiveSplitPane";
import { useIsMobile } from "@/hooks/useViewport";
import { formatTimeAgo } from "@/lib/format";
import { IdeaIcon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { FilterButton, Button } from "@/ui";

const PAGE_SIZE = 50;

const EVENT_CATEGORY_COLORS: Record<string, string> = {
	bulletin_generated: "bg-blue-500/15 text-blue-400",
	bulletin_failed: "bg-red-500/15 text-red-400",
	maintenance_run: "bg-green-500/15 text-green-400",
	memory_merged: "bg-green-500/15 text-green-400",
	memory_decayed: "bg-green-500/15 text-green-400",
	memory_pruned: "bg-green-500/15 text-green-400",
	association_created: "bg-violet-500/15 text-violet-400",
	contradiction_flagged: "bg-violet-500/15 text-violet-400",
	worker_killed: "bg-amber-500/15 text-amber-400",
	branch_killed: "bg-amber-500/15 text-amber-400",
	circuit_breaker_tripped: "bg-amber-500/15 text-amber-400",
	observation_created: "bg-cyan-500/15 text-cyan-400",
	health_check: "bg-blue-500/15 text-blue-400",
};

/** Groups for the filter pills — reduces clutter vs showing all 13 types. */
const FILTER_GROUPS: { label: string; types: CortexEventType[] }[] = [
	{ label: "Bulletin", types: ["bulletin_generated", "bulletin_failed"] },
	{ label: "Maintenance", types: ["maintenance_run", "memory_merged", "memory_decayed", "memory_pruned"] },
	{ label: "Health", types: ["worker_killed", "branch_killed", "circuit_breaker_tripped", "health_check"] },
	{ label: "Consolidation", types: ["association_created", "contradiction_flagged", "observation_created"] },
];

function EventTypeBadge({ eventType }: { eventType: string }) {
	const color = EVENT_CATEGORY_COLORS[eventType] ?? "bg-app-darkBox text-ink-faint";
	const label = eventType.replace(/_/g, " ");
	return (
		<span className={`inline-flex items-center rounded px-1.5 py-0.5 text-tiny font-medium ${color}`}>
			{label}
		</span>
	);
}

function DetailsPanel({ details }: { details: Record<string, unknown> }) {
	return (
		<div className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-tiny">
			{Object.entries(details).map(([key, value]) => (
				<div key={key} className="contents">
					<span className="text-ink-faint">{key}</span>
					<span className="font-mono text-ink-dull">
						{typeof value === "object" ? JSON.stringify(value) : String(value)}
					</span>
				</div>
			))}
		</div>
	);
}

interface AgentCortexProps {
	agentId: string;
}

export function AgentCortex({ agentId }: AgentCortexProps) {
	const isMobile = useIsMobile();
	const [typeFilter, setTypeFilter] = useState<CortexEventType | null>(null);
	const [groupFilter, setGroupFilter] = useState<string | null>(null);
	const [offset, setOffset] = useState(0);
	const [expandedId, setExpandedId] = useState<string | null>(null);
	const [chatOpen, setChatOpen] = useState(!isMobile);

	// Determine actual event_type filter from group or individual selection
	// For groups, we pass no event_type and filter client-side (API only supports single type)
	const activeGroupTypes = groupFilter
		? FILTER_GROUPS.find((g) => g.label === groupFilter)?.types ?? []
		: [];
	const isGroupFiltering = activeGroupTypes.length > 0;
	const serverLimit = isGroupFiltering ? 200 : PAGE_SIZE;
	const serverOffset = isGroupFiltering ? 0 : offset;

	const { data, isLoading, isError } = useQuery({
		queryKey: ["cortex-events", agentId, typeFilter, groupFilter, serverOffset, serverLimit],
		queryFn: () =>
			api.cortexEvents(agentId, {
				limit: serverLimit,
				offset: serverOffset,
				event_type: typeFilter ?? undefined,
			}),
		staleTime: 5_000,
	});

	// Client-side filter for group selections
	let events: CortexEvent[] = data?.events ?? [];
	let total = data?.total ?? 0;

	if (isGroupFiltering) {
		events = events.filter((event) => activeGroupTypes.includes(event.event_type));
		total = events.length;
		events = events.slice(offset, offset + PAGE_SIZE);
	}

	const hasNext = offset + PAGE_SIZE < total;
	const hasPrev = offset > 0;

	const handleFilterChange = (group: string | null, type_: CortexEventType | null) => {
		setGroupFilter(group);
		setTypeFilter(type_);
		setOffset(0);
		setExpandedId(null);
	};

	const primary = (
		<div className="flex h-full flex-col overflow-hidden">
				{/* Filter bar */}
				<div className="flex flex-wrap items-center gap-1.5 border-b border-app-line/50 bg-app-darkBox/20 px-3 py-2 md:px-6">
					<button
						onClick={() => handleFilterChange(null, null)}
						className={`rounded-md px-2 py-1 text-tiny font-medium transition-colors ${
							!typeFilter && !groupFilter
								? "bg-accent/15 text-accent"
								: "text-ink-faint hover:text-ink-dull"
						}`}
					>
						All
					</button>
				{FILTER_GROUPS.map((group) => (
					<FilterButton
						key={group.label}
						onClick={() =>
							handleFilterChange(
								groupFilter === group.label ? null : group.label,
								null,
							)
						}
						active={groupFilter === group.label}
						colorClass={EVENT_CATEGORY_COLORS[group.types[0]]}
					>
						{group.label}
					</FilterButton>
				))}

					{/* Count + pagination + chat toggle */}
					<div className="flex w-full items-center justify-between gap-2 sm:ml-auto sm:w-auto sm:justify-end sm:gap-3">
						{total > 0 && (
							<span className="text-tiny text-ink-faint">
								{offset + 1}-{Math.min(offset + PAGE_SIZE, total)} of {total}
							</span>
						)}
						{(hasPrev || hasNext) && (
							<div className="flex items-center gap-1">
								<button
									onClick={() => setOffset(Math.max(0, offset - PAGE_SIZE))}
									disabled={!hasPrev}
									className="rounded px-1.5 py-0.5 text-tiny text-ink-faint transition-colors hover:text-ink-dull disabled:opacity-30"
								>
									Prev
								</button>
								<button
									onClick={() => setOffset(offset + PAGE_SIZE)}
									disabled={!hasNext}
									className="rounded px-1.5 py-0.5 text-tiny text-ink-faint transition-colors hover:text-ink-dull disabled:opacity-30"
								>
									Next
								</button>
							</div>
						)}
					<div className="flex overflow-hidden rounded-md border border-app-line bg-app-darkBox">
						<Button
							onClick={() => setChatOpen(!chatOpen)}
							variant={chatOpen ? "secondary" : "ghost"}
							size="icon"
							className={chatOpen ? "bg-app-selected text-ink" : ""}
							title="Toggle cortex chat"
						aria-label="Toggle cortex chat"
						>
							<HugeiconsIcon icon={IdeaIcon} className="h-4 w-4" />
						</Button>
					</div>
					</div>
				</div>

				{/* Event list */}
				{isLoading ? (
					<div className="flex flex-1 items-center justify-center">
						<div className="flex items-center gap-2 text-ink-dull">
							<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
							Loading cortex events...
						</div>
					</div>
				) : isError ? (
					<div className="flex flex-1 items-center justify-center">
						<p className="text-sm text-red-400">Failed to load cortex events</p>
					</div>
				) : events.length === 0 ? (
					<div className="flex flex-1 items-center justify-center">
						<p className="text-sm text-ink-faint">
							{typeFilter || groupFilter ? "No events match this filter" : "No cortex events yet"}
						</p>
					</div>
				) : (
					<div className="flex-1 overflow-y-auto">
						<div className="flex flex-col">
							{events.map((event) => {
								const isExpanded = expandedId === event.id;
								return (
									<div key={event.id} className="border-b border-app-line/30">
										<button
											onClick={() => setExpandedId(isExpanded ? null : event.id)}
											className="flex w-full items-center gap-4 px-6 py-3 text-left transition-colors hover:bg-app-darkBox/30"
										>
											<span className="w-20 flex-shrink-0 text-tiny text-ink-faint">
												{formatTimeAgo(event.created_at)}
											</span>
											<EventTypeBadge eventType={event.event_type} />
											<span className="min-w-0 flex-1 truncate text-sm text-ink-dull">
												{event.summary}
											</span>
											{event.details && (
												<span className="flex-shrink-0 text-tiny text-ink-faint">
													{isExpanded ? "v" : ">"}
												</span>
											)}
										</button>

										<AnimatePresence>
											{isExpanded && event.details && (
												<motion.div
													initial={{ height: 0, opacity: 0 }}
													animate={{ height: "auto", opacity: 1 }}
													exit={{ height: 0, opacity: 0 }}
													transition={{ type: "spring", stiffness: 500, damping: 35 }}
													className="overflow-hidden"
												>
													<div className="border-t border-app-line/20 bg-app-darkBox/20 px-6 py-4">
														<DetailsPanel details={event.details} />
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
		</div>
	);

	const secondary = (
		<div className="h-full">
			<CortexChatPanel agentId={agentId} onClose={() => setChatOpen(false)} />
		</div>
	);

	return (
		<ResponsiveSplitPane
			primary={primary}
			secondary={secondary}
			showSecondary={chatOpen}
			onCloseSecondary={() => setChatOpen(false)}
			secondaryTitle="Cortex Chat"
			secondaryWidthClassName="w-[min(400px,40%)]"
		/>
	);
}
