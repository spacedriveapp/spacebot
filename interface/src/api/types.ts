// Type re-exports from OpenAPI schema
import type { components } from "./schema";

// =============================================================================
// System Types
// =============================================================================

export type StatusResponse = components["schemas"]["StatusResponse"];
export type IdleResponse = components["schemas"]["IdleResponse"];
export type HealthResponse = components["schemas"]["HealthResponse"];
export type InstanceOverviewResponse =
  components["schemas"]["InstanceOverviewResponse"];

// =============================================================================
// Event/SSE Types
// =============================================================================

// NOTE: These SSE event types are NOT in the OpenAPI schema - they are manual types
// defined in client.ts because they come from server-sent events, not REST responses.
// Keep these in client.ts:
// - InboundMessageEvent
// - OutboundMessageEvent
// - OutboundMessageDeltaEvent
// - TypingStateEvent
// - WorkerStartedEvent
// - WorkerStatusEvent
// - WorkerIdleEvent
// - WorkerCompletedEvent
// - BranchStartedEvent
// - BranchCompletedEvent
// - ToolStartedEvent
// - ToolCompletedEvent
// - OpenCodePartUpdatedEvent
// - WorkerTextEvent
// - CortexChatMessageEvent
// - ApiEvent (union of all above)
// - OpenCodeToolState
// - OpenCodePart
// - CortexChatSSEEvent

// =============================================================================
// Channel Types
// =============================================================================

export type ChannelResponse = components["schemas"]["ChannelResponse"];
export type ChannelsResponse = components["schemas"]["ChannelsResponse"];
export type MessagesResponse = components["schemas"]["MessagesResponse"];
export type TimelineItem = components["schemas"]["TimelineItem"];

// Status-related
export type CancelProcessRequest =
  components["schemas"]["CancelProcessRequest"];
export type CancelProcessResponse =
  components["schemas"]["CancelProcessResponse"];

// Prompt inspection
export type PromptCaptureBody = components["schemas"]["PromptCaptureBody"];

// Archive
export type SetChannelArchiveRequest =
  components["schemas"]["SetChannelArchiveRequest"];

// =============================================================================
// Worker Types
// =============================================================================

export type WorkerListItem = components["schemas"]["WorkerListItem"];
export type WorkerListResponse = components["schemas"]["WorkerListResponse"];
export type WorkerDetailResponse =
  components["schemas"]["WorkerDetailResponse"];

// Transcript
export type ActionContent = components["schemas"]["ActionContent"];
export type TranscriptStep = components["schemas"]["TranscriptStep"];

// =============================================================================
// Agent Types
// =============================================================================

export type AgentInfo = components["schemas"]["AgentInfo"];
export type AgentsResponse = components["schemas"]["AgentsResponse"];
export type AgentSummary = components["schemas"]["AgentSummary"];
export type AgentOverviewResponse =
  components["schemas"]["AgentOverviewResponse"];
export type AgentProfile = components["schemas"]["AgentProfile"];
export type AgentProfileResponse =
  components["schemas"]["AgentProfileResponse"];

// CRUD
export type CreateAgentRequest = components["schemas"]["CreateAgentRequest"];
export type UpdateAgentRequest = components["schemas"]["UpdateAgentRequest"];

// Warmup
export type WarmupSection = components["schemas"]["WarmupSection"];
export type WarmupUpdate = components["schemas"]["WarmupUpdate"];
export type WarmupStatus = components["schemas"]["WarmupStatus"];
export type WarmupStatusEntry = components["schemas"]["WarmupStatusEntry"];
export type WarmupStatusResponse =
  components["schemas"]["WarmupStatusResponse"];
export type WarmupState = components["schemas"]["WarmupState"];
export type WarmupTriggerRequest =
  components["schemas"]["WarmupTriggerRequest"];
export type WarmupTriggerResponse =
  components["schemas"]["WarmupTriggerResponse"];

// Webchat conversations
export type WebChatConversation = components["schemas"]["WebChatConversation"];
export type WebChatConversationSummary =
  components["schemas"]["WebChatConversationSummary"];
export type WebChatConversationsResponse =
  components["schemas"]["WebChatConversationsResponse"];
export type WebChatConversationResponse =
  components["schemas"]["WebChatConversationResponse"];
export type CreateWebChatConversationRequest =
  components["schemas"]["CreateWebChatConversationRequest"];
export type UpdateWebChatConversationRequest =
  components["schemas"]["UpdateWebChatConversationRequest"];
export type WebChatHistoryMessage =
  components["schemas"]["WebChatHistoryMessage"];

// Activity
export type ActivityDayCount = components["schemas"]["ActivityDayCount"];
export type DayCount = components["schemas"]["DayCount"];
export type HeatmapCell = components["schemas"]["HeatmapCell"];

// =============================================================================
// Memory Types
// =============================================================================

