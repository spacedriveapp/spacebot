import {useMemo, useState} from "react";
import {
	Link,
	useMatchRoute,
	useNavigate,
	useLocation,
} from "@tanstack/react-router";
import {useQuery} from "@tanstack/react-query";
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
import {CSS} from "@dnd-kit/utilities";
import {api, getApiBase} from "@/api/client";
import type {ChannelLiveState} from "@/hooks/useChannelLiveState";
import {useAgentOrder} from "@/hooks/useAgentOrder";
import FolderPng from "@spacedrive/icons/icons/Folder.png";

/** Wraps the Spacedrive brand folder PNG so it can be passed as `icon` to
 * CircleButton (which expects a React component, not an image source). */
function FilesIcon({className}: {className?: string}) {
	return <img src={FolderPng} alt="" className={className} />;
}
import {
	House,
	TreeStructure,
	Wrench,
	CheckSquare,
	GearSix,
	DotsThree,
	ChatCircleDots,
	Broadcast,
	Brain,
	Lightning,
	CalendarDots,
	SlidersHorizontal,
	BookBookmark,
} from "@phosphor-icons/react";
import {
	CircleButton,
	SelectPill,
	Popover,
	OptionList,
	OptionListItem,
	DialogRoot,
	DialogContent,
	DialogHeader,
	DialogTitle,
	DialogDescription,
	DialogFooter,
	Button,
} from "@spacedrive/primitives";
import {CreateAgentDialog} from "@/components/CreateAgentDialog";
import {ProfileAvatar} from "@/components/ProfileAvatar";
import {WorkersPanelButton} from "@/components/WorkersPanel";
import {IS_DESKTOP, IS_MACOS} from "@/platform";

interface SidebarProps {
	liveStates: Record<string, ChannelLiveState>;
}

const agentSubItems = [
	{path: "chat", icon: ChatCircleDots, label: "Chat"},
	{path: "channels", icon: Broadcast, label: "Channels"},
	{path: "memories", icon: Brain, label: "Memory"},
	{path: "skills", icon: Lightning, label: "Skills"},
	{path: "cron", icon: CalendarDots, label: "Schedule"},
	{path: "config", icon: SlidersHorizontal, label: "Config"},
] as const;

interface SortableAgentItemProps {
	agentId: string;
	displayName?: string | null | undefined;
	gradientStart?: string | null | undefined;
	gradientEnd?: string | null | undefined;
	isActive: boolean;
	isExpanded: boolean;
	onToggle: () => void;
}

function SortableAgentItem({
	agentId,
	displayName,
	gradientStart,
	gradientEnd,
	isActive,
	isExpanded,
	onToggle,
}: SortableAgentItemProps) {
	const {attributes, listeners, setNodeRef, transform, transition, isDragging} =
		useSortable({id: agentId});

	const matchRoute = useMatchRoute();
	const isAgentRoot = !!matchRoute({to: "/agents/$agentId", params: {agentId}});

	const style = {
		transform: CSS.Transform.toString(transform),
		transition,
		opacity: isDragging ? 0.5 : 1,
		cursor: isDragging ? "grabbing" : "grab",
	};

	const name = displayName ?? agentId;

	return (
		<div ref={setNodeRef} style={style}>
			<Link
				to="/agents/$agentId"
				params={{agentId}}
				onClick={() => {
					onToggle();
				}}
				className={`flex w-full items-center gap-2.5 rounded-lg px-2 py-1.5 text-left transition-colors ${
					isAgentRoot
						? "bg-sidebar-selected/40 text-sidebar-ink"
						: isActive
							? "text-sidebar-ink"
							: "text-sidebar-inkDull hover:bg-sidebar-selected/20 hover:text-sidebar-ink"
				}`}
				style={{pointerEvents: isDragging ? "none" : "auto"}}
				{...attributes}
				{...listeners}
			>
				<ProfileAvatar
					seed={agentId}
					name={name}
					size={22}
					className="shrink-0 rounded-full"
					gradientStart={gradientStart}
					gradientEnd={gradientEnd}
				/>
				<span className="truncate text-sm font-medium">{name}</span>
			</Link>
			{isExpanded && (
				<div className="mt-0.5 mb-1 space-y-0.5 pl-4">
					{agentSubItems.map((item) => {
						const subActive = !!matchRoute({
							to: `/agents/$agentId/${item.path}`,
							params: {agentId},
							fuzzy: true,
						});
						const Icon = item.icon;
						return (
							<Link
								key={item.path}
								to={`/agents/$agentId/${item.path}`}
								params={{agentId}}
								className={`flex w-full items-center gap-2.5 rounded-lg px-2 py-1.5 text-left text-sm font-medium tracking-wide transition-colors ${
									subActive
										? "bg-sidebar-selected/40 text-sidebar-ink"
										: "text-sidebar-inkDull hover:bg-sidebar-selected/20 hover:text-sidebar-ink"
								}`}
							>
								<div className="flex size-[22px] shrink-0 items-center justify-center">
									<Icon className="size-4" weight="bold" />
								</div>
								<span>{item.label}</span>
							</Link>
						);
					})}
				</div>
			)}
		</div>
	);
}

