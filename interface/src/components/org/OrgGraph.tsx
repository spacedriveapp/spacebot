import {ReactFlowProvider} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import type {AgentSummary} from "@/api/client";
import {OrgGraphInner} from "./OrgGraphInner";

export interface OrgGraphProps {
	activeEdges?: Set<string>;
	agents?: AgentSummary[];
}

export function OrgGraph({activeEdges, agents}: OrgGraphProps) {
	const edges = activeEdges ?? new Set<string>();
	const agentList = agents ?? [];
	return (
		<ReactFlowProvider>
			<OrgGraphInner activeEdges={edges} agents={agentList} />
		</ReactFlowProvider>
	);
}
