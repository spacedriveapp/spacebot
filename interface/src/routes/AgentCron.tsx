import { useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { api, type CronJobWithStats, type CreateCronRequest } from "@/api/client";
import { formatDuration, formatTimeAgo } from "@/lib/format";
import { AnimatePresence, motion } from "framer-motion";
import { Modal } from "@/ui/Modal";

// -- Helpers --

function intervalToSeconds(value: number, unit: string): number {
	switch (unit) {
		case "minutes": return value * 60;
		case "hours": return value * 3600;
		case "days": return value * 86400;
		default: return value;
	}
}

function secondsToInterval(seconds: number): { value: number; unit: "minutes" | "hours" | "days" } {
	if (seconds % 86400 === 0 && seconds >= 86400) return { value: seconds / 86400, unit: "days" };
	if (seconds % 3600 === 0 && seconds >= 3600) return { value: seconds / 3600, unit: "hours" };
	return { value: Math.max(1, Math.floor(seconds / 60)), unit: "minutes" };
}

interface CronFormData {
	id: string;
	prompt: string;
	interval_value: number;
	interval_unit: "minutes" | "hours" | "days";
	delivery_target: string;
	active_start_hour: string;
	active_end_hour: string;
	enabled: boolean;
}

function defaultFormData(): CronFormData {
	return {
		id: "",
		prompt: "",
		interval_value: 1,
		interval_unit: "hours",
		delivery_target: "",
		active_start_hour: "",
		active_end_hour: "",
		enabled: true,
	};
}

function jobToFormData(job: CronJobWithStats): CronFormData {
	const interval = secondsToInterval(job.interval_secs);
	return {
		id: job.id,
		prompt: job.prompt,
		interval_value: interval.value,
		interval_unit: interval.unit,
		delivery_target: job.delivery_target,
		active_start_hour: job.active_hours?.[0]?.toString() ?? "",
		active_end_hour: job.active_hours?.[1]?.toString() ?? "",
		enabled: job.enabled,
	};
}

function formDataToRequest(data: CronFormData): CreateCronRequest {
	const active_start = data.active_start_hour ? parseInt(data.active_start_hour, 10) : undefined;
	const active_end = data.active_end_hour ? parseInt(data.active_end_hour, 10) : undefined;
	return {
		id: data.id,
		prompt: data.prompt,
		interval_secs: intervalToSeconds(data.interval_value, data.interval_unit),
		delivery_target: data.delivery_target,
		active_start_hour: active_start,
		active_end_hour: active_end,
		enabled: data.enabled,
	};
}

// -- Main Component --

interface AgentCronProps {
	agentId: string;
}

export function AgentCron({ agentId }: AgentCronProps) {
	const queryClient = useQueryClient();
	const [isModalOpen, setIsModalOpen] = useState(false);
	const [editingJob, setEditingJob] = useState<CronJobWithStats | null>(null);
	const [formData, setFormData] = useState<CronFormData>(defaultFormData());
	const [expandedJobs, setExpandedJobs] = useState<Set<string>>(new Set());
	const [deleteConfirmId, setDeleteConfirmId] = useState<string | null>(null);

	const { data, isLoading, error } = useQuery({
		queryKey: ["cron-jobs", agentId],
		queryFn: () => api.listCronJobs(agentId),
		refetchInterval: 15_000,
	});

	const toggleMutation = useMutation({
		mutationFn: ({ cronId, enabled }: { cronId: string; enabled: boolean }) =>
			api.toggleCronJob(agentId, cronId, enabled),
		onSuccess: () => queryClient.invalidateQueries({ queryKey: ["cron-jobs", agentId] }),
	});

	const triggerMutation = useMutation({
		mutationFn: (cronId: string) => api.triggerCronJob(agentId, cronId),
		onSuccess: () => queryClient.invalidateQueries({ queryKey: ["cron-jobs", agentId] }),
	});

	const deleteMutation = useMutation({
		mutationFn: (cronId: string) => api.deleteCronJob(agentId, cronId),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["cron-jobs", agentId] });
			setDeleteConfirmId(null);
		},
	});

	const saveMutation = useMutation({
		mutationFn: (request: CreateCronRequest) => api.createCronJob(agentId, request),
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["cron-jobs", agentId] });
			setIsModalOpen(false);
			setEditingJob(null);
			setFormData(defaultFormData());
		},
	});

	const openCreate = () => {
		setEditingJob(null);
		setFormData(defaultFormData());
		setIsModalOpen(true);
	};

	const openEdit = (job: CronJobWithStats) => {
		setEditingJob(job);
		setFormData(jobToFormData(job));
		setIsModalOpen(true);
	};

	const closeModal = () => {
		setIsModalOpen(false);
		setEditingJob(null);
		setFormData(defaultFormData());
	};

	const handleSave = () => {
		if (!formData.id.trim() || !formData.prompt.trim() || !formData.delivery_target.trim()) return;
		saveMutation.mutate(formDataToRequest(formData));
	};

	const toggleExpanded = (jobId: string) => {
		setExpandedJobs((prev) => {
			const next = new Set(prev);
			if (next.has(jobId)) next.delete(jobId);
			else next.add(jobId);
			return next;
		});
	};

	const totalJobs = data?.jobs.length ?? 0;
	const enabledJobs = data?.jobs.filter((j) => j.enabled).length ?? 0;
	const totalRuns = data?.jobs.reduce((sum, j) => sum + j.success_count + j.failure_count, 0) ?? 0;
	const failedRuns = data?.jobs.reduce((sum, j) => sum + j.failure_count, 0) ?? 0;

	return (
		<div className="flex h-full flex-col">
			{/* Stats bar */}
			<div className="flex items-center gap-6 border-b border-app-line px-6 py-3">
				<Stat label="total" value={totalJobs} color="text-accent" />
				<Stat label="enabled" value={enabledJobs} color="text-green-500" />
				<Stat label="runs" value={totalRuns} color="text-ink-dull" />
				{failedRuns > 0 && <Stat label="failed" value={failedRuns} color="text-red-500" />}

				<div className="flex-1" />

				<button
					onClick={openCreate}
					className="rounded-lg bg-accent px-3 py-1.5 text-sm font-medium text-white transition-colors hover:bg-accent/80"
				>
					+ New Job
				</button>
			</div>

			{/* Content */}
			<div className="flex-1 overflow-auto p-6">
				{isLoading && (
					<div className="flex items-center justify-center py-12">
						<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
					</div>
				)}

				{error && (
					<div className="rounded-xl bg-red-500/10 px-4 py-3 text-sm text-red-400">
						Failed to load cron jobs
					</div>
				)}

				{!isLoading && !error && totalJobs === 0 && (
					<div className="flex flex-col items-center justify-center py-16 text-center">
						<div className="mb-4 text-3xl text-ink-faint">&#9200;</div>
						<h3 className="mb-2 font-plex text-base font-medium text-ink">No cron jobs</h3>
						<p className="mb-6 max-w-sm text-sm text-ink-dull">
							Schedule automated tasks that run on a timer and deliver results to any messaging channel.
						</p>
						<button
							onClick={openCreate}
							className="rounded-lg bg-accent px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-accent/80"
						>
							Create your first cron job
						</button>
					</div>
				)}

				{totalJobs > 0 && (
					<div className="flex flex-col gap-3">
						{data!.jobs.map((job) => (
							<CronJobCard
								key={job.id}
								job={job}
								agentId={agentId}
								isExpanded={expandedJobs.has(job.id)}
								onToggleExpand={() => toggleExpanded(job.id)}
								onToggleEnabled={() =>
									toggleMutation.mutate({ cronId: job.id, enabled: !job.enabled })
								}
								onTrigger={() => triggerMutation.mutate(job.id)}
								onEdit={() => openEdit(job)}
								onDelete={() => setDeleteConfirmId(job.id)}
								isToggling={toggleMutation.isPending}
								isTriggering={triggerMutation.isPending}
							/>
						))}
					</div>
				)}
			</div>

			{/* Create / Edit Modal */}
			<Modal isOpen={isModalOpen} onClose={closeModal} title={editingJob ? "Edit Cron Job" : "Create Cron Job"}>
				<div className="flex flex-col gap-4">
					<Field label="Job ID">
						<input
							value={formData.id}
							onChange={(e) => setFormData((d) => ({ ...d, id: e.target.value }))}
							placeholder="e.g. check-email"
							disabled={!!editingJob}
							className="w-full rounded-lg border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none disabled:opacity-50"
						/>
					</Field>

					<Field label="Prompt">
						<textarea
							value={formData.prompt}
							onChange={(e) => setFormData((d) => ({ ...d, prompt: e.target.value }))}
							placeholder="What should the agent do on each run?"
							rows={3}
							className="w-full rounded-lg border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
						/>
					</Field>

					<div className="grid grid-cols-2 gap-4">
						<Field label="Interval">
							<div className="flex gap-2">
								<input
									type="number"
									min={1}
									value={formData.interval_value}
									onChange={(e) =>
										setFormData((d) => ({ ...d, interval_value: parseInt(e.target.value, 10) || 1 }))
									}
									className="w-20 rounded-lg border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink focus:border-accent focus:outline-none"
								/>
								<select
									value={formData.interval_unit}
									onChange={(e) =>
										setFormData((d) => ({ ...d, interval_unit: e.target.value as "minutes" | "hours" | "days" }))
									}
									className="flex-1 rounded-lg border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink"
								>
									<option value="minutes">minutes</option>
									<option value="hours">hours</option>
									<option value="days">days</option>
								</select>
							</div>
						</Field>

						<Field label="Delivery Target">
							<input
								value={formData.delivery_target}
								onChange={(e) => setFormData((d) => ({ ...d, delivery_target: e.target.value }))}
								placeholder="discord:channel_id"
								className="w-full rounded-lg border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
							/>
						</Field>
					</div>

					<div className="grid grid-cols-2 gap-4">
						<Field label="Active Start Hour (optional)">
							<input
								type="number"
								min={0}
								max={23}
								value={formData.active_start_hour}
								onChange={(e) => setFormData((d) => ({ ...d, active_start_hour: e.target.value }))}
								placeholder="e.g. 9"
								className="w-full rounded-lg border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
							/>
						</Field>
						<Field label="Active End Hour (optional)">
							<input
								type="number"
								min={0}
								max={23}
								value={formData.active_end_hour}
								onChange={(e) => setFormData((d) => ({ ...d, active_end_hour: e.target.value }))}
								placeholder="e.g. 17"
								className="w-full rounded-lg border border-app-line bg-app-darkBox px-3 py-2 text-sm text-ink placeholder:text-ink-faint focus:border-accent focus:outline-none"
							/>
						</Field>
					</div>

					<label className="flex items-center gap-2 text-sm text-ink-dull">
						<input
							type="checkbox"
							checked={formData.enabled}
							onChange={(e) => setFormData((d) => ({ ...d, enabled: e.target.checked }))}
							className="rounded border-app-line"
						/>
						Enabled
					</label>

					<div className="mt-2 flex justify-end gap-2">
						<button
							onClick={closeModal}
							className="rounded-lg px-3 py-1.5 text-sm text-ink-dull transition-colors hover:text-ink"
						>
							Cancel
						</button>
						<button
							onClick={handleSave}
							disabled={!formData.id.trim() || !formData.prompt.trim() || !formData.delivery_target.trim() || saveMutation.isPending}
							className="rounded-lg bg-accent px-4 py-1.5 text-sm font-medium text-white transition-colors hover:bg-accent/80 disabled:opacity-50"
						>
							{saveMutation.isPending ? "Saving..." : editingJob ? "Save Changes" : "Create Job"}
						</button>
					</div>
				</div>
			</Modal>

			{/* Delete Confirmation */}
			<Modal isOpen={!!deleteConfirmId} onClose={() => setDeleteConfirmId(null)} title="Delete Cron Job?">
				<p className="mb-4 text-sm text-ink-dull">
					This will permanently delete <code className="rounded bg-app-darkBox px-1.5 py-0.5 text-ink">{deleteConfirmId}</code> and its execution history.
				</p>
				<div className="flex justify-end gap-2">
					<button
						onClick={() => setDeleteConfirmId(null)}
						className="rounded-lg px-3 py-1.5 text-sm text-ink-dull transition-colors hover:text-ink"
					>
						Cancel
					</button>
					<button
						onClick={() => deleteConfirmId && deleteMutation.mutate(deleteConfirmId)}
						disabled={deleteMutation.isPending}
						className="rounded-lg bg-red-600 px-4 py-1.5 text-sm font-medium text-white transition-colors hover:bg-red-700 disabled:opacity-50"
					>
						{deleteMutation.isPending ? "Deleting..." : "Delete"}
					</button>
				</div>
			</Modal>
		</div>
	);
}

