import {useState} from "react";
import {useQuery, useMutation, useQueryClient} from "@tanstack/react-query";
import {api, type UpdateStatus} from "@/api/client";
import {Button} from "@spacedrive/primitives";

function formatCheckedAt(checkedAt: string | null): string {
	if (!checkedAt) return "Never";
	const timestamp = new Date(checkedAt);
	if (Number.isNaN(timestamp.getTime())) return checkedAt;
	return timestamp.toLocaleString();
}

function pullableDockerImage(image: string | null): string {
	if (!image) return "ghcr.io/spacedriveapp/spacebot:latest";
	return image.split("@")[0] ?? image;
}

export function UpdatesSection() {
	const queryClient = useQueryClient();
	const [message, setMessage] = useState<{
		text: string;
		type: "success" | "error";
	} | null>(null);
	const [copiedBlock, setCopiedBlock] = useState<string | null>(null);

	const {data, isLoading, isFetching} = useQuery<UpdateStatus>({
		queryKey: ["update-check"],
		queryFn: api.updateCheck,
		staleTime: 30_000,
		refetchInterval: 300_000,
	});

	const checkNowMutation = useMutation({
		mutationFn: api.updateCheckNow,
		onSuccess: (status) => {
			queryClient.setQueryData(["update-check"], status);
			if (status.update_available && status.latest_version) {
				setMessage({
					text: `Update ${status.latest_version} is available.`,
					type: "success",
				});
			} else {
				setMessage({text: "No newer release found.", type: "success"});
			}
		},
		onError: (error) => {
			setMessage({
				text: `Failed to check updates: ${error.message}`,
				type: "error",
			});
		},
	});

	const applyMutation = useMutation({
		mutationFn: api.updateApply,
		onSuccess: (result) => {
			if (result.status === "updating") {
				setMessage({
					text: "Applying update. This instance will restart in a few seconds.",
					type: "success",
				});
				setTimeout(() => {
					queryClient.invalidateQueries({queryKey: ["update-check"]});
				}, 3000);
				return;
			}

			setMessage({text: result.error ?? "Update failed", type: "error"});
		},
		onError: (error) => {
			setMessage({
				text: `Failed to apply update: ${error.message}`,
				type: "error",
			});
		},
	});

	const handleCopy = async (label: string, content: string) => {
		try {
			if (navigator.clipboard?.writeText) {
				await navigator.clipboard.writeText(content);
			} else {
				const textarea = document.createElement("textarea");
				textarea.value = content;
				textarea.setAttribute("readonly", "");
				textarea.style.position = "absolute";
				textarea.style.left = "-9999px";
				document.body.appendChild(textarea);
				textarea.select();
				document.execCommand("copy");
				document.body.removeChild(textarea);
			}
			setCopiedBlock(label);
			setTimeout(
				() => setCopiedBlock((current) => (current === label ? null : current)),
				1200,
			);
		} catch (error: any) {
			setMessage({
				text: `Failed to copy commands: ${error.message}`,
				type: "error",
			});
		}
	};

	const deployment = data?.deployment ?? "native";
	const deploymentLabel =
		deployment === "docker"
			? "Docker"
			: deployment === "hosted"
				? "Hosted"
				: "Native";

	const dockerComposeCommands = [
		"docker compose pull spacebot",
		"docker compose up -d --force-recreate spacebot",
	];

	const dockerRunCommands = [
		`docker pull ${pullableDockerImage(data?.docker_image ?? null)}`,
		"docker stop spacebot && docker rm spacebot",
		"# re-run your docker run command",
	];

	const nativeCommands = [
		"git pull",
		"cargo install --path . --force",
		"spacebot restart",
	];

	return (
		<div className="mx-auto max-w-2xl px-6 py-6">
			<div className="mb-6">
				<h2 className="font-plex text-sm font-semibold text-ink">Updates</h2>
				<p className="mt-1 text-sm text-ink-dull">
					Check release status, trigger one-click Docker updates, and copy
					manual update commands.
				</p>
			</div>

			{isLoading ? (
				<div className="flex items-center gap-2 text-ink-dull">
					<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
					Loading update status...
				</div>
			) : (
				<div className="flex flex-col gap-4">
					<div className="rounded-lg border border-app-line bg-app-box p-4">
						<div className="flex items-center justify-between gap-4">
							<div>
								<p className="text-sm font-medium text-ink">Release Status</p>
								<p className="mt-0.5 text-sm text-ink-dull">
									{data?.update_available
										? `Update ${data.latest_version ?? ""} is available`
										: "You're running the latest available release"}
								</p>
							</div>
							<Button
								onClick={() => {
									setMessage(null);
									checkNowMutation.mutate();
								}}
								loading={checkNowMutation.isPending || isFetching}
								size="md"
								variant="outline"
							>
								Check now
							</Button>
						</div>

						<div className="mt-4 grid grid-cols-2 gap-3 text-sm">
							<div>
								<p className="text-ink-faint">Deployment</p>
								<p className="text-ink">{deploymentLabel}</p>
							</div>
							<div>
								<p className="text-ink-faint">Current version</p>
								<p className="text-ink">{data?.current_version ?? "Unknown"}</p>
							</div>
							<div>
								<p className="text-ink-faint">Latest release</p>
								<p className="text-ink">{data?.latest_version ?? "Unknown"}</p>
							</div>
							<div>
								<p className="text-ink-faint">Last checked</p>
								<p className="text-ink">
									{formatCheckedAt(data?.checked_at ?? null)}
								</p>
							</div>
						</div>

						{data?.docker_image && (
							<div className="mt-3 rounded border border-app-line/70 bg-app-dark-box/30 px-3 py-2">
								<p className="text-tiny text-ink-faint">Container image</p>
								<p className="font-mono text-xs text-ink">
									{data.docker_image}
								</p>
							</div>
						)}

						{data?.release_url && (
							<a
								href={data.release_url}
								target="_blank"
								rel="noopener noreferrer"
								className="mt-3 inline-block text-sm text-accent hover:underline"
							>
								View release notes
							</a>
						)}
					</div>

					{deployment === "docker" && (
						<div className="rounded-lg border border-app-line bg-app-box p-4">
							<div className="flex items-center justify-between gap-3">
								<div>
									<p className="text-sm font-medium text-ink">
										One-Click Docker Update
									</p>
									<p className="mt-0.5 text-sm text-ink-dull">
										Pull and swap to the latest release image from the web UI.
									</p>
								</div>
								<Button
									onClick={() => {
										setMessage(null);
										applyMutation.mutate();
									}}
									disabled={!data?.can_apply || !data?.update_available}
									loading={applyMutation.isPending}
									size="md"
								>
									Update now
								</Button>
							</div>
							{!data?.update_available && (
								<p className="mt-3 text-xs text-ink-faint">
									No update available yet.
								</p>
							)}
							{!data?.can_apply && data?.cannot_apply_reason && (
								<p className="mt-3 text-xs text-yellow-300">
									{data.cannot_apply_reason}
								</p>
							)}
							{data?.can_apply && (
								<p className="mt-3 text-xs text-ink-faint">
									Applying an update restarts this instance. The UI should
									reconnect in 10-30 seconds.
								</p>
							)}
						</div>
					)}

					<div className="rounded-lg border border-app-line bg-app-box p-4">
						<p className="text-sm font-medium text-ink">
							Manual Update Commands
						</p>
						<p className="mt-0.5 text-sm text-ink-dull">
							Use these when one-click update is unavailable or when you prefer
							manual rollouts.
						</p>

						{deployment === "docker" && (
							<div className="mt-3 flex flex-col gap-3">
								<div className="rounded border border-app-line/70 bg-app-dark-box/30 p-3">
									<div className="mb-2 flex items-center justify-between">
										<p className="text-xs font-medium uppercase tracking-wider text-ink-faint">
											Docker Compose
										</p>
										<Button
											onClick={() =>
												handleCopy("compose", dockerComposeCommands.join("\n"))
											}
											variant="outline"
											size="md"
										>
											{copiedBlock === "compose" ? "Copied" : "Copy"}
										</Button>
									</div>
									<pre className="overflow-x-auto text-xs text-ink">
										<code>{dockerComposeCommands.join("\n")}</code>
									</pre>
								</div>
								<div className="rounded border border-app-line/70 bg-app-dark-box/30 p-3">
									<div className="mb-2 flex items-center justify-between">
										<p className="text-xs font-medium uppercase tracking-wider text-ink-faint">
											docker run
										</p>
										<Button
											onClick={() =>
												handleCopy("docker-run", dockerRunCommands.join("\n"))
											}
											variant="outline"
											size="md"
										>
											{copiedBlock === "docker-run" ? "Copied" : "Copy"}
										</Button>
									</div>
									<pre className="overflow-x-auto text-xs text-ink">
										<code>{dockerRunCommands.join("\n")}</code>
									</pre>
								</div>
							</div>
						)}

						{deployment === "native" && (
							<div className="mt-3 rounded border border-app-line/70 bg-app-dark-box/30 p-3">
								<div className="mb-2 flex items-center justify-between">
									<p className="text-xs font-medium uppercase tracking-wider text-ink-faint">
										Source Install
									</p>
									<Button
										onClick={() =>
											handleCopy("native", nativeCommands.join("\n"))
										}
										variant="outline"
										size="md"
									>
										{copiedBlock === "native" ? "Copied" : "Copy"}
									</Button>
								</div>
								<pre className="overflow-x-auto text-xs text-ink">
									<code>{nativeCommands.join("\n")}</code>
								</pre>
							</div>
						)}

						{deployment === "hosted" && (
							<p className="mt-3 text-sm text-ink-dull">
								Hosted instances are updated through platform rollouts.
							</p>
						)}
					</div>

					{data?.error && (
						<div className="rounded-md border border-red-500/20 bg-red-500/10 px-3 py-2 text-sm text-red-400">
							Update check error: {data.error}
						</div>
					)}
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