export type Memory = components["schemas"]["Memory"];
export type MemoryType = components["schemas"]["MemoryType"];
export type MemoriesListResponse =
  components["schemas"]["MemoriesListResponse"];

// Search
export type MemorySearchResult = components["schemas"]["MemorySearchResult"];
export type MemoriesSearchResponse =
  components["schemas"]["MemoriesSearchResponse"];

// Graph
export type Association = components["schemas"]["Association"];
export type RelationType = components["schemas"]["RelationType"];
export type MemoryGraphResponse = components["schemas"]["MemoryGraphResponse"];
export type MemoryGraphNeighborsResponse =
  components["schemas"]["MemoryGraphNeighborsResponse"];

// =============================================================================
// Cortex Types
// =============================================================================

export type CortexEvent = components["schemas"]["CortexEvent"];
export type CortexEventsResponse =
  components["schemas"]["CortexEventsResponse"];

// Chat
export type CortexChatMessage = components["schemas"]["CortexChatMessage"];
export type CortexChatMessagesResponse =
  components["schemas"]["CortexChatMessagesResponse"];
export type CortexChatThread = components["schemas"]["CortexChatThread"];
export type CortexChatThreadsResponse =
  components["schemas"]["CortexChatThreadsResponse"];
export type CortexChatToolCall = components["schemas"]["CortexChatToolCall"];
export type CortexChatSendRequest =
  components["schemas"]["CortexChatSendRequest"];
export type CortexChatDeleteThreadRequest =
  components["schemas"]["CortexChatDeleteThreadRequest"];

// =============================================================================
// Config Types
// =============================================================================

export type AgentConfigResponse = components["schemas"]["AgentConfigResponse"];
export type AgentConfigUpdateRequest =
  components["schemas"]["AgentConfigUpdateRequest"];

// Sections
export type RoutingSection = components["schemas"]["RoutingSection"];
export type TuningSection = components["schemas"]["TuningSection"];
export type CompactionSection = components["schemas"]["CompactionSection"];
export type CortexSection = components["schemas"]["CortexSection"];
export type CoalesceSection = components["schemas"]["CoalesceSection"];
export type MemoryPersistenceSection =
  components["schemas"]["MemoryPersistenceSection"];
export type BrowserSection = components["schemas"]["BrowserSection"];
export type ChannelSection = components["schemas"]["ChannelSection"];
export type SandboxSection = components["schemas"]["SandboxSection"];
export type ProjectsSection = components["schemas"]["ProjectsSection"];
export type DiscordSection = components["schemas"]["DiscordSection"];

// Updates (partial)
export type RoutingUpdate = components["schemas"]["RoutingUpdate"];
export type TuningUpdate = components["schemas"]["TuningUpdate"];
export type CompactionUpdate = components["schemas"]["CompactionUpdate"];
export type CortexUpdate = components["schemas"]["CortexUpdate"];
export type CoalesceUpdate = components["schemas"]["CoalesceUpdate"];
export type MemoryPersistenceUpdate =
  components["schemas"]["MemoryPersistenceUpdate"];
export type BrowserUpdate = components["schemas"]["BrowserUpdate"];
export type ChannelUpdate = components["schemas"]["ChannelUpdate"];
export type SandboxUpdate = components["schemas"]["SandboxUpdate"];
export type ProjectsUpdate = components["schemas"]["ProjectsUpdate"];
export type DiscordUpdate = components["schemas"]["DiscordUpdate"];

// Browser
export type ClosePolicy = components["schemas"]["ClosePolicy"];

// Global settings
export type GlobalSettingsResponse =
  components["schemas"]["GlobalSettingsResponse"];
export type GlobalSettingsUpdate =
  components["schemas"]["GlobalSettingsUpdate"];
export type GlobalSettingsUpdateResponse =
  components["schemas"]["GlobalSettingsUpdateResponse"];

// OpenCode (within global settings)
export type OpenCodeSettingsResponse =
  components["schemas"]["OpenCodeSettingsResponse"];
export type OpenCodeSettingsUpdate =
  components["schemas"]["OpenCodeSettingsUpdate"];
export type OpenCodePermissionsResponse =
  components["schemas"]["OpenCodePermissionsResponse"];
export type OpenCodePermissionsUpdate =
  components["schemas"]["OpenCodePermissionsUpdate"];

// Raw config
export type RawConfigResponse = components["schemas"]["RawConfigResponse"];
export type RawConfigUpdateRequest =
  components["schemas"]["RawConfigUpdateRequest"];
export type RawConfigUpdateResponse =
  components["schemas"]["RawConfigUpdateResponse"];

// =============================================================================
// Cron Types
// =============================================================================