const navItems = [
	{to: "/dashboard", icon: House, label: "Dashboard", exact: true},
	{to: "/", icon: TreeStructure, label: "Org Chart", exact: true},
	{to: "/workbench", icon: Wrench, label: "Workbench", exact: true},
	{to: "/tasks", icon: CheckSquare, label: "Tasks", exact: true},
	{to: "/wiki", icon: BookBookmark, label: "Wiki", exact: true},
] as const;

export function Sidebar({liveStates: _liveStates}: SidebarProps) {
	const navigate = useNavigate();
	const [createOpen, setCreateOpen] = useState(false);
	const [expandedAgent, setExpandedAgent] = useState<string | null>(null);
	const [hasDefaulted, setHasDefaulted] = useState(false);
	const [switcherOpen, setSwitcherOpen] = useState(false);
	const [comingSoonOpen, setComingSoonOpen] = useState(false);
	const [scrollTop, setScrollTop] = useState(0);

	const {data: globalSettings} = useQuery({
		queryKey: ["global-settings"],
		queryFn: api.globalSettings,
		staleTime: 10_000,
	});

	const companyName = globalSettings?.company_name ?? "My Company";

	const {data: agentsData} = useQuery({
		queryKey: ["agents"],
		queryFn: api.agents,
		refetchInterval: 30_000,
	});

	const {data: providersData} = useQuery({
		queryKey: ["providers"],
		queryFn: api.providers,
		staleTime: 10_000,
	});

	const {data: projectsData} = useQuery({
		queryKey: ["projects"],
		queryFn: () => api.listProjects("active"),
		staleTime: 30_000,
	});

	const hasProvider = providersData?.has_any ?? false;

	const agents = agentsData?.agents ?? [];
	const projects = projectsData?.projects ?? [];

	const agentIds = useMemo(() => agents.map((a) => a.id), [agents]);
	const agentDisplayNames = useMemo(() => {
		const map: Record<string, string | null | undefined> = {};
		for (const a of agents) map[a.id] = a.display_name;
		return map;
	}, [agents]);
	const agentGradients = useMemo(() => {
		const map: Record<
			string,
			{start?: string | null | undefined; end?: string | null | undefined}
		> = {};
		for (const a of agents)
			map[a.id] = {start: a.gradient_start, end: a.gradient_end};
		return map;
	}, [agents]);
	const [agentOrder, setAgentOrder] = useAgentOrder(agentIds);

	// Default-expand the first agent once loaded
	if (!hasDefaulted && agentOrder.length > 0) {
		setExpandedAgent(agentOrder[0]);
		setHasDefaulted(true);
	}

	const matchRoute = useMatchRoute();
	const location = useLocation();
	const activeProjectId =
		location.pathname === "/projects" &&
		typeof (location.search as Record<string, unknown>)?.id === "string"
			? ((location.search as Record<string, unknown>).id as string)
			: null;

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
		const {active, over} = event;
		if (over && active.id !== over.id) {
			const oldIndex = agentOrder.indexOf(active.id as string);
			const newIndex = agentOrder.indexOf(over.id as string);
			setAgentOrder(arrayMove(agentOrder, oldIndex, newIndex));
		}
	};

	return (
		<aside className="flex w-[220px] shrink-0 flex-col bg-sidebar">
			{/* Company switcher */}
			<div className={`px-3 ${IS_DESKTOP && IS_MACOS ? "pt-[50px]" : "pt-3"}`}>
				<Popover.Root open={switcherOpen} onOpenChange={setSwitcherOpen}>
					<Popover.Trigger asChild>
						<SelectPill variant="sidebar" size="md" className="w-full">
							<span className="font-semibold">{companyName}</span>
						</SelectPill>
					</Popover.Trigger>
					<Popover.Content
						align="start"
						sideOffset={8}
						className="min-w-[200px] p-2"
					>
						<OptionList>
							<OptionListItem selected>{companyName}</OptionListItem>
						</OptionList>
						<div className="my-2 h-px bg-app-line" />
						<OptionList>
							<OptionListItem
								onClick={() => {
									setSwitcherOpen(false);
									setComingSoonOpen(true);
								}}
							>
								<span>Add Instance</span>
							</OptionListItem>
							<OptionListItem
								onClick={() => {
									setSwitcherOpen(false);
									navigate({to: "/settings", search: {tab: "instance"}});
								}}
							>
								<span>Edit</span>
							</OptionListItem>
						</OptionList>
					</Popover.Content>
				</Popover.Root>
			</div>

			{/* Scrollable area: primary nav + sections */}
			<div className="relative flex-1 overflow-hidden">
				<div className={`pointer-events-none absolute inset-x-0 top-0 z-10 h-6 bg-gradient-to-b from-sidebar to-transparent transition-opacity duration-150 ${scrollTop > 0 ? "opacity-100" : "opacity-0"}`} />
			<div className="h-full overflow-y-auto px-3 pb-4" onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}>
			{/* Primary nav */}
			<nav className="space-y-0.5 py-3">
				{navItems.map((item) => {
					const Icon = item.icon;
					const isActive = item.exact
						? !!matchRoute({to: item.to})
						: !!matchRoute({to: item.to, fuzzy: true});
					return (
						<Link
							key={item.label}
							to={item.to}
							className={`flex w-full items-center gap-2.5 rounded-lg px-2 py-1.5 text-left text-sm font-medium tracking-wide outline-none ring-inset ring-transparent transition-colors focus:ring-1 focus:ring-accent ${
								isActive
									? "bg-sidebar-selected/40 text-sidebar-ink"
									: "text-sidebar-inkDull hover:bg-sidebar-selected/20 hover:text-sidebar-ink"
							}`}
						>
							<div className="flex size-[22px] shrink-0 items-center justify-center">
								<Icon className="size-4" weight="bold" />
							</div>
							<span className="truncate">{item.label}</span>
						</Link>
					);
				})}
			</nav>
				{/* Projects section */}
				{projects.length > 0 && (
					<section className="mb-4">
						<div className="mb-2 flex items-center justify-between px-2">
							<div className="text-sidebar-inkDull text-[11px] font-semibold uppercase tracking-[0.16em]">
								Projects
							</div>
							<Link to="/projects">
								<CircleButton icon={DotsThree} size="sm" title="All Projects" />
							</Link>
						</div>
						<div className="space-y-0.5">
							{projects.slice(0, 5).map((project) => {
								const logoUrl = project.logo_path
									? `${getApiBase()}/agents/projects/${encodeURIComponent(project.id)}/logo`
									: null;
								const fallback =
									project.icon || project.name.slice(0, 1).toUpperCase();
								return (
									<Link
										key={project.id}
										to="/projects"
										search={{id: project.id}}
										className={`flex w-full items-center gap-2.5 rounded-lg px-2 py-1.5 text-left transition-colors ${
											activeProjectId === project.id
												? "bg-sidebar-selected/40 text-sidebar-ink"
												: "text-sidebar-inkDull hover:bg-sidebar-selected/20 hover:text-sidebar-ink"
										}`}
									>
										{logoUrl ? (
											<img
												src={logoUrl}
												alt=""
												className="size-[22px] shrink-0 rounded-md object-contain"
												draggable={false}
											/>
										) : (
											<div className="flex size-[22px] shrink-0 items-center justify-center rounded-md bg-sidebar-selected/40 text-[10px] font-semibold text-sidebar-ink">
												{fallback}
											</div>
										)}
										<div className="min-w-0">
											<div className="text-sidebar-ink truncate text-sm font-medium">
												{project.name}
											</div>
											{project.description && (
												<div className="text-ink-faint truncate text-[11px]">
													{project.description}
												</div>
											)}
										</div>
									</Link>
								);
							})}
							{projects.length > 5 && (
								<Link
									to="/projects"
									className="flex w-full items-center px-2 py-1"
								>
									<span className="text-[11px] text-ink-faint">
										{projects.length - 5} more project{projects.length - 5 !== 1 ? "s" : ""}
									</span>
								</Link>
							)}
						</div>
					</section>
				)}

				{/* Agents section */}
				<section>
					<div className="mb-2 flex items-center justify-between px-2">
						<div className="text-sidebar-inkDull text-[11px] font-semibold uppercase tracking-[0.16em]">
							Agents
						</div>
						{hasProvider && (
							<CircleButton
								icon={DotsThree}
								size="sm"
								onClick={() => setCreateOpen(true)}
								title="New Agent"
							/>
						)}
					</div>
					<div className="space-y-0.5">
						<DndContext
							sensors={sensors}
							collisionDetection={closestCenter}
							onDragEnd={handleDragEnd}
						>
							<SortableContext
								items={agentOrder}
								strategy={verticalListSortingStrategy}
							>
								{agentOrder.map((agentId) => {
									const isActive = !!matchRoute({
										to: "/agents/$agentId",
										params: {agentId},
										fuzzy: true,
									});
									return (
										<SortableAgentItem
											key={agentId}
											agentId={agentId}
											displayName={agentDisplayNames[agentId]}
											gradientStart={agentGradients[agentId]?.start}
											gradientEnd={agentGradients[agentId]?.end}
											isActive={isActive}
											isExpanded={expandedAgent === agentId}
											onToggle={() => setExpandedAgent(agentId)}
										/>
									);
								})}
							</SortableContext>
						</DndContext>
					</div>
				</section>
			</div>
			</div>

			{/* Footer */}
			<div className="flex shrink-0 items-center justify-between border-t border-app-line/30 px-3 py-2">
				<div className="flex items-center gap-2">
					<WorkersPanelButton />
					{globalSettings?.spacedrive?.enabled && (
						<Link to="/spacedrive">
							<CircleButton
								icon={FilesIcon}
								title="Files"
								variant={
									!!matchRoute({to: "/spacedrive", fuzzy: true})
										? "active"
										: "default"
								}
							/>
						</Link>
					)}
				</div>
				<Link to="/settings">
					<CircleButton
						icon={GearSix}
						title="Settings"
						variant={
							!!matchRoute({to: "/settings", fuzzy: true})
								? "active"
								: "default"
						}
					/>
				</Link>
			</div>

			{agents[0] && (
				<CreateAgentDialog
					open={createOpen}
					onOpenChange={setCreateOpen}
					agentId={agents[0].id}
				/>
			)}

			<DialogRoot open={comingSoonOpen} onOpenChange={setComingSoonOpen}>
				<DialogContent className="max-w-sm">
					<DialogHeader>
						<DialogTitle>Add Instance</DialogTitle>
						<DialogDescription>
							Multi-instance support is coming soon. You'll be able to connect
							to additional Spacebot instances from here.
						</DialogDescription>
					</DialogHeader>
					<DialogFooter>
						<Button onClick={() => setComingSoonOpen(false)} size="md">
							Close
						</Button>
					</DialogFooter>
				</DialogContent>
			</DialogRoot>
		</aside>
	);
}
