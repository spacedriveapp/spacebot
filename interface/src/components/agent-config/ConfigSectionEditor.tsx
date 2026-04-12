import {useCallback, useEffect, useState} from "react";
import {
	Input,
	NumberStepper,
	SelectRoot,
	SelectTrigger,
	SelectValue,
	SelectContent,
	SelectItem,
	Switch,
} from "@spacedrive/primitives";
import {ModelSelect} from "@/components/ModelSelect";
import {TagInput} from "@/components/TagInput";
import {supportsAdaptiveThinking} from "./utils";
import {SANDBOX_DEFAULTS} from "./constants";
import type {ConfigSectionEditorProps} from "./types";
import type {
	BrowserUpdate,
	CoalesceUpdate,
	CompactionUpdate,
	CortexUpdate,
	MemoryPersistenceUpdate,
	ProjectsUpdate,
	RoutingUpdate,
	SandboxUpdate,
	TuningUpdate,
} from "@/api/client";

// Union of every section's update type. Each instance of the editor renders
// exactly one section, so the active section decides which fields are present.
type ConfigValues = RoutingUpdate &
	TuningUpdate &
	CompactionUpdate &
	CortexUpdate &
	CoalesceUpdate &
	MemoryPersistenceUpdate &
	BrowserUpdate &
	SandboxUpdate &
	ProjectsUpdate;