export type CronJobInfo = components["schemas"]["CronJobInfo"];
export type CronJobWithStats = components["schemas"]["CronJobWithStats"];
export type CronListResponse = components["schemas"]["CronListResponse"];
export type CronExecutionEntry = components["schemas"]["CronExecutionEntry"];
export type CronExecutionsResponse =
  components["schemas"]["CronExecutionsResponse"];
export type CronActionResponse = components["schemas"]["CronActionResponse"];

// Requests
export type CreateCronRequest = components["schemas"]["CreateCronRequest"];
export type ToggleCronRequest = components["schemas"]["ToggleCronRequest"];
export type TriggerCronRequest = components["schemas"]["TriggerCronRequest"];

// =============================================================================
// Provider/Model Types
// =============================================================================

export type ProviderStatus = components["schemas"]["ProviderStatus"];
export type ProvidersResponse = components["schemas"]["ProvidersResponse"];
export type ProviderUpdateRequest =
  components["schemas"]["ProviderUpdateRequest"];
export type ProviderUpdateResponse =
  components["schemas"]["ProviderUpdateResponse"];

// Model testing
export type ProviderModelTestRequest =
  components["schemas"]["ProviderModelTestRequest"];
export type ProviderModelTestResponse =
  components["schemas"]["ProviderModelTestResponse"];

// OAuth
export type OpenAiOAuthBrowserStartRequest =
  components["schemas"]["OpenAiOAuthBrowserStartRequest"];
export type OpenAiOAuthBrowserStartResponse =
  components["schemas"]["OpenAiOAuthBrowserStartResponse"];
export type OpenAiOAuthBrowserStatusResponse =
  components["schemas"]["OpenAiOAuthBrowserStatusResponse"];

// Models
export type ModelInfo = components["schemas"]["ModelInfo"];
export type ModelsResponse = components["schemas"]["ModelsResponse"];

// =============================================================================
// Ingest Types
// =============================================================================

export type IngestFileInfo = components["schemas"]["IngestFileInfo"];
export type IngestFilesResponse = components["schemas"]["IngestFilesResponse"];
export type IngestUploadResponse =
  components["schemas"]["IngestUploadResponse"];
export type IngestDeleteResponse =
  components["schemas"]["IngestDeleteResponse"];

// =============================================================================
// Skills Types
// =============================================================================

export type SkillInfo = components["schemas"]["SkillInfo"];
export type SkillsListResponse = components["schemas"]["SkillsListResponse"];
export type SkillContentResponse =
  components["schemas"]["SkillContentResponse"];

// Install/Remove
export type InstallSkillRequest = components["schemas"]["InstallSkillRequest"];
export type InstallSkillResponse =
  components["schemas"]["InstallSkillResponse"];
export type RemoveSkillRequest = components["schemas"]["RemoveSkillRequest"];
export type RemoveSkillResponse = components["schemas"]["RemoveSkillResponse"];
export type UploadSkillResponse = components["schemas"]["UploadSkillResponse"];

// Registry
export type RegistrySkill = components["schemas"]["RegistrySkill"];
export type RegistryBrowseResponse =
  components["schemas"]["RegistryBrowseResponse"];
export type RegistrySearchResponse =
  components["schemas"]["RegistrySearchResponse"];
export type RegistrySkillContentResponse =
  components["schemas"]["RegistrySkillContentResponse"];

// =============================================================================
// Tasks Types
// =============================================================================

export type Task = components["schemas"]["Task"];
export type TaskStatus = components["schemas"]["TaskStatus"];
export type TaskPriority = components["schemas"]["TaskPriority"];
export type TaskSubtask = components["schemas"]["TaskSubtask"];
export type TaskListResponse = components["schemas"]["TaskListResponse"];
export type TaskResponse = components["schemas"]["TaskResponse"];
export type TaskActionResponse = components["schemas"]["TaskActionResponse"];

// Requests
export type CreateTaskRequest = components["schemas"]["CreateTaskRequest"];
export type UpdateTaskRequest = components["schemas"]["UpdateTaskRequest"];
export type ApproveRequest = components["schemas"]["ApproveRequest"];
export type AssignRequest = components["schemas"]["AssignRequest"];

// =============================================================================
// Messaging Types
// =============================================================================

export type PlatformStatus = components["schemas"]["PlatformStatus"];
export type AdapterInstanceStatus =
  components["schemas"]["AdapterInstanceStatus"];
export type MessagingStatusResponse =
  components["schemas"]["MessagingStatusResponse"];
export type MessagingInstanceActionResponse =
  components["schemas"]["MessagingInstanceActionResponse"];

// Instances
export type CreateMessagingInstanceRequest =
  components["schemas"]["CreateMessagingInstanceRequest"];
export type DeleteMessagingInstanceRequest =
  components["schemas"]["DeleteMessagingInstanceRequest"];
export type InstanceCredentials = components["schemas"]["InstanceCredentials"];

