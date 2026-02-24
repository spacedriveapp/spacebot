import { useCallback, useMemo, useRef, useState } from "react";
import {
	ReactFlow,
	Background,
	Controls,
	type Node,
	type Edge,
	type Connection,
	type NodeTypes,
	type EdgeTypes,
	type NodeProps,
	type EdgeProps,
	type NodeChange,
	useNodesState,
	useEdgesState,
	useReactFlow,
	MarkerType,
	Handle,
	Position,
	BaseEdge,
	getSmoothStepPath,
	ReactFlowProvider,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { AnimatePresence, motion } from "framer-motion";
import {
	api,
	type AgentSummary,
	type TopologyResponse,
	type TopologyGroup,
	type LinkDirection,
	type LinkKind,
} from "@/api/client";
import { Button, Input, Dialog, DialogContent, DialogHeader, DialogTitle, DialogFooter } from "@/ui";
import { Link } from "@tanstack/react-router";

// -- Colors --

const EDGE_COLOR = "hsla(230, 10%, 35%, 0.6)";
const EDGE_COLOR_ACTIVE = "hsla(230, 10%, 55%, 0.9)";

const GROUP_COLORS = [
	"#6366f1",
	"#8b5cf6",
	"#ec4899",
	"#f59e0b",
	"#22c55e",
	"#06b6d4",
	"#f97316",
	"#14b8a6",
];

// -- Position persistence --

const POSITIONS_KEY = "spacebot:topology:positions";

type SavedPositions = Record<string, { x: number; y: number }>;

function loadPositions(): SavedPositions {
	try {
		const raw = localStorage.getItem(POSITIONS_KEY);
		if (raw) return JSON.parse(raw);
	} catch {
		// ignore
	}
	return {};
}

function savePositions(nodes: Node[]) {
	const positions: SavedPositions = {};
	for (const node of nodes) {
		positions[node.id] = node.position;
	}
	try {
		localStorage.setItem(POSITIONS_KEY, JSON.stringify(positions));
	} catch {
		// ignore
	}
}

/** Deterministic gradient from a seed string. */
function seedGradient(seed: string): [string, string] {
	let hash = 0;
	for (let i = 0; i < seed.length; i++) {
		hash = seed.charCodeAt(i) + ((hash << 5) - hash);
		hash |= 0;
	}
	const hue1 = (hash >>> 0) % 360;
	const hue2 = (hue1 + 40 + ((hash >>> 8) % 60)) % 360;
	return [`hsl(${hue1}, 70%, 55%)`, `hsl(${hue2}, 60%, 45%)`];
}

// -- Custom Node: Group --

const GROUP_PADDING = 30;
const GROUP_HEADER = 36;

function GroupNode({ data, selected }: NodeProps) {
	const color = (data.color as string) ?? "#6366f1";
	const name = data.label as string;

	return (
		<div
			className={`rounded-2xl border transition-all ${
				selected ? "border-opacity-60" : "border-opacity-30"
			}`}
			style={{
				width: data.width as number,
				height: data.height as number,
				borderColor: color,
				backgroundColor: `${color}08`,
			}}
		>
			<div
				className="flex items-center gap-2 rounded-t-2xl px-4"
				style={{
					height: GROUP_HEADER,
					background: `linear-gradient(135deg, ${color}18, ${color}08)`,
				}}
			>
				<span
					className="h-2 w-2 rounded-full"
					style={{ backgroundColor: color }}
				/>
				<span
					className="text-[11px] font-semibold uppercase tracking-wider"
					style={{ color }}
				>
					{name}
				</span>
			</div>
		</div>
	);
}

// -- Shared Profile Node (used for both agent and human) --

const NODE_WIDTH = 240;

function ProfileNode({ data, selected }: NodeProps) {
	const nodeId = (data.nodeId as string) ?? "";
	const avatarSeed = (data.avatarSeed as string) ?? nodeId;
	const [c1, c2] = seedGradient(avatarSeed);
	const configDisplayName = data.configDisplayName as string | null;
	const configRole = data.configRole as string | null;
	const chosenName = data.chosenName as string | null;
	const bio = data.bio as string | null;
	const isOnline = data.isOnline as boolean;
	const channelCount = (data.channelCount as number) ?? 0;
	const memoryCount = (data.memoryCount as number) ?? 0;
	const connected = (data.connectedHandles as Record<string, boolean>) ?? {};
	const nodeKind = (data.nodeKind as string) ?? "agent";
	const isAgent = nodeKind === "agent";
	const primaryName = configDisplayName ?? nodeId;
	const onEdit = data.onEdit as (() => void) | undefined;

	return (
		<div
			className={`group/node relative rounded-xl border bg-app-darkBox transition-all ${
				selected
					? "border-accent shadow-lg shadow-accent/20"
					: "border-app-line hover:border-app-line/80"
			}`}
			style={{ width: NODE_WIDTH }}
		>
			{/* Edit button (visible on hover) */}
			{onEdit && (
				<button
					onClick={(e) => {
						e.stopPropagation();
						onEdit();
					}}
					className="absolute top-2 right-2 z-10 rounded p-1 text-ink-faint opacity-0 transition-opacity hover:bg-ink/10 hover:text-ink group-hover/node:opacity-100"
				>
					<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
						<path d="M11.5 1.5l3 3L5 14H2v-3L11.5 1.5z" />
					</svg>
				</button>
			)}

			{/* Gradient banner */}
			<div className="relative h-12 overflow-hidden rounded-t-xl">
				<svg
					className="absolute inset-0 h-full w-full"
					preserveAspectRatio="none"
					viewBox="0 0 240 48"
				>
					<defs>
						<linearGradient
							id={`node-grad-${nodeId}`}
							x1="0%"
							y1="0%"
							x2="100%"
							y2="100%"
						>
							<stop offset="0%" stopColor={c1} stopOpacity={0.35} />
							<stop offset="100%" stopColor={c2} stopOpacity={0.2} />
						</linearGradient>
					</defs>
					<rect width="240" height="48" fill={`url(#node-grad-${nodeId})`} />
				</svg>
			</div>

			{/* Avatar + badge row */}
			<div className="relative -mt-6 px-4 pt-3 flex items-end justify-between">
				<div className="relative inline-block">
					<svg
						width={48}
						height={48}
						viewBox="0 0 64 64"
						className="rounded-full border-[3px] border-app-darkBox"
					>
						<defs>
							<linearGradient
								id={`av-${nodeId}`}
								x1="0%"
								y1="0%"
								x2="100%"
								y2="100%"
							>
								<stop offset="0%" stopColor={c1} />
								<stop offset="100%" stopColor={c2} />
							</linearGradient>
						</defs>
						<circle cx="32" cy="32" r="32" fill={`url(#av-${nodeId})`} />
					</svg>
					{isAgent && (
						<div
							className={`absolute bottom-0 right-0 h-3.5 w-3.5 rounded-full border-[2px] border-app-darkBox ${
								isOnline ? "bg-green-500" : "bg-gray-500"
							}`}
						/>
					)}
				</div>
				{/* Badge */}
				<span
					className={`mb-1 rounded px-1.5 py-0.5 text-[9px] font-bold uppercase tracking-wider ${
						isAgent
							? "bg-accent/20 text-accent"
							: "bg-ink-faint/10 text-ink-faint"
					}`}
				>
					{nodeKind}
				</span>
			</div>

			{/* Profile content */}
			<div className="px-4 pt-1.5 pb-3">
				{/* Primary name + self-chosen name inline */}
				{isAgent ? (
					<Link
						to="/agents/$agentId"
						params={{ agentId: nodeId }}
						className="flex items-baseline gap-1.5 truncate"
						onClick={(e) => e.stopPropagation()}
					>
						<span className="font-plex text-sm font-semibold text-ink hover:text-accent transition-colors truncate">
							{primaryName}
						</span>
						{chosenName && chosenName !== primaryName && chosenName !== nodeId && (
							<span className="text-[11px] text-ink-faint truncate">
								"{chosenName}"
							</span>
						)}
					</Link>
				) : (
					<div className="flex items-baseline gap-1.5 truncate">
						<span className="font-plex text-sm font-semibold text-ink truncate">
							{primaryName}
						</span>
					</div>
				)}

				{/* Role */}
				{configRole && (
					<p className="mt-0.5 text-[11px] text-ink-dull truncate">
						{configRole}
					</p>
				)}

				{/* Bio */}
				{bio && (
					<p className="mt-2 text-[11px] leading-relaxed text-ink-faint line-clamp-3">
						{bio}
					</p>
				)}

				{/* Stats (agents only) */}
				{isAgent && (
					<div className="mt-2 flex items-center gap-3 border-t border-app-line/40 pt-2 text-[10px] text-ink-faint">
						<span>
							<span className="font-medium text-ink-dull">{channelCount}</span> channels
						</span>
						<span>
							<span className="font-medium text-ink-dull">
								{memoryCount >= 1000 ? `${(memoryCount / 1000).toFixed(1)}k` : memoryCount}
							</span> memories
						</span>
					</div>
				)}
			</div>

			{/* Handles on all four sides */}
			{(["top", "bottom", "left", "right"] as const).map((side) => (
				<Handle key={`src-${side}`} type="source" id={side}
					position={side === "top" ? Position.Top : side === "bottom" ? Position.Bottom : side === "left" ? Position.Left : Position.Right}
					className={`!h-2.5 !w-2.5 !border-2 !border-app-darkBox ${connected[side] ? "!bg-accent" : "!bg-app-line"}`} />
			))}
			{(["top", "bottom", "left", "right"] as const).map((side) => (
				<Handle key={`tgt-${side}`} type="target" id={side}
					position={side === "top" ? Position.Top : side === "bottom" ? Position.Bottom : side === "left" ? Position.Left : Position.Right}
					className={`!h-2.5 !w-2.5 !border-2 !border-app-darkBox ${connected[side] ? "!bg-accent" : "!bg-app-line"}`} />
			))}
		</div>
	);
}

// -- Custom Edge --

function LinkEdge({
	id,
	sourceX,
	sourceY,
	targetX,
	targetY,
	sourcePosition,
	targetPosition,
	data,
	selected,
}: EdgeProps) {
	const [edgePath] = getSmoothStepPath({
		sourceX,
		sourceY,
		sourcePosition,
		targetX,
		targetY,
		targetPosition,
		borderRadius: 16,
	});

	const isActive = data?.active as boolean;
	const color = isActive ? EDGE_COLOR_ACTIVE : EDGE_COLOR;

	return (
		<>
			<BaseEdge
				id={id}
				path={edgePath}
				style={{
					stroke: selected ? "hsl(var(--color-accent))" : color,
					strokeWidth: selected ? 2.5 : isActive ? 2 : 1.5,
					opacity: selected ? 1 : isActive ? 0.9 : 0.4,
				}}
			/>
			{/* Animated dot traveling along the edge during active message traffic */}
			{isActive && (
				<circle r="3" fill={EDGE_COLOR_ACTIVE}>
					<animateMotion dur="2s" repeatCount="indefinite" path={edgePath} />
				</circle>
			)}
		</>
	);
}

/** Pick source/target handle IDs based on link kind and node positions. */
function getHandlesForKind(
	kind: string,
	sourcePos?: { x: number; y: number },
	targetPos?: { x: number; y: number },
): {
	sourceHandle: string;
	targetHandle: string;
} {
	if (kind === "hierarchical") {
		// from is above to: connect bottom of superior to top of subordinate
		return { sourceHandle: "bottom", targetHandle: "top" };
	}
	// Peer: pick the side facing the other node
	if (sourcePos && targetPos) {
		const dx = targetPos.x - sourcePos.x;
		const dy = targetPos.y - sourcePos.y;
		if (Math.abs(dy) > Math.abs(dx)) {
			return dy > 0
				? { sourceHandle: "bottom", targetHandle: "top" }
				: { sourceHandle: "top", targetHandle: "bottom" };
		}
		return dx > 0
			? { sourceHandle: "right", targetHandle: "left" }
			: { sourceHandle: "left", targetHandle: "right" };
	}
	return { sourceHandle: "right", targetHandle: "left" };
}

/** Infer link kind from the handle the user dragged from. */
function inferKindFromHandle(sourceHandle: string | null): LinkKind {
	switch (sourceHandle) {
		case "top":
		case "bottom":
			return "hierarchical";
		default:
			return "peer";
	}
}

const nodeTypes: NodeTypes = {
	agent: ProfileNode,
	human: ProfileNode,
	group: GroupNode,
};

const edgeTypes: EdgeTypes = {
	link: LinkEdge,
};

// -- Edge Config Panel --

interface EdgeConfigPanelProps {
	edge: Edge;
	onUpdate: (direction: LinkDirection, kind: LinkKind) => void;
	onDelete: () => void;
	onClose: () => void;
}

function EdgeConfigPanel({
	edge,
	onUpdate,
	onDelete,
	onClose,
}: EdgeConfigPanelProps) {
	const [direction, setDirection] = useState<LinkDirection>(
		(edge.data?.direction as LinkDirection) ?? "two_way",
	);
	const [kind, setKind] = useState<LinkKind>(
		(edge.data?.kind as LinkKind) ?? "peer",
	);

	return (
		<motion.div
			initial={{ opacity: 0, y: 8 }}
			animate={{ opacity: 1, y: 0 }}
			exit={{ opacity: 0, y: 8 }}
			transition={{ duration: 0.15 }}
			className="absolute right-4 top-4 z-20 w-64 rounded-lg border border-app-line/50 bg-app-darkBox/95 p-4 shadow-xl backdrop-blur-sm"
		>
			<div className="mb-3 flex items-center justify-between">
				<span className="text-sm font-medium text-ink">Link Settings</span>
				<button
					onClick={onClose}
					className="text-ink-faint hover:text-ink transition-colors text-sm"
				>
					Close
				</button>
			</div>

			<div className="mb-2 text-tiny text-ink-faint">
				{edge.source} → {edge.target}
			</div>

			{/* Kind */}
			<div className="mb-3">
				<label className="mb-1 block text-tiny font-medium text-ink-dull">Kind</label>
				<div className="flex gap-1.5">
					{(["hierarchical", "peer"] as const).map((k) => (
						<button
							key={k}
							onClick={() => setKind(k)}
							className={`flex-1 rounded px-2 py-1.5 text-tiny transition-colors capitalize ${
								kind === k
									? "bg-ink/10 text-ink"
									: "bg-app-box/50 text-ink-faint hover:text-ink-dull"
							}`}
						>
							{k}
						</button>
					))}
				</div>
			</div>

			{/* Direction */}
			<div className="mb-4">
				<label className="mb-1 block text-tiny font-medium text-ink-dull">Direction</label>
				<div className="flex gap-1.5">
					{(["one_way", "two_way"] as const).map((d) => (
						<button
							key={d}
							onClick={() => setDirection(d)}
							className={`flex-1 rounded px-2 py-1.5 text-tiny transition-colors ${
								direction === d
									? "bg-ink/10 text-ink"
									: "bg-app-box/50 text-ink-faint hover:text-ink-dull"
							}`}
						>
							{d === "one_way" ? "One Way" : "Two Way"}
						</button>
					))}
				</div>
			</div>

			<div className="flex gap-2">
				<Button
					onClick={() => onUpdate(direction, kind)}
					size="sm"
					className="flex-1"
				>
					Save
				</Button>
				<Button
					onClick={onDelete}
					size="sm"
					variant="outline"
					className="text-tiny text-ink-faint"
				>
					Delete
				</Button>
			</div>
		</motion.div>
	);
}

// -- Human Edit Dialog --

interface HumanEditDialogProps {
	human: { id: string; display_name?: string; role?: string; bio?: string } | null;
	open: boolean;
	onOpenChange: (open: boolean) => void;
	onUpdate: (displayName: string, role: string, bio: string) => void;
	onDelete: () => void;
}

function HumanEditDialog({
	human,
	open,
	onOpenChange,
	onUpdate,
	onDelete,
}: HumanEditDialogProps) {
	const [displayName, setDisplayName] = useState("");
	const [role, setRole] = useState("");
	const [bio, setBio] = useState("");

	// Sync state when a different human is selected
	const prevId = useRef<string | null>(null);
	if (human && human.id !== prevId.current) {
		prevId.current = human.id;
		setDisplayName(human.display_name ?? "");
		setRole(human.role ?? "");
		setBio(human.bio ?? "");
	}

	if (!human) return null;

	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent className="max-w-sm">
				<DialogHeader>
					<DialogTitle>Edit Human</DialogTitle>
				</DialogHeader>
				<div className="flex flex-col gap-3">
					<div className="text-tiny text-ink-faint">{human.id}</div>
					<div>
						<label className="mb-1.5 block text-sm font-medium text-ink-dull">Display Name</label>
						<Input
							size="lg"
							value={displayName}
							onChange={(e) => setDisplayName(e.target.value)}
							placeholder={human.id}
						/>
					</div>
					<div>
						<label className="mb-1.5 block text-sm font-medium text-ink-dull">Role</label>
						<Input
							size="lg"
							value={role}
							onChange={(e) => setRole(e.target.value)}
							placeholder="e.g. CEO, Lead Developer"
						/>
					</div>
					<div>
						<label className="mb-1.5 block text-sm font-medium text-ink-dull">Bio</label>
						<textarea
							value={bio}
							onChange={(e) => setBio(e.target.value)}
							placeholder="A short description..."
							rows={3}
							className="w-full rounded-md bg-app-input px-3 py-2 text-sm text-ink outline-none border border-app-line/50 focus:border-accent/50 resize-none"
						/>
					</div>
				</div>
				<DialogFooter>
					<Button variant="destructive" size="sm" onClick={onDelete}>
						Delete
					</Button>
					<div className="flex-1" />
					<Button variant="ghost" size="sm" onClick={() => onOpenChange(false)}>
						Cancel
					</Button>
					<Button size="sm" onClick={() => onUpdate(displayName, role, bio)}>
						Save
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}

// -- Agent Edit Dialog --

interface AgentEditDialogProps {
	agent: { id: string; display_name?: string; role?: string } | null;
	open: boolean;
	onOpenChange: (open: boolean) => void;
	onUpdate: (displayName: string, role: string) => void;
}

function AgentEditDialog({
	agent,
	open,
	onOpenChange,
	onUpdate,
}: AgentEditDialogProps) {
	const [displayName, setDisplayName] = useState("");
	const [role, setRole] = useState("");

	const prevId = useRef<string | null>(null);
	if (agent && agent.id !== prevId.current) {
		prevId.current = agent.id;
		setDisplayName(agent.display_name ?? "");
		setRole(agent.role ?? "");
	}

	if (!agent) return null;

	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent className="max-w-sm">
				<DialogHeader>
					<DialogTitle>Edit Agent</DialogTitle>
				</DialogHeader>
				<div className="flex flex-col gap-3">
					<div className="text-tiny text-ink-faint">{agent.id}</div>
					<div>
						<label className="mb-1.5 block text-sm font-medium text-ink-dull">Display Name</label>
						<Input
							size="lg"
							value={displayName}
							onChange={(e) => setDisplayName(e.target.value)}
							placeholder={agent.id}
						/>
					</div>
					<div>
						<label className="mb-1.5 block text-sm font-medium text-ink-dull">Role</label>
						<Input
							size="lg"
							value={role}
							onChange={(e) => setRole(e.target.value)}
							placeholder="e.g. Research Assistant, Code Reviewer"
						/>
					</div>
				</div>
				<DialogFooter>
					<div className="flex-1" />
					<Button variant="ghost" size="sm" onClick={() => onOpenChange(false)}>
						Cancel
					</Button>
					<Button size="sm" onClick={() => onUpdate(displayName, role)}>
						Save
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}

// -- Group Config Panel --

interface GroupConfigPanelProps {
	group: TopologyGroup;
	allAgents: string[];
	onUpdate: (agentIds: string[], name: string) => void;
	onDelete: () => void;
	onClose: () => void;
}

function GroupConfigPanel({
	group,
	allAgents,
	onUpdate,
	onDelete,
	onClose,
}: GroupConfigPanelProps) {
	const [name, setName] = useState(group.name);
	const [agentIds, setAgentIds] = useState<Set<string>>(new Set(group.agent_ids));

	const toggleAgent = (id: string) => {
		setAgentIds((prev) => {
			const next = new Set(prev);
			if (next.has(id)) next.delete(id);
			else next.add(id);
			return next;
		});
	};

	return (
		<motion.div
			initial={{ opacity: 0, y: 8 }}
			animate={{ opacity: 1, y: 0 }}
			exit={{ opacity: 0, y: 8 }}
			transition={{ duration: 0.15 }}
			className="absolute right-4 top-4 z-20 w-64 rounded-lg border border-app-line/50 bg-app-darkBox/95 p-4 shadow-xl backdrop-blur-sm"
		>
			<div className="mb-3 flex items-center justify-between">
				<span className="text-sm font-medium text-ink">Group Settings</span>
				<button
					onClick={onClose}
					className="text-ink-faint hover:text-ink transition-colors text-sm"
				>
					Close
				</button>
			</div>

			{/* Name */}
			<div className="mb-3">
				<label className="mb-1 block text-tiny font-medium text-ink-dull">Name</label>
				<input
					value={name}
					onChange={(e) => setName(e.target.value)}
					className="w-full rounded bg-app-input px-2.5 py-1.5 text-sm text-ink outline-none border border-app-line/50 focus:border-accent/50"
				/>
			</div>

			{/* Agent membership */}
			<div className="mb-4">
				<label className="mb-1 block text-tiny font-medium text-ink-dull">Agents</label>
				<div className="flex flex-col gap-1 max-h-40 overflow-y-auto">
					{allAgents.map((id) => (
						<button
							key={id}
							onClick={() => toggleAgent(id)}
							className={`flex items-center gap-2 rounded px-2 py-1.5 text-tiny transition-colors text-left ${
								agentIds.has(id)
									? "bg-accent/15 text-accent"
									: "bg-app-box text-ink-faint hover:text-ink-dull"
							}`}
						>
							<span
								className={`h-2 w-2 rounded-full flex-shrink-0 ${
									agentIds.has(id) ? "bg-accent" : "bg-app-line"
								}`}
							/>
							{id}
						</button>
					))}
				</div>
			</div>

			<div className="flex gap-2">
				<Button
					onClick={() => onUpdate([...agentIds], name)}
					size="sm"
					className="flex-1 bg-accent/15 text-tiny text-accent hover:bg-accent/25"
				>
					Save
				</Button>
				<Button
					onClick={onDelete}
					size="sm"
					variant="destructive"
					className="text-tiny"
				>
					Delete
				</Button>
			</div>
		</motion.div>
	);
}

// -- Main Component (inner, needs ReactFlowProvider) --

interface TopologyGraphInnerProps {
	activeEdges: Set<string>;
	agents: AgentSummary[];
}

function TopologyGraphInner({ activeEdges, agents }: TopologyGraphInnerProps) {
	const queryClient = useQueryClient();
	const [selectedEdge, setSelectedEdge] = useState<Edge | null>(null);
	const [selectedGroup, setSelectedGroup] = useState<TopologyGroup | null>(null);
	const [selectedHuman, setSelectedHuman] = useState<{ id: string; display_name?: string; role?: string; bio?: string } | null>(null);
	const [humanDialogOpen, setHumanDialogOpen] = useState(false);
	const [selectedAgent, setSelectedAgent] = useState<{ id: string; display_name?: string; role?: string } | null>(null);
	const [agentDialogOpen, setAgentDialogOpen] = useState(false);

	const { data, isLoading, error } = useQuery({
		queryKey: ["topology"],
		queryFn: api.topology,
		refetchInterval: 10_000,
	});

	// Stable refs for opening edit dialogs from node callbacks
	const openHumanEditRef = useRef<(humanId: string) => void>(() => {});
	const openAgentEditRef = useRef<(agentId: string) => void>(() => {});

	// Build agent profile lookup
	const agentProfiles = useMemo(() => {
		const map = new Map<string, AgentSummary>();
		for (const agent of agents) {
			map.set(agent.id, agent);
		}
		return map;
	}, [agents]);

	/** Inject onEdit callbacks into profile nodes */
	const patchEditCallbacks = useCallback((nodes: Node[]): Node[] =>
		nodes.map((n) => {
			if (n.type === "human") {
				return { ...n, data: { ...n.data, onEdit: () => openHumanEditRef.current(n.id) } };
			}
			if (n.type === "agent") {
				return { ...n, data: { ...n.data, onEdit: () => openAgentEditRef.current(n.id) } };
			}
			return n;
		}),
	[]);

	// Build nodes and edges from topology data
	const { initialNodes, initialEdges } = useMemo(() => {
		if (!data) return { initialNodes: [], initialEdges: [] };
		const graph = buildGraph(data, activeEdges, agentProfiles);
		return { initialNodes: patchEditCallbacks(graph.initialNodes), initialEdges: graph.initialEdges };
	}, [data, activeEdges, agentProfiles, patchEditCallbacks]);

	const [nodes, setNodes, onNodesChange] = useNodesState(initialNodes);
	const [edges, setEdges, onEdgesChange] = useEdgesState(initialEdges);

	// Sync when topology data or agent profiles change
	const prevDataRef = useRef(data);
	const prevProfilesRef = useRef(agentProfiles);
	if (data !== prevDataRef.current || agentProfiles !== prevProfilesRef.current) {
		prevDataRef.current = data;
		prevProfilesRef.current = agentProfiles;
		if (data) {
			const { initialNodes: newNodes, initialEdges: newEdges } = buildGraph(
				data,
				activeEdges,
				agentProfiles,
			);

			// Preserve existing node positions
			const positionMap = new Map(
				nodes.map((n) => [n.id, n.position]),
			);
			const mergedNodes = patchEditCallbacks(newNodes.map((n) => ({
				...n,
				position: positionMap.get(n.id) ?? n.position,
			})));

			setNodes(mergedNodes);
			setEdges(newEdges);
		}
	}

	// Update edge activity state when activeEdges changes
	const prevActiveRef = useRef(activeEdges);
	if (activeEdges !== prevActiveRef.current) {
		prevActiveRef.current = activeEdges;
		setEdges((eds) =>
			eds.map((e) => ({
				...e,
				data: { ...e.data, active: activeEdges.has(e.id) },
			})),
		);
	}

	// -- Mutations --

	const createLink = useMutation({
		mutationFn: (params: {
			from: string;
			to: string;
			direction: LinkDirection;
			kind: LinkKind;
		}) =>
			api.createLink({
				from: params.from,
				to: params.to,
				direction: params.direction,
				kind: params.kind,
			}),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["topology"] });
		},
	});

	const updateLink = useMutation({
		mutationFn: (params: {
			from: string;
			to: string;
			direction: LinkDirection;
			kind: LinkKind;
		}) =>
			api.updateLink(params.from, params.to, {
				direction: params.direction,
				kind: params.kind,
			}),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["topology"] });
			setSelectedEdge(null);
		},
	});

	const deleteLink = useMutation({
		mutationFn: (params: { from: string; to: string }) =>
			api.deleteLink(params.from, params.to),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["topology"] });
			setSelectedEdge(null);
		},
	});

	const createGroup = useMutation({
		mutationFn: (name: string) =>
			api.createGroup({ name, agent_ids: [] }),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["topology"] });
		},
	});

	const updateGroup = useMutation({
		mutationFn: (params: { originalName: string; agentIds: string[]; name: string }) =>
			api.updateGroup(params.originalName, {
				name: params.name,
				agent_ids: params.agentIds,
			}),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["topology"] });
			setSelectedGroup(null);
		},
	});

	const deleteGroup = useMutation({
		mutationFn: (name: string) => api.deleteGroup(name),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["topology"] });
			setSelectedGroup(null);
		},
	});

	const createHuman = useMutation({
		mutationFn: (id: string) => api.createHuman({ id }),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["topology"] });
		},
	});

	const updateHuman = useMutation({
		mutationFn: (params: { id: string; displayName?: string; role?: string; bio?: string }) =>
			api.updateHuman(params.id, {
				display_name: params.displayName,
				role: params.role,
				bio: params.bio,
			}),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["topology"] });
			setSelectedHuman(null);
		},
	});

	const deleteHuman = useMutation({
		mutationFn: (id: string) => api.deleteHuman(id),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["topology"] });
			setSelectedHuman(null);
		},
	});

	const updateAgentMutation = useMutation({
		mutationFn: (params: { id: string; displayName?: string; role?: string }) =>
			api.updateAgent(params.id, {
				display_name: params.displayName,
				role: params.role,
			}),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["topology"] });
			queryClient.invalidateQueries({ queryKey: ["agents"] });
			setSelectedAgent(null);
			setAgentDialogOpen(false);
		},
	});

	// Handle new connection (drag from handle to handle)
	const onConnect = useCallback(
		(connection: Connection) => {
			if (!connection.source || !connection.target) return;
			if (connection.source === connection.target) return;

			const exists = edges.some(
				(e) =>
					e.source === connection.source && e.target === connection.target,
			);
			if (exists) return;

			const kind = inferKindFromHandle(connection.sourceHandle ?? null);

			// For hierarchical links from bottom handle, from=source (superior), to=target
			// For hierarchical links from top handle, from=target (superior), to=source
			const isFromTop = connection.sourceHandle === "top";
			const from = (kind === "hierarchical" && isFromTop) ? connection.target : connection.source;
			const to = (kind === "hierarchical" && isFromTop) ? connection.source : connection.target;

			createLink.mutate({
				from,
				to,
				direction: "two_way",
				kind,
			});
		},
		[edges, createLink],
	);

	const onEdgeClick = useCallback(
		(_: React.MouseEvent, edge: Edge) => {
			setSelectedEdge(edge);
			setSelectedGroup(null);
		},
		[],
	);

	const groups = data?.groups ?? [];
	const { fitView } = useReactFlow();

	const openHumanEdit = useCallback(
		(humanId: string) => {
			const humans = data?.humans ?? [];
			const human = humans.find((h) => h.id === humanId);
			if (human) {
				setSelectedHuman(human);
				setHumanDialogOpen(true);
				setSelectedEdge(null);
				setSelectedGroup(null);
			}
		},
		[data],
	);
	openHumanEditRef.current = openHumanEdit;

	const openAgentEdit = useCallback(
		(agentId: string) => {
			const topoAgent = data?.agents.find((a) => a.id === agentId);
			if (topoAgent) {
				setSelectedAgent({
					id: topoAgent.id,
					display_name: topoAgent.display_name,
					role: topoAgent.role,
				});
				setAgentDialogOpen(true);
				setSelectedEdge(null);
				setSelectedGroup(null);
				setSelectedHuman(null);
			}
		},
		[data],
	);
	openAgentEditRef.current = openAgentEdit;

	const onNodeClick = useCallback(
		(_: React.MouseEvent, node: Node) => {
			if (node.type === "group" && groups.length > 0) {
				const group = groups.find((g) => `group:${g.name}` === node.id);
				if (group) {
					setSelectedGroup(group);
					setSelectedEdge(null);
					setSelectedHuman(null);
				}
			} else {
				setSelectedGroup(null);
				setSelectedHuman(null);
			}

			// Zoom the selected node into view
			if (node.type === "agent" || node.type === "human") {
				fitView({
					nodes: [{ id: node.id }],
					duration: 400,
					padding: 0.5,
					maxZoom: 1.5,
				});
			}
		},
		[groups, fitView],
	);

	const onPaneClick = useCallback(() => {
		setSelectedEdge(null);
		setSelectedGroup(null);
		setSelectedHuman(null);
	}, []);

	// Handle node drops into/out of groups via position change
	const handleNodesChange = useCallback(
		(changes: NodeChange[]) => {
			onNodesChange(changes);

			// Persist positions when any drag ends
			const hasDragEnd = changes.some(
				(c) => c.type === "position" && !c.dragging,
			);
			if (hasDragEnd) {
				// Read latest nodes after the change is applied
				setNodes((current) => {
					savePositions(current);
					return current;
				});
			}

			// After a drag ends, check if an agent node was dropped onto a group
			for (const change of changes) {
				if (change.type === "position" && !change.dragging && groups.length > 0) {
					const draggedNode = nodes.find((n) => n.id === change.id);
					if (!draggedNode || draggedNode.type !== "agent") continue;

					const agentId = draggedNode.id;
					const currentGroup = groups.find((g) =>
						g.agent_ids.includes(agentId),
					);

					// Find if the agent was dropped onto a group node
					const groupNodes = nodes.filter((n) => n.type === "group");
					let targetGroup: TopologyGroup | null = null;

					for (const gNode of groupNodes) {
						const gw = (gNode.data.width as number) ?? 0;
						const gh = (gNode.data.height as number) ?? 0;
						const pos = draggedNode.position;
						if (
							pos.x > gNode.position.x &&
							pos.x < gNode.position.x + gw &&
							pos.y > gNode.position.y &&
							pos.y < gNode.position.y + gh
						) {
							const group = groups.find(
								(g) => `group:${g.name}` === gNode.id,
							);
							if (group) {
								targetGroup = group;
								break;
							}
						}
					}

					if (targetGroup && !targetGroup.agent_ids.includes(agentId)) {
						// Add to target group
						const newIds = [...targetGroup.agent_ids, agentId];
						// Remove from current group if any
						if (currentGroup && currentGroup.name !== targetGroup.name) {
							api.updateGroup(currentGroup.name, {
								agent_ids: currentGroup.agent_ids.filter(
									(id) => id !== agentId,
								),
							}).then(() => queryClient.invalidateQueries({ queryKey: ["topology"] }));
						}
						api.updateGroup(targetGroup.name, {
							agent_ids: newIds,
						}).then(() => queryClient.invalidateQueries({ queryKey: ["topology"] }));
					} else if (
						!targetGroup &&
						currentGroup
					) {
						// Dragged out of a group
						api.updateGroup(currentGroup.name, {
							agent_ids: currentGroup.agent_ids.filter(
								(id) => id !== agentId,
							),
						}).then(() => queryClient.invalidateQueries({ queryKey: ["topology"] }));
					}
				}
			}
		},
		[onNodesChange, nodes, groups, queryClient],
	);

	const handleCreateGroup = useCallback(() => {
		const name = `Group ${groups.length + 1}`;
		createGroup.mutate(name);
	}, [data, createGroup]);

	if (isLoading) {
		return (
			<div className="flex h-full items-center justify-center">
				<div className="flex items-center gap-2 text-ink-dull">
					<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
					Loading topology...
				</div>
			</div>
		);
	}

	if (error) {
		return (
			<div className="flex h-full items-center justify-center">
				<p className="text-sm text-red-400">
					Failed to load topology
				</p>
			</div>
		);
	}

	if (!data || data.agents.length === 0) {
		return (
			<div className="flex h-full items-center justify-center">
				<p className="text-sm text-ink-faint">
					No agents configured
				</p>
			</div>
		);
	}

	const allAgentIds = data.agents.map((a) => a.id);

	return (
		<div className="relative h-full w-full select-none">
			<ReactFlow
				nodes={nodes}
				edges={edges}
				onNodesChange={handleNodesChange}
				onEdgesChange={onEdgesChange}
				onConnect={onConnect}
				onEdgeClick={onEdgeClick}
				onNodeClick={onNodeClick}
				onPaneClick={onPaneClick}
				nodeTypes={nodeTypes}
				edgeTypes={edgeTypes}
				defaultEdgeOptions={{
					type: "link",
					markerEnd: {
						type: MarkerType.ArrowClosed,
						width: 16,
						height: 16,
					},
				}}
				fitView
				fitViewOptions={{ padding: 0.3 }}
				proOptions={{ hideAttribution: true }}
				className="topology-graph"
			>
				<Background
					color="hsla(230, 8%, 14%, 0.5)"
					gap={20}
					size={1}
				/>
				<Controls
					showInteractive={false}
					className="!bg-app-darkBox/80 !border-app-line !backdrop-blur-sm [&>button]:!bg-transparent [&>button]:!border-app-line [&>button]:!text-ink-dull [&>button:hover]:!bg-app-hover"
				/>
			</ReactFlow>

			{/* Legend + controls */}
			<div className="absolute bottom-4 left-4 z-10 rounded-md bg-app-darkBox/80 p-3 backdrop-blur-sm">
				<div className="mb-2 text-tiny text-ink-faint">
					Drag between handles to link
				</div>
				<div className="flex flex-col gap-1 text-tiny text-ink-faint">
					<span>Top/Bottom → Hierarchical</span>
					<span>Left/Right → Peer</span>
				</div>
				<div className="mt-2 flex flex-col gap-1">
					<button
						onClick={handleCreateGroup}
						className="w-full rounded bg-app-box px-2 py-1.5 text-tiny text-ink-faint hover:text-ink transition-colors text-left"
					>
						+ New Group
					</button>
					<button
						onClick={() => {
							const id = `human-${(data?.humans?.length ?? 0) + 1}`;
							createHuman.mutate(id);
						}}
						className="w-full rounded bg-app-box px-2 py-1.5 text-tiny text-ink-faint hover:text-ink transition-colors text-left"
					>
						+ New Human
					</button>
				</div>
			</div>

			{/* Edge config panel */}
			<AnimatePresence>
				{selectedEdge && (
					<EdgeConfigPanel
						key={selectedEdge.id}
						edge={selectedEdge}
						onUpdate={(direction, kind) =>
							updateLink.mutate({
								from: selectedEdge.source,
								to: selectedEdge.target,
								direction,
								kind,
							})
						}
						onDelete={() =>
							deleteLink.mutate({
								from: selectedEdge.source,
								to: selectedEdge.target,
							})
						}
						onClose={() => setSelectedEdge(null)}
					/>
				)}
			</AnimatePresence>

			{/* Group config panel */}
			<AnimatePresence>
				{selectedGroup && (
					<GroupConfigPanel
						key={selectedGroup.name}
						group={selectedGroup}
						allAgents={allAgentIds}
						onUpdate={(agentIds, name) =>
							updateGroup.mutate({
								originalName: selectedGroup.name,
								agentIds,
								name,
							})
						}
						onDelete={() => deleteGroup.mutate(selectedGroup.name)}
						onClose={() => setSelectedGroup(null)}
					/>
				)}
			</AnimatePresence>

			{/* Human edit dialog */}
			<HumanEditDialog
				human={selectedHuman}
				open={humanDialogOpen}
				onOpenChange={(open) => {
					setHumanDialogOpen(open);
					if (!open) setSelectedHuman(null);
				}}
				onUpdate={(displayName, role, bio) => {
					if (selectedHuman) {
						updateHuman.mutate({
							id: selectedHuman.id,
							displayName: displayName || undefined,
							role: role || undefined,
							bio: bio || undefined,
						});
						setHumanDialogOpen(false);
					}
				}}
				onDelete={() => {
					if (selectedHuman) {
						deleteHuman.mutate(selectedHuman.id);
						setHumanDialogOpen(false);
					}
				}}
			/>

			{/* Agent edit dialog */}
			<AgentEditDialog
				agent={selectedAgent}
				open={agentDialogOpen}
				onOpenChange={(open) => {
					setAgentDialogOpen(open);
					if (!open) setSelectedAgent(null);
				}}
				onUpdate={(displayName, role) => {
					if (selectedAgent) {
						updateAgentMutation.mutate({
							id: selectedAgent.id,
							displayName: displayName || undefined,
							role: role || undefined,
						});
					}
				}}
			/>
		</div>
	);
}

