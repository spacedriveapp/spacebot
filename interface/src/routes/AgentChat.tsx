import { PortalPanel } from "@/components/portal";

export function AgentChat({ agentId }: { agentId: string }) {
	return <PortalPanel agentId={agentId} />;
}
