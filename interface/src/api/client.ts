declare global {
	interface Window {
		__SPACEBOT_BASE_PATH?: string;
	}
}

export const BASE_PATH: string = window.__SPACEBOT_BASE_PATH || "";

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

export function getApiBase(): string {
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
	ProviderEntry,
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

// Conversation-related types
export type { ConversationSettings, ConversationDefaultsResponse } from "./types";
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
	attachments?: AttachmentMeta[];
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
	call_id: string;
	tool_name: string;
	args: string;
}

export interface ToolOutputEvent {
	type: "tool_output";
	agent_id: string;
	channel_id: string | null;
	process_type: ProcessType;
	process_id: string;
	/** Stable identifier matching the tool_call that initiated this stream. */
	call_id: string;
	tool_name: string;
	line: string;
	stream: "stdout" | "stderr";
}

export interface ToolCompletedEvent {
	type: "tool_completed";
	agent_id: string;
	channel_id: string | null;
	process_type: ProcessType;
	process_id: string;
	call_id: string;
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
	| ToolOutputEvent
	| OpenCodePartUpdatedEvent
	| WorkerTextEvent
	| CortexChatMessageEvent;

// -- Timeline types (discriminated union parts) --

export type AttachmentMeta = Types.SavedAttachmentMeta;

export type TimelineMessage = Types.TimelineItem;

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

/**
 * A mutating call whose refusal text is the point.
 *
 * Every workflow endpoint answers a rejection with a plain-text body that names
 * what is actually wrong — "step `draft` cannot wait for itself", "no step
 * `nope` in this workflow", "the steps form a cycle and cannot be ordered:
 * draft -> publish -> review". Those sentences are the whole diagnosis, and
 * flattening them into "API error: 409" leaves the author with a number and no
 * idea which edge to remove. So the body is the message; the status code is
 * only the fallback for the rare empty one.
 */
async function mutateJson<T>(
	path: string,
	method: string,
	body?: unknown,
): Promise<T> {
	const response = await fetch(`${getApiBase()}${path}`, {
		method,
		...(body === undefined
			? {}
			: {
					headers: {"Content-Type": "application/json"},
					body: JSON.stringify(body),
				}),
	});
	if (!response.ok) {
		const text = (await response.text().catch(() => "")).trim();
		throw new Error(text || `API error: ${response.status}`);
	}
	return (await response.json()) as T;
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

/**
 * One entry in the prompt history. Mirrors rig's `Message` enum as
 * serialized to JSON: role plus content that may be a plain string,
 * a single block, or an array of blocks depending on the LLM provider.
 */
export interface PromptHistoryMessage {
	role: string;
	content: PromptHistoryContent;
}

export type PromptHistoryContent =
	| string
	| PromptHistoryBlock
	| PromptHistoryBlock[];

/**
 * A single content block inside a `PromptHistoryMessage`. Fields are
 * optional because rig's content variants are structurally different:
 * text blocks, tool calls, tool results, and reasoning all flow through
 * the same channel.
 */
export interface PromptHistoryBlock {
	type?: string;
	text?: string;
	id?: string;
	content?: unknown;
	function?: {
		name: string;
		arguments: string | Record<string, unknown>;
	};
	reasoning?: string[];
}

export interface PromptInspectResponse {
	channel_id: string;
	system_prompt: string;
	total_chars: number;
	history_length: number;
	history: PromptHistoryMessage[];
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
	history: PromptHistoryMessage[];
	history_length: number;
}

export interface PromptCaptureResponse {
	channel_id: string;
	capture_enabled: boolean;
}

// --- Memory helper types (extended beyond schema) ---

// Extended MemoryType with additional values not yet in schema
export type MemoryType = Types.MemoryType;

export const MEMORY_TYPES: MemoryType[] = [
	"fact", "preference", "decision", "identity",
	"event", "observation", "goal", "todo",
];

export type MemorySort = "recent" | "importance" | "most_accessed";

// Extended MemoryItem with forgotten field (not yet in schema)
export type MemoryItem = Types.Memory;

export type MemoriesListResponse = Types.MemoriesListResponse;

export type MemorySearchResultItem = Types.MemorySearchResult;

export type MemoriesSearchResponse = Types.MemoriesSearchResponse;

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

export type CortexEvent = Types.CortexEvent;

export type CortexEventsResponse = Types.CortexEventsResponse;

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

export type PresetDefaults = Types.PresetDefaults;

export type PresetMeta = Types.PresetMeta;

export interface PresetsResponse {
	presets: PresetMeta[];
}

// -- Config types with frontend-specific extensions --

export type RoutingSection = Types.RoutingSection;

export type TuningSection = Types.TuningSection;

export type CompactionSection = Types.CompactionSection;

export type CortexSection = Types.CortexSection;

export type CoalesceSection = Types.CoalesceSection;

export type MemoryPersistenceSection = Types.MemoryPersistenceSection;

export type BrowserSection = Types.BrowserSection;

export type ChannelSection = Types.ChannelSection;

export type SandboxSection = Types.SandboxSection;

export type ProjectsSection = Types.ProjectsSection;

export type DiscordSection = Types.DiscordSection;

export type AgentConfigResponse = Types.AgentConfigResponse;

// Partial update types - all fields are optional
export type RoutingUpdate = Types.RoutingUpdate;

export type TuningUpdate = Types.TuningUpdate;

export type CompactionUpdate = Types.CompactionUpdate;

export type CortexUpdate = Types.CortexUpdate;

export type CoalesceUpdate = Types.CoalesceUpdate;

export type MemoryPersistenceUpdate = Types.MemoryPersistenceUpdate;

export type BrowserUpdate = Types.BrowserUpdate;

export type ChannelUpdate = Types.ChannelUpdate;

export type SandboxUpdate = Types.SandboxUpdate;

export type ProjectsUpdate = Types.ProjectsUpdate;

export type DiscordUpdate = Types.DiscordUpdate;

export type AgentConfigUpdateRequest = Types.AgentConfigUpdateRequest;

// -- Cron Types --

export type CronJobWithStats = Types.CronJobWithStats;

export type CronExecutionEntry = Types.CronExecutionEntry;

export type CronListResponse = Types.CronListResponse;

export type CronExecutionsResponse = Types.CronExecutionsResponse;

export type CronActionResponse = Types.CronActionResponse;

export type CreateCronRequest = Types.CreateCronRequest;

export interface CronExecutionsParams {
	cron_id?: string;
	limit?: number;
}

// -- Update Types --

export type Deployment = Types.Deployment;

export type UpdateStatus = Types.UpdateStatus;

export interface UpdateApplyResponse {
	status: "updating" | "error";
	error?: string;
}

// -- Global Settings Types --

export type OpenCodePermissions = Types.OpenCodePermissionsResponse;

export type OpenCodeSettings = Types.OpenCodeSettingsResponse;

export type OpenCodeSettingsUpdate = Types.OpenCodeSettingsUpdate;

export type GlobalSettingsUpdate = Types.GlobalSettingsUpdate;

// -- Skills Types --

export type SkillInfo = Types.SkillInfo;

export type SkillsListResponse = Types.SkillsListResponse;

export type InstallSkillRequest = Types.InstallSkillRequest;

export type InstallSkillResponse = Types.InstallSkillResponse;

export type RemoveSkillRequest = Types.RemoveSkillRequest;

export type RemoveSkillResponse = Types.RemoveSkillResponse;

// -- Skills Registry Types (skills.sh) --

export type RegistryView = "all-time" | "trending" | "hot";

export type RegistrySkill = Types.RegistrySkill;

export type RegistryBrowseResponse = Types.RegistryBrowseResponse;

export type RegistrySearchResponse = Types.RegistrySearchResponse;

export type SkillContentResponse = Types.SkillContentResponse;

export type UploadSkillResponse = Types.UploadSkillResponse;

// -- Task Types --
//
// Aliased straight from the generated OpenAPI schema rather than hand-written.
// These used to be duplicated by hand here, which `check-typegen` cannot catch:
// it only diffs `schema.d.ts` against the Rust, so a local redeclaration could
// drift from the server indefinitely and the build stayed green.
//
// `TaskItem` is kept as the name most call sites use.
export type Task = Types.Task;
export type TaskStatus = Types.TaskStatus;
export type TaskPriority = Types.TaskPriority;
export type TaskSubtask = Types.TaskSubtask;
export type TaskRun = Types.TaskRun;
export type TaskRunOutcome = Types.TaskRunOutcome;
export type TaskRunsResponse = Types.TaskRunsResponse;
export type TaskListResponse = Types.TaskListResponse;
export type TaskResponse = Types.TaskResponse;
export type TaskActionResponse = Types.TaskActionResponse;
export type BlockKind = Types.BlockKind;
export type TaskEdgeSummary = Types.TaskEdgeSummary;
export type TaskDependenciesResponse = Types.TaskDependenciesResponse;
export type TaskTransition = Types.TaskTransition;
export type TaskTransitionsResponse = Types.TaskTransitionsResponse;
export type ContractProblem = Types.ContractProblem;
export type ContractSide = Types.ContractSide;
export type TaskInputBinding = Types.TaskInputBinding;
export type TaskContractResponse = Types.TaskContractResponse;
export type TaskProvenanceResponse = Types.TaskProvenanceResponse;
export type TaskItem = Types.Task;

export type CreateTaskRequest = Types.CreateTaskRequest;

export type UpdateTaskRequest = Types.UpdateTaskRequest;

// -- Workflow Types --
//
// A workflow is the reusable template; a run is one launch of it, compiled into
// real tasks with real dependency edges. Same rule as above: everything with a
// server counterpart is aliased from the generated schema, never redeclared.
export type Workflow = Types.Workflow;
export type WorkflowStep = Types.WorkflowStep;
export type WorkflowEdge = Types.WorkflowEdge;
export type StepBinding = Types.StepBinding;
export type BindingSource = Types.BindingSource;
export type WorkflowListResponse = Types.WorkflowListResponse;
export type WorkflowResponse = Types.WorkflowResponse;
export type WorkflowDetailResponse = Types.WorkflowDetailResponse;
export type WorkflowActionResponse = Types.WorkflowActionResponse;
export type WorkflowRun = Types.WorkflowRun;
export type RunDetailResponse = Types.RunDetailResponse;
export type RunListResponse = Types.RunListResponse;

export type SaveWorkflowRequest = Types.SaveWorkflowRequest;
export type SaveStepRequest = Types.SaveStepRequest;
export type SaveBindingRequest = Types.SaveBindingRequest;
export type StepEdgeRequest = Types.StepEdgeRequest;
export type LaunchRequest = Types.LaunchRequest;
export type LaunchResponse = Types.LaunchResponse;

// -- Notification Types --

export type NotificationKind = "task_approval" | "worker_failed" | "cortex_observation";
export type NotificationSeverity = "info" | "warn" | "error";

export type NotificationItem = Types.Notification;

export type NotificationsResponse = Types.NotificationsResponse;

export type UnreadCountResponse = Types.UnreadCountResponse;

export interface NotificationCreatedEvent {
	type: "notification_created";
	notification: NotificationItem;
}

export interface NotificationUpdatedEvent {
	type: "notification_updated";
	id: string;
	read: boolean;
	dismissed: boolean;
}

// -- Messaging / Bindings Types --

export type BindingInfo = Types.BindingResponse;

export type BindingsListResponse = Types.BindingsListResponse;

export type CreateBindingRequest = Types.CreateBindingRequest;

export type CreateBindingResponse = Types.CreateBindingResponse;

export type UpdateBindingRequest = Types.UpdateBindingRequest;

export type UpdateBindingResponse = Types.UpdateBindingResponse;

export type DeleteBindingRequest = Types.DeleteBindingRequest;

export type DeleteBindingResponse = Types.DeleteBindingResponse;

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

export type CreateHumanRequest = Types.CreateHumanRequest;

export type UpdateHumanRequest = Types.UpdateHumanRequest;

export type CreateGroupRequest = Types.CreateGroupRequest;

export type UpdateGroupRequest = Types.UpdateGroupRequest;

export type CreateLinkRequest = Types.CreateLinkRequest;

export type UpdateLinkRequest = Types.UpdateLinkRequest;

// -- Projects Types --

export type ProjectStatus = Types.ProjectStatus;

export type Project = Types.Project;

export type ProjectRepo = Types.ProjectRepo;

export type ProjectWorktree = Types.ProjectWorktree;

export type ProjectWorktreeWithRepo = Types.ProjectWorktreeWithRepo;

/** GET /agents/projects response */
export type ProjectListResponse = Types.ProjectListResponse;

/** GET /agents/projects/:id response — project fields are flattened */
export type ProjectWithRelations = Types.ProjectWithRelations;

export interface ProjectActionResponse {
	success: boolean;
	message: string;
}

export type DiskUsageEntry = Types.DiskUsageEntry;

export type DiskUsageResponse = Types.DiskUsageResponse;

export type CreateProjectRequest = Types.CreateProjectRequest;

export type UpdateProjectRequest = Types.UpdateProjectRequest;

export type CreateRepoRequest = Types.CreateRepoRequest;

export type CreateWorktreeRequest = Types.CreateWorktreeRequest;

// -- Secrets Types --

export type SecretCategory = Types.SecretCategory;
export type StoreState = "unencrypted" | "locked" | "unlocked";

export interface SecretStoreStatus {
	state: StoreState;
	encrypted: boolean;
	secret_count: number;
	system_count: number;
	tool_count: number;
	platform_managed: boolean;
}

export type SecretListItem = Types.SecretListItem;

export type SecretListResponse = Types.SecretListResponse;

export type PutSecretResponse = Types.PutSecretResponse;

export type DeleteSecretResponse = Types.DeleteSecretResponse;

export type EncryptResponse = Types.EncryptResponse;

export interface UnlockResponse {
	state: string;
	secret_count: number;
	message: string;
}

export type MigrationItem = Types.MigrationItem;

export type MigrateResponse = Types.MigrateResponse;

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
		fetchJson<PromptInspectResponse>(`/channels/prompt/inspect?channel_id=${encodeURIComponent(channelId)}`),
	setPromptCapture: async (channelId: string, enabled: boolean) => {
		const response = await fetch(`${getApiBase()}/channels/prompt/capture`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ channel_id: channelId, enabled }),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<PromptCaptureResponse>;
	},
	listPromptSnapshots: (channelId: string, limit = 50) =>
		fetchJson<PromptSnapshotListResponse>(
			`/channels/prompt/snapshots?channel_id=${encodeURIComponent(channelId)}&limit=${limit}`,
		),
	getPromptSnapshot: (channelId: string, timestampMs: number) =>
		fetchJson<PromptSnapshot>(
			`/channels/prompt/snapshots/get?channel_id=${encodeURIComponent(channelId)}&timestamp_ms=${timestampMs}`,
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

	// `agent_id` is supplied by the caller's route context and injected here,
	// so the request object itself never carries it.
	createCronJob: async (
		agentId: string,
		request: Omit<CreateCronRequest, "agent_id">,
	) => {
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
		const response = await fetch(`${getApiBase()}/channels/cancel-process`, {
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
	updateProvider: async (
		provider: string,
		apiKey: string,
		model: string,
		apiType: string,
		baseUrl?: string,
	) => {
		const response = await fetch(`${getApiBase()}/providers`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ provider, api_key: apiKey, model, api_type: apiType, base_url: baseUrl }),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<Types.ProviderUpdateResponse>;
	},
	testProviderModel: async (
		provider: string,
		apiKey: string,
		model: string,
		apiType: string,
		baseUrl?: string,
	) => {
		const response = await fetch(`${getApiBase()}/providers/test-model`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ provider, api_key: apiKey, model, api_type: apiType, base_url: baseUrl }),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<Types.ProviderModelTestResponse>;
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
			`${getApiBase()}/agents/ingest/files?agent_id=${encodeURIComponent(agentId)}`,
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
		const body: Types.TogglePlatformRequest = {
			platform,
			enabled,
			adapter: adapter ?? null,
		};
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
		const body: Types.DisconnectPlatformRequest = {
			platform,
			adapter: adapter ?? null,
		};
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
	rawConfig: () => fetchJson<Types.RawConfigResponse>("/settings/raw"),
	updateRawConfig: async (content: string) => {
		const response = await fetch(`${getApiBase()}/settings/raw`, {
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
	updateCheck: () => fetchJson<UpdateStatus>("/update-check"),
	updateCheckNow: async () => {
		const response = await fetch(`${getApiBase()}/update-check`, { method: "POST" });
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<UpdateStatus>;
	},
	updateApply: async () => {
		const response = await fetch(`${getApiBase()}/update-apply`, { method: "POST" });
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
		return response.json() as Promise<{ link: AgentLinkResponse }>;
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
		return response.json() as Promise<{ link: AgentLinkResponse }>;
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
	groups: () => fetchJson<{ groups: TopologyGroup[] }>("/links/groups"),
	createGroup: async (request: CreateGroupRequest): Promise<{ group: TopologyGroup }> => {
		const response = await fetch(`${getApiBase()}/links/groups`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<{ group: TopologyGroup }>;
	},
	updateGroup: async (name: string, request: UpdateGroupRequest): Promise<{ group: TopologyGroup }> => {
		const response = await fetch(
			`${getApiBase()}/links/groups/${encodeURIComponent(name)}`,
			{
				method: "PUT",
				headers: { "Content-Type": "application/json" },
				body: JSON.stringify(request),
			},
		);
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<{ group: TopologyGroup }>;
	},
	deleteGroup: async (name: string): Promise<void> => {
		const response = await fetch(
			`${getApiBase()}/links/groups/${encodeURIComponent(name)}`,
			{ method: "DELETE" },
		);
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
	},

	// Humans API
	humans: () => fetchJson<{ humans: TopologyHuman[] }>("/links/humans"),
	createHuman: async (request: CreateHumanRequest): Promise<{ human: TopologyHuman }> => {
		const response = await fetch(`${getApiBase()}/links/humans`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<{ human: TopologyHuman }>;
	},
	updateHuman: async (id: string, request: UpdateHumanRequest): Promise<{ human: TopologyHuman }> => {
		const response = await fetch(
			`${getApiBase()}/links/humans/${encodeURIComponent(id)}`,
			{
				method: "PUT",
				headers: { "Content-Type": "application/json" },
				body: JSON.stringify(request),
			},
		);
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<{ human: TopologyHuman }>;
	},
	deleteHuman: async (id: string): Promise<void> => {
		const response = await fetch(
			`${getApiBase()}/links/humans/${encodeURIComponent(id)}`,
			{ method: "DELETE" },
		);
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
	},

	// Attachment API
	uploadAttachment: (agentId: string, channelId: string, file: File) => {
		const form = new FormData();
		form.append("file", file, file.name);
		return fetch(
			`${getApiBase()}/agents/${encodeURIComponent(agentId)}/channels/${encodeURIComponent(channelId)}/attachments/upload`,
			{ method: "POST", body: form },
		);
	},

	attachmentUrl: (agentId: string, attachmentId: string, opts?: { thumbnail?: boolean; download?: boolean }) => {
		const params = new URLSearchParams();
		if (opts?.thumbnail) params.set("thumbnail", "true");
		if (opts?.download) params.set("download", "true");
		const qs = params.toString();
		return `${getApiBase()}/agents/${encodeURIComponent(agentId)}/attachments/${encodeURIComponent(attachmentId)}${qs ? `?${qs}` : ""}`;
	},

	listAttachments: (agentId: string, channelId: string, params?: { message_id?: string; limit?: number }) => {
		const search = new URLSearchParams();
		if (params?.message_id) search.set("message_id", params.message_id);
		if (params?.limit) search.set("limit", String(params.limit));
		return fetchJson<{ attachments: Array<{ id: string; original_filename: string; mime_type: string; size_bytes: number; created_at: string }> }>(
			`/agents/${encodeURIComponent(agentId)}/channels/${encodeURIComponent(channelId)}/attachments${search.toString() ? `?${search}` : ""}`,
		);
	},

	// Portal API (renamed from webchat)
	portalSend: (agentId: string, sessionId: string, message: string, senderName?: string, attachmentIds?: string[]) =>
		fetch(`${getApiBase()}/portal/send`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({
				agent_id: agentId,
				session_id: sessionId,
				sender_name: senderName ?? "user",
				message,
				...(attachmentIds?.length ? { attachment_ids: attachmentIds } : {}),
			}),
		}),

	portalHistory: (agentId: string, sessionId: string, limit = 100) =>
		fetch(`${getApiBase()}/portal/history?agent_id=${encodeURIComponent(agentId)}&session_id=${encodeURIComponent(sessionId)}&limit=${limit}`),

	listPortalConversations: (
		agentId: string,
		includeArchived = false,
		limit = 100,
	): Promise<Types.PortalConversationsResponse> =>
		fetchJson<Types.PortalConversationsResponse>(
			`/portal/conversations?agent_id=${encodeURIComponent(agentId)}&include_archived=${includeArchived}&limit=${limit}`,
		),

	createPortalConversation: async (
		agentId: string,
		title?: string,
		settings?: Types.ConversationSettings,
	): Promise<Types.PortalConversationResponse> => {
		const response = await fetch(`${getApiBase()}/portal/conversations`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ agent_id: agentId, title, settings }),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<Types.PortalConversationResponse>;
	},

	updatePortalConversation: async (
		agentId: string,
		sessionId: string,
		title?: string,
		archived?: boolean,
		settings?: Types.ConversationSettings,
	): Promise<Types.PortalConversationResponse> => {
		const response = await fetch(
			`${getApiBase()}/portal/conversations/${encodeURIComponent(sessionId)}`,
			{
				method: "PUT",
				headers: { "Content-Type": "application/json" },
				body: JSON.stringify({ agent_id: agentId, title, archived, settings }),
			},
		);
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<Types.PortalConversationResponse>;
	},

	deletePortalConversation: async (
		agentId: string,
		sessionId: string,
	): Promise<{ success: boolean }> => {
		const response = await fetch(
			`${getApiBase()}/portal/conversations/${encodeURIComponent(sessionId)}?agent_id=${encodeURIComponent(agentId)}`,
			{ method: "DELETE" },
		);
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return { success: true };
	},

	getConversationDefaults: (agentId: string) =>
		fetchJson<Types.ConversationDefaultsResponse>(`/conversation-defaults?agent_id=${encodeURIComponent(agentId)}`),

	// Channel settings API
	getChannelSettings: (channelId: string, agentId: string) =>
		fetchJson<{ conversation_id: string; settings: Types.ConversationSettings }>(
			`/channels/${encodeURIComponent(channelId)}/settings?agent_id=${encodeURIComponent(agentId)}`
		),

	updateChannelSettings: (channelId: string, agentId: string, settings: Types.ConversationSettings) =>
		fetch(`${getApiBase()}/channels/${encodeURIComponent(channelId)}/settings`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ agent_id: agentId, settings }),
		}),

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
	/** Per-attempt execution log for a task, oldest first. */
	listTaskRuns: (taskNumber: number) =>
		fetchJson<TaskRunsResponse>(`/tasks/${taskNumber}/runs`),
	/** Resolves live, so it shows what the task would get if it ran now. */
	getTaskContract: (taskNumber: number) =>
		fetchJson<TaskContractResponse>(`/tasks/${taskNumber}/contract`),
	/** Where this card came from, and what it filed. */
	getTaskProvenance: (taskNumber: number) =>
		fetchJson<TaskProvenanceResponse>(`/tasks/${taskNumber}/provenance`),
	/**
	 * Declare what a task must produce (and may require).
	 *
	 * A human defines the *shape*; only a worker ever writes the *values*, via
	 * `task_complete`. Setting an output schema is what makes that submission
	 * checked rather than taken on trust.
	 */
	setTaskContract: async (
		taskNumber: number,
		body: {input_schema?: unknown; output_schema?: unknown},
	) => {
		const response = await fetch(
			`${getApiBase()}/tasks/${taskNumber}/contract`,
			{
				method: "PUT",
				headers: {"Content-Type": "application/json"},
				body: JSON.stringify(body),
			},
		);
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return (await response.json()) as TaskContractResponse;
	},
	/**
	 * Wire one input to where its value comes from.
	 *
	 * Keyed by input key rather than by an id, so setting the same key twice
	 * rewires it instead of leaving two bindings fighting over one input.
	 *
	 * Exactly one source is meaningful: an upstream task's output at a JSON
	 * Pointer, or a literal. The server rejects a body carrying neither.
	 */
	setTaskBinding: async (
		taskNumber: number,
		inputKey: string,
		body: {
			source_task_number?: number;
			source_pointer?: string;
			literal_value?: unknown;
		},
	) => {
		const response = await fetch(
			`${getApiBase()}/tasks/${taskNumber}/bindings/${encodeURIComponent(inputKey)}`,
			{
				method: "PUT",
				headers: {"Content-Type": "application/json"},
				body: JSON.stringify(body),
			},
		);
		if (!response.ok) {
			// 422 is the "neither a source nor a literal" rejection, and it comes
			// back with an empty body — a bare status code would leave the caller
			// with nothing to say, so name the rule that was broken.
			if (response.status === 422) {
				throw new Error(
					"A binding must either read from a task or carry a literal value.",
				);
			}
			throw new Error((await response.text()) || `API error: ${response.status}`);
		}
		return (await response.json()) as TaskContractResponse;
	},
	removeTaskBinding: async (taskNumber: number, inputKey: string) => {
		const response = await fetch(
			`${getApiBase()}/tasks/${taskNumber}/bindings/${encodeURIComponent(inputKey)}`,
			{method: "DELETE"},
		);
		if (!response.ok) {
			throw new Error((await response.text()) || `API error: ${response.status}`);
		}
		return (await response.json()) as TaskContractResponse;
	},
	listTaskDependencies: (taskNumber: number) =>
		fetchJson<TaskDependenciesResponse>(`/tasks/${taskNumber}/dependencies`),
	/** The legal status moves, so the board never offers one the API rejects. */
	listTaskTransitions: () =>
		fetchJson<TaskTransitionsResponse>("/tasks/transitions"),
	addTaskDependency: async (taskNumber: number, parentTaskNumber: number) => {
		const response = await fetch(
			`${getApiBase()}/tasks/${taskNumber}/dependencies`,
			{
				method: "POST",
				headers: {"Content-Type": "application/json"},
				body: JSON.stringify({parent_task_number: parentTaskNumber}),
			},
		);
		if (!response.ok) {
			// The server explains cycles and self-loops in the body; surfacing
			// only a status code would strip the one detail that helps.
			throw new Error((await response.text()) || `API error: ${response.status}`);
		}
		return (await response.json()) as TaskDependenciesResponse;
	},
	removeTaskDependency: async (taskNumber: number, parentTaskNumber: number) => {
		const response = await fetch(
			`${getApiBase()}/tasks/${taskNumber}/dependencies/${parentTaskNumber}`,
			{method: "DELETE"},
		);
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return (await response.json()) as TaskDependenciesResponse;
	},
	blockTask: async (taskNumber: number, kind: BlockKind, reason: string) => {
		const response = await fetch(`${getApiBase()}/tasks/${taskNumber}/block`, {
			method: "POST",
			headers: {"Content-Type": "application/json"},
			body: JSON.stringify({kind, reason}),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return (await response.json()) as TaskResponse;
	},
	unblockTask: async (taskNumber: number) => {
		const response = await fetch(`${getApiBase()}/tasks/${taskNumber}/unblock`, {
			method: "POST",
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return (await response.json()) as TaskResponse;
	},
	/** Clear the failure budget and requeue. Used by the manual retry action. */
	retryTask: async (taskNumber: number): Promise<TaskResponse> => {
		const response = await fetch(`${getApiBase()}/tasks/${taskNumber}/retry`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<TaskResponse>;
	},

	// Workflows API
	//
	// A workflow is a reusable template; launching one compiles it into real
	// tasks with real dependency edges and hands them to the same scheduler the
	// board already shows. Every mutation below goes through `mutateJson` so the
	// server's refusal text reaches the editor intact.
	listWorkflows: () => fetchJson<WorkflowListResponse>("/workflows"),
	/** The template plus its steps, edges and bindings — one round trip. */
	getWorkflow: (id: string) =>
		fetchJson<WorkflowDetailResponse>(`/workflows/${encodeURIComponent(id)}`),
	createWorkflow: (body: SaveWorkflowRequest) =>
		mutateJson<WorkflowResponse>("/workflows", "POST", body),
	updateWorkflow: (id: string, body: SaveWorkflowRequest) =>
		mutateJson<WorkflowResponse>(
			`/workflows/${encodeURIComponent(id)}`,
			"PUT",
			body,
		),
	deleteWorkflow: (id: string) =>
		mutateJson<WorkflowActionResponse>(
			`/workflows/${encodeURIComponent(id)}`,
			"DELETE",
		),
	/**
	 * Add or replace a step.
	 *
	 * Keyed by `step_key` rather than an id, so saving the same key twice edits
	 * the step instead of leaving two behind — the same rule task bindings use,
	 * and the reason edges and bindings can reference a step by name at all.
	 */
	saveWorkflowStep: (id: string, stepKey: string, body: SaveStepRequest) =>
		mutateJson<WorkflowDetailResponse>(
			`/workflows/${encodeURIComponent(id)}/steps/${encodeURIComponent(stepKey)}`,
			"PUT",
			body,
		),
	deleteWorkflowStep: (id: string, stepKey: string) =>
		mutateJson<WorkflowDetailResponse>(
			`/workflows/${encodeURIComponent(id)}/steps/${encodeURIComponent(stepKey)}`,
			"DELETE",
		),
	addWorkflowEdge: (id: string, body: StepEdgeRequest) =>
		mutateJson<WorkflowDetailResponse>(
			`/workflows/${encodeURIComponent(id)}/edges`,
			"POST",
			body,
		),
	// The pair being removed identifies the edge, and there is no edge id to put
	// in a path — hence a body on DELETE.
	removeWorkflowEdge: (id: string, body: StepEdgeRequest) =>
		mutateJson<WorkflowDetailResponse>(
			`/workflows/${encodeURIComponent(id)}/edges`,
			"DELETE",
			body,
		),
	setWorkflowBinding: (
		id: string,
		stepKey: string,
		inputKey: string,
		body: SaveBindingRequest,
	) =>
		mutateJson<WorkflowDetailResponse>(
			`/workflows/${encodeURIComponent(id)}/steps/${encodeURIComponent(stepKey)}/bindings/${encodeURIComponent(inputKey)}`,
			"PUT",
			body,
		),
	removeWorkflowBinding: (id: string, stepKey: string, inputKey: string) =>
		mutateJson<WorkflowDetailResponse>(
			`/workflows/${encodeURIComponent(id)}/steps/${encodeURIComponent(stepKey)}/bindings/${encodeURIComponent(inputKey)}`,
			"DELETE",
		),
	/** Compile the template into tasks. Returns the step → task number map. */
	launchWorkflow: (id: string, body: LaunchRequest) =>
		mutateJson<LaunchResponse>(
			`/workflows/${encodeURIComponent(id)}/run`,
			"POST",
			body,
		),
	listWorkflowRuns: (id: string) =>
		fetchJson<RunListResponse>(`/workflows/${encodeURIComponent(id)}/runs`),
	// Not nested under the workflow: a run outlives the template it came from.
	getWorkflowRun: (runId: string) =>
		fetchJson<RunDetailResponse>(`/workflow-runs/${encodeURIComponent(runId)}`),

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
		return response.json() as Promise<{ state: string; message: string }>;
	},
	rotateKey: async (): Promise<{ master_key: string; message: string }> => {
		const response = await fetch(`${getApiBase()}/secrets/rotate`, { method: "POST" });
		if (!response.ok) {
			const body = await response.json().catch(() => ({}));
			throw new Error(body.error || `API error: ${response.status}`);
		}
		return response.json() as Promise<{ master_key: string; message: string }>;
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
	listProjects: (status?: ProjectStatus) => {
		const search = new URLSearchParams();
		if (status) search.set("status", status);
		const qs = search.toString();
		return fetchJson<ProjectListResponse>(`/agents/projects${qs ? `?${qs}` : ""}`);
	},

	getProject: (projectId: string) =>
		fetchJson<ProjectWithRelations>(
			`/agents/projects/${encodeURIComponent(projectId)}`,
		),

	createProject: async (request: CreateProjectRequest): Promise<ProjectWithRelations> => {
		const response = await fetch(`${getApiBase()}/agents/projects`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<ProjectWithRelations>;
	},

	updateProject: async (projectId: string, request: UpdateProjectRequest): Promise<ProjectWithRelations> => {
		const response = await fetch(`${getApiBase()}/agents/projects/${encodeURIComponent(projectId)}`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<ProjectWithRelations>;
	},

	deleteProject: async (projectId: string): Promise<ProjectActionResponse> => {
		const response = await fetch(
			`${getApiBase()}/agents/projects/${encodeURIComponent(projectId)}`,
			{ method: "DELETE" },
		);
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<ProjectActionResponse>;
	},

	scanProject: async (projectId: string): Promise<ProjectWithRelations> => {
		const response = await fetch(
			`${getApiBase()}/agents/projects/${encodeURIComponent(projectId)}/scan`,
			{ method: "POST" },
		);
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<ProjectWithRelations>;
	},

	reorderProjects: async (ids: string[]): Promise<void> => {
		const response = await fetch(`${getApiBase()}/agents/projects/reorder`, {
			method: "PUT",
			headers: {"Content-Type": "application/json"},
			body: JSON.stringify({ids}),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
	},

	projectDiskUsage: (projectId: string) =>
		fetchJson<DiskUsageResponse>(
			`/agents/projects/${encodeURIComponent(projectId)}/disk-usage`,
		),

	createProjectRepo: async (projectId: string, request: CreateRepoRequest): Promise<{ repo: ProjectRepo }> => {
		const response = await fetch(`${getApiBase()}/agents/projects/${encodeURIComponent(projectId)}/repos`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<{ repo: ProjectRepo }>;
	},

	deleteProjectRepo: async (projectId: string, repoId: string): Promise<ProjectActionResponse> => {
		const response = await fetch(
			`${getApiBase()}/agents/projects/${encodeURIComponent(projectId)}/repos/${encodeURIComponent(repoId)}`,
			{ method: "DELETE" },
		);
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<ProjectActionResponse>;
	},

	createProjectWorktree: async (projectId: string, request: CreateWorktreeRequest): Promise<{ worktree: ProjectWorktree }> => {
		const response = await fetch(`${getApiBase()}/agents/projects/${encodeURIComponent(projectId)}/worktrees`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<{ worktree: ProjectWorktree }>;
	},

	deleteProjectWorktree: async (projectId: string, worktreeId: string): Promise<ProjectActionResponse> => {
		const response = await fetch(
			`${getApiBase()}/agents/projects/${encodeURIComponent(projectId)}/worktrees/${encodeURIComponent(worktreeId)}`,
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

	portalSendAudio: async (agentId: string, _sessionId: string, _blob: Blob): Promise<Response> => {
		// TODO: Implement actual audio sending endpoint
		console.warn("portalSendAudio not implemented", agentId);
		return new Response(null, { status: 501 });
	},

	// -- Notifications --

	listNotifications: async (params?: {
		filter?: "unread" | "all";
		agent_id?: string;
		kind?: NotificationKind;
		limit?: number;
		offset?: number;
	}): Promise<NotificationsResponse> => {
		const query = new URLSearchParams();
		if (params?.filter) query.set("filter", params.filter);
		if (params?.agent_id) query.set("agent_id", params.agent_id);
		if (params?.kind) query.set("kind", params.kind);
		if (params?.limit !== undefined) query.set("limit", String(params.limit));
		if (params?.offset !== undefined) query.set("offset", String(params.offset));
		const qs = query.toString();
		const response = await fetch(`${getApiBase()}/notifications${qs ? `?${qs}` : ""}`);
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<NotificationsResponse>;
	},

	getUnreadCount: async (): Promise<UnreadCountResponse> => {
		const response = await fetch(`${getApiBase()}/notifications/unread_count`);
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<UnreadCountResponse>;
	},

	markNotificationRead: async (id: string): Promise<void> => {
		const response = await fetch(`${getApiBase()}/notifications/${encodeURIComponent(id)}/read`, {
			method: "POST",
		});
		if (!response.ok && response.status !== 404) throw new Error(`API error: ${response.status}`);
	},

	dismissNotification: async (id: string): Promise<void> => {
		const response = await fetch(`${getApiBase()}/notifications/${encodeURIComponent(id)}/dismiss`, {
			method: "POST",
		});
		if (!response.ok && response.status !== 404) throw new Error(`API error: ${response.status}`);
	},

	markAllNotificationsRead: async (): Promise<void> => {
		const response = await fetch(`${getApiBase()}/notifications/read_all`, { method: "POST" });
		if (!response.ok) throw new Error(`API error: ${response.status}`);
	},

	dismissReadNotifications: async (): Promise<void> => {
		const response = await fetch(`${getApiBase()}/notifications/dismiss_read`, { method: "POST" });
		if (!response.ok) throw new Error(`API error: ${response.status}`);
	},

	getEventsUrl: () => `${getApiBase()}/events`,

	// Wiki API
	listWikiPages: (params?: { page_type?: string }) => {
		const qs = new URLSearchParams();
		if (params?.page_type) qs.set("page_type", params.page_type);
		const query = qs.toString();
		return fetchJson<WikiListResponse>(`/wiki${query ? `?${query}` : ""}`);
	},

	searchWikiPages: (params: { query: string; page_type?: string }) => {
		const qs = new URLSearchParams({ query: params.query });
		if (params.page_type) qs.set("page_type", params.page_type);
		return fetchJson<WikiListResponse>(`/wiki/search?${qs}`);
	},

	getWikiPage: (slug: string, version?: number) => {
		const qs = version !== undefined ? `?version=${version}` : "";
		return fetchJson<WikiPageResponse>(`/wiki/${encodeURIComponent(slug)}${qs}`);
	},

	createWikiPage: async (request: CreateWikiPageRequest): Promise<WikiPageResponse> => {
		const response = await fetch(`${getApiBase()}/wiki`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<WikiPageResponse>;
	},

	editWikiPage: async (slug: string, request: EditWikiPageRequest): Promise<WikiPageResponse> => {
		const response = await fetch(`${getApiBase()}/wiki/${encodeURIComponent(slug)}/edit`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(request),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<WikiPageResponse>;
	},

	getWikiHistory: (slug: string, limit = 20) =>
		fetchJson<WikiHistoryResponse>(`/wiki/${encodeURIComponent(slug)}/history?limit=${limit}`),

	restoreWikiVersion: async (slug: string, version: number): Promise<WikiPageResponse> => {
		const response = await fetch(`${getApiBase()}/wiki/${encodeURIComponent(slug)}/restore`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ version }),
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json() as Promise<WikiPageResponse>;
	},

	archiveWikiPage: async (slug: string): Promise<{ success: boolean; message: string }> => {
		const response = await fetch(`${getApiBase()}/wiki/${encodeURIComponent(slug)}`, {
			method: "DELETE",
		});
		if (!response.ok) throw new Error(`API error: ${response.status}`);
		return response.json();
	},

	usage: (params?: { agent_id?: string; since?: string; until?: string; group_by?: string }) => {
		const qs = new URLSearchParams();
		if (params?.agent_id) qs.set("agent_id", params.agent_id);
		if (params?.since) qs.set("since", params.since);
		if (params?.until) qs.set("until", params.until);
		if (params?.group_by) qs.set("group_by", params.group_by);
		const query = qs.toString();
		return fetchJson<UsageResponse>(`/usage${query ? `?${query}` : ""}`);
	},

	activity: (params?: { since?: string; until?: string }) => {
		const qs = new URLSearchParams();
		if (params?.since) qs.set("since", params.since);
		if (params?.until) qs.set("until", params.until);
		const query = qs.toString();
		return fetchJson<ActivityResponse>(`/activity${query ? `?${query}` : ""}`);
	},
}

export type UsageTotals = Types.UsageTotals;

export type UsageByModel = Types.UsageByModel;

export type UsageResponse = Types.UsageResponse;;

// Activity types
export type ProcessTokens = Types.ProcessTokens;

export type TokenSummary = Types.TokenSummary;

export type ActivityDay = Types.ActivityDay;

export type ActivityTotals = Types.ActivityTotals;

export type ActivityResponse = Types.ActivityResponse;

// Wiki types
export type WikiPageType = "entity" | "concept" | "decision" | "project" | "reference";

export type WikiPageSummary = Types.WikiPageSummary;

export type WikiPage = Types.WikiPage;

export type WikiPageVersion = Types.WikiPageVersion;

export type WikiListResponse = Types.WikiListResponse;

export type WikiPageResponse = Types.WikiPageResponse;

export type WikiHistoryResponse = Types.WikiHistoryResponse;

export type CreateWikiPageRequest = Types.CreatePageRequest;

export type EditWikiPageRequest = Types.EditPageRequest;
