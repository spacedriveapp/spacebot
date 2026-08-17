import {useEffect, useMemo, useRef, useState} from "react";
import {useQuery} from "@tanstack/react-query";
import {ChatMessageList, InlineBranchCard, MessageBubble, type ChatMessageListHandle} from "@spacedrive/ai";
import {File as FileIcon} from "@phosphor-icons/react";
import {
	api,
	type AttachmentMeta,
	type TimelineBranchRun,
	type TimelineCheckpoint,
	type TimelineItem,
	type WorkerListItem,
} from "@/api/client";
import {Markdown} from "@/components/Markdown";
import {ToolCall, type ToolCallPair, tryParseJson, isErrorResult} from "@/components/ToolCall";
import {PortalWorkerCard} from "./PortalWorkerCard";
import clsx from "clsx";

/**
 * A chronicle checkpoint in the portal timeline: a quiet divider that names
 * the span it covers and opens to the summary. Not a message — it was written
 * by neither side of the conversation.
 */
function InlineCheckpointCard({ item }: { item: TimelineCheckpoint }) {
  const [expanded, setExpanded] = useState(false);
  const from = new Date(item.covers_from);
  const to = new Date(item.covers_to);
  const day = (value: Date) =>
    value.toLocaleDateString(undefined, { month: "short", day: "numeric" });
  const range =
    from.toDateString() === to.toDateString()
      ? day(from)
      : `${day(from)} → ${day(to)}`;

  return (
    <div className="py-2">
      <button
        type="button"
        onClick={() => setExpanded((value) => !value)}
        className="flex w-full items-center gap-3 text-left"
      >
        <span className="h-px flex-1 bg-app-line/60" />
        <span className="flex-shrink-0 text-tiny text-ink-faint">
          {item.level > 0 ? "Rollup" : "Checkpoint"} · {item.title} · {range} ·{" "}
          {item.message_count} messages {expanded ? "▾" : "▸"}
        </span>
        <span className="h-px flex-1 bg-app-line/60" />
      </button>
      {expanded && (
        <div className="mt-2 rounded-md border border-app-line/60 px-3 py-2 text-sm text-ink-dull">
          <Markdown>{item.summary}</Markdown>
        </div>
      )}
    </div>
  );
}

function ConversationStartMarker({ createdAt }: { createdAt: string }) {
	const timestamp = new Date(createdAt).toLocaleString(undefined, {
		month: "short",
		day: "numeric",
		year: "numeric",
		hour: "numeric",
		minute: "2-digit",
	});

	return (
		<div className="flex items-center gap-3 py-5">
			<span className="h-px flex-1 border-t border-dashed border-app-line/60" />
			<span className="shrink-0 text-[10px] uppercase tracking-[0.12em] text-ink-faint">
				Beginning of conversation · {timestamp}
			</span>
			<span className="h-px flex-1 border-t border-dashed border-app-line/60" />
		</div>
	);
}

function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function formatMessageTime(createdAt: string): string {
	return new Date(createdAt).toLocaleTimeString(undefined, {
		hour: "numeric",
		minute: "2-digit",
	});
}

function InlineMedia({
  agentId,
  attachment,
}: {
  agentId: string;
  attachment: AttachmentMeta;
}) {
  const url = api.attachmentUrl(agentId, attachment.id);
  if (attachment.mime_type.startsWith("audio/")) {
    return <audio controls src={url} className="max-w-full" />;
  }
  if (attachment.mime_type.startsWith("video/")) {
    return (
      <video controls src={url} className="max-h-72 max-w-full rounded-lg" />
    );
  }
  return null;
}