// -- Sub-components --

function Stat({ label, value, color }: { label: string; value: number; color: string }) {
	return (
		<div className="flex items-center gap-1.5">
			<span className={`font-plex text-lg font-semibold tabular-nums ${color}`}>{value}</span>
			<span className="text-xs text-ink-faint">{label}</span>
		</div>
	);
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
	return (
		<div className="space-y-1.5">
			<label className="text-xs font-medium text-ink-dull">{label}</label>
			{children}
		</div>
	);
}

function CronJobCard({
	job,
	agentId,
	isExpanded,
	onToggleExpand,
	onToggleEnabled,
	onTrigger,
	onEdit,
	onDelete,
	isToggling,
	isTriggering,
}: {
	job: CronJobWithStats;
	agentId: string;
	isExpanded: boolean;
	onToggleExpand: () => void;
	onToggleEnabled: () => void;
	onTrigger: () => void;
	onEdit: () => void;
	onDelete: () => void;
	isToggling: boolean;
	isTriggering: boolean;
}) {
	const totalRuns = job.success_count + job.failure_count;
	const successRate = totalRuns > 0 ? Math.round((job.success_count / totalRuns) * 100) : null;

	return (
		<div className="overflow-hidden rounded-xl border border-app-line bg-app-darkBox">
			{/* Job row */}
			<div className="flex items-start gap-3 p-4">
				{/* Status dot */}
				<div
					className={`mt-1.5 h-2.5 w-2.5 shrink-0 rounded-full ${
						job.enabled ? "bg-green-500" : "bg-gray-500"
					}`}
				/>

				{/* Info */}
				<div className="min-w-0 flex-1">
					<div className="mb-1 flex items-center gap-2">
						<code className="rounded bg-app-lightBox px-1.5 py-0.5 text-xs font-medium text-ink">
							{job.id}
						</code>
						{job.active_hours && (
							<span className="text-tiny text-ink-faint">
								{String(job.active_hours[0]).padStart(2, "0")}:00–{String(job.active_hours[1]).padStart(2, "0")}:00
							</span>
						)}
						{!job.enabled && (
							<span className="rounded bg-gray-500/20 px-1.5 py-0.5 text-tiny text-gray-400">disabled</span>
						)}
					</div>

					<p className="mb-2 text-sm text-ink-dull" title={job.prompt}>
						{job.prompt.length > 120 ? job.prompt.slice(0, 120) + "..." : job.prompt}
					</p>

					<div className="flex flex-wrap items-center gap-3 text-tiny text-ink-faint">
						<span>every {formatDuration(job.interval_secs)}</span>
						<span className="text-ink-faint/50">·</span>
						<span>{job.delivery_target}</span>
						{job.last_executed_at && (
							<>
								<span className="text-ink-faint/50">·</span>
								<span>ran {formatTimeAgo(job.last_executed_at)}</span>
							</>
						)}
						{successRate !== null && (
							<>
								<span className="text-ink-faint/50">·</span>
								<span className={successRate >= 90 ? "text-green-500" : successRate >= 50 ? "text-yellow-500" : "text-red-500"}>
									{successRate}% success ({job.success_count}/{totalRuns})
								</span>
							</>
						)}
					</div>
				</div>

				{/* Actions */}
				<div className="flex items-center gap-0.5">
					<ActionButton
						title={job.enabled ? "Disable" : "Enable"}
						onClick={onToggleEnabled}
						disabled={isToggling}
					>
						{job.enabled ? "⏸" : "▶"}
					</ActionButton>
					<ActionButton
						title="Run now"
						onClick={onTrigger}
						disabled={isTriggering || !job.enabled}
					>
						⚡
					</ActionButton>
					<ActionButton title="Edit" onClick={onEdit}>✎</ActionButton>
					<ActionButton title="Delete" onClick={onDelete} className="hover:text-red-400">
						✕
					</ActionButton>
				</div>
			</div>

			{/* Execution history (expandable) */}
			{isExpanded && (
				<div className="border-t border-app-line bg-app-darkBox/50 px-4 py-3">
					<JobExecutions agentId={agentId} jobId={job.id} />
				</div>
			)}

			{/* Expand toggle */}
			<button
				onClick={onToggleExpand}
				className="flex w-full items-center justify-center gap-1 border-t border-app-line/50 py-1.5 text-tiny text-ink-faint transition-colors hover:bg-app-lightBox/30 hover:text-ink-dull"
			>
				{isExpanded ? "▾ Hide history" : "▸ Show history"}
			</button>
		</div>
	);
}

