import {useState} from "react";
import {useMutation, useQueryClient} from "@tanstack/react-query";
import {api} from "@/api/client";
import {
	Button,
	Input,
	DialogRoot,
	DialogContent,
	DialogHeader,
	DialogTitle,
	DialogDescription,
	DialogFooter,
} from "@spacedrive/primitives";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {faSearch} from "@fortawesome/free-solid-svg-icons";
import type {GlobalSettingsSectionProps} from "./types";

export function ApiKeysSection({settings, isLoading}: GlobalSettingsSectionProps) {
	const queryClient = useQueryClient();
	const [editingBraveKey, setEditingBraveKey] = useState(false);
	const [braveKeyInput, setBraveKeyInput] = useState("");
	const [message, setMessage] = useState<{
		text: string;
		type: "success" | "error";
	} | null>(null);

	const updateMutation = useMutation({
		mutationFn: api.updateGlobalSettings,
		onSuccess: (result) => {
			if (result.success) {
				setEditingBraveKey(false);
				setBraveKeyInput("");
				setMessage({text: result.message, type: "success"});
				queryClient.invalidateQueries({queryKey: ["global-settings"]});
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) => {
			setMessage({text: `Failed: ${error.message}`, type: "error"});
		},
	});

	const handleSaveBraveKey = () => {
		updateMutation.mutate({brave_search_key: braveKeyInput.trim() || null});
	};

	const handleRemoveBraveKey = () => {
		updateMutation.mutate({brave_search_key: null});
	};

	return (
		<div className="mx-auto max-w-2xl px-6 py-6">
			<div className="mb-6">
				<h2 className="font-plex text-sm font-semibold text-ink">
					Third-Party API Keys
				</h2>
				<p className="mt-1 text-sm text-ink-dull">
					Configure API keys for third-party services used by workers.
				</p>
			</div>

			{isLoading ? (
				<div className="flex items-center gap-2 text-ink-dull">
					<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
					Loading settings...
				</div>
			) : (
				<div className="flex flex-col gap-3">
					<div className="rounded-lg border border-app-line bg-app-box p-4">
						<div className="flex items-center gap-3">
							<FontAwesomeIcon icon={faSearch} className="text-ink-faint" />
							<div className="flex-1">
								<div className="flex items-center gap-2">
									<span className="text-sm font-medium text-ink">
										Brave Search
									</span>
									{settings?.brave_search_key && (
										<span className="text-tiny text-green-400">
											● Configured
										</span>
									)}
								</div>
								<p className="mt-0.5 text-sm text-ink-dull">
									Powers web search capabilities for workers
								</p>
							</div>
							<div className="flex gap-2">
								<Button
									onClick={() => {
										setEditingBraveKey(true);
										setBraveKeyInput(settings?.brave_search_key || "");
										setMessage(null);
									}}
									variant="outline"
									size="md"
								>
									{settings?.brave_search_key ? "Update" : "Add key"}
								</Button>
								{settings?.brave_search_key && (
									<Button
										onClick={handleRemoveBraveKey}
										variant="outline"
										size="md"
										loading={updateMutation.isPending}
									>
										Remove
									</Button>
								)}
							</div>
						</div>
					</div>
				</div>
			)}

			{message && (
				<div
					className={`mt-4 rounded-md border px-3 py-2 text-sm ${
						message.type === "success"
							? "border-green-500/20 bg-green-500/10 text-green-400"
							: "border-red-500/20 bg-red-500/10 text-red-400"
					}`}
				>
					{message.text}
				</div>
			)}

			<DialogRoot
				open={editingBraveKey}
				onOpenChange={(open) => {
					if (!open) setEditingBraveKey(false);
				}}
			>
				<DialogContent className="max-w-md">
					<DialogHeader>
						<DialogTitle>
							{settings?.brave_search_key ? "Update" : "Add"} Brave Search Key
						</DialogTitle>
						<DialogDescription>
							Enter your Brave Search API key. Get one at brave.com/search/api
						</DialogDescription>
					</DialogHeader>
					<Input
						type="password"
						value={braveKeyInput}
						onChange={(e) => setBraveKeyInput(e.target.value)}
						placeholder="BSA..."
						autoFocus
						onKeyDown={(e) => {
							if (e.key === "Enter") handleSaveBraveKey();
						}}
					/>
					<DialogFooter>
						<Button
							onClick={() => setEditingBraveKey(false)}
							variant="outline"
							size="md"
						>
							Cancel
						</Button>
						<Button
							onClick={handleSaveBraveKey}
							disabled={!braveKeyInput.trim()}
							loading={updateMutation.isPending}
							size="md"
						>
							Save
						</Button>
					</DialogFooter>
				</DialogContent>
			</DialogRoot>
		</div>
	);
}
