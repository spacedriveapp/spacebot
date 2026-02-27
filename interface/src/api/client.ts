export const BASE_PATH: string = (window as any).__SPACEBOT_BASE_PATH || "";
const API_BASE = BASE_PATH + "/api";

export interface StatusResponse {
	status: string;
	version: string;
	pid: number;
	uptime_seconds: number;
}

export interface ChannelInfo {
	agent_id: string;
	id: string;
	platform: string;
	display_name: string | null;
	is_active: boolean;
	last_activity_at: string;
	created_at: string;
}

export interface ChannelsResponse {
	channels: ChannelInfo[];
}

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
}

export interface WorkerStatusEvent {
	type: "worker_status";
	agent_id: string;
	channel_id: string | null;
	worker_id: string;
	status: string;
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

export type ApiEvent =
	| InboundMessageEvent
	| OutboundMessageEvent
	| TypingStateEvent
	| WorkerStartedEvent
	| WorkerStatusEvent
	| WorkerCompletedEvent
	| BranchStartedEvent
	| BranchCompletedEvent
	| ToolStartedEvent
	| ToolCompletedEvent;

async function fetchJson<T>(path: string): Promise<T> {
	const response = await fetch(`${API_BASE}${path}`);
	if (!response.ok) {
		throw new Error(`API error: ${response.status}`);
	}
	return response.json();
}

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

export type TimelineItem = TimelineMessage | TimelineBranchRun | TimelineWorkerRun;

export interface MessagesResponse {
	items: TimelineItem[];
	has_more: boolean;
}

export interface WorkerStatusInfo {
	id: string;
	task: string;
	status: string;
	started_at: string;
	notify_on_complete: boolean;
	tool_calls: number;
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

/** channel_id -> StatusBlockSnapshot */
export type ChannelStatusResponse = Record<string, StatusBlockSnapshot>;

// --- Workers API types ---

export type ActionContent =
	| { type: "text"; text: string }
	| { type: "tool_call"; id: string; name: string; args: string };

export type TranscriptStep =
	| { type: "action"; content: ActionContent[] }
	| { type: "tool_result"; call_id: string; name: string; text: string };

export interface WorkerRunInfo {
	id: string;
	task: string;
	status: string;
	worker_type: string;
	channel_id: string | null;
	channel_name: string | null;
	started_at: string;
	completed_at: string | null;
	has_transcript: boolean;
	live_status: string | null;
	tool_calls: number;
}

export interface WorkerDetailResponse {
	id: string;
	task: string;
	result: string | null;
	status: string;
	worker_type: string;
	channel_id: string | null;
	channel_name: string | null;
	started_at: string;
	completed_at: string | null;
	transcript: TranscriptStep[] | null;
	tool_calls: number;
}

export interface WorkerListResponse {
	workers: WorkerRunInfo[];
	total: number;
}

export interface AgentInfo {
	id: string;
	display_name?: string;
	role?: string;
	workspace: string;
	context_window: number;
	max_turns: number;
	max_concurrent_branches: number;
	max_concurrent_workers: number;
}

export interface AgentsResponse {
	agents: AgentInfo[];
}

export interface CronJobInfo {
	id: string;
	prompt: string;
	interval_secs: number;
	delivery_target: string;
	enabled: boolean;
	run_once: boolean;
	active_hours: [number, number] | null;
}

export interface AgentOverviewResponse {
	memory_counts: Record<string, number>;
	memory_total: number;
	channel_count: number;
	cron_jobs: CronJobInfo[];
	last_bulletin_at: string | null;
	recent_cortex_events: CortexEvent[];
	memory_daily: { date: string; count: number }[];
	activity_daily: { date: string; branches: number; workers: number }[];
	activity_heatmap: { day: number; hour: number; count: number }[];
	latest_bulletin: string | null;
}

export interface AgentProfile {
	agent_id: string;
	display_name: string | null;
	status: string | null;
	bio: string | null;
	avatar_seed: string | null;
	generated_at: string;
	updated_at: string;
}

export interface AgentProfileResponse {
	profile: AgentProfile | null;
}

export interface AgentSummary {
	id: string;
	channel_count: number;
	memory_total: number;
	cron_job_count: number;
	activity_sparkline: number[];
	last_activity_at: string | null;
	last_bulletin_at: string | null;
	profile: AgentProfile | null;
}

export interface InstanceOverviewResponse {
	version: string;
	uptime_seconds: number;
	pid: number;
	agents: AgentSummary[];
}

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

export type RelationType =
	| "related_to"
	| "updates"
	| "contradicts"
	| "caused_by"
	| "result_of"
	| "part_of";

export interface AssociationItem {
	id: string;
	source_id: string;
	target_id: string;
	relation_type: RelationType;
	weight: number;
	created_at: string;
}

export interface MemoryGraphResponse {
	nodes: MemoryItem[];
	edges: AssociationItem[];
	total: number;
}

export interface MemoryGraphNeighborsResponse {
	nodes: MemoryItem[];
	edges: AssociationItem[];
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

// -- Cortex Chat --

export interface CortexChatMessage {
	id: string;
	thread_id: string;
	role: "user" | "assistant";
	content: string;
	channel_context: string | null;
	created_at: string;
}

export interface CortexChatMessagesResponse {
	messages: CortexChatMessage[];
	thread_id: string;
}

export type CortexChatSSEEvent =
	| { type: "thinking" }
	| { type: "done"; full_text: string }
	| { type: "error"; message: string };

export interface IdentityFiles {
	soul: string | null;
	identity: string | null;
	user: string | null;
}

export interface IdentityUpdateRequest {
	agent_id: string;
	soul?: string | null;
	identity?: string | null;
	user?: string | null;
}

// -- Agent Config Types --

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
}

export interface SandboxSection {
	mode: "enabled" | "disabled";
	writable_paths: string[];
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
	discord: DiscordSection;
	sandbox: SandboxSection;
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
}

export interface SandboxUpdate {
	mode?: "enabled" | "disabled";
	writable_paths?: string[];
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
	discord?: DiscordUpdate;
	sandbox?: SandboxUpdate;
}

// -- Cron Types --

export interface CronJobWithStats {
	id: string;
	prompt: string;
	interval_secs: number;
	delivery_target: string;
	enabled: boolean;
	run_once: boolean;
	active_hours: [number, number] | null;
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
	interval_secs: number;
	delivery_target: string;
	active_start_hour?: number;
	active_end_hour?: number;
	enabled: boolean;
	run_once: boolean;
}

export interface CronExecutionsParams {
	cron_id?: string;
	limit?: number;
}

export interface ProviderStatus {
	anthropic: boolean;
	openai: boolean;
	openai_chatgpt: boolean;
	openrouter: boolean;
	kilo: boolean;
	zhipu: boolean;
	groq: boolean;
	together: boolean;
	fireworks: boolean;
	deepseek: boolean;
	xai: boolean;
	mistral: boolean;
	gemini: boolean;
	ollama: boolean;
	opencode_zen: boolean;
	opencode_go: boolean;
	nvidia: boolean;
	minimax: boolean;
	minimax_cn: boolean;
	moonshot: boolean;
	zai_coding_plan: boolean;
}

export interface ProvidersResponse {
	providers: ProviderStatus;
	has_any: boolean;
}

export interface ProviderActionResponse {
	success: boolean;
	message: string;
}

export interface ProviderModelTestResponse {
	success: boolean;
	message: string;
	provider: string;
	model: string;
	sample: string | null;
}

export interface OpenAiOAuthBrowserStartResponse {
	success: boolean;
	message: string;
	user_code: string | null;
	verification_url: string | null;
	state: string | null;
}

export interface OpenAiOAuthBrowserStatusResponse {
	found: boolean;
	done: boolean;
	success: boolean;
	message: string | null;
}

// -- Model Types --

export interface ModelInfo {
	id: string;
	name: string;
	provider: string;
	context_window: number | null;
	tool_call: boolean;
	reasoning: boolean;
	input_audio: boolean;
}

export interface ModelsResponse {
	models: ModelInfo[];
}

// -- Ingest Types --

export interface IngestFileInfo {
	content_hash: string;
	filename: string;
	file_size: number;
	total_chunks: number;
	chunks_completed: number;
	status: "queued" | "processing" | "completed" | "failed";
	started_at: string;
	completed_at: string | null;
}

export interface IngestFilesResponse {
	files: IngestFileInfo[];
}

export interface IngestUploadResponse {
	uploaded: string[];
}

export interface IngestDeleteResponse {
	success: boolean;
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

// -- Task Types --

export type TaskStatus = "pending_approval" | "backlog" | "ready" | "in_progress" | "done";
export type TaskPriority = "critical" | "high" | "medium" | "low";

export interface TaskSubtask {
	title: string;
	completed: boolean;
}

export interface TaskItem {
	id: string;
	agent_id: string;
	task_number: number;
	title: string;
	description?: string;
	status: TaskStatus;
	priority: TaskPriority;
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
	subtasks?: TaskSubtask[];
	metadata?: Record<string, unknown>;
	complete_subtask?: number;
	worker_id?: string;
	approved_by?: string;
}

// -- Messaging / Bindings Types --

export interface PlatformStatus {
	configured: boolean;
	enabled: boolean;
}

export interface MessagingStatusResponse {
	discord: PlatformStatus;
	slack: PlatformStatus;
	telegram: PlatformStatus;
	webhook: PlatformStatus;
	twitch: PlatformStatus;
	email: PlatformStatus;
}

export interface BindingInfo {
	agent_id: string;
	channel: string;
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
	original_guild_id?: string;
	original_workspace_id?: string;
	original_chat_id?: string;
	agent_id: string;
	channel: string;
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
	guild_id?: string;
	workspace_id?: string;
	chat_id?: string;
}

export interface DeleteBindingResponse {
	success: boolean;
	message: string;
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

export interface GlobalSettingsResponse {
	brave_search_key: string | null;
	api_enabled: boolean;
	api_port: number;
	api_bind: string;
	worker_log_mode: string;
	opencode: OpenCodeSettings;
}

export interface GlobalSettingsUpdate {
	brave_search_key?: string | null;
	api_enabled?: boolean;
	api_port?: number;
	api_bind?: string;
	worker_log_mode?: string;
	opencode?: OpenCodeSettingsUpdate;
}

export interface GlobalSettingsUpdateResponse {
	success: boolean;
	message: string;
	requires_restart: boolean;
}

export interface RawConfigResponse {
	content: string;
}

export interface RawConfigUpdateResponse {
	success: boolean;
	message: string;
}

// -- Agent Links & Topology --

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

export interface TopologyAgent {
	id: string;
	name: string;
	display_name?: string;
	role?: string;
}

export interface TopologyLink {
	from: string;
	to: string;
	direction: string;
	kind: string;
}

export interface TopologyGroup {
	name: string;
	agent_ids: string[];
	color?: string;
}

export interface TopologyHuman {
	id: string;
	display_name?: string;
	role?: string;
	bio?: string;
}

export interface TopologyResponse {
	agents: TopologyAgent[];
	humans: TopologyHuman[];
	links: TopologyLink[];
	groups: TopologyGroup[];
}

export interface CreateHumanRequest {
	id: string;
	display_name?: string;
	role?: string;
	bio?: string;
}

export interface UpdateHumanRequest {
	display_name?: string;
	role?: string;
	bio?: string;
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

export interface AgentMessageEvent {
	from_agent_id: string;
	to_agent_id: string;
	link_id: string;
	channel_id: string;
}

export const api = {
	status: () => fetchJson<StatusResponse>("/status"),
	overview: () => fetchJson<InstanceOverviewResponse>("/overview"),
	agents: () => fetchJson<AgentsResponse>("/agents"),
	agentOverview: (agentId: string) =>
		fetchJson<AgentOverviewResponse>(`/agents/overview?agent_id=${encodeURIComponent(agentId)}`),
	channels: () => fetchJson<ChannelsResponse>("/channels"),
	deleteChannel: async (agentId: string, channelId: string) => {
		const params = new URLSearchParams({ agent_id: agentId, channel_id: channelId });
		const response = await fetch(`${API_BASE}/channels?${params}`, { method: "DELETE" });
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<{ success: boolean }>;
	},
	channelMessages: (channelId: string, limit = 20, before?: string) => {
		const params = new URLSearchParams({ channel_id: channelId, limit: String(limit) });
		if (before) params.set("before", before);
		return fetchJson<MessagesResponse>(`/channels/messages?${params}`);
	},
	channelStatus: () => fetchJson<ChannelStatusResponse>("/channels/status"),
	workersList: (agentId: string, params: { limit?: number; offset?: number; status?: string } = {}) => {
		const search = new URLSearchParams({ agent_id: agentId });
		if (params.limit) search.set("limit", String(params.limit));
		if (params.offset) search.set("offset", String(params.offset));
		if (params.status) search.set("status", params.status);
		return fetchJson<WorkerListResponse>(`/agents/workers?${search}`);
	},
	workerDetail: (agentId: string, workerId: string) =>
		fetchJson<WorkerDetailResponse>(`/agents/workers/detail?agent_id=${encodeURIComponent(agentId)}&worker_id=${encodeURIComponent(workerId)}`),
	agentMemories: (agentId: string, params: MemoriesListParams = {}) => {
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
		return fetchJson<MemoryGraphResponse>(`/agents/memories/graph?${search}`);
	},
	memoryGraphNeighbors: (agentId: string, memoryId: string, params: MemoryGraphNeighborsParams = {}) => {
		const search = new URLSearchParams({ agent_id: agentId, memory_id: memoryId });
		if (params.depth) search.set("depth", String(params.depth));
		if (params.exclude?.length) search.set("exclude", params.exclude.join(","));
		return fetchJson<MemoryGraphNeighborsResponse>(`/agents/memories/graph/neighbors?${search}`);
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
		return fetchJson<CortexChatMessagesResponse>(`/cortex-chat/messages?${search}`);
	},
	cortexChatSend: (agentId: string, threadId: string, message: string, channelId?: string) =>
		fetch(`${API_BASE}/cortex-chat/send`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({
				agent_id: agentId,
				thread_id: threadId,
				message,
				channel_id: channelId ?? null,
			}),
		}),
	agentProfile: (agentId: string) =>
		fetchJson<AgentProfileResponse>(`/agents/profile?agent_id=${encodeURIComponent(agentId)}`),
	agentIdentity: (agentId: string) =>
		fetchJson<IdentityFiles>(`/agents/identity?agent_id=${encodeURIComponent(agentId)}`),
	updateIdentity: async (request: IdentityUpdateRequest) => {
		const response = await fetch(`${API_BASE}/agents/identity`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<IdentityFiles>;
	},
	createAgent: async (agentId: string, displayName?: string, role?: string) => {
		const response = await fetch(`${API_BASE}/agents`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ agent_id: agentId, display_name: displayName || undefined, role: role || undefined }),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<{ success: boolean; agent_id: string; message: string }>;
	},

