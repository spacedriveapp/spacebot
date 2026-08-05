import {useQuery} from "@tanstack/react-query";
import {FontAwesomeIcon} from "@fortawesome/react-fontawesome";
import {
	faCircleCheck,
	faLockOpen,
	faTriangleExclamation,
} from "@fortawesome/free-solid-svg-icons";
import {api, type SandboxContainmentStatus} from "@/api/client";

/**
 * What the host is actually doing about containment, per agent.
 *
 * `sandbox.mode` is what the operator asked for; `containment_active` is what
 * is in force. They read alike from the config surface and are not the same
 * question, and reporting only the first is how an instance ends up running
 * unconfined while its config says `enabled` — the one-label-two-conditions
 * shape this codebase keeps paying for, this time in the security layer.
 *
 * Until now `GET /status` returned all three facts and nothing displayed any of
 * them.
 */
export function useContainmentStatus() {
	const {data, isLoading} = useQuery({
		queryKey: ["status"],
		queryFn: api.status,
		// Installing a backend requires a restart, so this does not change under
		// anyone. Polled slowly rather than never so a restarted instance is not
		// misreported for the life of the tab.
		staleTime: 30_000,
		refetchInterval: 60_000,
	});
	return {agents: data?.sandbox ?? [], isLoading};
}

/** Whether any agent's config claims containment the host is not providing. */
export function anyInert(agents: SandboxContainmentStatus[]): boolean {
	return agents.some((agent) => agent.requested_but_inert);
}

/**
 * One agent's containment, as the three facts rather than a green tick.
 *
 * The middle state is the one worth the space: mode says `enabled`, no backend
 * exists, and the read/write allowlists come back empty. That is the state a
 * command step refuses to run in, so this is also the explanation for a task
 * parked as `capability` with nothing obviously wrong.
 */
export function ContainmentRow({status}: {status: SandboxContainmentStatus}) {
	const inert = status.requested_but_inert;
	const active = status.containment_active;

	return (
		<div
			className={`rounded border px-2 py-1.5 text-[11px] ${
				inert
					? "border-status-warning/40 bg-status-warning/5"
					: active
						? "border-status-success/30 bg-status-success/5"
						: "border-app-line bg-app-box/40"
			}`}
		>
			<div className="flex flex-wrap items-center gap-x-2 gap-y-1">
				<FontAwesomeIcon
					icon={
						inert
							? faTriangleExclamation
							: active
								? faCircleCheck
								: faLockOpen
					}
					className={`text-[10px] ${
						inert
							? "text-status-warning"
							: active
								? "text-status-success"
								: "text-ink-faint"
					}`}
				/>
				<span className="font-mono text-ink-dull">{status.agent_id}</span>
				<span
					className={
						inert
							? "text-status-warning"
							: active
								? "text-status-success"
								: "text-ink-faint"
					}
				>
					{inert
						? "requested but inert"
						: active
							? `contained by ${status.backend ?? "an unnamed backend"}`
							: "not contained"}
				</span>
				<span className="text-ink-faint">
					· mode <span className="font-mono">{status.mode}</span>
					{status.backend ? (
						<>
							{" "}
							· backend <span className="font-mono">{status.backend}</span>
						</>
					) : (
						" · no backend detected"
					)}
					{status.require_containment && " · required"}
				</span>
			</div>

			{inert && (
				<p className="mt-1 text-[10px] text-status-warning">
					This config claims containment the host is not providing: the
					allowlists come back empty and a shell command runs with full host
					access. Command steps <strong className="font-medium">refuse to run</strong>{" "}
					in this state — they are stored, repeated and unattended, so they fail
					closed rather than inheriting a worker's watched-in-the-moment risk.
					Install a backend (on Linux, the <span className="font-mono">bubblewrap</span>{" "}
					package) and restart, or set mode to{" "}
					<span className="font-mono">disabled</span> to say out loud that this
					host runs uncontained — enforcement is identical either way.
				</p>
			)}
		</div>
	);
}

/** Every agent's containment, for a settings screen. */
export function ContainmentStatusList() {
	const {agents, isLoading} = useContainmentStatus();

	if (isLoading) {
		return (
			<p className="text-tiny text-ink-faint">Checking what the host enforces…</p>
		);
	}
	if (agents.length === 0) {
		return (
			<p className="text-tiny text-ink-faint">
				No agent has a live sandbox to report on.
			</p>
		);
	}
	return (
		<div className="flex flex-col gap-1.5">
			{agents.map((status) => (
				<ContainmentRow key={status.agent_id} status={status} />
			))}
		</div>
	);
}
