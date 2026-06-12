import {useState, useEffect} from "react";
import {useMutation, useQueryClient} from "@tanstack/react-query";
import {api} from "@/api/client";
import {
	Button,
	Input,
	SelectRoot,
	SelectTrigger,
	SelectValue,
	SelectContent,
	SelectItem,
} from "@spacedrive/primitives";
import type {GlobalSettingsSectionProps} from "./types";
import {PERMISSION_OPTIONS} from "./constants";

export function OpenCodeSection({settings, isLoading}: GlobalSettingsSectionProps) {
	const queryClient = useQueryClient();
	const [enabled, setEnabled] = useState(settings?.opencode?.enabled ?? false);
	const [path, setPath] = useState(settings?.opencode?.path ?? "opencode");
	const [maxServers, setMaxServers] = useState(
		settings?.opencode?.max_servers?.toString() ?? "5",
	);
	const [startupTimeout, setStartupTimeout] = useState(
		settings?.opencode?.server_startup_timeout_secs?.toString() ?? "30",
	);
	const [maxRetries, setMaxRetries] = useState(
		settings?.opencode?.max_restart_retries?.toString() ?? "5",
	);
	const [editPerm, setEditPerm] = useState(
		settings?.opencode?.permissions?.edit ?? "allow",
	);
	const [bashPerm, setBashPerm] = useState(
		settings?.opencode?.permissions?.bash ?? "allow",
	);
	const [webfetchPerm, setWebfetchPerm] = useState(
		settings?.opencode?.permissions?.webfetch ?? "allow",
	);
	const [message, setMessage] = useState<{
		text: string;
		type: "success" | "error";
	} | null>(null);

	useEffect(() => {
		if (settings?.opencode) {
			setEnabled(settings.opencode.enabled);
			setPath(settings.opencode.path);
			setMaxServers(settings.opencode.max_servers.toString());
			setStartupTimeout(
				settings.opencode.server_startup_timeout_secs.toString(),
			);
			setMaxRetries(settings.opencode.max_restart_retries.toString());
			setEditPerm(settings.opencode.permissions.edit);
			setBashPerm(settings.opencode.permissions.bash);
			setWebfetchPerm(settings.opencode.permissions.webfetch);
		}
	}, [settings?.opencode]);

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
		const servers = parseInt(maxServers, 10);
		if (isNaN(servers) || servers < 1) {
			setMessage({text: "Max servers must be at least 1", type: "error"});
			return;
		}
		const timeout = parseInt(startupTimeout, 10);
		if (isNaN(timeout) || timeout < 1) {
			setMessage({text: "Startup timeout must be at least 1", type: "error"});
			return;
		}
		const retries = parseInt(maxRetries, 10);
		if (isNaN(retries) || retries < 0) {
			setMessage({text: "Max retries cannot be negative", type: "error"});
			return;
		}

		updateMutation.mutate({
			opencode: {
				enabled,
				path: path.trim() || "opencode",
				max_servers: servers,
				server_startup_timeout_secs: timeout,
				max_restart_retries: retries,
				permissions: {
					edit: editPerm,
					bash: bashPerm,
					webfetch: webfetchPerm,
				},
			},
		});
	};

	return (
		<div className="mx-auto max-w-2xl px-6 py-6">
			<div className="mb-6">
				<h2 className="font-plex text-sm font-semibold text-ink">
					OpenCode Workers
				</h2>
				<p className="mt-1 text-sm text-ink-dull">
					Spawn{" "}
					<a
						href="https://opencode.ai"
						target="_blank"
						rel="noopener noreferrer"
						className="text-accent hover:underline"
					>
						OpenCode
					</a>{" "}
					coding agents as worker subprocesses. Requires the{" "}
					<code className="rounded bg-app-box px-1 py-0.5 text-tiny text-ink-dull">
						opencode
					</code>{" "}
					binary on PATH or a custom path below.
				</p>
			</div>

			{isLoading ? (
				<div className="flex items-center gap-2 text-ink-dull">
					<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
					Loading settings...
				</div>
			) : (
				<div className="flex flex-col gap-4">
					{/* Enable toggle */}
					<div className="rounded-lg border border-app-line bg-app-box p-4">
						<label className="flex items-center gap-3">
							<input
								type="checkbox"
								checked={enabled}
								onChange={(e) => setEnabled(e.target.checked)}
								className="h-4 w-4"
							/>
							<div>
								<span className="text-sm font-medium text-ink">
									Enable OpenCode Workers
								</span>
								<p className="mt-0.5 text-sm text-ink-dull">
									Allow agents to spawn OpenCode coding sessions
								</p>
							</div>
						</label>
					</div>

					{enabled && (
						<>
							{/* Binary path */}
							<div className="rounded-lg border border-app-line bg-app-box p-4">
								<label className="block">
									<span className="text-sm font-medium text-ink">
										Binary Path
									</span>
									<p className="mt-0.5 text-sm text-ink-dull">
										Path to the OpenCode binary, or just the name if it's on
										PATH
									</p>
									<Input
										type="text"
										value={path}
										onChange={(e) => setPath(e.target.value)}
										placeholder="opencode"
										className="mt-2"
									/>
								</label>
							</div>

							{/* Pool settings */}
							<div className="rounded-lg border border-app-line bg-app-box p-4">
								<span className="text-sm font-medium text-ink">
									Server Pool
								</span>
								<p className="mt-0.5 text-sm text-ink-dull">
									Controls how many OpenCode server processes can run
									concurrently
								</p>
								<div className="mt-3 grid grid-cols-3 gap-3">
									<label className="block">
										<span className="text-tiny font-medium text-ink-dull">
											Max Servers
										</span>
										<Input
											type="number"
											value={maxServers}
											onChange={(e) => setMaxServers(e.target.value)}
											min="1"
											max="20"
											className="mt-1"
										/>
									</label>
									<label className="block">
										<span className="text-tiny font-medium text-ink-dull">
											Startup Timeout (s)
										</span>
										<Input
											type="number"
											value={startupTimeout}
											onChange={(e) => setStartupTimeout(e.target.value)}
											min="1"
											className="mt-1"
										/>
									</label>
									<label className="block">
										<span className="text-tiny font-medium text-ink-dull">
											Max Retries
										</span>
										<Input
											type="number"
											value={maxRetries}
											onChange={(e) => setMaxRetries(e.target.value)}
											min="0"
											className="mt-1"
										/>
									</label>
								</div>
							</div>

							{/* Permissions */}
							<div className="rounded-lg border border-app-line bg-app-box p-4">
								<span className="text-sm font-medium text-ink">
									Permissions
								</span>
								<p className="mt-0.5 text-sm text-ink-dull">
									Control which tools OpenCode workers can use
								</p>
								<div className="mt-3 flex flex-col gap-3">
									{(
										[
											{
												label: "File Edit",
												value: editPerm,
												setter: setEditPerm,
											},
											{
												label: "Shell / Bash",
												value: bashPerm,
												setter: setBashPerm,
											},
											{
												label: "Web Fetch",
												value: webfetchPerm,
												setter: setWebfetchPerm,
											},
										] as const
									).map(({label, value, setter}) => (
										<div
											key={label}
											className="flex items-center justify-between"
										>
											<span className="text-sm text-ink">{label}</span>
											<SelectRoot
												value={value}
												onValueChange={(v) => setter(v)}
											>
												<SelectTrigger className="w-28">
													<SelectValue />
												</SelectTrigger>
												<SelectContent>
													{PERMISSION_OPTIONS.map((opt) => (
														<SelectItem key={opt.value} value={opt.value}>
															{opt.label}
														</SelectItem>
													))}
												</SelectContent>
											</SelectRoot>
										</div>
									))}
								</div>
							</div>
						</>
					)}

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