function ActionButton({
	title,
	onClick,
	disabled,
	className,
	children,
}: {
	title: string;
	onClick: () => void;
	disabled?: boolean;
	className?: string;
	children: React.ReactNode;
}) {
	return (
		<button
			title={title}
			onClick={onClick}
			disabled={disabled}
			className={`rounded-md px-2 py-1.5 text-sm text-ink-faint transition-colors hover:bg-app-lightBox hover:text-ink disabled:opacity-30 ${className ?? ""}`}
		>
			{children}
		</button>
	);
}

function JobExecutions({ agentId, jobId }: { agentId: string; jobId: string }) {
	const { data, isLoading } = useQuery({
		queryKey: ["cron-executions", agentId, jobId],
		queryFn: () => api.cronExecutions(agentId, { cron_id: jobId, limit: 10 }),
	});

	if (isLoading) {
		return (
			<div className="flex items-center justify-center py-3">
				<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
			</div>
		);
	}

	if (!data?.executions.length) {
		return <p className="py-2 text-tiny text-ink-faint">No execution history yet.</p>;
	}

	return (
		<div className="flex flex-col gap-1">
			{data.executions.map((execution) => (
				<div
					key={execution.id}
					className="flex items-center gap-3 rounded-lg px-3 py-1.5"
				>
					<span className={`text-xs ${execution.success ? "text-green-500" : "text-red-500"}`}>
						{execution.success ? "✓" : "✗"}
					</span>
					<span className="text-tiny tabular-nums text-ink-faint">
						{formatTimeAgo(execution.executed_at)}
					</span>
					{execution.result_summary && (
						<span className="min-w-0 flex-1 truncate text-tiny text-ink-dull">
							{execution.result_summary}
						</span>
					)}
				</div>
			))}
		</div>
	);
}
