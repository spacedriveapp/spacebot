import {useEffect, useMemo, useRef, useState} from "react";
import {Link} from "@tanstack/react-router";
import {
  api,
  type ChannelInfo,
  type TimelineItem,
  type TimelineBranchRun,
  type TimelineCheckpoint,
  type TimelineWorkerRun,
} from "@/api/client";
import {
  isOpenCodeWorker,
  type ChannelLiveState,
  type ActiveWorker,
  type ActiveBranch,
} from "@/hooks/useChannelLiveState";
import {useLiveContext} from "@/hooks/useLiveContext";
import {Markdown} from "@/components/Markdown";
import {PromptInspector} from "@/components/prompt/PromptInspector";
import {
	ProcessCard,
	ProcessDetail,
	branchRunStatus,
	type ProcessRunDisplay,
	type ProcessSelection,
} from "@/components/processes/ProcessRunView";
import {formatTimestamp, platformIcon, platformColor} from "@/lib/format";
import {
	Button,
	DropdownMenuContent,
	DropdownMenuItem,
	DropdownMenuRoot,
	DropdownMenuTrigger,
} from "@spacedrive/primitives";
import {ChatMessageList, type ChatMessageListHandle} from "@spacedrive/ai";
import {DotsThree} from "@phosphor-icons/react";

interface ChannelDetailProps {
  agentId: string;
  channelId: string;
  channel: ChannelInfo | undefined;
  liveState: ChannelLiveState | undefined;
  onLoadMore: () => void;
}

function LiveBranchRunItem({
  item,
  live,
  agentId,
  channelId,
  selected,
  onSelect,
}: {
  item: TimelineBranchRun;
  live: ActiveBranch;
  agentId: string;
  channelId: string;
  selected: boolean;
  onSelect: () => void;
}) {
  return (
    <ProcessCard
      kind="branch"
      id={item.id}
      title={item.description}
      status="running"
      startedAt={item.started_at}
      toolCalls={live.toolCalls}
      currentTool={live.currentTool ?? live.lastTool}
      selected={selected}
      onSelect={onSelect}
      onCancel={() => api.cancelProcess(agentId, channelId, "branch", item.id).catch(console.warn)}
    />
  );
}

function LiveWorkerRunItem({
  item,
  live,
  agentId,
  channelId,
  selected,
  onSelect,
}: {
  item: TimelineWorkerRun;
  live: ActiveWorker;
  agentId: string;
  channelId: string;
  selected: boolean;
  onSelect: () => void;
}) {
  return (
    <ProcessCard
      kind="worker"
      id={item.id}
      title={item.task}
      status={live.runtimeState === "waiting_for_input" ? "idle" : "running"}
      startedAt={item.started_at}
      toolCalls={live.toolCalls}
      currentTool={live.currentTool ?? live.status}
      processType={live.workerType}
      selected={selected}
      onSelect={onSelect}
      onCancel={() => api.cancelProcess(agentId, channelId, "worker", item.id).catch(console.warn)}
    />
  );
}

function BranchRunItem({ item, selected, onSelect }: { item: TimelineBranchRun; selected: boolean; onSelect: () => void }) {
  const status = branchRunStatus(item.conclusion);
  return (
    <ProcessCard kind="branch" id={item.id} title={item.description} status={status} startedAt={item.started_at} selected={selected} onSelect={onSelect} />
  );
}

function WorkerRunItem({
  item,
  selected,
  onSelect,
}: {
  item: TimelineWorkerRun;
  selected: boolean;
  onSelect: () => void;
}) {
  return (
    <ProcessCard kind="worker" id={item.id} title={item.task} status={item.status} startedAt={item.started_at} processType={isOpenCodeWorker({task: item.task}) ? "opencode" : "builtin"} selected={selected} onSelect={onSelect} />
  );
}

/** Range label for a checkpoint's coverage, collapsed when it's a single day. */
function coverageRange(from: string, to: string): string {
  const start = new Date(from);
  const end = new Date(to);
  const date = (value: Date) =>
    value.toLocaleDateString(undefined, { month: "short", day: "numeric" });
  const time = (value: Date) =>
    value.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  return start.toDateString() === end.toDateString()
    ? `${date(start)} · ${time(start)}–${time(end)}`
    : `${date(start)} ${time(start)} → ${date(end)} ${time(end)}`;
}

/**
 * A chronicle checkpoint, rendered inline where the conversation reached it.
 * Deliberately unlike the message rows on either side: this was authored by
 * neither the user nor the agent.
 */