	updateAgent: async (agentId: string, update: { display_name?: string; role?: string }) => {
		const response = await fetch(`${API_BASE}/agents`, {
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
		const response = await fetch(`${API_BASE}/agents?${params}`, {
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
		const response = await fetch(`${API_BASE}/agents/config`, {
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
		const response = await fetch(`${API_BASE}/agents/cron`, {
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
		const response = await fetch(`${API_BASE}/agents/cron?${search}`, {
			method: "DELETE",
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<CronActionResponse>;
	},

	toggleCronJob: async (agentId: string, cronId: string, enabled: boolean) => {
		const response = await fetch(`${API_BASE}/agents/cron/toggle`, {
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
		const response = await fetch(`${API_BASE}/agents/cron/trigger`, {
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
		const response = await fetch(`${API_BASE}/channels/cancel`, {
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
	providers: () => fetchJson<ProvidersResponse>("/providers"),
	updateProvider: async (provider: string, apiKey: string, model: string) => {
		const response = await fetch(`${API_BASE}/providers`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ provider, api_key: apiKey, model }),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<ProviderActionResponse>;
	},
	testProviderModel: async (provider: string, apiKey: string, model: string) => {
		const response = await fetch(`${API_BASE}/providers/test`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ provider, api_key: apiKey, model }),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<ProviderModelTestResponse>;
	},
	startOpenAiOAuthBrowser: async (params: {model: string}) => {
		const response = await fetch(`${API_BASE}/providers/openai/oauth/browser/start`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({
				model: params.model,
			}),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<OpenAiOAuthBrowserStartResponse>;
	},
	openAiOAuthBrowserStatus: async (state: string) => {
		const response = await fetch(
			`${API_BASE}/providers/openai/oauth/browser/status?state=${encodeURIComponent(state)}`,
		);
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<OpenAiOAuthBrowserStatusResponse>;
	},
	removeProvider: async (provider: string) => {
		const response = await fetch(`${API_BASE}/providers/${encodeURIComponent(provider)}`, {
			method: "DELETE",
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<ProviderActionResponse>;
	},

	// Model listing
	models: (provider?: string, capability?: "input_audio" | "voice_transcription") => {
		const params = new URLSearchParams();
		if (provider) params.set("provider", provider);
		if (capability) params.set("capability", capability);
		const query = params.toString() ? `?${params.toString()}` : "";
		return fetchJson<ModelsResponse>(`/models${query}`);
	},
	refreshModels: async () => {
		const response = await fetch(`${API_BASE}/models/refresh`, {
			method: "POST",
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<ModelsResponse>;
	},

	// Ingest API
	ingestFiles: (agentId: string) =>
		fetchJson<IngestFilesResponse>(`/agents/ingest/files?agent_id=${encodeURIComponent(agentId)}`),

	uploadIngestFiles: async (agentId: string, files: File[]) => {
		const formData = new FormData();
		for (const file of files) {
			formData.append("files", file);
		}
		const response = await fetch(
			`${API_BASE}/agents/ingest/upload?agent_id=${encodeURIComponent(agentId)}`,
			{ method: "POST", body: formData },
		);
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<IngestUploadResponse>;
	},

	deleteIngestFile: async (agentId: string, contentHash: string) => {
		const params = new URLSearchParams({ agent_id: agentId, content_hash: contentHash });
		const response = await fetch(`${API_BASE}/agents/ingest/files?${params}`, {
			method: "DELETE",
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<IngestDeleteResponse>;
	},

	// Messaging / Bindings API
	messagingStatus: () => fetchJson<MessagingStatusResponse>("/messaging/status"),

	bindings: (agentId?: string) => {
		const params = agentId
			? `?agent_id=${encodeURIComponent(agentId)}`
			: "";
		return fetchJson<BindingsListResponse>(`/bindings${params}`);
	},

	createBinding: async (request: CreateBindingRequest) => {
		const response = await fetch(`${API_BASE}/bindings`, {
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
		const response = await fetch(`${API_BASE}/bindings`, {
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
		const response = await fetch(`${API_BASE}/bindings`, {
			method: "DELETE",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<DeleteBindingResponse>;
	},

	togglePlatform: async (platform: string, enabled: boolean) => {
		const response = await fetch(`${API_BASE}/messaging/toggle`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ platform, enabled }),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<{ success: boolean; message: string }>;
	},

	disconnectPlatform: async (platform: string) => {
		const response = await fetch(`${API_BASE}/messaging/disconnect`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ platform }),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<{ success: boolean; message: string }>;
	},

	// Global Settings API
	globalSettings: () => fetchJson<GlobalSettingsResponse>("/settings"),
	
	updateGlobalSettings: async (settings: GlobalSettingsUpdate) => {
		const response = await fetch(`${API_BASE}/settings`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(settings),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<GlobalSettingsUpdateResponse>;
	},

	// Raw config API
	rawConfig: () => fetchJson<RawConfigResponse>("/config/raw"),
	updateRawConfig: async (content: string) => {
		const response = await fetch(`${API_BASE}/config/raw`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ content }),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<RawConfigUpdateResponse>;
	},

	// Update API
	updateCheck: () => fetchJson<UpdateStatus>("/update/check"),
	updateCheckNow: async () => {
		const response = await fetch(`${API_BASE}/update/check`, { method: "POST" });
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<UpdateStatus>;
	},
	updateApply: async () => {
		const response = await fetch(`${API_BASE}/update/apply`, { method: "POST" });
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<UpdateApplyResponse>;
	},

	// Skills API
	listSkills: (agentId: string) =>
		fetchJson<SkillsListResponse>(`/agents/skills?agent_id=${encodeURIComponent(agentId)}`),
	
	installSkill: async (request: InstallSkillRequest) => {
		const response = await fetch(`${API_BASE}/agents/skills/install`, {
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
		const response = await fetch(`${API_BASE}/agents/skills/remove`, {
			method: "DELETE",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<RemoveSkillResponse>;
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

	// Agent Links & Topology API
	topology: () => fetchJson<TopologyResponse>("/topology"),
	links: () => fetchJson<LinksResponse>("/links"),
	agentLinks: (agentId: string) =>
		fetchJson<LinksResponse>(`/agents/${encodeURIComponent(agentId)}/links`),
	createLink: async (request: CreateLinkRequest): Promise<{ link: AgentLinkResponse }> => {
		const response = await fetch(`${API_BASE}/links`, {
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
			`${API_BASE}/links/${encodeURIComponent(from)}/${encodeURIComponent(to)}`,
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
			`${API_BASE}/links/${encodeURIComponent(from)}/${encodeURIComponent(to)}`,
			{ method: "DELETE" },
		);
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
	},

	// Agent Groups API
	groups: () => fetchJson<{ groups: TopologyGroup[] }>("/groups"),
	createGroup: async (request: CreateGroupRequest): Promise<{ group: TopologyGroup }> => {
		const response = await fetch(`${API_BASE}/groups`, {
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
			`${API_BASE}/groups/${encodeURIComponent(name)}`,
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
			`${API_BASE}/groups/${encodeURIComponent(name)}`,
			{ method: "DELETE" },
		);
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
	},

	// Humans API
	humans: () => fetchJson<{ humans: TopologyHuman[] }>("/humans"),
	createHuman: async (request: CreateHumanRequest): Promise<{ human: TopologyHuman }> => {
		const response = await fetch(`${API_BASE}/humans`, {
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
			`${API_BASE}/humans/${encodeURIComponent(id)}`,
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
			`${API_BASE}/humans/${encodeURIComponent(id)}`,
			{ method: "DELETE" },
		);
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
	},

	// Web Chat API
	webChatSend: (agentId: string, sessionId: string, message: string, senderName?: string) =>
		fetch(`${API_BASE}/webchat/send`, {
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
		fetch(`${API_BASE}/webchat/history?agent_id=${encodeURIComponent(agentId)}&session_id=${encodeURIComponent(sessionId)}&limit=${limit}`),

	// Tasks API
	listTasks: (agentId: string, params?: { status?: TaskStatus; priority?: TaskPriority; limit?: number }) => {
		const search = new URLSearchParams({ agent_id: agentId });
		if (params?.status) search.set("status", params.status);
		if (params?.priority) search.set("priority", params.priority);
		if (params?.limit) search.set("limit", String(params.limit));
		return fetchJson<TaskListResponse>(`/agents/tasks?${search}`);
	},
	getTask: (agentId: string, taskNumber: number) =>
		fetchJson<TaskResponse>(`/agents/tasks/${taskNumber}?agent_id=${encodeURIComponent(agentId)}`),
	createTask: async (agentId: string, request: CreateTaskRequest): Promise<TaskResponse> => {
		const response = await fetch(`${API_BASE}/agents/tasks`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ ...request, agent_id: agentId }),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<TaskResponse>;
	},
	updateTask: async (agentId: string, taskNumber: number, request: UpdateTaskRequest): Promise<TaskResponse> => {
		const response = await fetch(`${API_BASE}/agents/tasks/${taskNumber}`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ ...request, agent_id: agentId }),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<TaskResponse>;
	},
	deleteTask: async (agentId: string, taskNumber: number): Promise<TaskActionResponse> => {
		const response = await fetch(`${API_BASE}/agents/tasks/${taskNumber}?agent_id=${encodeURIComponent(agentId)}`, {
			method: "DELETE",
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<TaskActionResponse>;
	},
	approveTask: async (agentId: string, taskNumber: number, approvedBy?: string): Promise<TaskResponse> => {
		const response = await fetch(`${API_BASE}/agents/tasks/${taskNumber}/approve`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ agent_id: agentId, approved_by: approvedBy }),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<TaskResponse>;
	},
	executeTask: async (agentId: string, taskNumber: number): Promise<TaskResponse> => {
		const response = await fetch(`${API_BASE}/agents/tasks/${taskNumber}/execute`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ agent_id: agentId }),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<TaskResponse>;
	},

	eventsUrl: `${API_BASE}/events`,
};
