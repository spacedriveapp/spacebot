import {useState} from "react";
import {useQuery, useMutation, useQueryClient} from "@tanstack/react-query";
import {
	api,
	type CronJobWithStats,
	type CreateCronRequest,
	type ChannelInfo,
} from "@/api/client";
import {formatCronSchedule, formatTimeAgo} from "@/lib/format";
import {
	Clock,
	Pause,
	Play,
	Lightning,
	PencilSimple,
	Trash,
	CaretDown,
	CaretUp,
} from "@phosphor-icons/react";
import {
	Button,
	DialogRoot,
	DialogContent,
	DialogHeader,
	DialogTitle,
	Badge,
	Input,
	TextArea,
	Label,
	NumberStepper,
	SelectRoot,
	SelectTrigger,
	SelectValue,
	SelectContent,
	SelectItem,
} from "@spacedrive/primitives";
import {Switch} from "@spacedrive/primitives";

// -- Helpers --

type ScheduleMode = "cron" | "interval";

function intervalToSeconds(value: number, unit: string): number {
	switch (unit) {
		case "minutes":
			return value * 60;
		case "hours":
			return value * 3600;
		case "days":
			return value * 86400;
		default:
			return value;
	}
}

function secondsToInterval(seconds: number): {
	value: number;
	unit: "minutes" | "hours" | "days";
} {
	if (seconds % 86400 === 0 && seconds >= 86400)
		return {value: seconds / 86400, unit: "days"};
	if (seconds % 3600 === 0 && seconds >= 3600)
		return {value: seconds / 3600, unit: "hours"};
	return {value: Math.max(1, Math.floor(seconds / 60)), unit: "minutes"};
}

interface CronFormData {
	id: string;
	prompt: string;
	schedule_mode: ScheduleMode;
	cron_expr: string;
	interval_value: number;
	interval_unit: "minutes" | "hours" | "days";
	delivery_target: string;
	active_start_hour: string;
	active_end_hour: string;
	enabled: boolean;
	run_once: boolean;
	timeout_secs: string;
}

function defaultFormData(): CronFormData {
	return {
		id: "",
		prompt: "",
		schedule_mode: "cron",
		cron_expr: "",
		interval_value: 1,
		interval_unit: "hours",
		delivery_target: "",
		active_start_hour: "",
		active_end_hour: "",
		enabled: true,
		run_once: false,
		timeout_secs: "",
	};
}

function jobToFormData(job: CronJobWithStats): CronFormData {
	const interval = secondsToInterval(job.interval_secs);
	return {
		id: job.id,
		prompt: job.prompt,
		schedule_mode: job.cron_expr ? "cron" : "interval",
		cron_expr: job.cron_expr ?? "",
		interval_value: interval.value,
		interval_unit: interval.unit,
		delivery_target: job.delivery_target,
		active_start_hour: job.active_hours?.[0]?.toString() ?? "",
		active_end_hour: job.active_hours?.[1]?.toString() ?? "",
		enabled: job.enabled,
		run_once: job.run_once,
		timeout_secs: job.timeout_secs?.toString() ?? "",
	};
}

function formDataToRequest(data: CronFormData): CreateCronRequest {
	const active_start = data.active_start_hour
		? parseInt(data.active_start_hour, 10)
		: undefined;
	const active_end = data.active_end_hour
		? parseInt(data.active_end_hour, 10)
		: undefined;
	const timeout = data.timeout_secs
		? parseInt(data.timeout_secs, 10)
		: undefined;
	return {
		id: data.id,
		prompt: data.prompt,
		cron_expr:
			data.schedule_mode === "cron" && data.cron_expr.trim()
				? data.cron_expr.trim()
				: undefined,
		interval_secs:
			data.schedule_mode === "interval"
				? intervalToSeconds(data.interval_value, data.interval_unit)
				: undefined,
		delivery_target: data.delivery_target,
		active_start_hour: active_start,
		active_end_hour: active_end,
		enabled: data.enabled,
		run_once: data.run_once,
		timeout_secs: timeout || undefined,
	};
}