function CheckpointItem({ item }: { item: TimelineCheckpoint }) {
  const [expanded, setExpanded] = useState(false);
  const emergency = item.kind === "emergency";

  return (
    <div className="flex gap-3 px-3 py-2">
      <span className="flex-shrink-0 pt-0.5 text-tiny text-ink-faint">
        {formatTimestamp(new Date(item.created_at).getTime())}
      </span>
      <div className="min-w-0 flex-1">
        <button
          type="button"
          onClick={() => setExpanded((value) => !value)}
          className={`w-full rounded-md border border-dashed px-3 py-2 text-left transition-colors ${
            emergency
              ? "border-status-error/30 bg-status-error/5 hover:bg-status-error/10"
              : "border-ink-faint/25 bg-app-dark-box/40 hover:bg-app-dark-box/60"
          }`}
        >
          <div className="flex min-w-0 items-baseline gap-2">
            <span
              className={`flex-shrink-0 text-tiny font-medium uppercase tracking-wide ${
                emergency ? "text-status-error/80" : "text-ink-faint"
              }`}
            >
              {item.level > 0 ? "Rollup" : "Checkpoint"} #{item.seq}
            </span>
            <span className="min-w-0 flex-1 truncate text-sm text-ink-dull">
              {item.title}
            </span>
            <span className="flex-shrink-0 text-tiny leading-5 text-ink-faint">
              {expanded ? "▾" : "▸"}
            </span>
          </div>
          <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 text-tiny text-ink-faint">
            <span>{coverageRange(item.covers_from, item.covers_to)}</span>
            <span>{item.message_count} messages</span>
            {item.kind !== "interval" && <span>{item.kind}</span>}
            {item.rolled_up_into && <span>rolled up</span>}
          </div>
        </button>
        {expanded && (
          <div className="mt-1 rounded-md border border-ink-faint/10 bg-app-dark-box/30 px-3 py-2">
            <div className="text-sm text-ink-dull">
              <Markdown className="break-words">{item.summary}</Markdown>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

/**
 * Per-message actions, revealed on hover. Prompt assembly changes between
 * turns, so the useful question is what this turn looked like, not what the
 * channel looks like now.
 */
function MessageActions({
  messageId,
  onInspect,
}: {
  messageId: string;
  onInspect: () => void;
}) {
  return (
    <DropdownMenuRoot>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          aria-label="Message actions"
          className="absolute right-1.5 top-1.5 rounded-md p-1 text-ink-faint opacity-0 transition-opacity hover:bg-app-hover hover:text-ink focus:opacity-100 group-hover:opacity-100"
        >
          <DotsThree className="size-4" weight="bold" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuItem onSelect={onInspect}>
          Inspect prompt at this turn
        </DropdownMenuItem>
        <DropdownMenuItem
          onSelect={() =>
            navigator.clipboard?.writeText(messageId).catch(console.warn)
          }
        >
          Copy message id
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenuRoot>
  );
}

function TimelineEntry({
  item,
  liveWorkers,
  liveBranches,
  agentId,
  channelId,
  selection,
  onSelect,
  onInspectMessage,
}: {
  item: TimelineItem;
  liveWorkers: Record<string, ActiveWorker>;
  liveBranches: Record<string, ActiveBranch>;
  agentId: string;
  channelId: string;
  selection: ProcessSelection | null;
  onSelect: (selection: ProcessSelection) => void;
  onInspectMessage: (messageId: string) => void;
}) {
  switch (item.type) {
    case "message":
      return (
        <div
          className={`group relative flex gap-3 rounded-md px-3 py-2 ${
            item.role === "user" ? "bg-app-dark-box/30" : item.role === "system" ? "border border-app-line/40 bg-app-box/30" : ""
          }`}
        >
          <span className="flex-shrink-0 pt-0.5 text-tiny text-ink-faint">
            {formatTimestamp(new Date(item.created_at).getTime())}
          </span>
          <div className="min-w-0 flex-1">
            <span
              className={`text-sm font-medium ${
                item.role === "user"
                  ? "text-accent-faint"
                  : item.role === "system"
                    ? "text-ink-faint"
                  : "text-status-success"
              }`}
            >
              {item.role === "user"
                ? (item.sender_name ?? "user")
                : item.role === "system"
                  ? "system"
                : (item.sender_name ?? "bot")}
            </span>
            <div className="mt-0.5 text-sm text-ink-dull">
              <Markdown>{item.content}</Markdown>
            </div>
          </div>
          <MessageActions
            messageId={item.id}
            onInspect={() => onInspectMessage(item.id)}
          />
        </div>
      );
    case "branch_run": {
      const live = liveBranches[item.id];
      const selected = selection?.kind === "branch" && selection.id === item.id;
      if (live)
        return (
          <LiveBranchRunItem
            item={item as TimelineBranchRun}
            live={live}
            agentId={agentId}
            channelId={channelId}
            selected={selected}
            onSelect={() => onSelect({kind: "branch", id: item.id})}
          />
        );
      return (
        <BranchRunItem
          item={item as TimelineBranchRun}
          selected={selected}
          onSelect={() => onSelect({kind: "branch", id: item.id})}
        />
      );
    }
    case "worker_run": {
      const live = liveWorkers[item.id];
      const selected = selection?.kind === "worker" && selection.id === item.id;
      if (live)
        return (
          <LiveWorkerRunItem
            item={item as TimelineWorkerRun}
            live={live}
            agentId={agentId}
            channelId={channelId}
            selected={selected}
            onSelect={() => onSelect({kind: "worker", id: item.id})}
          />
        );
      return (
        <WorkerRunItem
          item={item as TimelineWorkerRun}
          selected={selected}
          onSelect={() => onSelect({kind: "worker", id: item.id})}
        />
      );
    }
    case "checkpoint":
      return <CheckpointItem item={item as TimelineCheckpoint} />;
  }
}

/**
 * Estimated row height, used to place a row before measurement lands. A
 * constant estimate leaves freshly scrolled rows overlapping until they
 * settle, so track the shape of each row: process cards render at a fixed
 * height, and message height scales with content length.
 */
function estimateTimelineItemHeight(item: TimelineItem): number {
	if (item.type === "branch_run" || item.type === "worker_run") return 117;
	if (item.type === "checkpoint") return 80;
	if (item.type !== "message") return 71;

	// Wrapped prose and hard-wrapped blocks (code, lists) grow at different
	// rates, so take whichever model predicts the taller row.
	const content = item.content;
	const wrapped = 32 + content.length * 0.31;
	const lines = content
		.split("\n")
		.reduce((count, line) => count + Math.max(1, Math.ceil(line.length / 110)), 0);
	const stacked = 48 + lines * 22;
	return Math.min(2000, Math.max(71, Math.round(Math.max(wrapped, stacked))));
}

function processFallback(
  selection: ProcessSelection,
  timeline: TimelineItem[],
  workers: Record<string, ActiveWorker>,
  branches: Record<string, ActiveBranch>,
  channelName: string | null,
): ProcessRunDisplay | null {
  const item = timeline.find(
    (candidate) =>
      candidate.id === selection.id &&
      candidate.type === `${selection.kind}_run`,
  );

  if (item?.type === "branch_run") {
    const live = branches[item.id];
    const status = live ? "running" : branchRunStatus(item.conclusion ?? null);
    return {
      kind: "branch",
      id: item.id,
      input: item.description,
      output: item.conclusion ?? null,
      status,
      channel_name: channelName,
      started_at: item.started_at,
      completed_at: item.completed_at,
      tool_calls: live?.toolCalls,
    };
  }

  if (item?.type === "worker_run") {
    const live = workers[item.id];
    return {
      kind: "worker",
      id: item.id,
      input: item.task,
      output: item.result ?? null,
      status: live
        ? live.runtimeState === "waiting_for_input"
          ? "idle"
          : "running"
        : item.status,
      process_type: live?.workerType,
      channel_name: channelName,
      started_at: item.started_at,
      completed_at: item.completed_at,
      tool_calls: live?.toolCalls,
      interactive: live?.interactive,
    };
  }

  return null;
}

export function ChannelDetail({
  agentId,
  channelId,
  channel,
  liveState,
  onLoadMore,
}: ChannelDetailProps) {
  const timeline = liveState?.timeline ?? [];
  const hasMore = liveState?.hasMore ?? false;
  const loadingMore = liveState?.loadingMore ?? false;
  const isTyping = liveState?.isTyping ?? false;
  const thinking = liveState?.thinking ?? null;
  const workers = liveState?.workers ?? {};
  const branches = liveState?.branches ?? {};
  const activeWorkerCount = Object.keys(workers).length;
  const activeBranchCount = Object.keys(branches).length;
  const compaction = liveState?.compaction;
  const hasActivity =
    activeWorkerCount > 0 || activeBranchCount > 0 || compaction !== null;
  const [inspectOpen, setInspectOpen] = useState(false);
  const [inspectMessageId, setInspectMessageId] = useState<string | null>(null);
  const [selection, setSelection] = useState<ProcessSelection | null>(null);
  const { liveTranscripts } = useLiveContext();
  const selectedFallback = useMemo(
    () =>
      selection
        ? processFallback(
            selection,
            timeline,
            workers,
            branches,
            channel?.display_name ?? null,
          )
        : null,
    [selection, timeline, workers, branches, channel?.display_name],
  );

	type ChannelRow =
		| {kind: "beginning"}
		| {kind: "item"; item: TimelineItem}
		| {kind: "thinking"}
		| {kind: "typing"};

	const rows: ChannelRow[] = useMemo(() => {
		const list: ChannelRow[] = [];
		if (!hasMore && timeline.length > 0) list.push({kind: "beginning"});
		for (const item of timeline) list.push({kind: "item", item});
		if (thinking) list.push({kind: "thinking"});
		if (isTyping) list.push({kind: "typing"});
		return list;
	}, [timeline, hasMore, isTyping, thinking]);

	const rowCount = rows.length;
	const chatRef = useRef<ChatMessageListHandle>(null);
	const openedChannelRef = useRef<string | null>(null);

	useEffect(() => {
		openedChannelRef.current = null;
	}, [channelId]);

	// Open a channel at its newest message and keep following the end while
	// history streams in. Rows are placed from estimates and re-measured after
	// paint, and the timeline keeps growing after the first render, so hold the
	// bottom across a few frames each time. Once the reader scrolls up, their
	// position is left alone.
	//
	// The channel counts as opened on the first pin, not after the frame loop
	// finishes: rowCount changes on nearly every commit while history streams,
	// and the cleanup cancels the pending frame each time, so a loop that only
	// records itself at the end never gets there. Leaving it unrecorded holds
	// `opening` true, which skips the distance check below and drags the reader
	// back to the bottom on every update.
	useEffect(() => {
		if (rowCount === 0) return;
		const opening = openedChannelRef.current !== channelId;
		if (!opening && (chatRef.current?.getDistanceFromEnd() ?? 0) > 200) return;

		let frame = 0;
		let attempts = 0;
		const pinToEnd = () => {
			chatRef.current?.scrollToEnd({behavior: "auto"});
			openedChannelRef.current = channelId;
			attempts += 1;
			if (attempts < 12) {
				frame = requestAnimationFrame(pinToEnd);
			}
		};
		frame = requestAnimationFrame(pinToEnd);
		return () => cancelAnimationFrame(frame);
	}, [channelId, rowCount]);

  return (
    <div className="relative flex h-full min-w-0">
      {/* Main channel content */}
      <div className="flex min-w-0 flex-1 flex-col overflow-hidden">
        {/* Channel sub-header */}
        <div className="flex h-12 items-center gap-3 border-b border-app-line/50 bg-app-dark-box/20 px-6">
          <Link
            to="/agents/$agentId/channels"
            params={{ agentId }}
            className="text-tiny text-ink-faint hover:text-ink-dull"
          >
            Channels
          </Link>
          <span className="text-ink-faint/50">/</span>
          <span className="text-sm font-medium text-ink">
            {channel?.display_name ?? channelId}
            {channel?.display_name && (
              <span className="ml-2 font-normal text-ink-faint text-tiny">
                {channelId}
              </span>
            )}
          </span>
          {channel && (
            <span
              className={`inline-flex items-center rounded-md px-1.5 py-0.5 text-tiny font-medium ${platformColor(channel.platform)}`}
            >
              {platformIcon(channel.platform)}
            </span>
          )}

          {/* Right side: activity indicators + typing + inspect */}
          <div className="ml-auto flex items-center gap-3">
            {hasActivity && (
              <div className="flex items-center gap-2">
                {activeWorkerCount > 0 && (
                  <div className="flex items-center gap-1.5">
                    <div className="h-1.5 w-1.5 animate-pulse rounded-full bg-status-warning" />
                    <span className="text-tiny text-status-warning">
                      {activeWorkerCount} worker
                      {activeWorkerCount !== 1 ? "s" : ""}
                    </span>
                  </div>
                )}
                {activeBranchCount > 0 && (
                  <div className="flex items-center gap-1.5">
                    <div className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
                    <span className="text-tiny text-accent-faint">
                      {activeBranchCount} branch
                      {activeBranchCount !== 1 ? "es" : ""}
                    </span>
                  </div>
                )}
                {compaction && (
                  <div className="flex items-center gap-1.5">
                    <div className="h-1.5 w-1.5 animate-pulse rounded-full bg-cyan-400" />
                    <span className="text-tiny text-cyan-300">
                      {compaction.kind === "chronicle"
                        ? "Chronicle"
                        : "Compacting"}
                    </span>
                  </div>
                )}
              </div>
            )}
            {isTyping && (
              <div className="flex items-center gap-1">
                <span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
                <span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-accent [animation-delay:0.2s]" />
                <span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-accent [animation-delay:0.4s]" />
                <span className="ml-1 text-tiny text-ink-faint">typing</span>
              </div>
            )}
            <DropdownMenuRoot>
              <DropdownMenuTrigger asChild>
                <Button
                  aria-label="Channel actions"
                  variant="bare"
                  size="icon"
                  title="Channel actions"
                >
                  <DotsThree className="h-4 w-4" weight="bold" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem onSelect={() => setInspectOpen(true)}>
                  Inspect prompt
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenuRoot>
          </div>
        </div>

				{/* Timeline — min-h-0 lets the virtualized list bound its own
				    scroll height instead of growing to the full spacer size */}
				<div className="flex min-h-0 flex-1">
					{timeline.length === 0 ? (
						<p className="p-6 text-sm text-ink-faint">No messages yet</p>
					) : (
						<ChatMessageList<ChannelRow>
							className="flex-1"
							handleRef={chatRef}
							messages={rows}
							hasMoreHistory={hasMore}
							isLoadingOlder={loadingMore}
							onLoadOlder={onLoadMore}
							getMessageKey={(index) => {
								const row = rows[index]!;
								if (row.kind === "item") return row.item.id;
								return `__${row.kind}__`;
							}}
							estimateMessageSize={(index) => {
								const row = rows[index]!;
								if (row.kind === "beginning") return 43;
								if (row.kind === "thinking") return 80;
								if (row.kind === "typing") return 40;
								return estimateTimelineItemHeight(row.item);
							}}
							renderMessage={(row) => {
								if (row.kind === "beginning") {
									return (
										<div className="flex justify-center px-6 py-3">
											<span className="text-tiny text-ink-faint/50">
												Beginning of conversation
											</span>
										</div>
									);
								}
								if (row.kind === "thinking") {
									return (
										<div className="flex gap-3 px-9 py-2">
											<span className="flex-shrink-0 pt-0.5 text-tiny text-ink-faint">
												{formatTimestamp(Date.now())}
											</span>
											<div className="min-w-0 flex-1 rounded-md bg-app-dark-box/40 px-3 py-2">
												<span className="text-tiny font-medium uppercase tracking-wide text-ink-faint">
													thinking
												</span>
												<div className="mt-0.5 whitespace-pre-wrap break-words text-xs italic leading-relaxed text-ink-dull">
													{thinking}
												</div>
											</div>
										</div>
									);
								}
								if (row.kind === "typing") {
									return (
										<div className="flex gap-3 px-9 py-2">
											<span className="flex-shrink-0 pt-0.5 text-tiny text-ink-faint">
												{formatTimestamp(Date.now())}
											</span>
											<div className="flex items-center gap-1.5">
												<span className="text-sm font-medium text-status-success">
													bot
												</span>
												<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-ink-faint" />
												<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-ink-faint [animation-delay:0.2s]" />
												<span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-ink-faint [animation-delay:0.4s]" />
											</div>
										</div>
									);
								}
								return (
									<div className="px-6 pt-1">
										<TimelineEntry
											item={row.item}
											liveWorkers={workers}
											liveBranches={branches}
											agentId={agentId}
											channelId={channelId}
											selection={selection}
											onSelect={setSelection}
											onInspectMessage={setInspectMessageId}
										/>
									</div>
								);
							}}
						/>
					)}
				</div>
			</div>

      {selection && selectedFallback && (
        <aside className="absolute inset-0 z-30 border-l border-app-line/50 md:static md:z-auto md:w-[min(46%,560px)] md:flex-shrink-0">
          <ProcessDetail
            agentId={agentId}
            selection={selection}
            fallback={selectedFallback}
            liveTranscript={liveTranscripts[selection.id]}
            onClose={() => setSelection(null)}
          />
        </aside>
      )}

      <PromptInspector
        open={inspectOpen}
        onOpenChange={setInspectOpen}
        agentId={agentId}
        scope={{kind: "channel", channelId}}
      />

      {inspectMessageId && (
        <PromptInspector
          open
          onOpenChange={(open) => !open && setInspectMessageId(null)}
          agentId={agentId}
          scope={{kind: "message", messageId: inspectMessageId}}
        />
      )}

    </div>
  );
}
