import { TaskBoard } from "@/components/TaskBoard";

export function AgentTasks({ agentId }: { agentId: string }) {
  return <TaskBoard agentId={agentId} ownerAgentId={agentId} />;
}
