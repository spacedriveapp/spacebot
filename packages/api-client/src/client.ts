import type {
  PortalConversationResponse,
  PortalConversationsResponse,
  PortalHistoryMessage,
  ConversationDefaultsResponse,
  HealthResponse,
  MemoriesListResponse,
  MessagesResponse,
  StatusResponse,
  TaskListResponse,
  TimelineItem,
  WorkerDetailResponse,
  WorkerListResponse,
  WarmupStatusResponse,
} from "./types";

let baseUrl = "";
let authToken: string | null = null;

export function setServerUrl(url: string) {
  baseUrl = url.replace(/\/+$/, "");
}

export function getServerUrl() {
  return baseUrl;
}

export function setAuthToken(token: string | null | undefined) {
  authToken = token?.trim() || null;
}

export function getAuthToken() {
  return authToken;
}

export function getApiBase() {
  return baseUrl ? `${baseUrl}/api` : "/api";
}

export function getEventsUrl() {
  return `${getApiBase()}/events`;
}

function buildHeaders(extra?: HeadersInit): HeadersInit {
  return {
    "Content-Type": "application/json",
    ...(authToken ? { Authorization: `Bearer ${authToken}` } : {}),
    ...(extra ?? {}),
  };
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`${getApiBase()}${path}`, {
    ...init,
    headers: buildHeaders(init?.headers),
  });

  if (!response.ok) {
    throw new Error(`Spacebot API error: ${response.status}`);
  }

  return response.json() as Promise<T>;
}

export const apiClient = {
  health() {
    return request<HealthResponse>("/health");
  },

  status() {
    return request<StatusResponse>("/status");
  },

  warmup(agentId: string) {
    return request<WarmupStatusResponse>(
      `/agents/warmup?agent_id=${encodeURIComponent(agentId)}`,
    );
  },

  portalHistory(agentId: string, sessionId: string, limit = 100) {
    const params = new URLSearchParams({
      agent_id: agentId,
      session_id: sessionId,
      limit: String(limit),
    });
    return request<PortalHistoryMessage[]>(
      `/portal/history?${params.toString()}`,
    );
  },

  channelMessages(channelId: string, limit = 200, before?: string) {
    const params = new URLSearchParams({
      channel_id: channelId,
      limit: String(limit),
    });

    if (before) {
      params.set('before', before);
    }

    return request<MessagesResponse>(`/channels/messages?${params.toString()}`);
  },

  portalSend(input: {
    agentId: string;
    sessionId: string;
    senderName?: string;
    message: string;
  }) {
    return request<{ ok: boolean }>("/portal/send", {
      method: "POST",
      body: JSON.stringify({
        agent_id: input.agentId,
        session_id: input.sessionId,
        sender_name: input.senderName ?? "user",
        message: input.message,
      }),
    });
  },

  listPortalConversations(
    agentId: string,
    includeArchived = false,
    limit = 100,
  ) {
    const params = new URLSearchParams({
      agent_id: agentId,
      include_archived: includeArchived ? "true" : "false",
      limit: String(limit),
    });
    return request<PortalConversationsResponse>(
      `/portal/conversations?${params.toString()}`,
    );
  },

  createPortalConversation(input: { agentId: string; title?: string | null }) {
    return request<PortalConversationResponse>("/portal/conversations", {
      method: "POST",
      body: JSON.stringify({
        agent_id: input.agentId,
        title: input.title ?? undefined,
      }),
    });
  },

  updatePortalConversation(input: {
    agentId: string;
    sessionId: string;
    title?: string | null;
    archived?: boolean;
  }) {
    return request<PortalConversationResponse>(
      `/portal/conversations/${encodeURIComponent(input.sessionId)}`,
      {
        method: "PUT",
        body: JSON.stringify({
          agent_id: input.agentId,
          title: input.title ?? undefined,
          archived: input.archived,
        }),
      },
    );
  },

  deletePortalConversation(agentId: string, sessionId: string) {
    const params = new URLSearchParams({ agent_id: agentId });
    return request<{ ok: boolean }>(
      `/portal/conversations/${encodeURIComponent(sessionId)}?${params.toString()}`,
      {
        method: "DELETE",
      },
    );
  },

  getConversationDefaults(agentId: string) {
    return request<ConversationDefaultsResponse>(`/conversation-defaults?agent_id=${encodeURIComponent(agentId)}`);
  },

  listTasks(agentId: string, limit = 20) {
    const params = new URLSearchParams({
      agent_id: agentId,
      limit: String(limit),
    });
    return request<TaskListResponse>(`/tasks?${params.toString()}`);
  },

  listMemories(agentId: string, limit = 12, sort = "recent") {
    const params = new URLSearchParams({
      agent_id: agentId,
      limit: String(limit),
      sort,
    });
    return request<MemoriesListResponse>(
      `/agents/memories?${params.toString()}`,
    );
  },

  listWorkers(input: {
    agentId: string;
    limit?: number;
    offset?: number;
    status?: string;
  }) {
    const params = new URLSearchParams({
      agent_id: input.agentId,
      limit: String(input.limit ?? 50),
      offset: String(input.offset ?? 0),
    });

    if (input.status) {
      params.set("status", input.status);
    }

    return request<WorkerListResponse>(`/agents/workers?${params.toString()}`);
  },

  workerDetail(agentId: string, workerId: string) {
    const params = new URLSearchParams({
      agent_id: agentId,
      worker_id: workerId,
    });

    return request<WorkerDetailResponse>(`/agents/workers/detail?${params.toString()}`);
  },

  cancelProcess(input: {
    channelId: string;
    processType: "worker" | "branch";
    processId: string;
  }) {
    return request<{ success: boolean; message: string }>("/channels/cancel-process", {
      method: "POST",
      body: JSON.stringify({
        channel_id: input.channelId,
        process_type: input.processType,
        process_id: input.processId,
      }),
    });
  },

  // ── Voice / TTS ──────────────────────────────────────────────────────

  webChatSendAudio(
    agentId: string,
    sessionId: string,
    audioBlob: Blob,
    senderName?: string,
  ): Promise<Response> {
    const formData = new FormData();
    formData.append("agent_id", agentId);
    formData.append("session_id", sessionId);
    formData.append("sender_name", senderName ?? "user");
    formData.append("audio", audioBlob, "voice.webm");
    return fetch(`${getApiBase()}/webchat/send-audio`, {
      method: "POST",
      body: formData,
      ...(authToken ? { headers: { Authorization: `Bearer ${authToken}` } } : {}),
    });
  },

  async ttsGenerate(
    text: string,
    options?: { profileId?: string; engine?: string; agentId?: string },
  ): Promise<ArrayBuffer> {
    const response = await fetch(`${getApiBase()}/tts/generate`, {
      method: "POST",
      headers: buildHeaders(),
      body: JSON.stringify({
        text,
        profile_id: options?.profileId,
        engine: options?.engine,
        agent_id: options?.agentId,
      }),
    });
    if (!response.ok) throw new Error(`TTS error: ${response.status}`);
    return response.arrayBuffer();
  },

  ttsProfiles(agentId?: string) {
    const params = agentId ? `?agent_id=${encodeURIComponent(agentId)}` : "";
    return request<TtsProfile[]>(`/tts/profiles${params}`);
  },
};

export interface TtsProfile {
  id: string;
  name?: string | null;
  default_engine?: string | null;
  preset_engine?: string | null;
  effects_chain?: unknown;
}
