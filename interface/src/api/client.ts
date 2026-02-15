const API_BASE = "/api";

export interface StatusResponse {
	status: string;
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
}

export interface ToolCompletedEvent {
	type: "tool_completed";
	agent_id: string;
	channel_id: string | null;
	process_type: ProcessType;
	process_id: string;
	tool_name: string;
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

export interface AgentInfo {
	id: string;
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

export interface AgentSummary {
	id: string;
	channel_count: number;
	memory_total: number;
	cron_job_count: number;
	activity_sparkline: number[];
	last_activity_at: string | null;
	last_bulletin_at: string | null;
}

export interface InstanceOverviewResponse {
	uptime_seconds: number;
	pid: number;
	agents: AgentSummary[];
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
	rate_limit_cooldown_secs: number;
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
}

// Partial update types - all fields are optional
export interface RoutingUpdate {
	channel?: string;
	branch?: string;
	worker?: string;
	compactor?: string;
	cortex?: string;
	rate_limit_cooldown_secs?: number;
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
}

// -- Cron Types --

export interface CronJobWithStats {
	id: string;
	prompt: string;
	interval_secs: number;
	delivery_target: string;
	enabled: boolean;
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
}

export interface CronExecutionsParams {
	cron_id?: string;
	limit?: number;
}

export interface ProviderStatus {
	anthropic: boolean;
	openai: boolean;
	openrouter: boolean;
}

export interface ProvidersResponse {
	providers: ProviderStatus;
	has_any: boolean;
}

export interface ProviderActionResponse {
	success: boolean;
	message: string;
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

// -- Messaging / Bindings Types --

export interface PlatformStatus {
	configured: boolean;
	enabled: boolean;
}

export interface MessagingStatusResponse {
	discord: PlatformStatus;
	slack: PlatformStatus;
	webhook: PlatformStatus;
}

export interface BindingInfo {
	agent_id: string;
	channel: string;
	guild_id: string | null;
	workspace_id: string | null;
	chat_id: string | null;
	channel_ids: string[];
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
	dm_allowed_users?: string[];
	platform_credentials?: {
		discord_token?: string;
		slack_bot_token?: string;
		slack_app_token?: string;
	};
}

export interface CreateBindingResponse {
	success: boolean;
	restart_required: boolean;
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

export const api = {
	status: () => fetchJson<StatusResponse>("/status"),
	overview: () => fetchJson<InstanceOverviewResponse>("/overview"),
	agents: () => fetchJson<AgentsResponse>("/agents"),
	agentOverview: (agentId: string) =>
		fetchJson<AgentOverviewResponse>(`/agents/overview?agent_id=${encodeURIComponent(agentId)}`),
	channels: () => fetchJson<ChannelsResponse>("/channels"),
	channelMessages: (channelId: string, limit = 20, before?: string) => {
		const params = new URLSearchParams({ channel_id: channelId, limit: String(limit) });
		if (before) params.set("before", before);
		return fetchJson<MessagesResponse>(`/channels/messages?${params}`);
	},
	channelStatus: () => fetchJson<ChannelStatusResponse>("/channels/status"),
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
	updateProvider: async (provider: string, apiKey: string) => {
		const response = await fetch(`${API_BASE}/providers`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ provider, api_key: apiKey }),
		});
		if (!response.ok) {
			throw new Error(`API error: ${response.status}`);
		}
		return response.json() as Promise<ProviderActionResponse>;
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

	eventsUrl: `${API_BASE}/events`,
};
