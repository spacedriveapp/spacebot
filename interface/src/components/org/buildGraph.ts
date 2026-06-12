import type {Node, Edge} from "@xyflow/react";
import {
	api,
	type AgentSummary,
	type TopologyResponse,
	type TopologyGroup,
} from "@/api/client";
import {
	EDGE_COLOR,
	GROUP_COLORS,
	GROUP_HEADER,
	GROUP_PADDING,
	NODE_WIDTH,
} from "./constants";
import {getHandlesForKind} from "./handles";
import {loadHandles, loadPositions} from "./storage";

/** Estimate the rendered height of an agent node based on profile data. */
export function estimateNodeHeight(summary: AgentSummary | undefined): number {
	let h = 48 + 24 + 8 + 16 + 24; // banner + avatar + name row + stats + padding
	if (summary?.profile?.status) h += 16;
	if (summary?.profile?.bio) h += 40;
	return h;
}

export function buildGraph(
	data: TopologyResponse,
	activeEdges: Set<string>,
	agentProfiles: Map<string, AgentSummary>,
	agentInfoMap: Map<
		string,
		{
			gradient_start?: string | null | undefined;
			gradient_end?: string | null | undefined;
		}
	>,
): {initialNodes: Node[]; initialEdges: Edge[]} {
	const saved = loadPositions();
	const savedHandles = loadHandles();
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
	const groupPositions = new Map<string, {x: number; y: number}>();
	let groupX = 0;

	for (let gi = 0; gi < groups.length; gi++) {
		const group = groups[gi];
		const memberCount = group.agent_ids.length;
		const cols = Math.max(1, Math.min(memberCount, 2));
		const rows = Math.ceil(memberCount / cols);
		const groupWidth = cols * (NODE_WIDTH + GROUP_PADDING) + GROUP_PADDING;
		const maxMemberHeight = group.agent_ids.reduce((max, id) => {
			return Math.max(max, estimateNodeHeight(agentProfiles.get(id)));
		}, 170);
		const groupHeight =
			GROUP_HEADER + rows * (maxMemberHeight + GROUP_PADDING) + GROUP_PADDING;

		const color = group.color ?? GROUP_COLORS[gi % GROUP_COLORS.length];
		const pos = {x: groupX, y: 0};
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
			style: {width: groupWidth, height: groupHeight},
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
					y:
						GROUP_HEADER +
						GROUP_PADDING +
						row * (maxMemberHeight + GROUP_PADDING),
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
					gradientStart: agentInfoMap.get(agentId)?.gradient_start ?? null,
					gradientEnd: agentInfoMap.get(agentId)?.gradient_end ?? null,
					avatarUrl: api.agentAvatarUrl(agentId),
					isOnline,
					channelCount: summary?.channel_count ?? 0,
					memoryCount: summary?.memory_total ?? 0,
					connectedHandles: {
						top: false,
						bottom: false,
						left: false,
						right: false,
					},
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
			new Date(summary.last_activity_at).getTime() > Date.now() - 5 * 60 * 1000;

		allNodes.push({
			id: agent.id,
			type: "agent",
			position:
				count === 1
					? {x: ungroupedStartX, y: 100}
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
				gradientStart: agentInfoMap.get(agent.id)?.gradient_start ?? null,
				gradientEnd: agentInfoMap.get(agent.id)?.gradient_end ?? null,
				avatarUrl: api.agentAvatarUrl(agent.id),
				isOnline,
				channelCount: summary?.channel_count ?? 0,
				memoryCount: summary?.memory_total ?? 0,
				connectedHandles: {
					top: false,
					bottom: false,
					left: false,
					right: false,
				},
			},
		});
	});

	// Add human nodes
	const humans = data.humans ?? [];
	const humanStartX =
		ungroupedAgents.length > 0
			? centerX + radius + NODE_WIDTH + 80
			: ungroupedStartX;

	humans.forEach((human, index) => {
		allNodes.push({
			id: human.id,
			type: "human",
			position: {x: humanStartX, y: index * 220},
			data: {
				nodeId: human.id,
				nodeKind: "human",
				configDisplayName: human.display_name ?? null,
				configRole: human.role ?? null,
				bio: human.bio ?? null,
				description: human.description ?? null,
				avatarSeed: human.id,
				connectedHandles: {
					top: false,
					bottom: false,
					left: false,
					right: false,
				},
			},
		});
	});

	// Build absolute position lookup for handle routing
	const nodePositionMap = new Map<string, {x: number; y: number}>();
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
		const edgeId = `${link.from}->${link.to}`;
		const {sourceHandle, targetHandle} = getHandlesForKind(
			link.kind,
			nodePositionMap.get(link.from),
			nodePositionMap.get(link.to),
			savedHandles[edgeId],
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
		const {sourceHandle, targetHandle} = getHandlesForKind(
			link.kind,
			nodePositionMap.get(link.from),
			nodePositionMap.get(link.to),
			savedHandles[edgeId],
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

	return {initialNodes: allNodes, initialEdges};
}