/** User message bubble with attachments rendered inline at the top. */
function UserMessageWithAttachments({
  content,
  attachments,
  agentId,
	createdAt,
}: {
  content: string;
  attachments: AttachmentMeta[];
  agentId: string;
	createdAt: string;
}) {
  const images = attachments.filter((a) => a.mime_type.startsWith("image/"));
  const media = attachments.filter(
    (a) => a.mime_type.startsWith("audio/") || a.mime_type.startsWith("video/"),
  );
  const files = attachments.filter(
    (a) =>
      !a.mime_type.startsWith("image/") &&
      !a.mime_type.startsWith("audio/") &&
      !a.mime_type.startsWith("video/"),
  );

  return (
    <div className="group flex flex-col items-end py-2">
      <div className="max-w-[80%] overflow-hidden rounded-2xl bg-accent text-sm leading-6 text-white">
        {images.length > 0 && (
          <div
            className={clsx(
              images.length === 1 ? "" : "grid grid-cols-2 gap-px",
            )}
          >
            {images.map((att) => (
              <a
                key={att.id}
                href={api.attachmentUrl(agentId, att.id)}
                target="_blank"
                rel="noopener noreferrer"
              >
                <img
                  src={api.attachmentUrl(agentId, att.id)}
                  alt={att.filename}
                  className="max-h-72 w-full object-cover"
                  loading="lazy"
                />
              </a>
            ))}
          </div>
        )}
        {media.length > 0 && (
          <div className="flex flex-col gap-2 px-3 pt-2.5">
            {media.map((att) => (
              <InlineMedia key={att.id} agentId={agentId} attachment={att} />
            ))}
          </div>
        )}
        {files.length > 0 && (
          <div className="flex flex-wrap gap-1.5 px-3 pt-2.5">
            {files.map((att) => (
              <a
                key={att.id}
                href={api.attachmentUrl(agentId, att.id, { download: true })}
                download={att.filename}
                className="flex items-center gap-1.5 rounded-md bg-white/20 px-2 py-1 text-xs transition-colors hover:bg-white/30"
              >
                <FileIcon size={12} className="flex-shrink-0" />
                <span className="max-w-[160px] truncate">{att.filename}</span>
                <span className="opacity-70">
                  {formatFileSize(att.size_bytes)}
                </span>
              </a>
            ))}
          </div>
        )}
        {content && (
          <div
            className={clsx(
              "px-4 py-2 whitespace-pre-wrap break-words",
              (images.length > 0 || files.length > 0) && "pt-1.5",
            )}
          >
            {content}
          </div>
        )}
      </div>
			<span className="mt-1 text-[10px] text-ink-faint">{formatMessageTime(createdAt)}</span>
    </div>
  );
}

/** Attachments shown below an assistant message, inline with the thread. */
function AssistantAttachments({
  agentId,
  attachments,
}: {
  agentId: string;
  attachments: AttachmentMeta[];
}) {
  if (attachments.length === 0) return null;

  const images = attachments.filter((a) => a.mime_type.startsWith("image/"));
  const media = attachments.filter(
    (a) => a.mime_type.startsWith("audio/") || a.mime_type.startsWith("video/"),
  );
  const files = attachments.filter(
    (a) =>
      !a.mime_type.startsWith("image/") &&
      !a.mime_type.startsWith("audio/") &&
      !a.mime_type.startsWith("video/"),
  );

  return (
    <div className="mt-2 flex flex-col gap-2">
      {images.length > 0 && (
        <div className={clsx("flex flex-wrap gap-2")}>
          {images.map((att) => (
            <a
              key={att.id}
              href={api.attachmentUrl(agentId, att.id)}
              target="_blank"
              rel="noopener noreferrer"
              className="block overflow-hidden rounded-lg"
            >
              <img
                src={api.attachmentUrl(agentId, att.id)}
                alt={att.filename}
                className="max-h-64 max-w-xs rounded-lg object-cover"
                loading="lazy"
              />
            </a>
          ))}
        </div>
      )}
      {media.length > 0 && (
        <div className="flex flex-col gap-2">
          {media.map((att) => (
            <InlineMedia key={att.id} agentId={agentId} attachment={att} />
          ))}
        </div>
      )}
      {files.length > 0 && (
        <div className="flex flex-wrap gap-2">
          {files.map((att) => (
            <a
              key={att.id}
              href={api.attachmentUrl(agentId, att.id, { download: true })}
              download={att.filename}
              className="border-app-line bg-app-box hover:bg-app-box/80 flex items-center gap-2 rounded-lg border px-3 py-2 text-sm transition-colors"
            >
              <FileIcon size={16} className="text-ink-faint flex-shrink-0" />
              <div className="min-w-0">
                <div className="text-ink max-w-[200px] truncate">
                  {att.filename}
                </div>
                <div className="text-ink-faint text-xs">
                  {formatFileSize(att.size_bytes)}
                </div>
              </div>
            </a>
          ))}
        </div>
      )}
    </div>
  );
}

