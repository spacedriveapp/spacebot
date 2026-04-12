import {useCallback, useEffect, useMemo, useRef, useState} from "react";
import {MagnifyingGlassMinus, MagnifyingGlassPlus, CornersOut} from "@phosphor-icons/react";
import {CircleButton} from "@spacedrive/primitives";
import {
	ReactFlow,
	Background,
	type Node,
	type Edge,
	type Connection,
	type NodeTypes,
	type EdgeTypes,
	type NodeChange,
	type EdgeChange,
	useNodesState,
	useEdgesState,
	useReactFlow,
	MarkerType,
} from "@xyflow/react";
import {useQuery, useMutation, useQueryClient} from "@tanstack/react-query";
import {AnimatePresence} from "framer-motion";
import {
	api,
	type AgentSummary,
	type TopologyGroup,
	type TopologyAgent,
	type TopologyHuman,
	type LinkDirection,
	type LinkKind,
} from "@/api/client";
import {GroupNode} from "./GroupNode";
import {ProfileNode} from "./ProfileNode";
import {LinkEdge} from "./LinkEdge";
import {EdgeConfigPanel} from "./EdgeConfigPanel";
import {GroupConfigPanel} from "./GroupConfigPanel";
import {HumanEditDialog} from "./HumanEditDialog";
import {AgentEditDialog} from "./AgentEditDialog";
import {buildGraph} from "./buildGraph";
import {inferKindFromHandle} from "./handles";
import {HANDLES_KEY, loadHandles, savePositions} from "./storage";

const nodeTypes: NodeTypes = {
	agent: ProfileNode,
	human: ProfileNode,
	group: GroupNode,
};

const edgeTypes: EdgeTypes = {
	link: LinkEdge,
};

interface OrgGraphInnerProps {
	activeEdges: Set<string>;
	agents: AgentSummary[];
}

