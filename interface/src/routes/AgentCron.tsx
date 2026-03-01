import { useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { api, type CronJobWithStats, type CreateCronRequest } from "@/api/client";
import { formatDuration, formatTimeAgo } from "@/lib/format";
import { Clock05Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import {
	Button,
	Dialog,
	DialogContent,
	DialogHeader,
	DialogTitle,
	Badge,
	Input,
	TextArea,
	Toggle,
	Label,
	NumberStepper,
	Select,
	SelectTrigger,
	SelectValue,
	SelectContent,
	SelectItem,
} from "@/ui";

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
	run_once: boolean;
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
		run_once: false,
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
		run_once: job.run_once,
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
		run_once: data.run_once,
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
			{totalJobs > 0 && (
				<div className="flex items-center gap-2 border-b border-app-line px-6 py-3">
					<Badge variant="accent" size="md">{totalJobs} total</Badge>
					<Badge variant="green" size="md">{enabledJobs} enabled</Badge>
					<Badge variant="outline" size="md">{totalRuns} runs</Badge>
					{failedRuns > 0 && <Badge variant="red" size="md">{failedRuns} failed</Badge>}

					<div className="flex-1" />

					<Button onClick={openCreate} size="sm">
						+ New Job
					</Button>
				</div>
			)}

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
					<div className="flex h-full items-start justify-center pt-[15vh]">
						<div className="flex max-w-sm flex-col items-center rounded-xl border border-dashed border-app-line/50 bg-app-darkBox/20 p-8 text-center">
							<div className="mb-4 flex h-12 w-12 items-center justify-center rounded-full border border-app-line bg-app-darkBox">
								<HugeiconsIcon icon={Clock05Icon} className="h-6 w-6 text-ink-faint" />
							</div>
							<h3 className="mb-1 font-plex text-sm font-medium text-ink">No cron jobs yet</h3>
							<p className="mb-5 max-w-md text-sm text-ink-faint">
								Schedule automated tasks that run on a timer and deliver results to messaging channels
							</p>
							<Button onClick={openCreate} variant="secondary" size="sm">
								+ New Job
							</Button>
						</div>
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
			<Dialog open={isModalOpen} onOpenChange={(open) => !open && closeModal()}>
				<DialogContent>
					<DialogHeader>
						<DialogTitle>{editingJob ? "Edit Cron Job" : "Create Cron Job"}</DialogTitle>
					</DialogHeader>
					<div className="flex flex-col gap-4">
						<Field label="Job ID">
							<Input
								value={formData.id}
								onChange={(e) => setFormData((d) => ({ ...d, id: e.target.value }))}
								placeholder="e.g. check-email"
								disabled={!!editingJob}
								autoComplete="off"
							/>
						</Field>

						<Field label="Prompt">
							<TextArea
								value={formData.prompt}
								onChange={(e) => setFormData((d) => ({ ...d, prompt: e.target.value }))}
								placeholder="What should the agent do on each run?"
								rows={3}
							/>
						</Field>

						<Field label="Interval">
							<div className="flex items-center gap-2">
								<span className="text-sm text-ink-faint">Every</span>
								<NumberStepper
									value={formData.interval_value}
									onChange={(v) => setFormData((d) => ({ ...d, interval_value: v }))}
									min={1}
									max={999}
									variant="compact"
									suffix={` ${formData.interval_unit}`}
								/>
								<Select
									value={formData.interval_unit}
									onValueChange={(value) =>
										setFormData((d) => ({ ...d, interval_unit: value as "minutes" | "hours" | "days" }))
									}
								>
									<SelectTrigger className="w-32">
										<SelectValue />
									</SelectTrigger>
									<SelectContent>
										<SelectItem value="minutes">minutes</SelectItem>
										<SelectItem value="hours">hours</SelectItem>
										<SelectItem value="days">days</SelectItem>
									</SelectContent>
								</Select>
							</div>
							<p className="mt-1 text-tiny text-ink-faint">How often this job should run</p>
						</Field>

						<div className="grid grid-cols-2 gap-4">

							<Field label="Delivery Target">
								<Input
									value={formData.delivery_target}
									onChange={(e) => setFormData((d) => ({ ...d, delivery_target: e.target.value }))}
									placeholder="discord:channel_id"
								/>
							</Field>
						</div>

						<Field label="Active Hours (optional)">
							<div className="flex items-center gap-2">
								<NumberStepper
									value={parseInt(formData.active_start_hour) || 0}
									onChange={(v) => setFormData((d) => ({ ...d, active_start_hour: v.toString() }))}
									min={0}
									max={23}
									suffix="h"
									variant="compact"
								/>
								<span className="text-sm text-ink-faint">to</span>
								<NumberStepper
									value={parseInt(formData.active_end_hour) || 23}
									onChange={(v) => setFormData((d) => ({ ...d, active_end_hour: v.toString() }))}
									min={0}
									max={23}
									suffix="h"
									variant="compact"
								/>
							</div>
							<p className="mt-1 text-tiny text-ink-faint">Job will only run during these hours (0-23, 24-hour format)</p>
						</Field>

						<div className="flex items-center justify-between">
							<Label>Enabled</Label>
							<Toggle checked={formData.enabled} onCheckedChange={(checked) => setFormData((d) => ({ ...d, enabled: checked }))} size="lg" />
						</div>

						<div className="flex items-center justify-between">
							<Label>Run Once</Label>
							<Toggle checked={formData.run_once} onCheckedChange={(checked) => setFormData((d) => ({ ...d, run_once: checked }))} size="lg" />
						</div>

					<div className="mt-2 flex justify-end gap-2">
						<Button variant="ghost" size="sm" onClick={closeModal}>
							Cancel
						</Button>
						<Button
							size="sm"
							onClick={handleSave}
							disabled={!formData.id.trim() || !formData.prompt.trim() || !formData.delivery_target.trim()}
							loading={saveMutation.isPending}
						>
							{editingJob ? "Save Changes" : "Create Job"}
						</Button>
					</div>
					</div>
				</DialogContent>
			</Dialog>

			{/* Delete Confirmation */}
			<Dialog open={!!deleteConfirmId} onOpenChange={(open) => !open && setDeleteConfirmId(null)}>
				<DialogContent>
					<DialogHeader>
						<DialogTitle>Delete Cron Job?</DialogTitle>
					</DialogHeader>
					<p className="mb-4 text-sm text-ink-dull">
						This will permanently delete <code className="rounded bg-app-darkBox px-1.5 py-0.5 text-ink">{deleteConfirmId}</code> and its execution history.
					</p>
					<div className="flex justify-end gap-2">
						<Button
							variant="ghost"
							size="sm"
							onClick={() => setDeleteConfirmId(null)}
						>
							Cancel
						</Button>
						<Button
							variant="destructive"
							size="sm"
							onClick={() => deleteConfirmId && deleteMutation.mutate(deleteConfirmId)}
							loading={deleteMutation.isPending}
						>
							Delete
						</Button>
					</div>
				</DialogContent>
			</Dialog>
		</div>
	);
}

// -- Sub-components --

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
						{job.run_once && (
							<span className="rounded bg-accent/20 px-1.5 py-0.5 text-tiny text-accent">one-time</span>
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
			<Button
				variant="ghost"
				size="sm"
				onClick={onToggleExpand}
				className="w-full rounded-none border-t border-app-line/50"
			>
				{isExpanded ? "▾ Hide history" : "▸ Show history"}
			</Button>
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
		<Button
			title={title}
			onClick={onClick}
			disabled={disabled}
			variant="ghost"
			size="sm"
			className={className}
		>
			{children}
		</Button>
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