interface PortalTimelineProps {
  agentId: string;
  conversationId: string;
	conversationCreatedAt?: string;
  timeline: TimelineItem[];
  isTyping: boolean;
  sendCount: number;
}

function ThinkingIndicator() {
  return (
    <div className="flex items-center gap-1.5 py-1">
      <span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-ink-faint" />
      <span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-ink-faint [animation-delay:0.2s]" />
      <span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-ink-faint [animation-delay:0.4s]" />
    </div>
  );
}

/** Synthesize a minimal WorkerListItem from a timeline item when the workers
 * list hasn't caught up yet. */
function synthesizeWorker(
  item: Extract<TimelineItem, { type: "worker_run" }>,
  channelId: string,
): WorkerListItem {
  return {
    id: item.id,
    task: item.task,
    status: item.status,
    started_at: item.started_at,
    completed_at: item.completed_at ?? null,
    channel_id: channelId,
    channel_name: null,
    has_transcript: true,
    worker_type: "builtin",
    backend: "builtin",
    runtime_attached: false,
    routable: false,
    tool_calls: 0,
    live_status: null,
    interactive: false,
    directory: null,
    opencode_port: null,
    opencode_session_id: null,
  };
}


type TimelineRow =
	| {kind: "conversation_start"; createdAt: string}
	| {kind: "item"; item: TimelineItem}
	| {kind: "typing"}
	| {kind: "spacer"};

export function PortalTimeline({
  agentId,
  conversationId,
	conversationCreatedAt,
  timeline,
  isTyping,
  sendCount,
}: PortalTimelineProps) {
	const chatRef = useRef<ChatMessageListHandle>(null);

	const workersQuery = useQuery({
		queryKey: ["portal-workers", agentId, conversationId],
		queryFn: () => api.workersList(agentId, {limit: 20}),
		enabled: Boolean(conversationId),
		refetchInterval: 2000,
	});

	// The workers query is a page of the agent's most recent workers, not this
	// conversation's full set, so it cannot decide which rows exist. It only
	// enriches the rows the timeline already carries; `renderTimelineItem`
	// falls back to `synthesizeWorker` for any worker outside the page.
	const conversationWorkers = useMemo(
		() =>
			(workersQuery.data?.workers ?? []).filter(
				(worker) => worker.channel_id === conversationId,
			),
		[workersQuery.data, conversationId],
	);

	const rows: TimelineRow[] = useMemo(() => {
		const list: TimelineRow[] = timeline.map((item) => ({
			kind: "item",
			item,
		}));
		if (conversationCreatedAt && timeline.length > 0) {
			list.unshift({kind: "conversation_start", createdAt: conversationCreatedAt});
		}
		if (isTyping) list.push({kind: "typing"});
		list.push({kind: "spacer"});
		return list;
	}, [conversationCreatedAt, timeline, isTyping]);

	useEffect(() => {
		if (sendCount === 0) return;
		chatRef.current?.scrollToEnd({behavior: "smooth"});
	}, [sendCount]);

	const copyMessage = async (content: string) => {
		await navigator.clipboard.writeText(content);
	};

	return (
		<ChatMessageList<TimelineRow>
			className="flex-1"
			handleRef={chatRef}
			messages={rows}
			getMessageKey={(index) => {
				const row = rows[index]!;
				if (row.kind === "item") return row.item.id;
				if (row.kind === "conversation_start") return "__conversation_start__";
				return `__${row.kind}__`;
			}}
			estimateMessageSize={(index) => {
				const row = rows[index]!;
				if (row.kind === "spacer") return 260;
				if (row.kind === "typing") return 32;
				if (row.kind === "conversation_start") return 42;
				return 80;
			}}
			renderMessage={(row) => {
				if (row.kind === "spacer") return <div className="h-[260px]" />;
				if (row.kind === "conversation_start") {
					return (
						<div className="mx-auto max-w-3xl px-4">
							<ConversationStartMarker createdAt={row.createdAt} />
						</div>
					);
				}
				if (row.kind === "typing") {
					return (
						<div className="mx-auto max-w-3xl px-4 pb-2">
							<ThinkingIndicator />
						</div>
					);
				}
				const item = row.item;
				return (
					<div className="mx-auto max-w-3xl px-4 pb-2">
						{renderTimelineItem(item, {
							agentId,
							conversationId,
							conversationWorkers,
							onCopy: copyMessage,
						})}
					</div>
				);
			}}
		/>
	);
}

