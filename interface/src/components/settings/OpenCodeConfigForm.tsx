import {useState, useEffect} from "react";
import {useMutation, useQueryClient} from "@tanstack/react-query";
import {api} from "@/api/client";
import type {IntegrationEntry} from "@/api/client";
import {
	Button,
	Input,
	SelectRoot,
	SelectTrigger,
	SelectValue,
	SelectContent,
	SelectItem,
} from "@spacedrive/primitives";
import {PERMISSION_OPTIONS} from "./constants";

export function OpenCodeConfigForm({
	integration,
	onSaved,
}: {
	integration: IntegrationEntry;
	onSaved: () => void;
}) {
	const queryClient = useQueryClient();
	const config = integration.config as Record<string, any>;
	const perms = (config.permissions ?? {}) as Record<string, string>;

	const [path, setPath] = useState((config.path as string) ?? "opencode");
	const [maxServers, setMaxServers] = useState(
		String(config.max_servers ?? 5),
	);
	const [startupTimeout, setStartupTimeout] = useState(
		String(config.server_startup_timeout_secs ?? 30),
	);
	const [maxRetries, setMaxRetries] = useState(
		String(config.max_restart_retries ?? 5),
	);
	const [editPerm, setEditPerm] = useState(perms.edit ?? "allow");
	const [bashPerm, setBashPerm] = useState(perms.bash ?? "allow");
	const [webfetchPerm, setWebfetchPerm] = useState(perms.webfetch ?? "allow");
	const [message, setMessage] = useState<{
		text: string;
		type: "success" | "error";
	} | null>(null);

	useEffect(() => {
		setPath((config.path as string) ?? "opencode");
		setMaxServers(String(config.max_servers ?? 5));
		setStartupTimeout(String(config.server_startup_timeout_secs ?? 30));
		setMaxRetries(String(config.max_restart_retries ?? 5));
		setEditPerm(perms.edit ?? "allow");
		setBashPerm(perms.bash ?? "allow");
		setWebfetchPerm(perms.webfetch ?? "allow");
	}, [integration]);

	const mutation = useMutation({
		mutationFn: (cfg: Record<string, unknown>) =>
			api.updateIntegration("opencode", cfg),
		onSuccess: (result) => {
			if (result.success) {
				setMessage({text: result.message, type: "success"});
				queryClient.invalidateQueries({queryKey: ["integrations"]});
				queryClient.invalidateQueries({queryKey: ["global-settings"]});
				onSaved();
			} else {
				setMessage({text: result.message, type: "error"});
			}
		},
		onError: (error) =>
			setMessage({text: `Failed: ${error.message}`, type: "error"}),
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

		mutation.mutate({
			enabled: integration.enabled,
			path: path.trim() || "opencode",
			max_servers: servers,
			server_startup_timeout_secs: timeout,
			max_restart_retries: retries,
			permissions: {
				edit: editPerm,
				bash: bashPerm,
				webfetch: webfetchPerm,
			},
		});
	};

	return (
		<div className="flex flex-col gap-3 pt-3">
			<p className="text-sm text-ink-dull">
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

			{/* Binary path */}
			<label className="block">
				<span className="text-tiny font-medium text-ink-dull">
					Binary Path
				</span>
				<Input
					type="text"
					value={path}
					onChange={(e) => setPath(e.target.value)}
					placeholder="opencode"
					className="mt-1"
				/>
			</label>

			{/* Pool settings */}
			<div>
				<span className="text-tiny font-medium text-ink-dull">
					Server Pool
				</span>
				<div className="mt-1 grid grid-cols-3 gap-3">
					<label className="block">
						<span className="text-tiny text-ink-faint">Max Servers</span>
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
						<span className="text-tiny text-ink-faint">Timeout (s)</span>
						<Input
							type="number"
							value={startupTimeout}
							onChange={(e) => setStartupTimeout(e.target.value)}
							min="1"
							className="mt-1"
						/>
					</label>
					<label className="block">
						<span className="text-tiny text-ink-faint">Max Retries</span>
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
			<div>
				<span className="text-tiny font-medium text-ink-dull">
					Permissions
				</span>
				<div className="mt-1 flex flex-col gap-2">
					{(
						[
							{label: "File Edit", value: editPerm, setter: setEditPerm},
							{label: "Shell / Bash", value: bashPerm, setter: setBashPerm},
							{
								label: "Web Fetch",
								value: webfetchPerm,
								setter: setWebfetchPerm,
							},
						] as const
					).map(({label, value, setter}) => (
						<div key={label} className="flex items-center justify-between">
							<span className="text-sm text-ink">{label}</span>
							<SelectRoot value={value} onValueChange={(v) => setter(v)}>
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

			<Button onClick={handleSave} loading={mutation.isPending} size="sm">
				Save
			</Button>

			{message && (
				<div
					className={`rounded-md border px-3 py-2 text-sm ${
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
