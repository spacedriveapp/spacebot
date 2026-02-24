import {useState} from "react";
import {useMutation, useQueryClient} from "@tanstack/react-query";
import {useNavigate} from "@tanstack/react-router";
import {api} from "@/api/client";
import {Button, Input, Dialog, DialogContent, DialogHeader, DialogTitle, DialogFooter} from "@/ui";

interface CreateAgentDialogProps {
	open: boolean;
	onOpenChange: (open: boolean) => void;
}

export function CreateAgentDialog({open, onOpenChange}: CreateAgentDialogProps) {
	const [agentId, setAgentId] = useState("");
	const [displayName, setDisplayName] = useState("");
	const [role, setRole] = useState("");
	const [error, setError] = useState<string | null>(null);
	const queryClient = useQueryClient();
	const navigate = useNavigate();

	const createMutation = useMutation({
		mutationFn: (params: { id: string; displayName: string; role: string }) =>
			api.createAgent(params.id, params.displayName, params.role),
		onSuccess: (result) => {
			if (result.success) {
				queryClient.invalidateQueries({queryKey: ["agents"]});
				queryClient.invalidateQueries({queryKey: ["overview"]});
				queryClient.invalidateQueries({queryKey: ["topology"]});
				onOpenChange(false);
				setAgentId("");
				setDisplayName("");
				setRole("");
				setError(null);
				navigate({to: "/agents/$agentId", params: {agentId: result.agent_id}});
			} else {
				setError(result.message);
			}
		},
		onError: (err) => setError(`Failed: ${err.message}`),
	});

	function handleSubmit() {
		const trimmed = agentId.trim().toLowerCase().replace(/[^a-z0-9_-]/g, "");
		if (!trimmed) {
			setError("Agent ID is required");
			return;
		}
		setError(null);
		createMutation.mutate({
			id: trimmed,
			displayName: displayName.trim(),
			role: role.trim(),
		});
	}

	function handleClose() {
		setError(null);
		setAgentId("");
		setDisplayName("");
		setRole("");
	}

	return (
		<Dialog open={open} onOpenChange={(v) => { if (!v) handleClose(); onOpenChange(v); }}>
			<DialogContent className="max-w-sm">
				<DialogHeader>
					<DialogTitle>Create Agent</DialogTitle>
				</DialogHeader>
				<div className="flex flex-col gap-3">
					<div>
						<label className="mb-1.5 block text-sm font-medium text-ink-dull">Agent ID</label>
						<Input
							size="lg"
							value={agentId}
							onChange={(e) => setAgentId(e.target.value)}
							placeholder="e.g. research, support, dev"
							onKeyDown={(e) => { if (e.key === "Enter") handleSubmit(); }}
							autoFocus
						/>
						<p className="mt-1.5 text-tiny text-ink-faint">
							Lowercase letters, numbers, hyphens, and underscores only.
						</p>
					</div>
					<div>
						<label className="mb-1.5 block text-sm font-medium text-ink-dull">
							Display Name
							<span className="ml-1 font-normal text-ink-faint">optional</span>
						</label>
						<Input
							size="lg"
							value={displayName}
							onChange={(e) => setDisplayName(e.target.value)}
							placeholder="e.g. Research Agent"
							onKeyDown={(e) => { if (e.key === "Enter") handleSubmit(); }}
						/>
					</div>
					<div>
						<label className="mb-1.5 block text-sm font-medium text-ink-dull">
							Role
							<span className="ml-1 font-normal text-ink-faint">optional</span>
						</label>
						<Input
							size="lg"
							value={role}
							onChange={(e) => setRole(e.target.value)}
							placeholder="e.g. Handles tier 1 support tickets"
							onKeyDown={(e) => { if (e.key === "Enter") handleSubmit(); }}
						/>
					</div>
					{error && (
						<div className="rounded-md border border-red-500/20 bg-red-500/10 px-3 py-2 text-sm text-red-400">
							{error}
						</div>
					)}
				</div>
				<DialogFooter>
					<Button variant="ghost" size="sm" onClick={() => onOpenChange(false)}>
						Cancel
					</Button>
					<Button size="sm" onClick={handleSubmit} loading={createMutation.isPending}>
						Create
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}
