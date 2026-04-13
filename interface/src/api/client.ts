export const BASE_PATH: string = (window as any).__SPACEBOT_BASE_PATH || "";

/**
 * Dynamic server URL for the Tauri desktop app. When set, all API
 * requests target this absolute URL (e.g. "http://localhost:19898/api/...").
 * When empty the app uses relative paths (same-origin / proxy mode).
 */
let _serverUrl = "";
export function setServerUrl(url: string) {
	_serverUrl = url.replace(/\/+$/, "");
}
export function getServerUrl(): string {
	return _serverUrl;
}

function getApiBase(): string {
	if (_serverUrl) return `${_serverUrl}/api`;
	return BASE_PATH + "/api";
}

import type * as Types from "./types";

// Re-export commonly used types from schema for backward compatibility
// Only re-export types that don't have local definitions with extra fields
export type {
	// System
	StatusResponse,
	InstanceOverviewResponse,
	// Channels
	ChannelResponse,
	ChannelsResponse,
	MessagesResponse,
	TimelineItem,
	// Workers
	WorkerListItem,
	WorkerListResponse,
	WorkerDetailResponse,
	TranscriptStep,
	// Agents
	AgentInfo,
	AgentsResponse,
	AgentSummary,
	AgentOverviewResponse,
	AgentProfile,
	AgentProfileResponse,
	CronJobInfo,
	// Memory (schema types only)
	Memory,
	Association,
	RelationType,
	MemoryGraphResponse,
	MemoryGraphNeighborsResponse,
	// Cortex chat (schema types)
	CortexChatMessage,
	CortexChatThread,
	CortexChatToolCall,
	CortexChatMessagesResponse,
	CortexChatThreadsResponse,
	// Config (schema types only)
	GlobalSettingsResponse,
	GlobalSettingsUpdateResponse,
	RawConfigResponse,
	RawConfigUpdateResponse,
	// Providers
	ProvidersResponse,
	ProviderUpdateResponse,
	ProviderModelTestResponse,
	OpenAiOAuthBrowserStartResponse,
	OpenAiOAuthBrowserStatusResponse,
	ModelInfo,
	ModelsResponse,
	// Ingest
	IngestFileInfo,
	IngestFilesResponse,
	IngestUploadResponse,
	IngestDeleteResponse,
	// Messaging
	PlatformStatus,
	AdapterInstanceStatus,
	MessagingStatusResponse,
	CreateMessagingInstanceRequest,
	MessagingInstanceActionResponse,
} from "./types";

// Import and re-export Topology types from schema
import type {
	TopologyAgent,
	TopologyLink,
	TopologyGroup,
	TopologyHuman,
	TopologyResponse,
} from "./types";

export type { TopologyAgent, TopologyLink, TopologyGroup, TopologyHuman, TopologyResponse };

// Aliases for backward compatibility
export type ChannelInfo = Types.ChannelResponse;
export type WorkerRunInfo = Types.WorkerListItem;
export type AssociationItem = Types.Association;

export type ProcessType = "channel" | "branch" | "worker";

export interface InboundMessageEvent {
	type: "inbound_message";
	agent_id: string;
	channel_id: string;
	sender_name?: string | null;
	sender_id: string;
	text: string;
}

export interface OutboundMessageEvent {
	type: "outbound_message";
	agent_id: string;
	channel_id: string;
	text: string;
}

export interface OutboundMessageDeltaEvent {
	type: "outbound_message_delta";
	agent_id: string;
	channel_id: string;
	text_delta: string;
	aggregated_text: string;
}

export interface TypingStateEvent {
	type: "typing_state";
	agent_id: string;
	channel_id: string;
	is_typing: boolean;
}

export interface WorkerStartedEvent {
	type: "worker_started";
	agent_id: string;
	channel_id: string | null;
	worker_id: string;
	task: string;
	worker_type?: string;
	interactive?: boolean;
}

export interface WorkerStatusEvent {
	type: "worker_status";
	agent_id: string;
	channel_id: string | null;
	worker_id: string;
	status: string;
}

export interface WorkerIdleEvent {
	type: "worker_idle";
	agent_id: string;
	channel_id: string | null;
	worker_id: string;
}

export interface WorkerCompletedEvent {
	type: "worker_completed";
	agent_id: string;
	channel_id: string | null;
	worker_id: string;
	result: string;
	success?: boolean;
}

export interface BranchStartedEvent {
	type: "branch_started";
	agent_id: string;
	channel_id: string;
	branch_id: string;
	description: string;
}

export interface BranchCompletedEvent {
	type: "branch_completed";
	agent_id: string;
	channel_id: string;
	branch_id: string;
	conclusion: string;
}

export interface ToolStartedEvent {
	type: "tool_started";
	agent_id: string;
	channel_id: string | null;
	process_type: ProcessType;
	process_id: string;
	tool_name: string;
	args: string;
}

export interface ToolCompletedEvent {
	type: "tool_completed";
	agent_id: string;
	channel_id: string | null;
	process_type: ProcessType;
	process_id: string;
	tool_name: string;
	result: string;
}

// -- Agent link events --

export interface AgentMessageEvent {
	from_agent_id: string;
	to_agent_id: string;
	link_id: string;
	channel_id: string;
}

// -- OpenCode live transcript part types --

export type OpenCodeToolState =
	| { status: "pending" }
	| { status: "running"; title?: string; input?: string }
	| { status: "completed"; title?: string; input?: string; output?: string }
	| { status: "error"; error?: string };

export type OpenCodePart =
	| { type: "text"; id: string; text: string }
	| { type: "tool"; id: string; tool: string } & OpenCodeToolState
	| { type: "step_start"; id: string }
	| { type: "step_finish"; id: string; reason?: string };

export interface OpenCodePartUpdatedEvent {
	type: "opencode_part_updated";
	agent_id: string;
	worker_id: string;
	part: OpenCodePart;
}

export interface WorkerTextEvent {
	type: "worker_text";
	agent_id: string;
	worker_id: string;
	text: string;
}

export interface CortexChatMessageEvent {
	type: "cortex_chat_message";
	agent_id: string;
	thread_id: string;
	content: string;
	tool_calls?: Types.CortexChatToolCall[];
}

export type ApiEvent =
	| InboundMessageEvent
	| OutboundMessageEvent
	| OutboundMessageDeltaEvent
	| TypingStateEvent
	| WorkerStartedEvent
	| WorkerStatusEvent
	| WorkerIdleEvent
	| WorkerCompletedEvent
	| BranchStartedEvent
	| BranchCompletedEvent
	| ToolStartedEvent
	| ToolCompletedEvent
	| OpenCodePartUpdatedEvent
	| WorkerTextEvent
	| CortexChatMessageEvent;

// -- Timeline types (discriminated union parts) --

export interface TimelineMessage {
	type: "message";
	id: string;
	role: "user" | "assistant";
	sender_name: string | null;
	sender_id: string | null;
	content: string;
	created_at: string;
}

export interface TimelineBranchRun {
	type: "branch_run";
	id: string;
	description: string;
	conclusion: string | null;
	started_at: string;
	completed_at: string | null;
}

export interface TimelineWorkerRun {
	type: "worker_run";
	id: string;
	task: string;
	result: string | null;
	status: string;
	started_at: string;
	completed_at: string | null;
}

// Note: TimelineItem is re-exported from types.ts as a union type

async function fetchJson<T>(path: string): Promise<T> {
	const response = await fetch(`${getApiBase()}${path}`);
	if (!response.ok) {
		throw new Error(`API error: ${response.status}`);
	}
	return response.json();
}

/** channel_id -> StatusBlockSnapshot */
export type ChannelStatusResponse = Record<string, StatusBlockSnapshot>;

export interface WorkerStatusInfo {
	id: string;
	task: string;
	status: string;
	started_at: string;
	notify_on_complete: boolean;
	tool_calls: number;
	interactive: boolean;
}

export interface BranchStatusInfo {
	id: string;
	started_at: string;
	description: string;
}

export interface CompletedItemInfo {
	id: string;
	item_type: "Branch" | "Worker";
	description: string;
	completed_at: string;
	result_summary: string;
}

export interface StatusBlockSnapshot {
	active_workers: WorkerStatusInfo[];
	active_branches: BranchStatusInfo[];
	completed_items: CompletedItemInfo[];
}

export interface PromptInspectResponse {
	channel_id: string;
	system_prompt: string;
	total_chars: number;
	history_length: number;
	history: unknown[];
	capture_enabled: boolean;
	/** Present when the channel is not active */
	error?: string;
	message?: string;
}

export interface PromptSnapshotSummary {
	timestamp_ms: number;
	user_message: string;
	system_prompt_chars: number;
	history_length: number;
}

export interface PromptSnapshotListResponse {
	channel_id: string;
	snapshots: PromptSnapshotSummary[];
}

export interface PromptSnapshot {
	channel_id: string;
	timestamp_ms: number;
	user_message: string;
	system_prompt: string;
	system_prompt_chars: number;
	history: unknown;
	history_length: number;
}

export interface PromptCaptureResponse {
	channel_id: string;
	capture_enabled: boolean;
}

