import {useState, useEffect} from "react";
import {useMutation, useQueryClient} from "@tanstack/react-query";
import {api} from "@/api/client";
import {Button, Input, Switch} from "@spacedrive/primitives";
import type {GlobalSettingsSectionProps} from "./types";

export function ServerSection({settings, isLoading}: GlobalSettingsSectionProps) {
	const queryClient = useQueryClient();
	const [apiEnabled, setApiEnabled] = useState(settings?.api_enabled ?? true);
	const [apiPort, setApiPort] = useState(
		settings?.api_port.toString() ?? "19898",
	);
	const [apiBind, setApiBind] = useState(settings?.api_bind ?? "127.0.0.1");
	const [message, setMessage] = useState<{
		text: string;
		type: "success" | "error";
		requiresRestart?: boolean;
	} | null>(null);

	// Update form state when settings load
	useEffect(() => {
		if (settings) {
			setApiEnabled(settings.api_enabled);
			setApiPort(settings.api_port.toString());
			setApiBind(settings.api_bind);
		}
	}, [settings]);

	const updateMutation = useMutation({
		mutationFn: api.updateGlobalSettings,
		onSuccess: (result) => {
			if (result.success) {
				setMessage({
					text: result.message,
					type: "success",
					requiresRestart: result.requires_restart,
				});
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
		const port = parseInt(apiPort, 10);
		if (isNaN(port) || port < 1024 || port > 65535) {
			setMessage({text: "Port must be between 1024 and 65535", type: "error"});
			return;
		}

		updateMutation.mutate({
			api_enabled: apiEnabled,
			api_port: port,
			api_bind: apiBind.trim(),
		});
	};

	return (
		<div className="mx-auto max-w-2xl px-6 py-6">
			<div className="mb-6">
				<h2 className="font-plex text-sm font-semibold text-ink">
					API Server Configuration
				</h2>
				<p className="mt-1 text-sm text-ink-dull">
					Configure the HTTP API server. Changes require a restart to take
					effect.
				</p>
			</div>

			{isLoading ? (
				<div className="flex items-center gap-2 text-ink-dull">
					<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
					Loading settings...
				</div>
			) : (
				<div className="flex flex-col gap-4">
					<div className="rounded-lg border border-app-line bg-app-box p-4">
						<div className="flex items-center justify-between">
							<div>
								<span className="text-sm font-medium text-ink">
									Enable API Server
								</span>
								<p className="mt-0.5 text-sm text-ink-dull">
									Disable to prevent the HTTP API from starting
								</p>
							</div>
							<Switch
								size="md"
								checked={apiEnabled}
								onCheckedChange={setApiEnabled}
							/>
						</div>
					</div>

					<div className="rounded-lg border border-app-line bg-app-box p-4">
						<label className="block">
							<span className="text-sm font-medium text-ink">Port</span>
							<p className="mt-0.5 text-sm text-ink-dull">
								Port number for the API server
							</p>
							<Input
								type="number"
								value={apiPort}
								onChange={(e) => setApiPort(e.target.value)}
								min="1024"
								max="65535"
								className="mt-2"
							/>
						</label>
					</div>

					<div className="rounded-lg border border-app-line bg-app-box p-4">
						<label className="block">
							<span className="text-sm font-medium text-ink">Bind Address</span>
							<p className="mt-0.5 text-sm text-ink-dull">
								IP address to bind to (127.0.0.1 for local, 0.0.0.0 for all
								interfaces)
							</p>
							<Input
								type="text"
								value={apiBind}
								onChange={(e) => setApiBind(e.target.value)}
								placeholder="127.0.0.1"
								className="mt-2"
							/>
						</label>
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
					{message.requiresRestart && (
						<div className="mt-1 text-yellow-400">
							⚠️ Restart required for changes to take effect
						</div>
					)}
				</div>
			)}
		</div>
	);
}
