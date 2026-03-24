import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/api/client";
import { TaskBoard } from "@/components/TaskBoard";

export function GlobalTasks() {
  const { data: agentsData } = useQuery({
    queryKey: ["agents"],
    queryFn: api.agents,
    staleTime: 10_000,
  });

  const agents = agentsData?.agents ?? [];
  const [selectedOwnerId, setSelectedOwnerId] = useState<string | undefined>();

  // Resolve which agent owns newly created tasks. Explicit selection takes
  // priority, then fall back to the first agent.
  const effectiveOwner = selectedOwnerId ?? agents[0]?.id;

  const agentNames = useMemo(() => {
    const map: Record<string, string | undefined> = {};
    for (const agent of agents) {
      map[agent.id] = agent.display_name;
    }
    return map;
  }, [agents]);

  return (
    <div className="flex h-full flex-col">
      {agents.length > 1 && (
        <div className="flex items-center gap-2 border-b border-app-line px-4 py-1.5">
          <label className="text-xs text-ink-dull">Create tasks as:</label>
          <select
            className="rounded-md border border-app-line bg-app-darkBox px-2 py-1 text-xs text-ink focus:border-accent focus:outline-none"
            value={effectiveOwner ?? ""}
            onChange={(e) => setSelectedOwnerId(e.target.value || undefined)}
          >
            {agents.map((agent) => (
              <option key={agent.id} value={agent.id}>
                {agent.display_name ?? agent.id}
              </option>
            ))}
          </select>
        </div>
      )}
      <div className="flex-1 overflow-hidden">
        <TaskBoard
          ownerAgentId={effectiveOwner}
          agentNames={agentNames}
          showAgentBadge={agents.length > 1}
        />
      </div>
    </div>
  );
}
