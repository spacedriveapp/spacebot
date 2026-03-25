import { useState, useCallback, useEffect, lazy, Suspense } from "react";
import { Button } from "@/ui/Button";
import { Input } from "@/ui/Input";
import { useServer } from "@/hooks/useServer";
import { IS_TAURI } from "@/platform";

const Orb = lazy(() => import("@/components/Orb"));

type SidecarState = "idle" | "starting" | "running" | "error";

/**
 * Full-screen connection screen shown when the app cannot reach
 * the spacebot server. Allows changing the server URL and, in
 * Tauri builds with a bundled sidecar, starting a local instance.
 */
export function ConnectionScreen() {
	const { serverUrl, setServerUrl, state, hasSidecar } = useServer();
	const [draft, setDraft] = useState(serverUrl);
	const [sidecarState, setSidecarState] = useState<SidecarState>("idle");
	const [sidecarError, setSidecarError] = useState<string | null>(null);

	// Keep draft in sync when serverUrl changes externally
	useEffect(() => {
		setDraft(serverUrl);
	}, [serverUrl]);

	const handleConnect = useCallback(() => {
		setServerUrl(draft);
	}, [draft, setServerUrl]);

	const handleKeyDown = useCallback(
		(event: React.KeyboardEvent) => {
			if (event.key === "Enter") handleConnect();
		},
		[handleConnect],
	);

	const handleStartLocal = useCallback(async () => {
		if (!IS_TAURI) return;
		setSidecarState("starting");
		setSidecarError(null);
		try {
			// Dynamic import: this module only exists in Tauri builds
			const { Command } = await import("@tauri-apps/plugin-shell");
			const command = Command.sidecar("binaries/spacebot", [
				"start",
				"--foreground",
			]);
			let sawReady = false;

			command.on("error", (error: string) => {
				setSidecarState("error");
				setSidecarError(error);
			});

			command.on("close", (data: { code: number | null }) => {
				if (!sawReady || data.code === null || data.code !== 0) {
					setSidecarState("error");
					setSidecarError(
						data.code === null
							? "Process exited before the HTTP server became ready"
							: `Process exited with code ${data.code}`,
					);
					return;
				}
				setSidecarState("idle");
			});

			command.stdout.on("data", (line: string) => {
				// Look for the "HTTP server listening" log line
				if (line.includes("HTTP server listening")) {
					sawReady = true;
					setSidecarState("running");
					// Point the app at localhost
					setServerUrl("http://localhost:19898");
				}
			});

			await command.spawn();
			setSidecarState("starting");
		} catch (error) {
			setSidecarState("error");
			setSidecarError(
				error instanceof Error ? error.message : String(error),
			);
		}
	}, [setServerUrl]);

	const isChecking = state === "checking";

	return (
		<div className="flex h-screen w-full flex-col items-center justify-center bg-app overflow-hidden">
			{/* Draggable titlebar region for Tauri */}
			{IS_TAURI && (
				<div
					data-tauri-drag-region
					className="fixed inset-x-0 top-0 h-8"
				/>
			)}

			<div className="flex w-full max-w-md flex-col items-center gap-8 px-6">
				{/* Orb + Title */}
				<div className="flex flex-col items-center gap-3">
					<div className="relative h-[160px] w-[160px]">
						<div className="absolute inset-[calc(5%-10px)] z-0">
							<img
								src="/ball.png"
								alt="Spacebot"
								className="h-full w-full object-contain"
							/>
						</div>
						<div className="absolute inset-0 z-10">
							<Suspense fallback={null}>
								<Orb
									hue={-30}
									hoverIntensity={0}
									rotateOnHover
								/>
							</Suspense>
						</div>
					</div>
					<h1 className="font-plex text-xl font-semibold text-ink">
						Connect to Spacebot
					</h1>
					<p className="text-center text-sm text-ink-dull">
						Enter the URL of a running Spacebot instance, or start
						one locally.
					</p>
				</div>

				{/* URL Input */}
				<div className="flex w-full flex-col gap-3">
					<label className="text-xs font-medium text-ink-dull">
						Server URL
					</label>
					<div className="flex gap-2">
						<Input
							value={draft}
							onChange={(event) => setDraft(event.target.value)}
							onKeyDown={handleKeyDown}
							placeholder="http://localhost:19898"
							className="flex-1"
							size="md"
							disabled={isChecking}
						/>
						<Button
							onClick={handleConnect}
							disabled={isChecking || !draft.trim()}
							size="default"
							variant="ghost"
							className="bg-[hsl(282,70%,57%)] text-white shadow hover:bg-[hsl(282,70%,50%)] hover:text-white"
						>
							Connect
						</Button>
					</div>

					{/* Connection status */}
					{isChecking ? (
						<p className="text-xs text-ink-faint">
							Connecting...
						</p>
					) : state === "disconnected" ? (
						<p className="text-xs text-ink-faint">
							Not connected
						</p>
					) : null}
				</div>

				{/* Divider */}
				{hasSidecar && (
					<>
						<div className="flex w-full items-center gap-3">
							<div className="h-px flex-1 bg-app-line" />
							<span className="text-xs text-ink-faint">or</span>
							<div className="h-px flex-1 bg-app-line" />
						</div>

						{/* Start Local Server */}
						<div className="flex w-full flex-col gap-3">
							<Button
								onClick={handleStartLocal}
								variant="outline"
								loading={sidecarState === "starting"}
								disabled={
									sidecarState === "starting" ||
									sidecarState === "running"
								}
								className="w-full"
							>
								{sidecarState === "starting"
									? "Starting Spacebot..."
									: sidecarState === "running"
										? "Server Running"
										: "Start Local Server"}
							</Button>

							{sidecarState === "starting" && (
								<p className="text-xs text-ink-faint">
									Starting the bundled Spacebot binary. This
									may take a few seconds on first run...
								</p>
							)}

							{sidecarState === "error" && sidecarError && (
								<p className="text-xs text-red-400">
									{sidecarError}
								</p>
							)}
						</div>
					</>
				)}

				{/* Footer hint */}
				<p className="text-center text-xs text-ink-faint">
					Spacebot runs on port 19898 by default.
					{!hasSidecar && (
						<>
							{" "}
							Install via{" "}
							<span className="font-mono text-ink-dull">
								docker
							</span>{" "}
							or download from{" "}
							<a
								href="https://spacebot.sh"
								target="_blank"
								rel="noopener noreferrer"
								className="text-accent hover:underline"
							>
								spacebot.sh
							</a>
						</>
					)}
				</p>
			</div>
		</div>
	);
}