// Bindings
export type BindingResponse = components["schemas"]["BindingResponse"];
export type BindingsListResponse =
  components["schemas"]["BindingsListResponse"];
export type CreateBindingRequest =
  components["schemas"]["CreateBindingRequest"];
export type CreateBindingResponse =
  components["schemas"]["CreateBindingResponse"];
export type UpdateBindingRequest =
  components["schemas"]["UpdateBindingRequest"];
export type UpdateBindingResponse =
  components["schemas"]["UpdateBindingResponse"];
export type DeleteBindingRequest =
  components["schemas"]["DeleteBindingRequest"];
export type DeleteBindingResponse =
  components["schemas"]["DeleteBindingResponse"];
export type PlatformCredentials = components["schemas"]["PlatformCredentials"];

// Toggles
export type TogglePlatformRequest =
  components["schemas"]["TogglePlatformRequest"];
export type DisconnectPlatformRequest =
  components["schemas"]["DisconnectPlatformRequest"];

// Web chat
export type WebChatSendRequest = components["schemas"]["WebChatSendRequest"];
export type WebChatSendResponse = components["schemas"]["WebChatSendResponse"];

// =============================================================================
// Links Types
// =============================================================================

export type TopologyAgent = components["schemas"]["TopologyAgent"];
export type TopologyLink = components["schemas"]["TopologyLink"];
export type TopologyGroup = components["schemas"]["TopologyGroup"];
export type TopologyHuman = components["schemas"]["TopologyHuman"];
export type TopologyResponse = components["schemas"]["TopologyResponse"];

// Groups
export type CreateGroupRequest = components["schemas"]["CreateGroupRequest"];
export type UpdateGroupRequest = components["schemas"]["UpdateGroupRequest"];

// Humans
export type CreateHumanRequest = components["schemas"]["CreateHumanRequest"];
export type UpdateHumanRequest = components["schemas"]["UpdateHumanRequest"];

// Links
export type CreateLinkRequest = components["schemas"]["CreateLinkRequest"];
export type UpdateLinkRequest = components["schemas"]["UpdateLinkRequest"];

// =============================================================================
// Projects Types
// =============================================================================

export type Project = components["schemas"]["Project"];
export type ProjectStatus = components["schemas"]["ProjectStatus"];
export type ProjectRepo = components["schemas"]["ProjectRepo"];
export type ProjectWorktree = components["schemas"]["ProjectWorktree"];
export type ProjectWorktreeWithRepo =
  components["schemas"]["ProjectWorktreeWithRepo"];
export type ProjectWithRelations =
  components["schemas"]["ProjectWithRelations"];
export type ProjectListResponse = components["schemas"]["ProjectListResponse"];
export type ProjectResponse = components["schemas"]["ProjectResponse"];

// Disk usage
export type DiskUsageEntry = components["schemas"]["DiskUsageEntry"];
export type DiskUsageResponse = components["schemas"]["DiskUsageResponse"];

// CRUD
export type CreateProjectRequest =
  components["schemas"]["CreateProjectRequest"];
export type UpdateProjectRequest =
  components["schemas"]["UpdateProjectRequest"];
export type CreateRepoRequest = components["schemas"]["CreateRepoRequest"];
export type CreateWorktreeRequest =
  components["schemas"]["CreateWorktreeRequest"];

// Responses
export type RepoResponse = components["schemas"]["RepoResponse"];
export type WorktreeResponse = components["schemas"]["WorktreeResponse"];
export type ActionResponse = components["schemas"]["ActionResponse"];

// =============================================================================
// Secrets/Settings Types
// =============================================================================

export type SecretCategory = components["schemas"]["SecretCategory"];
export type SecretListItem = components["schemas"]["SecretListItem"];
export type SecretListResponse = components["schemas"]["SecretListResponse"];
export type SecretInfoResponse = components["schemas"]["SecretInfoResponse"];

// CRUD
export type PutSecretBody = components["schemas"]["PutSecretBody"];
export type PutSecretResponse = components["schemas"]["PutSecretResponse"];
export type DeleteSecretResponse =
  components["schemas"]["DeleteSecretResponse"];

// Encryption
export type EncryptResponse = components["schemas"]["EncryptResponse"];
export type UnlockBody = components["schemas"]["UnlockBody"];

// Migration
export type MigrationItem = components["schemas"]["MigrationItem"];
export type MigrateResponse = components["schemas"]["MigrateResponse"];

// Export/Import
export type ExportData = components["schemas"]["ExportData"];
export type ExportEntry = components["schemas"]["ExportEntry"];
export type ImportBody = components["schemas"]["ImportBody"];

// Storage
export type StorageStatus = components["schemas"]["StorageStatus"];

// NOTE: Backup operations use raw binary data (zip/octet-stream), not JSON schema types:
// - Backup export returns application/zip
// - Backup restore accepts application/octet-stream
