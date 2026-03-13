import { useMemo, useState } from "react";
import { Link, useMatchRoute } from "@tanstack/react-router";
import { useQuery } from "@tanstack/react-query";
import {
	DndContext,
	closestCenter,
	KeyboardSensor,
	PointerSensor,
	useSensor,
	useSensors,
	type DragEndEvent,
} from "@dnd-kit/core";
import {
	arrayMove,
	SortableContext,
	sortableKeyboardCoordinates,
	useSortable,
	verticalListSortingStrategy,
} from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { api } from "@/api/client";
import type { ChannelLiveState } from "@/hooks/useChannelLiveState";
import { useAgentOrder } from "@/hooks/useAgentOrder";
import { DashboardSquare01Icon, Settings01Icon, Cancel01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { CreateAgentDialog } from "@/components/CreateAgentDialog";
import { ProfileAvatar } from "@/components/ProfileAvatar";

interface SidebarProps {
	liveStates: Record<string, ChannelLiveState>;
	isMobile?: boolean;
	mobileOpen?: boolean;
	onCloseMobile?: () => void;
}

interface SortableAgentItemProps {
	agentId: string;
	displayName?: string;
	gradientStart?: string;
	gradientEnd?: string;
	isActive: boolean;
}

function SortableAgentItem({ agentId, displayName, gradientStart, gradientEnd, isActive }: SortableAgentItemProps) {
	const {
		attributes,
		listeners,
		setNodeRef,
		transform,
		transition,
		isDragging,
	} = useSortable({ id: agentId });

	const style = {
		transform: CSS.Transform.toString(transform),
		transition,
		opacity: isDragging ? 0.5 : 1,
		cursor: isDragging ? "grabbing" : "grab",
	};

	return (
		<div ref={setNodeRef} style={style} {...attributes} {...listeners}>
			<Link
				to="/agents/$agentId"
				params={{ agentId }}
				className="flex h-8 w-8 items-center justify-center"
				style={{ pointerEvents: isDragging ? "none" : "auto" }}
				title={displayName ?? agentId}
			>
				<ProfileAvatar
					seed={agentId}
					name={displayName ?? agentId}
					size={28}
					className={`rounded-full ${isActive ? "ring-2 ring-sidebar-line" : "opacity-70 hover:opacity-100"}`}
					gradientStart={gradientStart}
					gradientEnd={gradientEnd}
				/>
			</Link>
		</div>
	);
}

export function Sidebar({ liveStates: _liveStates, isMobile = false, mobileOpen = false, onCloseMobile }: SidebarProps) {
	const [createOpen, setCreateOpen] = useState(false);

	const { data: agentsData } = useQuery({
		queryKey: ["agents"],
		queryFn: api.agents,
		refetchInterval: 30_000,
	});

	const { data: providersData } = useQuery({
		queryKey: ["providers"],
		queryFn: api.providers,
		staleTime: 10_000,
	});

	const hasProvider = providersData?.has_any ?? false;
	const agents = agentsData?.agents ?? [];
	const agentIds = useMemo(() => agents.map((a) => a.id), [agents]);
	const agentDisplayNames = useMemo(() => {
		const map: Record<string, string | undefined> = {};
		for (const a of agents) map[a.id] = a.display_name;
		return map;
	}, [agents]);
	const agentGradients = useMemo(() => {
		const map: Record<string, { start?: string; end?: string }> = {};
		for (const a of agents) map[a.id] = { start: a.gradient_start, end: a.gradient_end };
		return map;
	}, [agents]);
	const [agentOrder, setAgentOrder] = useAgentOrder(agentIds);

	const matchRoute = useMatchRoute();
	const isOverview = matchRoute({ to: "/" });
	const isSettings = matchRoute({ to: "/settings" });

	const sensors = useSensors(
		useSensor(PointerSensor, {
			activationConstraint: {
				delay: 150,
				tolerance: 5,
			},
		}),
		useSensor(KeyboardSensor, {
			coordinateGetter: sortableKeyboardCoordinates,
		}),
	);

	const handleDragEnd = (event: DragEndEvent) => {
		const { active, over } = event;
		if (over && active.id !== over.id) {
			const oldIndex = agentOrder.indexOf(active.id as string);
			const newIndex = agentOrder.indexOf(over.id as string);
			setAgentOrder(arrayMove(agentOrder, oldIndex, newIndex));
		}
	};

	if (isMobile) {
		if (!mobileOpen) return null;
		return (
			<>
				<div className="fixed inset-0 z-40 bg-black/40" onClick={onCloseMobile} />
				<nav className="fixed inset-y-0 left-0 z-50 flex w-72 flex-col border-r border-sidebar-line bg-sidebar">
					<div className="flex h-12 items-center justify-between border-b border-sidebar-line px-3">
						<span className="font-plex text-sm font-medium text-sidebar-ink">Navigation</span>
						<button
							type="button"
							onClick={onCloseMobile}
							aria-label="Close navigation"
							className="flex h-8 w-8 items-center justify-center rounded-md text-sidebar-inkDull hover:bg-sidebar-selected/50 hover:text-sidebar-ink"
						>
							<HugeiconsIcon icon={Cancel01Icon} className="h-4 w-4" />
						</button>
					</div>
					<div className="flex flex-col gap-1 px-2 py-2">
						<Link
							to="/"
							onClick={onCloseMobile}
							className={`flex items-center gap-2 rounded-md px-3 py-2 text-sm ${isOverview ? "bg-sidebar-selected text-sidebar-ink" : "text-sidebar-inkDull hover:bg-sidebar-selected/50"}`}
						>
							<HugeiconsIcon icon={DashboardSquare01Icon} className="h-4 w-4" />
							Overview
						</Link>
						<Link
							to="/settings"
							onClick={onCloseMobile}
							className={`flex items-center gap-2 rounded-md px-3 py-2 text-sm ${isSettings ? "bg-sidebar-selected text-sidebar-ink" : "text-sidebar-inkDull hover:bg-sidebar-selected/50"}`}
						>
							<HugeiconsIcon icon={Settings01Icon} className="h-4 w-4" />
							Settings
						</Link>
					</div>
					<div className="mx-3 my-1 h-px bg-sidebar-line" />
					<div className="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
						<div className="mb-2 px-1 text-tiny uppercase tracking-wider text-sidebar-inkFaint">Agents</div>
						<div className="flex flex-col gap-1">
							{agentOrder.map((agentId) => {
								const isActive = !!matchRoute({ to: "/agents/$agentId", params: { agentId }, fuzzy: true });
								return (
									<Link
										key={agentId}
										to="/agents/$agentId"
										params={{ agentId }}
										onClick={onCloseMobile}
										className={`flex items-center gap-2 rounded-md px-2 py-1.5 ${isActive ? "bg-sidebar-selected text-sidebar-ink" : "text-sidebar-inkDull hover:bg-sidebar-selected/50"}`}
									>
										<ProfileAvatar
											seed={agentId}
											name={agentDisplayNames[agentId] ?? agentId}
											size={22}
											className="rounded-full"
											gradientStart={agentGradients[agentId]?.start}
											gradientEnd={agentGradients[agentId]?.end}
										/>
										<span className="truncate text-sm">{agentDisplayNames[agentId] ?? agentId}</span>
									</Link>
								);
							})}
						</div>
					</div>
				{hasProvider && agents[0] && (
					<div className="border-t border-sidebar-line p-2">
						<button
							onClick={() => setCreateOpen(true)}
							className="w-full rounded-md bg-sidebar-selected px-3 py-2 text-sm text-sidebar-ink hover:bg-sidebar-selected/80"
						>
							 New Agent
						</button>
					</div>
				)}
			</nav>
			{agents[0] && (
				<CreateAgentDialog open={createOpen} onOpenChange={setCreateOpen} agentId={agents[0].id} />
			)}
			</>
		);
	}

	return (
		<nav className="flex w-14 shrink-0 flex-col items-center overflow-hidden border-r border-sidebar-line bg-sidebar">
			<div className="flex flex-col items-center gap-1 pt-2">
				<Link
					to="/"
					className={`flex h-8 w-8 items-center justify-center rounded-md ${
						isOverview ? "bg-sidebar-selected text-sidebar-ink" : "text-sidebar-inkDull hover:bg-sidebar-selected/50"
					}`}
					title="Dashboard"
				>
					<HugeiconsIcon icon={DashboardSquare01Icon} className="h-4 w-4" />
				</Link>
				<Link
					to="/settings"
					className={`flex h-8 w-8 items-center justify-center rounded-md ${
						isSettings ? "bg-sidebar-selected text-sidebar-ink" : "text-sidebar-inkDull hover:bg-sidebar-selected/50"
					}`}
					title="Settings"
				>
					<HugeiconsIcon icon={Settings01Icon} className="h-4 w-4" />
				</Link>
				<div className="my-1 h-px w-5 bg-sidebar-line" />
				<DndContext
					sensors={sensors}
					collisionDetection={closestCenter}
					onDragEnd={handleDragEnd}
				>
					<SortableContext items={agentOrder} strategy={verticalListSortingStrategy}>
						{agentOrder.map((agentId) => {
							const isActive = !!matchRoute({ to: "/agents/$agentId", params: { agentId }, fuzzy: true });
							return (
								<SortableAgentItem
									key={agentId}
									agentId={agentId}
									displayName={agentDisplayNames[agentId]}
									gradientStart={agentGradients[agentId]?.start}
									gradientEnd={agentGradients[agentId]?.end}
									isActive={isActive}
								/>
							);
						})}
					</SortableContext>
				</DndContext>
				{hasProvider && (
					<button
						onClick={() => setCreateOpen(true)}
						className="flex h-8 w-8 items-center justify-center rounded-md text-sidebar-inkFaint hover:bg-sidebar-selected/50 hover:text-sidebar-inkDull"
						title="New Agent"
					>
						+
					</button>
				)}
			</div>
			{agents[0] && (
				<CreateAgentDialog open={createOpen} onOpenChange={setCreateOpen} agentId={agents[0].id} />
			)}
		</nav>
	);
}