// -- Graph Builder --

/** Estimate the rendered height of an agent node based on profile data. */
function estimateNodeHeight(summary: AgentSummary | undefined): number {
	let h = 48 + 24 + 8 + 16 + 24; // banner + avatar + name row + stats + padding
	if (summary?.profile?.status) h += 16;
	if (summary?.profile?.bio) h += 40;
	return h;
}

function buildGraph(
	data: TopologyResponse,
	activeEdges: Set<string>,
	agentProfiles: Map<string, AgentSummary>,
): { initialNodes: Node[]; initialEdges: Edge[] } {
	const saved = loadPositions();
	const allNodes: Node[] = [];

	// Topology agent lookup for display_name / role
	const topologyAgentMap = new Map(data.agents.map((a) => [a.id, a]));

	const links = data.links ?? [];
	// connectedHandles is computed after nodes are positioned (needs position map for peers)

	// Build group membership lookup
	const groups = data.groups ?? [];
	const agentToGroup = new Map<string, TopologyGroup>();
	for (const group of groups) {
		for (const agentId of group.agent_ids) {
			agentToGroup.set(agentId, group);
		}
	}

	// Agents not in any group
	const ungroupedAgents = data.agents.filter((a) => !agentToGroup.has(a.id));

	// Create group nodes
	const groupPositions = new Map<string, { x: number; y: number }>();
	let groupX = 0;

	for (let gi = 0; gi < groups.length; gi++) {
		const group = groups[gi];
		const memberCount = group.agent_ids.length;
		const cols = Math.max(1, Math.min(memberCount, 2));
		const rows = Math.ceil(memberCount / cols);
		const groupWidth =
			cols * (NODE_WIDTH + GROUP_PADDING) + GROUP_PADDING;
		const maxMemberHeight = group.agent_ids.reduce((max, id) => {
			return Math.max(max, estimateNodeHeight(agentProfiles.get(id)));
		}, 170);
		const groupHeight =
			GROUP_HEADER + rows * (maxMemberHeight + GROUP_PADDING) + GROUP_PADDING;

		const color = group.color ?? GROUP_COLORS[gi % GROUP_COLORS.length];
		const pos = { x: groupX, y: 0 };
		groupPositions.set(group.name, pos);

		allNodes.push({
			id: `group:${group.name}`,
			type: "group",
			position: pos,
			data: {
				label: group.name,
				color,
				width: groupWidth,
				height: groupHeight,
			},
			style: { width: groupWidth, height: groupHeight },
			draggable: true,
			selectable: true,
			zIndex: -1,
		});

		// Position member agents inside the group
		group.agent_ids.forEach((agentId, idx) => {
			const col = idx % cols;
			const row = Math.floor(idx / cols);
			const summary = agentProfiles.get(agentId);
			const profile = summary?.profile;
			const isOnline =
				summary?.last_activity_at != null &&
				new Date(summary.last_activity_at).getTime() >
					Date.now() - 5 * 60 * 1000;

			const topoAgent = topologyAgentMap.get(agentId);
			allNodes.push({
				id: agentId,
				type: "agent",
				position: {
					x: GROUP_PADDING + col * (NODE_WIDTH + GROUP_PADDING),
					y: GROUP_HEADER + GROUP_PADDING + row * (maxMemberHeight + GROUP_PADDING),
				},
				parentId: `group:${group.name}`,
				extent: "parent" as const,
				data: {
					nodeId: agentId,
					nodeKind: "agent",
					configDisplayName: topoAgent?.display_name ?? null,
					configRole: topoAgent?.role ?? null,
					chosenName: profile?.display_name ?? null,
					avatarSeed: profile?.avatar_seed ?? agentId,
					bio: profile?.bio ?? null,
					isOnline,
					channelCount: summary?.channel_count ?? 0,
					memoryCount: summary?.memory_total ?? 0,
					connectedHandles: { top: false, bottom: false, left: false, right: false },
				},
			});
		});

		groupX += groupWidth + 80;
	}

	// Position ungrouped agents
	const ungroupedStartX = groupX;
	const radius = Math.max(200, ungroupedAgents.length * 80);
	const centerX = ungroupedStartX + radius + NODE_WIDTH / 2;
	const centerY = 300;

	ungroupedAgents.forEach((agent, index) => {
		const count = ungroupedAgents.length;
		const angle = (2 * Math.PI * index) / count - Math.PI / 2;
		const summary = agentProfiles.get(agent.id);
		const profile = summary?.profile;
		const isOnline =
			summary?.last_activity_at != null &&
			new Date(summary.last_activity_at).getTime() >
				Date.now() - 5 * 60 * 1000;

		allNodes.push({
			id: agent.id,
			type: "agent",
			position:
				count === 1
					? { x: ungroupedStartX, y: 100 }
					: {
							x: centerX + radius * Math.cos(angle),
							y: centerY + radius * Math.sin(angle),
						},
			data: {
				nodeId: agent.id,
				nodeKind: "agent",
				configDisplayName: agent.display_name ?? null,
				configRole: agent.role ?? null,
				chosenName: profile?.display_name ?? null,
				avatarSeed: profile?.avatar_seed ?? agent.id,
				bio: profile?.bio ?? null,
				isOnline,
				channelCount: summary?.channel_count ?? 0,
				memoryCount: summary?.memory_total ?? 0,
				connectedHandles: { top: false, bottom: false, left: false, right: false },
			},
		});
	});

	// Add human nodes
	const humans = data.humans ?? [];
	const humanStartX = ungroupedAgents.length > 0
		? centerX + radius + NODE_WIDTH + 80
		: ungroupedStartX;

	humans.forEach((human, index) => {
		allNodes.push({
			id: human.id,
			type: "human",
			position: { x: humanStartX, y: index * 220 },
			data: {
				nodeId: human.id,
				nodeKind: "human",
				configDisplayName: human.display_name ?? null,
				configRole: human.role ?? null,
				bio: human.bio ?? null,
				avatarSeed: human.id,
				connectedHandles: { top: false, bottom: false, left: false, right: false },
			},
		});
	});

	// Build absolute position lookup for handle routing
	const nodePositionMap = new Map<string, { x: number; y: number }>();
	for (const node of allNodes) {
		if (node.parentId) {
			// Child nodes have positions relative to parent — compute absolute
			const parent = allNodes.find((n) => n.id === node.parentId);
			if (parent) {
				nodePositionMap.set(node.id, {
					x: parent.position.x + node.position.x,
					y: parent.position.y + node.position.y,
				});
				continue;
			}
		}
		nodePositionMap.set(node.id, node.position);
	}

	// Compute connected handles using actual positions
	const connectedHandles = new Set<string>();
	for (const link of links) {
		const { sourceHandle, targetHandle } = getHandlesForKind(
			link.kind,
			nodePositionMap.get(link.from),
			nodePositionMap.get(link.to),
		);
		connectedHandles.add(`${link.from}:${sourceHandle}`);
		connectedHandles.add(`${link.to}:${targetHandle}`);
	}

	// Patch connectedHandles onto nodes
	for (const node of allNodes) {
		if (node.type === "agent" || node.type === "human") {
			const nid = node.id;
			node.data = {
				...node.data,
				connectedHandles: {
					top: connectedHandles.has(`${nid}:top`),
					bottom: connectedHandles.has(`${nid}:bottom`),
					left: connectedHandles.has(`${nid}:left`),
					right: connectedHandles.has(`${nid}:right`),
				},
			};
		}
	}

	const initialEdges: Edge[] = data.links.map((link) => {
		const edgeId = `${link.from}->${link.to}`;
		const { sourceHandle, targetHandle } = getHandlesForKind(
			link.kind,
			nodePositionMap.get(link.from),
			nodePositionMap.get(link.to),
		);
		return {
			id: edgeId,
			source: link.from,
			target: link.to,
			sourceHandle,
			targetHandle,
			type: "link",
			data: {
				direction: link.direction,
				kind: link.kind,
				active: activeEdges.has(edgeId),
			},
			style: {
				stroke: EDGE_COLOR,
			},
		};
	});

	// Apply saved positions (override computed layout)
	for (const node of allNodes) {
		const savedPos = saved[node.id];
		if (savedPos) {
			node.position = savedPos;
		}
	}

	return { initialNodes: allNodes, initialEdges };
}

// -- Exported component with provider wrapper --

export interface TopologyGraphProps {
	activeEdges?: Set<string>;
	agents?: AgentSummary[];
}

export function TopologyGraph({ activeEdges, agents }: TopologyGraphProps) {
	const edges = activeEdges ?? new Set<string>();
	const agentList = agents ?? [];
	return (
		<ReactFlowProvider>
			<TopologyGraphInner activeEdges={edges} agents={agentList} />
		</ReactFlowProvider>
	);
}