export function OrgGraphInner({activeEdges, agents}: OrgGraphInnerProps) {
	const queryClient = useQueryClient();
	const [selectedEdge, setSelectedEdge] = useState<Edge | null>(null);
	const [selectedGroup, setSelectedGroup] = useState<TopologyGroup | null>(
		null,
	);
	const [selectedHuman, setSelectedHuman] = useState<TopologyHuman | null>(
		null,
	);
	const [humanDialogOpen, setHumanDialogOpen] = useState(false);
	const [selectedAgent, setSelectedAgent] = useState<TopologyAgent | null>(
		null,
	);
	const [agentDialogOpen, setAgentDialogOpen] = useState(false);

	const {data, isLoading, error} = useQuery({
		queryKey: ["topology"],
		queryFn: api.topology,
		refetchInterval: 10_000,
	});

	// Stable refs for opening edit dialogs from node callbacks
	const openHumanEditRef = useRef<(humanId: string) => void>(() => {});
	const openAgentEditRef = useRef<(agentId: string) => void>(() => {});

	// Fetch agent info for gradient/appearance data
	const {data: agentInfoData} = useQuery({
		queryKey: ["agents"],
		queryFn: api.agents,
		refetchInterval: 30_000,
	});

	// Build agent profile lookup
	const agentProfiles = useMemo(() => {
		const map = new Map<string, AgentSummary>();
		for (const agent of agents) {
			map.set(agent.id, agent);
		}
		return map;
	}, [agents]);

	// Build agent info lookup (for gradient colors)
	const agentInfoMap = useMemo(() => {
		const map = new Map<
			string,
			{
				gradient_start?: string | null | undefined;
				gradient_end?: string | null | undefined;
			}
		>();
		for (const info of agentInfoData?.agents ?? []) {
			map.set(info.id, {
				gradient_start: info.gradient_start,
				gradient_end: info.gradient_end,
			});
		}
		return map;
	}, [agentInfoData]);

	/** Inject onEdit callbacks into profile nodes */
	const patchEditCallbacks = useCallback(
		(nodes: Node[]): Node[] =>
			nodes.map((n) => {
				if (n.type === "human") {
					return {
						...n,
						data: {...n.data, onEdit: () => openHumanEditRef.current(n.id)},
					};
				}
				if (n.type === "agent") {
					return {
						...n,
						data: {...n.data, onEdit: () => openAgentEditRef.current(n.id)},
					};
				}
				return n;
			}),
		[],
	);

	// Build nodes and edges from topology data
	const {initialNodes, initialEdges} = useMemo(() => {
		if (!data) return {initialNodes: [], initialEdges: []};
		const graph = buildGraph(data, activeEdges, agentProfiles, agentInfoMap);
		return {
			initialNodes: patchEditCallbacks(graph.initialNodes),
			initialEdges: graph.initialEdges,
		};
	}, [data, activeEdges, agentProfiles, agentInfoMap, patchEditCallbacks]);

	const [nodes, setNodes, onNodesChange] = useNodesState(initialNodes);
	const [edges, setEdges, onEdgesChange] = useEdgesState(initialEdges);

	// Holds the latest activeEdges for the topology-sync effect so we do not
	// have to rebuild the whole graph every time edge activity flickers.
	const activeEdgesRef = useRef(activeEdges);

	// Sync when topology data or agent profiles change. Uses the functional
	// updater form of setNodes so the effect does not need `nodes` in its deps,
	// which would cause an infinite loop.
	useEffect(() => {
		if (!data) return;
		const {initialNodes: newNodes, initialEdges: newEdges} = buildGraph(
			data,
			activeEdgesRef.current,
			agentProfiles,
			agentInfoMap,
		);
		setNodes((prevNodes) => {
			const positionMap = new Map(prevNodes.map((n) => [n.id, n.position]));
			return patchEditCallbacks(
				newNodes.map((n) => ({
					...n,
					position: positionMap.get(n.id) ?? n.position,
				})),
			);
		});
		setEdges(newEdges);
	}, [data, agentProfiles, agentInfoMap, patchEditCallbacks, setNodes, setEdges]);

	// Update edge activity state when activeEdges changes, without rebuilding
	// the entire node graph.
	useEffect(() => {
		activeEdgesRef.current = activeEdges;
		setEdges((eds) =>
			eds.map((e) => ({
				...e,
				data: {...e.data, active: activeEdges.has(e.id)},
			})),
		);
	}, [activeEdges, setEdges]);

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
			queryClient.invalidateQueries({queryKey: ["topology"]});
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
			queryClient.invalidateQueries({queryKey: ["topology"]});
			setSelectedEdge(null);
		},
	});

	const deleteLink = useMutation({
		mutationFn: (params: {from: string; to: string}) =>
			api.deleteLink(params.from, params.to),
		onSuccess: (_, params) => {
			// Clean up saved handles
			const edgeId = `${params.from}->${params.to}`;
			const currentHandles = loadHandles();
			delete currentHandles[edgeId];
			try {
				localStorage.setItem(HANDLES_KEY, JSON.stringify(currentHandles));
			} catch {
				// ignore
			}
			queryClient.invalidateQueries({queryKey: ["topology"]});
			setSelectedEdge(null);
		},
	});

	const updateGroup = useMutation({
		mutationFn: (params: {
			originalName: string;
			agentIds: string[];
			name: string;
		}) =>
			api.updateGroup(params.originalName, {
				name: params.name,
				agent_ids: params.agentIds,
			}),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["topology"]});
			setSelectedGroup(null);
		},
	});

	const deleteGroup = useMutation({
		mutationFn: (name: string) => api.deleteGroup(name),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["topology"]});
			setSelectedGroup(null);
		},
	});

	// Updates only the membership of a group, used by the drag-to-group
	// gesture. Separate from `updateGroup` so it does not clear selection.
	const assignGroupMembership = useMutation({
		mutationFn: (params: {groupName: string; agentIds: string[]}) =>
			api.updateGroup(params.groupName, {agent_ids: params.agentIds}),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["topology"]});
		},
		onError: (error) => {
			console.error("failed to update group membership", error);
		},
	});

	const updateHuman = useMutation({
		mutationFn: (params: {
			id: string;
			displayName?: string;
			role?: string;
			bio?: string;
			description?: string;
			discordId?: string;
			telegramId?: string;
			slackId?: string;
			email?: string;
		}) =>
			api.updateHuman(params.id, {
				display_name: params.displayName,
				role: params.role,
				bio: params.bio,
				description: params.description,
				discord_id: params.discordId,
				telegram_id: params.telegramId,
				slack_id: params.slackId,
				email: params.email,
			}),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["topology"]});
			setSelectedHuman(null);
		},
	});

	const deleteHuman = useMutation({
		mutationFn: (id: string) => api.deleteHuman(id),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["topology"]});
			setSelectedHuman(null);
		},
	});

	const updateAgentMutation = useMutation({
		mutationFn: (params: {id: string; displayName?: string; role?: string}) =>
			api.updateAgent(params.id, {
				display_name: params.displayName,
				role: params.role,
			}),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["topology"]});
			queryClient.invalidateQueries({queryKey: ["agents"]});
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
				(e) => e.source === connection.source && e.target === connection.target,
			);
			if (exists) return;

			const kind = inferKindFromHandle(connection.sourceHandle ?? null);

			// For hierarchical links from bottom handle, from=source (superior), to=target
			// For hierarchical links from top handle, from=target (superior), to=source
			const isFromTop = connection.sourceHandle === "top";
			const from =
				kind === "hierarchical" && isFromTop
					? connection.target
					: connection.source;
			const to =
				kind === "hierarchical" && isFromTop
					? connection.source
					: connection.target;

			// Save handle information to localStorage
			// Handles need to be stored relative to the final from/to, not the connection source/target
			const edgeId = `${from}->${to}`;
			if (connection.sourceHandle && connection.targetHandle) {
				const currentHandles = loadHandles();
				currentHandles[edgeId] = {
					sourceHandle:
						kind === "hierarchical" && isFromTop
							? connection.targetHandle
							: connection.sourceHandle,
					targetHandle:
						kind === "hierarchical" && isFromTop
							? connection.sourceHandle
							: connection.targetHandle,
				};
				try {
					localStorage.setItem(HANDLES_KEY, JSON.stringify(currentHandles));
				} catch {
					// ignore
				}
			}

			createLink.mutate({
				from,
				to,
				direction: "two_way",
				kind,
			});
		},
		[edges, createLink],
	);

	const handleEdgesChange = useCallback(
		(changes: EdgeChange[]) => {
			// Intercept edge removal (Delete/Backspace key)
			for (const change of changes) {
				if (change.type === "remove") {
					const edge = edges.find((e) => e.id === change.id);
					if (edge) {
						// Parse edge ID format: "from->to"
						const [from, to] = edge.id.split("->");
						if (from && to) {
							deleteLink.mutate({from, to});
						}
					}
				}
			}
			// Apply the changes to local state
			onEdgesChange(changes);
		},
		[edges, deleteLink, onEdgesChange],
	);

	const onEdgeClick = useCallback((_: React.MouseEvent, edge: Edge) => {
		setSelectedEdge(edge);
		setSelectedGroup(null);
	}, []);

	const groups = data?.groups ?? [];
	const {fitView, zoomIn, zoomOut} = useReactFlow();

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
				setSelectedAgent(topoAgent);
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
					nodes: [{id: node.id}],
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
				if (
					change.type === "position" &&
					!change.dragging &&
					groups.length > 0
				) {
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
							const group = groups.find((g) => `group:${g.name}` === gNode.id);
							if (group) {
								targetGroup = group;
								break;
							}
						}
					}

					if (targetGroup && !targetGroup.agent_ids.includes(agentId)) {
						const newIds = [...targetGroup.agent_ids, agentId];
						if (currentGroup && currentGroup.name !== targetGroup.name) {
							assignGroupMembership.mutate({
								groupName: currentGroup.name,
								agentIds: currentGroup.agent_ids.filter(
									(id) => id !== agentId,
								),
							});
						}
						assignGroupMembership.mutate({
							groupName: targetGroup.name,
							agentIds: newIds,
						});
					} else if (!targetGroup && currentGroup) {
						assignGroupMembership.mutate({
							groupName: currentGroup.name,
							agentIds: currentGroup.agent_ids.filter(
								(id) => id !== agentId,
							),
						});
					}
				}
			}
		},
		[onNodesChange, nodes, groups, setNodes, assignGroupMembership],
	);

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
				<p className="text-sm text-red-400">Failed to load topology</p>
			</div>
		);
	}

	if (!data || data.agents.length === 0) {
		return (
			<div className="flex h-full items-center justify-center">
				<p className="text-sm text-ink-faint">No agents configured</p>
			</div>
		);
	}

	const allAgentIds = data.agents.map((a) => a.id);

	return (
		<div className="relative h-full w-full select-none bg-app">
			<ReactFlow
				colorMode="dark"
				style={
					{
						backgroundColor: "transparent",
						"--xy-background-color-default": "transparent",
						"--xy-background-color": "transparent",
					} as React.CSSProperties
				}
				nodes={nodes}
				edges={edges}
				onNodesChange={handleNodesChange}
				onEdgesChange={handleEdgesChange}
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
				fitViewOptions={{padding: 0.3}}
				proOptions={{hideAttribution: true}}
				className="topology-graph"
			>
				<Background color="hsla(230, 8%, 18%, 1)" gap={20} size={1} />
			</ReactFlow>

			{/* Zoom controls */}
			<div className="absolute bottom-4 right-4 z-10 flex flex-col gap-1">
				<CircleButton icon={MagnifyingGlassPlus} title="Zoom in" onClick={() => zoomIn({duration: 200})} />
				<CircleButton icon={MagnifyingGlassMinus} title="Zoom out" onClick={() => zoomOut({duration: 200})} />
				<CircleButton icon={CornersOut} title="Fit view" onClick={() => fitView({padding: 0.3, duration: 400})} />
			</div>

			{/* Legend + controls */}
			<div className="absolute bottom-4 left-4 z-10 rounded-md bg-app-dark-box/80 p-3 backdrop-blur-sm">
				<div className="mb-2 text-tiny text-ink-faint">
					Drag between handles to link
				</div>
				<div className="flex flex-col gap-1 text-tiny text-ink-faint">
					<span>Top/Bottom → Hierarchical</span>
					<span>Left/Right → Peer</span>
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
				onUpdate={(fields) => {
					if (selectedHuman) {
						updateHuman.mutate({
							id: selectedHuman.id,
							displayName: fields.displayName || undefined,
							role: fields.role || undefined,
							bio: fields.bio || undefined,
							description: fields.description || undefined,
							discordId: fields.discordId || undefined,
							telegramId: fields.telegramId || undefined,
							slackId: fields.slackId || undefined,
							email: fields.email || undefined,
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
