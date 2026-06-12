import {useRef, useState} from "react";
import {
	Button,
	Input,
	DialogRoot,
	DialogContent,
	DialogHeader,
	DialogTitle,
	DialogFooter,
} from "@spacedrive/primitives";
import type {TopologyAgent} from "@/api/client";

interface AgentEditDialogProps {
	agent: TopologyAgent | null;
	open: boolean;
	onOpenChange: (open: boolean) => void;
	onUpdate: (displayName: string, role: string) => void;
}

export function AgentEditDialog({
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
		<DialogRoot open={open} onOpenChange={onOpenChange}>
			<DialogContent className="max-w-sm">
				<DialogHeader>
					<DialogTitle>Edit Agent</DialogTitle>
				</DialogHeader>
				<div className="flex flex-col gap-3">
					<div className="text-tiny text-ink-faint">{agent.id}</div>
					<div>
						<label className="mb-1.5 block text-sm font-medium text-ink-dull">
							Display Name
						</label>
						<Input
							size="lg"
							value={displayName}
							onChange={(e) => setDisplayName(e.target.value)}
							placeholder={agent.id}
						/>
					</div>
					<div>
						<label className="mb-1.5 block text-sm font-medium text-ink-dull">
							Role
						</label>
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
					<Button variant="bare" size="sm" onClick={() => onOpenChange(false)}>
						Cancel
					</Button>
					<Button size="sm" onClick={() => onUpdate(displayName, role)}>
						Save
					</Button>
				</DialogFooter>
			</DialogContent>
		</DialogRoot>
	);
}