export function ConfigSectionEditor({
	sectionId,
	label,
	description,
	detail,
	config,
	onDirtyChange,
	saveHandlerRef,
	onSave,
}: ConfigSectionEditorProps) {
	const sandbox = config.sandbox ?? SANDBOX_DEFAULTS;

	const [localValues, setLocalValues] = useState<ConfigValues>(() => {
		switch (sectionId) {
			case "routing":
				return {...config.routing} as ConfigValues;
			case "tuning":
				return {...config.tuning} as ConfigValues;
			case "compaction":
				return {...config.compaction} as ConfigValues;
			case "cortex":
				return {...config.cortex} as ConfigValues;
			case "coalesce":
				return {...config.coalesce} as ConfigValues;
			case "memory":
				return {...config.memory_persistence} as ConfigValues;
			case "browser":
				return {...config.browser} as ConfigValues;
			case "sandbox":
				return {
					mode: sandbox.mode,
					writable_paths: sandbox.writable_paths,
				} as ConfigValues;
			case "projects":
				return {...config.projects} as ConfigValues;
			default:
				return {};
		}
	});

	const [localDirty, setLocalDirty] = useState(false);

	useEffect(() => {
		onDirtyChange(localDirty);
	}, [localDirty, onDirtyChange]);

	useEffect(() => {
		if (!localDirty) {
			switch (sectionId) {
				case "routing":
					setLocalValues({...config.routing});
					break;
				case "tuning":
					setLocalValues({...config.tuning});
					break;
				case "compaction":
					setLocalValues({...config.compaction});
					break;
				case "cortex":
					setLocalValues({...config.cortex});
					break;
				case "coalesce":
					setLocalValues({...config.coalesce});
					break;
				case "memory":
					setLocalValues({...config.memory_persistence});
					break;
				case "browser":
					setLocalValues({...config.browser});
					break;
				case "sandbox":
					setLocalValues({
						mode: sandbox.mode,
						writable_paths: sandbox.writable_paths,
					});
					break;
				case "projects":
					setLocalValues({...config.projects});
					break;
			}
		}
	}, [config, sectionId, localDirty]);

	const handleChange = useCallback(
		(field: string, value: string | number | boolean | string[]) => {
			setLocalValues((prev) => ({...prev, [field]: value}));
			setLocalDirty(true);
		},
		[],
	);

	const handleSave = useCallback(() => {
		onSave({[sectionId]: localValues});
		setLocalDirty(false);
	}, [onSave, sectionId, localValues]);

	const handleRevert = useCallback(() => {
		switch (sectionId) {
			case "routing":
				setLocalValues({...config.routing});
				break;
			case "tuning":
				setLocalValues({...config.tuning});
				break;
			case "compaction":
				setLocalValues({...config.compaction});
				break;
			case "cortex":
				setLocalValues({...config.cortex});
				break;
			case "coalesce":
				setLocalValues({...config.coalesce});
				break;
			case "memory":
				setLocalValues({...config.memory_persistence});
				break;
			case "browser":
				setLocalValues({...config.browser});
				break;
			case "sandbox":
				setLocalValues({
					mode: sandbox.mode,
					writable_paths: sandbox.writable_paths,
				});
				break;
			case "projects":
				setLocalValues({...config.projects});
				break;
		}
		setLocalDirty(false);
	}, [config, sectionId]);

	useEffect(() => {
		saveHandlerRef.current.save = handleSave;
		saveHandlerRef.current.revert = handleRevert;
		return () => {
			saveHandlerRef.current.save = undefined;
			saveHandlerRef.current.revert = undefined;
		};
	}, [handleSave, handleRevert]);

	const renderFields = () => {
		switch (sectionId) {
			case "routing": {
				const modelSlots: Array<{
					key: "channel" | "branch" | "worker" | "compactor" | "cortex" | "voice";
					label: string;
					description: string;
				}> = [
					{
						key: "channel",
						label: "Channel Model",
						description: "Model for user-facing channels",
					},
					{
						key: "branch",
						label: "Branch Model",
						description: "Model for thinking branches",
					},
					{
						key: "worker",
						label: "Worker Model",
						description: "Model for task workers",
					},
					{
						key: "compactor",
						label: "Compactor Model",
						description: "Model for summarization",
					},
					{
						key: "cortex",
						label: "Cortex Model",
						description: "Model for system observation",
					},
					{
						key: "voice",
						label: "Voice Model",
						description: "Model for transcribing audio attachments",
					},
				];
				return (
					<div className="grid gap-4">
						{modelSlots.map(({key, label, description}) => {
							const modelValue = localValues[key] ?? "";
							const thinkingKey =
								key === "voice" ? null : (`${key}_thinking_effort` as const);
							return (
								<div key={key} className="flex flex-col gap-2">
									<ModelSelect
										label={label}
										description={description}
										value={modelValue}
										onChange={(v) => handleChange(key, v)}
										capability={
											key === "voice" ? "voice_transcription" : undefined
										}
									/>
									{thinkingKey && supportsAdaptiveThinking(modelValue) && (
										<div className="ml-4 flex flex-col gap-1">
											<label className="text-xs font-medium text-ink-dull">
												Thinking Effort
											</label>
											<SelectRoot
												value={localValues[thinkingKey] ?? "auto"}
												onValueChange={(value) =>
													handleChange(thinkingKey, value)
												}
											>
												<SelectTrigger className="border-app-line/50 bg-app-dark-box/30">
													<SelectValue />
												</SelectTrigger>
												<SelectContent>
													<SelectItem value="auto">Auto</SelectItem>
													<SelectItem value="max">Max</SelectItem>
													<SelectItem value="high">High</SelectItem>
													<SelectItem value="medium">Medium</SelectItem>
													<SelectItem value="low">Low</SelectItem>
												</SelectContent>
											</SelectRoot>
										</div>
									)}
								</div>
							);
						})}
						<NumberStepper
							label="Rate Limit Cooldown"
							description="Seconds to deprioritize rate-limited models"
							value={localValues.rate_limit_cooldown_secs ?? 0}
							onChange={(v) => handleChange("rate_limit_cooldown_secs", v)}
							min={0}
							suffix="s"
						/>
					</div>
				);
			}
			case "tuning":
				return (
					<div className="grid gap-4">
						<NumberStepper
							label="Max Concurrent Branches"
							description="Maximum branches per channel"
							value={localValues.max_concurrent_branches as number}
							onChange={(v) => handleChange("max_concurrent_branches", v)}
							min={1}
							max={20}
						/>
						<NumberStepper
							label="Max Concurrent Workers"
							description="Maximum workers per channel"
							value={localValues.max_concurrent_workers as number}
							onChange={(v) => handleChange("max_concurrent_workers", v)}
							min={1}
							max={20}
						/>
						<NumberStepper
							label="Max Turns"
							description="Max LLM turns per channel message"
							value={localValues.max_turns as number}
							onChange={(v) => handleChange("max_turns", v)}
							min={1}
							max={50}
						/>
						<NumberStepper
							label="Branch Max Turns"
							description="Max turns for thinking branches"
							value={localValues.branch_max_turns as number}
							onChange={(v) => handleChange("branch_max_turns", v)}
							min={1}
							max={100}
						/>
						<NumberStepper
							label="Context Window"
							description="Context window size in tokens"
							value={localValues.context_window as number}
							onChange={(v) => handleChange("context_window", v)}
							min={1000}
							step={1000}
							suffix=" tokens"
						/>
						<NumberStepper
							label="History Backfill"
							description="Messages to fetch on new channel"
							value={localValues.history_backfill_count as number}
							onChange={(v) => handleChange("history_backfill_count", v)}
							min={0}
							max={500}
							suffix=" messages"
						/>
					</div>
				);
			case "compaction":
				return (
					<div className="grid gap-4">
						<NumberStepper
							label="Background Threshold"
							description="Start background summarization (fraction of context window)"
							value={localValues.background_threshold as number}
							onChange={(v) => handleChange("background_threshold", v)}
							min={0}
							max={1}
							step={0.01}
						/>
						<NumberStepper
							label="Aggressive Threshold"
							description="Start aggressive summarization"
							value={localValues.aggressive_threshold as number}
							onChange={(v) => handleChange("aggressive_threshold", v)}
							min={0}
							max={1}
							step={0.01}
						/>
						<NumberStepper
							label="Emergency Threshold"
							description="Emergency truncation (no LLM, drop oldest 50%)"
							value={localValues.emergency_threshold as number}
							onChange={(v) => handleChange("emergency_threshold", v)}
							min={0}
							max={1}
							step={0.01}
						/>
					</div>
				);
			case "cortex":
				return (
					<div className="grid gap-4">
						<NumberStepper
							label="Tick Interval"
							description="How often the cortex checks system state"
							value={localValues.tick_interval_secs as number}
							onChange={(v) => handleChange("tick_interval_secs", v)}
							min={1}
							suffix="s"
						/>
						<NumberStepper
							label="Worker Timeout"
							description="Worker timeout before cancellation"
							value={localValues.worker_timeout_secs as number}
							onChange={(v) => handleChange("worker_timeout_secs", v)}
							min={10}
							suffix="s"
						/>
						<NumberStepper
							label="Branch Timeout"
							description="Branch timeout before cancellation"
							value={localValues.branch_timeout_secs as number}
							onChange={(v) => handleChange("branch_timeout_secs", v)}
							min={5}
							suffix="s"
						/>
						<NumberStepper
							label="Circuit Breaker"
							description="Consecutive failures before auto-disable"
							value={localValues.circuit_breaker_threshold as number}
							onChange={(v) => handleChange("circuit_breaker_threshold", v)}
							min={1}
							max={10}
						/>
						<NumberStepper
							label="Bulletin Interval"
							description="Seconds between memory bulletin refreshes"
							value={localValues.bulletin_interval_secs as number}
							onChange={(v) => handleChange("bulletin_interval_secs", v)}
							min={60}
							suffix="s"
						/>
						<NumberStepper
							label="Bulletin Max Words"
							description="Target word count for memory bulletin"
							value={localValues.bulletin_max_words as number}
							onChange={(v) => handleChange("bulletin_max_words", v)}
							min={100}
							max={5000}
							suffix=" words"
						/>
						<NumberStepper
							label="Bulletin Max Turns"
							description="Max LLM turns for bulletin generation"
							value={localValues.bulletin_max_turns as number}
							onChange={(v) => handleChange("bulletin_max_turns", v)}
							min={5}
							max={50}
						/>
					</div>
				);
			case "coalesce":
				return (
					<div className="grid gap-4">
						<ConfigToggleField
							label="Enabled"
							description="Enable message coalescing for multi-user channels"
							value={localValues.enabled as boolean}
							onChange={(v) => handleChange("enabled", v)}
						/>
						<NumberStepper
							label="Debounce"
							description="Initial debounce window after first message"
							value={localValues.debounce_ms as number}
							onChange={(v) => handleChange("debounce_ms", v)}
							min={100}
							max={10000}
							suffix="ms"
						/>
						<NumberStepper
							label="Max Wait"
							description="Maximum time to wait before flushing"
							value={localValues.max_wait_ms as number}
							onChange={(v) => handleChange("max_wait_ms", v)}
							min={500}
							max={30000}
							suffix="ms"
						/>
						<NumberStepper
							label="Min Messages"
							description="Min messages to trigger coalesce mode"
							value={localValues.min_messages as number}
							onChange={(v) => handleChange("min_messages", v)}
							min={1}
							max={10}
						/>
						<ConfigToggleField
							label="Multi-User Only"
							description="Apply only to multi-user conversations (skip for DMs)"
							value={localValues.multi_user_only as boolean}
							onChange={(v) => handleChange("multi_user_only", v)}
						/>
					</div>
				);
			case "memory":
				return (
					<div className="grid gap-4">
						<ConfigToggleField
							label="Enabled"
							description="Enable automatic memory persistence branches"
							value={localValues.enabled as boolean}
							onChange={(v) => handleChange("enabled", v)}
						/>
						<NumberStepper
							label="Message Interval"
							description="Number of user messages between automatic saves"
							value={localValues.message_interval as number}
							onChange={(v) => handleChange("message_interval", v)}
							min={1}
							max={200}
							suffix=" messages"
						/>
					</div>
				);
			case "browser":
				return (
					<div className="grid gap-4">
						<ConfigToggleField
							label="Enabled"
							description="Enable browser automation tools for workers"
							value={localValues.enabled as boolean}
							onChange={(v) => handleChange("enabled", v)}
						/>
						<ConfigToggleField
							label="Headless"
							description="Run Chrome in headless mode"
							value={localValues.headless as boolean}
							onChange={(v) => handleChange("headless", v)}
						/>
						<ConfigToggleField
							label="JavaScript Evaluation"
							description="Allow JavaScript evaluation via browser tool"
							value={localValues.evaluate_enabled as boolean}
							onChange={(v) => handleChange("evaluate_enabled", v)}
						/>
						<ConfigToggleField
							label="Persist Session"
							description="Keep the browser alive across worker lifetimes. Cookies, tabs, and login sessions survive between tasks. Requires agent restart to take effect."
							value={localValues.persist_session as boolean}
							onChange={(v) => handleChange("persist_session", v)}
						/>
						<div className="flex flex-col gap-1.5">
							<label className="text-sm font-medium text-ink">Close Policy</label>
							<p className="text-tiny text-ink-faint">
								What happens when a worker calls &quot;close&quot; or finishes.
							</p>
							<SelectRoot
								value={localValues.close_policy as string}
								onValueChange={(v) => handleChange("close_policy", v)}
							>
								<SelectTrigger className="border-app-line/50 bg-app-dark-box/30">
									<SelectValue />
								</SelectTrigger>
								<SelectContent>
									<SelectItem value="close_browser">Close Browser</SelectItem>
									<SelectItem value="close_tabs">Close Tabs</SelectItem>
									<SelectItem value="detach">Detach</SelectItem>
								</SelectContent>
							</SelectRoot>
						</div>
					</div>
				);
			case "sandbox":
				return (
					<div className="grid gap-4">
						<div className="flex flex-col gap-1.5">
							<label className="text-sm font-medium text-ink">Mode</label>
							<p className="text-tiny text-ink-faint">
								Kernel-enforced filesystem containment for shell subprocesses.
							</p>
							<SelectRoot
								value={localValues.mode as string}
								onValueChange={(v) => handleChange("mode", v)}
							>
								<SelectTrigger className="border-app-line/50 bg-app-dark-box/30">
									<SelectValue />
								</SelectTrigger>
								<SelectContent>
									<SelectItem value="enabled">Enabled</SelectItem>
									<SelectItem value="disabled">Disabled</SelectItem>
								</SelectContent>
							</SelectRoot>
						</div>
						<div className="flex flex-col gap-1.5">
							<label className="text-sm font-medium text-ink">
								Extra Allowed Paths
							</label>
							<p className="text-tiny text-ink-faint">
								Additional directories workers can read and write beyond the
								workspace. The workspace is always accessible. Press Enter to
								add a path.
							</p>
							<TagInput
								value={(localValues.writable_paths as string[]) ?? []}
								onChange={(paths) => handleChange("writable_paths", paths)}
								placeholder="/home/user/projects/myapp"
							/>
						</div>
					</div>
				);
			case "projects":
				return (
					<div className="grid gap-4">
						<ConfigToggleField
							label="Use Worktrees"
							description="Enable git worktree support for parallel feature branches within projects."
							value={localValues.use_worktrees as boolean}
							onChange={(v) => handleChange("use_worktrees", v)}
						/>
						<div className="flex flex-col gap-1.5">
							<label className="text-sm font-medium text-ink">
								Worktree Name Template
							</label>
							<p className="text-tiny text-ink-faint">
								Template for naming new worktrees. Use{" "}
								<code className="text-ink-dull">{"{branch}"}</code> for the
								branch name.
							</p>
							<Input
								value={localValues.worktree_name_template as string}
								onChange={(e) =>
									handleChange("worktree_name_template", e.target.value)
								}
								placeholder="{branch}"
								className="border-app-line/50 bg-app-dark-box/30"
							/>
						</div>
						<ConfigToggleField
							label="Auto-Create Worktrees"
							description="Automatically create a worktree when spawning a worker on a new branch."
							value={localValues.auto_create_worktrees as boolean}
							onChange={(v) => handleChange("auto_create_worktrees", v)}
						/>
						<ConfigToggleField
							label="Auto-Discover Repos"
							description="Scan the project root for git repositories when a project is created."
							value={localValues.auto_discover_repos as boolean}
							onChange={(v) => handleChange("auto_discover_repos", v)}
						/>
						<ConfigToggleField
							label="Auto-Discover Worktrees"
							description="Scan discovered repos for existing git worktrees on project creation."
							value={localValues.auto_discover_worktrees as boolean}
							onChange={(v) => handleChange("auto_discover_worktrees", v)}
						/>
						<NumberStepper
							label="Disk Usage Warning"
							description={`Warn when total project disk usage exceeds this threshold (${Math.round((localValues.disk_usage_warning_threshold as number) / 1073741824)} GB)`}
							value={localValues.disk_usage_warning_threshold as number}
							onChange={(v) => handleChange("disk_usage_warning_threshold", v)}
							min={0}
							step={1073741824}
						/>
					</div>
				);
			default:
				return null;
		}
	};

	return (
		<>
			<div className="flex items-center justify-between border-b border-app-line/50 bg-app-dark-box/20 px-5 py-2.5">
				<div className="flex items-center gap-3">
					<h3 className="text-sm font-medium text-ink">{label}</h3>
					<span className="text-tiny text-ink-faint">{description}</span>
				</div>
				{localDirty ? (
					<span className="text-tiny text-amber-400">Unsaved changes</span>
				) : (
					<span className="text-tiny text-ink-faint/50">
						Changes saved to config.toml
					</span>
				)}
			</div>
			<div className="flex-1 overflow-y-auto px-8 py-8">
				<div className="mb-6 rounded-lg border border-app-line/30 bg-app-dark-box/20 px-5 py-4">
					<p className="text-sm leading-relaxed text-ink-dull">{detail}</p>
				</div>
				{renderFields()}
			</div>
		</>
	);
}

function ConfigToggleField({
	label,
	description,
	value,
	onChange,
}: {
	label: string;
	description: string;
	value: boolean;
	onChange: (value: boolean) => void;
}) {
	return (
		<div className="flex items-center justify-between py-2">
			<div className="flex flex-col gap-0.5">
				<label className="text-sm font-medium text-ink">{label}</label>
				<p className="text-tiny text-ink-faint">{description}</p>
			</div>
			<Switch checked={value} onCheckedChange={onChange} size="lg" />
		</div>
	);
}