function renderTimelineItem(
	item: TimelineItem,
	{
		agentId,
		conversationId,
		conversationWorkers,
		onCopy,
	}: {
		agentId: string;
		conversationId: string;
		conversationWorkers: WorkerListItem[];
		onCopy: (content: string) => Promise<void>;
	},
) {
	if (item.type === "message") {
		const attachments = item.attachments ?? [];
		if (item.role === "user" && attachments.length > 0) {
			return (
				<UserMessageWithAttachments
					content={item.content}
					attachments={attachments}
					agentId={agentId}
					createdAt={item.created_at}
				/>
			);
		}
		return (
			<div>
				<MessageBubble
					content={item.content}
					isUser={item.role === "user"}
					onCopy={(content) => void onCopy(content)}
				/>
				<div
					className={clsx(
						"mt-[-0.25rem] text-[10px] text-ink-faint",
						item.role === "user" ? "text-right" : "text-left",
					)}
				>
					{formatMessageTime(item.created_at)}
				</div>
				{attachments.length > 0 && (
					<AssistantAttachments agentId={agentId} attachments={attachments} />
				)}
			</div>
		);
	}
	if (item.type === "branch_run") {
		return (
			<div className="py-1">
				<InlineBranchCard
					description={(item as TimelineBranchRun).description}
					completedAt={(item as TimelineBranchRun).completed_at ?? null}
					conclusion={(item as TimelineBranchRun).conclusion}
				/>
			</div>
		);
	}
	if (item.type === "worker_run") {
		const worker =
			conversationWorkers.find((w) => w.id === item.id) ??
			synthesizeWorker(item, conversationId);
		return (
			<div className="py-2">
				<PortalWorkerCard agentId={agentId} worker={worker} />
			</div>
		);
	}
	if (item.type === "tool_call_run") {
		const parsedArgs = tryParseJson(item.args);
		const parsedResult = item.result ? tryParseJson(item.result) : null;
		const pair: ToolCallPair = {
			id: item.id,
			name: item.tool_name,
			argsRaw: item.args,
			args: parsedArgs,
			resultRaw: item.result ?? null,
			result: parsedResult,
			status:
				item.status === "running"
					? "running"
					: item.result && isErrorResult(item.result, parsedResult)
						? "error"
						: "completed",
		};
		return (
			<div className="py-1">
				<ToolCall pair={pair} />
			</div>
		);
	}
	if (item.type === "checkpoint") {
		return <InlineCheckpointCard item={item} />;
	}
	return null;
}