// --- Memory helper types (extended beyond schema) ---

// Extended MemoryType with additional values not yet in schema
export type MemoryType =
	| "fact"
	| "preference"
	| "decision"
	| "identity"
	| "event"
	| "observation"
	| "goal"
	| "todo";

export const MEMORY_TYPES: MemoryType[] = [
	"fact", "preference", "decision", "identity",
	"event", "observation", "goal", "todo",
];

export type MemorySort = "recent" | "importance" | "most_accessed";

// Extended MemoryItem with forgotten field (not yet in schema)
export interface MemoryItem {
	id: string;
	content: string;
	memory_type: MemoryType;
	importance: number;
	created_at: string;
	updated_at: string;
	last_accessed_at: string;
	access_count: number;
	source: string | null;
	channel_id: string | null;
	forgotten: boolean;
}

export interface MemoriesListResponse {
	memories: MemoryItem[];
	total: number;
}

export interface MemorySearchResultItem {
	memory: MemoryItem;
	score: number;
	rank: number;
}

export interface MemoriesSearchResponse {
	results: MemorySearchResultItem[];
}

export interface MemoryGraphParams {
	limit?: number;
	offset?: number;
	memory_type?: MemoryType;
	sort?: MemorySort;
}

export interface MemoryGraphNeighborsParams {
	depth?: number;
	exclude?: string[];
}

export interface MemoriesListParams {
	limit?: number;
	offset?: number;
	memory_type?: MemoryType;
	sort?: MemorySort;
}

export interface MemoriesSearchParams {
	limit?: number;
	memory_type?: MemoryType;
}

// --- Cortex event types ---

export type CortexEventType =
	| "bulletin_generated"
	| "bulletin_failed"
	| "maintenance_run"
	| "memory_merged"
	| "memory_decayed"
	| "memory_pruned"
	| "association_created"
	| "contradiction_flagged"
	| "worker_killed"
	| "branch_killed"
	| "circuit_breaker_tripped"
	| "observation_created"
	| "health_check";

export const CORTEX_EVENT_TYPES: CortexEventType[] = [
	"bulletin_generated", "bulletin_failed",
	"maintenance_run", "memory_merged", "memory_decayed", "memory_pruned",
	"association_created", "contradiction_flagged",
	"worker_killed", "branch_killed", "circuit_breaker_tripped",
	"observation_created", "health_check",
];

export interface CortexEvent {
	id: string;
	event_type: CortexEventType;
	summary: string;
	details: Record<string, unknown> | null;
	created_at: string;
}

export interface CortexEventsResponse {
	events: CortexEvent[];
	total: number;
}

export interface CortexEventsParams {
	limit?: number;
	offset?: number;
	event_type?: CortexEventType;
}

// -- Cortex Chat SSE types (not in schema) --

export type CortexChatSSEEvent =
	| { type: "thinking" }
	| { type: "tool_started"; tool: string; call_id: string; args: string }
	| { type: "tool_completed"; tool: string; call_id: string; args: string; result: string; result_preview: string }
	| { type: "done"; full_text: string; tool_calls: Types.CortexChatToolCall[] }
	| { type: "error"; message: string };

// -- Factory Presets --

export interface PresetDefaults {
	max_concurrent_workers: number | null;
	max_turns: number | null;
}

export interface PresetMeta {
	id: string;
	name: string;
	description: string;
	icon: string;
	tags: string[];
	defaults: PresetDefaults;
}

export interface PresetsResponse {
	presets: PresetMeta[];
}

// -- Config types with frontend-specific extensions --

export interface RoutingSection {
	channel: string;
	branch: string;
	worker: string;
	compactor: string;
	cortex: string;
	voice: string;
	rate_limit_cooldown_secs: number;
	channel_thinking_effort: string;
	branch_thinking_effort: string;
	worker_thinking_effort: string;
	compactor_thinking_effort: string;
	cortex_thinking_effort: string;
}

export interface TuningSection {
	max_concurrent_branches: number;
	max_concurrent_workers: number;
	max_turns: number;
	branch_max_turns: number;
	context_window: number;
	history_backfill_count: number;
}

export interface CompactionSection {
	background_threshold: number;
	aggressive_threshold: number;
	emergency_threshold: number;
}

export interface CortexSection {
	tick_interval_secs: number;
	worker_timeout_secs: number;
	branch_timeout_secs: number;
	circuit_breaker_threshold: number;
	bulletin_interval_secs: number;
	bulletin_max_words: number;
	bulletin_max_turns: number;
}

export interface CoalesceSection {
	enabled: boolean;
	debounce_ms: number;
	max_wait_ms: number;
	min_messages: number;
	multi_user_only: boolean;
}

export interface MemoryPersistenceSection {
	enabled: boolean;
	message_interval: number;
}

export interface BrowserSection {
	enabled: boolean;
	headless: boolean;
	evaluate_enabled: boolean;
	persist_session: boolean;
	close_policy: "close_browser" | "close_tabs" | "detach";
}

export interface ChannelSection {
	listen_only_mode: boolean;
}

export interface SandboxSection {
	mode: "enabled" | "disabled";
	writable_paths: string[];
}

export interface ProjectsSection {
	use_worktrees: boolean;
	worktree_name_template: string;
	auto_create_worktrees: boolean;
	auto_discover_repos: boolean;
	auto_discover_worktrees: boolean;
	disk_usage_warning_threshold: number;
}

export interface DiscordSection {
	enabled: boolean;
	allow_bot_messages: boolean;
}

export interface AgentConfigResponse {
	routing: RoutingSection;
	tuning: TuningSection;
	compaction: CompactionSection;
	cortex: CortexSection;
	coalesce: CoalesceSection;
	memory_persistence: MemoryPersistenceSection;
	browser: BrowserSection;
	channel: ChannelSection;
	discord: DiscordSection;
	sandbox: SandboxSection;
	projects: ProjectsSection;
}

// Partial update types - all fields are optional
export interface RoutingUpdate {
	channel?: string;
	branch?: string;
	worker?: string;
	compactor?: string;
	cortex?: string;
	voice?: string;
	rate_limit_cooldown_secs?: number;
	channel_thinking_effort?: string;
	branch_thinking_effort?: string;
	worker_thinking_effort?: string;
	compactor_thinking_effort?: string;
	cortex_thinking_effort?: string;
}

export interface TuningUpdate {
	max_concurrent_branches?: number;
	max_concurrent_workers?: number;
	max_turns?: number;
	branch_max_turns?: number;
	context_window?: number;
	history_backfill_count?: number;
}

export interface CompactionUpdate {
	background_threshold?: number;
	aggressive_threshold?: number;
	emergency_threshold?: number;
}

export interface CortexUpdate {
	tick_interval_secs?: number;
	worker_timeout_secs?: number;
	branch_timeout_secs?: number;
	circuit_breaker_threshold?: number;
	bulletin_interval_secs?: number;
	bulletin_max_words?: number;
	bulletin_max_turns?: number;
}

export interface CoalesceUpdate {
	enabled?: boolean;
	debounce_ms?: number;
	max_wait_ms?: number;
	min_messages?: number;
	multi_user_only?: boolean;
}

export interface MemoryPersistenceUpdate {
	enabled?: boolean;
	message_interval?: number;
}

export interface BrowserUpdate {
	enabled?: boolean;
	headless?: boolean;
	evaluate_enabled?: boolean;
	persist_session?: boolean;
	close_policy?: "close_browser" | "close_tabs" | "detach";
}

export interface ChannelUpdate {
	listen_only_mode?: boolean;
}

export interface SandboxUpdate {
	mode?: "enabled" | "disabled";
	writable_paths?: string[];
}

export interface ProjectsUpdate {
	use_worktrees?: boolean;
	worktree_name_template?: string;
	auto_create_worktrees?: boolean;
	auto_discover_repos?: boolean;
	auto_discover_worktrees?: boolean;
	disk_usage_warning_threshold?: number;
}

export interface DiscordUpdate {
	allow_bot_messages?: boolean;
}

export interface AgentConfigUpdateRequest {
	agent_id: string;
	routing?: RoutingUpdate;
	tuning?: TuningUpdate;
	compaction?: CompactionUpdate;
	cortex?: CortexUpdate;
	coalesce?: CoalesceUpdate;
	memory_persistence?: MemoryPersistenceUpdate;
	browser?: BrowserUpdate;
	channel?: ChannelUpdate;
	discord?: DiscordUpdate;
	sandbox?: SandboxUpdate;
	projects?: ProjectsUpdate;
}

// -- Cron Types --

export interface CronJobWithStats {
	id: string;
	prompt: string;
	cron_expr: string | null;
	interval_secs: number;
	delivery_target: string;
	enabled: boolean;
	run_once: boolean;
	active_hours: [number, number] | null;
	timeout_secs: number | null;
	success_count: number;
	failure_count: number;
	last_executed_at: string | null;
}

export interface CronExecutionEntry {
	id: string;
	executed_at: string;
	success: boolean;
	result_summary: string | null;
}

