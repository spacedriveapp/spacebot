import {useState} from "react";
import {useMutation, useQueryClient} from "@tanstack/react-query";
import {useNavigate} from "@tanstack/react-router";
import {api} from "@/api/client";
import {Button, Input, DialogRoot, DialogContent, DialogHeader, DialogTitle, DialogFooter} from "@spacedrive/primitives";

interface DeleteAgentDialogProps {
	open: boolean;
	onOpenChange: (open: boolean) => void;
	agentId: string;
}

export function DeleteAgentDialog({open, onOpenChange, agentId}: DeleteAgentDialogProps) {
	const [confirmation, setConfirmation] = useState("");
	const [error, setError] = useState<string | null>(null);
	const queryClient = useQueryClient();
	const navigate = useNavigate();

	const deleteMutation = useMutation({
		mutationFn: () => api.deleteAgent(agentId),
		onSuccess: (result) => {
			if (result.success) {
				queryClient.invalidateQueries({queryKey: ["agents"]});
				queryClient.invalidateQueries({queryKey: ["overview"]});
				onOpenChange(false);
				setConfirmation("");
				setError(null);
				navigate({to: "/"});
			} else {
				setError(result.message);
			}
		},
		onError: (err) => setError(`Failed: ${err.message}`),
	});

	const confirmed = confirmation === agentId;

	function handleSubmit() {
		if (!confirmed) return;
		setError(null);
		deleteMutation.mutate();
	}

	return (
		<DialogRoot open={open} onOpenChange={(v) => { if (!v) { setError(null); setConfirmation(""); } onOpenChange(v); }}>
			<DialogContent className="max-w-sm">
				<DialogHeader>
					<DialogTitle>Delete Agent</DialogTitle>
				</DialogHeader>
				<div className="flex flex-col gap-3">
					<p className="text-sm text-ink-dull">
						This will remove <span className="font-medium text-ink">{agentId}</span> from
						your configuration. The agent's data directory will not be deleted.
					</p>
					<div>
						<label className="mb-1.5 block text-sm font-medium text-ink-dull">
							Type <span className="font-mono text-ink">{agentId}</span> to confirm
						</label>
						<Input
							size="lg"
							value={confirmation}
							onChange={(e) => setConfirmation(e.target.value)}
							placeholder={agentId}
							onKeyDown={(e) => { if (e.key === "Enter") handleSubmit(); }}
							autoFocus
						/>
					</div>
					{error && (
						<div className="rounded-md border border-red-500/20 bg-red-500/10 px-3 py-2 text-sm text-red-400">
							{error}
						</div>
					)}
				</div>
				<DialogFooter>
					<Button variant="bare" size="sm" onClick={() => onOpenChange(false)}>
						Cancel
					</Button>
					<Button
						size="sm"
						variant="accent"
						onClick={handleSubmit}
						loading={deleteMutation.isPending}
						disabled={!confirmed}
					>
						Delete Agent
					</Button>
				</DialogFooter>
			</DialogContent>
		</DialogRoot>
	);
}
