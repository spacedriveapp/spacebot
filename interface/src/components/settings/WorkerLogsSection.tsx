import {useState, useEffect} from "react";
import {useMutation, useQueryClient} from "@tanstack/react-query";
import {api} from "@/api/client";
import {Button} from "@spacedrive/primitives";
import type {GlobalSettingsSectionProps} from "./types";

export function WorkerLogsSection({settings, isLoading}: GlobalSettingsSectionProps) {
	const queryClient = useQueryClient();
	const [logMode, setLogMode] = useState(
		settings?.worker_log_mode ?? "errors_only",
	);
	const [message, setMessage] = useState<{
		text: string;
		type: "success" | "error";
	} | null>(null);

	// Update form state when settings load
	useEffect(() => {
		if (settings) {
			setLogMode(settings.worker_log_mode);
		}
	}, [settings]);

	const updateMutation = useMutation({
		mutationFn: api.updateGlobalSettings,
		onSuccess: (result) => {
			if (result.success) {
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

	const handleSave = () => {
		updateMutation.mutate({worker_log_mode: logMode});
	};

	const modes = [
		{
			value: "errors_only",
			label: "Errors Only",
			description: "Only log failed worker runs (saves disk space)",
		},
		{
			value: "all_separate",
			label: "All (Separate)",
			description: "Log all runs with separate directories for success/failure",
		},
		{
			value: "all_combined",
			label: "All (Combined)",
			description: "Log all runs to the same directory",
		},
	];

	return (
		<div className="mx-auto max-w-2xl px-6 py-6">
			<div className="mb-6">
				<h2 className="font-plex text-sm font-semibold text-ink">
					Worker Execution Logs
				</h2>
				<p className="mt-1 text-sm text-ink-dull">
					Control how worker execution logs are stored in the logs directory.
				</p>
			</div>

			{isLoading ? (
				<div className="flex items-center gap-2 text-ink-dull">
					<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
					Loading settings...
				</div>
			) : (
				<div className="flex flex-col gap-4">
					<div className="flex flex-col gap-3">
						{modes.map((mode) => (
							<div
								key={mode.value}
								className={`rounded-lg border p-4 cursor-pointer transition-colors ${
									logMode === mode.value
										? "border-accent bg-accent/5"
										: "border-app-line bg-app-box hover:border-app-line/80"
								}`}
								onClick={() => setLogMode(mode.value)}
							>
								<label className="flex items-start gap-3 cursor-pointer">
									<input
										type="radio"
										value={mode.value}
										checked={logMode === mode.value}
										onChange={(e) => setLogMode(e.target.value)}
										className="mt-0.5"
									/>
									<div className="flex-1">
										<span className="text-sm font-medium text-ink">
											{mode.label}
										</span>
										<p className="mt-0.5 text-sm text-ink-dull">
											{mode.description}
										</p>
									</div>
								</label>
							</div>
						))}
					</div>

					<Button onClick={handleSave} loading={updateMutation.isPending}>
						Save Changes
					</Button>
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
		</div>
	);
}