export interface CronListResponse {
	jobs: CronJobWithStats[];
	timezone: string;
}

export interface CronExecutionsResponse {
	executions: CronExecutionEntry[];
}

export interface CronActionResponse {
	success: boolean;
	message: string;
}

export interface CreateCronRequest {
	id: string;
	prompt: string;
	cron_expr?: string;
	interval_secs?: number;
	delivery_target: string;
	active_start_hour?: number;
	active_end_hour?: number;
	enabled: boolean;
	run_once: boolean;
	timeout_secs?: number;
}

export interface CronExecutionsParams {
	cron_id?: string;
	limit?: number;
}

// -- Update Types --

export type Deployment = "docker" | "hosted" | "native";

export interface UpdateStatus {
	current_version: string;
	latest_version: string | null;
	update_available: boolean;
	release_url: string | null;
	release_notes: string | null;
	deployment: Deployment;
	can_apply: boolean;
	cannot_apply_reason: string | null;
	docker_image: string | null;
	checked_at: string | null;
	error: string | null;
}

export interface UpdateApplyResponse {
	status: "updating" | "error";
	error?: string;
}

// -- Global Settings Types --

export interface OpenCodePermissions {
	edit: string;
	bash: string;
	webfetch: string;
}

export interface OpenCodeSettings {
	enabled: boolean;
	path: string;
	max_servers: number;
	server_startup_timeout_secs: number;
	max_restart_retries: number;
	permissions: OpenCodePermissions;
}

export interface OpenCodeSettingsUpdate {
	enabled?: boolean;
	path?: string;
	max_servers?: number;
	server_startup_timeout_secs?: number;
	max_restart_retries?: number;
	permissions?: Partial<OpenCodePermissions>;
}

export interface ClaudeCliStatusResponse {
	claude_folder_exists: boolean;
	credentials_file_exists: boolean;
	cli_installed: boolean;
	cli_version: string | null;
	authenticated: boolean;
	email: string | null;
	oauth_configured: boolean;
}

export interface AnthropicOAuthStartResponse {
	success: boolean;
	message: string;
	authorize_url: string | null;
	state: string | null;
}

export interface AnthropicOAuthExchangeResponse {
	success: boolean;
	message: string;
}

export interface GlobalSettingsUpdate {
	brave_search_key?: string | null;
	api_enabled?: boolean;
	api_port?: number;
	api_bind?: string;
	worker_log_mode?: string;
	opencode?: OpenCodeSettingsUpdate;
}

// -- Skills Types --

export interface SkillInfo {
	name: string;
	description: string;
	file_path: string;
	base_dir: string;
	source: "instance" | "workspace";
	source_repo?: string;
}

export interface SkillsListResponse {
	skills: SkillInfo[];
}

export interface InstallSkillRequest {
	agent_id: string;
	spec: string;
	instance?: boolean;
}

export interface InstallSkillResponse {
	installed: string[];
}

export interface RemoveSkillRequest {
	agent_id: string;
	name: string;
}

export interface RemoveSkillResponse {
	success: boolean;
	path: string | null;
}

// -- Skills Registry Types (skills.sh) --

export type RegistryView = "all-time" | "trending" | "hot";

export interface RegistrySkill {
	source: string;
	skillId: string;
	name: string;
	installs: number;
	description?: string;
	id?: string;
}

export interface RegistryBrowseResponse {
	skills: RegistrySkill[];
	has_more: boolean;
	total?: number;
}

export interface RegistrySearchResponse {
	skills: RegistrySkill[];
	query: string;
	count: number;
}

export interface SkillContentResponse {
	name: string;
	description: string;
	content: string;
	file_path: string;
	base_dir: string;
	source: string;
	source_repo?: string;
}

export interface UploadSkillResponse {
	installed: string[];
}

// -- Task Types --

export type TaskStatus = "pending_approval" | "backlog" | "ready" | "in_progress" | "done";
export type TaskPriority = "critical" | "high" | "medium" | "low";

export interface TaskSubtask {
	title: string;
	completed: boolean;
}

export interface TaskItem {
	id: string;
	task_number: number;
	title: string;
	description?: string;
	status: TaskStatus;
	priority: TaskPriority;
	owner_agent_id: string;
	assigned_agent_id: string;
	subtasks: TaskSubtask[];
	metadata: Record<string, unknown>;
	source_memory_id?: string;
	worker_id?: string;
	created_by: string;
	approved_at?: string;
	approved_by?: string;
	created_at: string;
	updated_at: string;
	completed_at?: string;
}

export interface TaskListResponse {
	tasks: TaskItem[];
}

export interface TaskResponse {
	task: TaskItem;
}

export interface TaskActionResponse {
	success: boolean;
	message: string;
}

export interface CreateTaskRequest {
	owner_agent_id: string;
	assigned_agent_id?: string;
	title: string;
	description?: string;
	status?: TaskStatus;
	priority?: TaskPriority;
	subtasks?: TaskSubtask[];
	metadata?: Record<string, unknown>;
	source_memory_id?: string;
	created_by?: string;
}

export interface UpdateTaskRequest {
	title?: string;
	description?: string;
	status?: TaskStatus;
	priority?: TaskPriority;
	assigned_agent_id?: string;
	subtasks?: TaskSubtask[];
	metadata?: Record<string, unknown>;
	complete_subtask?: number;
	worker_id?: string;
	approved_by?: string;
}

// -- Messaging / Bindings Types --

export interface BindingInfo {
	agent_id: string;
	channel: string;
	adapter: string | null;
	guild_id: string | null;
	workspace_id: string | null;
	chat_id: string | null;
	channel_ids: string[];
	require_mention: boolean;
	dm_allowed_users: string[];
}

export interface BindingsListResponse {
	bindings: BindingInfo[];
}

export interface CreateBindingRequest {
	agent_id: string;
	channel: string;
	adapter?: string;
	guild_id?: string;
	workspace_id?: string;
	chat_id?: string;
	channel_ids?: string[];
	require_mention?: boolean;
	dm_allowed_users?: string[];
	platform_credentials?: {
		discord_token?: string;
		slack_bot_token?: string;
		slack_app_token?: string;
		telegram_token?: string;
		email_imap_host?: string;
		email_imap_port?: number;
		email_imap_username?: string;
		email_imap_password?: string;
		email_smtp_host?: string;
		email_smtp_port?: number;
		email_smtp_username?: string;
		email_smtp_password?: string;
		email_from_address?: string;
		email_from_name?: string;
		twitch_username?: string;
		twitch_oauth_token?: string;
		twitch_client_id?: string;
		twitch_client_secret?: string;
		twitch_refresh_token?: string;
	};
}

export interface CreateBindingResponse {
	success: boolean;
	restart_required: boolean;
	message: string;
}

export interface UpdateBindingRequest {
	original_agent_id: string;
	original_channel: string;
	original_adapter?: string;
	original_guild_id?: string;
	original_workspace_id?: string;
	original_chat_id?: string;
	agent_id: string;
	channel: string;
	adapter?: string;
	guild_id?: string;
	workspace_id?: string;
	chat_id?: string;
	channel_ids?: string[];
	require_mention?: boolean;
	dm_allowed_users?: string[];
}

export interface UpdateBindingResponse {
	success: boolean;
	message: string;
}

export interface DeleteBindingRequest {
	agent_id: string;
	channel: string;
	adapter?: string;
	guild_id?: string;
	workspace_id?: string;
	chat_id?: string;
}

export interface DeleteBindingResponse {
	success: boolean;
	message: string;
}

// -- Links & Topology Types --

export type LinkDirection = "one_way" | "two_way";
export type LinkKind = "hierarchical" | "peer";

export interface AgentLinkResponse {
	from_agent_id: string;
	to_agent_id: string;
	direction: LinkDirection;
	kind: LinkKind;
}

export interface LinksResponse {
	links: AgentLinkResponse[];
}

export interface CreateHumanRequest {
	id: string;
	display_name?: string;
	role?: string;
	bio?: string;
	description?: string;
	discord_id?: string;
	telegram_id?: string;
	slack_id?: string;
	email?: string;
}

export interface UpdateHumanRequest {
	display_name?: string;
	role?: string;
	bio?: string;
	description?: string;
	discord_id?: string;
	telegram_id?: string;
	slack_id?: string;
	email?: string;
}

export interface CreateGroupRequest {
	name: string;
	agent_ids?: string[];
	color?: string;
}

export interface UpdateGroupRequest {
	name?: string;
	agent_ids?: string[];
	color?: string;
}

export interface CreateLinkRequest {
	from: string;
	to: string;
	direction?: LinkDirection;
	kind?: LinkKind;
}

export interface UpdateLinkRequest {
	direction?: LinkDirection;
	kind?: LinkKind;
}

// -- Projects Types --

export type ProjectStatus = "active" | "archived";

export interface Project {
	id: string;
	agent_id: string;
	name: string;
	description: string;
	icon: string;
	tags: string[];
	root_path: string;
	settings: Record<string, unknown>;
	status: ProjectStatus;
	created_at: string;
	updated_at: string;
}

