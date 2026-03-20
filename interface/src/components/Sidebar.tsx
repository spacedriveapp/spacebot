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
import { DashboardSquare01Icon, Settings01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { CreateAgentDialog } from "@/components/CreateAgentDialog";
import { ProfileAvatar } from "@/components/ProfileAvatar";

interface SidebarProps {
	liveStates: Record<string, ChannelLiveState>;
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
		cursor: isDragging ? 'grabbing' : 'grab',
	};

	return (
		<div ref={setNodeRef} style={style} {...attributes} {...listeners}>
			<Link
				to="/agents/$agentId"
				params={{ agentId }}
				className="flex h-8 w-8 items-center justify-center"
				style={{ pointerEvents: isDragging ? 'none' : 'auto' }}
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

export function Sidebar({ liveStates: _liveStates }: SidebarProps) {
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
	const isOrchestrate = matchRoute({ to: "/orchestrate" });

	const sensors = useSensors(
		useSensor(PointerSensor, {
			activationConstraint: {
				delay: 150,
				tolerance: 5,
			},
		}),
		useSensor(KeyboardSensor, {
			coordinateGetter: sortableKeyboardCoordinates,
		})
	);

	const handleDragEnd = (event: DragEndEvent) => {
		const { active, over } = event;
		if (over && active.id !== over.id) {
			const oldIndex = agentOrder.indexOf(active.id as string);
			const newIndex = agentOrder.indexOf(over.id as string);
			setAgentOrder(arrayMove(agentOrder, oldIndex, newIndex));
		}
	};

	return (
		<nav className="flex w-14 shrink-0 flex-col items-center overflow-hidden border-r border-sidebar-line bg-sidebar">
			{/* Icon nav */}
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
					to="/orchestrate"
					className={`flex h-8 w-8 items-center justify-center rounded-md ${
						isOrchestrate ? "bg-sidebar-selected text-sidebar-ink" : "text-sidebar-inkDull hover:bg-sidebar-selected/50"
					}`}
					title="Orchestrate"
				>
					<svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
						<rect x="1" y="2" width="4" height="12" rx="1" />
						<rect x="6" y="2" width="4" height="12" rx="1" />
						<rect x="11" y="2" width="4" height="12" rx="1" />
					</svg>
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