// Cron expression presets for the dropdown
const CRON_PRESETS: {label: string; value: string}[] = [
	{label: "Every hour", value: "0 * * * *"},
	{label: "Every 15 minutes", value: "*/15 * * * *"},
	{label: "Every 30 minutes", value: "*/30 * * * *"},
	{label: "Daily at 9:00", value: "0 9 * * *"},
	{label: "Daily at midnight", value: "0 0 * * *"},
	{label: "Weekdays at 9:00", value: "0 9 * * 1-5"},
	{label: "Weekdays at 17:00", value: "0 17 * * 1-5"},
	{label: "Every Monday at 9:00", value: "0 9 * * 1"},
	{label: "1st of month at noon", value: "0 12 1 * *"},
];

// -- Main Component --

interface AgentCronProps {
	agentId: string;
}

export function AgentCron({agentId}: AgentCronProps) {
	const queryClient = useQueryClient();
	const [isModalOpen, setIsModalOpen] = useState(false);
	const [editingJob, setEditingJob] = useState<CronJobWithStats | null>(null);
	const [formData, setFormData] = useState<CronFormData>(defaultFormData());
	const [expandedJobs, setExpandedJobs] = useState<Set<string>>(new Set());
	const [deleteConfirmId, setDeleteConfirmId] = useState<string | null>(null);

	const {data, isLoading, error} = useQuery({
		queryKey: ["cron-jobs", agentId],
		queryFn: () => api.listCronJobs(agentId),
		refetchInterval: 15_000,
	});

	const {data: channelsData} = useQuery({
		queryKey: ["channels"],
		queryFn: () => api.channels(),
	});

	// Build delivery target options from known channels (exclude cron and link channels)
	const EXCLUDED_PLATFORMS = new Set(["cron"]);
	const deliveryTargets = (channelsData?.channels ?? [])
		.filter((ch: ChannelInfo) => !EXCLUDED_PLATFORMS.has(ch.platform))
		.map((ch: ChannelInfo) => {
			// Portal channels need the "webchat:" prefix since the adapter is
			// registered as "webchat", not "portal".
			const value = ch.platform === "portal" ? `webchat:${ch.id}` : ch.id;
			let label: string;
			if (ch.platform === "portal") {
				label = "Web Portal";
			} else if (ch.platform === "link") {
				// Link channels have IDs like "link:main:spacebot-engineer"
				const parts = ch.id.split(":");
				label = parts.length >= 3 ? parts.slice(1).join(" → ") : ch.id;
			} else {
				label = ch.display_name ?? ch.id;
			}
			return {value, label, platform: ch.platform, agentId: ch.agent_id};
		});

	const toggleMutation = useMutation({
		mutationFn: ({cronId, enabled}: {cronId: string; enabled: boolean}) =>
			api.toggleCronJob(agentId, cronId, enabled),
		onSuccess: () =>
			queryClient.invalidateQueries({queryKey: ["cron-jobs", agentId]}),
	});

	const triggerMutation = useMutation({
		mutationFn: (cronId: string) => api.triggerCronJob(agentId, cronId),
		onSuccess: () =>
			queryClient.invalidateQueries({queryKey: ["cron-jobs", agentId]}),
	});

	const deleteMutation = useMutation({
		mutationFn: (cronId: string) => api.deleteCronJob(agentId, cronId),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["cron-jobs", agentId]});
			setDeleteConfirmId(null);
		},
	});

	const saveMutation = useMutation({
		mutationFn: (request: CreateCronRequest) =>
			api.createCronJob(agentId, request),
		onSuccess: () => {
			queryClient.invalidateQueries({queryKey: ["cron-jobs", agentId]});
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
		if (
			!formData.id.trim() ||
			!formData.prompt.trim() ||
			!formData.delivery_target.trim()
		)
			return;
		if (formData.schedule_mode === "cron" && !formData.cron_expr.trim()) return;
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
	const totalRuns =
		data?.jobs.reduce(
			(sum, j) => sum + j.execution_success_count + j.execution_failure_count,
			0,
		) ?? 0;
	const executionFailures =
		data?.jobs.reduce((sum, j) => sum + j.execution_failure_count, 0) ?? 0;
	const deliveryFailures =
		data?.jobs.reduce((sum, j) => sum + j.delivery_failure_count, 0) ?? 0;

	return (
		<div className="flex h-full flex-col">
			{/* Stats bar */}
			{totalJobs > 0 && (
				<div className="flex items-center gap-2 border-b border-app-line px-6 py-2">
					<Badge variant="outline" size="md">
						{totalJobs} total
					</Badge>
					<Badge variant="outline" size="md">
						{enabledJobs} enabled
					</Badge>
					<Badge variant="outline" size="md">
						{totalRuns} executions
					</Badge>
					{executionFailures > 0 && (
						<Badge variant="error" size="md">
							{executionFailures} exec failed
						</Badge>
					)}
					{deliveryFailures > 0 && (
						<Badge variant="error" size="md">
							{deliveryFailures} delivery failed
						</Badge>
					)}
					{data?.timezone && (
						<Badge variant="outline" size="md">
							Timezone: {data.timezone}
						</Badge>
					)}

					<div className="flex-1" />

					<Button onClick={openCreate} variant="gray" size="md">
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
						<div className="flex max-w-sm flex-col items-center rounded-xl border border-dashed border-app-line/50 bg-app-dark-box/20 p-8 text-center">
							<div className="mb-4 flex h-12 w-12 items-center justify-center rounded-full border border-app-line bg-app-dark-box">
								<Clock className="h-6 w-6 text-ink-faint" />
							</div>
							<h3 className="mb-1 font-plex text-sm font-medium text-ink">
								No cron jobs yet
							</h3>
							<p className="mb-5 max-w-md text-sm text-ink-faint">
								Schedule automated tasks that run on a timer and deliver results
								to messaging channels
							</p>
							<Button onClick={openCreate} variant="gray" size="sm">
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
									toggleMutation.mutate({cronId: job.id, enabled: !job.enabled})
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
			<DialogRoot
				open={isModalOpen}
				onOpenChange={(open) => !open && closeModal()}
			>
				<DialogContent className="!max-w-4xl">
					<DialogHeader>
						<DialogTitle>
							{editingJob ? "Edit Cron Job" : "Create Cron Job"}
						</DialogTitle>
					</DialogHeader>
					<div className="grid grid-cols-2 gap-6">
						{/* Left column — what & when */}
						<div className="flex flex-col gap-4">
							<Field label="Job ID">
								<Input
									value={formData.id}
									onChange={(e) =>
										setFormData((d) => ({...d, id: e.target.value}))
									}
									placeholder="e.g. daily-summary"
									disabled={!!editingJob}
									autoComplete="off"
								/>
							</Field>

							<Field label="Prompt">
								<TextArea
									value={formData.prompt}
									onChange={(e) =>
										setFormData((d) => ({...d, prompt: e.target.value}))
									}
									placeholder="What should the agent do on each run?"
									rows={4}
								/>
							</Field>

							<Field label="Schedule">
								<div className="flex flex-col gap-3">
									<div className="flex gap-1 rounded-lg bg-app-lightBox p-0.5">
										<button
											type="button"
											className={`flex-1 rounded-md px-3 py-1.5 text-xs font-medium transition-colors ${
												formData.schedule_mode === "cron"
													? "bg-app-dark-box text-ink shadow-sm"
													: "text-ink-faint hover:text-ink"
											}`}
											onClick={() =>
												setFormData((d) => ({...d, schedule_mode: "cron"}))
											}
										>
											Cron Expression
										</button>
										<button
											type="button"
											className={`flex-1 rounded-md px-3 py-1.5 text-xs font-medium transition-colors ${
												formData.schedule_mode === "interval"
													? "bg-app-dark-box text-ink shadow-sm"
													: "text-ink-faint hover:text-ink"
											}`}
											onClick={() =>
												setFormData((d) => ({...d, schedule_mode: "interval"}))
											}
										>
											Interval
										</button>
									</div>

									{formData.schedule_mode === "cron" && (
										<div className="flex flex-col gap-2">
											<Input
												value={formData.cron_expr}
												onChange={(e) =>
													setFormData((d) => ({
														...d,
														cron_expr: e.target.value,
													}))
												}
												placeholder="0 9 * * *"
												className="font-mono"
												autoComplete="off"
											/>
											<div className="flex flex-wrap gap-1.5">
												{CRON_PRESETS.map((preset) => (
													<button
														key={preset.value}
														type="button"
														className={`rounded-md border px-2 py-1 text-tiny transition-colors ${
															formData.cron_expr === preset.value
																? "border-accent/50 bg-accent/10 text-accent"
																: "border-app-line bg-app-dark-box text-ink-faint hover:border-app-line/80 hover:text-ink-dull"
														}`}
														onClick={() =>
															setFormData((d) => ({
																...d,
																cron_expr: preset.value,
															}))
														}
													>
														{preset.label}
													</button>
												))}
											</div>
											<p className="text-tiny text-ink-faint">
												min hour day-of-month month day-of-week
											</p>
										</div>
									)}

									{formData.schedule_mode === "interval" && (
										<div>
											<div className="flex items-center gap-2">
												<span className="text-sm text-ink-faint">Every</span>
												<NumberStepper
													value={formData.interval_value}
													onChange={(v) =>
														setFormData((d) => ({...d, interval_value: v}))
													}
													min={1}
													max={999}
														/>
												<SelectRoot
													value={formData.interval_unit}
													onValueChange={(value) =>
														setFormData((d) => ({
															...d,
															interval_unit: value as
																| "minutes"
																| "hours"
																| "days",
														}))
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
												</SelectRoot>
											</div>
											<p className="mt-1 text-tiny text-ink-faint">
												May drift. Prefer cron expressions.
											</p>
										</div>
									)}
								</div>
							</Field>
						</div>

						{/* Right column — delivery & options */}
						<div className="flex flex-col gap-4">
							<Field label="Delivery Target">
								<SelectRoot
									value={formData.delivery_target}
									onValueChange={(value) => {
										if (value === "__custom__") {
											setFormData((d) => ({...d, delivery_target: ""}));
										} else {
											setFormData((d) => ({...d, delivery_target: value}));
										}
									}}
								>
									<SelectTrigger>
										<SelectValue placeholder="Select a channel..." />
									</SelectTrigger>
									<SelectContent>
										{deliveryTargets.map((target) => (
											<SelectItem key={target.value} value={target.value}>
												<span>{target.label}</span>
												<span className="ml-1.5 text-ink-faint">
													{target.value}
												</span>
											</SelectItem>
										))}
										<SelectItem value="__custom__">Custom...</SelectItem>
									</SelectContent>
								</SelectRoot>
								{(formData.delivery_target === "" ||
									!deliveryTargets.some(
										(t) => t.value === formData.delivery_target,
									)) && (
									<Input
										value={formData.delivery_target}
										onChange={(e) =>
											setFormData((d) => ({
												...d,
												delivery_target: e.target.value,
											}))
										}
										placeholder="adapter:target (e.g. discord:123456)"
										className="mt-1.5"
									/>
								)}
							</Field>

							<Field label="Active Hours (optional)">
								<div className="flex items-center gap-2">
									<NumberStepper
										value={parseInt(formData.active_start_hour) || 0}
										onChange={(v) =>
											setFormData((d) => ({
												...d,
												active_start_hour: v.toString(),
											}))
										}
										min={0}
										max={23}
										suffix="h"
									/>
									<span className="text-sm text-ink-faint">to</span>
									<NumberStepper
										value={parseInt(formData.active_end_hour) || 23}
										onChange={(v) =>
											setFormData((d) => ({
												...d,
												active_end_hour: v.toString(),
											}))
										}
										min={0}
										max={23}
										suffix="h"
									/>
								</div>
								<p className="mt-1 text-tiny text-ink-faint">
									Only run during these hours (24h)
								</p>
							</Field>

							<Field label="Timeout (optional)">
								<Input
									value={formData.timeout_secs}
									onChange={(e) =>
										setFormData((d) => ({
											...d,
											timeout_secs: e.target.value.replace(/\D/g, ""),
										}))
									}
									placeholder="120"
									className="w-32"
								/>
								<p className="mt-1 text-tiny text-ink-faint">
									Max seconds per run (default 1500)
								</p>
							</Field>

							<div className="flex items-center justify-between">
								<Label>Enabled</Label>
								<Switch
									checked={formData.enabled}
									onCheckedChange={(checked) =>
										setFormData((d) => ({...d, enabled: checked}))
									}
									size="lg"
								/>
							</div>

							<div className="flex items-center justify-between">
								<Label>Run Once</Label>
								<Switch
									checked={formData.run_once}
									onCheckedChange={(checked) =>
										setFormData((d) => ({...d, run_once: checked}))
									}
									size="lg"
								/>
							</div>
						</div>
					</div>

					<div className="mt-2 flex justify-end gap-2">
						<Button variant="gray" size="sm" onClick={closeModal}>
							Cancel
						</Button>
						<Button
							size="sm"
							onClick={handleSave}
							disabled={
								!formData.id.trim() ||
								!formData.prompt.trim() ||
								!formData.delivery_target.trim() ||
								(formData.schedule_mode === "cron" &&
									!formData.cron_expr.trim())
							}
						>
							{editingJob ? "Save Changes" : "Create Job"}
						</Button>
					</div>
				</DialogContent>
			</DialogRoot>

			{/* Delete Confirmation */}
			<DialogRoot
				open={!!deleteConfirmId}
				onOpenChange={(open) => !open && setDeleteConfirmId(null)}
			>
				<DialogContent>
					<DialogHeader>
						<DialogTitle>Delete Cron Job?</DialogTitle>
					</DialogHeader>
					<p className="mb-4 text-sm text-ink-dull">
						This will permanently delete{" "}
						<code className="rounded bg-app-dark-box px-1.5 py-0.5 text-ink">
							{deleteConfirmId}
						</code>{" "}
						and its execution history.
					</p>
					<div className="flex justify-end gap-2">
						<Button
							variant="gray"
							size="sm"
							onClick={() => setDeleteConfirmId(null)}
						>
							Cancel
						</Button>
						<Button
							variant="accent"
							size="sm"
							onClick={() =>
								deleteConfirmId && deleteMutation.mutate(deleteConfirmId)
							}
						>
							Delete
						</Button>
					</div>
				</DialogContent>
			</DialogRoot>
		</div>
	);
}

// -- Sub-components --

function Field({label, children}: {label: string; children: React.ReactNode}) {
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
	const totalRuns = job.execution_success_count + job.execution_failure_count;
	const executionSuccessRate =
		totalRuns > 0
			? Math.round((job.execution_success_count / totalRuns) * 100)
			: null;
	const schedule = formatCronSchedule(job.cron_expr, job.interval_secs);

	return (
		<div className="overflow-hidden rounded-xl border border-app-line bg-app-dark-box">
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
						<code className="rounded bg-app-lightBox/60 px-1.5 py-0.5 font-mono text-tiny text-ink-dull">
							{schedule}
						</code>
						{job.active_hours && (
							<span className="text-tiny text-ink-faint">
								{String(job.active_hours[0]).padStart(2, "0")}:00-
								{String(job.active_hours[1]).padStart(2, "0")}:00
							</span>
						)}
						{!job.enabled && (
							<span className="rounded bg-gray-500/20 px-1.5 py-0.5 text-tiny text-gray-400">
								disabled
							</span>
						)}
						{job.run_once && (
							<span className="rounded bg-accent/20 px-1.5 py-0.5 text-tiny text-accent">
								one-time
							</span>
						)}
					</div>

					<p className="mb-2 text-sm text-ink-dull" title={job.prompt}>
						{job.prompt.length > 120
							? job.prompt.slice(0, 120) + "..."
							: job.prompt}
					</p>

					<div className="flex flex-wrap items-center gap-3 text-tiny text-ink-faint">
						<span>{job.delivery_target}</span>
						{job.last_executed_at && (
							<>
								<span className="text-ink-faint/50">·</span>
								<span>ran {formatTimeAgo(job.last_executed_at)}</span>
							</>
						)}
						{executionSuccessRate !== null && (
							<>
								<span className="text-ink-faint/50">·</span>
								<span
									className={
										executionSuccessRate >= 90
											? "text-green-500"
											: executionSuccessRate >= 50
												? "text-yellow-500"
												: "text-red-500"
									}
								>
									exec {executionSuccessRate}% ({job.execution_success_count}/
									{totalRuns})
								</span>
							</>
						)}
						{(job.delivery_success_count > 0 ||
							job.delivery_failure_count > 0 ||
							job.delivery_skipped_count > 0) && (
							<>
								<span className="text-ink-faint/50">·</span>
								<span className="text-ink-faint">
									delivery {job.delivery_success_count} sent
									{job.delivery_failure_count > 0
										? `, ${job.delivery_failure_count} failed`
										: ""}
									{job.delivery_skipped_count > 0
										? `, ${job.delivery_skipped_count} skipped`
										: ""}
								</span>
							</>
						)}
					</div>
				</div>

				{/* Actions */}
				<div className="flex items-center gap-0.5">
					<Button
						title={job.enabled ? "Disable" : "Enable"}
						onClick={onToggleEnabled}
						disabled={isToggling}
						variant="gray"
						size="sm"
					>
						{job.enabled ? (
							<Pause className="h-3.5 w-3.5" />
						) : (
							<Play className="h-3.5 w-3.5" />
						)}
					</Button>
					<Button
						title="Run now"
						onClick={onTrigger}
						disabled={isTriggering || !job.enabled}
						variant="gray"
						size="sm"
					>
						<Lightning className="h-3.5 w-3.5" />
					</Button>
					<Button title="Edit" onClick={onEdit} variant="gray" size="sm">
						<PencilSimple className="h-3.5 w-3.5" />
					</Button>
					<Button
						title="Delete"
						onClick={onDelete}
						variant="gray"
						size="sm"
						className="hover:text-red-400"
					>
						<Trash className="h-3.5 w-3.5" />
					</Button>
				</div>
			</div>

			{/* Execution history (expandable) */}
			{isExpanded && (
				<div className="border-t border-app-line bg-app-dark-box/50 px-4 py-3">
					<JobExecutions agentId={agentId} jobId={job.id} />
				</div>
			)}

			{/* Expand toggle */}
			<button
				type="button"
				onClick={onToggleExpand}
				className="flex w-full items-center justify-center gap-1.5 border-t border-app-line/50 px-3 py-1.5 text-tiny text-ink-faint transition-colors hover:bg-app-lightBox/30 hover:text-ink-dull"
			>
				{isExpanded ? (
					<CaretUp className="h-3 w-3" />
				) : (
					<CaretDown className="h-3 w-3" />
				)}
				{isExpanded ? "Hide history" : "Show history"}
			</button>
		</div>
	);
}

function JobExecutions({agentId, jobId}: {agentId: string; jobId: string}) {
	const {data, isLoading} = useQuery({
		queryKey: ["cron-executions", agentId, jobId],
		queryFn: () => api.cronExecutions(agentId, {cron_id: jobId, limit: 10}),
	});

	if (isLoading) {
		return (
			<div className="flex items-center justify-center py-3">
				<div className="h-2 w-2 animate-pulse rounded-full bg-accent" />
			</div>
		);
	}

	if (!data?.executions.length) {
		return (
			<p className="py-2 text-tiny text-ink-faint">No execution history yet.</p>
		);
	}

	return (
		<div className="flex flex-col gap-1">
			{data.executions.map((execution) => {
				const statusTone = !execution.execution_succeeded
					? "bg-red-500"
					: execution.delivery_attempted &&
						  execution.delivery_succeeded === false
						? "bg-yellow-500"
						: execution.delivery_attempted &&
							  execution.delivery_succeeded === true
							? "bg-green-500"
							: "bg-gray-500";
				const detail =
					execution.delivery_error ??
					execution.execution_error ??
					execution.result_summary;
				const deliveryLabel = !execution.delivery_attempted
					? "no delivery"
					: execution.delivery_succeeded === true
						? "delivered"
						: execution.delivery_succeeded === false
							? "delivery failed"
							: "delivery unknown";

				return (
					<div
						key={execution.id}
						className="flex items-center gap-3 rounded-lg px-3 py-1.5"
					>
						<span className={`h-1.5 w-1.5 rounded-full ${statusTone}`} />
						<span className="text-tiny tabular-nums text-ink-faint">
							{formatTimeAgo(execution.executed_at)}
						</span>
						<span
							className={`rounded px-1.5 py-0.5 text-tiny ${
								execution.execution_succeeded
									? "bg-green-500/10 text-green-400"
									: "bg-red-500/10 text-red-400"
							}`}
						>
							{execution.execution_succeeded ? "exec ok" : "exec failed"}
						</span>
						<span
							className={`rounded px-1.5 py-0.5 text-tiny ${
								!execution.delivery_attempted
									? "bg-app-lightBox text-ink-faint"
									: execution.delivery_succeeded === true
										? "bg-green-500/10 text-green-400"
										: execution.delivery_succeeded === false
											? "bg-yellow-500/10 text-yellow-300"
											: "bg-app-lightBox text-ink-faint"
							}`}
						>
							{deliveryLabel}
						</span>
						{detail && (
							<span className="min-w-0 flex-1 truncate text-tiny text-ink-dull">
								{detail}
							</span>
						)}
					</div>
				);
			})}
		</div>
	);
}