export interface ProjectRepo {
	id: string;
	project_id: string;
	name: string;
	path: string;
	remote_url: string;
	default_branch: string;
	current_branch: string | null;
	description: string;
	disk_usage_bytes: number | null;
	created_at: string;
	updated_at: string;
}

export interface ProjectWorktree {
	id: string;
	project_id: string;
	repo_id: string;
	name: string;
	path: string;
	branch: string;
	created_by: string;
	disk_usage_bytes: number | null;
	created_at: string;
	updated_at: string;
}

export interface ProjectWorktreeWithRepo extends ProjectWorktree {
	repo_name: string;
}

/** GET /agents/projects response */
export interface ProjectListResponse {
	projects: Project[];
}

/** GET /agents/projects/:id response — project fields are flattened */
export interface ProjectWithRelations extends Project {
	repos: ProjectRepo[];
	worktrees: ProjectWorktreeWithRepo[];
}

export interface ProjectActionResponse {
	success: boolean;
	message: string;
}

export interface DiskUsageEntry {
	name: string;
	bytes: number;
	is_dir: boolean;
}

export interface DiskUsageResponse {
	total_bytes: number;
	entries: DiskUsageEntry[];
}

export interface DirEntry {
	name: string;
	path: string;
	is_dir: boolean;
}

export interface ListDirResponse {
	path: string;
	parent: string | null;
	entries: DirEntry[];
}

export interface CreateProjectRequest {
	name: string;
	description?: string;
	icon?: string;
	tags?: string[];
	root_path: string;
	settings?: Record<string, unknown>;
	auto_discover?: boolean;
}

export interface UpdateProjectRequest {
	name?: string;
	description?: string;
	icon?: string;
	tags?: string[];
	settings?: Record<string, unknown>;
	status?: ProjectStatus;
}

export interface CreateRepoRequest {
	name: string;
	path: string;
	remote_url?: string;
	default_branch?: string;
	description?: string;
}

export interface CreateWorktreeRequest {
	repo_id: string;
	branch: string;
	worktree_name?: string;
	start_point?: string;
}

// -- Secrets Types --

export type SecretCategory = "system" | "tool";
export type StoreState = "unencrypted" | "locked" | "unlocked";

export interface SecretStoreStatus {
	state: StoreState;
	encrypted: boolean;
	secret_count: number;
	system_count: number;
	tool_count: number;
	platform_managed: boolean;
}

export interface SecretListItem {
	name: string;
	category: SecretCategory;
	created_at: string;
	updated_at: string;
}

export interface SecretListResponse {
	secrets: SecretListItem[];
}

export interface PutSecretResponse {
	name: string;
	category: SecretCategory;
	reload_required: boolean;
	message: string;
}

export interface DeleteSecretResponse {
	deleted: string;
	warning?: string;
}

export interface EncryptResponse {
	master_key: string;
	message: string;
}

export interface UnlockResponse {
	state: string;
	secret_count: number;
	message: string;
}

export interface MigrationItem {
	config_key: string;
	secret_name: string;
	category: SecretCategory;
}

export interface MigrateResponse {
	migrated: MigrationItem[];
	skipped: string[];
	message: string;
}

// ---------------------------------------------------------------------------
// Code Graph types
// ---------------------------------------------------------------------------

export type CodeGraphIndexStatus = "pending" | "indexing" | "indexed" | "stale" | "error";

export interface CodeGraphProject {
	project_id: string;
	name: string;
	root_path: string;
	status: CodeGraphIndexStatus;
	progress?: {
		phase: string;
		phase_progress: number;
		message: string;
		stats: CodeGraphPipelineStats;
	};
	error_message?: string;
	last_index_stats?: CodeGraphPipelineStats;
	last_indexed_at?: string;
	primary_language?: string;
	schema_version: number;
	created_at: string;
	updated_at: string;
}

export interface CodeGraphPipelineStats {
	files_found: number;
	files_parsed: number;
	files_skipped: number;
	nodes_created: number;
	edges_created: number;
	communities_detected: number;
	processes_traced: number;
	errors: number;
}

export interface CodeGraphProjectListResponse {
	projects: CodeGraphProject[];
}

export interface CodeGraphProjectDetailResponse {
	project: CodeGraphProject;
}

export interface CodeGraphCommunity {
	id: string;
	name: string;
	description?: string;
	node_count: number;
	file_count: number;
	function_count: number;
	key_symbols: string[];
}

export interface CodeGraphCommunitiesResponse {
	communities: CodeGraphCommunity[];
	total: number;
}

export interface CodeGraphProcess {
	id: string;
	entry_function: string;
	source_file: string;
	call_depth: number;
	community?: string;
	steps: string[];
}

export interface CodeGraphProcessesResponse {
	processes: CodeGraphProcess[];
	total: number;
}

export interface CodeGraphSearchResult {
	node_id: number;
	qualified_name: string;
	name: string;
	label: string;
	source_file?: string;
	line_start?: number;
	score: number;
	community?: string;
	snippet?: string;
}

export interface CodeGraphSearchResponse {
	results: CodeGraphSearchResult[];
	total: number;
}

export interface CodeGraphIndexLogEntry {
	run_id: string;
	status: CodeGraphIndexStatus;
	started_at: string;
	completed_at?: string;
	current_phase?: string;
	progress?: { phase: string; phase_progress: number; message: string };
	stats?: CodeGraphPipelineStats;
	error?: string;
}

export interface CodeGraphIndexLogResponse {
	entries: CodeGraphIndexLogEntry[];
}

export interface CodeGraphRemoveInfoResponse {
	node_count: number;
	edge_count: number;
}

export interface CodeGraphActionResponse {
	success: boolean;
	message: string;
}

// -- Node / Edge types for the graph explorer --

export type CodeGraphNodeLabel =
	| "project" | "package" | "module" | "folder" | "file"
	| "class" | "function" | "method" | "variable" | "parameter"
	| "interface" | "enum" | "decorator" | "import" | "type"
	| "struct" | "macro" | "trait" | "impl" | "namespace"
	| "type_alias" | "const" | "record" | "template"
	| "community" | "process" | "section" | "test" | "route";

export type CodeGraphEdgeType =
	| "CONTAINS" | "DEFINES" | "CALLS" | "IMPORTS" | "EXTENDS"
	| "IMPLEMENTS" | "OVERRIDES" | "HAS_METHOD" | "HAS_PROPERTY"
	| "ACCESSES" | "USES" | "HAS_PARAMETER" | "DECORATES"
	| "MEMBER_OF" | "STEP_IN_PROCESS" | "TESTED_BY"
	| "ENTRY_POINT_OF" | "HANDLES_ROUTE" | "FETCHES" | "QUERIES"
	| "HANDLES_TOOL";

export interface CodeGraphNodeSummary {
	id: number;
	qualified_name: string;
	name: string;
	label: string;
	source_file?: string;
	line_start?: number;
	line_end?: number;
	file_size?: number;
}

export interface CodeGraphNodeFull extends CodeGraphNodeSummary {
	source?: string;
	written_by?: string;
	properties: Record<string, unknown>;
}

export interface CodeGraphEdgeSummary {
	from_id: number;
	from_name: string;
	from_label: string;
	to_id: number;
	to_name: string;
	to_label: string;
	edge_type: string;
	confidence: number;
}

export interface CodeGraphLabelCount {
	label: string;
	count: number;
}

export interface CodeGraphTypeCount {
	edge_type: string;
	count: number;
}

export interface CodeGraphNodeListResponse {
	nodes: CodeGraphNodeSummary[];
	total: number;
	offset: number;
	limit: number;
}

export interface CodeGraphNodeDetailResponse {
	node: CodeGraphNodeFull;
}

export interface CodeGraphEdgeListResponse {
	edges: CodeGraphEdgeSummary[];
	total: number;
	offset: number;
	limit: number;
}

export interface CodeGraphStatsResponse {
	total_nodes: number;
	total_edges: number;
	nodes_by_label: CodeGraphLabelCount[];
	edges_by_type: CodeGraphTypeCount[];
}

export interface CodeGraphBulkNodesResponse {
	nodes: CodeGraphNodeSummary[];
	truncated: boolean;
	total_available: number;
}

export interface CodeGraphBulkEdgeSummary {
	from_qname: string;
	from_label: string;
	to_qname: string;
	to_label: string;
	edge_type: string;
	confidence: number;
}

export interface CodeGraphBulkEdgesResponse {
	edges: CodeGraphBulkEdgeSummary[];
}

export interface FsReadFileResponse {
	path: string;
	content: string;
	start_line: number;
	total_lines: number;
	language: string;
}

