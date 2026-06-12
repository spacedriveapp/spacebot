import type { CortexChatToolCall } from "./types";

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

export type OpenCodeToolState =
  | { status: "pending" }
  | { status: "running"; title?: string; input?: string }
  | { status: "completed"; title?: string; input?: string; output?: string }
  | { status: "error"; error?: string };

export type OpenCodePart =
  | { type: "text"; id: string; text: string }
  | ({ type: "tool"; id: string; tool: string } & OpenCodeToolState)
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
  tool_calls?: CortexChatToolCall[];
}

export interface SpokenResponseEvent {
  type: "spoken_response";
  agent_id: string;
  channel_id: string;
  spoken_text: string;
  full_text: string;
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
  | CortexChatMessageEvent
  | SpokenResponseEvent;