export const api = {
	status: () => fetchJson<Types.StatusResponse>("/status"),
	overview: () => fetchJson<Types.InstanceOverviewResponse>("/agents/instance"),
	agents: () => fetchJson<Types.AgentsResponse>("/agents"),
	factoryPresets: () => fetchJson<PresetsResponse>("/factory/presets"),
	agentOverview: (agentId: string) =>
		fetchJson<Types.AgentOverviewResponse>(`/agents/overview?agent_id=${encodeURIComponent(agentId)}`),
	channels: () => fetchJson<Types.ChannelsResponse>("/channels"),
	deleteChannel: async (agentId: string, channelId: string) => {
		const params = new URLSearchParams({ agent_id: agentId, channel_id: channelId });
		const response = await fetch(`${getApiBase()}/channels?${params}`, { method: "DELETE" });
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<{ success: boolean }>;
	},
	channelMessages: (channelId: string, limit = 20, before?: string) => {
		const params = new URLSearchParams({ channel_id: channelId, limit: String(limit) });
		if (before) params.set("before", before);
		return fetchJson<Types.MessagesResponse>(`/channels/messages?${params}`);
	},
	channelStatus: () => fetchJson<ChannelStatusResponse>("/channels/status"),
	inspectPrompt: (channelId: string) =>
		fetchJson<PromptInspectResponse>(`/channels/inspect?channel_id=${encodeURIComponent(channelId)}`),
	setPromptCapture: async (channelId: string, enabled: boolean) => {
		const response = await fetch(`${getApiBase()}/channels/inspect/capture`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ channel_id: channelId, enabled }),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<PromptCaptureResponse>;
	},
	listPromptSnapshots: (channelId: string, limit = 50) =>
		fetchJson<PromptSnapshotListResponse>(
			`/channels/inspect/snapshots?channel_id=${encodeURIComponent(channelId)}&limit=${limit}`,
		),
	getPromptSnapshot: (channelId: string, timestampMs: number) =>
		fetchJson<PromptSnapshot>(
			`/channels/inspect/snapshot?channel_id=${encodeURIComponent(channelId)}&timestamp_ms=${timestampMs}`,
		),
	workersList: (agentId: string, params: { limit?: number; offset?: number; status?: string } = {}) => {
		const search = new URLSearchParams({ agent_id: agentId });
		if (params.limit) search.set("limit", String(params.limit));
		if (params.offset) search.set("offset", String(params.offset));
		if (params.status) search.set("status", params.status);
		return fetchJson<Types.WorkerListResponse>(`/agents/workers?${search}`);
	},
	workerDetail: (agentId: string, workerId: string) =>
		fetchJson<Types.WorkerDetailResponse>(`/agents/workers/detail?agent_id=${encodeURIComponent(agentId)}&worker_id=${encodeURIComponent(workerId)}`),
	agentMemories: (agentId: string, params: MemoryGraphParams = {}) => {
		const search = new URLSearchParams({ agent_id: agentId });
		if (params.limit) search.set("limit", String(params.limit));
		if (params.offset) search.set("offset", String(params.offset));
		if (params.memory_type) search.set("memory_type", params.memory_type);
		if (params.sort) search.set("sort", params.sort);
		return fetchJson<MemoriesListResponse>(`/agents/memories?${search}`);
	},
	searchMemories: (agentId: string, query: string, params: MemoriesSearchParams = {}) => {
		const search = new URLSearchParams({ agent_id: agentId, q: query });
		if (params.limit) search.set("limit", String(params.limit));
		if (params.memory_type) search.set("memory_type", params.memory_type);
		return fetchJson<MemoriesSearchResponse>(`/agents/memories/search?${search}`);
	},
	memoryGraph: (agentId: string, params: MemoryGraphParams = {}) => {
		const search = new URLSearchParams({ agent_id: agentId });
		if (params.limit) search.set("limit", String(params.limit));
		if (params.offset) search.set("offset", String(params.offset));
		if (params.memory_type) search.set("memory_type", params.memory_type);
		if (params.sort) search.set("sort", params.sort);
		return fetchJson<Types.MemoryGraphResponse>(`/agents/memories/graph?${search}`);
	},
	memoryGraphNeighbors: (agentId: string, memoryId: string, params: MemoryGraphNeighborsParams = {}) => {
		const search = new URLSearchParams({ agent_id: agentId, memory_id: memoryId });
		if (params.depth) search.set("depth", String(params.depth));
		if (params.exclude?.length) search.set("exclude", params.exclude.join(","));
		return fetchJson<Types.MemoryGraphNeighborsResponse>(`/agents/memories/graph/neighbors?${search}`);
	},
	cortexEvents: (agentId: string, params: CortexEventsParams = {}) => {
		const search = new URLSearchParams({ agent_id: agentId });
		if (params.limit) search.set("limit", String(params.limit));
		if (params.offset) search.set("offset", String(params.offset));
		if (params.event_type) search.set("event_type", params.event_type);
		return fetchJson<CortexEventsResponse>(`/cortex/events?${search}`);
	},
	cortexChatMessages: (agentId: string, threadId?: string, limit = 50) => {
		const search = new URLSearchParams({ agent_id: agentId, limit: String(limit) });
		if (threadId) search.set("thread_id", threadId);
		return fetchJson<Types.CortexChatMessagesResponse>(`/cortex-chat/messages?${search}`);
	},
	cortexChatSend: (agentId: string, threadId: string, message: string, channelId?: string) =>
		fetch(`${getApiBase()}/cortex-chat/send`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({
				agent_id: agentId,
				thread_id: threadId,
				message,
				channel_id: channelId ?? null,
			}),
		}),
	cortexChatThreads: (agentId: string) =>
		fetchJson<Types.CortexChatThreadsResponse>(
			`/cortex-chat/threads?agent_id=${encodeURIComponent(agentId)}`,
		),
	cortexChatDeleteThread: async (agentId: string, threadId: string) => {
		const response = await fetch(`${getApiBase()}/cortex-chat/thread`, {
			method: "DELETE",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ agent_id: agentId, thread_id: threadId }),
		});
		if (!response.ok) throw new Error(`HTTP ${response.status}`);
	},
	agentProfile: (agentId: string) =>
		fetchJson<Types.AgentProfileResponse>(`/agents/profile?agent_id=${encodeURIComponent(agentId)}`),
	agentIdentity: (agentId: string) =>
		fetchJson<{ soul: string | null; identity: string | null; role: string | null }>(`/agents/identity?agent_id=${encodeURIComponent(agentId)}`),
	updateIdentity: async (request: { agent_id: string; soul?: string | null; identity?: string | null; role?: string | null }) => {
		const response = await fetch(`${getApiBase()}/agents/identity`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<{ soul: string | null; identity: string | null; role: string | null }>;
	},
	createAgent: async (agentId: string, displayName?: string, role?: string) => {
		const response = await fetch(`${getApiBase()}/agents`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ agent_id: agentId, display_name: displayName || undefined, role: role || undefined }),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<{ success: boolean; agent_id: string; message: string }>;
	},

	updateAgent: async (agentId: string, update: { display_name?: string; role?: string; gradient_start?: string; gradient_end?: string }) => {
		const response = await fetch(`${getApiBase()}/agents`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ agent_id: agentId, ...update }),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<{ success: boolean; agent_id: string; message: string }>;
	},

	deleteAgent: async (agentId: string) => {
		const params = new URLSearchParams({ agent_id: agentId });
		const response = await fetch(`${getApiBase()}/agents?${params}`, {
			method: "DELETE",
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<{ success: boolean; message: string }>;
	},

	/** Get the avatar URL for an agent (returns the raw URL, not fetched). */
	agentAvatarUrl: (agentId: string) => `${getApiBase()}/agents/avatar?agent_id=${encodeURIComponent(agentId)}`,

	/** Upload an avatar image for an agent. */
	uploadAvatar: async (agentId: string, file: File) => {
		const params = new URLSearchParams({ agent_id: agentId });
		const response = await fetch(`${getApiBase()}/agents/avatar?${params}`, {
			method: "POST",
			headers: { "Content-Type": file.type },
			body: file,
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<{ success: boolean; path?: string; message?: string }>;
	},

	/** Delete the avatar for an agent. */
	deleteAvatar: async (agentId: string) => {
		const params = new URLSearchParams({ agent_id: agentId });
		const response = await fetch(`${getApiBase()}/agents/avatar?${params}`, {
			method: "DELETE",
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<{ success: boolean; message: string }>;
	},

	agentConfig: (agentId: string) =>
		fetchJson<AgentConfigResponse>(`/agents/config?agent_id=${encodeURIComponent(agentId)}`),
	updateAgentConfig: async (request: AgentConfigUpdateRequest) => {
		const response = await fetch(`${getApiBase()}/agents/config`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<AgentConfigResponse>;
	},

	// Cron API
	listCronJobs: (agentId: string) =>
		fetchJson<CronListResponse>(`/agents/cron?agent_id=${encodeURIComponent(agentId)}`),

	cronExecutions: (agentId: string, params: CronExecutionsParams = {}) => {
		const search = new URLSearchParams({ agent_id: agentId });
		if (params.cron_id) search.set("cron_id", params.cron_id);
		if (params.limit) search.set("limit", String(params.limit));
		return fetchJson<CronExecutionsResponse>(`/agents/cron/executions?${search}`);
	},

	createCronJob: async (agentId: string, request: CreateCronRequest) => {
		const response = await fetch(`${getApiBase()}/agents/cron`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ ...request, agent_id: agentId }),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<CronActionResponse>;
	},

	deleteCronJob: async (agentId: string, cronId: string) => {
		const search = new URLSearchParams({ agent_id: agentId, cron_id: cronId });
		const response = await fetch(`${getApiBase()}/agents/cron?${search}`, {
			method: "DELETE",
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<CronActionResponse>;
	},

	toggleCronJob: async (agentId: string, cronId: string, enabled: boolean) => {
		const response = await fetch(`${getApiBase()}/agents/cron/toggle`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ agent_id: agentId, cron_id: cronId, enabled }),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<CronActionResponse>;
	},

	triggerCronJob: async (agentId: string, cronId: string) => {
		const response = await fetch(`${getApiBase()}/agents/cron/trigger`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ agent_id: agentId, cron_id: cronId }),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<CronActionResponse>;
	},

	cancelProcess: async (channelId: string, processType: "worker" | "branch", processId: string) => {
		const response = await fetch(`${getApiBase()}/channels/cancel`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ channel_id: channelId, process_type: processType, process_id: processId }),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<{ success: boolean; message: string }>;
	},

	// Provider management
	providers: () => fetchJson<Types.ProvidersResponse>("/providers"),
	updateProvider: async (provider: string, apiKey: string, model: string) => {
		const response = await fetch(`${getApiBase()}/providers`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ provider, api_key: apiKey, model }),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<Types.ProviderUpdateResponse>;
	},
	testProviderModel: async (provider: string, apiKey: string, model: string) => {
		const response = await fetch(`${getApiBase()}/providers/test`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ provider, api_key: apiKey, model }),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<Types.ProviderModelTestResponse>;
	},
	claudeCliStatus: () => fetchJson<ClaudeCliStatusResponse>("/providers/anthropic/oauth/cli-status"),
	startAnthropicOAuth: async (params: { model: string; mode?: string }) => {
		const response = await fetch(`${getApiBase()}/providers/anthropic/oauth/start`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(params),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<AnthropicOAuthStartResponse>;
	},
	exchangeAnthropicOAuth: async (params: { code: string; state: string }) => {
		const response = await fetch(`${getApiBase()}/providers/anthropic/oauth/exchange`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(params),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<AnthropicOAuthExchangeResponse>;
	},
	startOpenAiOAuthBrowser: async (params: {model: string}) => {
		const response = await fetch(`${getApiBase()}/providers/openai/oauth/browser/start`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({
				model: params.model,
			}),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<Types.OpenAiOAuthBrowserStartResponse>;
	},
	openAiOAuthBrowserStatus: async (state: string) => {
		const response = await fetch(
			`${getApiBase()}/providers/openai/oauth/browser/status?state=${encodeURIComponent(state)}`,
		);
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<Types.OpenAiOAuthBrowserStatusResponse>;
	},
	removeProvider: async (provider: string) => {
		const response = await fetch(`${getApiBase()}/providers/${encodeURIComponent(provider)}`, {
			method: "DELETE",
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<Types.ProviderUpdateResponse>;
	},

	// Model listing
	models: (provider?: string, capability?: "input_audio" | "voice_transcription") => {
		const params = new URLSearchParams();
		if (provider) params.set("provider", provider);
		if (capability) params.set("capability", capability);
		const query = params.toString() ? `?${params.toString()}` : "";
		return fetchJson<Types.ModelsResponse>(`/models${query}`);
	},
	refreshModels: async () => {
		const response = await fetch(`${getApiBase()}/models/refresh`, {
			method: "POST",
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<Types.ModelsResponse>;
	},

	// Ingest API
	ingestFiles: (agentId: string) =>
		fetchJson<Types.IngestFilesResponse>(`/agents/ingest/files?agent_id=${encodeURIComponent(agentId)}`),

	uploadIngestFiles: async (agentId: string, files: File[]) => {
		const formData = new FormData();
		for (const file of files) {
			formData.append("files", file);
		}
		const response = await fetch(
			`${getApiBase()}/agents/ingest/upload?agent_id=${encodeURIComponent(agentId)}`,
			{ method: "POST", body: formData },
		);
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<Types.IngestUploadResponse>;
	},

	deleteIngestFile: async (agentId: string, contentHash: string) => {
		const params = new URLSearchParams({ agent_id: agentId, content_hash: contentHash });
		const response = await fetch(`${getApiBase()}/agents/ingest/files?${params}`, {
			method: "DELETE",
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<Types.IngestDeleteResponse>;
	},

	// Messaging / Bindings API
	messagingStatus: () => fetchJson<Types.MessagingStatusResponse>("/messaging/status"),

	bindings: (agentId?: string) => {
		const params = agentId
			? `?agent_id=${encodeURIComponent(agentId)}`
			: "";
		return fetchJson<BindingsListResponse>(`/bindings${params}`);
	},

	createBinding: async (request: CreateBindingRequest) => {
		const response = await fetch(`${getApiBase()}/bindings`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<CreateBindingResponse>;
	},

	updateBinding: async (request: UpdateBindingRequest) => {
		const response = await fetch(`${getApiBase()}/bindings`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<UpdateBindingResponse>;
	},

	deleteBinding: async (request: DeleteBindingRequest) => {
		const response = await fetch(`${getApiBase()}/bindings`, {
			method: "DELETE",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<DeleteBindingResponse>;
	},

	togglePlatform: async (platform: string, enabled: boolean, adapter?: string) => {
		const body: Record<string, unknown> = { platform, enabled };
		if (adapter) body.adapter = adapter;
		const response = await fetch(`${getApiBase()}/messaging/toggle`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(body),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<{ success: boolean; message: string }>;
	},

	disconnectPlatform: async (platform: string, adapter?: string) => {
		const body: Record<string, unknown> = { platform };
		if (adapter) body.adapter = adapter;
		const response = await fetch(`${getApiBase()}/messaging/disconnect`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(body),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<{ success: boolean; message: string }>;
	},

	createMessagingInstance: async (request: Types.CreateMessagingInstanceRequest) => {
		const response = await fetch(`${getApiBase()}/messaging/instances`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<Types.MessagingInstanceActionResponse>;
	},

	deleteMessagingInstance: async (request: Types.DeleteMessagingInstanceRequest) => {
		const response = await fetch(`${getApiBase()}/messaging/instances`, {
			method: "DELETE",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<Types.MessagingInstanceActionResponse>;
	},

	// Global Settings API
	globalSettings: () => fetchJson<Types.GlobalSettingsResponse>("/settings"),
	
	updateGlobalSettings: async (settings: Types.GlobalSettingsUpdate) => {
		const response = await fetch(`${getApiBase()}/settings`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(settings),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<Types.GlobalSettingsUpdateResponse>;
	},

	// Raw config API
	rawConfig: () => fetchJson<Types.RawConfigResponse>("/config/raw"),
	updateRawConfig: async (content: string) => {
		const response = await fetch(`${getApiBase()}/config/raw`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ content }),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<Types.RawConfigUpdateResponse>;
	},

	// Changelog API
	changelog: async (): Promise<string> => {
		const data = await fetchJson<{ content: string }>("/changelog");
		return data.content;
	},

	// Update API
	updateCheck: () => fetchJson<UpdateStatus>("/update/check"),
	updateCheckNow: async () => {
		const response = await fetch(`${getApiBase()}/update/check`, { method: "POST" });
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<UpdateStatus>;
	},
	updateApply: async () => {
		const response = await fetch(`${getApiBase()}/update/apply`, { method: "POST" });
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<UpdateApplyResponse>;
	},

	// Skills API
	listSkills: (agentId: string) =>
		fetchJson<SkillsListResponse>(`/agents/skills?agent_id=${encodeURIComponent(agentId)}`),
	
	installSkill: async (request: InstallSkillRequest) => {
		const response = await fetch(`${getApiBase()}/agents/skills/install`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<InstallSkillResponse>;
	},
	
	removeSkill: async (request: RemoveSkillRequest) => {
		const response = await fetch(`${getApiBase()}/agents/skills/remove`, {
			method: "DELETE",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<RemoveSkillResponse>;
	},

	getSkillContent: (agentId: string, name: string) =>
		fetchJson<SkillContentResponse>(
			`/agents/skills/content?agent_id=${encodeURIComponent(agentId)}&name=${encodeURIComponent(name)}`,
		),

	uploadSkillFiles: async (agentId: string, files: File[]) => {
		const form = new FormData();
		for (const file of files) {
			form.append("file", file);
		}
		const response = await fetch(
			`${getApiBase()}/agents/skills/upload?agent_id=${encodeURIComponent(agentId)}`,
			{ method: "POST", body: form },
		);
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<UploadSkillResponse>;
	},

	// Skills Registry API (skills.sh proxy)
	registryBrowse: (view: RegistryView = "all-time", page = 0) =>
		fetchJson<RegistryBrowseResponse>(
			`/skills/registry/browse?view=${encodeURIComponent(view)}&page=${page}`,
		),

	registrySearch: (query: string, limit = 50) =>
		fetchJson<RegistrySearchResponse>(
			`/skills/registry/search?q=${encodeURIComponent(query)}&limit=${limit}`,
		),

	registrySkillContent: (source: string, skillId: string) =>
		fetchJson<SkillContentResponse>(
			`/skills/registry/content?source=${encodeURIComponent(source)}&skill_id=${encodeURIComponent(skillId)}`,
		),

	// Agent Links & Topology API
	topology: () => fetchJson<TopologyResponse>("/topology"),
	links: () => fetchJson<LinksResponse>("/links"),
	agentLinks: (agentId: string) =>
		fetchJson<LinksResponse>(`/agents/${encodeURIComponent(agentId)}/links`),
	createLink: async (request: CreateLinkRequest): Promise<{ link: AgentLinkResponse }> => {
		const response = await fetch(`${getApiBase()}/links`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json();
	},
	updateLink: async (from: string, to: string, request: UpdateLinkRequest): Promise<{ link: AgentLinkResponse }> => {
		const response = await fetch(
			`${getApiBase()}/links/${encodeURIComponent(from)}/${encodeURIComponent(to)}`,
			{
				method: "PUT",
				headers: { "Content-Type": "application/json" },
				body: JSON.stringify(request),
			},
		);
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json();
	},
	deleteLink: async (from: string, to: string): Promise<void> => {
		const response = await fetch(
			`${getApiBase()}/links/${encodeURIComponent(from)}/${encodeURIComponent(to)}`,
			{ method: "DELETE" },
		);
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
	},

	// Agent Groups API
	groups: () => fetchJson<{ groups: TopologyGroup[] }>("/groups"),
	createGroup: async (request: CreateGroupRequest): Promise<{ group: TopologyGroup }> => {
		const response = await fetch(`${getApiBase()}/groups`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json();
	},
	updateGroup: async (name: string, request: UpdateGroupRequest): Promise<{ group: TopologyGroup }> => {
		const response = await fetch(
			`${getApiBase()}/groups/${encodeURIComponent(name)}`,
			{
				method: "PUT",
				headers: { "Content-Type": "application/json" },
				body: JSON.stringify(request),
			},
		);
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json();
	},
	deleteGroup: async (name: string): Promise<void> => {
		const response = await fetch(
			`${getApiBase()}/groups/${encodeURIComponent(name)}`,
			{ method: "DELETE" },
		);
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
	},

	// Humans API
	humans: () => fetchJson<{ humans: TopologyHuman[] }>("/humans"),
	createHuman: async (request: CreateHumanRequest): Promise<{ human: TopologyHuman }> => {
		const response = await fetch(`${getApiBase()}/humans`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json();
	},
	updateHuman: async (id: string, request: UpdateHumanRequest): Promise<{ human: TopologyHuman }> => {
		const response = await fetch(
			`${getApiBase()}/humans/${encodeURIComponent(id)}`,
			{
				method: "PUT",
				headers: { "Content-Type": "application/json" },
				body: JSON.stringify(request),
			},
		);
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json();
	},
	deleteHuman: async (id: string): Promise<void> => {
		const response = await fetch(
			`${getApiBase()}/humans/${encodeURIComponent(id)}`,
			{ method: "DELETE" },
		);
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
	},

	// Web Chat API
	webChatSend: (agentId: string, sessionId: string, message: string, senderName?: string) =>
		fetch(`${getApiBase()}/webchat/send`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({
				agent_id: agentId,
				session_id: sessionId,
				sender_name: senderName ?? "user",
				message,
			}),
		}),

	webChatHistory: (agentId: string, sessionId: string, limit = 100) =>
		fetch(`${getApiBase()}/webchat/history?agent_id=${encodeURIComponent(agentId)}&session_id=${encodeURIComponent(sessionId)}&limit=${limit}`),

	// Tasks API
	listTasks: (params?: { agent_id?: string; owner_agent_id?: string; assigned_agent_id?: string; status?: TaskStatus; priority?: TaskPriority; created_by?: string; limit?: number }) => {
		const search = new URLSearchParams();
		if (params?.agent_id) search.set("agent_id", params.agent_id);
		if (params?.owner_agent_id) search.set("owner_agent_id", params.owner_agent_id);
		if (params?.assigned_agent_id) search.set("assigned_agent_id", params.assigned_agent_id);
		if (params?.status) search.set("status", params.status);
		if (params?.priority) search.set("priority", params.priority);
		if (params?.created_by) search.set("created_by", params.created_by);
		if (params?.limit) search.set("limit", String(params.limit));
		const query = search.toString();
		return fetchJson<TaskListResponse>(query ? `/tasks?${query}` : "/tasks");
	},
	getTask: (taskNumber: number) =>
		fetchJson<TaskResponse>(`/tasks/${taskNumber}`),
	createTask: async (request: CreateTaskRequest): Promise<TaskResponse> => {
		const response = await fetch(`${getApiBase()}/tasks`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<TaskResponse>;
	},
	updateTask: async (taskNumber: number, request: UpdateTaskRequest): Promise<TaskResponse> => {
		const response = await fetch(`${getApiBase()}/tasks/${taskNumber}`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<TaskResponse>;
	},
	deleteTask: async (taskNumber: number): Promise<TaskActionResponse> => {
		const response = await fetch(`${getApiBase()}/tasks/${taskNumber}`, {
			method: "DELETE",
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<TaskActionResponse>;
	},
	approveTask: async (taskNumber: number, approvedBy?: string): Promise<TaskResponse> => {
		const response = await fetch(`${getApiBase()}/tasks/${taskNumber}/approve`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ approved_by: approvedBy }),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<TaskResponse>;
	},
	executeTask: async (taskNumber: number): Promise<TaskResponse> => {
		const response = await fetch(`${getApiBase()}/tasks/${taskNumber}/execute`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({}),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<TaskResponse>;
	},
	assignTask: async (taskNumber: number, assignedAgentId: string): Promise<TaskResponse> => {
		const response = await fetch(`${getApiBase()}/tasks/${taskNumber}/assign`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ assigned_agent_id: assignedAgentId }),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<TaskResponse>;
	},

	// Secrets API
	secretsStatus: () => fetchJson<SecretStoreStatus>("/secrets/status"),
	listSecrets: () => fetchJson<SecretListResponse>("/secrets"),
	putSecret: async (name: string, value: string, category?: SecretCategory): Promise<PutSecretResponse> => {
		const response = await fetch(`${getApiBase()}/secrets/${encodeURIComponent(name)}`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ value, category }),
		});
		if (!response.ok) {
			const body = await response.json().catch(() => ({}));
			throw new Error(body.error || `API error: ${response.status}`);
		}
		return response.json() as Promise<PutSecretResponse>;
	},
	deleteSecret: async (name: string): Promise<DeleteSecretResponse> => {
		const response = await fetch(`${getApiBase()}/secrets/${encodeURIComponent(name)}`, {
			method: "DELETE",
		});
		if (!response.ok) {
			const body = await response.json().catch(() => ({}));
			throw new Error(body.error || `API error: ${response.status}`);
		}
		return response.json() as Promise<DeleteSecretResponse>;
	},
	enableEncryption: async (): Promise<EncryptResponse> => {
		const response = await fetch(`${getApiBase()}/secrets/encrypt`, { method: "POST" });
		if (!response.ok) {
			const body = await response.json().catch(() => ({}));
			throw new Error(body.error || `API error: ${response.status}`);
		}
		return response.json() as Promise<EncryptResponse>;
	},
	unlockSecrets: async (masterKey: string): Promise<UnlockResponse> => {
		const response = await fetch(`${getApiBase()}/secrets/unlock`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ master_key: masterKey }),
		});
		if (!response.ok) {
			const body = await response.json().catch(() => ({}));
			throw new Error(body.error || `API error: ${response.status}`);
		}
		return response.json() as Promise<UnlockResponse>;
	},
	lockSecrets: async (): Promise<{ state: string; message: string }> => {
		const response = await fetch(`${getApiBase()}/secrets/lock`, { method: "POST" });
		if (!response.ok) {
			const body = await response.json().catch(() => ({}));
			throw new Error(body.error || `API error: ${response.status}`);
		}
		return response.json();
	},
	rotateKey: async (): Promise<{ master_key: string; message: string }> => {
		const response = await fetch(`${getApiBase()}/secrets/rotate`, { method: "POST" });
		if (!response.ok) {
			const body = await response.json().catch(() => ({}));
			throw new Error(body.error || `API error: ${response.status}`);
		}
		return response.json();
	},
	migrateSecrets: async (): Promise<MigrateResponse> => {
		const response = await fetch(`${getApiBase()}/secrets/migrate`, { method: "POST" });
		if (!response.ok) {
			const body = await response.json().catch(() => ({}));
			throw new Error(body.error || `API error: ${response.status}`);
		}
		return response.json() as Promise<MigrateResponse>;
	},

	// Projects API
	listProjects: (agentId: string, status?: ProjectStatus) => {
		const search = new URLSearchParams({ agent_id: agentId });
		if (status) search.set("status", status);
		return fetchJson<ProjectListResponse>(`/agents/projects?${search}`);
	},

	getProject: (agentId: string, projectId: string) =>
		fetchJson<ProjectWithRelations>(
			`/agents/projects/${encodeURIComponent(projectId)}?agent_id=${encodeURIComponent(agentId)}`,
		),

	createProject: async (agentId: string, request: CreateProjectRequest): Promise<ProjectWithRelations> => {
		const response = await fetch(`${getApiBase()}/agents/projects`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ ...request, agent_id: agentId }),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<ProjectWithRelations>;
	},

	updateProject: async (agentId: string, projectId: string, request: UpdateProjectRequest): Promise<ProjectWithRelations> => {
		const response = await fetch(`${getApiBase()}/agents/projects/${encodeURIComponent(projectId)}`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ ...request, agent_id: agentId }),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<ProjectWithRelations>;
	},

	deleteProject: async (agentId: string, projectId: string): Promise<ProjectActionResponse> => {
		const response = await fetch(
			`${getApiBase()}/agents/projects/${encodeURIComponent(projectId)}?agent_id=${encodeURIComponent(agentId)}`,
			{ method: "DELETE" },
		);
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<ProjectActionResponse>;
	},

	scanProject: async (agentId: string, projectId: string): Promise<ProjectWithRelations> => {
		const response = await fetch(
			`${getApiBase()}/agents/projects/${encodeURIComponent(projectId)}/scan?agent_id=${encodeURIComponent(agentId)}`,
			{ method: "POST" },
		);
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<ProjectWithRelations>;
	},

	projectDiskUsage: (agentId: string, projectId: string) =>
		fetchJson<DiskUsageResponse>(
			`/agents/projects/${encodeURIComponent(projectId)}/disk-usage?agent_id=${encodeURIComponent(agentId)}`,
		),

	createProjectRepo: async (agentId: string, projectId: string, request: CreateRepoRequest): Promise<{ repo: ProjectRepo }> => {
		const response = await fetch(`${getApiBase()}/agents/projects/${encodeURIComponent(projectId)}/repos`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ ...request, agent_id: agentId }),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<{ repo: ProjectRepo }>;
	},

	deleteProjectRepo: async (agentId: string, projectId: string, repoId: string): Promise<ProjectActionResponse> => {
		const response = await fetch(
			`${getApiBase()}/agents/projects/${encodeURIComponent(projectId)}/repos/${encodeURIComponent(repoId)}?agent_id=${encodeURIComponent(agentId)}`,
			{ method: "DELETE" },
		);
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<ProjectActionResponse>;
	},

	createProjectWorktree: async (agentId: string, projectId: string, request: CreateWorktreeRequest): Promise<{ worktree: ProjectWorktree }> => {
		const response = await fetch(`${getApiBase()}/agents/projects/${encodeURIComponent(projectId)}/worktrees`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ ...request, agent_id: agentId }),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<{ worktree: ProjectWorktree }>;
	},

	deleteProjectWorktree: async (agentId: string, projectId: string, worktreeId: string): Promise<ProjectActionResponse> => {
		const response = await fetch(
			`${getApiBase()}/agents/projects/${encodeURIComponent(projectId)}/worktrees/${encodeURIComponent(worktreeId)}?agent_id=${encodeURIComponent(agentId)}`,
			{ method: "DELETE" },
		);
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<ProjectActionResponse>;
	},

	// TTS / Voice overlay methods (stubs)
	ttsProfiles: async (_agentId: string): Promise<{ id: string; name: string }[]> => {
		// TODO: Implement actual TTS profiles endpoint
		return [];
	},

	webChatSendAudio: async (agentId: string, _sessionId: string, _blob: Blob): Promise<Response> => {
		// TODO: Implement actual audio sending endpoint
		console.warn("webChatSendAudio not implemented", agentId);
		return new Response(null, { status: 501 });
	},

	getEventsUrl: () => `${getApiBase()}/events`,

	listDir: async (path?: string): Promise<ListDirResponse> => {
		const params = new URLSearchParams();
		if (path) params.set("path", path);
		const response = await fetch(`${getApiBase()}/fs/list-dir?${params.toString()}`);
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<ListDirResponse>;
	},

	// ── Code Graph API ────────────────────────────────────────────────────

	codegraphProjects: (status?: CodeGraphIndexStatus) => {
		const params = status ? `?status=${encodeURIComponent(status)}` : "";
		return fetchJson<CodeGraphProjectListResponse>(`/codegraph/projects${params}`);
	},

	codegraphProject: (projectId: string) =>
		fetchJson<CodeGraphProjectDetailResponse>(`/codegraph/projects/${encodeURIComponent(projectId)}`),

	codegraphCreateProject: async (name: string, rootPath: string): Promise<CodeGraphProjectDetailResponse> => {
		const response = await fetch(`${getApiBase()}/codegraph/projects`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ name, root_path: rootPath }),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<CodeGraphProjectDetailResponse>;
	},

	codegraphDeleteProject: async (projectId: string): Promise<CodeGraphActionResponse> => {
		const response = await fetch(
			`${getApiBase()}/codegraph/projects/${encodeURIComponent(projectId)}`,
			{ method: "DELETE" },
		);
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<CodeGraphActionResponse>;
	},

	codegraphReindex: async (projectId: string): Promise<CodeGraphActionResponse> => {
		const response = await fetch(
			`${getApiBase()}/codegraph/projects/${encodeURIComponent(projectId)}/reindex`,
			{ method: "POST" },
		);
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<CodeGraphActionResponse>;
	},

	codegraphCommunities: (projectId: string) =>
		fetchJson<CodeGraphCommunitiesResponse>(`/codegraph/projects/${encodeURIComponent(projectId)}/graph/communities`),

	codegraphProcesses: (projectId: string) =>
		fetchJson<CodeGraphProcessesResponse>(`/codegraph/projects/${encodeURIComponent(projectId)}/graph/processes`),

	codegraphSearch: (projectId: string, query: string, limit = 20) =>
		fetchJson<CodeGraphSearchResponse>(
			`/codegraph/projects/${encodeURIComponent(projectId)}/graph/search?q=${encodeURIComponent(query)}&limit=${limit}`,
		),

	codegraphIndexLog: (projectId: string) =>
		fetchJson<CodeGraphIndexLogResponse>(`/codegraph/projects/${encodeURIComponent(projectId)}/graph/index-log`),

	codegraphRemoveInfo: (projectId: string) =>
		fetchJson<CodeGraphRemoveInfoResponse>(`/codegraph/projects/${encodeURIComponent(projectId)}/remove-info`),

	codegraphNodes: (projectId: string, params?: { label?: string; offset?: number; limit?: number }) => {
		const search = new URLSearchParams();
		if (params?.label) search.set("label", params.label);
		if (params?.offset != null) search.set("offset", String(params.offset));
		if (params?.limit != null) search.set("limit", String(params.limit));
		const qs = search.toString();
		return fetchJson<CodeGraphNodeListResponse>(
			`/codegraph/projects/${encodeURIComponent(projectId)}/graph/nodes${qs ? `?${qs}` : ""}`
		);
	},

	codegraphNode: (projectId: string, nodeId: number, label?: string) => {
		const qs = label ? `?label=${encodeURIComponent(label)}` : "";
		return fetchJson<CodeGraphNodeDetailResponse>(
			`/codegraph/projects/${encodeURIComponent(projectId)}/graph/nodes/${nodeId}${qs}`
		);
	},

	codegraphNodeEdges: (projectId: string, nodeId: number, params?: { direction?: string; edge_type?: string; offset?: number; limit?: number }) => {
		const search = new URLSearchParams();
		if (params?.direction) search.set("direction", params.direction);
		if (params?.edge_type) search.set("edge_type", params.edge_type);
		if (params?.offset != null) search.set("offset", String(params.offset));
		if (params?.limit != null) search.set("limit", String(params.limit));
		const qs = search.toString();
		return fetchJson<CodeGraphEdgeListResponse>(
			`/codegraph/projects/${encodeURIComponent(projectId)}/graph/nodes/${nodeId}/edges${qs ? `?${qs}` : ""}`
		);
	},

	codegraphStats: (projectId: string) =>
		fetchJson<CodeGraphStatsResponse>(`/codegraph/projects/${encodeURIComponent(projectId)}/graph/stats`),

	codegraphBulkNodes: (projectId: string) =>
		fetchJson<CodeGraphBulkNodesResponse>(
			`/codegraph/projects/${encodeURIComponent(projectId)}/graph/bulk-nodes`,
		),

	codegraphBulkEdges: (projectId: string) =>
		fetchJson<CodeGraphBulkEdgesResponse>(
			`/codegraph/projects/${encodeURIComponent(projectId)}/graph/bulk-edges`,
		),

	fsReadFile: (params: { projectId: string; path: string; startLine?: number; endLine?: number }) => {
		const qs = new URLSearchParams({
			project_id: params.projectId,
			path: params.path,
		});
		if (params.startLine !== undefined) qs.set("start_line", String(params.startLine));
		if (params.endLine !== undefined) qs.set("end_line", String(params.endLine));
		return fetchJson<FsReadFileResponse>(`/fs/read-file?${qs.toString()}`);
	},
};
